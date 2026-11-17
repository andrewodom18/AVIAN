use crate::{Result, SecretProtector, StorageError};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Serialize)]
struct Request<'a> {
    action: &'a str,
    path: &'a str,
    data: &'a str,
    maximum_bytes: usize,
}
#[derive(Deserialize)]
struct Response {
    ok: bool,
    data: String,
}
impl Drop for Response {
    fn drop(&mut self) {
        self.data.zeroize();
    }
}

fn invoke(
    action: &str,
    path: &Path,
    bytes: &[u8],
    maximum_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    let root = std::env::var_os("SystemRoot").ok_or(StorageError::BackendUnavailable)?;
    let backend = PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    if !backend.is_absolute() || !backend.is_file() {
        return Err(StorageError::BackendUnavailable);
    }
    let absolute = if path.as_os_str().is_empty() {
        PathBuf::new()
    } else {
        std::path::absolute(path)?
    };
    let encoded = Zeroizing::new(STANDARD.encode(bytes));
    let request = Request {
        action,
        path: absolute.to_str().ok_or(StorageError::BackendFailed)?,
        data: &encoded,
        maximum_bytes,
    };
    let payload =
        Zeroizing::new(serde_json::to_vec(&request).map_err(|_| StorageError::BackendFailed)?);
    let mut child = Command::new(backend)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            include_str!("windows.ps1"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| StorageError::BackendUnavailable)?;
    let stdout = child.stdout.take().ok_or(StorageError::BackendFailed)?;
    let output_limit = maximum_bytes
        .saturating_add(bytes.len())
        .saturating_add(128 * 1024)
        .saturating_mul(2);
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut output = Zeroizing::new(Vec::new());
        stdout
            .take((output_limit as u64).saturating_add(1))
            .read_to_end(&mut output)?;
        std::io::Result::Ok(output)
    });
    let mut stdin = child.stdin.take().ok_or(StorageError::BackendFailed)?;
    let writer = std::thread::spawn(move || stdin.write_all(&payload));
    let started = Instant::now();
    let successful = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if started.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let input_written = writer
        .join()
        .map_err(|_| StorageError::BackendFailed)?
        .is_ok();
    let output = reader.join().map_err(|_| StorageError::BackendFailed)??;
    if !successful || !input_written || output.len() > output_limit {
        return Err(StorageError::BackendFailed);
    }
    let response: Response =
        serde_json::from_slice(&output).map_err(|_| StorageError::BackendFailed)?;
    if !response.ok {
        return Err(StorageError::BackendFailed);
    }
    STANDARD
        .decode(&response.data)
        .map(Zeroizing::new)
        .map_err(|_| StorageError::BackendFailed)
}

pub(super) fn validate_directory(path: &Path) -> Result<()> {
    invoke("directory", path, &[], 0).map(|_| ())
}
pub(super) fn create_directory(path: &Path) -> Result<()> {
    invoke("create_directory", path, &[], 0).map(|_| ())
}
pub(super) fn validate_file(path: &Path) -> Result<()> {
    invoke("validate", path, &[], 0).map(|_| ())
}
pub(super) fn read(path: &Path, maximum_bytes: usize) -> Result<Zeroizing<Vec<u8>>> {
    invoke("read", path, &[], maximum_bytes)
}
pub(super) fn write(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    invoke(if replace { "replace" } else { "create" }, path, bytes, 0).map(|_| ())
}
pub(super) fn open_lock(path: &Path) -> Result<File> {
    invoke("lock", path, &[], 0)?;
    Ok(OpenOptions::new().read(true).write(true).open(path)?)
}
pub(super) struct CurrentUserProtector;
impl SecretProtector for CurrentUserProtector {
    fn protect(&self, plaintext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        invoke("protect", Path::new(""), plaintext, 0)
    }
    fn unprotect(&self, protected: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        invoke("unprotect", Path::new(""), protected, protected.len())
    }
}
