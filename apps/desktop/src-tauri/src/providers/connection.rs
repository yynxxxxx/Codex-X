use crate::error::{CodexxError, Result};
use crate::failover::protocol::UpstreamApi;
#[cfg(test)]
use crate::remote::ensure_crypto_provider;
use crate::remote::{remote_client, remote_request_error, RemoteSource};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Instant;
use toml_edit::{DocumentMut, Item};

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

#[derive(Debug, Deserialize)]
struct GeminiModelsPayload {
    models: Vec<GeminiModelPayload>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiModelPayload {
    name: String,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}

enum ProviderModelsAttempt {
    Success(ProviderModelsResult),
    HttpError { status: u16, duration_ms: u128 },
}

fn provider_base_url(base_url: &str) -> Result<reqwest::Url> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(CodexxError::Config("base_url 不能为空".to_string()));
    }

    let url = reqwest::Url::parse(trimmed)
        .map_err(|_| CodexxError::Config("base_url 格式不正确".to_string()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(CodexxError::Config(
            "base_url 必须是有效的 http:// 或 https:// 地址".to_string(),
        ));
    }

    Ok(url)
}

fn provider_models_url(base_url: &str) -> Result<reqwest::Url> {
    let mut url = provider_base_url(base_url)?;
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

fn provider_models_url_with_protocol(
    base_url: &str,
    protocol: UpstreamApi,
) -> Result<reqwest::Url> {
    if protocol != UpstreamApi::Gemini {
        let mut url = provider_base_url(base_url)?;
        let path = url.path().trim_end_matches('/');
        let suffix = match protocol {
            UpstreamApi::Responses => "/responses",
            UpstreamApi::ChatCompletions => "/chat/completions",
            UpstreamApi::AnthropicMessages => "/messages",
            UpstreamApi::Gemini => unreachable!(),
        };
        if let Some(prefix) = path.strip_suffix(suffix) {
            let prefix = prefix.to_string();
            url.set_path(&prefix);
        }
        return provider_models_url(url.as_str());
    }
    let mut url = provider_base_url(base_url)?;
    let path = url.path().trim_end_matches('/');
    // A pasted generateContent endpoint still points to the same API prefix.
    let prefix = path
        .split_once("/models/")
        .map_or(path, |(prefix, _)| prefix);
    let path = if prefix.ends_with("/models") {
        prefix.to_string()
    } else if prefix.ends_with("/v1") || prefix.ends_with("/v1beta") {
        format!("{prefix}/models")
    } else {
        format!("{prefix}/v1beta/models")
    };
    url.set_path(&path);
    url.set_fragment(None);
    Ok(url)
}

fn provider_models_headers_with_env(
    config_text: Option<&str>,
    resolve_env: impl Fn(&str) -> Option<String>,
) -> Result<HeaderMap> {
    let Some(config_text) = config_text.filter(|text| !text.trim().is_empty()) else {
        return Ok(HeaderMap::new());
    };
    let doc = config_text
        .parse::<DocumentMut>()
        .map_err(|_| CodexxError::Config("供应商 TOML 无效，请先修正配置".into()))?;
    let id = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| CodexxError::Config("供应商 TOML 缺少 model_provider".into()))?;
    let table = doc
        .get("model_providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(id))
        .and_then(Item::as_table)
        .ok_or_else(|| CodexxError::Config("供应商 TOML 缺少当前供应商配置表".into()))?;
    super::store::validate_provider_header_table(table)?;
    let mut headers = HeaderMap::new();
    for header in super::store::read_provider_headers_inner(config_text.to_string())? {
        let text = match header.source {
            super::store::ProviderHeaderSource::Static => header.value,
            super::store::ProviderHeaderSource::Env => {
                resolve_env(&header.value).ok_or_else(|| {
                    CodexxError::Config(format!(
                        "供应商 Header ({}) 所需的环境变量不可用",
                        header.name
                    ))
                })?
            }
        };
        let name = HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| CodexxError::Config("供应商 Header 名称无效".into()))?;
        let mut value = HeaderValue::from_str(&text).map_err(|_| {
            CodexxError::Config(format!(
                "供应商 Header ({}) 值无效，不能包含换行或控制字符",
                header.name
            ))
        })?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(headers)
}

fn has_custom_provider_auth(headers: &HeaderMap) -> bool {
    ["authorization", "x-api-key", "x-goog-api-key"]
        .iter()
        .any(|name| headers.contains_key(*name))
}

fn provider_models_api_key_with_env(
    config_text: Option<&str>,
    explicit_api_key: Option<&str>,
    headers: &HeaderMap,
    resolve_env: impl Fn(&str) -> Option<String>,
) -> Result<Option<String>> {
    // Existing custom auth headers and explicit form keys retain precedence.
    // Do not consult a stale/missing env_key when another source was selected.
    if has_custom_provider_auth(headers) {
        return Ok(None);
    }
    if let Some(key) = explicit_api_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        return Ok(Some(key.to_owned()));
    }
    let Some(config_text) = config_text.filter(|text| !text.trim().is_empty()) else {
        return Ok(None);
    };
    let doc = config_text
        .parse::<DocumentMut>()
        .map_err(|_| CodexxError::Config("供应商 TOML 无效，请先修正配置".into()))?;
    let env_key = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .and_then(|id| doc.get("model_providers")?.as_table()?.get(id.trim()))
        .and_then(Item::as_table)
        .and_then(|table| table.get("env_key"));
    let Some(env_key) = env_key else {
        return Ok(None);
    };
    let name = env_key
        .as_str()
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| CodexxError::Config("供应商 env_key 必须是非空的环境变量名称".into()))?;
    resolve_env(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(Some)
        .ok_or_else(|| {
            CodexxError::Config(
                "供应商 env_key 指定的环境变量未设置或为空，请先设置该变量，再重新打开 Codex-X 后重试".into(),
            )
        })
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
    Ok(normalize_models(payload.data))
}

