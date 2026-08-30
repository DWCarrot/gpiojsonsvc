use thiserror::Error;

use crate::config::ConfigError;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("application bootstrap failed: {0}")]
    Bootstrap(String),
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("real backend unavailable; use --mock")]
    RealBackendUnavailable,
    #[error("GPIOJSONSVC_MOCK_LOG is set but --mock was not given")]
    MockLogWithoutMock,
    #[error("mock write log `{path}` is unavailable: {source}")]
    MockWriteLogUnavailable {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("device file `{device}` is unavailable: {source}")]
    UnavailableDeviceFile {
        device: String,
        #[source]
        source: std::io::Error,
    },
    #[error("device file `{device}` is not a valid one-chip XML document: {message}")]
    InvalidChipFile { device: String, message: String },
    #[error("line {line} is not available on device `{device}` (pin `{pin}`)")]
    MissingLine {
        pin: String,
        device: String,
        line: u32,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
