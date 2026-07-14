use crate::error::{CodexxError, Result};
use crate::paths::home_dir;
use std::path::PathBuf;

fn push_existing_candidate(candidates: &mut Vec<PathBuf>, candidate: Option<PathBuf>) {
    let Some(path) = candidate else {
        return;
    };
    if !candidates.iter().any(|item| item == &path) {
        candidates.push(path);
    }
}

pub(crate) fn ccswitch_db_candidates() -> Result<Vec<PathBuf>> {
    let mut candidates = Vec::new();

    if let Ok(value) = std::env::var("CC_SWITCH_HOME") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            push_existing_candidate(
                &mut candidates,
                Some(PathBuf::from(trimmed).join("cc-switch.db")),
            );
        }
    }

    let home = home_dir()?;
    // cc-switch 当前主要使用这个位置，macOS/Windows/Linux 都适用。
    push_existing_candidate(
        &mut candidates,
        Some(home.join(".cc-switch").join("cc-switch.db")),
    );

    // 兼容 Tauri/AppData 风格位置，防止未来或不同发行版变更数据目录。
    if let Some(data_dir) = dirs::data_dir() {
        push_existing_candidate(
            &mut candidates,
            Some(data_dir.join("com.ccswitch.desktop").join("cc-switch.db")),
        );
        push_existing_candidate(
            &mut candidates,
            Some(data_dir.join("cc-switch").join("cc-switch.db")),
        );
        push_existing_candidate(
            &mut candidates,
            Some(data_dir.join("CC Switch").join("cc-switch.db")),
        );
    }
    if let Some(data_local_dir) = dirs::data_local_dir() {
        push_existing_candidate(
            &mut candidates,
            Some(
                data_local_dir
                    .join("com.ccswitch.desktop")
                    .join("cc-switch.db"),
            ),
        );
        push_existing_candidate(
            &mut candidates,
            Some(data_local_dir.join("cc-switch").join("cc-switch.db")),
        );
        push_existing_candidate(
            &mut candidates,
            Some(data_local_dir.join("CC Switch").join("cc-switch.db")),
        );
    }

    #[cfg(target_os = "macos")]
    {
        push_existing_candidate(
            &mut candidates,
            Some(
                home.join("Library")
                    .join("Application Support")
                    .join("com.ccswitch.desktop")
                    .join("cc-switch.db"),
            ),
        );
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            push_existing_candidate(
                &mut candidates,
                Some(
                    PathBuf::from(appdata)
                        .join("com.ccswitch.desktop")
                        .join("cc-switch.db"),
                ),
            );
        }
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            push_existing_candidate(
                &mut candidates,
                Some(
                    PathBuf::from(localappdata)
                        .join("com.ccswitch.desktop")
                        .join("cc-switch.db"),
                ),
            );
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME") {
            push_existing_candidate(
                &mut candidates,
                Some(
                    PathBuf::from(xdg_data_home)
                        .join("com.ccswitch.desktop")
                        .join("cc-switch.db"),
                ),
            );
        }
        push_existing_candidate(
            &mut candidates,
            Some(
                home.join(".local")
                    .join("share")
                    .join("com.ccswitch.desktop")
                    .join("cc-switch.db"),
            ),
        );
    }

    Ok(candidates)
}

pub(crate) fn default_ccswitch_db_path() -> Result<PathBuf> {
    let candidates = ccswitch_db_candidates()?;
    candidates
        .iter()
        .find(|path| path.exists())
        .cloned()
        .or_else(|| candidates.into_iter().next())
        .ok_or_else(|| CodexxError::Config("无法生成 cc-switch 数据库候选路径".to_string()))
}
