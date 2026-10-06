use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::config::GPIODPinSpec;
use crate::config::GpioConsumer;
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
use crate::gpio::RequestConfig;
use crate::gpio::ValidLineValue;
use crate::protocol::common::ArrayMap;
use crate::protocol::request::BiasMode;
use crate::protocol::request::DriveMode;
use crate::protocol::request::EdgeMode;
use crate::protocol::request::PinConfigRequest;
use crate::protocol::request::PinSelector;

use super::batch::CombinedOffsets;
use super::compiled::ChipIndices;
use super::compiled::CompiledPin;
use super::compiled::CompiledPins;
use super::compiled::ResolvedPin;

use super::state::SessionError;

pub trait SessionConfig: Send + Sync {
    fn resolve_gpiod_pin(&self, pin: &str) -> Option<&GPIODPinSpec>;

    fn gpiod_pins(&self) -> &BTreeMap<String, GPIODPinSpec>;

    /// Render `service.gpio-consumer` for `session_id`.
    ///
    /// Test pin-map implementations use the default template `svc_{id}`.
    fn render_gpio_consumer(&self, session_id: u64) -> String {
        GpioConsumer::default().render(session_id)
    }
}

impl SessionConfig for ServiceConfig {
    fn gpiod_pins(&self) -> &BTreeMap<String, GPIODPinSpec> {
        ServiceConfig::gpiod_pins(self)
    }

    fn resolve_gpiod_pin(&self, pin: &str) -> Option<&GPIODPinSpec> {
        ServiceConfig::resolve_gpiod_pin(self, pin)
    }

    fn render_gpio_consumer(&self, session_id: u64) -> String {
        self.gpio_consumer.render(session_id)
    }
}

impl SessionConfig for BTreeMap<String, GPIODPinSpec> {
    fn gpiod_pins(&self) -> &BTreeMap<String, GPIODPinSpec> {
        self
    }

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
    pub compiled_pins: CompiledPins,
    pub edge_buffer: B::EdgeEventBuffer,
    pub chips: Vec<SessionChip<B>>,
    /// Compiled per-pin `final` writes, applied on graceful session close.
    pub final_batch: CombinedOffsets<ValidLineValue>,
}

