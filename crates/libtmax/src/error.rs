use thiserror::Error;
use tmax_protocol::{ErrorCode, SessionId};

#[derive(Error, Debug)]
pub enum TmaxError {
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    #[error("input denied: session {0} requires edit attachment")]
    InputDenied(SessionId),

    #[error("attachment denied: {0}")]
    AttachmentDenied(String),

    #[error("sandbox violation: {0}")]
    SandboxViolation(String),

    #[error("pty error: {0}")]
    PtyError(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("session already exited: {0}")]
    SessionExited(SessionId),
}

impl TmaxError {
    /// Convert to protocol error code and sanitized message.
    pub fn to_error_code(&self) -> (ErrorCode, String) {
        match self {
            TmaxError::SessionNotFound(_) => (ErrorCode::SessionNotFound, self.to_string()),
            TmaxError::InputDenied(_) => (ErrorCode::InputDenied, self.to_string()),
            TmaxError::AttachmentDenied(_) => (ErrorCode::AttachmentDenied, self.to_string()),
            TmaxError::SandboxViolation(_) => (ErrorCode::SandboxViolation, self.to_string()),
            TmaxError::PtyError(_) => (ErrorCode::ServerError, self.to_string()),
            TmaxError::Io(_) => (ErrorCode::ServerError, "internal I/O error".to_string()),
            TmaxError::SessionExited(_) => (ErrorCode::SessionNotFound, self.to_string()),
        }
    }
}
