use std::os::fd::AsRawFd;
use std::os::fd::RawFd;

use crate::gpio::EdgeEventBuffer;
use crate::gpio::GPIOError;
use crate::gpio::LineRequest;
use crate::gpio::LineValue;
use crate::gpio::WaitStatus;

use super::config::MockLineConfig;
use super::config::MockRequestLineState;
use super::events::MockEdgeEventBuffer;
use super::state::READ_EDGE_EVENTS_EMPTY;
use super::state::RequestId;
use super::state::SharedChipState;
use super::state::drain_eventfd;
use super::state::persist_snapshot;
use super::state::persist_snapshot_for_line_write;
use super::state::to_logical_value;
use super::state::to_physical_value;
use super::state::validate_write_target;

/// Exclusive mock line request over one or more offsets from a chip.
#[derive(Debug)]
pub struct MockLineRequest {
    chip_state: SharedChipState,
    request_id: RequestId,
    chip_name: String,
    offsets: Vec<u32>,
}

impl MockLineRequest {
    pub(crate) fn new(
        chip_state: SharedChipState,
        request_id: RequestId,
        chip_name: String,
        offsets: Vec<u32>,
    ) -> Self {
        Self {
            chip_state,
            request_id,
            chip_name,
            offsets,
        }
    }

    fn with_state_mut<T>(
        &self,
        f: impl FnOnce(&mut super::state::MockChipState) -> Result<T, GPIOError>,
    ) -> Result<T, GPIOError> {
        let mut state = self
            .chip_state
            .lock()
            .map_err(|_| GPIOError::Other("mock chip state lock poisoned".to_owned()))?;
        if state
            .requests
            .get(&self.request_id)
            .is_none_or(|registration| registration.closed)
        {
            return Err(GPIOError::Closed);
        }
        f(&mut state)
    }

    fn read_logical_value(
        state: &super::state::MockChipState,
        request: &MockRequestLineState,
        offset: u32,
    ) -> Result<LineValue, GPIOError> {
        let physical = state
            .snapshot
            .lines
            .get(&offset)
            .map(|line| line.persisted_level)
            .ok_or(GPIOError::InvalidOffset(offset))?;
        Ok(to_logical_value(physical, request.active_low))
    }

    fn ensure_offsets_in_request(&self, offsets: &[u32]) -> Result<(), GPIOError> {
        for &offset in offsets {
            if !self.offsets.contains(&offset) {
                return Err(GPIOError::UnconfiguredOffset { offset });
            }
        }
        Ok(())
    }
}

impl Drop for MockLineRequest {
    fn drop(&mut self) {
        if let Ok(mut state) = self.chip_state.lock() {
            if let Some(registration) = state.requests.remove(&self.request_id) {
                for offset in registration.offsets_in_order {
                    if let Some(line) = state.snapshot.lines.get_mut(&offset) {
                        line.consumer.clear();
                    }
                }
                let _ = persist_snapshot(&state);
            }
        }
    }
}

impl AsRawFd for MockLineRequest {
    fn as_raw_fd(&self) -> RawFd {
        self.chip_state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .requests
                    .get(&self.request_id)
                    .map(|registration| registration.eventfd.as_raw_fd())
            })
            .unwrap_or(-1)
    }
}

impl LineRequest for MockLineRequest {
    type LineConfig = MockLineConfig;
    type EdgeEventBuffer = MockEdgeEventBuffer;

    fn get_chip_name(&self) -> &str {
        &self.chip_name
    }

    fn get_num_requested_lines(&self) -> usize {
        self.offsets.len()
    }

    fn get_requested_offsets(&self, out: &mut [u32]) -> usize {
        let count = out.len().min(self.offsets.len());
        out[..count].copy_from_slice(&self.offsets[..count]);
        count
    }

    fn get_value(&self, offset: u32) -> Result<LineValue, GPIOError> {
        self.with_state_mut(|state| {
            let registration = state.requests.get(&self.request_id).unwrap();
            if !registration.offsets_in_order.contains(&offset) {
                return Err(GPIOError::UnconfiguredOffset { offset });
            }
            let request = registration
                .line_settings
                .get(&offset)
                .ok_or(GPIOError::UnconfiguredOffset { offset })?;
            Self::read_logical_value(state, request, offset)
        })
    }

