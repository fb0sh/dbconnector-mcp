use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidArgument,
    UnsupportedEngine,
    UnsupportedOperation,
    ConnectionFailed,
    ConnectionNotFound,
    PermissionDenied,
    QueryTimeout,
    QueryFailed,
    ResourceLimitExceeded,
    TransactionNotFound,
    TransactionConflict,
    InternalError,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::UnsupportedEngine => "UNSUPPORTED_ENGINE",
            Self::UnsupportedOperation => "UNSUPPORTED_OPERATION",
            Self::ConnectionFailed => "CONNECTION_FAILED",
            Self::ConnectionNotFound => "CONNECTION_NOT_FOUND",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::QueryTimeout => "QUERY_TIMEOUT",
            Self::QueryFailed => "QUERY_FAILED",
            Self::ResourceLimitExceeded => "RESOURCE_LIMIT_EXCEEDED",
            Self::TransactionNotFound => "TRANSACTION_NOT_FOUND",
            Self::TransactionConflict => "TRANSACTION_CONFLICT",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DbError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl DbError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn permission(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PermissionDenied, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnsupportedOperation, message)
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for DbError {}
