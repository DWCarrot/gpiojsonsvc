use std::collections::BTreeMap;

use tokio::time::Duration;
use tokio::time::Instant;

use crate::gpio::GPIOCluster;
use crate::gpio::GPIOClusterError;
use crate::gpio::PinLevel;
use crate::handlers::response::cluster_error_response;
use crate::protocol::request::SetTargetValue;
use crate::protocol::response::ResponseMessage;
use crate::session::ResolvedPinIndices;
use crate::session::SessionContext;
use crate::session::TargetMode;
use crate::session::TargetsCache;
use crate::session::TargetsCacheError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetHandler {
    pub max_sequence_steps: usize,
    pub max_total_execution_time_ms: u32,
}

impl Default for SetHandler {
    fn default() -> Self {
        Self {
            max_sequence_steps: 32,
            max_total_execution_time_ms: 60_000,
        }
    }
}

impl SetHandler {
    pub async fn handle<C>(
        &self,
        session: &mut SessionContext<C>,
        request_id: &str,
        target: &BTreeMap<String, SetTargetValue>,
    ) -> ResponseMessage
    where
        C: GPIOCluster,
        C::Error: GPIOClusterError,
    {
        let mut set_guard = match session.begin_set() {
            Ok(guard) => guard,
            Err(error) => return error.into_response(request_id),
        };

        let initialized = match set_guard.initialized_session() {
            Ok(session) => session,
            Err(error) => return error.into_response(request_id),
        };

        let cache = initialized.cache();
        let (immediate_writes, delayed_writes) = match collect_write_plan(
            cache,
            target,
            self.max_sequence_steps,
            self.max_total_execution_time_ms,
        ) {
            Ok(plan) => plan,
            Err(error) => return error.into_response(request_id),
        };

        let schedule_base = Instant::now();

        match initialized
            .cluster
            .write(
                immediate_writes.iter().map(PinWriteInfo::get_index),
                immediate_writes.iter().map(PinWriteInfo::get_value),
            )
            .await
        {
            Ok(_) => (),
            Err(error) => return cluster_error_response(request_id, error),
        }

        if !delayed_writes.is_empty() {
            let scheduled_tasks: Vec<ScheduleWriteTask> = delayed_writes
                .into_iter()
                .map(|(lag_ms, writes)| ScheduleWriteTask {
                    time: schedule_base + Duration::from_millis(lag_ms as u64),
                    writes,
                })
                .collect();

            if let Err(error) = set_guard.enable_schedule() {
                return error.into_response(request_id);
            }

            for task in scheduled_tasks {
                if let Err(error) = set_guard.schedule_wait(task.time).await {
                    set_guard.disable_schedule();
                    return error.into_response(request_id);
                }

                if let Err(error) = initialized
                    .cluster
                    .write(
                        task.writes.iter().map(PinWriteInfo::get_index),
                        task.writes.iter().map(PinWriteInfo::get_value),
                    )
                    .await
                {
                    set_guard.disable_schedule();
                    return cluster_error_response(request_id, error);
                }
            }
        }

        set_guard.disable_schedule();
        ResponseMessage::ok(request_id)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ScheduleWriteTask {
    time: Instant,
    writes: Vec<PinWriteInfo>,
}

#[derive(Debug, PartialEq, Eq)]
struct PinWriteInfo {
    index: usize,
    bit_offset: u32,
    value: PinLevel,
}

impl PinWriteInfo {
    #[inline]
    pub fn get_index(&self) -> usize {
        self.index
    }

    #[inline]
    pub fn get_value(&self) -> PinLevel {
        self.value
    }
}

fn collect_write_plan<'a>(
    cache: &TargetsCache,
    targets: &'a BTreeMap<String, SetTargetValue>,
    max_sequence_steps: usize,
    max_total_execution_time_ms: u32,
) -> Result<(Vec<PinWriteInfo>, BTreeMap<u32, Vec<PinWriteInfo>>), ExtractWriteInfoError<'a>> {
    let mut immediate_writes = Vec::new();
    let mut delayed_writes = BTreeMap::new();

