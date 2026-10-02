pub mod common;
pub mod request;
pub mod response;

use serde::Deserialize;
use thiserror::Error;

use self::request::RequestPayload;

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
        "init" | "get" | "set" => {
            let request = serde_json::from_str::<request::RequestMessage>(line)
                .map_err(|error| ProtocolError::InvalidJson(error.to_string()))?;
            validate_request(&request)?;
            Ok(request)
        }
        other => Err(ProtocolError::UnknownAction(other.to_owned())),
    }
}

fn validate_request(request: &request::RequestMessage) -> Result<(), ProtocolError> {
    let validate_write = |name: &str, value: u8| {
        request::validate_pin_name(name).map_err(ProtocolError::Validation)?;
        if value > 1 {
            return Err(ProtocolError::Validation(format!(
                "pin `{name}` value must be 0 or 1"
            )));
        }
        Ok(())
    };
    match &request.payload {
        RequestPayload::Init { target } => {
            if target.is_empty() {
                return Err(ProtocolError::Validation(
                    "init target must not be empty".to_owned(),
                ));
            }
            for (_, config) in target {
                config.validate().map_err(ProtocolError::Validation)?;
            }
        }
        RequestPayload::Get { target } => {
            for selector in target.as_slice() {
                request::validate_pin_name(selector).map_err(ProtocolError::Validation)?;
            }
        }
        RequestPayload::Set { target } => match target {
            request::SetRequest::Immediate(writes) => {
                for (name, value) in writes {
                    validate_write(name, *value)?;
                }
            }
            request::SetRequest::Steps(steps) => {
                for step in steps {
                    for (name, value) in &step.target {
                        validate_write(name, *value)?;
                    }
                }
            }
        },
    }
    Ok(())
}

