use super::types::SessionPreview;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
#[cfg(unix)]
use std::fs::File;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    key: String,
    birth_verified: bool,
}

fn filesystem_identity(
    platform: &[u8],
    parts: &[&[u8]],
    birth: Option<SystemTime>,
) -> DirectoryIdentity {
    let mut digest = Sha256::new();
    digest.update(b"codex-x/session-directory-identity/v1\0");
    digest.update(platform);
    digest.update(b"\0");
    for part in parts {
        digest.update(part);
    }
    if let Some(birth) = birth {
        digest.update(b"\0birth\0");
        let (sign, age) = match birth.duration_since(UNIX_EPOCH) {
            Ok(age) => (b'+', age),
            Err(before_epoch) => (b'-', before_epoch.duration()),
        };
        digest.update([sign]);
        digest.update(age.as_secs().to_le_bytes());
        digest.update(age.subsec_nanos().to_le_bytes());
    } else {
        digest.update(b"\0no-birth\0");
    }
    DirectoryIdentity {
        key: format!("filesystem:{:x}", digest.finalize()),
        birth_verified: birth.is_some(),
    }
}

#[cfg(any(unix, test))]
fn unix_directory_identity(
    device: u64,
    inode: u64,
    birth: Option<SystemTime>,
) -> Option<DirectoryIdentity> {
    // Some virtual or unsupported filesystems expose no usable inode number.
    if inode == 0 {
        return None;
    }
    Some(filesystem_identity(
        b"unix",
        &[&device.to_le_bytes(), &inode.to_le_bytes()],
        birth,
    ))
}

#[cfg(any(windows, test))]
fn windows_directory_identity(
    volume: u64,
    identifier: &[u8; 16],
    birth: Option<SystemTime>,
) -> Option<DirectoryIdentity> {
    // The complete 128-bit ID is needed for ReFS; the old 64-bit file index
    // cannot safely distinguish all directories on that filesystem.
    if identifier.iter().all(|byte| *byte == 0) {
        return None;
    }
    Some(filesystem_identity(
        b"windows",
        &[&volume.to_le_bytes(), identifier],
        birth,
    ))
}

#[cfg(unix)]
fn native_directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {
    use std::os::unix::fs::MetadataExt;

    let handle = File::open(path)?;
    let metadata = handle.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    unix_directory_identity(metadata.dev(), metadata.ino(), metadata.created().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "directory has no stable inode"))
}

#[cfg(windows)]
fn native_directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    // READ_ATTRIBUTES plus BACKUP_SEMANTICS opens directories without requiring
    // permission to list them. No content is read and no filesystem state is changed.
    let handle = OpenOptions::new()
        .read(true)
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let metadata = handle.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    let mut info = FILE_ID_INFO::default();
    // The owned read-only handle stays alive while Windows fills this exact
    // FILE_ID_INFO buffer. Unsupported filesystems return an error and fall back.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    windows_directory_identity(
        info.VolumeSerialNumber,
        &info.FileId.Identifier,
        metadata.created().ok(),
    )
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "directory has no stable file ID",
        )
    })
}

#[cfg(not(any(unix, windows)))]
fn native_directory_identity(_path: &Path) -> io::Result<DirectoryIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory identity is unavailable",
    ))
}

fn unresolved_directory_identity(path: &str) -> DirectoryIdentity {
    DirectoryIdentity {
        key: format!("path:{path}"),
        birth_verified: false,
    }
}

fn directory_lookup(path: &str) -> DirectoryIdentity {
    let filesystem_path = Path::new(path);
    // Historical relative cwd values cannot be resolved against this app's
    // process cwd. Check the path kind before opening, including FIFO/device
    // values that must never be treated as a workspace directory.
    if !filesystem_path.is_absolute()
        || !std::fs::metadata(filesystem_path).is_ok_and(|metadata| metadata.is_dir())
    {
        return unresolved_directory_identity(path);
    }
    native_directory_identity(filesystem_path)
        .unwrap_or_else(|_| unresolved_directory_identity(path))
}

#[cfg(test)]
fn directory_identity(path: &str) -> String {
    directory_lookup(path).key
}

fn pin_scope_key(path: &str, identity: &DirectoryIdentity) -> String {
    if identity.birth_verified {
        identity.key.clone()
    } else {
        format!("path:{path}")
    }
}

