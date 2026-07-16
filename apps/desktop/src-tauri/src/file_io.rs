use crate::error::{CodexxError, Result};
use chrono::Local;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::Path;
use toml_edit::DocumentMut;

pub(crate) fn io_err(path: &Path, source: std::io::Error) -> CodexxError {
    CodexxError::Io {
        path: path.display().to_string(),
        source,
    }
}

pub(crate) fn json_err(path: &Path, source: serde_json::Error) -> CodexxError {
    CodexxError::Json {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(target_os = "windows")]
fn metadata_is_file_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::FileTypeExt;
    metadata.file_type().is_symlink_file()
}

#[cfg(not(target_os = "windows"))]
fn metadata_is_file_link(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(target_os = "windows")]
fn metadata_is_directory_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::FileTypeExt;
    metadata.file_type().is_symlink_dir()
}

#[cfg(not(target_os = "windows"))]
fn metadata_is_directory_link(_metadata: &fs::Metadata) -> bool {
    false
}

fn metadata_is_directory_entry(metadata: &fs::Metadata) -> bool {
    metadata.is_dir() && !metadata_is_directory_link(metadata)
}

pub(crate) fn directory_exists(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    metadata_is_directory_entry(&metadata)
        || (!metadata_is_file_link(&metadata)
            && (metadata_is_directory_link(&metadata) || metadata.file_type().is_symlink())
            && fs::metadata(path).is_ok_and(|target| target.is_dir()))
}

fn existing_directory_entry(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata_is_directory_entry(metadata) {
        return Ok(());
    }

    if metadata_is_file_link(metadata) {
        return Err(CodexxError::Config(format!(
            "此链接被创建成了文件链接，不能作为文件夹使用：{}。请在创建链接的工具中重新建立目录链接",
            path.display()
        )));
    }

    if metadata_is_directory_link(metadata) || metadata.file_type().is_symlink() {
        return match fs::metadata(path) {
            Ok(target) if target.is_dir() => Ok(()),
            _ => Err(CodexxError::Config(format!(
                "文件夹链接已失效或目标不是文件夹：{}",
                path.display()
            ))),
        };
    }

    Err(CodexxError::Config(format!(
        "此路径已被同名文件占用，不是文件夹：{}",
        path.display()
    )))
}

pub(crate) fn ensure_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => existing_directory_entry(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir_all(path) {
                Ok(()) => Ok(()),
                Err(create_error) => match fs::symlink_metadata(path) {
                    Ok(metadata) => existing_directory_entry(path, &metadata),
                    Err(_) => Err(io_err(path, create_error)),
                },
            }
        }
        Err(error) => Err(io_err(path, error)),
    }
}

pub(crate) fn read_to_string_if_exists(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(path).map_err(|e| io_err(path, e))
}

