use std::collections::BTreeMap;
use std::ops::Deref;
use std::slice::Iter as SliceIter;

use smallvec::SmallVec;
use thiserror::Error;

use crate::gpio::GPIOCluster;
use crate::gpio::PinLevel;
use crate::protocol::request::PinSelector;
use crate::protocol::request::TargetConfigRequest;
use crate::protocol::response::ResponseMessage;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TargetsCacheError<'a> {
    #[error("unknown target `{target}`")]
    UnknownTarget { target: &'a str },
    #[error("target `{target}` is not readable")]
    TargetNotReadable { target: &'a str },
    #[error("target `{target}` is not writable")]
    TargetNotWritable { target: &'a str },
    #[error("cluster did not expose configured pin `{pin}`")]
    ClusterPinMissing { pin: &'a str },
    #[error("target `{target}` value {value} exceeds {bits} configured bits")]
    TargetValueOutOfRange {
        target: &'a str,
        value: u8,
        bits: usize,
    },
    #[error("backend event index {index} is not mapped to a trigger target")]
    UnmappedEventIndex { index: usize },
}

impl TargetsCacheError<'_> {
    pub fn into_response(self, id: impl Into<String>) -> ResponseMessage {
        ResponseMessage::error(id, self.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    Input,
    Output,
    Trigger,
}

/// Alias retained while handlers and session code migrate to `TargetMode`.
pub type ResolvedPinMode = TargetMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedPinIndices {
    Single(usize),
    Combined(SmallVec<[usize; 8]>),
}

impl ResolvedPinIndices {
    pub fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Combined(indices) => indices.len(),
        }
    }

    pub fn iter<'a>(&'a self) -> ResolvedPinIndicesIter<'a> {
        ResolvedPinIndicesIter::new(self)
    }

    pub fn first(&self) -> Option<usize> {
        match self {
            Self::Single(index) => Some(*index),
            Self::Combined(indices) => indices.first().copied(),
        }
    }
}

pub struct ResolvedPinIndicesIter<'a> {
    fixed: [Option<&'a usize>; 2],
    fixed_len: usize,
    combined: &'a [usize],
    index: usize,
}

