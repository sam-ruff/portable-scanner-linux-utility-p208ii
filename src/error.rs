use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TransportError {
    #[error("USB transfer timed out")]
    Timeout,
    #[error("USB endpoint stalled")]
    Stall,
    #[error("scanner was disconnected")]
    Disconnected,
    #[error("USB error: {0}")]
    Other(String),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScanError {
    #[error("{0}")]
    NotFound(String),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("no paper in the feeder")]
    NoDocuments,
    #[error("paper jam: {0}")]
    Jammed(&'static str),
    #[error("scanner cover is open")]
    CoverOpen,
    #[error("scanner is busy")]
    Busy,
    #[error("scan was cancelled by the scanner")]
    Cancelled,
    #[error("scanner rejected the request: {0}")]
    InvalidRequest(&'static str),
    #[error("scanner hardware error: {0}")]
    Hardware(&'static str),
    #[error("scanner ran out of memory")]
    OutOfMemory,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("I/O error: {0}")]
    Io(String),
    #[error("{0}")]
    Folder(String),
}

impl From<std::io::Error> for ScanError {
    fn from(err: std::io::Error) -> Self {
        ScanError::Io(err.to_string())
    }
}
