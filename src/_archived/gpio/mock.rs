use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::thread;

use inotify::Inotify;
use inotify::WatchMask;
use quick_xml::de::from_str;
use quick_xml::se::to_string;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use tokio::sync::Notify;

use crate::protocol::request::EdgeMode;
use crate::protocol::response::EventType;

use super::GPIOBackend;
use super::GPIOCluster;
use super::GPIOClusterError;
use super::GPIOPinConfig;
use super::GPIOProperties;
use super::PinLevel;
use super::PropertyBiasMode;
use super::PropertyDriveMode;
use super::PropertyEventMode;
use super::pinspec::PinSpec;

#[derive(Debug, Error)]
pub enum MockError {
    #[error("cluster closed")]
    Closed,
    #[error("invalid pin index")]
    InvalidIndex,
    #[error("unmatched value count")]
    UnmatchedValue,
    #[error("line not found: {chip}:line_{line}")]
    LineNotFound { chip: String, line: u32 },
    #[error("direction mismatch for {chip}:line_{line}: expected {expected}, found {found}")]
    DirectionMismatch {
        chip: String,
        line: u32,
        expected: &'static str,
        found: &'static str,
    },
    #[error("trigger not enabled for {chip}:line_{line}")]
    TriggerNotSupported { chip: String, line: u32 },
    #[error("invalid line value `{value}` at {chip}:line_{line}")]
    InvalidValue {
        chip: String,
        line: u32,
        value: String,
    },
    #[error("write to input line: {chip}:line_{line}")]
    WriteToInput { chip: String, line: u32 },
    #[error("invalid pin name `{pin}`")]
    InvalidPinName {
        pin: String,
        reason: super::pinspec::PinSpecParseError,
    },
    #[error("parse error: {0}")]
    Parse(String),
    #[error("watcher failed: {0}")]
    WatcherFailed(String),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

impl GPIOClusterError for MockError {
    fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }

    fn is_invalid_index(&self) -> bool {
        matches!(self, Self::InvalidIndex)
    }

    fn is_unmatched_value(&self) -> bool {
        matches!(self, Self::UnmatchedValue)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "gpiochips", deny_unknown_fields)]
