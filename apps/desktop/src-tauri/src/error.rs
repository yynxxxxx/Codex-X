use serde::Serializer;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum CodexxError {
    #[error("无法获取用户主目录")]
    NoHomeDir,
    #[error("IO error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error at {path}: {message}")]
    Toml { path: String, message: String },
    #[error("JSON error at {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("配置错误: {0}")]
    Config(String),
    #[error("SQLite error: {0}")]
    Database(String),
}

pub(crate) type Result<T> = std::result::Result<T, CodexxError>;

impl serde::Serialize for CodexxError {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
