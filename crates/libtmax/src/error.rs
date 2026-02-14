use thiserror::Error;
use tmax_protocol::SessionId;

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
