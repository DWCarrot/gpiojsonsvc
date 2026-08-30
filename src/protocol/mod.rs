pub mod common;
pub mod request;
pub mod response;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("request line must not contain line terminators")]
    InvalidFraming,
    #[error("request action `{0}` is not supported")]
    UnknownAction(String),
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    #[error("protocol validation failed: {0}")]
    Validation(String),
    #[error("protocol serialization failed: {0}")]
    Serialize(String),
}

#[derive(Debug, Clone, Deserialize)]
struct RequestEnvelope {
    id: String,
    action: String,
}

pub fn parse_request_line(line: &str) -> Result<request::RequestMessage, ProtocolError> {
    if line.contains('\n') || line.contains('\r') {
        return Err(ProtocolError::InvalidFraming);
    }

    let envelope: RequestEnvelope = serde_json::from_str(line)
        .map_err(|error| ProtocolError::InvalidJson(error.to_string()))?;

    if envelope.id.trim().is_empty() {
        return Err(ProtocolError::Validation(String::from(
            "request id must not be empty",
        )));
    }

    match envelope.action.as_str() {
        "init" | "get" | "set" => serde_json::from_str::<request::RequestMessage>(line)
            .map_err(|error| ProtocolError::InvalidJson(error.to_string())),
        other => Err(ProtocolError::UnknownAction(other.to_owned())),
    }
}

