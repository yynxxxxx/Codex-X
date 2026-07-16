use crate::error::{CodexxError, Result};
#[cfg(test)]
use crate::remote::ensure_crypto_provider;
use crate::remote::{remote_client, remote_request_error, RemoteSource};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Instant;

const MODELS_SOURCE_KEY: &str = "获取模型列表";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderConnectionResult {
    pub(crate) ok: bool,
    pub(crate) status: Option<u16>,
    pub(crate) message: String,
    pub(crate) duration_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderModel {
    pub(crate) id: String,
    pub(crate) created: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderModelsResult {
    pub(crate) models: Vec<ProviderModel>,
    pub(crate) status: u16,
    pub(crate) duration_ms: u128,
}

#[derive(Debug, Deserialize)]
struct ModelsPayload {
    data: Vec<ModelPayload>,
}

#[derive(Debug, Deserialize)]
struct ModelPayload {
    id: String,
    #[serde(default)]
    created: Option<serde_json::Value>,
}

enum ProviderModelsAttempt {
    Success(ProviderModelsResult),
    HttpError { status: u16, duration_ms: u128 },
}

fn provider_models_url(base_url: &str) -> Result<reqwest::Url> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(CodexxError::Config("base_url 不能为空".to_string()));
    }

    let mut url = reqwest::Url::parse(trimmed)
        .map_err(|_| CodexxError::Config("base_url 格式不正确".to_string()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(CodexxError::Config(
            "base_url 必须是有效的 http:// 或 https:// 地址".to_string(),
        ));
    }

    let segments = url
        .path_segments()
        .ok_or_else(|| CodexxError::Config("base_url 格式不正确".to_string()))?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let already_models = segments.len() >= 2
        && segments[segments.len() - 2].eq_ignore_ascii_case("v1")
        && segments[segments.len() - 1].eq_ignore_ascii_case("models");
    let already_v1 = segments
        .last()
        .is_some_and(|segment| segment.eq_ignore_ascii_case("v1"));

    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| CodexxError::Config("base_url 格式不正确".to_string()))?;
        path.pop_if_empty();
        if !already_models {
            if !already_v1 {
                path.push("v1");
            }
            path.push("models");
        }
    }
    url.set_fragment(None);
    Ok(url)
}

fn parse_created(value: Option<serde_json::Value>) -> Option<i64> {
    value.and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
    })
}

fn compare_digit_runs(left: &[u8], right: &[u8]) -> Ordering {
    let left_significant = left
        .iter()
        .position(|byte| *byte != b'0')
        .map_or(&left[left.len()..], |index| &left[index..]);
    let right_significant = right
        .iter()
        .position(|byte| *byte != b'0')
        .map_or(&right[right.len()..], |index| &right[index..]);

    left_significant
        .len()
        .cmp(&right_significant.len())
        .then_with(|| left_significant.cmp(right_significant))
}

