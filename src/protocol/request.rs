use std::collections::BTreeMap;
use std::fmt;
use std::slice::Iter as SliceIter;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de;
use serde::de::MapAccess;
use serde::de::SeqAccess;
use serde::de::Visitor;
use smallvec::SmallVec;

use super::common::deserialize_non_empty_string;

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
        target: BTreeMap<String, TargetConfigRequest>,
    },
    #[serde(rename = "get")]
    Get { target: TargetSelector },
    #[serde(rename = "set")]
    Set { target: SetRequest },
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
#[serde(tag = "mode")]
pub enum TargetConfigRequest {
    #[serde(rename = "input")]
    Input {
        pin: PinSelector,
        #[serde(default)]
        bias: Option<BiasMode>,
    },
    #[serde(rename = "output")]
    Output {
        pin: PinSelector,
        #[serde(default)]
        drive: Option<DriveMode>,
    },
    #[serde(rename = "trigger")]
    Trigger {
        #[serde(deserialize_with = "deserialize_non_empty_string")]
        pin: String,
        #[serde(default = "default_edge_mode")]
        edge: EdgeMode,
    },
}

impl TargetConfigRequest {
    #[deprecated = "build a iterator instead of constructing a vector"]
    pub fn pins(&self) -> impl Iterator<Item = &str> {
        let pins: Vec<&str> = match self {
            Self::Input { pin, .. } | Self::Output { pin, .. } => pin.iter().collect(),
            Self::Trigger { pin, .. } => vec![pin.as_str()],
        };

        pins.into_iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PinSelector {
    Single(String),
    Combined(SmallVec<[String; 8]>),
}

impl PinSelector {
    pub fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Combined(pins) => pins.len(),
        }
    }

    pub fn iter<'a>(&'a self) -> PinSelectorIter<'a> {
        PinSelectorIter::new(self)
    }
}

impl<'de> Deserialize<'de> for PinSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(PinSelectorVisitor)
    }
}

struct PinSelectorVisitor;

impl<'de> Visitor<'de> for PinSelectorVisitor {
    type Value = PinSelector;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a pin selector")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.is_empty() {
            return Err(serde::de::Error::custom("pin name must not be empty"));
        }
        Ok(PinSelector::Single(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.is_empty() {
            return Err(serde::de::Error::custom("pin name must not be empty"));
        }
        Ok(PinSelector::Single(value))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let len = seq.size_hint().unwrap_or(0);
        if len > 8 {
            return Err(serde::de::Error::custom(
                "combined pin list must not contain more than 8 pins",
            ));
        }
        let mut values = SmallVec::new();

        while let Some(value) = seq.next_element::<String>()? {
            if value.is_empty() {
                return Err(serde::de::Error::custom("pin name must not be empty"));
            }
            if values.len() >= 8 {
                return Err(serde::de::Error::custom(
                    "combined pin list must not contain more than 8 pins",
                ));
            }
            values.push(value);
        }

        if values.is_empty() {
            return Err(serde::de::Error::custom(
                "combined pin list must not be empty",
            ));
        }

        Ok(PinSelector::Combined(values))
    }
}

pub struct PinSelectorIter<'a> {
    fixed: [Option<&'a String>; 2],
    fixed_len: usize,
    combined: &'a [String],
    index: usize,
}

impl<'a> PinSelectorIter<'a> {
    pub fn new(selector: &'a PinSelector) -> Self {
        match selector {
            PinSelector::Single(pin) => Self {
                fixed: [Some(pin), None],
                fixed_len: 1,
                combined: &[],
                index: 0,
            },
            PinSelector::Combined(pins) => Self {
                fixed: [None, None],
                fixed_len: 0,
                combined: pins.as_slice(),
                index: 0,
            },
        }
    }

    pub fn len(&self) -> usize {
        if self.fixed_len > 0 {
            self.fixed_len
        } else {
            self.combined.len()
        }
    }

    pub fn reset(&mut self) {
        self.index = 0;
    }
}

