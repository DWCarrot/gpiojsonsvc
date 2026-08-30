use std::os::fd::AsRawFd;
use std::os::fd::RawFd;
use std::sync::Arc;

use crate::gpio::Backend;
use crate::gpio::Chip;
use crate::gpio::GPIOError;
use crate::gpio::LineConfig;
use crate::gpio::RequestConfig;
use crate::gpio::WaitStatus;

use super::MockBackend;
use super::config::MockLineConfig;
use super::config::MockLineSettings;
use super::config::MockRequestConfig;
use super::config::MockRequestLineState;
use super::info::MockChipInfo;
use super::info::MockInfoEvent;
use super::info::MockLineInfo;
use super::request::MockLineRequest;
use super::state::SharedChipState;
use super::state::apply_persisted_metadata;
use super::state::apply_request_output_values;
use super::state::offset_in_use;
use super::state::persist_snapshot;
use super::watcher::ChipWatcher;

/// One XML-backed GPIO chip opened through [`MockBackend::open_chip`].
#[derive(Debug, Clone)]
pub struct MockChip {
    state: SharedChipState,
    watcher: Arc<ChipWatcher>,
    path: String,
}

impl MockChip {
    pub(crate) fn new(state: SharedChipState, watcher: Arc<ChipWatcher>) -> Self {
        let path = state
            .lock()
            .map(|locked| locked.path_string.clone())
            .unwrap_or_default();
        Self {
            state,
            watcher,
            path,
        }
    }

    pub(crate) fn wait_for_watcher_ready(&self, timeout: std::time::Duration) -> bool {
        self.watcher.wait_until_ready(timeout)
    }

    #[cfg(test)]
    pub(crate) fn apply_external_file_diff_for_test(&self, content: &str) -> Result<(), GPIOError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| GPIOError::Other("mock chip state lock poisoned".to_owned()))?;
        super::state::apply_external_file_diff(&mut state, content);
        Ok(())
    }

    fn with_state<T>(
        &self,
        f: impl FnOnce(&super::state::MockChipState) -> Result<T, GPIOError>,
    ) -> Result<T, GPIOError> {
        let state = self
            .state
            .lock()
            .map_err(|_| GPIOError::Other("mock chip state lock poisoned".to_owned()))?;
        f(&state)
    }

    fn build_line_info(
        state: &super::state::MockChipState,
        offset: u32,
    ) -> Result<MockLineInfo, GPIOError> {
        let line = state
            .snapshot
            .lines
            .get(&offset)
            .ok_or(GPIOError::InvalidOffset(offset))?;
        let mut info = MockLineInfo::from_persisted_snapshot(offset, line);
        if let Some(registration) = super::state::find_request_for_offset(state, offset) {
            if let Some(request) = registration.line_settings.get(&offset) {
                info = info.with_request_state(&registration.consumer, request, line);
            }
        }
        Ok(info)
    }
}

impl AsRawFd for MockChip {
    fn as_raw_fd(&self) -> RawFd {
        self.with_state(|state| Ok(state.chip_eventfd.as_raw_fd()))
            .unwrap_or(-1)
    }
}

impl Chip for MockChip {
    type ChipInfoOwned = MockChipInfo;
    type LineInfoOwned = MockLineInfo;
    type InfoEventOwned = MockInfoEvent;
    type LineRequestOwned = MockLineRequest;
    type LineSettings = MockLineSettings;
    type LineConfig = MockLineConfig;
    type RequestConfig = MockRequestConfig;
    type EdgeEventBuffer = super::events::MockEdgeEventBuffer;

    fn get_info(&self) -> Result<Self::ChipInfoOwned, GPIOError> {
        self.with_state(|state| Ok(MockChipInfo::from_snapshot(&state.snapshot)))
    }

    fn get_path(&self) -> &str {
        &self.path
    }

    fn get_line_info(&self, offset: u32) -> Result<Self::LineInfoOwned, GPIOError> {
        self.with_state(|state| Self::build_line_info(state, offset))
    }

    fn watch_line_info(&self, _offset: u32) -> Result<Self::LineInfoOwned, GPIOError> {
        unimplemented!("mock backend line-info watch APIs are not implemented")
    }

    fn unwatch_line_info(&self, _offset: u32) -> Result<(), GPIOError> {
        unimplemented!("mock backend line-info watch APIs are not implemented")
    }

    fn wait_info_event(&self, _timeout_ns: Option<u64>) -> Result<WaitStatus, GPIOError> {
        unimplemented!("mock backend line-info watch APIs are not implemented")
    }