impl<'a> ResolvedPinIndicesIter<'a> {
    pub fn new(indices: &'a ResolvedPinIndices) -> Self {
        match indices {
            ResolvedPinIndices::Single(index) => Self {
                fixed: [Some(index), None],
                fixed_len: 1,
                combined: &[],
                index: 0,
            },
            ResolvedPinIndices::Combined(indices) => Self {
                fixed: [None, None],
                fixed_len: 0,
                combined: indices.as_slice(),
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

impl<'a> Iterator for ResolvedPinIndicesIter<'a> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if self.fixed_len > 0 {
            if self.index < self.fixed_len {
                let v = unsafe { self.fixed.get_unchecked(self.index).unwrap_unchecked() };
                self.index += 1;
                Some(*v)
            } else {
                None
            }
        } else {
            if let Some(v) = self.combined.get(self.index) {
                self.index += 1;
                Some(*v)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedTarget {
    pub mode: TargetMode,
    pub indices: ResolvedPinIndices,
}

impl CachedTarget {
    pub fn input(indices: ResolvedPinIndices) -> Self {
        Self {
            mode: TargetMode::Input,
            indices,
        }
    }

    pub fn output(indices: ResolvedPinIndices) -> Self {
        Self {
            mode: TargetMode::Output,
            indices,
        }
    }

    pub fn trigger(indices: ResolvedPinIndices) -> Self {
        Self {
            mode: TargetMode::Trigger,
            indices,
        }
    }
}

#[derive(Debug)]
pub struct TargetsCache {
    targets_by_name: BTreeMap<String, CachedTarget>,
    trigger_targets_by_index: BTreeMap<usize, String>,
}

impl TargetsCache {
    pub fn build<'a, C>(
        cluster: &C,
        request: &'a BTreeMap<String, TargetConfigRequest>,
    ) -> Result<Self, TargetsCacheError<'a>>
    where
        C: GPIOCluster,
    {
        let mut name_index_table = BTreeMap::new();
        for index in 0..cluster.pin_count() {
            let name = unsafe { cluster.pin_name_unchecked(index) };
            name_index_table.insert(name, index);
        }

        let mut targets_by_name = BTreeMap::new();
        let mut trigger_targets_by_index = BTreeMap::new();

        for (target_name, target_config) in request {
            let resolved = match target_config {
                TargetConfigRequest::Input { pin, .. } => {
                    resolve_pin_indices(&name_index_table, pin).map(CachedTarget::input)?
                }
                TargetConfigRequest::Output { pin, .. } => {
                    resolve_pin_indices(&name_index_table, pin).map(CachedTarget::output)?
                }
                TargetConfigRequest::Trigger { pin, .. } => {
                    let index = name_index_table
                        .get(pin.as_str())
                        .copied()
                        .ok_or(TargetsCacheError::ClusterPinMissing { pin })?;
                    trigger_targets_by_index.insert(index, target_name.clone());
                    CachedTarget::trigger(ResolvedPinIndices::Single(index))
                }
            };

            targets_by_name.insert(target_name.clone(), resolved);
        }

        Ok(Self {
            targets_by_name,
            trigger_targets_by_index,
        })
    }

    pub fn targets_by_name(&self) -> &BTreeMap<String, CachedTarget> {
        &self.targets_by_name
    }

    pub fn target<'a>(&self, name: &'a str) -> Result<&CachedTarget, TargetsCacheError<'a>> {
        self.targets_by_name
            .get(name)
            .ok_or(TargetsCacheError::UnknownTarget { target: name })
    }

    pub fn event_target_by_index(&self, index: usize) -> Result<&str, TargetsCacheError<'_>> {
        self.trigger_targets_by_index
            .get(&index)
            .map(String::as_str)
            .ok_or(TargetsCacheError::UnmappedEventIndex { index })
    }
}

fn resolve_pin_indices<'a>(
    name_index_table: &BTreeMap<&str, usize>,
    pin: &'a PinSelector,
) -> Result<ResolvedPinIndices, TargetsCacheError<'a>> {
    match pin {
        PinSelector::Single(pin) => {
            let index = name_index_table
                .get(pin.as_str())
                .copied()
                .ok_or(TargetsCacheError::ClusterPinMissing { pin })?;
            Ok(ResolvedPinIndices::Single(index))
        }
        PinSelector::Combined(pins) => {
            let mut indices = SmallVec::new();
            for pin in pins {
                let index = name_index_table
                    .get(pin.as_str())
                    .copied()
                    .ok_or(TargetsCacheError::ClusterPinMissing { pin })?;
                indices.push(index);
            }
            Ok(ResolvedPinIndices::Combined(indices))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::gpio::GPIOBackend;
    use crate::gpio::GPIOPinConfig;
    use crate::gpio::PinLevel;
    use crate::gpio::mock::MockBackend;
    use crate::protocol::request::BiasMode;
    use crate::protocol::request::DriveMode;
    use crate::protocol::request::EdgeMode;
    use crate::protocol::request::TargetConfigRequest;

    use super::*;

    async fn sample_cluster() -> crate::gpio::mock::MockCluster {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gpiochips.xml");
        std::fs::write(
            &path,
            r#"
<gpiochips>
    <gpiochip id="gpiochip0">
        <line id="0" mode="input" bias="pull_up">1</line>
        <line id="1" mode="input" bias="pull_down">0</line>
        <line id="2" mode="output" drive="push_pull">0</line>
        <line id="3" mode="output" drive="push_pull">1</line>
        <line id="4" mode="input" event="both">0</line>
    </gpiochip>
</gpiochips>
"#,
        )
        .expect("write state");

        let mut backend = MockBackend::new(path);
        backend
            .cluster(
                [
                    (
                        "gpiochip0:0",
                        GPIOPinConfig::Input {
                            bias: BiasMode::AsIs,
                        },
                    ),
                    (
                        "gpiochip0:1",
                        GPIOPinConfig::Input {
                            bias: BiasMode::AsIs,
                        },
                    ),
                    (
                        "gpiochip0:2",
                        GPIOPinConfig::Output {
                            drive: DriveMode::PushPull,
                        },
                    ),
                    (
                        "gpiochip0:3",
                        GPIOPinConfig::Output {
                            drive: DriveMode::PushPull,
                        },
                    ),
                    (
                        "gpiochip0:4",
                        GPIOPinConfig::Trigger {
                            edge: EdgeMode::Both,
                        },
                    ),
                ]
                .into_iter(),
            )
            .await
            .expect("cluster")
    }

    fn sample_init_request() -> BTreeMap<String, TargetConfigRequest> {
        BTreeMap::from([
            (
                "IN".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Single("gpiochip0:0".to_owned()),
                    bias: None,
                },
            ),
            (
                "OUT2".to_owned(),
                TargetConfigRequest::Output {
                    pin: PinSelector::Combined(smallvec::smallvec![
                        "gpiochip0:2".to_owned(),
                        "gpiochip0:3".to_owned(),
                    ]),
                    drive: None,
                },
            ),
            (
                "TRIG".to_owned(),
                TargetConfigRequest::Trigger {
                    pin: "gpiochip0:4".to_owned(),
                    edge: EdgeMode::Both,
                },
            ),
        ])
    }

    #[tokio::test]
    async fn build_resolves_input_output_and_trigger_targets() {
        let cluster = sample_cluster().await;
        let cache = TargetsCache::build(&cluster, &sample_init_request()).expect("build");

        let input = cache.target("IN").expect("IN");
        assert_eq!(input.mode, TargetMode::Input);
        assert_eq!(input.indices, ResolvedPinIndices::Single(0));

        let output = cache.target("OUT2").expect("OUT2");
        assert_eq!(output.mode, TargetMode::Output);
        assert_eq!(
            output.indices,
            ResolvedPinIndices::Combined(SmallVec::from_slice(&[2, 3]))
        );

        let trigger = cache.target("TRIG").expect("TRIG");
        assert_eq!(trigger.mode, TargetMode::Trigger);
        assert_eq!(cache.event_target_by_index(4), Ok("TRIG"));
    }

    #[tokio::test]
    async fn build_reports_missing_pin_by_name() {
        let cluster = sample_cluster().await;
        let request = BTreeMap::from([(
            "BAD".to_owned(),
            TargetConfigRequest::Input {
                pin: PinSelector::Single("gpiochip0:99".to_owned()),
                bias: None,
            },
        )]);

        assert_eq!(
            TargetsCache::build(&cluster, &request).unwrap_err(),
            TargetsCacheError::ClusterPinMissing {
                pin: "gpiochip0:99"
            }
        );
    }

    #[tokio::test]
    async fn event_target_by_index_reports_unmapped_index() {
        let cluster = sample_cluster().await;
        let cache = TargetsCache::build(&cluster, &sample_init_request()).expect("build");

        assert_eq!(
            cache.event_target_by_index(0),
            Err(TargetsCacheError::UnmappedEventIndex { index: 0 })
        );
    }
}

#[cfg(test)]
mod resolved_pin_indices_iter_tests {
    use super::ResolvedPinIndices;
    use super::ResolvedPinIndicesIter;
    use smallvec::smallvec;