fn parse_models_with_protocol(body: &str, protocol: UpstreamApi) -> Result<Vec<ProviderModel>> {
    if protocol != UpstreamApi::Gemini {
        return parse_models(body);
    }
    let payload: GeminiModelsPayload = serde_json::from_str(body)
        .map_err(|_| CodexxError::Config("模型列表返回格式不正确".to_string()))?;
    let models = payload
        .models
        .into_iter()
        .filter(|model| {
            model
                .supported_generation_methods
                .iter()
                .any(|method| method == "generateContent")
        })
        .map(|model| ModelPayload {
            id: model
                .name
                .trim()
                .strip_prefix("models/")
                .unwrap_or(model.name.trim())
                .to_string(),
            created: None,
        })
        .collect();
    Ok(normalize_models(models))
}

fn normalize_models(payload: Vec<ModelPayload>) -> Vec<ProviderModel> {
    let mut models = Vec::<ProviderModel>::new();
    let mut indexes = HashMap::<String, usize>::new();

    for model in payload {
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

    models
}

#[cfg(test)]
fn request_provider_models_with_client(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<ProviderModelsAttempt> {
    request_provider_models_with_protocol_client(
        client,
        base_url,
        api_key,
        UpstreamApi::Responses,
        None,
    )
}

fn request_provider_models_with_protocol_client(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
    protocol: UpstreamApi,
    config_text: Option<&str>,
) -> Result<ProviderModelsAttempt> {
    request_provider_models_with_protocol_client_and_env(
        client,
        base_url,
        api_key,
        protocol,
        config_text,
        |name| std::env::var(name).ok(),
    )
}

fn request_provider_models_with_protocol_client_and_env(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
    protocol: UpstreamApi,
    config_text: Option<&str>,
    resolve_env: impl Fn(&str) -> Option<String>,
) -> Result<ProviderModelsAttempt> {
    let headers = provider_models_headers_with_env(config_text, &resolve_env)?;
    let api_key = provider_models_api_key_with_env(config_text, api_key, &headers, &resolve_env)?;
    request_provider_models_with_headers(client, base_url, api_key.as_deref(), protocol, headers)
}

fn request_provider_models_with_headers(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
    protocol: UpstreamApi,
    headers: HeaderMap,
) -> Result<ProviderModelsAttempt> {
    let url = provider_models_url_with_protocol(base_url, protocol)?;
    let source = RemoteSource::new(MODELS_SOURCE_KEY, url.as_str(), Some("application/json"));
    let mut request = client
        .get(url.as_str())
        .header(reqwest::header::ACCEPT, "application/json");
    if protocol == UpstreamApi::AnthropicMessages {
        request = request.header("anthropic-version", "2023-06-01");
    }
    let custom_auth = has_custom_provider_auth(&headers);
    if let Some(api_key) = api_key
        .map(str::trim)
        .filter(|key| !key.is_empty() && !custom_auth)
    {
        let (name, text) = match protocol {
            UpstreamApi::Responses | UpstreamApi::ChatCompletions => {
                ("authorization", format!("Bearer {api_key}"))
            }
            UpstreamApi::AnthropicMessages => ("x-api-key", api_key.to_string()),
            UpstreamApi::Gemini => ("x-goog-api-key", api_key.to_string()),
        };
        let mut value = HeaderValue::from_str(&text)
            .map_err(|_| CodexxError::Config("供应商 API Key 格式不正确".into()))?;
        value.set_sensitive(true);
        request = request.header(name, value);
    }
    request = request.headers(headers);

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
        models: parse_models_with_protocol(&body, protocol)?,
        status,
        duration_ms,
    }))
}

