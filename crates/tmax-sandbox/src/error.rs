use thiserror::Error;

#[derive(Error, Debug)]
pub enum SandboxError {
    #[error("invalid sandbox config: {0}")]
    InvalidConfig(String),

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("nesting violation: {0}")]
    NestingViolation(String),
}
