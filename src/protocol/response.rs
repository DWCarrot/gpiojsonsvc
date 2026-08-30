use serde::Deserialize;
use serde::Serialize;

use super::common::deserialize_non_empty_string;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseMessage {
    pub id: String,
    #[serde(flatten)]
    pub status: ResponseStatus,
}

impl ResponseMessage {
    pub fn ok(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: ResponseStatus::Ok,
        }
    }

    pub fn pin_value(id: impl Into<String>, value: PinValuePayload) -> Self {
        Self {
            id: id.into(),
            status: ResponseStatus::PinValue { value },
        }
    }

    pub fn event(id: impl Into<String>, event: EventPayload) -> Self {
        Self {
            id: id.into(),
            status: ResponseStatus::Event { event },
        }
    }

    pub fn error(id: impl Into<String>, error: ErrorPayload) -> Self {
        Self {
            id: id.into(),
            status: ResponseStatus::Error { error },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResponseStatus {
    Ok,
    Error { error: ErrorPayload },
    Event { event: EventPayload },
    PinValue { value: PinValuePayload },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PinValuePayload {
    Value(u8),
    Values(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventPayload {
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    pub target: String,
    #[serde(rename = "type")]
    pub kind: EventType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventType {
    Rising,
    Falling,
}

pub type ErrorPayload = String;