    for (name, target_value) in targets {
        let target_name = name.as_str();
        let target = cache
            .target(target_name)
            .map_err(ExtractWriteInfoError::TargetsCacheError)?;
        if target.mode != TargetMode::Output {
            return Err(ExtractWriteInfoError::TargetsCacheError(
                TargetsCacheError::TargetNotWritable {
                    target: target_name,
                },
            ));
        }
        match target_value {
            SetTargetValue::Immediate(value) => {
                extract_write_info(&target.indices, *value, &mut immediate_writes);
            }
            SetTargetValue::Sequence(steps) => {
                if steps.len() > max_sequence_steps {
                    return Err(ExtractWriteInfoError::SequenceStepLimitExceeded {
                        target: target_name,
                        steps: steps.len(),
                        limit: max_sequence_steps,
                    });
                }

                let mut lag_accumulate = 0;
                for (step_index, step) in steps.iter().enumerate() {
                    if step.lag == 0 {
                        if step_index > 0 {
                            return Err(ExtractWriteInfoError::InvalidLag {
                                target: target_name,
                                step: step_index,
                            });
                        }
                        extract_write_info(&target.indices, step.value, &mut immediate_writes);
                    } else {
                        lag_accumulate += step.lag;
                        if lag_accumulate > max_total_execution_time_ms {
                            return Err(ExtractWriteInfoError::TotalExecutionTimeLimitExceeded {
                                total_ms: lag_accumulate,
                                limit: max_total_execution_time_ms,
                            });
                        }
                        let writes = delayed_writes
                            .entry(lag_accumulate)
                            .or_insert_with(Vec::new);
                        extract_write_info(&target.indices, step.value, writes);
                    }
                }
            }
        }
    }

    Ok((immediate_writes, delayed_writes))
}

