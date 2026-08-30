use crate::gpio::ChipInfo;
use crate::gpio::InfoEvent;
use crate::gpio::InfoEventType;
use crate::gpio::LineBias;
use crate::gpio::LineClock;
use crate::gpio::LineDirection;
use crate::gpio::LineDrive;
use crate::gpio::LineEdge;
use crate::gpio::LineInfo;

use super::config::MockRequestLineState;
use super::snapshot::MockChipSnapshot;
use super::snapshot::MockLineSnapshot;
use super::state::effective_direction;

/// Immutable chip metadata snapshot for the mock backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockChipInfo {
    name: String,
    label: String,
    num_lines: usize,
}

impl MockChipInfo {
    pub fn from_snapshot(snapshot: &MockChipSnapshot) -> Self {
        Self {
            name: snapshot.name.clone(),
            label: snapshot.label.clone(),
            num_lines: snapshot.num_lines(),
        }
    }
}

impl ChipInfo for MockChipInfo {
    fn get_name(&self) -> &str {
        &self.name
    }

    fn get_label(&self) -> &str {
        &self.label
    }

    fn get_num_lines(&self) -> usize {
        self.num_lines
    }
}

/// Immutable line metadata snapshot for the mock backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockLineInfo {
    offset: u32,
    name: Option<String>,
    used: bool,
    consumer: Option<String>,
    direction: LineDirection,
    edge: LineEdge,
    bias: LineBias,
    drive: LineDrive,
    active_low: bool,
    debounced: bool,
    debounce_period_us: u64,
    event_clock: LineClock,
}

impl MockLineInfo {
    pub fn from_persisted_snapshot(offset: u32, line: &MockLineSnapshot) -> Self {
        Self {
            offset,
            name: if line.name.is_empty() {
                None
            } else {
                Some(line.name.clone())
            },
            used: false,
            consumer: None,
            direction: line.direction,
            edge: LineEdge::None,
            bias: line.bias,
            drive: line.drive,
            active_low: line.active_low,
            debounced: false,
            debounce_period_us: 0,
            event_clock: LineClock::Monotonic,
        }
    }

    pub fn with_request_state(
        mut self,
        consumer: &str,
        request: &MockRequestLineState,
        persisted: &MockLineSnapshot,
    ) -> Self {
        self.used = true;
        self.consumer = if consumer.is_empty() {
            None
        } else {
            Some(consumer.to_owned())
        };
        self.direction = effective_direction(request, persisted);
        self.edge = request.edge;
        self.bias = match request.bias {
            LineBias::AsIs => persisted.bias,
            other => other,
        };
        self.drive = request.drive;
        self.active_low = request.active_low;
        self.debounced = request.debounce_period_us > 0;
        self.debounce_period_us = request.debounce_period_us;
        self.event_clock = request.event_clock;
        self
    }
}

/// Placeholder for [`Chip::read_info_event`]; mock chip info events are unsupported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockInfoEvent {}

impl InfoEvent for MockInfoEvent {
    type LineInfo<'a>
        = MockLineInfo
    where
        Self: 'a;

    fn get_event_type(&self) -> InfoEventType {
        unimplemented!("mock backend line-info events are not implemented")
    }

    fn get_timestamp_ns(&self) -> u64 {
        unimplemented!("mock backend line-info events are not implemented")
    }

    fn get_line_info<'a>(&'a self) -> Self::LineInfo<'a> {
        unimplemented!("mock backend line-info events are not implemented")
    }
}

impl LineInfo for MockLineInfo {
    fn get_offset(&self) -> u32 {
        self.offset
    }

    fn get_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn is_used(&self) -> bool {
        self.used
    }

    fn get_consumer(&self) -> Option<&str> {
        self.consumer.as_deref()
    }

    fn get_direction(&self) -> LineDirection {
        self.direction
    }

    fn get_edge_detection(&self) -> LineEdge {
        self.edge
    }

    fn get_bias(&self) -> LineBias {
        if self.direction == LineDirection::Input {
            self.bias
        } else {
            LineBias::Unknown
        }
    }

    fn get_drive(&self) -> LineDrive {
        self.drive
    }

    fn is_active_low(&self) -> bool {
        self.active_low
    }

    fn is_debounced(&self) -> bool {
        self.debounced
    }

    fn get_debounce_period_us(&self) -> u64 {
        self.debounce_period_us
    }

    fn get_event_clock(&self) -> LineClock {
        self.event_clock
    }
}
