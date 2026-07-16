use crate::error::{CodexxError, Result};
use std::sync::OnceLock;
use std::time::Duration;

const REMOTE_USER_AGENT: &str = "Codex-X";
const REMOTE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
pub(crate) struct RemoteSource<'a> {
    pub(crate) key: &'static str,
    pub(crate) url: &'a str,
    pub(crate) accept: Option<&'static str>,
}

impl<'a> RemoteSource<'a> {
    pub(crate) const fn new(key: &'static str, url: &'a str, accept: Option<&'static str>) -> Self {
        Self { key, url, accept }
    }
}

pub(crate) fn ensure_crypto_provider() {
    static CRYPTO_PROVIDER: OnceLock<()> = OnceLock::new();
    CRYPTO_PROVIDER.get_or_init(|| {
        // Another TLS client may already have initialized the process-wide provider.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub(crate) fn remote_client() -> Result<reqwest::blocking::Client> {
    ensure_crypto_provider();
    reqwest::blocking::Client::builder()
        .timeout(REMOTE_TIMEOUT)
        .user_agent(REMOTE_USER_AGENT)
        .build()
        .map_err(|_| CodexxError::Config("网络客户端初始化失败".to_string()))
}

pub(crate) fn remote_request_error(
    source: &RemoteSource<'_>,
    error: &reqwest::Error,
) -> CodexxError {
    let reason = if error.is_timeout() {
        "请求超时"
    } else if error.is_connect() {
        "网络连接失败"
    } else {
        "网络请求失败"
    };
    CodexxError::Config(format!("{} {reason}", source.key))
}

fn fetch_remote_text(source: &RemoteSource<'_>) -> Result<String> {
    // A fresh client picks up proxy changes made while the app is running. Reqwest's
    // system proxy resolver also handles proxy environment variables and NO_PROXY.
    let client = remote_client()?;
    let mut request = client.get(source.url);
    if let Some(accept) = source.accept {
        request = request.header(reqwest::header::ACCEPT, accept);
    }
    let response = request
        .send()
        .map_err(|error| remote_request_error(source, &error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(CodexxError::Config(format!(
            "{} 请求失败（HTTP {}）",
            source.key,
            status.as_u16()
        )));
    }
    response
        .text()
        .map_err(|_| CodexxError::Config(format!("{} 响应读取失败", source.key)))
}

pub(crate) fn fetch_first_valid<T, Parse>(sources: &[RemoteSource<'_>], parse: Parse) -> Result<T>
where
    Parse: FnMut(&RemoteSource<'_>, &str) -> Result<T>,
{
    fetch_first_valid_with(sources, fetch_remote_text, parse)
}

pub(crate) fn fetch_first_valid_with<T, Fetch, Parse>(
    sources: &[RemoteSource<'_>],
    mut fetch: Fetch,
    mut parse: Parse,
) -> Result<T>
where
    Fetch: FnMut(&RemoteSource<'_>) -> Result<String>,
    Parse: FnMut(&RemoteSource<'_>, &str) -> Result<T>,
{
    let mut errors = Vec::new();
    for source in sources {
        match fetch(source) {
            Ok(body) if body.trim().is_empty() => {
                errors.push(format!("{} 返回空内容", source.key));
            }
            Ok(body) => match parse(source, &body) {
                Ok(value) => return Ok(value),
                Err(error) => errors.push(format!("{}: {error}", source.key)),
            },
            Err(error) => errors.push(format!("{}: {error}", source.key)),
        }
    }

    Err(CodexxError::Config(format!(
        "远程内容获取失败{}",
        if errors.is_empty() {
            String::new()
        } else {
            format!("：{}", errors.join("；"))
        }
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Command;
    use std::thread;
    use std::time::Instant;

    const SOURCES: [RemoteSource<'static>; 2] = [
        RemoteSource::new("cdn", "https://cdn.example.test", None),
        RemoteSource::new("origin", "https://origin.example.test", None),
    ];
    const PROXY_TEST_URL: &str = "CODEXX_REMOTE_PROXY_TEST_URL";
    const PROXY_TEST_BODY: &str = "CODEXX_REMOTE_PROXY_TEST_BODY";

    fn clear_proxy_environment(command: &mut Command) {
        for name in [
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
            "NO_PROXY",
            "no_proxy",
            "REQUEST_METHOD",
        ] {
            command.env_remove(name);
        }
    }

    fn serve_once(listener: TcpListener, body: &'static str) -> String {
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        let started = Instant::now();
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        started.elapsed() < Duration::from_secs(8),
                        "proxy test connection timed out"
                    );
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("accept proxy test connection: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let mut request = [0_u8; 8192];
        let read = stream.read(&mut request).expect("read proxy request");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write proxy response");
        String::from_utf8_lossy(&request[..read]).into_owned()
    }

    fn run_proxy_test_child(url: &str, body: &str, configure: impl FnOnce(&mut Command)) {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command.args([
            "--exact",
            "remote::tests::proxy_environment_child",
            "--nocapture",
        ]);
        clear_proxy_environment(&mut command);
        command.env(PROXY_TEST_URL, url).env(PROXY_TEST_BODY, body);
        configure(&mut command);
        let output = command.output().expect("run proxy test child");
        assert!(
            output.status.success(),
            "proxy test child failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn proxy_environment_child() {
        let Ok(url) = std::env::var(PROXY_TEST_URL) else {
            return;
        };
        let expected = std::env::var(PROXY_TEST_BODY).expect("proxy test body");
        let source = RemoteSource::new("proxy test", &url, None);
        assert_eq!(
            fetch_remote_text(&source).expect("fetch test response"),
            expected
        );
    }

    #[test]
    fn successful_source_stops_fallback_chain() {
        let mut calls = Vec::new();
        let value = fetch_first_valid_with(
            &SOURCES,
            |source| {
                calls.push(source.key);
                Ok("valid".to_string())
            },
            |_, body| Ok(body.to_string()),
        )
        .expect("first source succeeds");

        assert_eq!(value, "valid");
        assert_eq!(calls, ["cdn"]);
    }

    #[test]
    fn empty_and_unparseable_responses_try_later_sources() {
        let mut calls = Vec::new();
        let value = fetch_first_valid_with(
            &SOURCES,
            |source| {
                calls.push(source.key);
                if source.key == "cdn" {
                    Ok("   ".to_string())
                } else {
                    Ok("42".to_string())
                }
            },
            |_, body| {
                body.parse::<u32>()
                    .map_err(|error| CodexxError::Config(error.to_string()))
            },
        )
        .expect("origin succeeds after empty CDN response");

        assert_eq!(value, 42);
        assert_eq!(calls, ["cdn", "origin"]);
    }

    #[test]
    fn remote_client_uses_lowercase_http_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test proxy");
        let proxy_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || serve_once(listener, "through proxy"));

        run_proxy_test_child(
            "http://remote.invalid/template.md",
            "through proxy",
            |command| {
                command.env("http_proxy", proxy_url);
            },
        );

        let request = server.join().expect("join proxy server");
        assert!(request.starts_with("GET http://remote.invalid/template.md "));
    }

    #[test]
    fn remote_client_respects_lowercase_no_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test origin");
        let origin_url = format!("http://{}/direct.md", listener.local_addr().unwrap());
        let server = thread::spawn(move || serve_once(listener, "direct response"));

        run_proxy_test_child(&origin_url, "direct response", |command| {
            command
                .env("HTTP_PROXY", "http://127.0.0.1:9")
                .env("no_proxy", "127.0.0.1");
        });

        let request = server.join().expect("join origin server");
        assert!(request.starts_with("GET /direct.md "));
    }

    #[test]
    fn connection_errors_do_not_expose_request_or_proxy_details() {
        remote_client().expect("initialize TLS provider");
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(200))
            .build()
            .expect("build direct client");
        let error = client
            .get("http://127.0.0.1:0/private")
            .send()
            .expect_err("reserved port should fail");
        let message = remote_request_error(&SOURCES[0], &error).to_string();

        assert!(message.contains("cdn"));
        assert!(!message.contains("127.0.0.1"));
        assert!(!message.contains("private"));
        assert!(!message.contains('@'));
    }
}
