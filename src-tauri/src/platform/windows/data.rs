//! Windows collation and legacy WebView2 profile access.
use std::path::{Path, PathBuf};

pub fn compare_terms(a: &str, b: &str) -> std::cmp::Ordering {
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
        crate::native_data::key(a).cmp(&crate::native_data::key(b))
    } else {
        result.cmp(&2)
    }
}

pub fn lock_legacy_storage(source: &Path) -> Result<Option<std::fs::File>, String> {
    use std::os::windows::fs::OpenOptionsExt;
    let lock = source.join("LOCK");
    // Holding the existing LOCK file read-only with no sharing prevents an old
    // WebView from opening/writing this profile halfway through the snapshot.
    match std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&lock)
    {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            return Err(format!(
                "Close the previous Annotate before importing its data: {e}"
            ))
        }
    }
}

pub fn legacy_storage_paths(directory: &Path) -> [PathBuf; 3] {
    [
        directory.join("EBWebView/Default/Local Storage/leveldb"),
        directory.join(".WebView2/EBWebView/Default/Local Storage/leveldb"),
        directory.join("WebView2/EBWebView/Default/Local Storage/leveldb"),
    ]
}
