use std::collections::BTreeMap;
use std::collections::BTreeSet;

use smallvec::SmallVec;

use crate::config::GPIODPinSpec;
use crate::config::ServiceConfig;
use crate::gpio::Backend;
use crate::gpio::Chip;
use crate::gpio::GPIOError;
use crate::gpio::LineBias;
use crate::gpio::LineConfig;
use crate::gpio::LineDirection;
use crate::gpio::LineDrive;
use crate::gpio::LineEdge;
use crate::gpio::LineSettings;
use crate::protocol::request::BiasMode;
use crate::protocol::request::DriveMode;
use crate::protocol::request::EdgeMode;
use crate::protocol::request::PinSelector;
use crate::protocol::request::TargetConfigRequest;
use crate::session::ResolvedPins;

use super::compiled::ChipIndices;
use super::compiled::CompiledTarget;
use super::compiled::CompiledTargets;
use super::compiled::ResolvedPin;
use super::state::SessionError;

pub trait SessionConfig: Send + Sync {
    fn resolve_gpiod_pin(&self, pin: &str) -> Option<&GPIODPinSpec>;
}

impl SessionConfig for ServiceConfig {
    fn resolve_gpiod_pin(&self, pin: &str) -> Option<&GPIODPinSpec> {
        ServiceConfig::resolve_gpiod_pin(self, pin)
    }
}

impl SessionConfig for BTreeMap<String, GPIODPinSpec> {
    fn resolve_gpiod_pin(&self, pin: &str) -> Option<&GPIODPinSpec> {
        self.get(pin)
    }
}

/// One opened chip and its active line request for a session.
pub struct SessionChip<B: Backend> {
    pub chip_index: u32,
    pub chip_name: String,
    pub chip: B::Chip,
    pub request: <B::Chip as Chip>::LineRequestOwned,
}

/// Session state produced by a successful `init` request.
pub struct InitializedSession<B: Backend> {
    pub init_request_id: String,
    pub compiled_targets: CompiledTargets,
    pub edge_buffer: B::EdgeEventBuffer,
    pub chips: Vec<SessionChip<B>>,
}

impl<B: Backend> InitializedSession<B> {
    pub fn initialize<'a>(
        init_request_id: String,
        request: &'a BTreeMap<String, TargetConfigRequest>,
        backend: &B,
        config: &dyn SessionConfig,
    ) -> Result<Self, SessionError<'a>> {
        let mut gpiod_targets_builder = GPIODTargetsBuilder::new(backend, config)?;

        for (target_name, target_config) in request {
            match target_config {
                TargetConfigRequest::Input { pin, bias } => {
                    gpiod_targets_builder.handle_input_target(target_name, pin, bias)?;
                }
                TargetConfigRequest::Output { pin, drive } => {
                    gpiod_targets_builder.handle_output_target(target_name, pin, drive)?;
                }
                TargetConfigRequest::Trigger { pin, edge } => {
                    gpiod_targets_builder.handle_trigger_target(target_name, pin, edge)?;
                }
            }
        }

        let compiled_targets = gpiod_targets_builder.compiled_targets;
        let edge_buffer = backend.new_edge_event_buffer(16)?;
        let chips_data = gpiod_targets_builder.chip_indices.collect();

        let mut chips = Vec::new();
        for (chip_index, (device, chip_data)) in chips_data.into_iter().enumerate() {
            let Some(chip_data) = chip_data else {
                continue;
            };
            let chip = backend
                .open_chip(device)
                .map_err(|error| map_open_chip_error(device, error))?;
            let request = chip
                .request_lines(None, &chip_data.line_config)
                .map_err(|error| map_request_lines_error(device, error))?;
            chips.push(SessionChip {
                chip_index: chip_index as u32,
                chip_name: device.to_owned(),
                chip,
                request,
            });
        }

        Ok(Self {
            init_request_id,
            compiled_targets,
            edge_buffer,
            chips,
        })
    }

    pub fn chip_count(&self) -> usize {
        self.chips.len()
    }
}

