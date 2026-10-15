//! Import Chromium's last live localStorage values without opening a browser.
//! LevelDB recovery runs on a private copy; the source is opened for reading.
use crate::native_data::{Dictionary, History};
use rusty_leveldb::{LdbIterator, Options, DB};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

// Chromium persists StorageFormat::UTF16=0 and Latin1=1. Latin1 bytes are
// individual code points, not UTF-8 (for example café is stored with 0xe9).
// Source: third_party/blink/renderer/modules/storage/cached_storage_area.cc.
fn decode(bytes: &[u8]) -> Result<String, String> {
    match bytes.first() {
        Some(1) => Ok(bytes[1..].iter().map(|byte| char::from(*byte)).collect()),
        Some(0) => {
            if (bytes.len() - 1) % 2 != 0 {
                return Err("Truncated UTF-16 localStorage value".into());
            }
            String::from_utf16(
                &bytes[1..]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| e.to_string())
        }
        _ => Err("Unknown localStorage string encoding".into()),
    }
}
fn fingerprint(
    directory: &Path,
) -> Result<BTreeMap<std::ffi::OsString, (u64, std::time::SystemTime)>, String> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_file() && entry.file_name() != "LOCK" {
            let metadata = entry.metadata().map_err(|e| e.to_string())?;
            files.insert(
                entry.file_name(),
                (
                    metadata.len(),
                    metadata.modified().map_err(|e| e.to_string())?,
                ),
            );
        }
    }
    Ok(files)
}
fn validate(value: &Value, field: &str) -> Result<(), String> {
    if value.is_array() {
        return Ok(());
    }
    if let Some(object) = value.as_object() {
        if object
            .get("version")
            .and_then(Value::as_u64)
            .is_some_and(|version| version > 2)
        {
            return Err("Legacy storage uses a newer sync schema; original data preserved".into());
        }
        if object.get(field).is_some_and(Value::is_array)
            || object.get("deleted").is_some_and(Value::is_array)
        {
            return Ok(());
        }
    }
    Err("Legacy storage has an unsupported shape; original data preserved".into())
}