struct XmlDocument {
    #[serde(rename = "gpiochip", default)]
    chips: Vec<XmlChip>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct XmlChip {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "line", default)]
    lines: Vec<XmlLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct XmlLine {
    #[serde(rename = "@id")]
    id: u32,
    #[serde(rename = "@mode")]
    mode: XmlLineMode,
    #[serde(rename = "@bias", default, skip_serializing_if = "Option::is_none")]
    bias: Option<XmlBiasMode>,
    #[serde(rename = "@drive", default, skip_serializing_if = "Option::is_none")]
    drive: Option<XmlDriveMode>,
    #[serde(rename = "@event", default, skip_serializing_if = "Option::is_none")]
    event: Option<XmlEventMode>,
    #[serde(rename = "$text")]
    level: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum XmlLineMode {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum XmlBiasMode {
    Disabled,
    PullUp,
    PullDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum XmlDriveMode {
    PushPull,
    OpenDrain,
    OpenSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum XmlEventMode {
    Rising,
    Falling,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct BackendSnapshot {
    chips: BTreeMap<String, ChipSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ChipSnapshot {
    lines: BTreeMap<u32, LineSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LineSnapshot {
    level: PinLevel,
    mode: LineMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LineMode {
    Input {
        bias: PropertyBiasMode,
        event: PropertyEventMode,
    },
    Output {
        drive: PropertyDriveMode,
    },
}

impl BackendSnapshot {
    fn line(&self, chip: &str, line: u32) -> Option<&LineSnapshot> {
        self.chips
            .get(chip)
            .and_then(|lines| lines.lines.get(&line))
    }

    fn load(path: &Path) -> Result<Self, MockError> {
        parse_xml_snapshot(&fs::read_to_string(path)?)
    }

    fn save(&self, path: &Path) -> Result<(), MockError> {
        fs::write(path, serialize_xml_snapshot(self)?)?;
        Ok(())
    }
}

impl LineSnapshot {
    fn properties(&self) -> GPIOProperties {
        match self.mode {
            LineMode::Input { bias, event } => GPIOProperties::input(bias, event),
            LineMode::Output { drive } => GPIOProperties::output(drive),
        }
    }

    fn validate_config(&self, spec: &PinSpec, config: &GPIOPinConfig) -> Result<(), MockError> {
        match (config, &self.mode) {
            (GPIOPinConfig::Output { .. }, LineMode::Output { .. }) => Ok(()),
            (GPIOPinConfig::Output { .. }, LineMode::Input { .. }) => {
                Err(MockError::DirectionMismatch {
                    chip: spec.chip_name.clone(),
                    line: spec.line_offset,
                    expected: "output",
                    found: "input",
                })
            }
            (GPIOPinConfig::Input { .. }, LineMode::Input { .. }) => Ok(()),
            (GPIOPinConfig::Trigger { .. }, LineMode::Input { event, .. })
                if *event != PropertyEventMode::None =>
            {
                Ok(())
            }
            (GPIOPinConfig::Trigger { .. }, LineMode::Input { .. }) => {
                Err(MockError::TriggerNotSupported {
                    chip: spec.chip_name.clone(),
                    line: spec.line_offset,
                })
            }
            (
                GPIOPinConfig::Input { .. } | GPIOPinConfig::Trigger { .. },
                LineMode::Output { .. },
            ) => Err(MockError::DirectionMismatch {
                chip: spec.chip_name.clone(),
                line: spec.line_offset,
                expected: "input",
                found: "output",
            }),
        }
    }

    fn level_transition(&self, next: &Self) -> Option<EventType> {
        match (self.level, next.level) {
            (PinLevel::Low, PinLevel::High) => Some(EventType::Rising),
            (PinLevel::High, PinLevel::Low) => Some(EventType::Falling),
            _ => None,
        }
    }

    fn event_capability(&self) -> PropertyEventMode {
        match self.mode {
            LineMode::Input { event, .. } => event,
            LineMode::Output { .. } => PropertyEventMode::None,
        }
    }

    fn structure_kind(&self) -> &'static str {
        match self.mode {
            LineMode::Input { .. } => "input",
            LineMode::Output { .. } => "output",
        }
    }
}

fn parse_xml_snapshot(content: &str) -> Result<BackendSnapshot, MockError> {
    let document: XmlDocument =
        from_str(content).map_err(|err| MockError::Parse(format!("xml parse error: {err}")))?;
    let mut snapshot = BackendSnapshot::default();

    for chip in document.chips {
        if chip.id.trim().is_empty() {
            return Err(MockError::Parse("gpiochip id must not be empty".to_owned()));
        }
        let mut lines = BTreeMap::new();
        for line in chip.lines {
            let level = match line.level.trim() {
                "0" => PinLevel::Low,
                "1" => PinLevel::High,
                other => {
                    return Err(MockError::InvalidValue {
                        chip: chip.id.clone(),
                        line: line.id,
                        value: other.to_owned(),
                    });
                }
            };

            let line_snapshot = match line.mode {
                XmlLineMode::Input => {
                    if line.drive.is_some() {
                        return Err(MockError::Parse(format!(
                            "input line {}:{} must not define drive",
                            chip.id, line.id
                        )));
                    }
                    LineSnapshot {
                        level,
                        mode: LineMode::Input {
                            bias: xml_bias_to_property(line.bias.unwrap_or(XmlBiasMode::Disabled)),
                            event: xml_event_to_property(line.event),
                        },
                    }
                }
                XmlLineMode::Output => {
                    if line.bias.is_some() || line.event.is_some() {
                        return Err(MockError::Parse(format!(
                            "output line {}:{} must not define bias or event",
                            chip.id, line.id
                        )));
                    }
                    LineSnapshot {
                        level,
                        mode: LineMode::Output {
                            drive: xml_drive_to_property(
                                line.drive.unwrap_or(XmlDriveMode::PushPull),
                            ),
                        },
                    }
                }
            };

            if lines.insert(line.id, line_snapshot).is_some() {
                return Err(MockError::Parse(format!(
                    "duplicate line id {} in chip {}",
                    line.id, chip.id
                )));
            }
        }

        if snapshot
            .chips
            .insert(chip.id.clone(), ChipSnapshot { lines })
            .is_some()
        {
            return Err(MockError::Parse(format!(
                "duplicate gpiochip id {}",
                chip.id
            )));
        }
    }

    Ok(snapshot)
}

fn serialize_xml_snapshot(snapshot: &BackendSnapshot) -> Result<String, MockError> {
    let document = XmlDocument {
        chips: snapshot
            .chips
            .iter()
            .map(|(chip_id, chip)| XmlChip {
                id: chip_id.clone(),
                lines: chip
                    .lines
                    .iter()
                    .map(|(line_id, line)| match line.mode {
                        LineMode::Input { bias, event } => XmlLine {
                            id: *line_id,
                            mode: XmlLineMode::Input,
                            bias: Some(property_bias_to_xml(bias)),
                            drive: None,
                            event: property_event_to_xml(event),
                            level: format_pin_level(line.level),
                        },
                        LineMode::Output { drive } => XmlLine {
                            id: *line_id,
                            mode: XmlLineMode::Output,
                            bias: None,
                            drive: Some(property_drive_to_xml(drive)),
                            event: None,
                            level: format_pin_level(line.level),
                        },
                    })
                    .collect(),
            })
            .collect(),
    };

    to_string(&document).map_err(|err| MockError::Parse(format!("xml serialize error: {err}")))
}

fn format_pin_level(level: PinLevel) -> String {
    match level {
        PinLevel::Low => "0".to_owned(),
        PinLevel::High => "1".to_owned(),
    }
}

fn xml_bias_to_property(mode: XmlBiasMode) -> PropertyBiasMode {
    match mode {
        XmlBiasMode::Disabled => PropertyBiasMode::Disabled,
        XmlBiasMode::PullUp => PropertyBiasMode::PullUp,
        XmlBiasMode::PullDown => PropertyBiasMode::PullDown,
    }
}

fn xml_drive_to_property(mode: XmlDriveMode) -> PropertyDriveMode {
    match mode {
        XmlDriveMode::PushPull => PropertyDriveMode::PushPull,
        XmlDriveMode::OpenDrain => PropertyDriveMode::OpenDrain,
        XmlDriveMode::OpenSource => PropertyDriveMode::OpenSource,
    }
}

fn xml_event_to_property(mode: Option<XmlEventMode>) -> PropertyEventMode {
    match mode {
        None => PropertyEventMode::None,
        Some(XmlEventMode::Rising) => PropertyEventMode::Rising,
        Some(XmlEventMode::Falling) => PropertyEventMode::Falling,
        Some(XmlEventMode::Both) => PropertyEventMode::Both,
    }
}

fn property_bias_to_xml(mode: PropertyBiasMode) -> XmlBiasMode {
    match mode {
        PropertyBiasMode::Disabled => XmlBiasMode::Disabled,
        PropertyBiasMode::PullUp => XmlBiasMode::PullUp,
        PropertyBiasMode::PullDown => XmlBiasMode::PullDown,
    }
}

fn property_drive_to_xml(mode: PropertyDriveMode) -> XmlDriveMode {
    match mode {
        PropertyDriveMode::PushPull => XmlDriveMode::PushPull,
        PropertyDriveMode::OpenDrain => XmlDriveMode::OpenDrain,
        PropertyDriveMode::OpenSource => XmlDriveMode::OpenSource,
    }
}

fn property_event_to_xml(mode: PropertyEventMode) -> Option<XmlEventMode> {
    match mode {
        PropertyEventMode::None => None,
        PropertyEventMode::Rising => Some(XmlEventMode::Rising),
        PropertyEventMode::Falling => Some(XmlEventMode::Falling),
        PropertyEventMode::Both => Some(XmlEventMode::Both),
    }
}

fn edge_mode_matches(edge: EdgeMode, event: EventType) -> bool {
    match edge {
        EdgeMode::Rising => event == EventType::Rising,
        EdgeMode::Falling => event == EventType::Falling,
        EdgeMode::Both => true,
    }
}

fn property_event_matches(mode: PropertyEventMode, event: EventType) -> bool {
    match mode {
        PropertyEventMode::None => false,
        PropertyEventMode::Rising => event == EventType::Rising,
        PropertyEventMode::Falling => event == EventType::Falling,
        PropertyEventMode::Both => true,
    }
}

fn ensure_structure_compatible(
    previous: &BackendSnapshot,
    current: &BackendSnapshot,
) -> Result<(), MockError> {
    if previous.chips.len() != current.chips.len() {
        return Err(MockError::WatcherFailed(
            "gpiochip structure changed".to_owned(),
        ));
    }

    for (chip_name, previous_chip) in &previous.chips {
        let current_chip = current.chips.get(chip_name).ok_or_else(|| {
            MockError::WatcherFailed(format!("chip removed or renamed: {chip_name}"))
        })?;

        if previous_chip.lines.len() != current_chip.lines.len() {
            return Err(MockError::WatcherFailed(format!(
                "line structure changed for chip {chip_name}"
            )));
        }

        for (line_id, previous_line) in &previous_chip.lines {
            let current_line = current_chip.lines.get(line_id).ok_or_else(|| {
                MockError::WatcherFailed(format!("line removed or renamed: {chip_name}:{line_id}"))
            })?;

            if previous_line.structure_kind() != current_line.structure_kind() {
                return Err(MockError::WatcherFailed(format!(
                    "line mode changed for {chip_name}:{line_id}"
                )));
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct ClusterPin {
    name: String,
    spec: PinSpec,
    config: GPIOPinConfig,
    props: GPIOProperties,
}

struct ClusterState {
    id: u64,
    pins: Vec<ClusterPin>,
    pending_events: Mutex<VecDeque<(usize, EventType)>>,
    notify: Notify,
    closed: AtomicBool,
}

impl ClusterState {
    fn new(id: u64, pins: Vec<ClusterPin>) -> Self {
        Self {
            id,
            pins,
            pending_events: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            closed: AtomicBool::new(false),
        }
    }

    fn push_event(&self, event: (usize, EventType)) -> Result<(), MockError> {
        let mut pending = self
            .pending_events
            .lock()
            .map_err(|_| MockError::Parse("pending event queue poisoned".to_owned()))?;
        pending.push_back(event);
        self.notify.notify_waiters();
        Ok(())
    }

    fn pop_event(&self) -> Result<Option<(usize, EventType)>, MockError> {
        let mut pending = self
            .pending_events
            .lock()
            .map_err(|_| MockError::Parse("pending event queue poisoned".to_owned()))?;
        Ok(pending.pop_front())
    }
}

struct SharedState {
    snapshot: Option<BackendSnapshot>,
    clusters: BTreeMap<u64, Arc<ClusterState>>,
    error: Option<String>,
}

struct BackendShared {
    path: PathBuf,
    watch_dir: PathBuf,
    file_name: OsString,
    state: Mutex<SharedState>,
    next_cluster_id: AtomicU64,
    watcher_started: AtomicBool,
}

impl BackendShared {
    fn new(path: PathBuf) -> Self {
        let watch_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let file_name = path
            .file_name()
            .unwrap_or_else(|| OsStr::new("gpiochips.xml"))
            .to_os_string();
        Self {
            path,
            watch_dir,
            file_name,
            state: Mutex::new(SharedState {
                snapshot: None,
                clusters: BTreeMap::new(),
                error: None,
            }),
            next_cluster_id: AtomicU64::new(1),
            watcher_started: AtomicBool::new(false),
        }
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, SharedState>, MockError> {
        self.state
            .lock()
            .map_err(|_| MockError::Parse("backend state lock poisoned".to_owned()))
    }

    fn clone_error(&self) -> Result<Option<MockError>, MockError> {
        let state = self.lock_state()?;
        Ok(state
            .error
            .as_ref()
            .map(|message| MockError::WatcherFailed(message.clone())))
    }

    fn ensure_snapshot_loaded(&self, state: &mut SharedState) -> Result<(), MockError> {
        if let Some(message) = &state.error {
            return Err(MockError::WatcherFailed(message.clone()));
        }
        if state.snapshot.is_none() {
            state.snapshot = Some(BackendSnapshot::load(&self.path)?);
        }
        Ok(())
    }

    fn start_watcher_if_needed(self: &Arc<Self>) {
        if self
            .watcher_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let shared = Arc::clone(self);
        if let Err(error) = thread::Builder::new()
            .name("mock-gpio-inotify".to_owned())
            .spawn(move || shared.watch_loop())
        {
            self.set_error(format!("failed to spawn watcher thread: {error}"));
        }
    }

    fn set_error(&self, message: String) {
        let clusters = match self.lock_state() {
            Ok(mut state) => {
                if state.error.is_none() {
                    state.error = Some(message);
                }
                state.clusters.values().cloned().collect::<Vec<_>>()
            }
            Err(_) => return,
        };

        for cluster in clusters {
            cluster.notify.notify_waiters();
        }
    }

    fn watch_loop(self: Arc<Self>) {
        let mut inotify = match Inotify::init() {
            Ok(inotify) => inotify,
            Err(error) => {
                self.set_error(format!("failed to initialize inotify: {error}"));
                return;
            }
        };

        if let Err(error) = inotify.watches().add(
            &self.watch_dir,
            WatchMask::MODIFY
                | WatchMask::CLOSE_WRITE
                | WatchMask::CREATE
                | WatchMask::DELETE
                | WatchMask::MOVED_TO
                | WatchMask::ATTRIB,
        ) {
            self.set_error(format!(
                "failed to watch {}: {error}",
                self.watch_dir.display()
            ));
            return;
        }

        let mut buffer = [0u8; 4096];
        loop {
            let mut should_reload = false;
            let events = match inotify.read_events_blocking(&mut buffer) {
                Ok(events) => events,
                Err(error) => {
                    self.set_error(format!("inotify read failed: {error}"));
                    return;
                }
            };

            for event in events {
                if event
                    .name
                    .is_some_and(|name| name == self.file_name.as_os_str())
                {
                    should_reload = true;
                }
            }

            if should_reload {
                self.reload_from_disk();
            }
        }
    }

    fn reload_from_disk(&self) {
        let next_snapshot = match BackendSnapshot::load(&self.path) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.set_error(format!("failed to reload {}: {error}", self.path.display()));
                return;
            }
        };

        let (previous_snapshot, clusters) = match self.lock_state() {
            Ok(mut state) => {
                if state.error.is_some() {
                    return;
                }

                if let Some(previous) = &state.snapshot {
                    if let Err(error) = ensure_structure_compatible(previous, &next_snapshot) {
                        state.error = Some(error.to_string());
                        let clusters = state.clusters.values().cloned().collect::<Vec<_>>();
                        drop(state);
                        for cluster in clusters {
                            cluster.notify.notify_waiters();
                        }
                        return;
                    }

                    let previous_snapshot = previous.clone();
                    let clusters = state.clusters.values().cloned().collect::<Vec<_>>();
                    state.snapshot = Some(next_snapshot.clone());
                    (previous_snapshot, clusters)
                } else {
                    state.snapshot = Some(next_snapshot);
                    return;
                }
            }
            Err(error) => {
                self.set_error(error.to_string());
                return;
            }
        };

        for cluster in clusters {
            let _ = dispatch_cluster_events(&cluster, &previous_snapshot, &next_snapshot);
        }
    }

    fn register_cluster(&self, cluster: Arc<ClusterState>) -> Result<(), MockError> {
        let mut state = self.lock_state()?;
        if let Some(message) = &state.error {
            return Err(MockError::WatcherFailed(message.clone()));
        }
        state.clusters.insert(cluster.id, cluster);
        Ok(())
    }

    fn unregister_cluster(&self, cluster_id: u64) {
        if let Ok(mut state) = self.lock_state() {
            state.clusters.remove(&cluster_id);
        }
    }
}

fn dispatch_cluster_events(
    cluster: &ClusterState,
    previous: &BackendSnapshot,
    current: &BackendSnapshot,
) -> Result<(), MockError> {
    if cluster.closed.load(Ordering::Acquire) {
        return Ok(());
    }

    for (index, pin) in cluster.pins.iter().enumerate() {
        let GPIOPinConfig::Trigger { edge } = pin.config else {
            continue;
        };

        let previous_line = previous
            .line(&pin.spec.chip_name, pin.spec.line_offset)
            .ok_or_else(|| MockError::LineNotFound {
                chip: pin.spec.chip_name.clone(),
                line: pin.spec.line_offset,
            })?;
        let current_line = current
            .line(&pin.spec.chip_name, pin.spec.line_offset)
            .ok_or_else(|| MockError::LineNotFound {
                chip: pin.spec.chip_name.clone(),
                line: pin.spec.line_offset,
            })?;

        let Some(event) = previous_line.level_transition(current_line) else {
            continue;
        };

        if !property_event_matches(previous_line.event_capability(), event) {
            continue;
        }
        if !edge_mode_matches(edge, event) {
            continue;
        }

        cluster.push_event((index, event))?;
    }

    Ok(())
}

pub struct MockBackend {
    shared: Arc<BackendShared>,
}

impl MockBackend {
    pub fn new(state_path: impl Into<PathBuf>) -> Self {
        Self {
            shared: Arc::new(BackendShared::new(state_path.into())),
        }
    }
}

impl GPIOBackend for MockBackend {
    type Error = MockError;
    type Cluster = MockCluster;

    async fn cluster(
        &self,
        configs: impl Iterator<Item = (&str, GPIOPinConfig)>,
    ) -> Result<Self::Cluster, Self::Error> {
        self.shared.start_watcher_if_needed();

        let configs: Vec<_> = configs.collect();
        let snapshot = {
            let mut state = self.shared.lock_state()?;
            self.shared.ensure_snapshot_loaded(&mut state)?;
            state
                .snapshot
                .clone()
                .ok_or_else(|| MockError::Parse("snapshot missing after load".to_owned()))?
        };

        let mut pins = Vec::with_capacity(configs.len());
        for (pin_name, config) in configs {
            let spec = PinSpec::from_str(pin_name).map_err(|reason| MockError::InvalidPinName {
                pin: pin_name.to_owned(),
                reason,
            })?;
            let line = snapshot
                .line(&spec.chip_name, spec.line_offset)
                .ok_or_else(|| MockError::LineNotFound {
                    chip: spec.chip_name.clone(),
                    line: spec.line_offset,
                })?;

            line.validate_config(&spec, &config)?;
            pins.push(ClusterPin {
                name: pin_name.to_owned(),
                spec,
                props: line.properties(),
                config,
            });
        }

        let cluster_state = Arc::new(ClusterState::new(
            self.shared.next_cluster_id.fetch_add(1, Ordering::Relaxed),
            pins,
        ));
        self.shared.register_cluster(Arc::clone(&cluster_state))?;

        Ok(MockCluster {
            shared: Arc::clone(&self.shared),
            state: cluster_state,
        })
    }
}

pub struct MockCluster {
    shared: Arc<BackendShared>,
    state: Arc<ClusterState>,
}

impl MockCluster {
    fn ensure_open(&self) -> Result<(), MockError> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(MockError::Closed);
        }
        if let Some(error) = self.shared.clone_error()? {
            return Err(error);
        }
        Ok(())
    }

    fn validate_index(&self, index: usize) -> Result<(), MockError> {
        if index >= self.state.pins.len() {
            return Err(MockError::InvalidIndex);
        }
        Ok(())
    }

    fn mark_closed(&self) {
        if self.state.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.shared.unregister_cluster(self.state.id);
        self.state.notify.notify_waiters();
    }
}

impl GPIOCluster for MockCluster {
    type Error = MockError;

    fn pin_count(&self) -> usize {
        self.state.pins.len()
    }

    unsafe fn pin_name_unchecked(&self, index: usize) -> &str {
        unsafe { &self.state.pins.get_unchecked(index).name }
    }

    fn pin_name(&self, index: usize) -> Option<&str> {
        self.state.pins.get(index).map(|pin| pin.name.as_str())
    }

    unsafe fn pin_props_unchecked(&self, index: usize) -> GPIOProperties {
        unsafe { self.state.pins.get_unchecked(index).props.clone() }
    }

    fn pin_props(&self, index: usize) -> Option<GPIOProperties> {
        self.state.pins.get(index).map(|pin| pin.props.clone())
    }

    async fn read(
        &self,
        indices: impl Iterator<Item = usize>,
    ) -> Result<impl Iterator<Item = PinLevel>, Self::Error> {
        self.ensure_open()?;
        let indices: Vec<_> = indices.collect();
        for &index in &indices {
            self.validate_index(index)?;
        }

        let mut state = self.shared.lock_state()?;
        self.shared.ensure_snapshot_loaded(&mut state)?;
        let snapshot = state
            .snapshot
            .as_ref()
            .ok_or_else(|| MockError::Parse("snapshot missing after load".to_owned()))?;

        let mut levels = Vec::with_capacity(indices.len());
        for index in indices {
            let pin = &self.state.pins[index];
            let line = snapshot
                .line(&pin.spec.chip_name, pin.spec.line_offset)
                .ok_or_else(|| MockError::LineNotFound {
                    chip: pin.spec.chip_name.clone(),
                    line: pin.spec.line_offset,
                })?;
            levels.push(line.level);
        }

        Ok(levels.into_iter())
    }

    async fn write(
        &self,
        indices: impl Iterator<Item = usize>,
        values: impl Iterator<Item = PinLevel>,
    ) -> Result<(), Self::Error> {
        self.ensure_open()?;
        let indices: Vec<_> = indices.collect();
        let values: Vec<_> = values.collect();
        if indices.len() != values.len() {
            return Err(MockError::UnmatchedValue);
        }
        for &index in &indices {
            self.validate_index(index)?;
        }

        let mut state = self.shared.lock_state()?;
        self.shared.ensure_snapshot_loaded(&mut state)?;
        let mut next_snapshot = state
            .snapshot
            .clone()
            .ok_or_else(|| MockError::Parse("snapshot missing after load".to_owned()))?;

        for (&index, &value) in indices.iter().zip(values.iter()) {
            let pin = &self.state.pins[index];
            let line = next_snapshot
                .chips
                .get_mut(&pin.spec.chip_name)
                .and_then(|chip| chip.lines.get_mut(&pin.spec.line_offset))
                .ok_or_else(|| MockError::LineNotFound {
                    chip: pin.spec.chip_name.clone(),
                    line: pin.spec.line_offset,
                })?;

            match line.mode {
                LineMode::Output { .. } => {
                    line.level = value;
                }
                LineMode::Input { .. } => {
                    return Err(MockError::WriteToInput {
                        chip: pin.spec.chip_name.clone(),
                        line: pin.spec.line_offset,
                    });
                }
            }
        }

        next_snapshot.save(&self.shared.path)?;
        state.snapshot = Some(next_snapshot);
        Ok(())
    }

    async fn wait(&self) -> Result<(usize, EventType), Self::Error> {
        self.ensure_open()?;

        loop {
            if let Some(event) = self.state.pop_event()? {
                return Ok(event);
            }
            if self.state.closed.load(Ordering::Acquire) {
                return Err(MockError::Closed);
            }
            if let Some(error) = self.shared.clone_error()? {
                return Err(error);
            }
            self.state.notify.notified().await;
        }
    }

    async fn close(&self) {
        self.mark_closed();
    }
}

impl Drop for MockCluster {
    fn drop(&mut self) {
        self.mark_closed();
    }
}

impl fmt::Debug for MockBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockBackend")
            .field("state_path", &self.shared.path)
            .finish()
    }
}

impl fmt::Debug for MockCluster {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockCluster")
            .field("pin_count", &self.state.pins.len())
            .field("closed", &self.state.closed.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::protocol::request::BiasMode;
    use crate::protocol::request::DriveMode;
    use crate::protocol::request::EdgeMode;

    const SAMPLE_XML: &str = r#"
<gpiochips>
    <gpiochip id="gpiochip0">
        <line id="0" mode="input" bias="pull_up" event="both">0</line>
        <line id="1" mode="output" drive="push_pull">1</line>
    </gpiochip>
</gpiochips>
"#;

    fn temp_state(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gpiochips.xml");
        fs::write(&path, content).expect("write state");
        (dir, path)
    }

    async fn settle_watcher() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[test]
    fn parses_valid_xml_snapshot() {
        let snapshot = parse_xml_snapshot(SAMPLE_XML).expect("parse");
        let line0 = snapshot.line("gpiochip0", 0).expect("line 0");
        assert_eq!(line0.level, PinLevel::Low);
        assert!(matches!(
            line0.mode,
            LineMode::Input {
                bias: PropertyBiasMode::PullUp,
                event: PropertyEventMode::Both
            }
        ));

        let line1 = snapshot.line("gpiochip0", 1).expect("line 1");
        assert_eq!(line1.level, PinLevel::High);
        assert!(matches!(
            line1.mode,
            LineMode::Output {
                drive: PropertyDriveMode::PushPull
            }
        ));
    }

    #[test]
    fn rejects_invalid_line_values() {
        let error = parse_xml_snapshot(
            r#"<gpiochips><gpiochip id="gpiochip0"><line id="0" mode="input">2</line></gpiochip></gpiochips>"#,
        )
        .expect_err("invalid value");
        assert!(matches!(error, MockError::InvalidValue { .. }));
    }

    #[test]
    fn round_trips_serialized_xml() {
        let original = parse_xml_snapshot(SAMPLE_XML).expect("parse");
        let restored = parse_xml_snapshot(&serialize_xml_snapshot(&original).expect("serialize"))
            .expect("reparse");
        assert_eq!(original, restored);
    }

    #[tokio::test]
    async fn cluster_rejects_missing_line() {
        let (_dir, path) = temp_state(SAMPLE_XML);
        let mut backend = MockBackend::new(&path);
        let error = backend
            .cluster(std::iter::once((
                "gpiochip0:9",
                GPIOPinConfig::Input {
                    bias: BiasMode::AsIs,
                },
            )))
            .await
            .expect_err("missing line");
        assert!(matches!(error, MockError::LineNotFound { .. }));
    }

    #[tokio::test]
    async fn cluster_rejects_direction_mismatch() {
        let (_dir, path) = temp_state(SAMPLE_XML);
        let mut backend = MockBackend::new(&path);

        let input_on_output = backend
            .cluster(std::iter::once((
                "gpiochip0:1",
                GPIOPinConfig::Input {
                    bias: BiasMode::AsIs,
                },
            )))
            .await
            .expect_err("input on output");
        assert!(matches!(
            input_on_output,
            MockError::DirectionMismatch { .. }
        ));

        let output_on_input = backend
            .cluster(std::iter::once((
                "gpiochip0:0",
                GPIOPinConfig::Output {
                    drive: DriveMode::PushPull,
                },
            )))
            .await
            .expect_err("output on input");
        assert!(matches!(
            output_on_input,
            MockError::DirectionMismatch { .. }
        ));
    }

    #[tokio::test]
    async fn cluster_rejects_trigger_on_non_event_line() {
        let (_dir, path) = temp_state(
            r#"<gpiochips><gpiochip id="gpiochip0"><line id="0" mode="input">0</line></gpiochip></gpiochips>"#,
        );
        let mut backend = MockBackend::new(&path);
        let error = backend
            .cluster(std::iter::once((
                "gpiochip0:0",
                GPIOPinConfig::Trigger {
                    edge: EdgeMode::Rising,
                },
            )))
            .await
            .expect_err("trigger without event");
        assert!(matches!(error, MockError::TriggerNotSupported { .. }));
    }

    #[tokio::test]
    async fn read_returns_levels_in_requested_order() {
        let (_dir, path) = temp_state(
            r#"
<gpiochips>
    <gpiochip id="gpiochip0">
        <line id="0" mode="input">0</line>
        <line id="1" mode="input">1</line>
        <line id="2" mode="output">1</line>
    </gpiochip>
</gpiochips>
"#,
        );
        let mut backend = MockBackend::new(&path);
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
                ]
                .into_iter(),
            )
            .await
            .expect("cluster");

        let levels: Vec<_> = cluster
            .read([1, 0].into_iter())
            .await
            .expect("read")
            .collect();
        assert_eq!(levels, vec![PinLevel::High, PinLevel::Low]);
    }

    #[tokio::test]
    async fn write_updates_output_lines_only() {
        let (_dir, path) = temp_state(SAMPLE_XML);
        let mut backend = MockBackend::new(&path);
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
                        GPIOPinConfig::Output {
                            drive: DriveMode::PushPull,
                        },
                    ),
                ]
                .into_iter(),
            )
            .await
            .expect("cluster");

        cluster
            .write([1].into_iter(), [PinLevel::Low].into_iter())
            .await
            .expect("write output");

        let snapshot = BackendSnapshot::load(&path).expect("reload");
        assert_eq!(
            snapshot.line("gpiochip0", 1).expect("line 1").level,
            PinLevel::Low
        );

        let write_input = cluster
            .write([0].into_iter(), [PinLevel::High].into_iter())
            .await
            .expect_err("write input");
        assert!(matches!(write_input, MockError::WriteToInput { .. }));
    }

    #[tokio::test]
    async fn write_rejects_unmatched_values() {
        let (_dir, path) = temp_state(SAMPLE_XML);
        let mut backend = MockBackend::new(&path);
        let cluster = backend
            .cluster(std::iter::once((
                "gpiochip0:1",
                GPIOPinConfig::Output {
                    drive: DriveMode::PushPull,
                },
            )))
            .await
            .expect("cluster");

        let error = cluster
            .write([0, 0].into_iter(), [PinLevel::High].into_iter())
            .await
            .expect_err("unmatched");
        assert!(error.is_unmatched_value());
    }

    #[tokio::test]
    async fn watcher_detects_rising_and_falling_edges() {
        let (_dir, path) = temp_state(SAMPLE_XML);
        let mut backend = MockBackend::new(&path);
        let cluster = Arc::new(
            backend
                .cluster(std::iter::once((
                    "gpiochip0:0",
                    GPIOPinConfig::Trigger {
                        edge: EdgeMode::Both,
                    },
                )))
                .await
                .expect("cluster"),
        );

        settle_watcher().await;
        let waiter = {
            let cluster = Arc::clone(&cluster);
            tokio::spawn(async move { cluster.wait().await.expect("rising event") })
        };
        fs::write(
            &path,
            r#"<gpiochips><gpiochip id="gpiochip0"><line id="0" mode="input" bias="pull_up" event="both">1</line><line id="1" mode="output" drive="push_pull">1</line></gpiochip></gpiochips>"#,
        )
        .expect("set high");

        let (index, event) = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("rising timeout")
            .expect("join");
        assert_eq!(index, 0);
        assert_eq!(event, EventType::Rising);

        let waiter = {
            let cluster = Arc::clone(&cluster);
            tokio::spawn(async move { cluster.wait().await.expect("falling event") })
        };
        fs::write(
            &path,
            r#"<gpiochips><gpiochip id="gpiochip0"><line id="0" mode="input" bias="pull_up" event="both">0</line><line id="1" mode="output" drive="push_pull">1</line></gpiochip></gpiochips>"#,
        )
        .expect("set low");

        let (index, event) = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("falling timeout")
            .expect("join");
        assert_eq!(index, 0);
        assert_eq!(event, EventType::Falling);
    }

    #[tokio::test]
    async fn watcher_ignores_property_only_changes() {
        let (_dir, path) = temp_state(SAMPLE_XML);
        let mut backend = MockBackend::new(&path);
        let cluster = backend
            .cluster(std::iter::once((
                "gpiochip0:0",
                GPIOPinConfig::Trigger {
                    edge: EdgeMode::Both,
                },
            )))
            .await
            .expect("cluster");

        settle_watcher().await;
        fs::write(
            &path,
            r#"<gpiochips><gpiochip id="gpiochip0"><line id="0" mode="input" bias="pull_down" event="both">0</line><line id="1" mode="output" drive="push_pull">1</line></gpiochip></gpiochips>"#,
        )
        .expect("change property");

        let result = tokio::time::timeout(Duration::from_millis(200), cluster.wait()).await;
        assert!(
            result.is_err(),
            "property-only change should not emit an event"
        );
    }

    #[tokio::test]
    async fn watcher_rejects_structural_changes() {
        let (_dir, path) = temp_state(SAMPLE_XML);
        let mut backend = MockBackend::new(&path);
        let cluster = Arc::new(
            backend
                .cluster(std::iter::once((
                    "gpiochip0:0",
                    GPIOPinConfig::Trigger {
                        edge: EdgeMode::Both,
                    },
                )))
                .await
                .expect("cluster"),
        );

        settle_watcher().await;
        let waiter = {
            let cluster = Arc::clone(&cluster);
            tokio::spawn(async move { cluster.wait().await.expect_err("watcher should fail") })
        };

        fs::write(
            &path,
            r#"<gpiochips><gpiochip id="gpiochip0"><line id="4" mode="input" bias="pull_up" event="both">0</line><line id="1" mode="output" drive="push_pull">1</line></gpiochip></gpiochips>"#,
        )
        .expect("structural change");

        let error = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("error timeout")
            .expect("join");
        assert!(matches!(error, MockError::WatcherFailed(_)));
    }

    #[tokio::test]
    async fn watcher_fans_out_events_to_multiple_clusters() {
        let (_dir, path) = temp_state(SAMPLE_XML);
        let mut backend = MockBackend::new(&path);
        let cluster_a = Arc::new(
            backend
                .cluster(std::iter::once((
                    "gpiochip0:0",
                    GPIOPinConfig::Trigger {
                        edge: EdgeMode::Both,
                    },
                )))
                .await
                .expect("cluster a"),
        );
        let cluster_b = Arc::new(
            backend
                .cluster(std::iter::once((
                    "gpiochip0:0",
                    GPIOPinConfig::Trigger {
                        edge: EdgeMode::Both,
                    },
                )))
                .await
                .expect("cluster b"),
        );

        settle_watcher().await;
        let wait_a = {
            let cluster = Arc::clone(&cluster_a);
            tokio::spawn(async move { cluster.wait().await.expect("event a") })
        };
        let wait_b = {
            let cluster = Arc::clone(&cluster_b);
            tokio::spawn(async move { cluster.wait().await.expect("event b") })
        };

        fs::write(
            &path,
            r#"<gpiochips><gpiochip id="gpiochip0"><line id="0" mode="input" bias="pull_up" event="both">1</line><line id="1" mode="output" drive="push_pull">1</line></gpiochip></gpiochips>"#,
        )
        .expect("set high");

        let event_a = tokio::time::timeout(Duration::from_secs(1), wait_a)
            .await
            .expect("cluster a timeout")
            .expect("join a");
        let event_b = tokio::time::timeout(Duration::from_secs(1), wait_b)
            .await
            .expect("cluster b timeout")
            .expect("join b");

        assert_eq!(event_a, (0, EventType::Rising));
        assert_eq!(event_b, (0, EventType::Rising));
    }

    #[tokio::test]
    async fn closed_cluster_returns_error() {
        let (_dir, path) = temp_state(SAMPLE_XML);
        let mut backend = MockBackend::new(&path);
        let cluster = backend
            .cluster(std::iter::once((
                "gpiochip0:0",
                GPIOPinConfig::Input {
                    bias: BiasMode::AsIs,
                },
            )))
            .await
            .expect("cluster");

        cluster.close().await;
        let result = cluster.read([0].into_iter()).await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("expected closed error"),
        };
        assert!(error.is_closed());
    }
}
