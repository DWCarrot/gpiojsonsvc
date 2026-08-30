use crate::gpio::EdgeEvent;
use crate::gpio::EdgeEventBuffer;
use crate::gpio::EdgeEventType;
use crate::gpio::GPIOError;

/// Queued edge event record for one mock line request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockEdgeEventRecord {
    pub event_type: EdgeEventType,
    pub timestamp_ns: u64,
    pub line_offset: u32,
    pub global_seqno: u64,
    pub line_seqno: u64,
}

/// Single edge event exposed through [`MockEdgeEventBuffer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockEdgeEvent {
    event_type: EdgeEventType,
    timestamp_ns: u64,
    line_offset: u32,
    global_seqno: u64,
    line_seqno: u64,
}

impl MockEdgeEvent {
    pub fn from_record(record: &MockEdgeEventRecord) -> Self {
        Self {
            event_type: record.event_type,
            timestamp_ns: record.timestamp_ns,
            line_offset: record.line_offset,
            global_seqno: record.global_seqno,
            line_seqno: record.line_seqno,
        }
    }
}

impl EdgeEvent for MockEdgeEvent {
    fn get_event_type(&self) -> EdgeEventType {
        self.event_type
    }

    fn get_timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }

    fn get_line_offset(&self) -> u32 {
        self.line_offset
    }

    fn get_global_seqno(&self) -> u64 {
        self.global_seqno
    }

    fn get_line_seqno(&self) -> u64 {
        self.line_seqno
    }
}

/// Userspace batch buffer for mock edge events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockEdgeEventBuffer {
    capacity: usize,
    events: Vec<MockEdgeEvent>,
}

impl MockEdgeEventBuffer {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            events: Vec::new(),
        }
    }

    pub fn push_record(&mut self, record: &MockEdgeEventRecord) -> Result<(), GPIOError> {
        if self.events.len() >= self.capacity {
            return Err(GPIOError::Other("edge event buffer is full".to_owned()));
        }
        self.events.push(MockEdgeEvent::from_record(record));
        Ok(())
    }
}

impl EdgeEventBuffer for MockEdgeEventBuffer {
    type EdgeEvent<'a>
        = MockEdgeEvent
    where
        Self: 'a;

    fn get_capacity(&self) -> usize {
        self.capacity
    }

    fn get_num_events(&self) -> usize {
        self.events.len()
    }

    fn get_event<'a>(&'a self, index: usize) -> Result<Self::EdgeEvent<'a>, GPIOError> {
        self.events.get(index).cloned().ok_or_else(|| {
            GPIOError::InvalidArgument(format!("edge event index {index} out of range"))
        })
    }

    fn clear(&mut self) {
        self.events.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpio::mock::config::default_edge_event_buffer_capacity;

    #[test]
    fn edge_event_buffer_respects_capacity() {
        let mut buffer = MockEdgeEventBuffer::with_capacity(1);
        let record = MockEdgeEventRecord {
            event_type: EdgeEventType::RisingEdge,
            timestamp_ns: 1,
            line_offset: 0,
            global_seqno: 1,
            line_seqno: 1,
        };

        buffer.push_record(&record).unwrap();
        assert!(buffer.push_record(&record).is_err());
    }

    #[test]
    fn default_buffer_capacity_matches_libgpiod_default() {
        assert_eq!(default_edge_event_buffer_capacity(0), 64);
        assert_eq!(default_edge_event_buffer_capacity(16), 16);
    }
}
