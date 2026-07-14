use crate::error::{CodexxError, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderConnectionResult {
    pub(crate) ok: bool,
    pub(crate) status: Option<u16>,
    pub(crate) message: String,
    pub(crate) duration_ms: u128,
}

#[allow(clippy::result_large_err)]
fn provider_test_request(
    agent: &ureq::Agent,
    url: &str,
    api_key: Option<&str>,
) -> std::result::Result<ureq::Response, ureq::Error> {
    let request = agent.get(url);
    if let Some(api_key) = api_key.filter(|s| !s.trim().is_empty()) {
        request.set("Authorization", &format!("Bearer {}", api_key.trim()))
    } else {
        request
    }
    .call()
}

pub(crate) fn provider_status_result(status: u16, duration_ms: u128) -> ProviderConnectionResult {
    ProviderConnectionResult {
        ok: (200..300).contains(&status),
        status: Some(status),
        message: if (200..300).contains(&status) {
            format!("{duration_ms} ms")
        } else if status == 401 || status == 403 {
            format!("HTTP {status} · {duration_ms} ms（认证失败或无权限）")
        } else {
            format!("HTTP {status} · {duration_ms} ms")
        },
        duration_ms,
    }
}

pub(crate) fn test_provider_connection_inner(
    base_url: String,
    api_key: Option<String>,
) -> Result<ProviderConnectionResult> {
    let base = base_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err(CodexxError::Config("base_url 不能为空".to_string()));
    }
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return Err(CodexxError::Config(
            "base_url 必须以 http:// 或 https:// 开头".to_string(),
        ));
    }

    let api_key = api_key.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(6))
        .build();
    let models_url = format!("{base}/models");
    let started = Instant::now();

    match provider_test_request(&agent, &models_url, api_key) {
        Ok(response) => Ok(provider_status_result(
            response.status(),
            started.elapsed().as_millis(),
        )),
        Err(ureq::Error::Status(status, _)) => {
            // /models exists but rejected the request. This is not a successful
            // provider test; notably HTTP 403 must not be shown as “连接正常”.
            Ok(provider_status_result(
                status,
                started.elapsed().as_millis(),
            ))
        }
        Err(_models_error) => {
            // Network-level failure on /models: try the base endpoint once so
            // users can distinguish DNS/TLS failures from a provider with no
            // models route.
            match provider_test_request(&agent, &base, api_key) {
                Ok(response) => Ok(provider_status_result(
                    response.status(),
                    started.elapsed().as_millis(),
                )),
                Err(ureq::Error::Status(status, _)) => Ok(provider_status_result(
                    status,
                    started.elapsed().as_millis(),
                )),
                Err(_base_error) => Ok(ProviderConnectionResult {
                    ok: false,
                    status: None,
                    message: format!("请求失败 · {} ms", started.elapsed().as_millis()),
                    duration_ms: started.elapsed().as_millis(),
                }),
            }
        }
    }
}
