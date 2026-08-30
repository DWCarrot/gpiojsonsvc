use crate::gpio::GPIOCluster;
use crate::gpio::GPIOClusterError;
use crate::gpio::PinLevel;
use crate::handlers::response::cluster_error_response;
use crate::protocol::request::TargetSelector;
use crate::protocol::response::PinValuePayload;
use crate::protocol::response::ResponseMessage;
use crate::session::SessionContext;
use crate::session::TargetMode;
use crate::session::TargetsCache;
use crate::session::TargetsCacheError;

#[derive(Debug, Default)]
pub struct GetHandler;

impl GetHandler {
    pub async fn handle<C>(
        session: &SessionContext<C>,
        request_id: &str,
        target: &TargetSelector,
    ) -> ResponseMessage
    where
        C: GPIOCluster,
        C::Error: GPIOClusterError,
    {
        let initialized = match session.initialized_session_required() {
            Ok(session) => session,
            Err(error) => return error.into_response(request_id),
        };
        let cache = initialized.cache();
        let pin_reads = match collect_pin_reads(cache, target.as_slice()) {
            Ok(pin_reads) => pin_reads,
            Err(error) => return error.into_response(request_id),
        };

        let levels = match initialized
            .cluster
            .read(pin_reads.iter().map(PinReadInfo::get_index))
            .await
        {
            Ok(levels) => levels,
            Err(error) => return cluster_error_response(request_id, error),
        };

        let pin_value = match target {
            TargetSelector::Single(_) => {
                let value = fold_pin_reads(&pin_reads, levels);
                PinValuePayload::Value(value)
            }
            TargetSelector::Multiple(names) => {
                let mut values = vec![0u8; names.len()];
                for (pin, level) in pin_reads.iter().zip(levels) {
                    values[pin.target_slot] =
                        accumulate_pin_level(values[pin.target_slot], pin, level);
                }
                PinValuePayload::Values(values)
            }
        };

        ResponseMessage::pin_value(request_id, pin_value)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PinReadInfo {
    index: usize,
    bit_offset: u32,
    target_slot: usize,
}

impl PinReadInfo {
    #[inline]
    pub fn get_index(&self) -> usize {
        self.index
    }
}

fn collect_pin_reads<'a>(
    cache: &TargetsCache,
    targets: &'a [String],
) -> Result<Vec<PinReadInfo>, TargetsCacheError<'a>> {
    let mut pin_reads = Vec::new();
    for (slot, name) in targets.iter().enumerate() {
        let target_name = name.as_str();
        let target = cache.target(target_name)?;
        if target.mode != TargetMode::Input {
            return Err(TargetsCacheError::TargetNotReadable {
                target: target_name,
            });
        }
        let it = target.indices.iter();
        let len = it.len();
        assert!(
            len > 0 && len <= 8,
            "target pins count must be between 1 and 8"
        );
        let mut bit_offset = len as u32;
        for index in it {
            bit_offset -= 1;
            pin_reads.push(PinReadInfo {
                index,
                bit_offset,
                target_slot: slot,
            });
        }
    }
    Ok(pin_reads)
}

fn accumulate_pin_level(value: u8, pin: &PinReadInfo, level: PinLevel) -> u8 {
    let bit: u8 = level.into();
    value | (bit << pin.bit_offset)
}

fn fold_pin_reads(pin_reads: &[PinReadInfo], levels: impl Iterator<Item = PinLevel>) -> u8 {
    pin_reads
        .iter()
        .zip(levels)
        .fold(0u8, |value, (pin, level)| {
            accumulate_pin_level(value, pin, level)
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use smallvec::smallvec;

    use crate::gpio::GPIOBackend;
    use crate::gpio::GPIOPinConfig;
    use crate::gpio::mock::MockBackend;
    use crate::protocol::request::BiasMode;
    use crate::protocol::request::DriveMode;
    use crate::protocol::request::EdgeMode;
    use crate::protocol::request::PinSelector;
    use crate::protocol::request::TargetConfigRequest;

    use super::*;

    async fn sample_cache() -> TargetsCache {
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
        let cluster = backend
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
            .expect("cluster");

        let request = BTreeMap::from([
            (
                "IN".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Single("gpiochip0:0".to_owned()),
                    bias: None,
                },
            ),
            (
                "IN2".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Combined(smallvec![
                        "gpiochip0:1".to_owned(),
                        "gpiochip0:0".to_owned(),
                    ]),
                    bias: None,
                },
            ),
            (
                "OUT".to_owned(),
                TargetConfigRequest::Output {
                    pin: PinSelector::Single("gpiochip0:2".to_owned()),
                    drive: None,
                },
            ),
        ]);

        TargetsCache::build(&cluster, &request).expect("build cache")
    }

    #[tokio::test]
    async fn collect_pin_reads_for_single_input_target() {
        let cache = sample_cache().await;
        let targets = vec!["IN".to_owned()];

        let pin_reads = collect_pin_reads(&cache, &targets).expect("collect pin reads");

        assert_eq!(
            pin_reads,
            vec![PinReadInfo {
                index: 0,
                bit_offset: 0,
                target_slot: 0,
            }]
        );
    }

    #[tokio::test]
    async fn collect_pin_reads_for_combined_input_target() {
        let cache = sample_cache().await;
        let targets = vec!["IN2".to_owned()];

        let pin_reads = collect_pin_reads(&cache, &targets).expect("collect pin reads");

        assert_eq!(
            pin_reads,
            vec![
                PinReadInfo {
                    index: 1,
                    bit_offset: 1,
                    target_slot: 0,
                },
                PinReadInfo {
                    index: 0,
                    bit_offset: 0,
                    target_slot: 0,
                },
            ]
        );
    }

    #[tokio::test]
    async fn collect_pin_reads_assigns_target_slots() {
        let cache = sample_cache().await;
        let targets = vec!["IN".to_owned(), "IN2".to_owned()];

        let pin_reads = collect_pin_reads(&cache, &targets).expect("collect pin reads");

        assert_eq!(pin_reads.len(), 3);
        assert_eq!(pin_reads[0].target_slot, 0);
        assert_eq!(pin_reads[1].target_slot, 1);
        assert_eq!(pin_reads[2].target_slot, 1);
    }

    #[tokio::test]
    async fn collect_pin_reads_rejects_unknown_target() {
        let cache = sample_cache().await;
        let targets = vec!["MISSING".to_owned()];

        assert_eq!(
            collect_pin_reads(&cache, &targets).unwrap_err(),
            TargetsCacheError::UnknownTarget { target: "MISSING" }
        );
    }

    #[tokio::test]
    async fn collect_pin_reads_rejects_non_input_target() {
        let cache = sample_cache().await;
        let targets = vec!["OUT".to_owned()];

        assert_eq!(
            collect_pin_reads(&cache, &targets).unwrap_err(),
            TargetsCacheError::TargetNotReadable { target: "OUT" }
        );
    }

    #[test]
    fn accumulate_pin_level_uses_bit_offsets() {
        let pin_reads = [
            PinReadInfo {
                target_slot: 0,
                index: 3,
                bit_offset: 0,
            },
            PinReadInfo {
                target_slot: 0,
                index: 2,
                bit_offset: 1,
            },
        ];
        let levels = [PinLevel::Low, PinLevel::High];
        let value = fold_pin_reads(&pin_reads, levels.into_iter());
        assert_eq!(value, 0b10);
    }
}
