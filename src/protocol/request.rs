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
pub enum PinSelector {
    Single(String),
    Combined(SmallVec<[String; 8]>),
}

impl PinSelector {
    pub fn parse(expression: String) -> Result<Self, String> {
        if !expression.contains('|') {
            validate_pin_name(&expression)?;
            return Ok(Self::Single(expression));
        }

        let mut pins = SmallVec::new();
        for pin in expression.split('|') {
            validate_pin_name(pin)?;
            if pins.iter().any(|existing: &String| existing == pin) {
                return Err(format!("duplicate pin `{pin}` in expression"));
            }
            pins.push(pin.to_owned());
        }
        Ok(Self::Combined(pins))
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Combined(pins) => pins.len(),
        }
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> {
        let pins: &[String] = match self {
            Self::Single(pin) => std::slice::from_ref(pin),
            Self::Combined(pins) => pins.as_slice(),
        };
        pins.iter().map(String::as_str)
    }
}

impl fmt::Display for PinSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Single(pin) => formatter.write_str(pin),
            Self::Combined(pins) => formatter.write_str(&pins.join("|")),
        }
    }
}

impl Serialize for PinSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
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
    use smallvec::smallvec;

    use super::PinSelector;

    #[test]
    fn parses_single_and_combined_pin_selectors() {
        assert_eq!(
            PinSelector::parse("GPIO4_B3".to_owned()).unwrap(),
            PinSelector::Single("GPIO4_B3".to_owned())
        );
        assert_eq!(
            PinSelector::parse("GPIO4_B3|GPIO4_B2".to_owned()).unwrap(),
            PinSelector::Combined(smallvec!["GPIO4_B3".to_owned(), "GPIO4_B2".to_owned(),])
        );
    }

    #[test]
    fn combined_selector_can_grow_beyond_inline_capacity_for_init() {
        let selector = PinSelector::parse("A|B|C|D|E|F|G|H|I".to_owned()).unwrap();
        assert_eq!(selector.len(), 9);
        assert_eq!(selector.to_string(), "A|B|C|D|E|F|G|H|I");
    }
}
