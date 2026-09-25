//! Error type shared by the whole crate.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Net(String),
    #[error("HTTP {0} for {1}")]
    Status(u16, String),
    #[error("the recording is private or the link is wrong (HTTP {0}); pass a session id: --session-id or WEBINARIP_SESSION_ID")]
    Access(u16),
    #[error("unexpected data: {0}")]
    Parse(String),
    #[error("decode: {0}")]
    Decode(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