fn natural_model_id_cmp(left: &str, right: &str) -> Ordering {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let (mut left_index, mut right_index) = (0, 0);

    while left_index < left.len() && right_index < right.len() {
        if left[left_index].is_ascii_digit() && right[right_index].is_ascii_digit() {
            let left_end = left[left_index..]
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .map_or(left.len(), |offset| left_index + offset);
            let right_end = right[right_index..]
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .map_or(right.len(), |offset| right_index + offset);
            let order =
                compare_digit_runs(&left[left_index..left_end], &right[right_index..right_end]);
            if order != Ordering::Equal {
                return order;
            }
            left_index = left_end;
            right_index = right_end;
            continue;
        }

        let order = left[left_index]
            .to_ascii_lowercase()
            .cmp(&right[right_index].to_ascii_lowercase());
        if order != Ordering::Equal {
            return order;
        }
        left_index += 1;
        right_index += 1;
    }

    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn parse_models(body: &str) -> Result<Vec<ProviderModel>> {
    let payload: ModelsPayload = serde_json::from_str(body)
        .map_err(|_| CodexxError::Config("模型列表返回格式不正确".to_string()))?;
    let mut models = Vec::<ProviderModel>::new();
    let mut indexes = HashMap::<String, usize>::new();

    for model in payload.data {
        let id = model.id.trim();
        if id.is_empty() {
            continue;
        }
        let created = parse_created(model.created);
        if let Some(index) = indexes.get(id).copied() {
            if created > models[index].created {
                models[index].created = created;
            }
            continue;
        }
        indexes.insert(id.to_string(), models.len());
        models.push(ProviderModel {
            id: id.to_string(),
            created,
        });
    }

    models.sort_by(|left, right| {
        right
            .created
            .cmp(&left.created)
            .then_with(|| natural_model_id_cmp(&right.id, &left.id))
    });

    Ok(models)
}

fn request_provider_models_with_client(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<ProviderModelsAttempt> {
    let url = provider_models_url(base_url)?;
    let source = RemoteSource::new(MODELS_SOURCE_KEY, url.as_str(), Some("application/json"));
    let mut request = client
        .get(url.as_str())
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(api_key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
        request = request.bearer_auth(api_key);
    }

    let started = Instant::now();
    let response = request
        .send()
        .map_err(|error| remote_request_error(&source, &error))?;
    let duration_ms = started.elapsed().as_millis();
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Ok(ProviderModelsAttempt::HttpError {
            status,
            duration_ms,
        });
    }

    let body = response
        .text()
        .map_err(|_| CodexxError::Config("模型列表读取失败".to_string()))?;
    Ok(ProviderModelsAttempt::Success(ProviderModelsResult {
        models: parse_models(&body)?,
        status,
        duration_ms,
    }))
}

fn request_provider_models(base_url: &str, api_key: Option<&str>) -> Result<ProviderModelsAttempt> {
    let client = remote_client()?;
    request_provider_models_with_client(&client, base_url, api_key)
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
    match request_provider_models(&base_url, api_key.as_deref())? {
        ProviderModelsAttempt::Success(result) => {
            Ok(provider_status_result(result.status, result.duration_ms))
        }
        ProviderModelsAttempt::HttpError {
            status,
            duration_ms,
        } => Ok(provider_status_result(status, duration_ms)),
    }
}

pub(crate) fn fetch_provider_models_inner(
    base_url: String,
    api_key: Option<String>,
) -> Result<ProviderModelsResult> {
    match request_provider_models(&base_url, api_key.as_deref())? {
        ProviderModelsAttempt::Success(result) => Ok(result),
        ProviderModelsAttempt::HttpError { status, .. } => {
            Err(CodexxError::Config(if matches!(status, 401 | 403) {
                format!("获取模型列表失败（HTTP {status}，请检查 API Key）")
            } else {
                format!("获取模型列表失败（HTTP {status}）")
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    fn direct_client() -> Client {
        ensure_crypto_provider();
        Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("build test client")
    }

    fn serve_once(status: u16, body: &'static str) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set read timeout");
            let mut bytes = [0_u8; 8192];
            let read = stream.read(&mut bytes).expect("read request");
            let status_text = if status == 200 { "OK" } else { "Error" };
            let response = format!(
                "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            String::from_utf8_lossy(&bytes[..read]).into_owned()
        });
        (base_url, server)
    }

    #[test]
    fn models_url_adds_exactly_one_v1_segment() {
        for (base, expected) in [
            ("https://example.com", "https://example.com/v1/models"),
            ("https://example.com/v1", "https://example.com/v1/models"),
            ("https://example.com/v1/", "https://example.com/v1/models"),
            (
                "https://example.com/openai",
                "https://example.com/openai/v1/models",
            ),
        ] {
            assert_eq!(provider_models_url(base).unwrap().as_str(), expected);
        }
    }

    #[test]
    fn fetches_models_with_bearer_auth_and_deduplicates_ids() {
        let body = r#"{"data":[{"id":" gpt-5.6-sol ","created":20},{"id":"gpt-5.5","created":"10"},{"id":"gpt-5.6-sol","created":30},{"id":"  "}]}"#;
        let (base_url, server) = serve_once(200, body);
        let attempt = request_provider_models_with_client(
            &direct_client(),
            &base_url,
            Some("sk-private-test"),
        )
        .expect("request succeeds");
        let ProviderModelsAttempt::Success(result) = attempt else {
            panic!("expected successful models response");
        };

        assert_eq!(
            result.models,
            vec![
                ProviderModel {
                    id: "gpt-5.6-sol".to_string(),
                    created: Some(30),
                },
                ProviderModel {
                    id: "gpt-5.5".to_string(),
                    created: Some(10),
                },
            ]
        );
        let request = server.join().expect("join mock server");
        assert!(request.starts_with("GET /v1/models HTTP/1.1"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-private-test"));
    }

    #[test]
    fn sorts_by_created_then_natural_model_version_descending() {
        let models = parse_models(
            r#"{"data":[{"id":"gpt-5.5","created":20},{"id":"gpt-5.6-sol","created":20},{"id":"gpt-5.9"},{"id":"gpt-5.10"},{"id":"older-by-name","created":30}]}"#,
        )
        .expect("parse models");

        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            [
                "older-by-name",
                "gpt-5.6-sol",
                "gpt-5.5",
                "gpt-5.10",
                "gpt-5.9",
            ]
        );
    }

    #[test]
    fn http_errors_are_not_reported_as_connected_or_leaked() {
        let private_body = r#"{"error":"secret upstream response"}"#;
        let (base_url, server) = serve_once(403, private_body);
        let attempt = request_provider_models_with_client(
            &direct_client(),
            &base_url,
            Some("sk-private-test"),
        )
        .expect("HTTP status remains an inspectable result");
        let ProviderModelsAttempt::HttpError {
            status,
            duration_ms,
        } = attempt
        else {
            panic!("expected HTTP error");
        };
        let result = provider_status_result(status, duration_ms);

        assert!(!result.ok);
        assert_eq!(result.status, Some(403));
        assert!(!result.message.contains("secret upstream response"));
        assert!(!result.message.contains("sk-private-test"));
        assert!(!result.message.contains(&base_url));
        server.join().expect("join mock server");
    }

    #[test]
    fn invalid_payload_error_does_not_include_response_body() {
        let private_body = r#"{"private":"secret response"}"#;
        let (base_url, server) = serve_once(200, private_body);
        let error = request_provider_models_with_client(
            &direct_client(),
            &base_url,
            Some("sk-private-test"),
        )
        .err()
        .expect("invalid payload fails");
        let message = error.to_string();

        assert!(!message.contains("secret response"));
        assert!(!message.contains("sk-private-test"));
        assert!(!message.contains(&base_url));
        server.join().expect("join mock server");
    }
}
