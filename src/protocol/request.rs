use std::fmt;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de;
use serde::de::MapAccess;
use serde::de::SeqAccess;
use serde::de::Visitor;
use smallvec::SmallVec;

use super::common::ArrayMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestMessage {
    pub id: String,
    #[serde(flatten)]
    pub payload: RequestPayload,
}

impl RequestMessage {
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum RequestPayload {
    #[serde(rename = "init")]
    Init {
        target: ArrayMap<PinSelector, PinConfigRequest>,
    },
    #[serde(rename = "get")]
    Get { target: TargetSelector },
    #[serde(rename = "set")]
    Set { target: SetRequest },
}

/// One configured pin name or an ordered `|`-separated pin combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinSelector {
    expression: String,
    /// Exclusive byte end offsets for each pin, including the final pin.
    split_indices: SmallVec<[usize; 8]>,
}

impl PinSelector {
    pub fn parse(expression: String) -> Result<Self, String> {
        let mut split_indices = SmallVec::new();
        let mut start = 0;
        for pin in expression.split('|') {
            validate_pin_name(pin)?;
            let mut previous_start = 0;
            for &end in &split_indices {
                if &expression[previous_start..end] == pin {
                    return Err(format!("duplicate pin `{pin}` in expression"));
                }
                previous_start = end + 1;
            }
            let end = start + pin.len();
            split_indices.push(end);
            start = end + 1;
        }
        Ok(Self {
            expression,
            split_indices,
        })
    }

    pub fn len(&self) -> usize {
        self.split_indices.len()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> {
        self.split_indices.iter().enumerate().map(|(index, &end)| {
            let start = if index == 0 {
                0
            } else {
                self.split_indices[index - 1] + 1
            };
            &self.expression[start..end]
        })
    }

    pub fn is_single(&self) -> bool {
        self.len() == 1
    }
}

impl fmt::Display for PinSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.expression)
    }
}

impl Serialize for PinSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.expression)
    }
}

