use super::util::nsstr;
use objc::{msg_send, sel, sel_impl};
use std::path::{Path, PathBuf};

pub fn compare_terms(a: &str, b: &str) -> std::cmp::Ordering {
    objc::rc::autoreleasepool(|| unsafe {
        let result: isize = msg_send![nsstr(a), localizedStandardCompare: nsstr(b)];
        result.cmp(&0)
    })
}
// Previous public releases were Windows-only; macOS has no WebView2 profile.
pub fn legacy_storage_paths(_: &Path) -> Vec<PathBuf> {
    Vec::new()
}
pub fn lock_legacy_storage(source: &Path) -> Result<Option<std::fs::File>, String> {
    use std::os::fd::AsRawFd;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .open(source.join("LOCK"))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Close the previous Annotate before importing its data".into());
    }
    Ok(Some(file))
}
