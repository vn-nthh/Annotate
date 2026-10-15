use tokio::process::{Child, Command};

// Upstream ffmpeg-static's pinned Apple silicon build, fetched only on request.
pub const FFMPEG_FILENAME: &str = "ffmpeg";
pub const FFMPEG_URL: &str =
    "https://github.com/eugeneware/ffmpeg-static/releases/download/b6.1.1/ffmpeg-darwin-arm64";
pub const FFMPEG_SHA256: &str = "a90e3db6a3fd35f6074b013f948b1aa45b31c6375489d39e572bea3f18336584";

// Normal teardown uses Child::kill_on_drop. The Metal worker watches its parent
// with a dispatch process source, so an app crash also releases its GPU memory.
pub struct WorkerJob;
impl WorkerJob {
    pub fn attach(child: &Child) -> Result<Self, String> {
        child
            .id()
            .map(|_| Self)
            .ok_or("Whisper worker already exited".into())
    }
}
pub fn configure_whisper_worker(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
}
pub fn hide_console(_: &mut Command) {}
#[cfg(test)]
pub fn test_worker() -> Child {
    Command::new("/bin/sleep")
        .arg("60")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}
