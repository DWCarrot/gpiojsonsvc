use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;

use crate::protocol::common::ArrayMap;

use super::common::deserialize_non_empty_string;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseMessage {
    pub id: String,
    #[serde(flatten)]
    pub status: ResponseStatus,
}

impl ResponseMessage {
    pub fn ok(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: ResponseStatus::Ok {},
        }
    }

    pub fn get_result(id: impl Into<String>, value: PinValuePayload) -> Self {
        Self {
            id: id.into(),
            status: ResponseStatus::GetResult { value },
        }
    }

    pub fn query_result(id: impl Into<String>, result: QueryResultPayload) -> Self {
        Self {
            id: id.into(),
            status: ResponseStatus::QueryResult(result),
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
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseStatus {
    // A struct variant lets Serde reject unknown fields even for this empty reply.
    Ok {},
    Error { error: ErrorPayload },
    Event { event: EventPayload },
    GetResult { value: PinValuePayload },
    QueryResult(QueryResultPayload),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "lowercase", deny_unknown_fields)]
pub enum QueryResultPayload {
    Gpio { pins: BTreeMap<String, GpioPinInfo> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GpioPinInfo {
    pub id: u32,
    pub is_used: bool,
    pub consumer: Option<String>,
    pub direction: GpioDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpioDirection {
    Input,
    Output,
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