    fn read_info_event(&self) -> Result<Self::InfoEventOwned, GPIOError> {
        unimplemented!("mock backend line-info watch APIs are not implemented")
    }

    fn get_line_offset_from_name(&self, name: &str) -> Result<u32, GPIOError> {
        self.with_state(|state| {
            for (offset, line) in &state.snapshot.lines {
                if line.name == name {
                    return Ok(*offset);
                }
            }
            Err(GPIOError::LineNameNotFound(name.to_owned()))
        })
    }

    fn request_lines(
        &self,
        req_cfg: Option<&Self::RequestConfig>,
        line_cfg: &Self::LineConfig,
    ) -> Result<Self::LineRequestOwned, GPIOError> {
        if line_cfg.get_num_configured_offsets() == 0 {
            return Err(GPIOError::EmptyLineConfig);
        }

        let consumer = req_cfg
            .map(RequestConfig::get_consumer)
            .unwrap_or("")
            .to_owned();

        let mut offsets = vec![0; line_cfg.get_num_configured_offsets()];
        let count = line_cfg.get_configured_offsets(&mut offsets);
        offsets.truncate(count);

        let mut line_settings = std::collections::BTreeMap::new();
        for &offset in &offsets {
            let settings = line_cfg.get_line_settings(offset)?;
            line_settings.insert(offset, MockRequestLineState::from_settings(&settings));
        }

        let mut state = self
            .state
            .lock()
            .map_err(|_| GPIOError::Other("mock chip state lock poisoned".to_owned()))?;

        for &offset in &offsets {
            if !state.snapshot.lines.contains_key(&offset) {
                return Err(GPIOError::InvalidOffset(offset));
            }
            if offset_in_use(&state, offset) {
                return Err(GPIOError::Other(format!(
                    "line offset {offset} is already requested"
                )));
            }
        }

        for (&offset, request) in &line_settings {
            let line = state.snapshot.lines.get_mut(&offset).unwrap();
            apply_persisted_metadata(line, request);
        }

        apply_request_output_values(&mut state.snapshot, line_cfg, &line_settings)?;
        for &offset in &offsets {
            if let Some(line) = state.snapshot.lines.get_mut(&offset) {
                line.consumer = consumer.clone();
            }
        }
        persist_snapshot(&state)?;

        let request_id = state.next_request_id;
        state.next_request_id += 1;
        let eventfd = super::state::new_eventfd()?;
        let chip_name = state.snapshot.name.clone();

        state.requests.insert(
            request_id,
            super::state::RequestRegistration {
                offsets_in_order: offsets.clone(),
                line_settings,
                consumer,
                pending_edge_events: std::collections::VecDeque::new(),
                eventfd,
                global_seqno_cursor: 0,
                per_line_seqno: std::collections::BTreeMap::new(),
                closed: false,
            },
        );

        Ok(MockLineRequest::new(
            self.state.clone(),
            request_id,
            chip_name,
            offsets,
        ))
    }
}

impl Backend for MockBackend {
    type Chip = MockChip;
    type LineSettings = MockLineSettings;
    type LineConfig = MockLineConfig;
    type RequestConfig = MockRequestConfig;
    type EdgeEventBuffer = super::events::MockEdgeEventBuffer;

    fn open_chip(&self, path: &str) -> Result<Self::Chip, GPIOError> {
        let state = super::state::open_chip_state(path, self.write_log())?;
        let watcher = Arc::new(ChipWatcher::new(state.clone(), self.poll_interval()));
        watcher.start_if_needed();
        Ok(MockChip::new(state, watcher))
    }

    fn new_line_settings(&self) -> Result<Self::LineSettings, GPIOError> {
        Ok(MockLineSettings::new())
    }

    fn new_line_config(&self) -> Result<Self::LineConfig, GPIOError> {
        Ok(MockLineConfig::new())
    }

    fn new_request_config(&self) -> Result<Self::RequestConfig, GPIOError> {
        Ok(MockRequestConfig::new())
    }

    fn new_edge_event_buffer(&self, capacity: usize) -> Result<Self::EdgeEventBuffer, GPIOError> {
        Ok(super::events::MockEdgeEventBuffer::with_capacity(
            super::config::default_edge_event_buffer_capacity(capacity),
        ))
    }

    fn is_gpiochip_device(&self, path: &str) -> bool {
        super::state::is_valid_chip_file(path)
    }

    fn api_version(&self) -> &'static str {
        super::MOCK_API_VERSION
    }
}
