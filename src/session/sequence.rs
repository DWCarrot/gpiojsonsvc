//! Multi-step `set` sequence compilation and reactor-owned pending state.

use std::time::Duration;

use tokio::time::Instant;

use crate::gpio::LineValue;
use crate::protocol::request::SetStepRequest;

use super::batch::CombinedOffsets;
use super::compiled::CompiledTargets;
use super::execute::compile_set_batch;
use super::state::SessionError;

/// One precompiled step of a pending multi-step `set` sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingCompiledStep {
    accumulated_lag: Duration,
    batch: CombinedOffsets<LineValue>,
}

pub struct PendingCompiledSteps(Vec<PendingCompiledStep>);

/// Reactor-owned execution plan for one in-progress multi-step `set`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSetSequence {
    token: u64,
    request_id: String,
    sequence_started_at: Instant,
    steps: Vec<PendingCompiledStep>,
    next_step_index: usize,
}

impl PendingSetSequence {
    /// Compiles every protocol step into a write batch before any GPIO I/O.
    pub fn compile_steps<'a>(
        compiled: &'a CompiledTargets,
        steps: &'a [SetStepRequest],
        chip_count: usize,
    ) -> Result<PendingCompiledSteps, SessionError<'a>> {
        if steps.is_empty() {
            return Err(SessionError::Other(
                "set target steps must contain at least one step".to_owned(),
            ));
        }

        let mut compiled_steps = Vec::with_capacity(steps.len());
        let mut accumulated_lag = Duration::from_millis(0);
        for step in steps {
            let writes = step
                .target
                .iter()
                .map(|(name, value)| (name.as_str(), *value));
            let batch = compile_set_batch(compiled, writes, chip_count)?;
            accumulated_lag += Duration::from_millis(step.lag as u64);
            compiled_steps.push(PendingCompiledStep {
                accumulated_lag,
                batch: batch.clone(),
            });
        }
        Ok(PendingCompiledSteps(compiled_steps))
    }

    /// Builds pending state from precompiled batches and captures schedule start.
    ///
    /// Callers must apply [`first_batch`](Self::first_batch) immediately after this
    /// returns. The cursor already points at step `1`.
    pub fn start(
        token: u64,
        request_id: String,
        steps: PendingCompiledSteps,
        sequence_started_at: Instant,
    ) -> Self {
        Self {
            token,
            request_id,
            sequence_started_at,
            steps: steps.0,
            next_step_index: 1,
        }
    }

    pub fn token(&self) -> u64 {
        self.token
    }

    pub fn matches_token(&self, token: u64) -> bool {
        self.token == token
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Batch for step `0`, which the caller applies before installing the sequence.
    pub fn first_batch(&self) -> &CombinedOffsets<LineValue> {
        &self.steps[0].batch
    }

    /// Batch for the next unapplied step, if any remain.
    pub fn batch(&self) -> Option<&CombinedOffsets<LineValue>> {
        self.steps.get(self.next_step_index).map(|step| &step.batch)
    }

    /// Absolute deadline for the next pending step, if any remain.
    pub fn deadline(&self) -> Option<Instant> {
        self.steps
            .get(self.next_step_index)
            .map(|step| self.sequence_started_at + step.accumulated_lag)
    }

    /// Advance after a successful apply of [`next_batch`](Self::next_batch).
    ///
    /// Returns `true` if more steps remain, `false` if the sequence is complete.
    pub fn advance(&mut self) -> bool {
        if self.is_complete() {
            return false;
        }
        self.next_step_index += 1;
        !self.is_complete()
    }

    pub fn is_complete(&self) -> bool {
        self.next_step_index >= self.steps.len()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use tokio::time::Instant;

    use crate::gpio::LineValue;
    use crate::protocol::request::SetStepRequest;
    use crate::session::CompiledTarget;
    use crate::session::CompiledTargets;
    use crate::session::ResolvedPin;
    use crate::session::ResolvedPins;
    use crate::session::TargetMode;

    use super::PendingSetSequence;

    fn output_targets() -> CompiledTargets {
        let mut by_name = BTreeMap::new();
        by_name.insert(
            "OUT".to_owned(),
            CompiledTarget {
                pins: ResolvedPins::Single(ResolvedPin {
                    chip_index: 0,
                    offset: 2,
                }),
                mode: TargetMode::Output,
            },
        );
        CompiledTargets {
            by_name,
            trigger_by_pin: BTreeMap::new(),
        }
    }

    #[test]
    fn compile_steps_builds_per_step_batches() {
        let compiled = output_targets();
        let steps = vec![
            SetStepRequest {
                lag: 0,
                target: BTreeMap::from([("OUT".to_owned(), 1)]),
            },
            SetStepRequest {
                lag: 100,
                target: BTreeMap::from([("OUT".to_owned(), 0)]),
            },
        ];

        let compiled_steps =
            PendingSetSequence::compile_steps(&compiled, &steps, 1).expect("compile");
        let compiled_steps = compiled_steps.0;
        assert_eq!(compiled_steps.len(), 2);
        assert_eq!(compiled_steps[0].accumulated_lag, Duration::from_millis(0));
        assert_eq!(
            compiled_steps[1].accumulated_lag,
            Duration::from_millis(100)
        );
        assert_eq!(
            compiled_steps[0].batch.attachments(0).unwrap(),
            &[LineValue::Active]
        );
        assert_eq!(
            compiled_steps[1].batch.attachments(0).unwrap(),
            &[LineValue::Inactive]
        );
    }

    #[test]
    fn start_computes_cumulative_deadlines_from_relative_lags() {
        let compiled = output_targets();
        let steps = vec![
            SetStepRequest {
                lag: 0,
                target: BTreeMap::from([("OUT".to_owned(), 1)]),
            },
            SetStepRequest {
                lag: 100,
                target: BTreeMap::from([("OUT".to_owned(), 0)]),
            },
            SetStepRequest {
                lag: 300,
                target: BTreeMap::from([("OUT".to_owned(), 1)]),
            },
        ];
        let compiled_steps =
            PendingSetSequence::compile_steps(&compiled, &steps, 1).expect("compile");
        let started = Instant::now();
        let mut pending = PendingSetSequence::start(7, "set-1".to_owned(), compiled_steps, started);

        assert!(pending.matches_token(7));
        assert_eq!(
            pending.first_batch().attachments(0).unwrap(),
            &[LineValue::Active]
        );
        assert_eq!(
            pending.deadline(),
            Some(started + Duration::from_millis(100))
        );
        assert!(!pending.is_complete());

        assert_eq!(
            pending.batch().unwrap().attachments(0).unwrap(),
            &[LineValue::Inactive]
        );
        assert!(pending.advance());
        assert_eq!(
            pending.deadline(),
            Some(started + Duration::from_millis(400))
        );
        assert_eq!(
            pending.batch().unwrap().attachments(0).unwrap(),
            &[LineValue::Active]
        );
        assert!(!pending.advance());
        assert!(pending.is_complete());
        assert!(pending.deadline().is_none());
        assert!(pending.batch().is_none());
    }

    #[test]
    fn single_step_sequence_is_complete_after_start() {
        let compiled = output_targets();
        let steps = vec![SetStepRequest {
            lag: 0,
            target: BTreeMap::from([("OUT".to_owned(), 1)]),
        }];
        let compiled_steps =
            PendingSetSequence::compile_steps(&compiled, &steps, 1).expect("compile");
        let mut pending =
            PendingSetSequence::start(1, "set-1".to_owned(), compiled_steps, Instant::now());
        assert!(pending.is_complete());
        assert!(pending.deadline().is_none());
        assert!(!pending.advance());
    }
}
