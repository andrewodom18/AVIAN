use avian_storage::{
    StorageError, atomic_write_private_file, create_private_directory, create_private_file,
    create_secret_file, read_private_file, read_secret_file, validate_private_file,
};

#[test]
fn create_only_identity_and_atomic_successor_preserve_complete_files() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    create_private_directory(&state).unwrap();
    let path = state.join("identity");
    create_private_file(&path, b"original identity").unwrap();
    validate_private_file(&path).unwrap();
    assert!(create_private_file(&path, b"replacement identity").is_err());
    assert_eq!(
        &*read_private_file(&path, 100).unwrap(),
        b"original identity"
    );
    atomic_write_private_file(&path, b"complete successor").unwrap();
    assert_eq!(
        &*read_private_file(&path, 100).unwrap(),
        b"complete successor"
    );
    assert!(matches!(
        read_private_file(&path, 3),
        Err(StorageError::TooLarge) | Err(StorageError::BackendFailed)
    ));
}

#[cfg(not(target_os = "android"))]
#[test]
fn native_secret_round_trip_keeps_existing_unix_format() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state");
    create_private_directory(&state).unwrap();
    let path = state.join("key");
    let key = b"existing signing seed format\n";
    create_secret_file(&path, key).unwrap();
    assert_eq!(&*read_secret_file(&path, 100).unwrap(), key);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::read(&path).unwrap(), key);
        let legacy = state.join("legacy-key");
        std::fs::write(&legacy, key).unwrap();
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(&*read_secret_file(&legacy, 100).unwrap(), key);
    }
    #[cfg(windows)]
    assert!(
        !std::fs::read(&path)
            .unwrap()
            .windows(key.len())
            .any(|window| window == key)
    );
}

#[cfg(unix)]
#[test]
fn unsafe_permissions_links_and_shared_writable_parents_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("key");
    create_private_file(&path, b"identity").unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        validate_private_file(&path),
        Err(StorageError::InsecurePermissions)
    ));
    assert!(atomic_write_private_file(&path, b"replacement").is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let linked = directory.path().join("linked");
    symlink(&path, &linked).unwrap();
    assert!(read_private_file(&linked, 100).is_err());
    assert!(atomic_write_private_file(&linked, b"replacement").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"identity");
    std::fs::remove_file(&linked).unwrap();
    std::fs::hard_link(&path, &linked).unwrap();
    assert!(validate_private_file(&path).is_err());
    std::fs::remove_file(&linked).unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(create_private_file(directory.path().join("other"), b"identity").is_err());
    assert!(read_private_file(&path, 100).is_err());
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[test]
fn concurrent_readers_observe_only_complete_commits() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state");
    create_private_file(&path, &[1; 4096]).unwrap();
    let complete = Arc::new(AtomicBool::new(false));
    let reading_complete = Arc::clone(&complete);
    let reading_path = path.clone();
    let reader = std::thread::spawn(move || {
        let mut reads = 0;
        while !reading_complete.load(Ordering::SeqCst) || reads < 100 {
            let data = read_private_file(&reading_path, 4096).unwrap();
            assert_eq!(data.len(), 4096);
            assert!(data.iter().all(|byte| *byte == data[0]));
            reads += 1;
        }
    });
    for generation in 2..20 {
        atomic_write_private_file(&path, &[generation; 4096]).unwrap();
    }
    complete.store(true, Ordering::SeqCst);
    reader.join().unwrap();
}

#[test]
fn absent_host_wrapping_never_creates_a_plaintext_key() {
    struct Unavailable;
    impl avian_storage::SecretProtector for Unavailable {
        fn protect(&self, _: &[u8]) -> avian_storage::Result<zeroize::Zeroizing<Vec<u8>>> {
            Err(StorageError::BackendUnavailable)
        }
        fn unprotect(&self, _: &[u8]) -> avian_storage::Result<zeroize::Zeroizing<Vec<u8>>> {
            Err(StorageError::BackendUnavailable)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host-key");
    assert!(
        avian_storage::create_secret_file_with_protector(&path, b"secret", &Unavailable).is_err()
    );
    assert!(!path.exists());
}
