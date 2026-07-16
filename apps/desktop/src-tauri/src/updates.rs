use crate::error::{CodexxError, Result};
#[cfg(test)]
use crate::remote::fetch_first_valid_with;
use crate::remote::{fetch_first_valid, RemoteSource};
use semver::Version;
use serde::{Deserialize, Serialize};

const RELEASES_URL: &str = "https://github.com/yynxxxxx/Codex-X/releases";
const SHIELDS_KEY: &str = "Shields CDN";
const GITHUB_KEY: &str = "GitHub Releases";
const RELEASE_SOURCES: [RemoteSource<'static>; 2] = [
    RemoteSource::new(
        SHIELDS_KEY,
        "https://img.shields.io/github/v/release/yynxxxxx/Codex-X.json?display_name=tag",
        Some("application/json"),
    ),
    RemoteSource::new(
        GITHUB_KEY,
        "https://api.github.com/repos/yynxxxxx/Codex-X/releases/latest",
        Some("application/vnd.github+json"),
    ),
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppUpdateInfo {
    latest_version: String,
    html_url: String,
    has_update: bool,
}

#[derive(Debug, Deserialize)]
struct ShieldsRelease {
    value: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedRelease {
    tag: String,
    version: Version,
}

fn parse_release_tag(value: &str) -> Result<ParsedRelease> {
    let tag = value.trim();
    let version_text = tag.strip_prefix('v').unwrap_or(tag);
    if tag.is_empty() || version_text.is_empty() {
        return Err(CodexxError::Config("版本号为空".to_string()));
    }
    let version = Version::parse(version_text)
        .map_err(|_| CodexxError::Config("返回的版本号无效".to_string()))?;
    Ok(ParsedRelease {
        tag: tag.to_string(),
        version,
    })
}

fn parse_shields_release(body: &str) -> Result<ParsedRelease> {
    let release: ShieldsRelease = serde_json::from_str(body)
        .map_err(|_| CodexxError::Config("CDN 版本信息无效".to_string()))?;
    let tag = release
        .value
        .or(release.message)
        .ok_or_else(|| CodexxError::Config("CDN 未返回版本号".to_string()))?;
    parse_release_tag(&tag)
}

fn parse_github_release(body: &str) -> Result<ParsedRelease> {
    let release: GithubRelease = serde_json::from_str(body)
        .map_err(|_| CodexxError::Config("GitHub 版本信息无效".to_string()))?;
    if release.draft || release.prerelease {
        return Err(CodexxError::Config("GitHub 未返回正式版本".to_string()));
    }
    parse_release_tag(&release.tag_name)
}

fn parse_release_response(source: &RemoteSource<'_>, body: &str) -> Result<ParsedRelease> {
    match source.key {
        SHIELDS_KEY => parse_shields_release(body),
        GITHUB_KEY => parse_github_release(body),
        _ => Err(CodexxError::Config("未知版本来源".to_string())),
    }
}

fn update_info(current_version: &str, release: ParsedRelease) -> Result<AppUpdateInfo> {
    let current = Version::parse(current_version)
        .map_err(|_| CodexxError::Config("当前软件版本无效".to_string()))?;
    let html_url = format!("{RELEASES_URL}/tag/{}", release.tag);
    Ok(AppUpdateInfo {
        latest_version: release.tag,
        html_url,
        has_update: release.version > current,
    })
}

#[cfg(test)]
fn check_app_update_with<Fetch>(current_version: &str, fetch: Fetch) -> Result<AppUpdateInfo>
where
    Fetch: FnMut(&RemoteSource<'_>) -> Result<String>,
{
    let release = fetch_first_valid_with(&RELEASE_SOURCES, fetch, parse_release_response)?;
    update_info(current_version, release)
}

fn check_app_update_inner() -> Result<AppUpdateInfo> {
    let release = fetch_first_valid(&RELEASE_SOURCES, parse_release_response)?;
    update_info(env!("CARGO_PKG_VERSION"), release)
}

#[tauri::command]
pub(crate) async fn check_app_update() -> Result<AppUpdateInfo> {
    tauri::async_runtime::spawn_blocking(check_app_update_inner)
        .await
        .map_err(|error| CodexxError::Config(format!("检查软件更新失败: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIELDS_OK: &str = r#"{"value":"v0.2.36","message":"v0.2.36"}"#;
    const GITHUB_OK: &str = r#"{"tag_name":"v0.2.36","draft":false,"prerelease":false}"#;

    #[test]
    fn cdn_success_does_not_call_github() {
        let mut calls = Vec::new();
        let info = check_app_update_with("0.2.35", |source| {
            calls.push(source.key);
            Ok(SHIELDS_OK.to_string())
        })
        .expect("CDN version is valid");

        assert_eq!(calls, [SHIELDS_KEY]);
        assert_eq!(info.latest_version, "v0.2.36");
        assert_eq!(
            info.html_url,
            "https://github.com/yynxxxxx/Codex-X/releases/tag/v0.2.36"
        );
        assert!(info.has_update);
        let json = serde_json::to_value(info).expect("serialize update info");
        assert_eq!(json["latestVersion"], "v0.2.36");
        assert_eq!(json["hasUpdate"], true);
        assert!(json.get("latest_version").is_none());
    }

    #[test]
    fn request_error_falls_back_to_github() {
        let mut calls = Vec::new();
        let info = check_app_update_with("0.2.35", |source| {
            calls.push(source.key);
            if source.key == SHIELDS_KEY {
                Err(CodexxError::Config("offline".to_string()))
            } else {
                Ok(GITHUB_OK.to_string())
            }
        })
        .expect("GitHub fallback succeeds");

        assert_eq!(calls, [SHIELDS_KEY, GITHUB_KEY]);
        assert!(info.has_update);
    }

    #[test]
    fn empty_response_falls_back_to_github() {
        let mut calls = Vec::new();
        let info = check_app_update_with("0.2.35", |source| {
            calls.push(source.key);
            Ok(if source.key == SHIELDS_KEY {
                "  ".to_string()
            } else {
                GITHUB_OK.to_string()
            })
        })
        .expect("GitHub fallback succeeds");

        assert_eq!(calls, [SHIELDS_KEY, GITHUB_KEY]);
        assert!(info.has_update);
    }

    #[test]
    fn parse_failure_falls_back_to_github() {
        let mut calls = Vec::new();
        let info = check_app_update_with("0.2.35", |source| {
            calls.push(source.key);
            Ok(if source.key == SHIELDS_KEY {
                r#"{"value":"no releases or repo not found"}"#.to_string()
            } else {
                GITHUB_OK.to_string()
            })
        })
        .expect("GitHub fallback succeeds");

        assert_eq!(calls, [SHIELDS_KEY, GITHUB_KEY]);
        assert!(info.has_update);
    }

    #[test]
    fn failure_from_every_source_is_reported() {
        let error = check_app_update_with("0.2.35", |source| {
            Err(CodexxError::Config(format!("{} unavailable", source.key)))
        })
        .expect_err("all sources fail");
        let message = error.to_string();

        assert!(message.contains(SHIELDS_KEY));
        assert!(message.contains(GITHUB_KEY));
    }

    #[test]
    fn parses_shields_and_github_release_payloads() {
        assert_eq!(
            parse_shields_release(SHIELDS_OK)
                .expect("parse Shields")
                .version,
            Version::parse("0.2.36").unwrap()
        );
        assert_eq!(
            parse_github_release(GITHUB_OK).expect("parse GitHub").tag,
            "v0.2.36"
        );
        assert!(parse_shields_release(r#"{"value":"repo not found"}"#).is_err());
        assert!(parse_github_release(r#"{"tag_name":"nightly"}"#).is_err());
    }

    #[test]
    fn semver_comparison_handles_equal_older_and_prerelease_versions() {
        let stable = parse_release_tag("v0.3.0").unwrap();
        assert!(!update_info("0.3.0", stable.clone()).unwrap().has_update);
        assert!(update_info("0.2.35", stable.clone()).unwrap().has_update);
        assert!(!update_info("0.4.0", stable).unwrap().has_update);

        let prerelease = parse_release_tag("v0.3.0-beta.1").unwrap();
        assert!(!update_info("0.3.0", prerelease).unwrap().has_update);
        let stable = parse_release_tag("v0.3.0").unwrap();
        assert!(update_info("0.3.0-beta.1", stable).unwrap().has_update);
    }
}
