//! Windows child-process lifetime, launch flags, and bundled FFmpeg artifact.
use tokio::process::{Child, Command};

/// FFmpeg binary filename
pub const FFMPEG_FILENAME: &str = "ffmpeg.exe";

/// FFmpeg download URL (gyan.dev essentials build — Windows x64, ~80 MB zip)
pub const FFMPEG_URL: &str =
    "https://pub-e97b79d01db7403587a869136310a65d.r2.dev/ffmpeg/ffmpeg.exe";
pub const FFMPEG_SHA256: &str = "1a65d5b0b10d8d9a81d2824a3538046a40ed3607c906b335a166add87613f705";

/// The OS closes this handle on app exit, including crashes and active inference.
pub struct WorkerJob(usize);

impl WorkerJob {
    pub fn attach(child: &Child) -> Result<Self, String> {
        use windows::Win32::{Foundation::HANDLE, System::JobObjects::*};
        unsafe {
            let handle = CreateJobObjectW(None, None).map_err(|e| e.to_string())?;
            let job = Self(handle.0 as usize);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
            .map_err(|e| format!("Could not protect the Whisper worker: {e}"))?;
            let process = child.raw_handle().ok_or("Whisper worker already exited")?;
            AssignProcessToJobObject(handle, HANDLE(process)).map_err(|e| {
                format!("Could not assign the Whisper worker to its process job: {e}")
            })?;
            Ok(job)
        }
    }
}

impl Drop for WorkerJob {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(
                self.0 as *mut _,
            ));
        }
    }
}

/// Keep worker loading below UI work and prevent a console window.
pub fn configure_whisper_worker(command: &mut Command) {
    command.creation_flags(0x0800_0000 | 0x0000_4000);
}

pub fn hide_console(command: &mut Command) {
    command.creation_flags(0x0800_0000);
}

#[cfg(test)]
pub fn test_worker() -> Child {
    let mut command = Command::new("ping.exe");
    command
        .args(["-n", "60", "127.0.0.1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    hide_console(&mut command);
    command.spawn().unwrap()
}

#[cfg(test)]
mod tests {
    #[test]
    fn closing_the_worker_job_terminates_a_busy_child() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut child = super::test_worker();
            let job = super::WorkerJob::attach(&child).unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert!(child.try_wait().unwrap().is_none());
            drop(job);
            tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
                .await
                .expect("worker survived its job")
                .unwrap();
        });
    }
}
