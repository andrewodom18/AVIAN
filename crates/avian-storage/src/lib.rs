//! Private files and platform-protected key material, without changing legacy Unix formats.

use std::{fs::File, path::Path};
use thiserror::Error;
use zeroize::Zeroizing;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("private storage requires a regular owner-only file in an owner-controlled directory")]
    InsecurePermissions,
    #[error("a protected platform storage adapter is required")]
    BackendUnavailable,
    #[error("platform storage operation failed")]
    BackendFailed,
    #[error("private file exceeds the permitted size")]
    TooLarge,
    #[error("protected key format is invalid")]
    InvalidEnvelope,
    #[error("randomness is unavailable")]
    Randomness,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, StorageError>;

const ENVELOPE: &[u8] = b"AVIAN-PROTECTED-KEY-v1\n";

/// The Android host must wrap and unwrap through its protected key store. A missing
/// host adapter never falls back to plaintext. Implementations must not log input.
pub trait SecretProtector: Send + Sync {
    fn protect(&self, plaintext: &[u8]) -> Result<Zeroizing<Vec<u8>>>;
    fn unprotect(&self, protected: &[u8]) -> Result<Zeroizing<Vec<u8>>>;
}

pub fn read_private_file(
    path: impl AsRef<Path>,
    maximum_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    platform::read(path.as_ref(), maximum_bytes)
}

pub fn validate_private_file(path: impl AsRef<Path>) -> Result<()> {
    platform::validate_file(path.as_ref())
}

pub fn validate_private_directory(path: impl AsRef<Path>) -> Result<()> {
    platform::validate_directory(path.as_ref())
}

/// Creates one private directory. Existing directories are validated, never repaired silently.
pub fn create_private_directory(path: impl AsRef<Path>) -> Result<()> {
    platform::create_directory(path.as_ref())
}

/// Commits a complete owner-only file without replacing an existing identity.
pub fn create_private_file(path: impl AsRef<Path>, bytes: &[u8]) -> Result<()> {
    platform::write(path.as_ref(), bytes, false)
}

/// Flushes a same-directory temporary file before replacing the destination.
/// An interrupted write leaves the previous complete file authoritative. Orphaned
/// private temporary files are never treated as committed state on subsequent reads.
pub fn atomic_write_private_file(path: impl AsRef<Path>, bytes: &[u8]) -> Result<()> {
    platform::write(path.as_ref(), bytes, true)
}

pub fn open_private_lock(path: impl AsRef<Path>) -> Result<File> {
    platform::open_lock(path.as_ref())
}

pub fn create_secret_file(path: impl AsRef<Path>, plaintext: &[u8]) -> Result<()> {
    #[cfg(target_os = "android")]
    {
        let _ = (path, plaintext);
        Err(StorageError::BackendUnavailable)
    }
    #[cfg(all(unix, not(target_os = "android")))]
    {
        create_private_file(path, plaintext)
    }
    #[cfg(windows)]
    {
        create_secret_file_with_protector(path, plaintext, &windows::CurrentUserProtector)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, plaintext);
        Err(StorageError::BackendUnavailable)
    }
}

pub fn read_secret_file(
    path: impl AsRef<Path>,
    maximum_bytes: usize,
) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(target_os = "android")]
    {
        let _ = (path, maximum_bytes);
        Err(StorageError::BackendUnavailable)
    }
    #[cfg(all(unix, not(target_os = "android")))]
    {
        read_private_file(path, maximum_bytes)
    }
    #[cfg(windows)]
    {
        read_secret_file_with_protector(path, maximum_bytes, &windows::CurrentUserProtector)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, maximum_bytes);
        Err(StorageError::BackendUnavailable)
    }
}

pub fn create_secret_file_with_protector(
    path: impl AsRef<Path>,
    plaintext: &[u8],
    protector: &dyn SecretProtector,
) -> Result<()> {
    let protected = protector.protect(plaintext)?;
    if protected.is_empty() || protected.as_slice() == plaintext {
        return Err(StorageError::BackendFailed);
    }
    let mut document = Zeroizing::new(Vec::with_capacity(ENVELOPE.len() + protected.len()));
    document.extend_from_slice(ENVELOPE);
    document.extend_from_slice(&protected);
    create_private_file(path, &document)
}

pub fn read_secret_file_with_protector(
    path: impl AsRef<Path>,
    maximum_bytes: usize,
    protector: &dyn SecretProtector,
) -> Result<Zeroizing<Vec<u8>>> {
    let protected = read_private_file(path, maximum_bytes.saturating_add(64 * 1024))?;
    let ciphertext = protected
        .strip_prefix(ENVELOPE)
        .ok_or(StorageError::InvalidEnvelope)?;
    let plaintext = protector.unprotect(ciphertext)?;
    if plaintext.len() > maximum_bytes {
        return Err(StorageError::TooLarge);
    }
    Ok(plaintext)
}

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

#[cfg(unix)]
impl From<rustix::io::Errno> for StorageError {
    fn from(error: rustix::io::Errno) -> Self {
        if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) {
            Self::InsecurePermissions
        } else {
            Self::Io(error.into())
        }
    }
}
