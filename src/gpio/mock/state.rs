use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::fs;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use nix::errno::Errno;
use nix::sys::eventfd::EfdFlags;
use nix::sys::eventfd::EventFd;
use nix::unistd;

use crate::gpio::EdgeEventType;
use crate::gpio::GPIOError;
use crate::gpio::LineBias;
use crate::gpio::LineConfig;
use crate::gpio::LineDirection;
use crate::gpio::LineEdge;
use crate::gpio::LineValue;
use crate::gpio::ValidLineValue;

use quick_xml::Reader;
use quick_xml::XmlVersion;

use super::config::MockLineConfig;
use super::config::MockRequestLineState;
use super::events::MockEdgeEventRecord;
use super::log::XmlWriteLog;
use super::parse_chip_xml;
use super::serialize_chip_xml;
use super::snapshot::LineLevel;
use super::snapshot::MockChipSnapshot;
use super::snapshot::diff_snapshot;
use super::snapshot::line_level_to_line_value;
use super::snapshot::line_value_to_line_level;

pub type RequestId = u64;

pub(crate) const CHIP_INFO_EVENTS_UNSUPPORTED: &str =
    "line-info events are not implemented for MockChip";
pub(crate) const WAIT_EDGE_EVENTS_UNSUPPORTED: &str =
    "wait_edge_events is not implemented for MockLineRequest; use fd readiness + read_edge_events";
pub(crate) const RECONFIGURE_LINES_UNSUPPORTED: &str =
    "reconfigure_lines is not implemented for MockLineRequest";
pub(crate) const READ_EDGE_EVENTS_EMPTY: &str =
    "no edge events are queued for this mock line request";

/// Shared runtime state for one opened mock chip file.
#[derive(Debug)]
pub struct MockChipState {
    pub xml_path: PathBuf,
    pub path_string: String,
    pub snapshot: MockChipSnapshot,
    pub requests: BTreeMap<RequestId, RequestRegistration>,
    pub next_request_id: RequestId,
    pub watcher_error: Option<String>,
    pub chip_eventfd: OwnedFd,
    pub write_log: Option<Arc<XmlWriteLog>>,
}

/// One active line request registered against a chip.
#[derive(Debug)]
pub struct RequestRegistration {
    pub offsets_in_order: Vec<u32>,
    pub line_settings: BTreeMap<u32, MockRequestLineState>,
    pub consumer: String,
    pub pending_edge_events: VecDeque<MockEdgeEventRecord>,
    pub eventfd: OwnedFd,
    pub global_seqno_cursor: u64,
    pub per_line_seqno: BTreeMap<u32, u64>,
    pub closed: bool,
}

pub type SharedChipState = Arc<Mutex<MockChipState>>;

pub fn open_chip_state(
    path: &str,
    write_log: Option<Arc<XmlWriteLog>>,
) -> Result<SharedChipState, GPIOError> {
    let xml_path = PathBuf::from(path);
    let content = fs::read_to_string(&xml_path)?;
    let snapshot = parse_chip_xml(&content)?;
    let chip_eventfd = new_eventfd()?;
    Ok(Arc::new(Mutex::new(MockChipState {
        path_string: path.to_owned(),
        xml_path,
        snapshot,
        requests: BTreeMap::new(),
        next_request_id: 1,
        watcher_error: None,
        chip_eventfd,
        write_log,
    })))
}

pub fn is_valid_chip_file(path: &str) -> bool {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| parse_chip_xml(&content).ok())
        .is_some()
}

pub fn new_eventfd() -> Result<OwnedFd, GPIOError> {
    EventFd::from_value_and_flags(0, EfdFlags::EFD_NONBLOCK | EfdFlags::EFD_CLOEXEC)
        .map(OwnedFd::from)
        .map_err(|errno| GPIOError::Io(std::io::Error::from_raw_os_error(errno as i32)))
}

pub fn persist_snapshot(state: &MockChipState) -> Result<(), GPIOError> {
    let xml = serialize_chip_xml(&state.snapshot)?;
    fs::write(&state.xml_path, xml)?;
    Ok(())
}