impl<B: Backend> InitializedSession<B> {
    pub fn initialize<'a>(
        init_request_id: String,
        request: &'a ArrayMap<PinSelector, PinConfigRequest>,
        backend: &B,
        config: &'a dyn SessionConfig,
        consumer: &str,
    ) -> Result<Self, SessionError<'a>> {
        if request.is_empty() {
            return Err(SessionError::InvalidParameters(
                "init target must not be empty".to_owned(),
            ));
        }
        let mut chip_indices = ChipIndices::<GPIODChipData<B>>::new();
        let mut compiled_pins = CompiledPins {
            by_name: BTreeMap::new(),
            trigger_by_pin: BTreeMap::new(),
        };
        let mut seen = BTreeSet::new();
        let mut finals = Vec::new();
        let mut settings = backend.new_line_settings()?;
        for (selector, pin_config) in request {
            pin_config
                .validate()
                .map_err(SessionError::InvalidParameters)?;
            settings.reset();
            let mode = match pin_config {
                PinConfigRequest::Input { bias } => {
                    settings.set_direction(LineDirection::Input)?;
                    if let Some(bias) = bias {
                        settings.set_bias(protocol_bias_to_line_bias(*bias))?;
                    }
                    super::PinMode::Input
                }
                PinConfigRequest::Output {
                    drive,
                    initial_value,
                    ..
                } => {
                    settings.set_direction(LineDirection::Output)?;
                    if let Some(drive) = drive {
                        settings.set_drive(protocol_drive_to_line_drive(*drive))?;
                    }
                    if let Some(value) = initial_value {
                        settings.set_output_value(if *value == 1 {
                            ValidLineValue::Active
                        } else {
                            ValidLineValue::Inactive
                        })?;
                    }
                    super::PinMode::Output
                }
                PinConfigRequest::Trigger { edge } => {
                    settings.set_direction(LineDirection::Input)?;
                    settings.set_edge_detection(protocol_edge_to_line_edge(*edge))?;
                    super::PinMode::Trigger
                }
            };
            for pin in selector.iter() {
                let spec = config
                    .resolve_gpiod_pin(pin)
                    .ok_or(SessionError::UnmappedPin { pin })?;
                let (resolved, data) = chip_indices.resolve(&spec.device, spec.line);
                if !seen.insert((resolved.chip_index, resolved.offset)) {
                    return Err(SessionError::DuplicatePhysicalLocation { pin });
                }
                append_line_settings(backend, &settings, &[resolved.offset], data)?;
                if mode == super::PinMode::Trigger {
                    compiled_pins
                        .trigger_by_pin
                        .insert((resolved.chip_index, resolved.offset), pin.to_owned());
                }
                compiled_pins.by_name.insert(
                    pin.to_owned(),
                    CompiledPin {
                        mode,
                        pin: resolved,
                    },
                );
                if let PinConfigRequest::Output {
                    final_value: Some(value),
                    ..
                } = pin_config
                {
                    finals.push((
                        resolved,
                        if *value == 1 {
                            ValidLineValue::Active
                        } else {
                            ValidLineValue::Inactive
                        },
                    ));
                }
            }
        }
        let mut final_batch = CombinedOffsets::new(chip_indices.len());
        for (pin, value) in finals {
            final_batch
                .add_set(pin.chip_index, pin.offset, value)
                .map_err(|error| SessionError::Other(error.into()))?;
        }
        let edge_buffer = backend.new_edge_event_buffer(16)?;
        let chips_data = chip_indices.collect();

        let mut req_cfg = backend.new_request_config()?;
        req_cfg.set_consumer(consumer);

        let mut chips = Vec::new();
        for (chip_index, (device, chip_data)) in chips_data.into_iter().enumerate() {
            let Some(chip_data) = chip_data else {
                continue;
            };
            let chip = backend
                .open_chip(device)
                .map_err(|error| map_open_chip_error(device, error))?;
            let request = chip
                .request_lines(Some(&req_cfg), &chip_data.line_config)
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
            compiled_pins,
            edge_buffer,
            chips,
            final_batch,
        })
    }

    pub fn chip_count(&self) -> usize {
        self.chips.len()
    }
}

struct GPIODChipData<B: Backend> {
    line_config: B::LineConfig,
}

fn map_open_chip_error<'a>(device: &'a str, error: GPIOError) -> SessionError<'a> {
    match error {
        GPIOError::Io(_) => SessionError::UnavailableDeviceFile { device },
        other => SessionError::GPIO(other),
    }
}