pub fn serialize_response(response: &response::ResponseMessage) -> Result<String, ProtocolError> {
    serde_json::to_string(response).map_err(|error| ProtocolError::Serialize(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::parse_request_line;
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
                let targets: Vec<_> = target.as_slice().iter().map(ToString::to_string).collect();
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
                    assert_eq!(target[0].target.iter().next().unwrap().1, &1);
                    assert_eq!(target[1].lag, 100);
                    assert_eq!(target[1].target.iter().next().unwrap().1, &0);
                }
                other => panic!("expected steps request, got {other:?}"),
            },
            other => panic!("expected set request, got {other:?}"),
        }
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
                "GPIO4_B6": 0
              }
            }
        "#,
        );

        let request = parse_request_line(&line).expect("set immediate request should parse");

        match request.payload {
            RequestPayload::Set { target } => match target {
                super::request::SetRequest::Immediate(target) => {
                    assert_eq!(
                        target
                            .iter()
                            .map(|(pin, value)| (pin.to_string(), *value))
                            .collect::<Vec<_>>(),
                        [("GPIO4_B5".to_owned(), 1), ("GPIO4_B6".to_owned(), 0)]
                    );
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
    #[test]
    fn parses_shared_init_parameters_and_round_trips() {
        let line = r#"{"id":"1","action":"init","target":{"A|B":{"mode":"output","initial":1,"final":0},"C|D":{"mode":"trigger","edge":"both"},"E":{"mode":"input"}}}"#;
        let request = parse_request_line(line).unwrap();
        let RequestPayload::Init { target } = &request.payload else {
            panic!("init")
        };
        assert_eq!(target.len(), 3);
        assert!(matches!(
            target.iter().next().unwrap().1,
            super::request::PinConfigRequest::Output {
                initial_value: Some(1),
                final_value: Some(0),
                ..
            }
        ));
        assert_eq!(
            parse_request_line(&serde_json::to_string(&request).unwrap()).unwrap(),
            request
        );
    }

    #[test]
    fn rejects_legacy_binding_in_all_modes() {
        for mode in ["input", "output", "trigger"] {
            let line = serde_json::json!({"id":"1","action":"init","target":{"A":{"mode":mode,"pin":"B"}}}).to_string();
            assert!(
                parse_request_line(&line)
                    .unwrap_err()
                    .to_string()
                    .contains("unknown field `pin`")
            );
        }
    }

    #[test]
    fn rejects_non_bit_initial_and_final_even_for_groups() {
        for field in ["initial", "final"] {
            for value in [2, 255] {
                let line = serde_json::json!({"id":"1","action":"init","target":{"A|B":{"mode":"output",field:value}}}).to_string();
                assert!(
                    parse_request_line(&line)
                        .unwrap_err()
                        .to_string()
                        .contains("must be 0 or 1")
                );
            }
        }
    }

    #[test]
    fn init_groups_are_not_limited_to_eight_pins() {
        let line = r#"{"id":"1","action":"init","target":{"A|B|C|D|E|F|G|H|I":{"mode":"input"}}}"#;
        assert!(parse_request_line(line).is_ok());
        for action in ["get", "set"] {
            let target = if action == "get" {
                serde_json::json!("A|B|C|D|E|F|G|H|I")
            } else {
                serde_json::json!({"A|B|C|D|E|F|G|H|I":0})
            };
            let line = serde_json::json!({"id":"1","action":action,"target":target}).to_string();
            assert!(
                parse_request_line(&line)
                    .unwrap_err()
                    .to_string()
                    .contains("reserved")
            );
        }
    }

    #[test]
    fn rejects_malformed_expressions_in_every_request_form() {
        for expression in ["", "|A", "A|", "A||B", "A|A", "lag", "A|lag"] {
            for request in [
                serde_json::json!({"id":"1","action":"init","target":{expression:{"mode":"input"}}}),
                serde_json::json!({"id":"1","action":"get","target":expression}),
                serde_json::json!({"id":"1","action":"get","target":[expression]}),
                serde_json::json!({"id":"1","action":"set","target":{expression:0}}),
                serde_json::json!({"id":"1","action":"set","target":[{expression:0}]}),
            ] {
                assert!(
                    parse_request_line(&request.to_string()).is_err(),
                    "{request}"
                );
            }
        }
    }

    #[test]
    fn expressions_preserve_exact_names_and_order() {
        assert_eq!(
            super::request::PinSelector::parse("B| A".to_owned())
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            ["B", " A"]
        );
        assert!(parse_request_line(r#"{"id":"1","action":"init","target":{}}"#).is_err());
    }
    #[test]
    fn rejects_duplicate_json_init_keys() {
        let error = parse_request_line(
            r#"{"id":"1","action":"init","target":{"A":{"mode":"input"},"A":{"mode":"output"}}}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("duplicate key"));
    }
    #[test]
    fn expression_requests_round_trip_without_changing_shape() {
        for line in [
            r#"{"id":"1","action":"get","target":"A"}"#,
            r#"{"id":"1","action":"get","target":["A","B"]}"#,
            r#"{"id":"1","action":"set","target":{"A":1,"B":0}}"#,
            r#"{"id":"1","action":"set","target":[{"A":1,"B":0},{"lag":1,"B":1,"A":0}]}"#,
        ] {
            let request = parse_request_line(line).unwrap();
            assert_eq!(
                parse_request_line(&serde_json::to_string(&request).unwrap()).unwrap(),
                request
            );
        }
    }
    #[test]
    fn rejects_shared_names_in_get_and_all_set_forms() {
        for line in [
            r#"{"id":"1","action":"get","target":"A|B"}"#,
            r#"{"id":"1","action":"get","target":["A","B|C"]}"#,
            r#"{"id":"1","action":"set","target":{"A|B":1}}"#,
            r#"{"id":"1","action":"set","target":[{"A":1},{"lag":1,"A|B":0}]}"#,
        ] {
            assert!(parse_request_line(line).is_err(), "{line}");
        }
    }

    #[test]
    fn set_values_are_single_bits_in_every_step() {
        for value in [2, 255] {
            for target in [
                serde_json::json!({"A":value}),
                serde_json::json!([{"A":0},{"lag":1,"A":value}]),
            ] {
                let line = serde_json::json!({"id":"1","action":"set","target":target}).to_string();
                assert!(
                    parse_request_line(&line)
                        .unwrap_err()
                        .to_string()
                        .contains("must be 0 or 1")
                );
            }
        }
    }

    #[test]
    fn duplicate_set_keys_are_rejected_and_multiple_reads_have_no_eight_pin_limit() {
        for line in [
            r#"{"id":"1","action":"set","target":{"A":0,"A":1}}"#,
            r#"{"id":"1","action":"set","target":[{"A":0,"A":1}]}"#,
        ] {
            assert!(parse_request_line(line).is_err());
        }
        assert!(
            parse_request_line(
                r#"{"id":"1","action":"get","target":["A","B","C","D","E","F","G","H","I"]}"#
            )
            .is_ok()
        );
    }
}
