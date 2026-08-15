use std::fmt;

/// AI operation names are intentionally low-cardinality so errors are safe to log.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiOperation {
    Chat,
    Analysis,
    Completion,
}

impl fmt::Display for AiOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Chat => "chat",
            Self::Analysis => "analysis",
            Self::Completion => "completion",
        })
    }
}

/// Errors returned by an [`AiProvider`](crate::AiProvider).
///
/// Variants never contain the API key, authorization header, request payload, or
/// response body. This makes ordinary error reporting safe by construction.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("invalid AI configuration: {0}")]
    Configuration(String),

    #[error("AI API key is missing from environment variable {env_var}")]
    MissingApiKey { env_var: String },

    #[error("AI support is offline")]
    Offline,

    #[error("{operation} request was cancelled")]
    Cancelled { operation: AiOperation },

    #[error("{operation} request timed out")]
    Timeout { operation: AiOperation },

    #[error("AI transport failed during {operation}")]
    Transport { operation: AiOperation },

    #[error("AI service returned HTTP status {status} during {operation}")]
    HttpStatus { operation: AiOperation, status: u16 },

    #[error("AI service returned an invalid {operation} response: {reason}")]
    InvalidResponse {
        operation: AiOperation,
        reason: &'static str,
    },

    #[error("AI stream protocol error: {reason}")]
    StreamProtocol { reason: &'static str },
}

pub type AiResult<T> = Result<T, AiError>;