fn request_provider_models_with_protocol(
    base_url: &str,
    api_key: Option<&str>,
    upstream_api: Option<&str>,
    config_text: Option<&str>,
) -> Result<ProviderModelsAttempt> {
    let protocol = UpstreamApi::from_provider(upstream_api, "responses")?;
    let client = remote_client()?;
    request_provider_models_with_protocol_client(&client, base_url, api_key, protocol, config_text)
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
    test_provider_connection_with_protocol_inner(base_url, api_key, None, None)
}

pub(crate) fn test_provider_connection_with_protocol_inner(
    base_url: String,
    api_key: Option<String>,
    upstream_api: Option<String>,
    config_text: Option<String>,
) -> Result<ProviderConnectionResult> {
    match request_provider_models_with_protocol(
        &base_url,
        api_key.as_deref(),
        upstream_api.as_deref(),
        config_text.as_deref(),
    )? {
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
    fetch_provider_models_with_protocol_inner(base_url, api_key, None, None)
}

pub(crate) fn fetch_provider_models_with_protocol_inner(
    base_url: String,
    api_key: Option<String>,
    upstream_api: Option<String>,
    config_text: Option<String>,
) -> Result<ProviderModelsResult> {
    match request_provider_models_with_protocol(
        &base_url,
        api_key.as_deref(),
        upstream_api.as_deref(),
        config_text.as_deref(),
    )? {
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

    #[test]
    fn protocol_model_urls_preserve_api_prefix_and_do_not_repeat_versions() {
        for (protocol, base, expected) in [
            (UpstreamApi::Responses, "https://example.test/v1/responses", "https://example.test/v1/models"),
            (UpstreamApi::ChatCompletions, "https://example.test/v1/chat/completions", "https://example.test/v1/models"),
            (UpstreamApi::AnthropicMessages, "https://example.test", "https://example.test/v1/models"),
            (UpstreamApi::AnthropicMessages, "https://example.test/anthropic/v1/messages", "https://example.test/anthropic/v1/models"),
            (UpstreamApi::Gemini, "https://example.test", "https://example.test/v1beta/models"),
            (UpstreamApi::Gemini, "https://example.test/v1beta", "https://example.test/v1beta/models"),
            (UpstreamApi::Gemini, "https://example.test/v1beta/", "https://example.test/v1beta/models"),
            (UpstreamApi::Gemini, "https://example.test/v1", "https://example.test/v1/models"),
            (UpstreamApi::Gemini, "https://example.test/api/google/v1/models", "https://example.test/api/google/v1/models"),
            (UpstreamApi::Gemini, "https://example.test/api/google/v1beta/models/gemini-2.5-pro:generateContent?route=fixture#ignored", "https://example.test/api/google/v1beta/models?route=fixture"),
            (UpstreamApi::Gemini, "https://example.test/api/google", "https://example.test/api/google/v1beta/models"),
        ] {
            assert_eq!(provider_models_url_with_protocol(base, protocol).unwrap().as_str(), expected);
        }
    }

    #[test]
    fn chat_and_anthropic_models_use_their_protocol_authentication() {
        for (protocol, auth) in [
            (
                UpstreamApi::ChatCompletions,
                "authorization: bearer fixture-protocol-key",
            ),
            (
                UpstreamApi::AnthropicMessages,
                "x-api-key: fixture-protocol-key",
            ),
        ] {
            let (url, server) = serve_once(200, r#"{"data":[{"id":"fixture-model"}]}"#);
            let attempt = request_provider_models_with_protocol_client(
                &direct_client(),
                &url,
                Some("fixture-protocol-key"),
                protocol,
                None,
            )
            .unwrap();
            let ProviderModelsAttempt::Success(result) = attempt else {
                panic!("expected model list")
            };
            assert_eq!(result.models[0].id, "fixture-model");
            let request = server.join().unwrap().to_ascii_lowercase();
            assert!(request.starts_with("get /v1/models http/1.1"));
            assert!(request.contains(auth));
            if protocol == UpstreamApi::AnthropicMessages {
                assert!(request.contains("anthropic-version: 2023-06-01"));
                assert!(!request.contains("authorization:"));
            } else {
                assert!(!request.contains("x-api-key:"));
            }
            assert!(!request.contains("x-goog-api-key:"));
        }
    }

    #[test]
    fn gemini_models_use_google_header_and_include_only_generate_content_models() {
        let body = r#"{"models":[{"name":"models/gemini-2.5-pro","supportedGenerationMethods":["generateContent","countTokens"]},{"name":"models/text-embedding-004","supportedGenerationMethods":["embedContent"]},{"name":" models/gemini-2.5-flash ","supportedGenerationMethods":["generateContent"]},{"name":"models/gemini-2.5-pro","supportedGenerationMethods":["generateContent"]},{"name":"models/count-only","supportedGenerationMethods":["countTokens"]}]}"#;
        let (url, server) = serve_once(200, body);
        let attempt = request_provider_models_with_protocol_client(
            &direct_client(),
            &format!("{url}/v1beta"),
            Some("fixture-google-key"),
            UpstreamApi::Gemini,
            None,
        )
        .unwrap();
        let ProviderModelsAttempt::Success(result) = attempt else {
            panic!("expected Gemini model list")
        };
        assert_eq!(
            result
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gemini-2.5-pro", "gemini-2.5-flash"]
        );
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /v1beta/models http/1.1"));
        assert!(request.contains("x-goog-api-key: fixture-google-key"));
        assert!(!request.contains("authorization:"));
        assert!(!request.contains("x-api-key:"));
        assert!(!request
            .lines()
            .next()
            .unwrap()
            .contains("fixture-google-key"));
    }

    #[test]
    fn static_and_environment_headers_are_applied_to_model_discovery() {
        let config = "model_provider='custom'\n[model_providers.custom]\nhttp_headers={User-Agent='Fixture agent',HTTP-Referer='https://fixture.example.test'}\nenv_http_headers={X-Project='FIXTURE_PROJECT'}\n";
        let headers = provider_models_headers_with_env(Some(config), |name| {
            (name == "FIXTURE_PROJECT").then(|| "fixture-project-value".to_string())
        })
        .unwrap();
        assert!(!format!("{headers:?}").contains("fixture-project-value"));
        let (url, server) = serve_once(200, r#"{"data":[]}"#);
        request_provider_models_with_headers(
            &direct_client(),
            &url,
            Some("fixture-api-key"),
            UpstreamApi::Responses,
            headers,
        )
        .unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("user-agent: fixture agent"));
        assert!(request.contains("http-referer: https://fixture.example.test"));
        assert!(request.contains("x-project: fixture-project-value"));
        assert!(request.contains("authorization: bearer fixture-api-key"));
    }

    #[test]
    fn explicit_auth_and_anthropic_version_headers_override_request_defaults() {
        let config = "model_provider='custom'\n[model_providers.custom]\nhttp_headers={Authorization='Bearer fixture-custom-auth',anthropic-version='2099-01-01'}\n";
        let (url, server) = serve_once(200, r#"{"data":[]}"#);
        request_provider_models_with_protocol_client(
            &direct_client(),
            &url,
            Some("fixture-unused-key"),
            UpstreamApi::AnthropicMessages,
            Some(config),
        )
        .unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("authorization: bearer fixture-custom-auth"));
        assert!(request.contains("anthropic-version: 2099-01-01"));
        assert!(!request.contains("anthropic-version: 2023-06-01"));
        assert!(!request.contains("x-api-key:"));
        assert!(!request.contains("fixture-unused-key"));
    }

    #[test]
    fn environment_provider_key_authenticates_model_discovery_and_connectivity() {
        let config = "model_provider='selected'\n[model_providers.selected]\nenv_key='FIXTURE_PROVIDER_KEY'\n[model_providers.unrelated]\nenv_key='UNRELATED_KEY'\n";
        for (protocol, expected_auth, body) in [
            (
                UpstreamApi::Responses,
                "authorization: bearer fixture-environment-key",
                r#"{"data":[{"id":"fixture-model"}]}"#,
            ),
            (
                UpstreamApi::ChatCompletions,
                "authorization: bearer fixture-environment-key",
                r#"{"data":[{"id":"fixture-model"}]}"#,
            ),
            (
                UpstreamApi::AnthropicMessages,
                "x-api-key: fixture-environment-key",
                r#"{"data":[{"id":"fixture-model"}]}"#,
            ),
            (
                UpstreamApi::Gemini,
                "x-goog-api-key: fixture-environment-key",
                r#"{"models":[{"name":"models/fixture-model","supportedGenerationMethods":["generateContent"]}]}"#,
            ),
        ] {
            let (url, server) = serve_once(200, body);
            // Model discovery and connection checks share this request pipeline.
            let attempt = request_provider_models_with_protocol_client_and_env(
                &direct_client(),
                &url,
                Some(" "),
                protocol,
                Some(config),
                |name| {
                    assert_eq!(name, "FIXTURE_PROVIDER_KEY");
                    Some("fixture-environment-key".into())
                },
            )
            .unwrap();
            let ProviderModelsAttempt::Success(result) = attempt else {
                panic!("expected successful models response");
            };
            assert_eq!(result.models[0].id, "fixture-model");
            assert!(provider_status_result(result.status, result.duration_ms).ok);
            let request = server.join().unwrap().to_ascii_lowercase();
            assert!(request.contains(expected_auth));
            assert!(!request
                .lines()
                .next()
                .unwrap()
                .contains("fixture-environment-key"));
        }
        assert!(!config.contains("fixture-environment-key"));
    }

    #[test]
    fn explicit_keys_and_custom_auth_headers_precede_provider_environment_keys() {
        for (config, explicit_key, expected_auth) in [
            (
                "model_provider='custom'\n[model_providers.custom]\nenv_key='UNAVAILABLE_ENV_KEY'\n",
                Some("fixture-explicit-key"),
                "authorization: bearer fixture-explicit-key",
            ),
            (
                "model_provider='custom'\n[model_providers.custom]\nenv_key='UNAVAILABLE_ENV_KEY'\nhttp_headers={Authorization='Bearer fixture-custom-auth'}\n",
                Some("fixture-explicit-key"),
                "authorization: bearer fixture-custom-auth",
            ),
            (
                "model_provider='custom'\n[model_providers.custom]\nenv_key='UNAVAILABLE_ENV_KEY'\nenv_http_headers={Authorization='FIXTURE_AUTH_HEADER'}\n",
                Some("fixture-explicit-key"),
                "authorization: bearer fixture-environment-header",
            ),
        ] {
            let (url, server) = serve_once(200, r#"{"data":[]}"#);
            request_provider_models_with_protocol_client_and_env(
                &direct_client(),
                &url,
                explicit_key,
                UpstreamApi::Responses,
                Some(config),
                |name| {
                    assert_eq!(name, "FIXTURE_AUTH_HEADER", "stale env_key must not be resolved");
                    Some("Bearer fixture-environment-header".into())
                },
            )
            .unwrap();
            let request = server.join().unwrap().to_ascii_lowercase();
            assert!(request.contains(expected_auth));
            assert_eq!(request.matches("authorization:").count(), 1);
        }
    }

    #[test]
    fn unavailable_provider_environment_key_fails_before_request_without_value_echo() {
        let config =
            "model_provider='custom'\n[model_providers.custom]\nenv_key='FIXTURE_PROVIDER_KEY'\n";
        for value in [None, Some("".into()), Some("   ".into())] {
            let error = request_provider_models_with_protocol_client_and_env(
                &direct_client(),
                "http://127.0.0.1:9",
                None,
                UpstreamApi::Responses,
                Some(config),
                |_| value.clone(),
            )
            .err()
            .expect("missing key must fail before connecting")
            .to_string();
            assert!(error.contains("env_key"));
            assert!(error.contains("重新打开 Codex-X"));
            assert!(!error.contains("127.0.0.1"));
        }
        let error = request_provider_models_with_protocol_client_and_env(
            &direct_client(),
            "http://127.0.0.1:9",
            None,
            UpstreamApi::Responses,
            Some(config),
            |_| Some("fixture-private-key\r\nInjected: value".into()),
        )
        .err()
        .expect("invalid key must fail before connecting")
        .to_string();
        assert!(error.contains("API Key"));
        assert!(!error.contains("fixture-private-key"));
        assert!(!error.contains("Injected"));
    }

    #[test]
    fn environment_authentication_errors_do_not_echo_key_or_upstream_body() {
        let config =
            "model_provider='custom'\n[model_providers.custom]\nenv_key='FIXTURE_PROVIDER_KEY'\n";
        let (url, server) = serve_once(401, r#"{"error":"fixture-private-response"}"#);
        let attempt = request_provider_models_with_protocol_client_and_env(
            &direct_client(),
            &url,
            None,
            UpstreamApi::Responses,
            Some(config),
            |_| Some("fixture-private-env-key".into()),
        )
        .unwrap();
        let ProviderModelsAttempt::HttpError {
            status,
            duration_ms,
        } = attempt
        else {
            panic!("expected authentication error");
        };
        let result = provider_status_result(status, duration_ms);
        assert!(!result.ok);
        assert_eq!(result.status, Some(401));
        assert!(!result.message.contains("fixture-private-env-key"));
        assert!(!result.message.contains("fixture-private-response"));
        assert!(server.join().unwrap().contains("fixture-private-env-key"));
    }

    #[test]
    fn header_validation_rejects_duplicates_and_invalid_resolved_values_without_secret_echo() {
        let duplicate = "model_provider='custom'\n[model_providers.custom]\nhttp_headers={X-Project='fixture-static-secret'}\nenv_http_headers={x-project='FIXTURE_PROJECT'}\n";
        let error = provider_models_headers_with_env(Some(duplicate), |_| {
            panic!("validation must happen before resolving environment values")
        })
        .unwrap_err()
        .to_string();
        assert!(!error.contains("fixture-static-secret"));
        assert!(error.contains("重复"));
        let env = "model_provider='custom'\n[model_providers.custom]\nenv_http_headers={X-Project='FIXTURE_PROJECT'}\n";
        for resolved in [
            None,
            Some("fixture-env-secret\r\nInjected: value".to_string()),
        ] {
            let error = provider_models_headers_with_env(Some(env), |_| resolved.clone())
                .unwrap_err()
                .to_string();
            assert!(error.contains("X-Project"));
            assert!(!error.contains("fixture-env-secret"));
            assert!(!error.contains("Injected"));
        }
    }

    #[test]
    fn gemini_payload_errors_and_authentication_statuses_do_not_echo_private_data() {
        let (url, server) = serve_once(200, r#"{"models":"fixture-private-response"}"#);
        let error = request_provider_models_with_protocol_client(
            &direct_client(),
            &url,
            Some("fixture-google-secret"),
            UpstreamApi::Gemini,
            None,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(!error.contains("fixture-private-response"));
        assert!(!error.contains("fixture-google-secret"));
        server.join().unwrap();
        let (url, server) = serve_once(401, r#"{"error":"fixture-auth-private-response"}"#);
        let attempt = request_provider_models_with_protocol_client(
            &direct_client(),
            &url,
            Some("fixture-anthropic-secret"),
            UpstreamApi::AnthropicMessages,
            None,
        )
        .unwrap();
        let ProviderModelsAttempt::HttpError {
            status,
            duration_ms,
        } = attempt
        else {
            panic!("expected authentication status")
        };
        let result = provider_status_result(status, duration_ms);
        assert!(!result.ok);
        assert_eq!(result.status, Some(401));
        assert!(!result.message.contains("fixture-auth-private-response"));
        assert!(!result.message.contains("fixture-anthropic-secret"));
        server.join().unwrap();
    }
}
