use crate::{Result, StorageError};
use rustix::fs::{self, AtFlags, Mode, OFlags};
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};
use zeroize::Zeroizing;

fn check_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
        || metadata.nlink() > 1
    {
        return Err(StorageError::InsecurePermissions);
    }
    Ok(())
}

fn check_directory(directory: &File) -> Result<()> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(StorageError::InsecurePermissions);
    }
    Ok(())
}

fn directory(path: &Path) -> Result<File> {
    let descriptor = fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let directory = File::from(descriptor);
    check_directory(&directory)?;
    Ok(directory)
}

fn parent(path: &Path) -> Result<(File, &std::ffi::OsStr)> {
    let filename = path.file_name().ok_or(StorageError::InsecurePermissions)?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok((directory(parent)?, filename))
}

fn open(parent: &File, filename: &std::ffi::OsStr, flags: OFlags) -> Result<File> {
    let descriptor = fs::openat(
        parent,
        filename,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR,
    )?;
    let file = File::from(descriptor);
    check_file(&file)?;
    Ok(file)
}

pub(super) fn validate_directory(path: &Path) -> Result<()> {
    let directory = directory(path)?;
    if directory.metadata()?.permissions().mode() & 0o077 != 0 {
        return Err(StorageError::InsecurePermissions);
    }
    Ok(())
}

pub(super) fn create_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let _parent = parent(path)?;
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => validate_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => validate_directory(path),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn validate_file(path: &Path) -> Result<()> {
    let (parent, filename) = parent(path)?;
    open(&parent, filename, OFlags::RDONLY).map(|_| ())
}

pub(super) fn read(path: &Path, maximum_bytes: usize) -> Result<Zeroizing<Vec<u8>>> {
    let (parent, filename) = parent(path)?;
    let file = open(&parent, filename, OFlags::RDONLY)?;
    let mut content = Zeroizing::new(Vec::new());
    file.take(maximum_bytes.saturating_add(1) as u64)
        .read_to_end(&mut content)?;
    if content.len() > maximum_bytes {
        return Err(StorageError::TooLarge);
    }
    Ok(content)
}

pub(super) fn open_lock(path: &Path) -> Result<File> {
    let (parent, filename) = parent(path)?;
    open(&parent, filename, OFlags::RDWR | OFlags::CREATE)
}

struct PendingFile {
    parent: File,
    temporary_name: OsString,
    file: File,
}

impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::unlinkat(&self.parent, &self.temporary_name, AtFlags::empty());
    }
}

impl PendingFile {
    fn create(parent: File) -> Result<Self> {
        let mut random = [0u8; 24];
        getrandom::fill(&mut random).map_err(|_| StorageError::Randomness)?;
        let name = format!(
            ".avian-storage-{}.tmp",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let temporary_name = OsString::from(name);
        let file = open(
            &parent,
            &temporary_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
        )?;
        Ok(Self {
            parent,
            temporary_name,
            file,
        })
    }

    fn commit(mut self, filename: &std::ffi::OsStr, bytes: &[u8], replace: bool) -> Result<()> {
        self.file.write_all(bytes)?;
        self.file.sync_all()?;
        if replace {
            match open(&self.parent, filename, OFlags::RDONLY) {
                Ok(_) => (),
                Err(StorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
            fs::renameat(&self.parent, &self.temporary_name, &self.parent, filename)?;
        } else {
            #[cfg(any(
                target_os = "linux",
                target_os = "android",
                target_os = "macos",
                target_os = "ios"
            ))]
            {
                let flags = fs::RenameFlags::NOREPLACE;
                fs::renameat_with(
                    &self.parent,
                    &self.temporary_name,
                    &self.parent,
                    filename,
                    flags,
                )?;
            }
            #[cfg(not(any(
                target_os = "linux",
                target_os = "android",
                target_os = "macos",
                target_os = "ios"
            )))]
            return Err(StorageError::BackendUnavailable);
        }
        self.parent.sync_all()?;
        Ok(())
    }
}

pub(super) fn write(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let (parent, filename) = parent(path)?;
    PendingFile::create(parent)?.commit(filename, bytes, replace)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abrupt_writer_exit_leaves_the_previous_document_recoverable() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("membership.json");
        write(&destination, b"committed", false).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "unix::tests::abrupt_writer_fixture",
                "--nocapture",
            ])
            .env("AVIAN_STORAGE_INTERRUPTION_FIXTURE", &destination)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(73));
        assert_eq!(&*read(&destination, 100).unwrap(), b"committed");
        write(&destination, b"recovered successor", true).unwrap();
        assert_eq!(&*read(&destination, 100).unwrap(), b"recovered successor");
    }

    #[test]
    fn abrupt_writer_fixture() {
        let Some(destination) = std::env::var_os("AVIAN_STORAGE_INTERRUPTION_FIXTURE") else {
            return;
        };
        let (parent, _) = parent(Path::new(&destination)).unwrap();
        let mut pending = PendingFile::create(parent).unwrap();
        pending.file.write_all(b"incomplete").unwrap();
        pending.file.sync_all().unwrap();
        std::process::exit(73);
    }

    #[test]
    fn interrupted_temporary_write_never_replaces_committed_state() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("membership.json");
        write(&destination, b"committed", false).unwrap();
        let (parent, _) = parent(&destination).unwrap();
        let mut pending = PendingFile::create(parent).unwrap();
        pending.file.write_all(b"incomplete").unwrap();
        pending.file.sync_all().unwrap();
        assert_eq!(&*read(&destination, 100).unwrap(), b"committed");
        drop(pending);
        write(&destination, b"successor", true).unwrap();
        assert_eq!(&*read(&destination, 100).unwrap(), b"successor");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