/// The explicitly selected Codex home may use its native identity. Historical
/// workspace paths remain lexical: opening or probing them merely to group
/// sessions can request access to protected macOS folders such as Documents.
/// Preserve their complete spelling on every platform; without inspecting a
/// directory, volume defaults cannot exclude per-directory case sensitivity.
pub(super) fn session_directory_identities(
    codex_dir: &Path,
    sessions: &[SessionPreview],
) -> (HashMap<String, String>, String) {
    let mut identities = HashMap::new();
    let home = codex_dir.display().to_string();
    let home_identity = directory_lookup(&home);
    let pin_scope = pin_scope_key(&home, &home_identity);
    identities.insert(home.clone(), home_identity.key);
    for path in sessions.iter().filter_map(|session| session.cwd.as_deref()) {
        identities
            .entry(path.to_string())
            .or_insert_with(|| unresolved_directory_identity(path).key);
    }
    (identities, pin_scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TestDirectory {
        path: PathBuf,
        _directory: tempfile::TempDir,
    }

    impl TestDirectory {
        fn new() -> Self {
            let mut builder = tempfile::Builder::new();
            builder.prefix("codex-x-directory-identity-");
            let directory = match std::env::var_os("CODEXX_TEST_CASE_DIRECTORY_ROOT") {
                Some(root) => builder.tempdir_in(root),
                None => builder.tempdir(),
            }
            .expect("create temporary directory identity fixture");
            Self {
                path: directory.path().to_path_buf(),
                _directory: directory,
            }
        }
    }

    fn preview(id: &str, cwd: Option<String>) -> SessionPreview {
        SessionPreview {
            id: id.to_string(),
            title: id.to_string(),
            first_user_message: None,
            cwd,
            model_provider: None,
            model: None,
            rollout_path: None,
            updated_at_ms: None,
            archived: false,
            has_user_event: false,
            is_subagent: false,
            needs_sync: false,
        }
    }

    #[test]
    fn existing_directory_and_lexical_alias_have_the_same_stable_identity() {
        let fixture = TestDirectory::new();
        let folder = fixture.path.join("Workspace");
        fs::create_dir(&folder).unwrap();
        let direct = directory_identity(&folder.display().to_string());
        let alias = directory_identity(&folder.join(".").display().to_string());
        assert!(direct.starts_with("filesystem:"));
        assert_eq!(direct, alias);
        assert_eq!(direct, directory_identity(&folder.display().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_directory_alias_resolves_to_the_same_identity() {
        let fixture = TestDirectory::new();
        let folder = fixture.path.join("Workspace");
        let alias = fixture.path.join("WorkspaceAlias");
        fs::create_dir(&folder).unwrap();
        std::os::unix::fs::symlink(&folder, &alias).unwrap();
        assert_eq!(
            directory_identity(&folder.display().to_string()),
            directory_identity(&alias.display().to_string())
        );
    }

    #[test]
    fn case_only_names_follow_the_actual_fixture_filesystem() {
        let fixture = TestDirectory::new();
        let upper = fixture.path.join("CaseDirectory");
        let lower = fixture.path.join("casedirectory");
        fs::create_dir(&upper).unwrap();
        let separate_directories = match fs::create_dir(&lower) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(error) => panic!("create case-only directory fixture: {error}"),
        };
        let upper_key = directory_identity(&upper.display().to_string());
        let lower_key = directory_identity(&lower.display().to_string());
        assert!(upper_key.starts_with("filesystem:"));
        assert!(lower_key.starts_with("filesystem:"));
        if separate_directories {
            assert_ne!(
                upper_key, lower_key,
                "case-sensitive directories must remain distinct"
            );
        } else {
            assert_eq!(
                upper_key, lower_key,
                "case aliases of the same directory must match"
            );
        }
    }

    #[test]
    fn relative_paths_never_borrow_identity_from_the_application_working_directory() {
        for raw in [".", "./", "src", "foo", "C:Project", " relative\\Name "] {
            assert_eq!(directory_identity(raw), format!("path:{raw}"));
        }
    }

    #[test]
    fn missing_and_nondirectory_paths_keep_the_complete_original_spelling() {
        let fixture = TestDirectory::new();
        for suffix in ["Missing", "missing", "Missing\\Name ", " missing "] {
            let raw = fixture.path.join(suffix).display().to_string();
            assert_eq!(directory_identity(&raw), format!("path:{raw}"));
        }
        let file = fixture.path.join("a-file");
        fs::write(&file, b"read-only identity lookup must not alter contents").unwrap();
        let raw = file.display().to_string();
        assert_eq!(directory_identity(&raw), format!("path:{raw}"));
        assert_eq!(
            fs::read(&file).unwrap(),
            b"read-only identity lookup must not alter contents"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_falls_back_without_normalizing_its_name() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = TestDirectory::new();
        let directory = fixture.path.join("UnreadableDirectory");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0)).unwrap();
        let raw = directory.display().to_string();
        let readable_by_process = File::open(&directory).is_ok();
        let identity = directory_identity(&raw);
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        if !readable_by_process {
            assert_eq!(identity, format!("path:{raw}"));
        } else {
            // A privileged test runner can bypass the permission restriction.
            assert!(identity.starts_with("filesystem:"));
        }
    }

    #[test]
    fn empty_native_identifiers_are_rejected_and_full_windows_file_ids_are_used() {
        assert!(unix_directory_identity(1, 0, None).is_none());
        assert!(windows_directory_identity(1, &[0; 16], None).is_none());
        let mut first = [0; 16];
        first[0] = 7;
        let mut second = first;
        second[15] = 1;
        assert_ne!(
            windows_directory_identity(1, &first, None),
            windows_directory_identity(1, &second, None),
            "ReFS IDs that differ only beyond the old 64-bit index must not merge"
        );
    }

    #[test]
    fn creation_generation_includes_epoch_sign_full_seconds_and_nanoseconds() {
        use std::time::Duration;
        // Windows SystemTime uses 100 ns intervals; use exactly representable
        // fractions while still testing that subsecond birth precision matters.
        let created = UNIX_EPOCH + Duration::new(1234, 100);
        let original = unix_directory_identity(1, 9, Some(created)).unwrap();
        assert!(original.birth_verified);
        assert_eq!(
            original,
            unix_directory_identity(1, 9, Some(created)).unwrap()
        );
        for new_generation in [
            UNIX_EPOCH + Duration::new(1234, 200),
            UNIX_EPOCH + Duration::new(1234 + (1u64 << 32), 100),
            UNIX_EPOCH - Duration::new(1234, 100),
        ] {
            assert_ne!(
                original,
                unix_directory_identity(1, 9, Some(new_generation)).unwrap(),
                "reused inode numbers from another directory generation must get different keys"
            );
        }
        let file_id = [7; 16];
        assert_ne!(
            windows_directory_identity(1, &file_id, Some(created)),
            windows_directory_identity(1, &file_id, Some(created + Duration::from_nanos(100)))
        );
    }

    #[test]
    fn persistent_pin_scope_requires_verified_birth_and_preserves_raw_fallback_paths() {
        let transient = unix_directory_identity(1, 9, None).unwrap();
        assert!(transient.key.starts_with("filesystem:"));
        assert!(!transient.birth_verified);
        for path in ["/Home/Codex", "/Home/codex", "/Home/Codex\\ "] {
            assert_eq!(pin_scope_key(path, &transient), format!("path:{path}"));
        }
        let verified = unix_directory_identity(1, 9, Some(UNIX_EPOCH)).unwrap();
        assert_eq!(pin_scope_key("/Home/Codex", &verified), verified.key);
        assert_eq!(pin_scope_key("/Home/codex", &verified), verified.key);
    }

    #[test]
    fn changing_directory_contents_does_not_change_birth_identity_or_pin_scope() {
        let fixture = TestDirectory::new();
        let directory = fixture.path.join("Workspace");
        fs::create_dir(&directory).unwrap();
        let raw = directory.display().to_string();
        let before = directory_lookup(&raw);
        fs::write(directory.join("first.txt"), b"first").unwrap();
        fs::write(directory.join("first.txt"), b"updated").unwrap();
        fs::write(directory.join("second.txt"), b"second").unwrap();
        fs::remove_file(directory.join("first.txt")).unwrap();
        let after = directory_lookup(&raw);
        assert_eq!(
            before, after,
            "mtime/ctime changes must not replace the directory birth identity"
        );
        assert_eq!(pin_scope_key(&raw, &before), pin_scope_key(&raw, &after));
    }

    #[test]
    fn status_identities_include_only_its_home_and_loaded_workspaces_and_isolate_homes() {
        let fixture = TestDirectory::new();
        let first_home = fixture.path.join("CodexHomeA");
        let second_home = fixture.path.join("CodexHomeB");
        let workspace = fixture.path.join("Project");
        let unused = fixture.path.join("Unused");
        for directory in [&first_home, &second_home, &workspace, &unused] {
            fs::create_dir(directory).unwrap();
        }
        let config = first_home.join("config.toml");
        fs::write(&config, b"model_provider = \"openai\"\n").unwrap();
        let cwd = workspace.display().to_string();
        let sessions = vec![
            preview("first", Some(cwd.clone())),
            preview("second", Some(cwd.clone())),
            preview("unknown", None),
        ];
        let (first, first_scope) = session_directory_identities(&first_home, &sessions);
        let (second, second_scope) = session_directory_identities(&second_home, &sessions);
        assert_eq!(first.len(), 2);
        assert!(first.contains_key(&first_home.display().to_string()));
        assert!(first.contains_key(&cwd));
        assert!(!first.contains_key(&unused.display().to_string()));
        assert_ne!(
            first[&first_home.display().to_string()],
            second[&second_home.display().to_string()]
        );
        assert_ne!(first_scope, second_scope);
        assert_eq!(first[&cwd], second[&cwd]);
        assert_eq!(fs::read(&config).unwrap(), b"model_provider = \"openai\"\n");
    }
}
