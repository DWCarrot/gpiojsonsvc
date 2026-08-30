use thiserror::Error;

use crate::gpio::GPIOError;
use crate::protocol::response::ResponseMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Connected,
    Initialized,
    SetSequenceRunning,
    Closing,
    Closed,
}

#[derive(Debug, Error)]
pub enum SessionError<'a> {
    #[error("session is already initialized")]
    AlreadyInitialized,
    #[error("session is not initialized")]
    NotInitialized,
    #[error("session is closed")]
    Closed,
    #[error("a set request is already in progress")]
    SetInProgress,
    #[error("unknown target `{target}`")]
    UnknownTarget { target: &'a str },
    #[error("target `{target}` is not readable")]
    TargetNotReadable { target: &'a str },
    #[error("target `{target}` is not writable")]
    TargetNotWritable { target: &'a str },
    #[error("target `{target}` value {value} exceeds {bits} configured bits")]
    TargetValueOutOfRange {
        target: &'a str,
        value: u8,
        bits: usize,
    },
    #[error("unmapped pin `{pin}`")]
    UnmappedPin { pin: &'a str },
    #[error("device file `{device}` is unavailable")]
    UnavailableDeviceFile { device: String },
    #[error("line {line} is not available on device `{device}`")]
    MissingLine { device: String, line: u32 },
    #[error("pin `{pin}` maps to a duplicate physical location")]
    DuplicatePhysicalLocation { pin: &'a str },
    #[error("no trigger target is configured for chip {chip_index} offset {offset}")]
    UnmappedTriggerPin { chip_index: u32, offset: u32 },
    #[error(transparent)]
    GPIO(GPIOError),
    #[error("{0}")]
    Other(String),
}

impl<'a> SessionError<'a> {
    pub fn into_response(self, request_id: impl Into<String>) -> ResponseMessage {
        ResponseMessage::error(request_id, self.to_string())
    }
}

impl<'a> From<GPIOError> for SessionError<'a> {
    fn from(error: GPIOError) -> Self {
        SessionError::GPIO(error)
    }
}