struct GPIODChipData<B: Backend> {
    line_config: B::LineConfig,
}

struct GPIODTargetsBuilder<'b, B: Backend> {
    backend: &'b B,
    config: &'b dyn SessionConfig,
    chip_indices: ChipIndices<'b, GPIODChipData<B>>,
    compiled_targets: CompiledTargets,
    line_settings: B::LineSettings,
}

impl<'a, 'b, B: Backend> GPIODTargetsBuilder<'b, B> {
    pub fn new(backend: &'b B, config: &'b dyn SessionConfig) -> Result<Self, SessionError<'a>> {
        Ok(Self {
            backend,
            config,
            chip_indices: ChipIndices::<GPIODChipData<B>>::new(),
            compiled_targets: CompiledTargets {
                by_name: BTreeMap::new(),
                trigger_by_pin: BTreeMap::new(),
            },
            line_settings: backend.new_line_settings()?,
        })
    }

    pub fn handle_input_target(
        &mut self,
        target_name: &'a str,
        pin: &'a PinSelector,
        bias: &'a Option<BiasMode>,
    ) -> Result<(), SessionError<'a>> {
        let settings = &mut self.line_settings;
        settings.reset();
        settings.set_direction(LineDirection::Input)?;
        if let Some(bias) = bias {
            settings.set_bias(protocol_bias_to_line_bias(*bias))?;
        }
        let resolved_pins = handle_pin_selector(
            self.backend,
            self.config,
            &self.line_settings,
            &mut self.chip_indices,
            pin,
        )?;
        let compiled_target = CompiledTarget {
            pins: resolved_pins,
            mode: super::TargetMode::Input,
        };
        self.compiled_targets
            .by_name
            .insert(target_name.to_owned(), compiled_target);
        Ok(())
    }

    pub fn handle_output_target(
        &mut self,
        target_name: &'a str,
        pin: &'a PinSelector,
        drive: &'a Option<DriveMode>,
    ) -> Result<(), SessionError<'a>> {
        let settings = &mut self.line_settings;
        settings.reset();
        settings.set_direction(LineDirection::Output)?;
        if let Some(drive) = drive {
            settings.set_drive(protocol_drive_to_line_drive(*drive))?;
        }
        let resolved_pins = handle_pin_selector(
            self.backend,
            self.config,
            &self.line_settings,
            &mut self.chip_indices,
            pin,
        )?;
        let compiled_target = CompiledTarget {
            pins: resolved_pins,
            mode: super::TargetMode::Output,
        };
        self.compiled_targets
            .by_name
            .insert(target_name.to_owned(), compiled_target);
        Ok(())
    }

    pub fn handle_trigger_target(
        &mut self,
        target_name: &'a str,
        pin: &'a str,
        edge: &'a EdgeMode,
    ) -> Result<(), SessionError<'a>> {
        let settings = &mut self.line_settings;
        settings.reset();
        settings.set_direction(LineDirection::Input)?;
        settings.set_edge_detection(protocol_edge_to_line_edge(*edge))?;

        let resolved_pins = resolve_mapped_pin(
            self.backend,
            self.config,
            &self.line_settings,
            &mut self.chip_indices,
            pin,
        )?;
        self.compiled_targets.trigger_by_pin.insert(
            (resolved_pins.chip_index, resolved_pins.offset),
            target_name.to_owned(),
        );
        let compiled_target = CompiledTarget {
            pins: ResolvedPins::Single(resolved_pins),
            mode: super::TargetMode::Trigger,
        };
        self.compiled_targets
            .by_name
            .insert(target_name.to_owned(), compiled_target);
        Ok(())
    }
}