fn map_request_lines_error<'a>(device: &'a str, error: GPIOError) -> SessionError<'a> {
    match error {
        GPIOError::InvalidOffset(line) => SessionError::MissingLine { device, line },
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
    use crate::config::GpioConsumer;
    use crate::config::ServiceConfig;
    use crate::gpio::Chip;
    use crate::gpio::ChipInfo;
    use crate::gpio::EdgeEventBuffer;
    use crate::gpio::LineInfo;
    use crate::gpio::LineRequest;
    use crate::gpio::ValidLineValue;
    use crate::gpio::mock::MockBackend;
    use crate::protocol::common::ArrayMap;
    use crate::protocol::request::EdgeMode;
    use crate::protocol::request::PinConfigRequest;
    use crate::protocol::request::PinSelector;
    use crate::session::ResolvedPin;
    use crate::session::SessionError;
    use tempfile::NamedTempFile;

    use super::InitializedSession;
    use super::SessionConfig;

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
                        id: *line,
                        device: path.to_owned(),
                        line: *line,
                    },
                )
            })
            .collect()
    }

    fn assert_requested_consumer<C: Chip>(chip: &C, offsets: &[u32], consumer: &str) {
        for offset in offsets {
            let info = chip.get_line_info(*offset).expect("line info");
            assert!(info.is_used(), "line {offset} should be requested");
            assert_eq!(
                info.get_consumer(),
                Some(consumer),
                "line {offset} consumer"
            );
        }
    }

    fn init_map<V, const N: usize>(entries: [(String, V); N]) -> ArrayMap<PinSelector, V> {
        entries
            .into_iter()
            .map(|(name, value)| (PinSelector::parse(name).unwrap(), value))
            .collect()
    }

    fn sample_init_request() -> ArrayMap<PinSelector, PinConfigRequest> {
        init_map([
            (
                "gpiochip0:0".to_owned(),
                PinConfigRequest::Input { bias: None },
            ),
            (
                "gpiochip0:2|gpiochip0:3".to_owned(),
                PinConfigRequest::Output {
                    drive: None,
                    initial_value: None,
                    final_value: None,
                },
            ),
            (
                "gpiochip0:4".to_owned(),
                PinConfigRequest::Trigger {
                    edge: EdgeMode::Both,
                },
            ),
        ])
    }

    #[test]
    fn initialize_classifies_invalid_request_parameters() {
        let backend = MockBackend::new();
        let pins = BTreeMap::<String, GPIODPinSpec>::new();
        let empty = ArrayMap::<PinSelector, PinConfigRequest>::new();
        let error =
            InitializedSession::initialize("init-1".to_owned(), &empty, &backend, &pins, "svc_1")
                .err()
                .expect("empty target");
        assert!(matches!(error, SessionError::InvalidParameters(_)));

        let invalid_value = init_map([(
            "OUT".to_owned(),
            PinConfigRequest::Output {
                drive: None,
                initial_value: Some(2),
                final_value: None,
            },
        )]);
        let error = InitializedSession::initialize(
            "init-2".to_owned(),
            &invalid_value,
            &backend,
            &pins,
            "svc_1",
        )
        .err()
        .expect("invalid output value");
        assert!(matches!(error, SessionError::InvalidParameters(_)));
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
            "svc_1",
        )
        .expect("initialize");

        assert_eq!(session.init_request_id, "init-1");
        assert_eq!(session.chips.len(), 1);
        assert_eq!(session.chips[0].chip_name, path);
        assert_eq!(session.chips[0].chip_index, 0);

        let info = session.chips[0].chip.get_info().expect("chip info");
        assert_eq!(info.get_name(), "gpiochip0");
        assert_requested_consumer(&session.chips[0].chip, &[0, 2, 3, 4], "svc_1");

        let request = &session.chips[0].request;
        assert_eq!(request.get_num_requested_lines(), 4);
        assert_eq!(
            request.get_value(0).expect("read input"),
            ValidLineValue::Inactive
        );
        assert_eq!(
            request.get_value(3).expect("read output"),
            ValidLineValue::Active
        );
        assert_eq!(
            session.compiled_pins.trigger_pin_name(0, 4),
            Some("gpiochip0:4")
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
        let init_request = init_map([
            (
                "gpiochip0:0".to_owned(),
                PinConfigRequest::Input { bias: None },
            ),
            (
                "GPIO1_B5".to_owned(),
                PinConfigRequest::Input { bias: None },
            ),
        ]);

        let session = InitializedSession::initialize(
            "init-multi".to_owned(),
            &init_request,
            &backend,
            &pins,
            "svc_1",
        )
        .expect("initialize");

        assert_eq!(session.chip_count(), 2);
        for (name, path, offset) in [("gpiochip0:0", path0, 0), ("GPIO1_B5", path1, 13)] {
            let pin = session.compiled_pins.pin(name).unwrap().pin;
            let chip = &session.chips[pin.chip_index as usize];
            assert_eq!(chip.chip_name, path);
            assert_eq!(pin.offset, offset);
            assert_eq!(chip.request.get_num_requested_lines(), 1);
            assert_requested_consumer(&chip.chip, &[offset], "svc_1");
        }
    }

    #[test]
    fn pin_map_session_config_renders_default_consumer_template() {
        let pins = pin_map("/unused", &[("gpiochip0:0", 0)]);
        assert_eq!(pins.render_gpio_consumer(1), "svc_1");
        assert_eq!(pins.render_gpio_consumer(42), "svc_42");
        assert_eq!(
            pins.render_gpio_consumer(1),
            GpioConsumer::default().render(1)
        );
    }

    #[test]
    fn service_config_renders_configured_gpio_consumer() {
        let config = ServiceConfig::from_toml_str(
            r#"
[service]
socket = "/tmp/gpiojsonsvc.sock"
gpio-consumer = "app_{id}"

[pins.gpiod]
"gpiochip0:0" = { id = 0, device = "/dev/gpiochip0", line = 0 }
"#,
        )
        .expect("config");
        assert_eq!(config.render_gpio_consumer(7), "app_7");
    }

    #[test]
    fn initialize_applies_custom_consumer_to_requested_lines() {
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
            "init-consumer".to_owned(),
            &sample_init_request(),
            &backend,
            &pins,
            "myapp",
        )
        .expect("initialize");

        assert_requested_consumer(&session.chips[0].chip, &[0, 2, 3, 4], "myapp");
    }

    #[test]
    fn initialize_reuses_one_session_chip_for_pins_sharing_a_device_path() {
        let chip = write_chip_file(CHIP1_XML);
        let path = chip.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip1:0", 0), ("GPIO1_B5", 13)]);
        let init_request = init_map([
            (
                "gpiochip1:0".to_owned(),
                PinConfigRequest::Input { bias: None },
            ),
            (
                "GPIO1_B5".to_owned(),
                PinConfigRequest::Input { bias: None },
            ),
        ]);

        let session = InitializedSession::initialize(
            "init-shared".to_owned(),
            &init_request,
            &backend,
            &pins,
            "svc_1",
        )
        .expect("initialize");

        assert_eq!(session.chip_count(), 1);
        assert_eq!(session.chips[0].chip_name, path);
        assert_eq!(session.chips[0].request.get_num_requested_lines(), 2);
        assert_eq!(
            session
                .compiled_pins
                .pin("gpiochip1:0")
                .expect("gpiochip1:0")
                .pin,
            ResolvedPin {
                chip_index: 0,
                offset: 0,
            }
        );
        assert_eq!(
            session.compiled_pins.pin("GPIO1_B5").expect("GPIO1_B5").pin,
            ResolvedPin {
                chip_index: 0,
                offset: 13,
            }
        );
    }

    #[test]
    fn initialize_rejects_unmapped_pin() {
        let backend = MockBackend::new();
        let pins = BTreeMap::<String, GPIODPinSpec>::new();
        let request = init_map([(
            "gpiochip0:0".to_owned(),
            PinConfigRequest::Input { bias: None },
        )]);

        let error =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
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
        let request = init_map([(
            "gpiochip0:0".to_owned(),
            PinConfigRequest::Input { bias: None },
        )]);

        let error =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
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
        let request = init_map([(
            "gpiochip0:0".to_owned(),
            PinConfigRequest::Input { bias: None },
        )]);

        let error =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
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
        let request = init_map([(
            "gpiochip1:13|GPIO1_B5".to_owned(),
            PinConfigRequest::Output {
                drive: None,
                initial_value: None,
                final_value: None,
            },
        )]);

        let error =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
                .err()
                .expect("duplicate location");
        assert!(matches!(
            error,
            SessionError::DuplicatePhysicalLocation { pin: "GPIO1_B5" }
        ));
    }

    #[test]
    fn initialize_applies_single_initial_value() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip0:2", 2)]);
        let request = init_map([(
            "gpiochip0:2".to_owned(),
            PinConfigRequest::Output {
                drive: None,
                initial_value: Some(1),
                final_value: None,
            },
        )]);

        let session =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
                .expect("initialize");
        assert_eq!(
            session.chips[0].request.get_value(2).expect("line 2"),
            ValidLineValue::Active
        );
        assert_eq!(session.final_batch.iter().count(), 0);
    }

    #[test]
    fn initialize_broadcasts_combined_initial_value() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip0:2", 2), ("gpiochip0:3", 3)]);
        let request = init_map([(
            "gpiochip0:2|gpiochip0:3".to_owned(),
            PinConfigRequest::Output {
                drive: None,
                initial_value: Some(1),
                final_value: None,
            },
        )]);

        let session =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
                .expect("initialize");
        let request = &session.chips[0].request;
        assert_eq!(
            request.get_value(2).expect("line 2"),
            ValidLineValue::Active
        );
        assert_eq!(
            request.get_value(3).expect("line 3"),
            ValidLineValue::Active
        );
    }

    #[test]
    fn initialize_preserves_persisted_output_when_initial_is_omitted() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip0:2", 2), ("gpiochip0:3", 3)]);
        let request = init_map([(
            "gpiochip0:2|gpiochip0:3".to_owned(),
            PinConfigRequest::Output {
                drive: None,
                initial_value: None,
                final_value: None,
            },
        )]);

        let session =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
                .expect("initialize");
        let request = &session.chips[0].request;
        assert_eq!(
            request.get_value(2).expect("line 2"),
            ValidLineValue::Inactive
        );
        assert_eq!(
            request.get_value(3).expect("line 3"),
            ValidLineValue::Active
        );
    }

    #[test]
    fn initialize_compiles_retained_final_batch() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip0:2", 2), ("gpiochip0:3", 3)]);
        let request = init_map([
            (
                "gpiochip0:2".to_owned(),
                PinConfigRequest::Output {
                    drive: None,
                    initial_value: Some(1),
                    final_value: Some(0),
                },
            ),
            (
                "gpiochip0:3".to_owned(),
                PinConfigRequest::Output {
                    drive: None,
                    initial_value: None,
                    final_value: Some(1),
                },
            ),
        ]);

        let session =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
                .expect("initialize");
        assert_eq!(session.final_batch.offsets(0).expect("offsets"), &[2, 3]);
        assert_eq!(
            session.final_batch.attachments(0).expect("values"),
            &[ValidLineValue::Inactive, ValidLineValue::Active]
        );
        assert_eq!(
            session.chips[0].request.get_value(2).expect("line 2"),
            ValidLineValue::Active
        );
        assert_eq!(
            session.chips[0].request.get_value(3).expect("line 3"),
            ValidLineValue::Active
        );
    }

    #[test]
    fn initialize_broadcasts_combined_final_value() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip0:2", 2), ("gpiochip0:3", 3)]);
        let request = init_map([(
            "gpiochip0:2|gpiochip0:3".to_owned(),
            PinConfigRequest::Output {
                drive: None,
                initial_value: None,
                final_value: Some(1),
            },
        )]);

        let session =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
                .expect("initialize");
        assert_eq!(session.final_batch.offsets(0).expect("offsets"), &[2, 3]);
        assert_eq!(
            session.final_batch.attachments(0).expect("values"),
            &[ValidLineValue::Active, ValidLineValue::Active]
        );
    }

    #[test]
    fn initialize_rejects_overlapping_init_entries() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(path, &[("gpiochip0:2", 2), ("gpiochip0:3", 3)]);
        let request = init_map([
            (
                "gpiochip0:2".to_owned(),
                PinConfigRequest::Output {
                    drive: None,
                    initial_value: None,
                    final_value: Some(0),
                },
            ),
            (
                "gpiochip0:2|gpiochip0:3".to_owned(),
                PinConfigRequest::Output {
                    drive: None,
                    initial_value: None,
                    final_value: Some(1),
                },
            ),
        ]);

        let error =
            InitializedSession::initialize("init-1".to_owned(), &request, &backend, &pins, "svc_1")
                .err()
                .expect("conflicting finals");
        assert!(error.to_string().contains("duplicate physical location"));
    }
}
