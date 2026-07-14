use crate::error::{CodexxError, Result};
#[cfg(test)]
use chrono::Local;
use std::path::PathBuf;

pub(crate) fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().ok_or(CodexxError::NoHomeDir)
}

pub(crate) fn app_home() -> Result<PathBuf> {
    #[cfg(test)]
    {
        use std::sync::OnceLock;
        static TEST_APP_HOME: OnceLock<PathBuf> = OnceLock::new();
        Ok(TEST_APP_HOME
            .get_or_init(|| {
                std::env::temp_dir().join(format!(
                    "codex-x-test-home-{}-{}",
                    std::process::id(),
                    Local::now().timestamp_nanos_opt().unwrap_or_default()
                ))
            })
            .clone())
    }
    #[cfg(not(test))]
    {
        if let Ok(value) = std::env::var("CODEXX_HOME") {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Ok(PathBuf::from(trimmed));
            }
        }
        Ok(home_dir()?.join(".codexx"))
    }
}