fn extract_write_info(
    target_pins: &ResolvedPinIndices,
    value: u8,
    storage: &mut Vec<PinWriteInfo>,
) {
    let iter = target_pins.iter();
    let len = iter.len();
    assert!(
        len > 0 && len <= 8,
        "target pins count must be between 1 and 8"
    );
    let mut bit_offset = len as u32;
    for pin in iter {
        bit_offset -= 1;
        let value = (value >> bit_offset) & 0b1;
        storage.push(PinWriteInfo {
            index: pin,
            bit_offset,
            value: unsafe { PinLevel::from_u8_unchecked(value) },
        });
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ExtractWriteInfoError<'a> {
    TargetsCacheError(TargetsCacheError<'a>),
    InvalidLag {
        target: &'a str,
        step: usize,
    },
    SequenceStepLimitExceeded {
        target: &'a str,
        steps: usize,
        limit: usize,
    },
    TotalExecutionTimeLimitExceeded {
        total_ms: u32,
        limit: u32,
    },
}

impl ExtractWriteInfoError<'_> {
    fn into_response(self, id: impl Into<String>) -> ResponseMessage {
        match self {
            Self::TargetsCacheError(error) => error.into_response(id),
            Self::InvalidLag { target, step } => ResponseMessage::error(
                id,
                format!("set target sequence step {step} must include lag for target `{target}`"),
            ),
            Self::SequenceStepLimitExceeded {
                target,
                steps,
                limit,
            } => ResponseMessage::error(
                id,
                format!(
                    "set target sequence for `{target}` has {steps} steps, exceeding limit of {limit}"
                ),
            ),
            Self::TotalExecutionTimeLimitExceeded { total_ms, limit } => ResponseMessage::error(
                id,
                format!("set request total execution time {total_ms}ms exceeds limit of {limit}ms"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use smallvec::smallvec;

    use crate::gpio::GPIOBackend;
    use crate::gpio::GPIOPinConfig;
    use crate::gpio::mock::MockBackend;
    use crate::protocol::request::BiasMode;
    use crate::protocol::request::DriveMode;
    use crate::protocol::request::PinSelector;
    use crate::protocol::request::SetStepRequest;
    use crate::protocol::request::TargetConfigRequest;
    use crate::protocol::response::ResponseStatus;
    use crate::session::InitializedSession;
    use crate::session::SessionContext;

    use super::*;

    const DEFAULT_MAX_SEQUENCE_STEPS: usize = 32;
    const DEFAULT_MAX_TOTAL_EXECUTION_TIME_MS: u32 = 60_000;

    fn collect_plan<'a>(
        cache: &'a TargetsCache,
        targets: &'a BTreeMap<String, SetTargetValue>,
    ) -> Result<(Vec<PinWriteInfo>, BTreeMap<u32, Vec<PinWriteInfo>>), ExtractWriteInfoError<'a>>
    {
        collect_write_plan(
            cache,
            targets,
            DEFAULT_MAX_SEQUENCE_STEPS,
            DEFAULT_MAX_TOTAL_EXECUTION_TIME_MS,
        )
    }

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
                "OUT".to_owned(),
                TargetConfigRequest::Output {
                    pin: PinSelector::Single("gpiochip0:2".to_owned()),
                    drive: None,
                },
            ),
            (
                "OUT2".to_owned(),
                TargetConfigRequest::Output {
                    pin: PinSelector::Combined(smallvec![
                        "gpiochip0:2".to_owned(),
                        "gpiochip0:3".to_owned(),
                    ]),
                    drive: None,
                },
            ),
        ]);

        TargetsCache::build(&cluster, &request).expect("build cache")
    }

    #[tokio::test]
    async fn collect_write_plan_for_immediate_value() {
        let cache = sample_cache().await;
        let targets = BTreeMap::from([("OUT".to_owned(), SetTargetValue::Immediate(1))]);

        let (immediate, delayed) = collect_plan(&cache, &targets).expect("collect write plan");

        assert!(delayed.is_empty());
        assert_eq!(
            immediate,
            vec![PinWriteInfo {
                index: 1,
                bit_offset: 0,
                value: PinLevel::High,
            }]
        );
    }

    #[tokio::test]
    async fn collect_write_plan_for_sequence_with_delayed_steps() {
        let cache = sample_cache().await;
        let targets = BTreeMap::from([(
            "OUT2".to_owned(),
            SetTargetValue::Sequence(vec![
                SetStepRequest {
                    lag: 0,
                    value: 0b10,
                },
                SetStepRequest {
                    lag: 100,
                    value: 0b01,
                },
                SetStepRequest {
                    lag: 50,
                    value: 0b11,
                },
            ]),
        )]);

        let (immediate, delayed) = collect_plan(&cache, &targets).expect("collect write plan");

        assert_eq!(
            immediate,
            vec![
                PinWriteInfo {
                    index: 1,
                    bit_offset: 1,
                    value: PinLevel::High,
                },
                PinWriteInfo {
                    index: 2,
                    bit_offset: 0,
                    value: PinLevel::Low,
                },
            ]
        );
        assert_eq!(delayed.len(), 2);
        assert_eq!(
            delayed.get(&100).expect("lag 100"),
            &vec![
                PinWriteInfo {
                    index: 1,
                    bit_offset: 1,
                    value: PinLevel::Low,
                },
                PinWriteInfo {
                    index: 2,
                    bit_offset: 0,
                    value: PinLevel::High,
                },
            ]
        );
        assert_eq!(
            delayed.get(&150).expect("lag 150"),
            &vec![
                PinWriteInfo {
                    index: 1,
                    bit_offset: 1,
                    value: PinLevel::High,
                },
                PinWriteInfo {
                    index: 2,
                    bit_offset: 0,
                    value: PinLevel::High,
                },
            ]
        );
    }

    #[tokio::test]
    async fn collect_write_plan_rejects_unknown_target() {
        let cache = sample_cache().await;
        let targets = BTreeMap::from([("MISSING".to_owned(), SetTargetValue::Immediate(0))]);

        assert_eq!(
            collect_plan(&cache, &targets).unwrap_err(),
            ExtractWriteInfoError::TargetsCacheError(TargetsCacheError::UnknownTarget {
                target: "MISSING"
            })
        );
    }

    #[tokio::test]
    async fn collect_write_plan_rejects_non_output_target() {
        let cache = sample_cache().await;
        let targets = BTreeMap::from([("IN".to_owned(), SetTargetValue::Immediate(0))]);

        assert_eq!(
            collect_plan(&cache, &targets).unwrap_err(),
            ExtractWriteInfoError::TargetsCacheError(TargetsCacheError::TargetNotWritable {
                target: "IN"
            })
        );
    }

    #[tokio::test]
    async fn collect_write_plan_rejects_sequence_step_missing_lag() {
        let cache = sample_cache().await;
        let targets = BTreeMap::from([(
            "OUT".to_owned(),
            SetTargetValue::Sequence(vec![
                SetStepRequest { lag: 0, value: 1 },
                SetStepRequest { lag: 0, value: 0 },
            ]),
        )]);

        assert_eq!(
            collect_plan(&cache, &targets).unwrap_err(),
            ExtractWriteInfoError::InvalidLag {
                target: "OUT",
                step: 1,
            }
        );
    }

    #[tokio::test]
    async fn collect_write_plan_rejects_sequence_step_limit() {
        let cache = sample_cache().await;
        let targets = BTreeMap::from([(
            "OUT".to_owned(),
            SetTargetValue::Sequence(vec![
                SetStepRequest { lag: 0, value: 1 },
                SetStepRequest { lag: 10, value: 0 },
                SetStepRequest { lag: 10, value: 1 },
            ]),
        )]);

        assert_eq!(
            collect_write_plan(&cache, &targets, 2, DEFAULT_MAX_TOTAL_EXECUTION_TIME_MS)
                .unwrap_err(),
            ExtractWriteInfoError::SequenceStepLimitExceeded {
                target: "OUT",
                steps: 3,
                limit: 2,
            }
        );
    }

    fn build_scheduled_tasks(
        delayed_writes: BTreeMap<u32, Vec<PinWriteInfo>>,
        schedule_base: Instant,
    ) -> Vec<ScheduleWriteTask> {
        delayed_writes
            .into_iter()
            .map(|(lag_ms, writes)| ScheduleWriteTask {
                time: schedule_base + Duration::from_millis(lag_ms as u64),
                writes,
            })
            .collect()
    }

    #[test]
    fn build_scheduled_tasks_from_delayed_writes_only() {
        let schedule_base = Instant::now();
        let mut delayed = BTreeMap::new();
        delayed.insert(
            100,
            vec![PinWriteInfo {
                index: 1,
                bit_offset: 0,
                value: PinLevel::Low,
            }],
        );
        delayed.insert(
            150,
            vec![PinWriteInfo {
                index: 1,
                bit_offset: 0,
                value: PinLevel::High,
            }],
        );

        let tasks = build_scheduled_tasks(delayed, schedule_base);

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].time, schedule_base + Duration::from_millis(100));
        assert_eq!(tasks[0].writes[0].value, PinLevel::Low);
        assert_eq!(tasks[1].time, schedule_base + Duration::from_millis(150));
        assert_eq!(tasks[1].writes[0].value, PinLevel::High);
    }

    async fn sample_session() -> (
        tempfile::TempDir,
        SessionContext<crate::gpio::mock::MockCluster>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gpiochips.xml");
        std::fs::write(
            &path,
            r#"
<gpiochips>
    <gpiochip id="gpiochip0">
        <line id="2" mode="output" drive="push_pull">0</line>
    </gpiochip>
</gpiochips>
"#,
        )
        .expect("write state");

        let mut backend = MockBackend::new(&path);
        let cluster = backend
            .cluster(
                [(
                    "gpiochip0:2",
                    GPIOPinConfig::Output {
                        drive: DriveMode::PushPull,
                    },
                )]
                .into_iter(),
            )
            .await
            .expect("cluster");

        let request = BTreeMap::from([(
            "OUT".to_owned(),
            TargetConfigRequest::Output {
                pin: PinSelector::Single("gpiochip0:2".to_owned()),
                drive: None,
            },
        )]);
        let cache = TargetsCache::build(&cluster, &request).expect("build cache");

        let mut session = SessionContext::new();
        session.set_initialized(Arc::new(InitializedSession {
            init_request_id: "init-1".to_owned(),
            cluster,
            cache,
        }));

        (dir, session)
    }

    #[tokio::test]
    async fn handle_executes_delayed_sequence_inline() {
        let (_dir, mut session) = sample_session().await;
        let targets = BTreeMap::from([(
            "OUT".to_owned(),
            SetTargetValue::Sequence(vec![
                SetStepRequest { lag: 0, value: 1 },
                SetStepRequest { lag: 20, value: 0 },
            ]),
        )]);

        let started = Instant::now();
        let response = SetHandler::default()
            .handle(&mut session, "set-1", &targets)
            .await;
        assert!(matches!(response.status, ResponseStatus::Ok));
        assert!(started.elapsed() >= Duration::from_millis(15));
    }

    #[tokio::test]
    async fn collect_write_plan_rejects_total_execution_time_limit() {
        let cache = sample_cache().await;
        let targets = BTreeMap::from([(
            "OUT".to_owned(),
            SetTargetValue::Sequence(vec![
                SetStepRequest { lag: 0, value: 1 },
                SetStepRequest { lag: 100, value: 0 },
                SetStepRequest { lag: 100, value: 1 },
            ]),
        )]);

        assert_eq!(
            collect_write_plan(&cache, &targets, DEFAULT_MAX_SEQUENCE_STEPS, 150).unwrap_err(),
            ExtractWriteInfoError::TotalExecutionTimeLimitExceeded {
                total_ms: 200,
                limit: 150,
            }
        );
    }
}
