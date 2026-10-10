use serde::Serialize;

/// Every fallible boundary in the app funnels through this type so the UI
/// always receives a structured, displayable error instead of a panic or a
/// stringly-typed blob.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("no OpenRouter API key configured")]
    MissingApiKey,

    #[error("cancelled")]
    Cancelled,

    #[error("timed out after {0}ms")]
    Timeout(u64),

    #[error("upstream error ({status}): {message}")]
    Upstream { status: u16, message: String },

    #[error("permission denied: {0}")]
    Denied(String),

    #[error(transparent)]
    Db(#[from] rusqlite::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl AppError {
    pub fn msg(s: impl Into<String>) -> Self {
        Self::Message(s.into())
    }

    /// Short machine-readable discriminant, so the UI can special-case things
    /// like "missing key" without string matching on the message.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Message(_) => "error",
            Self::NotFound(_) => "not_found",
            Self::MissingApiKey => "missing_api_key",
            Self::Cancelled => "cancelled",
            Self::Timeout(_) => "timeout",
            Self::Upstream { .. } => "upstream",
            Self::Denied(_) => "denied",
            Self::Db(_) => "db",
            Self::Io(_) => "io",
            Self::Http(_) => "network",
            Self::Json(_) => "decode",
            Self::Other(_) => "error",
        }
    }
}

#[derive(Serialize)]
struct WireError<'a> {
    kind: &'a str,
    message: String,
}

impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        WireError { kind: self.kind(), message: self.to_string() }.serialize(s)
    }
}

pub type Result<T, E = AppError> = std::result::Result<T, E>;