pub fn serialize_response(response: &response::ResponseMessage) -> Result<String, ProtocolError> {
    serde_json::to_string(response).map_err(|error| ProtocolError::Serialize(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::parse_request_line;
    use super::request::PinSelector;
    use super::request::RequestPayload;
    use super::response::EventPayload;
    use super::response::EventType;
    use super::response::PinValuePayload;
    use super::response::ResponseMessage;
    use super::serialize_response;

    /// Requests must be a single logical line (`parse_request_line` rejects `\n`).
    fn json_line(multiline_raw: &str) -> String {
        multiline_raw
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    #[test]
    fn parses_init_request_from_idea_example() {
        use crate::protocol::request::TargetConfigRequest;

        let line = json_line(
            r#"
            {
              "id": "init-1",
              "action": "init",
              "target": {
                "GPIO4_B3": {
                  "mode": "input",
                  "pin": "gpiochip0:2",
                  "bias": "as_is"
                },
                "CombinedIN2": {
                  "mode": "input",
                  "pin": ["gpiochip0:3", "gpiochip1:4", "gpiochip1:5"],
                  "bias": "pull_up"
                },
                "GPIO4_B5": {
                  "mode": "output",
                  "pin": "gpiochip2:1",
                  "drive": "push_pull"
                },
                "GPIO1_A2": {
                  "mode": "trigger",
                  "pin": "gpiochip1:1",
                  "edge": "rising"
                }
              }
            }
        "#,
        );

        let request = parse_request_line(&line).expect("init request should parse");

        match request.payload {
            RequestPayload::Init { target } => {
                assert_eq!(request.id, "init-1");
                assert_eq!(target.len(), 4);
                for (name, config) in target.iter() {
                    let n = name.as_str();
                    let (c, u) = match &config {
                        TargetConfigRequest::Input { pin, bias } => ("input", pin.len()),
                        TargetConfigRequest::Output { pin, drive } => ("output", pin.len()),
                        TargetConfigRequest::Trigger { pin, edge } => ("trigger", 1),
                    };
                    println!("name: {n}, config: {c}, unit: {u}");
                }
            }
            other => panic!("expected init request, got {other:?}"),
        }
    }

    #[test]
    fn parses_get_request_with_multiple_targets() {
        let line = json_line(
            r#"
            {
              "id": "get-1",
              "action": "get",
              "target": ["GPIO4_B3", "CombinedIN2"]
            }
        "#,
        );

        let request = parse_request_line(&line).expect("get request should parse");

        match request.payload {
            RequestPayload::Get { target } => {
                let targets: Vec<_> = target.as_slice().iter().collect();
                assert_eq!(targets, vec!["GPIO4_B3", "CombinedIN2"]);
            }
            other => panic!("expected get request, got {other:?}"),
        }
    }

    #[test]
    fn parses_set_request_with_sequence_steps() {
        let line = json_line(
            r#"
            {
              "id": "set-1",
              "action": "set",
              "target": [
                { "GPIO4_B5": 1, "GPIO4_B6": 1 },
                { "lag": 100, "GPIO4_B5": 0, "GPIO4_B6": 1 },
                { "lag": 200, "GPIO4_B5": 1, "GPIO4_B6": 0 }
              ]
            }
        "#,
        );

        let request = parse_request_line(&line).expect("set request should parse");

        match request.payload {
            RequestPayload::Set { target } => match target {
                super::request::SetRequest::Steps(target) => {
                    assert_eq!(target.len(), 3);
                    assert_eq!(target[0].lag, 0);
                    assert_eq!(target[0].target.get("GPIO4_B5"), Some(&1));
                    assert_eq!(target[1].lag, 100);
                    assert_eq!(target[1].target.get("GPIO4_B5"), Some(&0));
                }
                other => panic!("expected steps request, got {other:?}"),
            },
            other => panic!("expected set request, got {other:?}"),
        }
    }

    #[test]
    fn rejects_trigger_target_with_multiple_pins() {
        let line = json_line(
            r#"
            {
              "id": "init-1",
              "action": "init",
              "target": {
                "GPIO1_A2": {
                  "mode": "trigger",
                  "pin": ["gpiochip1:1", "gpiochip1:2"],
                  "edge": "rising"
                }
              }
            }
        "#,
        );

        let error = parse_request_line(&line).expect_err("trigger target should fail validation");

        assert!(error.to_string().contains("expected a string"));
    }

    #[test]
    fn rejects_input_target_with_too_many_pins() {
        let line = json_line(
            r#"
            {
              "id": "init-1",
              "action": "init",
              "target": {
                "CombinedIN": {
                  "mode": "input",
                  "pin": [
                    "gpiochip0:0",
                    "gpiochip0:1",
                    "gpiochip0:2",
                    "gpiochip0:3",
                    "gpiochip0:4",
                    "gpiochip0:5",
                    "gpiochip0:6",
                    "gpiochip0:7",
                    "gpiochip0:8"
                  ]
                }
              }
            }
        "#,
        );

        let error = parse_request_line(&line).expect_err("input target should fail validation");

        assert!(error.to_string().contains("more than 8 pins"));
    }

    #[test]
    fn rejects_set_sequence_missing_lag_after_first_step() {
        let line = json_line(
            r#"
            {
              "id": "set-1",
              "action": "set",
              "target": [
                { "GPIO4_B6": 1 },
                { "GPIO4_B6": 0 }
              ]
            }
        "#,
        );

        let error = parse_request_line(&line).expect_err("set sequence should fail validation");

        assert!(error.to_string().contains("must include lag"));
    }

    #[test]
    fn parses_set_request_with_immediate_pairs() {
        let line = json_line(
            r#"
            {
              "id": "set-2",
              "action": "set",
              "target": {
                "GPIO4_B5": 1,
                "GPIO4_B6": 2
              }
            }
        "#,
        );

        let request = parse_request_line(&line).expect("set immediate request should parse");

        match request.payload {
            RequestPayload::Set { target } => match target {
                super::request::SetRequest::Immediate(target) => {
                    assert_eq!(target.get("GPIO4_B5"), Some(&1));
                    assert_eq!(target.get("GPIO4_B6"), Some(&2));
                }
                other => panic!("expected immediate request, got {other:?}"),
            },
            other => panic!("expected set request, got {other:?}"),
        }
    }

    #[test]
    fn rejects_set_request_with_legacy_immediate_and_steps_fields() {
        let line = json_line(
            r#"
            {
              "id": "set-3",
              "action": "set",
              "immediate": {
                "GPIO4_B5": 1
              },
              "steps": [
                { "GPIO4_B5": 1 },
                { "lag": 100, "GPIO4_B5": 0 }
              ]
            }
        "#,
        );

        let error = parse_request_line(&line).expect_err("set should reject legacy fields");
        assert!(error.to_string().contains("target"));
    }

    #[test]
    fn rejects_set_request_with_empty_target_object() {
        let line = json_line(
            r#"
            {
              "id": "set-4",
              "action": "set",
              "target": {}
            }
        "#,
        );

        let error = parse_request_line(&line).expect_err("set should reject empty target object");
        assert!(
            error
                .to_string()
                .contains("set target object must contain at least one target value")
        );
    }

    #[test]
    fn parses_combined_pin_selector_as_smallvec_backed_variant() {
        let line = json_line(
            r#"
            {
              "id": "init-1",
              "action": "init",
              "target": {
                "CombinedIN": {
                  "mode": "input",
                  "pin": ["gpiochip0:3", "gpiochip1:4"]
                }
              }
            }
        "#,
        );

        let request = parse_request_line(&line).expect("init request should parse");

        match request.payload {
            RequestPayload::Init { target } => match target.get("CombinedIN") {
                Some(super::request::TargetConfigRequest::Input { pin, .. }) => match pin {
                    PinSelector::Combined(pins) => {
                        assert_eq!(pins.len(), 2);
                        assert_eq!(pins[0], "gpiochip0:3");
                        assert_eq!(pins[1], "gpiochip1:4");
                    }
                    other => panic!("expected combined pin selector, got {other:?}"),
                },
                other => panic!("expected input target, got {other:?}"),
            },
            other => panic!("expected init request, got {other:?}"),
        }
    }

    #[test]
    fn serializes_ok_response() {
        let response = ResponseMessage::ok("req-1");
        let line = serialize_response(&response).expect("response should serialize");

        assert_eq!(line, json_line(r#"{"id":"req-1","status":"ok"}"#));
    }

    #[test]
    fn serializes_pin_value_response() {
        let response = ResponseMessage::pin_value("get-1", PinValuePayload::Value(1));
        let line = serialize_response(&response).expect("response should serialize");

        assert_eq!(
            line,
            json_line(r#"{"id":"get-1","status":"pin_value","value":1}"#)
        );
    }

    #[test]
    fn serializes_event_response() {
        let response = ResponseMessage::event(
            "init-1",
            EventPayload {
                target: String::from("GPIO4_B5"),
                kind: EventType::Rising,
            },
        );

        let line = serialize_response(&response).expect("event response should serialize");

        assert_eq!(
            line,
            json_line(
                r#"
                {
                  "id": "init-1",
                  "status": "event",
                  "event": { "target": "GPIO4_B5", "type": "rising" }
                }
                "#
            )
        );
    }
}