fn handle_pin_selector<'a, 'b, B: Backend>(
    backend: &B,
    config: &'b dyn SessionConfig,
    line_settings: &B::LineSettings,
    chip_indices: &mut ChipIndices<'b, GPIODChipData<B>>,
    pin: &'a PinSelector,
) -> Result<ResolvedPins, SessionError<'a>> {
    match pin {
        PinSelector::Single(pin_key) => {
            let resolved =
                resolve_mapped_pin(backend, config, line_settings, chip_indices, pin_key)?;
            Ok(ResolvedPins::Single(resolved))
        }
        PinSelector::Combined(pin_keys) => {
            let mut seen = BTreeSet::new();
            let mut pins: SmallVec<[ResolvedPin; 8]> = SmallVec::new();
            for pin_key in pin_keys {
                let resolved =
                    resolve_mapped_pin(backend, config, line_settings, chip_indices, pin_key)?;
                if !seen.insert((resolved.chip_index, resolved.offset)) {
                    return Err(SessionError::DuplicatePhysicalLocation { pin: pin_key });
                }
                pins.push(resolved);
            }
            Ok(ResolvedPins::Combined(pins))
        }
    }
}

fn resolve_mapped_pin<'a, 'b, B: Backend>(
    backend: &B,
    config: &'b dyn SessionConfig,
    line_settings: &B::LineSettings,
    chip_indices: &mut ChipIndices<'b, GPIODChipData<B>>,
    pin: &'a str,
) -> Result<ResolvedPin, SessionError<'a>> {
    let spec = config
        .resolve_gpiod_pin(pin)
        .ok_or(SessionError::UnmappedPin { pin })?;
    let (resolved, data) = chip_indices.resolve(spec.device.as_str(), spec.line);
    append_line_settings(backend, line_settings, &[resolved.offset], data)?;
    Ok(resolved)
}

fn map_open_chip_error<'a>(device: &str, error: GPIOError) -> SessionError<'a> {
    match error {
        GPIOError::Io(_) => SessionError::UnavailableDeviceFile {
            device: device.to_owned(),
        },
        other => SessionError::GPIO(other),
    }
}

fn map_request_lines_error<'a>(device: &str, error: GPIOError) -> SessionError<'a> {
    match error {
        GPIOError::InvalidOffset(line) => SessionError::MissingLine {
            device: device.to_owned(),
            line,
        },
        other => SessionError::GPIO(other),
    }
}

fn append_line_settings<B: Backend>(
    backend: &B,
    line_settings: &B::LineSettings,
    offsets: &[u32],
    holder: &mut Option<GPIODChipData<B>>,
) -> Result<(), GPIOError> {
    if let Some(data) = holder.as_mut() {
        LineConfig::add_line_settings(&mut data.line_config, offsets, line_settings)?;
    } else {
        let line_config: B::LineConfig = backend.new_line_config()?;
        let mut data = GPIODChipData { line_config };
        LineConfig::add_line_settings(&mut data.line_config, offsets, line_settings)?;
        *holder = Some(data);
    }
    Ok(())
}

fn protocol_bias_to_line_bias(bias: BiasMode) -> LineBias {
    match bias {
        BiasMode::AsIs => LineBias::AsIs,
        BiasMode::Disabled => LineBias::Disabled,
        BiasMode::PullUp => LineBias::PullUp,
        BiasMode::PullDown => LineBias::PullDown,
    }
}

fn protocol_drive_to_line_drive(drive: DriveMode) -> LineDrive {
    match drive {
        DriveMode::PushPull => LineDrive::PushPull,
        DriveMode::OpenDrain => LineDrive::OpenDrain,
        DriveMode::OpenSource => LineDrive::OpenSource,
    }
}

