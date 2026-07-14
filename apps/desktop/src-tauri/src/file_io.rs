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
        fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
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
}