    fn get_values_subset(
        &self,
        offsets: &[u32],
        values: &mut [LineValue],
    ) -> Result<(), GPIOError> {
        if offsets.len() != values.len() {
            return Err(GPIOError::LengthMismatch {
                expected: offsets.len(),
                actual: values.len(),
            });
        }
        for (offset, value) in offsets.iter().zip(values.iter_mut()) {
            *value = self.get_value(*offset)?;
        }
        Ok(())
    }

    fn get_values(&self, values: &mut [LineValue]) -> Result<(), GPIOError> {
        if values.len() != self.offsets.len() {
            return Err(GPIOError::LengthMismatch {
                expected: self.offsets.len(),
                actual: values.len(),
            });
        }
        self.get_values_subset(&self.offsets, values)
    }

    fn set_value(&self, offset: u32, value: LineValue) -> Result<(), GPIOError> {
        let request = {
            let state = self
                .chip_state
                .lock()
                .map_err(|_| GPIOError::Other("mock chip state lock poisoned".to_owned()))?;
            validate_write_target(&state, self.request_id, offset)?
        };
        self.with_state_mut(|state| {
            let physical = to_physical_value(value, request.active_low);
            let line = state
                .snapshot
                .lines
                .get_mut(&offset)
                .ok_or(GPIOError::InvalidOffset(offset))?;
            line.persisted_level = physical;
            persist_snapshot_for_line_write(state)
        })
    }

    fn set_values_subset(&self, offsets: &[u32], values: &[LineValue]) -> Result<(), GPIOError> {
        if offsets.len() != values.len() {
            return Err(GPIOError::LengthMismatch {
                expected: offsets.len(),
                actual: values.len(),
            });
        }
        self.ensure_offsets_in_request(offsets)?;
        self.with_state_mut(|state| {
            let mut updates = Vec::with_capacity(offsets.len());
            for (&offset, &value) in offsets.iter().zip(values.iter()) {
                let request = validate_write_target(state, self.request_id, offset)?;
                let physical = to_physical_value(value, request.active_low);
                updates.push((offset, physical));
            }

            for (offset, physical) in updates {
                let line = state
                    .snapshot
                    .lines
                    .get_mut(&offset)
                    .ok_or(GPIOError::InvalidOffset(offset))?;
                line.persisted_level = physical;
            }

            persist_snapshot_for_line_write(state)
        })
    }

    fn set_values(&self, values: &[LineValue]) -> Result<(), GPIOError> {
        if values.len() != self.offsets.len() {
            return Err(GPIOError::LengthMismatch {
                expected: self.offsets.len(),
                actual: values.len(),
            });
        }
        self.set_values_subset(&self.offsets, values)
    }

    fn reconfigure_lines(&self, _config: &Self::LineConfig) -> Result<(), GPIOError> {
        unimplemented!("mock backend line reconfigure API is not implemented")
    }

    fn wait_edge_events(&self, _timeout_ns: Option<u64>) -> Result<WaitStatus, GPIOError> {
        unimplemented!("mock backend wait_edge_events API is not implemented")
    }

    fn read_edge_events(
        &self,
        buffer: &mut Self::EdgeEventBuffer,
        max_events: usize,
    ) -> Result<usize, GPIOError> {
        self.with_state_mut(|state| {
            let registration = state.requests.get_mut(&self.request_id).unwrap();
            if registration.pending_edge_events.is_empty() {
                return Err(GPIOError::Other(READ_EDGE_EVENTS_EMPTY.to_owned()));
            }

            buffer.clear();
            let limit = max_events.min(buffer.get_capacity());
            let mut drained = 0usize;
            while drained < limit {
                let Some(record) = registration.pending_edge_events.pop_front() else {
                    break;
                };
                buffer.push_record(&record)?;
                drained += 1;
            }

            if registration.pending_edge_events.is_empty() {
                drain_eventfd(&registration.eventfd)?;
            }

            Ok(drained)
        })
    }
}