impl<'a> Iterator for PinSelectorIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.fixed_len > 0 {
            if self.index < self.fixed_len {
                let v = unsafe { self.fixed.get_unchecked(self.index).unwrap_unchecked() };
                self.index += 1;
                Some(v.as_str())
            } else {
                None
            }
        } else {
            if let Some(v) = self.combined.get(self.index) {
                self.index += 1;
                Some(v.as_str())
            } else {
                None
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.index += n;
        self.next()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
        if value.trim().is_empty() {
            return Err(de::Error::custom("target name must not be empty"));
        }
        Ok(TargetSelector::Single(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.trim().is_empty() {
            return Err(de::Error::custom("target name must not be empty"));
        }
        Ok(TargetSelector::Single(value))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut targets = Vec::<String>::new();
        while let Some(target) = seq.next_element::<String>()? {
            if target.trim().is_empty() {
                return Err(de::Error::custom("target name must not be empty"));
            }
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
    pub fn as_slice<'a>(&'a self) -> &'a [String] {
        match self {
            Self::Single(target) => std::slice::from_ref(target),
            Self::Multiple(targets) => targets.as_slice(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum SetRequest {
    Immediate(BTreeMap<String, u8>),
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
        let mut target = BTreeMap::new();
        while let Some((name, value)) = map.next_entry::<String, u8>()? {
            target.insert(name, value);
        }
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
    pub target: BTreeMap<String, u8>,
}

#[derive(Debug, Deserialize)]
struct RawSetStepRequest {
    #[serde(default)]
    lag: u32,
    #[serde(flatten)]
    target: BTreeMap<String, u8>,
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

#[cfg(test)]
mod pin_selector_iter_tests {
    use super::PinSelector;
    use super::PinSelectorIter;
    use smallvec::smallvec;

    #[test]
    fn single_pin_yields_one_element() {
        let selector = PinSelector::Single("gpiochip0:2".to_owned());
        let pins: Vec<_> = selector.iter().collect();
        assert_eq!(pins, vec!["gpiochip0:2"]);
        assert_eq!(selector.len(), 1);
    }

    #[test]
    fn combined_pins_yield_in_order() {
        let selector = PinSelector::Combined(smallvec![
            "gpiochip0:3".to_owned(),
            "gpiochip1:4".to_owned(),
            "gpiochip1:5".to_owned(),
        ]);
        let pins: Vec<_> = selector.iter().collect();
        assert_eq!(pins, vec!["gpiochip0:3", "gpiochip1:4", "gpiochip1:5"]);
        assert_eq!(selector.len(), 3);
    }

    #[test]
    fn iter_len_is_total_pin_count() {
        let selector = PinSelector::Combined(smallvec!["a".to_owned(), "b".to_owned(),]);
        let mut iter = selector.iter();
        assert_eq!(iter.len(), 2);
        assert_eq!(iter.next(), Some("a"));
        assert_eq!(iter.len(), 2);
        assert_eq!(iter.next(), Some("b"));
        assert_eq!(iter.len(), 2);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn size_hint_reports_exact_length() {
        let selector = PinSelector::Single("only".to_owned());
        let iter = selector.iter();
        assert_eq!(iter.size_hint(), (1, Some(1)));

        let selector = PinSelector::Combined(smallvec!["x".to_owned(), "y".to_owned()]);
        let mut iter = selector.iter();
        assert_eq!(iter.size_hint(), (2, Some(2)));
        iter.next();
        assert_eq!(iter.size_hint(), (2, Some(2)));
    }

    #[test]
    fn reset_restarts_iteration() {
        let selector = PinSelector::Combined(smallvec!["first".to_owned(), "second".to_owned(),]);
        let mut iter = selector.iter();
        assert_eq!(iter.next(), Some("first"));
        assert_eq!(iter.next(), Some("second"));
        assert_eq!(iter.next(), None);

        iter.reset();
        assert_eq!(iter.collect::<Vec<_>>(), vec!["first", "second"]);
    }

    #[test]
    fn nth_skips_and_returns_element() {
        let selector =
            PinSelector::Combined(smallvec!["a".to_owned(), "b".to_owned(), "c".to_owned(),]);
        let mut iter = selector.iter();
        assert_eq!(iter.nth(1), Some("b"));
        assert_eq!(iter.next(), Some("c"));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn new_builds_same_sequence_as_pin_selector_iter() {
        let selector = PinSelector::Combined(smallvec!["p0".to_owned(), "p1".to_owned()]);
        let via_method: Vec<_> = selector.iter().collect();
        let via_new: Vec<_> = PinSelectorIter::new(&selector).collect();
        assert_eq!(via_method, via_new);
    }
}