pub(crate) fn parse_toml_document(path: &Path, text: &str) -> Result<DocumentMut> {
    if text.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    text.parse::<DocumentMut>().map_err(|e| CodexxError::Toml {
        path: path.display().to_string(),
        message: e.to_string(),
    })
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        ensure_directory(parent)?;
    }
    let tmp = path.with_extension(format!(
        "tmp.{}.{}.{}",
        std::process::id(),
        Local::now().timestamp_nanos_opt().unwrap_or_default(),
        WRITE_COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    {
        let mut file = fs::File::create(&tmp).map_err(|e| io_err(&tmp, e))?;
        file.write_all(bytes).map_err(|e| io_err(&tmp, e))?;
        file.sync_all().map_err(|e| io_err(&tmp, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| io_err(path, e))?;
    Ok(())
}

pub(crate) fn write_text(path: &Path, text: &str) -> Result<()> {
    atomic_write(path, text.as_bytes())
}

pub(crate) fn write_json(path: &Path, value: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|e| json_err(path, e))?;
    write_text(path, &(text + "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "codex-x-file-io-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test directory");
        path
    }

    #[test]
    fn atomic_write_replaces_existing_file_without_temp_residue() {
        let dir = temp_dir("replace");
        let path = dir.join("state.json");
        fs::write(&path, b"old").expect("write original file");

        atomic_write(&path, b"new").expect("replace file atomically");

        assert_eq!(fs::read(&path).expect("read replaced file"), b"new");
        let entries = fs::read_dir(&dir)
            .expect("read test directory")
            .map(|entry| entry.expect("read directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![path.file_name().unwrap().to_os_string()]);

        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn ensure_directory_accepts_existing_and_missing_directories() {
        let root = temp_dir("ensure-directory");
        let existing = root.join("existing");
        let missing = root.join("missing").join("nested");
        fs::create_dir(&existing).expect("create existing directory");

        ensure_directory(&existing).expect("accept existing directory");
        ensure_directory(&missing).expect("create missing directory");

        assert!(missing.is_dir());
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn ensure_directory_rejects_a_file_without_removing_it() {
        let root = temp_dir("ensure-directory-file");
        let occupied = root.join(".codex");
        fs::write(&occupied, "keep me").expect("create occupying file");

        let error = ensure_directory(&occupied).expect_err("file is not a directory");

        assert!(!directory_exists(&occupied));
        assert!(error.to_string().contains("不是文件夹"));
        assert_eq!(
            fs::read_to_string(&occupied).expect("read occupying file"),
            "keep me"
        );
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_directory_accepts_a_directory_symlink_and_rejects_a_broken_link() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("ensure-directory-link");
        let target = root.join("target");
        let link = root.join("linked-codex-home");
        let broken = root.join("broken-codex-home");
        fs::create_dir(&target).expect("create target directory");
        symlink(&target, &link).expect("create directory symlink");
        symlink(root.join("missing-target"), &broken).expect("create broken symlink");

        ensure_directory(&link).expect("accept directory symlink");
        atomic_write(&link.join("config.toml"), b"linked").expect("write through symlink");
        let error = ensure_directory(&broken).expect_err("reject broken symlink");

        assert_eq!(
            fs::read(target.join("config.toml")).expect("read symlink target"),
            b"linked"
        );
        assert!(error.to_string().contains("链接已失效"));
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn ensure_directory_accepts_a_windows_directory_symlink() {
        use std::os::windows::fs::symlink_dir;

        let root = temp_dir("ensure-directory-windows-link");
        let target = root.join("目标 文件夹");
        let link = root.join("linked-codex-home");
        fs::create_dir(&target).expect("create target directory");
        match symlink_dir(&target, &link) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(1314) => {
                fs::remove_dir_all(root).expect("remove test directory");
                return;
            }
            Err(error) => panic!("create directory symlink: {error}"),
        }

        ensure_directory(&link).expect("accept Windows directory symlink");
        assert!(directory_exists(&link));
        atomic_write(&link.join("config.toml"), b"first").expect("first linked write");
        atomic_write(&link.join("config.toml"), b"second").expect("replace linked file");

        assert_eq!(
            fs::read(target.join("config.toml")).expect("read linked target"),
            b"second"
        );
        fs::remove_file(target.join("config.toml")).expect("remove target file");
        fs::remove_dir(&target).expect("remove symlink target");
        let error = ensure_directory(&link).expect_err("reject broken directory symlink");
        assert!(!directory_exists(&link));
        assert!(error.to_string().contains("链接已失效"));
        fs::remove_dir(&link).expect("remove directory symlink");
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn ensure_directory_accepts_a_windows_junction() {
        use std::process::Command;

        let root = temp_dir("ensure-directory-windows-junction");
        let target = root.join("junction-target");
        let link = root.join("junction-codex-home");
        fs::create_dir(&target).expect("create junction target");
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .status()
            .expect("run mklink");
        assert!(status.success(), "create directory junction");

        ensure_directory(&link).expect("accept Windows junction");
        assert!(directory_exists(&link));
        atomic_write(&link.join("config.toml"), b"junction").expect("write through junction");

        assert_eq!(
            fs::read(target.join("config.toml")).expect("read junction target"),
            b"junction"
        );
        fs::remove_dir(&link).expect("remove directory junction");
        fs::remove_dir_all(root).expect("remove test directory");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn ensure_directory_rejects_a_file_link_to_a_directory_without_removing_it() {
        use std::os::windows::fs::symlink_file;

        let root = temp_dir("ensure-directory-windows-file-link");
        let target = root.join("target");
        let link = root.join("linked-codex-home");
        fs::create_dir(&target).expect("create target directory");
        match symlink_file(&target, &link) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(1314) => {
                fs::remove_dir_all(root).expect("remove test directory");
                return;
            }
            Err(error) => panic!("create file link: {error}"),
        }

        let error = ensure_directory(&link).expect_err("reject file link as directory");

        assert!(!directory_exists(&link));
        assert!(error.to_string().contains("文件链接"));
        assert!(fs::symlink_metadata(&link).is_ok());
        assert!(target.is_dir());
        fs::remove_file(&link).expect("remove file link");
        fs::remove_dir_all(root).expect("remove test directory");
    }
}