fn protocol_edge_to_line_edge(edge: EdgeMode) -> LineEdge {
    match edge {
        EdgeMode::Rising => LineEdge::Rising,
        EdgeMode::Falling => LineEdge::Falling,
        EdgeMode::Both => LineEdge::Both,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use crate::config::GPIODPinSpec;
    use crate::gpio::Chip;
    use crate::gpio::ChipInfo;
    use crate::gpio::EdgeEventBuffer;
    use crate::gpio::LineRequest;
    use crate::gpio::LineValue;
    use crate::gpio::mock::MockBackend;
    use crate::protocol::request::EdgeMode;
    use crate::protocol::request::PinSelector;
    use crate::protocol::request::TargetConfigRequest;
    use crate::session::ResolvedPin;
    use crate::session::ResolvedPins;
    use crate::session::SessionError;
    use tempfile::NamedTempFile;

    use super::InitializedSession;

    const SAMPLE_XML: &str = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">L</line>
    <line id="1" name="line1" direction="input" bias="pull_down">L</line>
    <line id="2" name="line2" direction="output" drive="push_pull">L</line>
    <line id="3" name="line3" direction="output" drive="push_pull">H</line>
    <line id="4" name="line4" direction="input">L</line>
</gpiochip>"#;

    fn write_chip_file(contents: &str) -> NamedTempFile {
        let file = NamedTempFile::new().expect("temp file");
        fs::write(file.path(), contents).expect("write chip xml");
        file
    }

    fn pin_map(path: &str, mappings: &[(&str, u32)]) -> BTreeMap<String, GPIODPinSpec> {
        mappings
            .iter()
            .map(|(pin, line)| {
                (
                    (*pin).to_owned(),
                    GPIODPinSpec {
                        device: path.to_owned(),
                        line: *line,
                    },
                )
            })
            .collect()
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

    #[test]
    fn initialize_opens_chip_and_requests_configured_lines() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(
            path,
            &[
                ("gpiochip0:0", 0),
                ("gpiochip0:2", 2),
                ("gpiochip0:3", 3),
                ("gpiochip0:4", 4),
            ],
        );

        let session = InitializedSession::initialize(
            "init-1".to_owned(),
            &sample_init_request(),
            &backend,
            &pins,
        )
        .expect("initialize");

        assert_eq!(session.init_request_id, "init-1");
        assert_eq!(session.chips.len(), 1);
        assert_eq!(session.chips[0].chip_name, path);
        assert_eq!(session.chips[0].chip_index, 0);

        let info = session.chips[0].chip.get_info().expect("chip info");
        assert_eq!(info.get_name(), "gpiochip0");

        let request = &session.chips[0].request;
        assert_eq!(request.get_num_requested_lines(), 4);
        assert_eq!(
            request.get_value(0).expect("read input"),
            LineValue::Inactive
        );
        assert_eq!(
            request.get_value(3).expect("read output"),
            LineValue::Active
        );
        assert_eq!(
            session.compiled_targets.trigger_target_name(0, 4),
            Some("TRIG")
        );
        assert!(session.edge_buffer.get_capacity() > 0);
    }

    const CHIP0_XML: &str = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input">L</line>
</gpiochip>"#;

    const CHIP1_XML: &str = r#"<gpiochip id="gpiochip1">
    <line id="0" direction="input">H</line>
    <line id="13" direction="input">L</line>
</gpiochip>"#;

    #[test]
    fn initialize_opens_two_configured_device_paths_as_independent_chips() {
        let chip0 = write_chip_file(CHIP0_XML);
        let chip1 = write_chip_file(CHIP1_XML);
        let path0 = chip0.path().to_str().expect("utf8 path");
        let path1 = chip1.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let mut pins = pin_map(path0, &[("gpiochip0:0", 0)]);
        pins.extend(pin_map(path1, &[("GPIO1_B5", 13)]));
        let init_request = BTreeMap::from([
            (
                "A".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Single("gpiochip0:0".to_owned()),
                    bias: None,
                },
            ),
            (
                "B".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Single("GPIO1_B5".to_owned()),
                    bias: None,
                },
            ),
        ]);

        let session =
            InitializedSession::initialize("init-multi".to_owned(), &init_request, &backend, &pins)
                .expect("initialize");

        assert_eq!(session.chip_count(), 2);
        assert_eq!(session.chips[0].chip_index, 0);
        assert_eq!(session.chips[0].chip_name, path0);
        assert_eq!(session.chips[1].chip_index, 1);
        assert_eq!(session.chips[1].chip_name, path1);
        assert_eq!(session.chips[0].request.get_num_requested_lines(), 1);
        assert_eq!(session.chips[1].request.get_num_requested_lines(), 1);
        assert_eq!(
            session.compiled_targets.target("A").expect("A").pins,
            ResolvedPins::Single(ResolvedPin {
                chip_index: 0,
                offset: 0,
            })
        );
        assert_eq!(
            session.compiled_targets.target("B").expect("B").pins,
            ResolvedPins::Single(ResolvedPin {
                chip_index: 1,
                offset: 13,
            })
        );
    }

    #[test]
    fn initialize_reuses_one_session_chip_for_pins_sharing_a_device_path() {
        let chip = write_chip_file(CHIP1_XML);
        let path = chip.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip1:0", 0), ("GPIO1_B5", 13)]);
        let init_request = BTreeMap::from([
            (
                "A".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Single("gpiochip1:0".to_owned()),
                    bias: None,
                },
            ),
            (
                "B".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Single("GPIO1_B5".to_owned()),
                    bias: None,
                },
            ),
        ]);

        let session = InitializedSession::initialize(
            "init-shared".to_owned(),
            &init_request,
            &backend,
            &pins,
        )
        .expect("initialize");

        assert_eq!(session.chip_count(), 1);
        assert_eq!(session.chips[0].chip_name, path);
        assert_eq!(session.chips[0].request.get_num_requested_lines(), 2);
        assert_eq!(
            session.compiled_targets.target("A").expect("A").pins,
            ResolvedPins::Single(ResolvedPin {
                chip_index: 0,
                offset: 0,
            })
        );
        assert_eq!(
            session.compiled_targets.target("B").expect("B").pins,
            ResolvedPins::Single(ResolvedPin {
                chip_index: 0,
                offset: 13,
            })
        );
    }

    #[test]
    fn initialize_rejects_unmapped_pin() {
        let backend = MockBackend::new();
        let request = BTreeMap::from([(
            "IN".to_owned(),
            TargetConfigRequest::Input {
                pin: PinSelector::Single("gpiochip0:0".to_owned()),
                bias: None,
            },
        )]);

        let error = InitializedSession::initialize(
            "init-1".to_owned(),
            &request,
            &backend,
            &BTreeMap::<String, GPIODPinSpec>::new(),
        )
        .err()
        .expect("unmapped pin");
        assert!(matches!(
            error,
            SessionError::UnmappedPin { pin: "gpiochip0:0" }
        ));
    }

    #[test]
    fn initialize_rejects_unavailable_device_file() {
        let backend = MockBackend::new();
        let pins = pin_map("/no/such/gpiochip0.xml", &[("gpiochip0:0", 0)]);
        let request = BTreeMap::from([(
            "IN".to_owned(),
            TargetConfigRequest::Input {
                pin: PinSelector::Single("gpiochip0:0".to_owned()),
                bias: None,
            },
        )]);

        let error = InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins)
            .err()
            .expect("unavailable device");
        match error {
            SessionError::UnavailableDeviceFile { device } => {
                assert_eq!(device, "/no/such/gpiochip0.xml");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn initialize_rejects_missing_line() {
        let file = write_chip_file(CHIP0_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip0:0", 99)]);
        let request = BTreeMap::from([(
            "IN".to_owned(),
            TargetConfigRequest::Input {
                pin: PinSelector::Single("gpiochip0:0".to_owned()),
                bias: None,
            },
        )]);

        let error = InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins)
            .err()
            .expect("missing line");
        match error {
            SessionError::MissingLine { device, line } => {
                assert_eq!(device, path);
                assert_eq!(line, 99);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn initialize_rejects_duplicate_physical_location_in_combined_target() {
        let file = write_chip_file(CHIP1_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip1:13", 13), ("GPIO1_B5", 13)]);
        let request = BTreeMap::from([(
            "OUT".to_owned(),
            TargetConfigRequest::Output {
                pin: PinSelector::Combined(smallvec::smallvec![
                    "gpiochip1:13".to_owned(),
                    "GPIO1_B5".to_owned(),
                ]),
                drive: None,
            },
        )]);

        let error = InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins)
            .err()
            .expect("duplicate location");
        assert!(matches!(
            error,
            SessionError::DuplicatePhysicalLocation { pin: "GPIO1_B5" }
        ));
    }
}
