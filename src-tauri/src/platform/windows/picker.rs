//! Owned Windows Shell file dialogs. Each call runs in the calling thread's STA.
use std::{
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use windows::{
    core::{w, HRESULT, HSTRING},
    Win32::{
        Foundation::{ERROR_CANCELLED, HWND},
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        },
        UI::{
            Shell::{
                Common::COMDLG_FILTERSPEC, FileOpenDialog, FileSaveDialog, IFileDialog,
                FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_OVERWRITEPROMPT,
                FOS_PATHMUSTEXIST, SIGDN_FILESYSPATH,
            },
            WindowsAndMessaging::IsWindow,
        },
    },
};

static OPEN: AtomicBool = AtomicBool::new(false);

struct PickerGuard;
impl PickerGuard {
    fn acquire() -> Option<Self> {
        OPEN.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}
impl Drop for PickerGuard {
    fn drop(&mut self) {
        OPEN.store(false, Ordering::Release);
    }
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Open one media file. Cancellation, including a duplicate request, is `None`.
/// Call from the UI STA or a dedicated thread, with a live owner HWND.
pub fn open(owner: HWND) -> Result<Option<PathBuf>, String> {
    pick(owner, None)
}

/// Save an SRT file, suggesting `filename` and appending .srt when necessary.
pub fn save(owner: HWND, filename: &str) -> Result<Option<PathBuf>, String> {
    pick(owner, Some(filename))
}

fn pick(owner: HWND, filename: Option<&str>) -> Result<Option<PathBuf>, String> {
    unsafe {
        if !IsWindow(Some(owner)).as_bool() {
            return Err("The settings window is no longer open".into());
        }
        let Some(_picker) = PickerGuard::acquire() else {
            return Ok(None);
        };
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        let _apartment = Apartment;
        let save = filename.is_some();
        let dialog: IFileDialog = CoCreateInstance(
            if save {
                &FileSaveDialog
            } else {
                &FileOpenDialog
            },
            None,
            CLSCTX_INPROC_SERVER,
        )
        .map_err(|e| e.to_string())?;
        let options = dialog.GetOptions().map_err(|e| e.to_string())?;
        dialog
            .SetOptions(
                options
                    | FOS_FORCEFILESYSTEM
                    | FOS_PATHMUSTEXIST
                    | FOS_NOCHANGEDIR
                    | if save {
                        FOS_OVERWRITEPROMPT
                    } else {
                        FOS_FILEMUSTEXIST
                    },
            )
            .map_err(|e| e.to_string())?;
        let filters = [if save {
            COMDLG_FILTERSPEC {
                pszName: w!("SRT Subtitle"),
                pszSpec: w!("*.srt"),
            }
        } else {
            COMDLG_FILTERSPEC {
                pszName: w!("Media Files"),
                pszSpec: w!(
                    "*.mp4;*.mkv;*.avi;*.mov;*.webm;*.mp3;*.wav;*.flac;*.ogg;*.aac;*.m4a;*.wma"
                ),
            }
        }];
        dialog.SetFileTypes(&filters).map_err(|e| e.to_string())?;
        dialog.SetFileTypeIndex(1).map_err(|e| e.to_string())?;
        if let Some(filename) = filename {
            dialog
                .SetFileName(&HSTRING::from(filename))
                .map_err(|e| e.to_string())?;
            dialog
                .SetDefaultExtension(w!("srt"))
                .map_err(|e| e.to_string())?;
        }
        match dialog.Show(Some(owner)) {
            Ok(()) => {}
            Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => {
                return Ok(None);
            }
            Err(error) => return Err(error.to_string()),
        }
        let item = dialog.GetResult().map_err(|e| e.to_string())?;
        let allocated = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|e| e.to_string())?;
        if allocated.is_null() {
            return Err("The file dialog did not return a filesystem path".into());
        }
        let path = path_from_wide(allocated.as_wide());
        CoTaskMemFree(Some(allocated.0.cast()));
        Ok(Some(path))
    }
}

fn path_from_wide(path: &[u16]) -> PathBuf {
    OsString::from_wide(path).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;

    #[test]
    fn only_one_picker_is_allowed_and_its_guard_releases_on_error() {
        let first = PickerGuard::acquire().unwrap();
        assert!(PickerGuard::acquire().is_none());
        drop(first);
        let result: Result<(), ()> = (|| {
            let _guard = PickerGuard::acquire().unwrap();
            Err(())
        })();
        assert!(result.is_err());
        assert!(PickerGuard::acquire().is_some());
    }

    #[test]
    fn filesystem_path_preserves_long_unicode_and_unpaired_utf16() {
        let mut wide: Vec<u16> = format!(r"C:\媒体\{}\字幕📝.srt", "long".repeat(100))
            .encode_utf16()
            .collect();
        wide.insert(wide.len() - 4, 0xd800);
        let path = path_from_wide(&wide);
        assert_eq!(path.as_os_str().encode_wide().collect::<Vec<_>>(), wide);
    }
}