/// Persist chip XML after a line write, then enqueue a timestamped dump if logging is enabled.
pub fn persist_snapshot_for_line_write(state: &MockChipState) -> Result<(), GPIOError> {
    let xml = serialize_chip_xml(&state.snapshot)?;
    fs::write(&state.xml_path, &xml)?;
    if let Some(write_log) = state.write_log.as_ref() {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        write_log.enqueue(timestamp_ms, xml);
    }
    Ok(())
}

pub fn effective_direction(
    request: &MockRequestLineState,
    persisted: &super::snapshot::MockLineSnapshot,
) -> LineDirection {
    match request.direction {
        LineDirection::AsIs => persisted.direction,
        other => other,
    }
}

pub fn to_logical_value(physical: LineLevel, active_low: bool) -> LineValue {
    line_level_to_line_value(physical, active_low).into()
}

pub fn to_physical_value(logical: ValidLineValue, active_low: bool) -> LineLevel {
    line_value_to_line_level(logical, active_low)
}

pub fn offset_in_use(state: &MockChipState, offset: u32) -> bool {
    state
        .requests
        .values()
        .any(|registration| !registration.closed && registration.offsets_in_order.contains(&offset))
}

pub fn find_request_for_offset<'a>(
    state: &'a MockChipState,
    offset: u32,
) -> Option<&'a RequestRegistration> {
    state.requests.values().find(|registration| {
        !registration.closed && registration.offsets_in_order.contains(&offset)
    })
}

pub fn apply_persisted_metadata(
    line: &mut super::snapshot::MockLineSnapshot,
    request: &MockRequestLineState,
) {
    line.active_low = request.active_low;
    let direction = effective_direction(request, line);
    match direction {
        LineDirection::Input => {
            line.direction = LineDirection::Input;
            if request.bias != LineBias::AsIs {
                line.bias = request.bias;
            }
        }
        LineDirection::Output => {
            line.direction = LineDirection::Output;
            line.drive = request.drive;
        }
        LineDirection::AsIs => {}
    }
}

pub fn apply_request_output_values(
    snapshot: &mut MockChipSnapshot,
    line_cfg: &MockLineConfig,
    line_settings: &BTreeMap<u32, MockRequestLineState>,
) -> Result<bool, GPIOError> {
    let count = line_cfg.get_num_configured_offsets();
    let mut offsets = vec![0; count];
    line_cfg.get_configured_offsets(&mut offsets);

    let override_values = line_cfg.configured_output_values();
    let mut changed = false;

    for (index, offset) in offsets.iter().enumerate() {
        let request = line_settings
            .get(offset)
            .ok_or(GPIOError::UnconfiguredOffset { offset: *offset })?;
        let line = snapshot
            .lines
            .get_mut(offset)
            .ok_or(GPIOError::InvalidOffset(*offset))?;
        if effective_direction(request, line) != LineDirection::Output {
            continue;
        }

        let logical_value = if let Some(values) = &override_values {
            if index < values.len() {
                Some(values[index])
            } else {
                request.output_value
            }
        } else {
            request.output_value
        };

        if let Some(logical) = logical_value {
            let physical = to_physical_value(logical, request.active_low);
            if line.persisted_level != physical {
                line.persisted_level = physical;
                changed = true;
            }
        }
    }

    Ok(changed)
}

/// Applies external XML file content for watcher-side edge detection only.
///
/// Uses [`diff_snapshot`] against the in-memory baseline, dispatches input
/// transition events, and updates only changed input `persisted_level` values.
/// Metadata and structure from the file are not merged into the in-memory snapshot.
pub fn apply_external_file_diff(state: &mut MockChipState, content: &str) {
    let baseline = state.snapshot.clone();
    let mut reader = Reader::from_str(content);
    let changes = match diff_snapshot(&mut reader, XmlVersion::Explicit1_0, &baseline) {
        Ok(changes) => changes,
        Err(error) => {
            state.watcher_error = Some(format!("failed to diff external file: {error}"));
            return;
        }
    };

    dispatch_input_level_changes(state, &baseline, &changes);

    for (offset, new_logical) in changes {
        if let Some(line) = state.snapshot.lines.get_mut(&offset) {
            line.persisted_level = line_value_to_line_level(new_logical, line.active_low);
        }
    }
}

