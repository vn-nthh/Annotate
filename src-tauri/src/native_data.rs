//! The native dictionary/history store uses the existing sync schema.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Term {
    pub value: String,
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TermDeletion {
    pub value: String,
    #[serde(default)]
    pub deleted_at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Dictionary {
    pub version: u8,
    pub terms: Vec<Term>,
    pub deleted: Vec<TermDeletion>,
}

impl Default for Dictionary {
    fn default() -> Self {
        Self {
            version: 2,
            terms: Vec::new(),
            deleted: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub text: String,
    pub time: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EntryDeletion {
    pub text: String,
    pub time: u64,
    pub deleted_at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct History {
    pub version: u8,
    pub entries: Vec<Entry>,
    pub deleted: Vec<EntryDeletion>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            version: 2,
            entries: Vec::new(),
            deleted: Vec::new(),
        }
    }
}

pub fn key(value: &str) -> String {
    trim(value).to_lowercase()
}

pub fn trim(value: &str) -> &str {
    value.trim_matches(|c| {
        matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|
        '\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
    })
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn timestamp(value: Option<&Value>, fallback: u64) -> u64 {
    if let Some(number) = value.and_then(Value::as_u64) {
        return number;
    }
    let number = match value {
        None => return fallback,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(value)) => u8::from(*value) as f64,
        Some(Value::String(value)) if trim(value).is_empty() => 0.0,
        Some(Value::String(value)) => trim(value).parse::<f64>().unwrap_or(f64::NAN),
        Some(value) => value.as_f64().unwrap_or(f64::NAN),
    };
    if number.is_finite() && number >= 0.0 {
        number as u64
    } else {
        fallback
    }
}

fn compare_terms(a: &str, b: &str) -> std::cmp::Ordering {
    use windows::{
        core::PCWSTR,
        Win32::Globalization::{CompareStringEx, NORM_IGNORECASE, NORM_IGNORENONSPACE},
    };
    let a_utf16: Vec<_> = a.encode_utf16().collect();
    let b_utf16: Vec<_> = b.encode_utf16().collect();
    let result = unsafe {
        CompareStringEx(
            PCWSTR::null(),
            NORM_IGNORECASE | NORM_IGNORENONSPACE,
            &a_utf16,
            &b_utf16,
            None,
            None,
            None,
        )
    }
    .0;
    if result == 0 {
        key(a).cmp(&key(b))
    } else {
        result.cmp(&2)
    }
}

fn array<'a>(value: &'a Value, field: &str) -> &'a [Value] {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

impl Dictionary {
    pub fn parse(raw: &Value) -> Self {
        let mut terms = BTreeMap::<String, Term>::new();
        let mut deleted = BTreeMap::<String, TermDeletion>::new();
        let mut order = BTreeMap::new();
        for (index, item) in raw
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_else(|| array(raw, "terms"))
            .iter()
            .enumerate()
        {
            if let Some(value) = item
                .as_str()
                .or_else(|| item.get("value").and_then(Value::as_str))
            {
                let value = trim(value);
                if value.is_empty() {
                    continue;
                }
                let term = Term {
                    value: value.into(),
                    updated_at: timestamp(item.get("updatedAt"), 0),
                };
                let normalized = key(value);
                order.entry(normalized.clone()).or_insert(index);
                if terms
                    .get(&normalized)
                    .is_none_or(|old| term.updated_at > old.updated_at)
                {
                    terms.insert(normalized, term);
                }
            }
        }
        for item in array(raw, "deleted") {
            if let Some(value) = item
                .as_str()
                .or_else(|| item.get("value").and_then(Value::as_str))
            {
                let value = trim(value);
                if value.is_empty() {
                    continue;
                }
                let deletion = TermDeletion {
                    value: value.into(),
                    deleted_at: timestamp(item.get("deletedAt"), 0),
                };
                let normalized = key(value);
                if deleted
                    .get(&normalized)
                    .is_none_or(|old| deletion.deleted_at > old.deleted_at)
                {
                    deleted.insert(normalized, deletion);
                }
            }
        }
        deleted.retain(|normalized, deletion| match terms.get(normalized) {
            Some(term) if term.updated_at > deletion.deleted_at => false,
            Some(_) => {
                terms.remove(normalized);
                true
            }
            None => true,
        });
        let mut terms: Vec<_> = terms.into_values().collect();
        terms.sort_by(|a, b| {
            compare_terms(&a.value, &b.value)
                .then_with(|| order[&key(&a.value)].cmp(&order[&key(&b.value)]))
        });
        Self {
            version: 2,
            terms,
            deleted: deleted.into_values().collect(),
        }
    }

    pub fn merge(&self, remote: &Self) -> Self {
        Self::parse(&serde_json::json!({
            "terms": self.terms.iter().chain(&remote.terms).collect::<Vec<_>>(),
            "deleted": self.deleted.iter().chain(&remote.deleted).collect::<Vec<_>>(),
        }))
    }

    pub fn add(&mut self, value: &str) -> &'static str {
        let value = trim(value);
        if value.is_empty() {
            return "invalid";
        }
        let normalized = key(value);
        if self.terms.iter().any(|term| key(&term.value) == normalized) {
            return "duplicate";
        }
        let updated_at = self
            .deleted
            .iter()
            .filter(|term| key(&term.value) == normalized)
            .map(|term| term.deleted_at.saturating_add(1))
            .max()
            .unwrap_or(0)
            .max(now());
        self.deleted.retain(|term| key(&term.value) != normalized);
        self.terms.push(Term {
            value: value.into(),
            updated_at,
        });
        self.terms.sort_by(|a, b| compare_terms(&a.value, &b.value));
        "added"
    }

    pub fn remove(&mut self, value: &str) {
        let normalized = key(value);
        if let Some(index) = self
            .terms
            .iter()
            .position(|term| key(&term.value) == normalized)
        {
            let term = self.terms.remove(index);
            self.deleted.retain(|term| key(&term.value) != normalized);
            self.deleted.push(TermDeletion {
                value: term.value,
                deleted_at: now().max(term.updated_at),
            });
        }
    }

    pub fn prompt(&self) -> Option<String> {
        (!self.terms.is_empty()).then(|| {
            self.terms
                .iter()
                .map(|term| term.value.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
    }
}

impl History {
    pub fn parse(raw: &Value) -> Self {
        let mut entries = BTreeMap::<String, Entry>::new();
        let mut deleted = BTreeMap::<String, EntryDeletion>::new();
        let mut order = BTreeMap::new();
        for (index, item) in raw
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_else(|| array(raw, "entries"))
            .iter()
            .enumerate()
        {
            if let Some(text) = item
                .get("text")
                .and_then(Value::as_str)
                .map(trim)
                .filter(|s| !s.is_empty())
            {
                let time = timestamp(item.get("time"), 0);
                let entry = Entry {
                    text: text.into(),
                    time,
                    updated_at: timestamp(item.get("updatedAt"), time),
                };
                let normalized = key(text);
                order.entry(normalized.clone()).or_insert(index);
                if entries
                    .get(&normalized)
                    .is_none_or(|old| entry.updated_at > old.updated_at)
                {
                    entries.insert(normalized, entry);
                }
            }
        }
        for item in array(raw, "deleted") {
            if let Some(text) = item
                .get("text")
                .and_then(Value::as_str)
                .map(trim)
                .filter(|s| !s.is_empty())
            {
                let time = timestamp(item.get("time"), 0);
                let deletion = EntryDeletion {
                    text: text.into(),
                    time,
                    deleted_at: timestamp(item.get("deletedAt"), time),
                };
                let normalized = key(text);
                if deleted
                    .get(&normalized)
                    .is_none_or(|old| deletion.deleted_at > old.deleted_at)
                {
                    deleted.insert(normalized, deletion);
                }
            }
        }
        deleted.retain(|normalized, deletion| match entries.get(normalized) {
            Some(entry) if entry.updated_at > deletion.deleted_at => false,
            Some(_) => {
                entries.remove(normalized);
                true
            }
            None => true,
        });
        let mut entries: Vec<_> = entries.into_values().collect();
        entries.sort_by(|a, b| {
            b.time
                .cmp(&a.time)
                .then_with(|| order[&key(&a.text)].cmp(&order[&key(&b.text)]))
        });
        entries.truncate(50);
        Self {
            version: 2,
            entries,
            deleted: deleted.into_values().collect(),
        }
    }

    pub fn merge(&self, remote: &Self) -> Self {
        Self::parse(&serde_json::json!({
            "entries": self.entries.iter().chain(&remote.entries).collect::<Vec<_>>(),
            "deleted": self.deleted.iter().chain(&remote.deleted).collect::<Vec<_>>(),
        }))
    }

    pub fn add(&mut self, text: &str) {
        let text = trim(text);
        if text.is_empty() {
            return;
        }
        let normalized = key(text);
        let updated_at = self
            .entries
            .iter()
            .filter(|entry| key(&entry.text) == normalized)
            .map(|entry| entry.updated_at)
            .chain(
                self.deleted
                    .iter()
                    .filter(|entry| key(&entry.text) == normalized)
                    .map(|entry| entry.deleted_at),
            )
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(now());
        self.entries.retain(|entry| key(&entry.text) != normalized);
        self.deleted.retain(|entry| key(&entry.text) != normalized);
        let time = now();
        self.entries.insert(
            0,
            Entry {
                text: text.into(),
                time,
                updated_at,
            },
        );
        self.entries.truncate(50);
    }

    pub fn remove(&mut self, text: &str) {
        let normalized = key(text);
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| key(&entry.text) == normalized)
        {
            let entry = self.entries.remove(index);
            self.deleted.retain(|entry| key(&entry.text) != normalized);
            self.deleted.push(EntryDeletion {
                text: entry.text,
                time: entry.time,
                deleted_at: now().max(entry.updated_at),
            });
        }
    }

    pub fn clear(&mut self) {
        let texts: Vec<_> = self
            .entries
            .iter()
            .map(|entry| entry.text.clone())
            .collect();
        for text in texts {
            self.remove(&text);
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Data {
    pub dictionary: Dictionary,
    pub history: History,
    #[serde(default)]
    pub migrated: bool,
}

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
    pub data: Data,
}

impl Store {
    pub fn open(directory: &Path) -> Result<Self, String> {
        let path = directory.join("native-data.json");
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                format!("Cannot read native data; original file was preserved: {error}")
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(error) => return Err(format!("Cannot open native data: {error}")),
        };
        Ok(Self { path, data })
    }

    pub fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self.data).map_err(|e| e.to_string())?;
        atomic_write(&self.path, &bytes)
    }

    pub fn import(&mut self, dictionary: &Value, history: &Value) -> Result<(), String> {
        let previous = self.data.clone();
        self.data.dictionary = self.data.dictionary.merge(&Dictionary::parse(dictionary));
        self.data.history = self.data.history.merge(&History::parse(history));
        self.data.migrated = true;
        if let Err(error) = self.save() {
            self.data = previous;
            return Err(error);
        }
        Ok(())
    }
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&temporary, path).map_err(|e| format!("Cannot save {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_store_preserves_legacy_data_and_sync_tombstones() {
        let legacy = Dictionary::parse(&json!(["Rust", " rust ", "WebView"]));
        assert_eq!(legacy.terms.len(), 2);
        let remote = Dictionary::parse(
            &json!({"terms":[{"value":"RUST","updatedAt":10}],"deleted":[{"value":"WebView","deletedAt":1}]}),
        );
        let merged = legacy.merge(&remote);
        assert_eq!(
            merged.terms,
            vec![Term {
                value: "RUST".into(),
                updated_at: 10
            }]
        );
        assert_eq!(merged.merge(&remote), merged);
        let resurrection = Dictionary::parse(&json!({"terms":[{"value":"WebView","updatedAt":2}]}));
        assert!(merged.merge(&resurrection).deleted.is_empty());

        let history =
            History::parse(&json!([{"text":"Hello","time":10},{"text":"hello","time":20}]));
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].time, 20);
        let deletion =
            History::parse(&json!({"deleted":[{"text":"HELLO","time":20,"deletedAt":20}]}));
        assert!(history.merge(&deletion).entries.is_empty());
        let again = History::parse(&json!([{"text":"Hello","time":30}]));
        assert_eq!(history.merge(&deletion).merge(&again).entries.len(), 1);

        let tied = History::parse(&json!([{"text":"Zulu","time":42},{"text":"Alpha","time":42}]));
        assert_eq!(tied.entries[0].text, "Zulu");
        let typed = History::parse(&json!([{"text":"String time","time":"100","updatedAt":null}]));
        assert_eq!(
            (typed.entries[0].time, typed.entries[0].updated_at),
            (100, 0)
        );
        let mut future = Dictionary::parse(
            &json!({"deleted":[{"value":"clock rollback","deletedAt":u64::MAX-1}]}),
        );
        assert_eq!(future.add("clock rollback"), "added");
        assert_eq!(future.terms[0].updated_at, u64::MAX);

        let directory = std::env::temp_dir().join(format!(
            "annotate-native-data-test-{}-{}",
            std::process::id(),
            now()
        ));
        let mut store = Store::open(&directory).unwrap();
        store
            .import(
                &json!(["a retained term"]),
                &json!([{"text":"Retained history","time":42}]),
            )
            .unwrap();
        store.data.dictionary.add("another term");
        store.save().unwrap();
        let reopened = Store::open(&directory).unwrap();
        assert!(reopened.data.migrated);
        assert_eq!(reopened.data.dictionary.terms.len(), 2);
        assert_eq!(reopened.data.history.entries[0].text, "Retained history");
        std::fs::remove_file(directory.join("native-data.json")).unwrap();
        std::fs::create_dir(directory.join("native-data.json")).unwrap();
        store.data.migrated = false;
        let original = store.data.clone();
        assert!(store
            .import(&json!(["must not replace existing state"]), &Value::Null)
            .is_err());
        assert_eq!(store.data, original);
        std::fs::remove_dir(directory.join("native-data.json")).unwrap();
        std::fs::remove_file(directory.join("native-data.tmp")).unwrap();
        std::fs::write(
            directory.join("native-data.json"),
            b"invalid JSON must survive",
        )
        .unwrap();
        assert!(Store::open(&directory).is_err());
        assert_eq!(
            std::fs::read(directory.join("native-data.json")).unwrap(),
            b"invalid JSON must survive"
        );
        std::fs::remove_file(directory.join("native-data.json")).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