    #[test]
    fn single_index_yields_one_element() {
        let indices = ResolvedPinIndices::Single(2);
        let values: Vec<_> = indices.iter().collect();
        assert_eq!(values, vec![2]);
        assert_eq!(indices.len(), 1);
    }

    #[test]
    fn combined_indices_yield_in_order() {
        let indices = ResolvedPinIndices::Combined(smallvec![2, 3, 5]);
        let values: Vec<_> = indices.iter().collect();
        assert_eq!(values, vec![2, 3, 5]);
        assert_eq!(indices.len(), 3);
    }

    #[test]
    fn iter_len_is_total_index_count() {
        let indices = ResolvedPinIndices::Combined(smallvec![10, 20]);
        let mut iter = indices.iter();
        assert_eq!(iter.len(), 2);
        assert_eq!(iter.next(), Some(10));
        assert_eq!(iter.len(), 2);
        assert_eq!(iter.next(), Some(20));
        assert_eq!(iter.len(), 2);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn size_hint_reports_exact_length() {
        let indices = ResolvedPinIndices::Single(0);
        let iter = indices.iter();
        assert_eq!(iter.size_hint(), (1, Some(1)));

        let indices = ResolvedPinIndices::Combined(smallvec![1, 2]);
        let mut iter = indices.iter();
        assert_eq!(iter.size_hint(), (2, Some(2)));
        iter.next();
        assert_eq!(iter.size_hint(), (2, Some(2)));
    }

    #[test]
    fn reset_restarts_iteration() {
        let indices = ResolvedPinIndices::Combined(smallvec![2, 3]);
        let mut iter = indices.iter();
        assert_eq!(iter.next(), Some(2));
        assert_eq!(iter.next(), Some(3));
        assert_eq!(iter.next(), None);

        iter.reset();
        assert_eq!(iter.collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn nth_skips_and_returns_element() {
        let indices = ResolvedPinIndices::Combined(smallvec![1, 2, 3]);
        let mut iter = indices.iter();
        assert_eq!(iter.nth(1), Some(2));
        assert_eq!(iter.next(), Some(3));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn new_builds_same_sequence_as_resolved_pin_indices_iter() {
        let indices = ResolvedPinIndices::Combined(smallvec![2, 3]);
        let via_method: Vec<_> = indices.iter().collect();
        let via_new: Vec<_> = ResolvedPinIndicesIter::new(&indices).collect();
        assert_eq!(via_method, via_new);
    }
}