// rusty-leveldb's iterators skip corrupt blocks instead of reporting the error.
// Validate the copied standard LevelDB records before recovery can silently
// turn partial data into an apparently successful empty import.
fn masked_crc(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0x82f63b78 } else { 0 };
        }
    }
    (!crc).rotate_right(15).wrapping_add(0xa282ead8)
}
fn varint(bytes: &[u8], position: &mut usize) -> Result<usize, String> {
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let byte = *bytes
            .get(*position)
            .ok_or("Truncated legacy block handle")?;
        *position += 1;
        if shift == 63 && byte > 1 {
            return Err("Oversized legacy block handle".into());
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return usize::try_from(value).map_err(|e| e.to_string());
        }
    }
    Err("Invalid legacy block handle".into())
}
fn table_block(bytes: &[u8], offset: usize, size: usize) -> Result<Vec<u8>, String> {
    let end = offset.checked_add(size).ok_or("Oversized legacy block")?;
    let block = bytes
        .get(offset..end.checked_add(5).ok_or("Oversized legacy block")?)
        .ok_or("Truncated legacy table block")?;
    if masked_crc(&block[..size + 1]) != u32::from_le_bytes(block[size + 1..].try_into().unwrap()) {
        return Err("Legacy table checksum failed; original data preserved".into());
    }
    rusty_leveldb::CompressorList::default()
        .get(block[size])
        .map_err(|e| e.to_string())?
        .decode(block[..size].to_vec())
        .map_err(|e| e.to_string())
}
fn validate_table(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 48 || bytes[bytes.len() - 8..] != 0xdb4775248b80fb57u64.to_le_bytes() {
        return Err("Invalid legacy LevelDB table footer".into());
    }
    let footer = &bytes[bytes.len() - 48..];
    let mut position = 0;
    for _ in 0..2 {
        let offset = varint(footer, &mut position)?;
        let size = varint(footer, &mut position)?;
        let index = table_block(bytes, offset, size)?;
        if index.len() < 4 {
            return Err("Truncated legacy table index".into());
        }
        let restarts = u32::from_le_bytes(index[index.len() - 4..].try_into().unwrap()) as usize;
        let data_end = index
            .len()
            .checked_sub(
                restarts
                    .checked_add(1)
                    .and_then(|n| n.checked_mul(4))
                    .ok_or("Oversized legacy table index")?,
            )
            .ok_or("Invalid legacy table index")?;
        let data = &index[..data_end];
        let mut entry = 0;
        let mut key_length = 0;
        while entry < data_end {
            let shared = varint(data, &mut entry)?;
            let key = varint(data, &mut entry)?;
            let value = varint(data, &mut entry)?;
            if shared > key_length {
                return Err("Invalid legacy table key prefix".into());
            }
            key_length = shared
                .checked_add(key)
                .ok_or("Oversized legacy table key")?;
            entry = entry.checked_add(key).ok_or("Oversized legacy table key")?;
            let end = entry
                .checked_add(value)
                .ok_or("Oversized legacy table handle")?;
            let handle = data
                .get(entry..end)
                .ok_or("Truncated legacy table handle")?;
            let mut start = 0;
            let offset = varint(handle, &mut start)?;
            let size = varint(handle, &mut start)?;
            table_block(bytes, offset, size)?;
            entry = end;
        }
    }
    Ok(())
}
fn validate_log(bytes: &[u8]) -> Result<(), String> {
    for block in bytes.chunks(32768) {
        let mut offset = 0;
        while block.len() - offset >= 7 {
            let header = &block[offset..offset + 7];
            if header.iter().all(|byte| *byte == 0) {
                if block[offset..].iter().any(|byte| *byte != 0) {
                    return Err("Invalid legacy log padding".into());
                }
                break;
            }
            let size = u16::from_le_bytes([header[4], header[5]]) as usize;
            let end = offset + 7 + size;
            let payload = block
                .get(offset + 7..end)
                .ok_or("Truncated legacy log record")?;
            let record: Vec<_> = std::iter::once(header[6])
                .chain(payload.iter().copied())
                .collect();
            if !(1..=4).contains(&header[6])
                || masked_crc(&record) != u32::from_le_bytes(header[..4].try_into().unwrap())
            {
                return Err("Legacy log checksum failed; original data preserved".into());
            }
            offset = end;
        }
        if block.len() - offset < 7 && block[offset..].iter().any(|byte| *byte != 0) {
            return Err("Truncated legacy log header".into());
        }
    }
    Ok(())
}
fn validate_files(directory: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let name = path.file_name().unwrap().to_string_lossy();
        let table = matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("ldb" | "sst")
        );
        if table
            || name.starts_with("MANIFEST-")
            || path.extension().is_some_and(|ext| ext == "log")
        {
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            if table {
                validate_table(&bytes)?;
            } else {
                validate_log(&bytes)?;
            }
        }
    }
    Ok(())
}
fn read_copy(source: &Path, directory: &Path) -> Result<(Value, Value), String> {
    let _lock = crate::platform::lock_legacy_storage(source)?;
    let copy = directory.join(format!(
        "legacy-import-{}-{}-{:x}",
        std::process::id(),
        crate::native_data::now(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&copy).map_err(|e| e.to_string())?;
    let owned_copy = std::fs::canonicalize(&copy).map_err(|e| e.to_string())?;
    let owned_parent = std::fs::canonicalize(directory).map_err(|e| e.to_string())?;
    let result = (|| {
        let before = fingerprint(source)?;
        for file in before.keys() {
            std::fs::copy(source.join(file), copy.join(file)).map_err(|e| e.to_string())?;
        }
        if fingerprint(source)? != before {
            return Err(
                "Legacy storage changed while being copied; close the previous Annotate and retry"
                    .into(),
            );
        }
        validate_files(&copy)?;
        let mut db = DB::open(
            &copy,
            Options {
                create_if_missing: false,
                paranoid_checks: true,
                block_cache_capacity_bytes: 64 * 1024,
                max_open_files: 32,
                ..Default::default()
            },
        )
        .map_err(|e| format!("Cannot read legacy storage; original data preserved: {e}"))?;
        let mut iter = db.new_iter().map_err(|e| e.to_string())?;
        let mut dictionary = Dictionary::default();
        let mut history = History::default();
        let mut has_dictionary = false;
        let mut has_history = false;
        while let Some((key, value)) = iter.next() {
            let Some(separator) = key.iter().position(|c| *c == 0) else {
                continue;
            };
            if ![
                b"_http://tauri.localhost".as_slice(),
                b"_https://tauri.localhost".as_slice(),
                b"_tauri://localhost".as_slice(),
            ]
            .contains(&&key[..separator])
            {
                continue;
            }
            let Ok(name) = decode(&key[separator + 1..]) else {
                continue;
            };
            if name != "annotate_dictionary" && name != "annotate_history" {
                continue;
            }
            let text = decode(&value)?;
            let data: Value = serde_json::from_str(&text)
                .map_err(|e| format!("Cannot import {name}; original data preserved: {e}"))?;
            if name == "annotate_dictionary" {
                validate(&data, "terms")?;
                dictionary = dictionary.merge(&Dictionary::parse(&data));
                has_dictionary = true;
            } else {
                validate(&data, "entries")?;
                history = history.merge(&History::parse(&data));
                has_history = true;
            }
        }
        drop(iter);
        db.close().map_err(|e| e.to_string())?;
        Ok((
            if has_dictionary {
                serde_json::to_value(dictionary).map_err(|e| e.to_string())?
            } else {
                Value::Null
            },
            if has_history {
                serde_json::to_value(history).map_err(|e| e.to_string())?
            } else {
                Value::Null
            },
        ))
    })();
    // Both absolute paths were resolved after create_dir succeeded. This copy
    // has no symlink/user-selected child components and cannot reach the source.
    if owned_copy.parent() == Some(owned_parent.as_path())
        && owned_copy
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("legacy-import-"))
    {
        let _ = std::fs::remove_dir_all(&owned_copy);
    }
    result
}
pub fn read_legacy(directory: &Path) -> Result<(Value, Value), String> {
    let candidates = crate::platform::legacy_storage_paths(directory);
    let mut dictionary = Dictionary::default();
    let mut history = History::default();
    let mut has_dictionary = false;
    let mut has_history = false;
    for source in &candidates {
        match std::fs::metadata(source) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(format!(
                    "Cannot inspect legacy storage; original data preserved: {e}"
                ))
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err("Legacy storage directory is not a directory".into())
            }
            _ => {}
        }
        if !source
            .join("CURRENT")
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            if std::fs::read_dir(source)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
            {
                return Err(
                    "Legacy storage has no LevelDB manifest; original data preserved".into(),
                );
            }
            continue;
        }
        let (terms, entries) = read_copy(source, directory)?;
        if !terms.is_null() {
            dictionary = dictionary.merge(&Dictionary::parse(&terms));
            has_dictionary = true;
        }
        if !entries.is_null() {
            history = history.merge(&History::parse(&entries));
            has_history = true;
        }
    }
    Ok((
        if has_dictionary {
            serde_json::to_value(dictionary).map_err(|e| e.to_string())?
        } else {
            Value::Null
        },
        if has_history {
            serde_json::to_value(history).map_err(|e| e.to_string())?
        } else {
            Value::Null
        },
    ))
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use serde_json::json;
    fn encode(text: &str) -> Vec<u8> {
        if text.chars().all(|c| c as u32 <= 255) {
            std::iter::once(1)
                .chain(text.chars().map(|c| c as u8))
                .collect()
        } else {
            std::iter::once(0)
                .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
                .collect()
        }
    }
    fn key(origin: &str, name: &str) -> Vec<u8> {
        let mut key = format!("_{origin}").into_bytes();
        key.push(0);
        key.extend(encode(name));
        key
    }
    fn contents(directory: &Path) -> BTreeMap<std::ffi::OsString, Vec<u8>> {
        std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), std::fs::read(entry.path()).unwrap())
            })
            .collect()
    }
    #[test]
    fn legacy_leveldb_import_is_read_only_and_preserves_live_unicode_tombstones() {
        assert_eq!(decode(&[1, b'c', b'a', b'f', 0xe9]).unwrap(), "café");
        assert_eq!(decode(&encode("你好 👋")).unwrap(), "你好 👋");
        assert!(decode(&[0, 1]).is_err());
        let directory = std::env::temp_dir().join(format!(
            "annotate-legacy-test-{}-{:x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let source = directory.join("EBWebView/Default/Local Storage/leveldb");
        std::fs::create_dir_all(&source).unwrap();
        let mut db = DB::open(
            &source,
            Options {
                compressor: 1,
                write_buffer_size: 128,
                ..Default::default()
            },
        )
        .unwrap();
        let dictionary_key = key("http://tauri.localhost", "annotate_dictionary");
        db.put(&dictionary_key, &encode(r#"["obsolete value"]"#))
            .unwrap();
        db.put(
            &dictionary_key,
            &encode(
                &json!({"version":2,"terms":[{"value":"café","updatedAt":10},
            {"value":"kept","updatedAt":20}],"deleted":[{"value":"removed","deletedAt":40}]})
                .to_string(),
            ),
        )
        .unwrap();
        db.put(&key("https://tauri.localhost","annotate_dictionary"),&encode(&json!({"version":2,
            "terms":[{"value":"removed","updatedAt":30},{"value":"HTTPS only","updatedAt":25}],"deleted":[]}).to_string())).unwrap();
        db.put(&key("http://tauri.localhost","annotate_history"),&encode(&json!({"version":2,
            "entries":[{"text":"你好 👋","time":10,"updatedAt":10},{"text":"removed history","time":5,"updatedAt":5}],
            "deleted":[{"text":"removed history","time":5,"deletedAt":10}]}).to_string())).unwrap();
        db.put(&key("https://other.example", "annotate_dictionary"), &[255])
            .unwrap();
        db.close().unwrap();
        drop(db);
        let before = contents(&source);
        let (dictionary, history) = read_legacy(&directory).unwrap();
        assert_eq!(contents(&source), before);
        let dictionary = Dictionary::parse(&dictionary);
        let history = History::parse(&history);
        assert_eq!(dictionary.terms.len(), 3);
        assert!(dictionary.terms.iter().any(|term| term.value == "café"));
        assert!(!dictionary
            .terms
            .iter()
            .any(|term| term.value == "obsolete value" || term.value == "removed"));
        assert_eq!(dictionary.deleted[0].value, "removed");
        assert_eq!(history.entries[0].text, "你好 👋");
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.deleted[0].text, "removed history");
        let table = std::fs::read_dir(&source)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("ldb" | "sst")
                )
            })
            .unwrap();
        let mut damaged = std::fs::read(&table).unwrap();
        damaged[0] ^= 1;
        std::fs::write(&table, &damaged).unwrap();
        let before = contents(&source);
        assert!(read_legacy(&directory).is_err());
        assert_eq!(contents(&source), before);
        assert!(!std::fs::read_dir(&directory).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("legacy-import-")));
        let resolved = std::fs::canonicalize(&directory).unwrap();
        assert_eq!(
            resolved.parent(),
            Some(
                std::fs::canonicalize(std::env::temp_dir())
                    .unwrap()
                    .as_path()
            )
        );
        assert!(resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("annotate-legacy-test-"));
        std::fs::remove_dir_all(resolved).unwrap();
    }
}
