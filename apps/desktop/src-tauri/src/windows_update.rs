//! Stage only bytes already verified by tauri-plugin-updater. MSI remains the
//! default installer; existing NSIS installations keep their compatible path.
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallerKind {
    Msi,
    Nsis,
}

pub(super) struct StagedInstaller {
    dir: tempfile::TempDir,
    executable: PathBuf,
    kind: InstallerKind,
}

fn is_windows_executable(bytes: &[u8]) -> bool {
    if bytes.len() < 64 || bytes.get(..2) != Some(b"MZ") {
        return false;
    }
    let offset = u32::from_le_bytes(bytes[60..64].try_into().unwrap()) as usize;
    offset >= 64
        && offset
            .checked_add(4)
            .is_some_and(|end| bytes.get(offset..end) == Some(b"PE\0\0"))
}

fn installer_kind(bytes: &[u8]) -> Option<InstallerKind> {
    // MSI is a Compound File Binary container. Verify the full signature and
    // header, not merely its extension or a four-byte prefix. The publisher's
    // updater signature has already authenticated the entire payload upstream.
    if bytes.len() >= 512
        && bytes.get(..8) == Some(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1")
        && bytes.get(28..30) == Some(b"\xfe\xff")
        && matches!(
            (bytes[26], bytes[27], bytes[30], bytes[31]),
            (3, 0, 9, 0) | (4, 0, 12, 0)
        )
    {
        Some(InstallerKind::Msi)
    } else if is_windows_executable(bytes) {
        Some(InstallerKind::Nsis)
    } else {
        None
    }
}

fn installer_arguments(kind: InstallerKind, path: &Path, pid: u32) -> Vec<OsString> {
    match kind {
        InstallerKind::Nsis => ["/P", "/R", "/UPDATE"]
            .into_iter()
            .map(OsString::from)
            .chain(std::iter::once(format!("/CODEXXPID={pid}").into()))
            .collect(),
        InstallerKind::Msi => vec![
            "/i".into(),
            path.as_os_str().to_owned(),
            "/passive".into(),
            "/norestart".into(),
            "AUTOLAUNCHAPP=True".into(),
        ],
    }
}

impl StagedInstaller {
    pub(super) fn create(verified_bytes: &[u8]) -> io::Result<Self> {
        let kind = installer_kind(verified_bytes).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "expected a verified Windows MSI or EXE installer",
            )
        })?;
        let dir = tempfile::Builder::new()
            .prefix("Codex-X-update-")
            .tempdir()?;
        let executable = dir.path().join(match kind {
            InstallerKind::Msi => "Codex-X-setup.msi",
            InstallerKind::Nsis => "Codex-X-setup.exe",
        });
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&executable)?;
        file.write_all(verified_bytes)?;
        file.sync_all()?;
        drop(file);
        Ok(Self {
            dir,
            executable,
            kind,
        })
    }

    pub(super) fn launch(self, parent_pid: u32) -> io::Result<()> {
        if parent_pid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid parent PID",
            ));
        }
        let program = match self.kind {
            InstallerKind::Nsis => self.executable.clone(),
            // Use the system installer rather than searching PATH. Windows
            // Installer handles MSI service elevation and the normal progress UI.
            InstallerKind::Msi => {
                PathBuf::from(std::env::var_os("SystemRoot").ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "Windows system directory is unavailable",
                    )
                })?)
                .join("System32")
                .join("msiexec.exe")
            }
        };
        // No shell or interpolated command line. Rust preserves Unicode and
        // spaces as one argument. MSI uses its standard Restart Manager while
        // the caller immediately performs normal Tauri exit; NSIS additionally
        // waits for parent_pid in its existing custom installer template.
        let child = std::process::Command::new(program)
            .args(installer_arguments(self.kind, &self.executable, parent_pid))
            .current_dir(self.dir.path())
            .spawn()?;
        drop(child);
        let _ = self.dir.keep();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pe_fixture() -> Vec<u8> {
        let mut bytes = vec![0; 128];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes
    }

    fn msi_fixture() -> Vec<u8> {
        let mut bytes = vec![0; 512];
        bytes[..8].copy_from_slice(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1");
        bytes[26] = 3;
        bytes[28..30].copy_from_slice(b"\xfe\xff");
        bytes[30] = 9;
        bytes
    }

    #[test]
    fn staging_rejects_html_truncated_and_invalid_installer_headers() {
        for bytes in [
            b"\xd0\xcf\x11\xe0".as_slice(),
            b"<html>login</html>",
            b"MZ",
            &[0; 128],
        ] {
            assert!(StagedInstaller::create(bytes).is_err());
        }
        let mut bytes = pe_fixture();
        bytes[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(StagedInstaller::create(&bytes).is_err());
        let mut bytes = msi_fixture();
        bytes[30] = 0;
        assert!(StagedInstaller::create(&bytes).is_err());
        assert!(StagedInstaller::create(&msi_fixture()[..511]).is_err());
    }

    #[test]
    fn installer_staging_is_unique_and_cancel_cleans_only_its_directory() {
        for bytes in [pe_fixture(), msi_fixture()] {
            let a = StagedInstaller::create(&bytes).unwrap();
            let b = StagedInstaller::create(&bytes).unwrap();
            assert_ne!(a.dir.path(), b.dir.path());
            assert_eq!(std::fs::read(&a.executable).unwrap(), bytes);
            let expected_extension = if a.kind == InstallerKind::Msi {
                "msi"
            } else {
                "exe"
            };
            assert_eq!(a.executable.extension().unwrap(), expected_extension);
            let path = a.dir.path().to_owned();
            drop(a);
            assert!(!path.exists());
            assert!(b.executable.is_file());
        }
    }

    #[test]
    fn nsis_launcher_preserves_legacy_update_arguments() {
        assert_eq!(
            installer_arguments(InstallerKind::Nsis, Path::new("unused.exe"), 123),
            ["/P", "/R", "/UPDATE", "/CODEXXPID=123"].map(OsString::from)
        );
    }

    #[test]
    fn msi_launcher_uses_native_installer_without_nsis_or_user_cli_arguments() {
        let path = Path::new("C:\\用户 文件\\Codex-X-setup.msi");
        assert_eq!(
            installer_arguments(InstallerKind::Msi, path, 123),
            [
                "/i",
                "C:\\用户 文件\\Codex-X-setup.msi",
                "/passive",
                "/norestart",
                "AUTOLAUNCHAPP=True"
            ]
            .map(OsString::from)
        );
    }
}
