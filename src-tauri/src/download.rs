use std::io::Write;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

pub async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    expected_sha256: &str,
    progress: impl Fn(u64, u64),
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    validate_expected_hash(expected_sha256)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let partial = partial_path(destination);
    let _ = std::fs::remove_file(&partial);
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {} for {url}", response.status()).into());
    }

    let total = response.content_length().unwrap_or(0);
    let mut downloaded = 0_u64;
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::create(&partial)?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        progress(downloaded, total);
    }
    file.sync_all()?;
    drop(file);

    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "SHA-256 mismatch for {url}: expected {expected_sha256}, received {actual}"
        )
        .into());
    }

    if destination.exists() {
        std::fs::remove_file(destination)?;
    }
    std::fs::rename(&partial, destination)?;
    Ok(downloaded)
}

pub fn verify_file_sha256(path: &Path, expected_sha256: &str) -> bool {
    sha256_file(path)
        .map(|actual| actual.eq_ignore_ascii_case(expected_sha256))
        .unwrap_or(false)
}

pub fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut HashWriter(&mut hasher))?;
    Ok(format!("{:x}", hasher.finalize()))
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_expected_hash(expected_sha256: &str) -> Result<(), String> {
    if expected_sha256.len() == 64 && expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("Expected SHA-256 must be 64 hexadecimal characters".into())
    }
}

fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "download".into());
    name.push(".part");
    destination.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_a_known_sha256() {
        let path = std::env::temp_dir().join(format!("annotate-sha256-{}.txt", std::process::id()));
        std::fs::write(&path, b"annotate").unwrap();
        assert!(verify_file_sha256(
            &path,
            "9c299759567c9c3963748e57fb44cc7f676b69219e955a0d74a3bebb3607c78d"
        ));
        let _ = std::fs::remove_file(path);
    }
}