fn dispatch_input_level_changes(
    state: &mut MockChipState,
    baseline: &MockChipSnapshot,
    changes: &BTreeMap<u32, ValidLineValue>,
) {
    for (offset, new_level) in changes {
        let Some(baseline_line) = baseline.lines.get(offset) else {
            continue;
        };
        let previous =
            line_level_to_line_value(baseline_line.persisted_level, baseline_line.active_low);
        let current = *new_level;
        if previous == current {
            continue;
        }
        if baseline_line.direction != LineDirection::Input {
            continue;
        }
        let Some(event_type) = level_transition_event(previous, current) else {
            continue;
        };

        let request_ids: Vec<RequestId> = state
            .requests
            .iter()
            .filter(|(_, registration)| {
                !registration.closed && registration.offsets_in_order.contains(offset)
            })
            .map(|(request_id, _)| *request_id)
            .collect();

        for request_id in request_ids {
            let Some(registration) = state.requests.get_mut(&request_id) else {
                continue;
            };
            let Some(request) = registration.line_settings.get(offset) else {
                continue;
            };
            if effective_direction(request, baseline_line) != LineDirection::Input {
                continue;
            }
            if !edge_matches_detection(request.edge, event_type) {
                continue;
            }
            let _ = enqueue_edge_event(registration, event_type, *offset);
        }
    }
}

fn level_transition_event(
    previous: ValidLineValue,
    current: ValidLineValue,
) -> Option<EdgeEventType> {
    match (previous, current) {
        (ValidLineValue::Inactive, ValidLineValue::Active) => Some(EdgeEventType::RisingEdge),
        (ValidLineValue::Active, ValidLineValue::Inactive) => Some(EdgeEventType::FallingEdge),
        _ => None,
    }
}

fn edge_matches_detection(edge: LineEdge, event_type: EdgeEventType) -> bool {
    match edge {
        LineEdge::None => false,
        LineEdge::Rising => event_type == EdgeEventType::RisingEdge,
        LineEdge::Falling => event_type == EdgeEventType::FallingEdge,
        LineEdge::Both => true,
    }
}

fn current_timestamp_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

pub fn enqueue_edge_event(
    registration: &mut RequestRegistration,
    event_type: EdgeEventType,
    line_offset: u32,
) -> Result<(), GPIOError> {
    registration.global_seqno_cursor += 1;
    let global_seqno = registration.global_seqno_cursor;
    let line_seqno = {
        let entry = registration.per_line_seqno.entry(line_offset).or_insert(0);
        *entry += 1;
        *entry
    };

    registration
        .pending_edge_events
        .push_back(MockEdgeEventRecord {
            event_type,
            timestamp_ns: current_timestamp_ns(),
            line_offset,
            global_seqno,
            line_seqno,
        });
    signal_eventfd(&registration.eventfd)
}

pub fn signal_eventfd(fd: &OwnedFd) -> Result<(), GPIOError> {
    let value: u64 = 1;
    unistd::write(fd, &value.to_ne_bytes()).map_err(map_nix_io_error)?;
    Ok(())
}

pub fn drain_eventfd(fd: &OwnedFd) -> Result<(), GPIOError> {
    let mut buffer = [0u8; 8];
    match unistd::read(fd, &mut buffer) {
        Ok(_) => Ok(()),
        Err(Errno::EAGAIN) => Ok(()),
        Err(errno) => Err(map_nix_io_error(errno)),
    }
}

fn map_nix_io_error(errno: Errno) -> GPIOError {
    GPIOError::Io(std::io::Error::from_raw_os_error(errno as i32))
}