impl<'de> Deserialize<'de> for PinSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let expression = String::deserialize(deserializer)?;
        Self::parse(expression).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BiasMode {
    AsIs,
    Disabled,
    PullUp,
    PullDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriveMode {
    PushPull,
    OpenDrain,
    OpenSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeMode {
    Rising,
    Falling,
    Both,
}

fn default_edge_mode() -> EdgeMode {
    EdgeMode::Rising
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", deny_unknown_fields)]
pub enum PinConfigRequest {
    #[serde(rename = "input")]
    Input {
        #[serde(default)]
        bias: Option<BiasMode>,
    },
    #[serde(rename = "output")]
    Output {
        #[serde(default)]
        drive: Option<DriveMode>,
        #[serde(default, rename = "initial")]
        initial_value: Option<u8>,
        #[serde(default, rename = "final")]
        final_value: Option<u8>,
    },
    #[serde(rename = "trigger")]
    Trigger {
        #[serde(default = "default_edge_mode")]
        edge: EdgeMode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum TargetSelector {
    Single(String),
    Multiple(Vec<String>),
}

struct TargetSelectorVisitor;

impl<'de> Visitor<'de> for TargetSelectorVisitor {
    type Value = TargetSelector;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a target selector string or non-empty string array")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.is_empty() {
            return Err(de::Error::custom("pin expression must not be empty"));
        }
        validate_pin_name(value).map_err(de::Error::custom)?;
        Ok(TargetSelector::Single(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.is_empty() {
            return Err(de::Error::custom("pin expression must not be empty"));
        }
        validate_pin_name(&value).map_err(de::Error::custom)?;
        Ok(TargetSelector::Single(value))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut targets = Vec::<String>::new();
        while let Some(target) = seq.next_element::<String>()? {
            validate_pin_name(&target).map_err(de::Error::custom)?;
            targets.push(target);
        }

        if targets.is_empty() {
            return Err(de::Error::custom("target list must not be empty"));
        }

        Ok(TargetSelector::Multiple(targets))
    }
}

impl<'de> Deserialize<'de> for TargetSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TargetSelectorVisitor)
    }
}

impl TargetSelector {
    pub fn as_slice(&self) -> &[String] {
        match self {
            Self::Single(target) => std::slice::from_ref(target),
            Self::Multiple(targets) => targets.as_slice(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum SetRequest {
    Immediate(ArrayMap<String, u8>),
    Steps(Vec<SetStepRequest>),
}

impl<'de> Deserialize<'de> for SetRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SetRequestVisitor)
    }
}

struct SetRequestVisitor;

impl<'de> Visitor<'de> for SetRequestVisitor {
    type Value = SetRequest;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a non-empty target object or non-empty target step array")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let target = ArrayMap::deserialize(de::value::MapAccessDeserializer::new(map))?;
        if target.is_empty() {
            return Err(de::Error::custom(
                "set target object must contain at least one target value",
            ));
        }
        Ok(SetRequest::Immediate(target))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut target = Vec::<SetStepRequest>::new();
        while let Some(step) = seq.next_element::<SetStepRequest>()? {
            target.push(step);
        }
        if target.is_empty() {
            return Err(de::Error::custom(
                "set target steps must contain at least one step",
            ));
        }
        for (index, step) in target.iter().enumerate() {
            if index == 0 && step.lag > 0 {
                return Err(de::Error::custom("set target[0] must not include lag"));
            }
            if index > 0 && step.lag == 0 {
                return Err(de::Error::custom(format!(
                    "set target[{}] must include lag",
                    index
                )));
            }
        }
        Ok(SetRequest::Steps(target))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetStepRequest {
    #[serde(default)]
    pub lag: u32,
    #[serde(flatten)]
    pub target: ArrayMap<String, u8>,
}

#[derive(Debug, Deserialize)]
struct RawSetStepRequest {
    #[serde(default)]
    lag: u32,
    #[serde(flatten)]
    target: ArrayMap<String, u8>,
}

impl<'de> Deserialize<'de> for SetStepRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawSetStepRequest::deserialize(deserializer)?;
        if raw.target.is_empty() {
            return Err(de::Error::custom(
                "set step must contain at least one target value",
            ));
        }
        Ok(Self {
            lag: raw.lag,
            target: raw.target,
        })
    }
}

pub fn validate_pin_name(pin: &str) -> Result<(), String> {
    if pin.is_empty() {
        return Err("pin name must not be empty".to_owned());
    }
    if pin.contains('|') || pin == "lag" {
        return Err(format!(
            "pin name `{pin}` is reserved: `|` and the name `lag` cannot be used"
        ));
    }
    Ok(())
}

impl PinConfigRequest {
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Output {
            initial_value,
            final_value,
            ..
        } = self
        {
            for (field, value) in [("initial", initial_value), ("final", final_value)] {
                if value.is_some_and(|value| value > 1) {
                    return Err(format!("output {field} must be 0 or 1 for each pin"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PinSelector;

    #[test]
    fn parses_single_and_combined_pin_selectors() {
        for (expression, pins) in [
            ("GPIO4_B3", vec!["GPIO4_B3"]),
            ("GPIO4_B3|GPIO4_B2", vec!["GPIO4_B3", "GPIO4_B2"]),
            ("引脚甲| 引脚乙", vec!["引脚甲", " 引脚乙"]),
        ] {
            let selector = PinSelector::parse(expression.to_owned()).unwrap();
            assert_eq!(selector.len(), pins.len());
            assert_eq!(selector.is_single(), pins.len() == 1);
            assert_eq!(selector.iter().len(), pins.len());
            assert_eq!(selector.iter().collect::<Vec<_>>(), pins);
            assert_eq!(selector.to_string(), expression);
            let json = serde_json::to_string(&selector).unwrap();
            assert_eq!(json, serde_json::to_string(expression).unwrap());
            assert_eq!(
                serde_json::from_str::<PinSelector>(&json).unwrap(),
                selector
            );
        }
    }

    #[test]
    fn rejects_invalid_pin_selectors() {
        for expression in [
            "",
            "|A",
            "A|",
            "A||B",
            "A|A",
            "A|B|A",
            "lag",
            "A|lag",
            "甲|乙|甲",
        ] {
            assert!(
                PinSelector::parse(expression.to_owned()).is_err(),
                "{expression}"
            );
        }
    }

    #[test]
    fn combined_selector_can_grow_beyond_inline_capacity_for_init() {
        let inline = PinSelector::parse("A|B|C|D|E|F|G|H".to_owned()).unwrap();
        assert!(!inline.split_indices.spilled());
        let selector = PinSelector::parse("A|B|C|D|E|F|G|H|I".to_owned()).unwrap();
        assert!(selector.split_indices.spilled());
        assert_eq!(selector.len(), 9);
        assert!(!selector.is_single());
        assert_eq!(
            selector.iter().collect::<Vec<_>>(),
            ["A", "B", "C", "D", "E", "F", "G", "H", "I"]
        );
        assert_eq!(selector.to_string(), "A|B|C|D|E|F|G|H|I");
    }
}