pub fn validate_write_target(
    state: &MockChipState,
    request_id: RequestId,
    offset: u32,
) -> Result<MockRequestLineState, GPIOError> {
    let registration = state
        .requests
        .get(&request_id)
        .filter(|registration| !registration.closed)
        .ok_or(GPIOError::Closed)?;
    if !registration.offsets_in_order.contains(&offset) {
        return Err(GPIOError::UnconfiguredOffset { offset });
    }
    let request = registration
        .line_settings
        .get(&offset)
        .cloned()
        .ok_or(GPIOError::UnconfiguredOffset { offset })?;
    let line = state
        .snapshot
        .lines
        .get(&offset)
        .ok_or(GPIOError::InvalidOffset(offset))?;
    if effective_direction(&request, line) != LineDirection::Output {
        return Err(GPIOError::Other(format!(
            "write to non-output line at offset {offset}"
        )));
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpio::LineBias;
    use crate::gpio::LineDirection;
    use crate::gpio::LineDrive;
    use crate::gpio::LineEdge;
    use crate::gpio::mock::config::MockRequestLineState;
    use crate::gpio::mock::snapshot::MockLineSnapshot;
    use std::collections::BTreeMap;

    fn input_snapshot(level: LineLevel) -> MockChipSnapshot {
        MockChipSnapshot {
            name: "gpiochip0".to_owned(),
            label: String::new(),
            lines: BTreeMap::from([(
                0,
                MockLineSnapshot {
                    name: String::new(),
                    consumer: String::new(),
                    persisted_level: level,
                    direction: LineDirection::Input,
                    bias: LineBias::Disabled,
                    drive: LineDrive::PushPull,
                    active_low: false,
                },
            )]),
        }
    }

    fn register_input_request(state: &mut MockChipState, edge: LineEdge) -> RequestId {
        let request_id = state.next_request_id;
        state.next_request_id += 1;
        let mut line_settings = BTreeMap::new();
        line_settings.insert(
            0,
            MockRequestLineState {
                direction: LineDirection::Input,
                edge,
                bias: LineBias::AsIs,
                drive: LineDrive::PushPull,
                active_low: false,
                debounce_period_us: 0,
                event_clock: crate::gpio::LineClock::Monotonic,
                output_value: None,
            },
        );
        state.requests.insert(
            request_id,
            RequestRegistration {
                offsets_in_order: vec![0],
                line_settings,
                consumer: String::new(),
                pending_edge_events: VecDeque::new(),
                eventfd: new_eventfd().expect("eventfd"),
                global_seqno_cursor: 0,
                per_line_seqno: BTreeMap::new(),
                closed: false,
            },
        );
        request_id
    }

    fn snapshot_to_xml(snapshot: &MockChipSnapshot) -> String {
        serialize_chip_xml(snapshot).expect("serialize")
    }

    #[test]
    fn external_file_diff_enqueues_matching_input_transition() {
        let mut state = MockChipState {
            xml_path: PathBuf::from("/tmp/unused.xml"),
            path_string: "/tmp/unused.xml".to_owned(),
            snapshot: input_snapshot(LineLevel::Low),
            requests: BTreeMap::new(),
            next_request_id: 1,
            watcher_error: None,
            chip_eventfd: new_eventfd().expect("eventfd"),
            write_log: None,
        };
        let request_id = register_input_request(&mut state, LineEdge::Both);

        let updated = input_snapshot(LineLevel::High);
        apply_external_file_diff(&mut state, &snapshot_to_xml(&updated));

        let registration = state.requests.get(&request_id).expect("request");
        assert_eq!(registration.pending_edge_events.len(), 1);
        assert_eq!(
            registration.pending_edge_events[0].event_type,
            EdgeEventType::RisingEdge
        );
        assert_eq!(registration.pending_edge_events[0].line_offset, 0);
        assert_eq!(
            state
                .snapshot
                .lines
                .get(&0)
                .expect("line 0")
                .persisted_level,
            LineLevel::High
        );
    }

    #[test]
    fn external_file_diff_respects_edge_detection_filter() {
        let mut state = MockChipState {
            xml_path: PathBuf::from("/tmp/unused.xml"),
            path_string: "/tmp/unused.xml".to_owned(),
            snapshot: input_snapshot(LineLevel::High),
            requests: BTreeMap::new(),
            next_request_id: 1,
            watcher_error: None,
            chip_eventfd: new_eventfd().expect("eventfd"),
            write_log: None,
        };
        let request_id = register_input_request(&mut state, LineEdge::Rising);

        apply_external_file_diff(
            &mut state,
            &snapshot_to_xml(&input_snapshot(LineLevel::Low)),
        );

        let registration = state.requests.get(&request_id).expect("request");
        assert!(registration.pending_edge_events.is_empty());

        apply_external_file_diff(
            &mut state,
            &snapshot_to_xml(&input_snapshot(LineLevel::High)),
        );

        let registration = state.requests.get(&request_id).expect("request");
        assert_eq!(registration.pending_edge_events.len(), 1);
        assert_eq!(
            registration.pending_edge_events[0].event_type,
            EdgeEventType::RisingEdge
        );
    }

    #[test]
    fn external_file_diff_ignores_output_line_changes() {
        let mut state = MockChipState {
            xml_path: PathBuf::from("/tmp/unused.xml"),
            path_string: "/tmp/unused.xml".to_owned(),
            snapshot: MockChipSnapshot {
                name: "gpiochip0".to_owned(),
                label: String::new(),
                lines: BTreeMap::from([(
                    1,
                    MockLineSnapshot {
                        name: String::new(),
                        consumer: String::new(),
                        persisted_level: LineLevel::Low,
                        direction: LineDirection::Output,
                        bias: LineBias::Disabled,
                        drive: LineDrive::PushPull,
                        active_low: false,
                    },
                )]),
            },
            requests: BTreeMap::new(),
            next_request_id: 1,
            watcher_error: None,
            chip_eventfd: new_eventfd().expect("eventfd"),
            write_log: None,
        };

        let request_id = state.next_request_id;
        state.next_request_id += 1;
        let mut line_settings = BTreeMap::new();
        line_settings.insert(
            1,
            MockRequestLineState {
                direction: LineDirection::Output,
                edge: LineEdge::Both,
                bias: LineBias::AsIs,
                drive: LineDrive::PushPull,
                active_low: false,
                debounce_period_us: 0,
                event_clock: crate::gpio::LineClock::Monotonic,
                output_value: None,
            },
        );
        state.requests.insert(
            request_id,
            RequestRegistration {
                offsets_in_order: vec![1],
                line_settings,
                consumer: String::new(),
                pending_edge_events: VecDeque::new(),
                eventfd: new_eventfd().expect("eventfd"),
                global_seqno_cursor: 0,
                per_line_seqno: BTreeMap::new(),
                closed: false,
            },
        );

        let mut updated = state.snapshot.clone();
        updated.lines.get_mut(&1).expect("line 1").persisted_level = LineLevel::High;
        apply_external_file_diff(&mut state, &snapshot_to_xml(&updated));

        let registration = state.requests.get(&request_id).expect("request");
        assert!(registration.pending_edge_events.is_empty());
        assert_eq!(
            state
                .snapshot
                .lines
                .get(&1)
                .expect("line 1")
                .persisted_level,
            LineLevel::Low
        );
    }

    #[test]
    fn external_file_diff_ignores_metadata_only_changes() {
        let mut state = MockChipState {
            xml_path: PathBuf::from("/tmp/unused.xml"),
            path_string: "/tmp/unused.xml".to_owned(),
            snapshot: input_snapshot(LineLevel::Low),
            requests: BTreeMap::new(),
            next_request_id: 1,
            watcher_error: None,
            chip_eventfd: new_eventfd().expect("eventfd"),
            write_log: None,
        };
        let _request_id = register_input_request(&mut state, LineEdge::Both);

        let mut metadata_edit = state.snapshot.clone();
        metadata_edit.lines.get_mut(&0).expect("line 0").bias = LineBias::PullDown;
        metadata_edit.lines.get_mut(&0).expect("line 0").direction = LineDirection::Output;
        apply_external_file_diff(&mut state, &snapshot_to_xml(&metadata_edit));

        assert!(state.watcher_error.is_none());
        let line = state.snapshot.lines.get(&0).expect("line 0");
        assert_eq!(line.direction, LineDirection::Input);
        assert_eq!(line.bias, LineBias::Disabled);
        assert_eq!(line.persisted_level, LineLevel::Low);
        let registration = state.requests.get(&_request_id).expect("request");
        assert!(registration.pending_edge_events.is_empty());
    }

    #[test]
    fn external_file_diff_records_watcher_error_on_unknown_line() {
        let mut state = MockChipState {
            xml_path: PathBuf::from("/tmp/unused.xml"),
            path_string: "/tmp/unused.xml".to_owned(),
            snapshot: input_snapshot(LineLevel::Low),
            requests: BTreeMap::new(),
            next_request_id: 1,
            watcher_error: None,
            chip_eventfd: new_eventfd().expect("eventfd"),
            write_log: None,
        };

        let unknown_line_xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" bias="disabled">L</line>
    <line id="99" direction="input" bias="disabled">L</line>
</gpiochip>"#;
        apply_external_file_diff(&mut state, unknown_line_xml);

        assert!(state.watcher_error.is_some());
    }
}
