//! File-backed mock GPIO backend.
//!
//! XML-backed chip snapshots with libgpiod-style trait implementations.
//! Authoritative spec: `.cursor/redesign/gpio-mock-backend.md`.

mod chip;
mod config;
mod events;
mod info;
mod log;
mod request;
mod snapshot;
mod state;
mod watcher;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use chip::MockChip;
pub use config::MockLineConfig;
pub use config::MockLineSettings;
pub use config::MockRequestConfig;
pub use config::MockRequestLineState;
pub use config::default_edge_event_buffer_capacity;
pub use events::MockEdgeEvent;
pub use events::MockEdgeEventBuffer;
pub use events::MockEdgeEventRecord;
pub use info::MockChipInfo;
pub use info::MockInfoEvent;
pub use info::MockLineInfo;
use log::XmlWriteLog;
use quick_xml::XmlVersion;
pub use request::MockLineRequest;
pub use snapshot::LineLevel;
pub use snapshot::MockChipSnapshot;
pub use snapshot::MockLineSnapshot;
pub use snapshot::line_level_to_line_value;
pub use snapshot::line_value_to_line_level;

use crate::gpio::GPIOError;

/// Static API version reported by [`MockBackend::api_version`](crate::gpio::Backend::api_version).
pub const MOCK_API_VERSION: &str = "mock-1.0";

#[cfg(test)]
pub(crate) use log::parse_write_log_blocks;

/// Process-wide mock GPIO backend entry point.
#[derive(Debug, Clone)]
pub struct MockBackend {
    poll_interval: Duration,
    write_log: Option<Arc<XmlWriteLog>>,
}

impl MockBackend {
    /// Creates a new mock backend with no per-process configuration.
    pub fn new() -> Self {
        Self {
            poll_interval: Duration::from_millis(250),
            write_log: None,
        }
    }

    /// Creates a mock backend with a custom file polling interval.
    pub fn with_poll_interval(interval: Duration) -> Self {
        Self {
            poll_interval: interval.max(Duration::from_millis(1)),
            write_log: None,
        }
    }

    /// Enables a background append log of chip XML snapshots after line writes.
    pub fn with_write_log(self, path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let write_log = XmlWriteLog::new(path)?;
        Ok(Self {
            write_log: Some(write_log),
            ..self
        })
    }

    pub(crate) fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub(crate) fn write_log(&self) -> Option<Arc<XmlWriteLog>> {
        self.write_log.clone()
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

/// Parses and normalizes a one-chip XML document string.
pub fn parse_chip_xml(content: &str) -> Result<MockChipSnapshot, GPIOError> {
    let mut reader = quick_xml::Reader::from_str(content);
    snapshot::load_snapshot(&mut reader, XmlVersion::Explicit1_0)
        .map_err(|err| GPIOError::Other(format!("xml parse error: {err}")))
}

/// Serializes a normalized chip snapshot to a one-chip XML document string.
pub fn serialize_chip_xml(snapshot: &MockChipSnapshot) -> Result<String, GPIOError> {
    let mut buffer = Vec::new();
    {
        let mut writer = quick_xml::Writer::new_with_indent(&mut buffer, b' ', 4);
        snapshot::save_snapshot(snapshot, &mut writer)?;
    }
    String::from_utf8(buffer).map_err(|err| GPIOError::Other(format!("xml serialize error: {err}")))
}

#[cfg(test)]
mod tests {
    use super::request::MockLineRequest;
    use super::*;
    use crate::gpio::Backend;
    use crate::gpio::Chip;
    use crate::gpio::ChipInfo;
    use crate::gpio::EdgeEvent;
    use crate::gpio::EdgeEventBuffer;
    use crate::gpio::EdgeEventType;
    use crate::gpio::LineBias;
    use crate::gpio::LineConfig;
    use crate::gpio::LineDirection;
    use crate::gpio::LineDrive;
    use crate::gpio::LineEdge;
    use crate::gpio::LineInfo;
    use crate::gpio::LineRequest;
    use crate::gpio::LineSettings;
    use crate::gpio::LineValue;
    use crate::gpio::RequestConfig;
    use crate::gpio::mock::LineLevel;
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::thread;
    use std::time::Duration;
    use tempfile::NamedTempFile;

    const SAMPLE_XML: &str = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">L</line>
    <line id="1" name="line1" direction="output" drive="push_pull">H</line>
</gpiochip>"#;

    #[test]
    fn parse_and_round_trip_sample_chip() {
        let snapshot = parse_chip_xml(SAMPLE_XML).expect("sample xml should parse");
        assert_eq!(snapshot.name, "gpiochip0");
        assert_eq!(snapshot.label, "mock gpiochip0");
        assert_eq!(snapshot.lines.len(), 2);

        let line0 = snapshot.lines.get(&0).expect("line 0");
        assert_eq!(line0.name, "line0");
        assert_eq!(line0.direction, LineDirection::Input);
        assert_eq!(line0.bias, LineBias::PullUp);
        assert_eq!(line0.persisted_level, LineLevel::Low);

        let line1 = snapshot.lines.get(&1).expect("line 1");
        assert_eq!(line1.direction, LineDirection::Output);
        assert_eq!(line1.drive, LineDrive::PushPull);
        assert_eq!(line1.persisted_level, LineLevel::High);

        let xml = serialize_chip_xml(&snapshot).expect("serialize");
        let round_trip = parse_chip_xml(&xml).expect("round trip");
        assert_eq!(round_trip, snapshot);
    }

    #[test]
    fn reject_duplicate_line_ids() {
        let xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input">L</line>
    <line id="0" direction="output">H</line>
</gpiochip>"#;
        let error = parse_chip_xml(xml).unwrap_err();
        assert!(matches!(error, GPIOError::Other(_)));
    }

    #[test]
    fn reject_input_line_with_drive() {
        let xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" drive="push_pull">L</line>
</gpiochip>"#;
        let error = parse_chip_xml(xml).unwrap_err();
        assert!(matches!(error, GPIOError::Other(_)));
    }

    #[test]
    fn reject_output_line_with_bias() {
        let xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="output" bias="pull_up">L</line>
</gpiochip>"#;
        let error = parse_chip_xml(xml).unwrap_err();
        assert!(matches!(error, GPIOError::Other(_)));
    }

    #[test]
    fn reject_invalid_level_text() {
        let xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input">2</line>
</gpiochip>"#;
        let error = parse_chip_xml(xml).unwrap_err();
        assert!(matches!(error, GPIOError::Other(_)));
    }

    #[test]
    fn reject_empty_chip_id() {
        let xml = r#"<gpiochip id="">
    <line id="0" direction="input">L</line>
</gpiochip>"#;
        let error = parse_chip_xml(xml).unwrap_err();
        assert!(matches!(
            error,
            GPIOError::Other(message) if message.contains("id must not be empty")
        ));
    }

    #[test]
    fn reject_unknown_root_element() {
        let xml = r#"<gpiochips>
    <gpiochip id="gpiochip0">
        <line id="0" direction="input">L</line>
    </gpiochip>
</gpiochips>"#;
        let error = parse_chip_xml(xml).unwrap_err();
        assert!(matches!(error, GPIOError::Other(_)));
    }

    #[test]
    fn reject_unknown_line_attribute() {
        let xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" event="both">L</line>
</gpiochip>"#;
        let error = parse_chip_xml(xml).unwrap_err();
        assert!(matches!(error, GPIOError::Other(_)));
    }

    #[test]
    fn is_gpiochip_device_rejects_invalid_files() {
        let file = NamedTempFile::new().expect("temp file");
        fs::write(file.path(), "not a gpiochip xml document").expect("write");
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        assert!(!backend.is_gpiochip_device(path));
    }

    #[test]
    fn normalize_missing_optional_attributes() {
        let xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input">H</line>
    <line id="1" direction="output">L</line>
</gpiochip>"#;
        let snapshot = parse_chip_xml(xml).expect("parse");
        let input = snapshot.lines.get(&0).expect("input line");
        assert!(input.name.is_empty());
        assert_eq!(input.bias, LineBias::Disabled);

        let output = snapshot.lines.get(&1).expect("output line");
        assert_eq!(output.drive, LineDrive::PushPull);
    }

    fn write_chip_file(contents: &str) -> NamedTempFile {
        let file = NamedTempFile::new().expect("temp file");
        fs::write(file.path(), contents).expect("write chip xml");
        file
    }

    fn request_outputs(backend: &MockBackend, chip: &MockChip, offsets: &[u32]) -> MockLineRequest {
        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut output_settings = backend.new_line_settings().expect("line settings");
        output_settings
            .set_direction(LineDirection::Output)
            .expect("direction");
        line_cfg
            .add_line_settings(offsets, &output_settings)
            .expect("configure outputs");
        chip.request_lines(None, &line_cfg).expect("request lines")
    }

    #[test]
    fn open_chip_exposes_metadata_and_path() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        assert_eq!(chip.get_path(), path);
        let info = chip.get_info().expect("chip info");
        assert_eq!(info.get_name(), "gpiochip0");
        assert_eq!(info.get_label(), "mock gpiochip0");
        assert_eq!(info.get_num_lines(), 2);
        assert!(backend.is_gpiochip_device(path));
    }

    #[test]
    fn request_read_write_and_persist_output() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut output_settings = backend.new_line_settings().expect("line settings");
        output_settings
            .set_direction(LineDirection::Output)
            .expect("direction");
        line_cfg
            .add_line_settings(&[1], &output_settings)
            .expect("configure output");

        let request = chip.request_lines(None, &line_cfg).expect("request lines");
        assert_eq!(request.get_chip_name(), "gpiochip0");
        assert_eq!(request.get_num_requested_lines(), 1);

        let mut offsets = [0u32; 1];
        assert_eq!(request.get_requested_offsets(&mut offsets), 1);
        assert_eq!(offsets, [1]);

        assert_eq!(request.get_value(1).expect("read"), LineValue::Active);
        request
            .set_value(1, LineValue::Active)
            .expect("write output");
        assert_eq!(request.get_value(1).expect("read again"), LineValue::Active);

        let persisted = fs::read_to_string(path).expect("read xml");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
        assert_eq!(
            snapshot.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::High
        );
    }

    #[test]
    fn request_lines_materializes_per_line_output_value_without_set_output_values() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut output_settings = backend.new_line_settings().expect("line settings");
        output_settings
            .set_direction(LineDirection::Output)
            .expect("direction");
        output_settings
            .set_output_value(LineValue::Active)
            .expect("output value");
        line_cfg
            .add_line_settings(&[1], &output_settings)
            .expect("configure output");

        let request = chip.request_lines(None, &line_cfg).expect("request lines");
        assert_eq!(request.get_value(1).expect("read"), LineValue::Active);

        let persisted = fs::read_to_string(path).expect("read xml");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
        assert_eq!(
            snapshot.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::High
        );
    }

    #[test]
    fn set_output_values_override_per_line_output_value_defaults() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut output_settings = backend.new_line_settings().expect("line settings");
        output_settings
            .set_direction(LineDirection::Output)
            .expect("direction");
        output_settings
            .set_output_value(LineValue::Active)
            .expect("output value");
        line_cfg
            .add_line_settings(&[1], &output_settings)
            .expect("configure output");
        line_cfg
            .set_output_values(&[LineValue::Inactive])
            .expect("override output values");

        let request = chip.request_lines(None, &line_cfg).expect("request lines");
        assert_eq!(request.get_value(1).expect("read"), LineValue::Inactive);

        let persisted = fs::read_to_string(path).expect("read xml");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
        assert_eq!(
            snapshot.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::Low
        );
    }

    #[test]
    fn request_keeps_stored_output_level_without_output_value() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut output_settings = backend.new_line_settings().expect("line settings");
        output_settings
            .set_direction(LineDirection::Output)
            .expect("direction");
        line_cfg
            .add_line_settings(&[1], &output_settings)
            .expect("configure output");

        let request = chip.request_lines(None, &line_cfg).expect("request lines");
        assert_eq!(request.get_value(1).expect("read"), LineValue::Active);

        let persisted = fs::read_to_string(path).expect("read xml");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
        assert_eq!(
            snapshot.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::High
        );
    }

    #[test]
    fn partial_set_output_values_falls_back_to_per_line_defaults() {
        let xml = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="output" drive="push_pull">L</line>
    <line id="1" name="line1" direction="output" drive="push_pull">H</line>
</gpiochip>"#;
        let file = write_chip_file(xml);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut first_output = backend.new_line_settings().expect("line settings");
        first_output
            .set_direction(LineDirection::Output)
            .expect("direction");
        let mut second_output = backend.new_line_settings().expect("line settings");
        second_output
            .set_direction(LineDirection::Output)
            .expect("direction");
        second_output
            .set_output_value(LineValue::Inactive)
            .expect("output value");
        line_cfg
            .add_line_settings(&[0, 1], &first_output)
            .expect("configure first output");
        line_cfg
            .add_line_settings(&[1], &second_output)
            .expect("configure second output");
        line_cfg
            .set_output_values(&[LineValue::Active])
            .expect("override first output only");

        let request = chip.request_lines(None, &line_cfg).expect("request lines");
        assert_eq!(request.get_value(0).expect("line 0"), LineValue::Active);
        assert_eq!(request.get_value(1).expect("line 1"), LineValue::Inactive);

        let persisted = fs::read_to_string(path).expect("read xml");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
        assert_eq!(
            snapshot.lines.get(&0).expect("line 0").persisted_level,
            LineLevel::High
        );
        assert_eq!(
            snapshot.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::Low
        );
    }

    #[test]
    fn active_low_inverts_materialized_output_default() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut output_settings = backend.new_line_settings().expect("line settings");
        output_settings
            .set_direction(LineDirection::Output)
            .expect("direction");
        output_settings.set_active_low(true);
        output_settings
            .set_output_value(LineValue::Active)
            .expect("output value");
        line_cfg
            .add_line_settings(&[1], &output_settings)
            .expect("configure output");

        let request = chip.request_lines(None, &line_cfg).expect("request lines");
        assert_eq!(request.get_value(1).expect("read"), LineValue::Active);

        let persisted = fs::read_to_string(path).expect("read xml");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
        assert_eq!(
            snapshot.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::Low
        );
    }

    #[test]
    fn set_values_subset_persists_output_levels() {
        let xml = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="output" drive="push_pull">L</line>
    <line id="1" name="line1" direction="output" drive="push_pull">H</line>
</gpiochip>"#;
        let file = write_chip_file(xml);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut output_settings = backend.new_line_settings().expect("line settings");
        output_settings
            .set_direction(LineDirection::Output)
            .expect("direction");
        line_cfg
            .add_line_settings(&[0, 1], &output_settings)
            .expect("configure outputs");

        let request = chip.request_lines(None, &line_cfg).expect("request lines");

        request
            .set_values_subset(&[1, 0], &[LineValue::Inactive, LineValue::Active])
            .expect("write subset");

        let mut values = [LineValue::Inactive; 2];
        request
            .get_values_subset(&[1, 0], &mut values)
            .expect("read subset");
        assert_eq!(values, [LineValue::Inactive, LineValue::Active]);

        let persisted = fs::read_to_string(path).expect("read xml");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
        assert_eq!(
            snapshot.lines.get(&0).expect("line 0").persisted_level,
            LineLevel::High
        );
        assert_eq!(
            snapshot.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::Low
        );
    }

    #[test]
    fn line_info_reports_consumer_while_requested() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let settings = backend.new_line_settings().expect("line settings");
        line_cfg
            .add_line_settings(&[0], &settings)
            .expect("configure line 0");

        let mut req_cfg = backend.new_request_config().expect("request config");
        req_cfg.set_consumer("test-consumer");

        {
            let _request = chip
                .request_lines(Some(&req_cfg), &line_cfg)
                .expect("request lines");

            let line_info = chip.get_line_info(0).expect("line info");
            assert!(line_info.is_used());
            assert_eq!(line_info.get_consumer(), Some("test-consumer"));

            let persisted = fs::read_to_string(path).expect("read xml");
            let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
            assert_eq!(
                snapshot.lines.get(&0).expect("line 0").consumer,
                "test-consumer"
            );
        }

        let persisted = fs::read_to_string(path).expect("read xml after drop");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted after drop");
        assert!(snapshot.lines.get(&0).expect("line 0").consumer.is_empty());
    }

    #[test]
    fn overlapping_requests_are_rejected() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let settings = backend.new_line_settings().expect("line settings");
        line_cfg
            .add_line_settings(&[0], &settings)
            .expect("configure line 0");

        let _first = chip.request_lines(None, &line_cfg).expect("first request");
        let error = chip.request_lines(None, &line_cfg).unwrap_err();
        assert!(matches!(
            error,
            GPIOError::Other(message) if message.contains("already requested")
        ));
    }

    #[test]
    fn partial_overlap_requests_are_rejected() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut first_cfg = backend.new_line_config().expect("line config");
        let settings = backend.new_line_settings().expect("line settings");
        first_cfg
            .add_line_settings(&[0], &settings)
            .expect("configure line 0");
        let _first = chip.request_lines(None, &first_cfg).expect("first request");

        let mut second_cfg = backend.new_line_config().expect("line config");
        second_cfg
            .add_line_settings(&[0, 1], &settings)
            .expect("configure lines 0 and 1");
        let error = chip.request_lines(None, &second_cfg).unwrap_err();
        assert!(matches!(
            error,
            GPIOError::Other(message) if message.contains("already requested")
        ));
    }

    #[test]
    fn dropped_request_releases_exclusive_lines() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let settings = backend.new_line_settings().expect("line settings");
        line_cfg
            .add_line_settings(&[0], &settings)
            .expect("configure line 0");

        {
            let _first = chip.request_lines(None, &line_cfg).expect("first request");
            let line_info = chip.get_line_info(0).expect("line info");
            assert!(line_info.is_used());
        }

        let line_info = chip.get_line_info(0).expect("line info after drop");
        assert!(!line_info.is_used());

        chip.request_lines(None, &line_cfg)
            .expect("second request after drop");
    }

    #[test]
    fn empty_line_config_is_rejected() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");
        let line_cfg = backend.new_line_config().expect("line config");
        let error = chip.request_lines(None, &line_cfg).unwrap_err();
        assert!(matches!(error, GPIOError::EmptyLineConfig));
    }

    #[test]
    fn get_requested_offsets_follows_line_config_order() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut input_settings = backend.new_line_settings().expect("line settings");
        input_settings
            .set_direction(LineDirection::Input)
            .expect("direction");
        let mut output_settings = backend.new_line_settings().expect("line settings");
        output_settings
            .set_direction(LineDirection::Output)
            .expect("direction");
        line_cfg
            .add_line_settings(&[1], &output_settings)
            .expect("configure output");
        line_cfg
            .add_line_settings(&[0], &input_settings)
            .expect("configure input");

        let request = chip.request_lines(None, &line_cfg).expect("request lines");

        let mut offsets = [0u32; 2];
        assert_eq!(request.get_requested_offsets(&mut offsets), 2);
        assert_eq!(offsets, [1, 0]);
    }

    #[test]
    fn request_lines_persists_property_changes_without_output_values() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut input_settings = backend.new_line_settings().expect("line settings");
        input_settings
            .set_direction(LineDirection::Input)
            .expect("direction");
        input_settings.set_bias(LineBias::PullDown).expect("bias");
        line_cfg
            .add_line_settings(&[0], &input_settings)
            .expect("configure input");

        let _request = chip.request_lines(None, &line_cfg).expect("request lines");

        let persisted = fs::read_to_string(path).expect("read xml");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
        let line = snapshot.lines.get(&0).expect("line 0");
        assert_eq!(line.direction, LineDirection::Input);
        assert_eq!(line.bias, LineBias::PullDown);
    }

    #[test]
    fn active_low_inverts_logical_values() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");

        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut settings = backend.new_line_settings().expect("line settings");
        settings.set_active_low(true);
        line_cfg
            .add_line_settings(&[0], &settings)
            .expect("configure input");

        let request = chip.request_lines(None, &line_cfg).expect("request lines");
        assert_eq!(request.get_value(0).expect("read"), LineValue::Active);

        let persisted = fs::read_to_string(path).expect("read xml");
        let snapshot = parse_chip_xml(&persisted).expect("parse persisted");
        assert!(snapshot.lines.get(&0).expect("line 0").active_low);
    }

    #[test]
    fn external_file_reload_ignores_metadata_and_active_low_edits() {
        let xml = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up" active_low="false">L</line>
    <line id="1" name="line1" direction="output" drive="push_pull" active_low="false">H</line>
</gpiochip>"#;
        let file = write_chip_file(xml);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");
        let request = request_input_with_edge(&backend, &chip, LineEdge::Both);

        let metadata_edit = r#"<gpiochip id="gpiochip0" label="renamed chip">
    <line id="0" name="renamed" direction="output" drive="push_pull" active_low="true">L</line>
    <line id="1" name="renamed-out" direction="input" bias="pull_down" active_low="true">H</line>
</gpiochip>"#;
        chip.apply_external_file_diff_for_test(metadata_edit)
            .expect("apply metadata-only external edit");

        let mut buffer = backend.new_edge_event_buffer(0).expect("buffer");
        let error = request.read_edge_events(&mut buffer, 16).unwrap_err();
        assert!(matches!(error, GPIOError::Other(message) if message.contains("no edge events")));

        let snapshot = parse_chip_xml(&fs::read_to_string(path).expect("read xml")).expect("parse");
        let line0 = snapshot.lines.get(&0).expect("line 0");
        assert_eq!(line0.name, "line0");
        assert_eq!(line0.direction, LineDirection::Input);
        assert_eq!(line0.bias, LineBias::PullUp);
        assert!(!line0.active_low);
        assert_eq!(line0.persisted_level, LineLevel::Low);
        let line1 = snapshot.lines.get(&1).expect("line 1");
        assert_eq!(line1.direction, LineDirection::Output);
        assert!(!line1.active_low);
        assert_eq!(line1.persisted_level, LineLevel::High);
    }

    #[test]
    fn active_low_xml_load_and_persist_round_trip() {
        let xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" bias="disabled" active_low="true">L</line>
    <line id="1" direction="output" drive="push_pull" active_low="true">L</line>
</gpiochip>"#;
        let snapshot = parse_chip_xml(xml).expect("parse");
        assert!(snapshot.lines.get(&0).expect("line 0").active_low);
        assert_eq!(
            snapshot.lines.get(&0).expect("line 0").persisted_level,
            LineLevel::Low
        );
        assert!(snapshot.lines.get(&1).expect("line 1").active_low);
        assert_eq!(
            snapshot.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::Low
        );

        let serialized = serialize_chip_xml(&snapshot).expect("serialize");
        assert!(serialized.contains(r#"active_low="true"#));
        let round_trip = parse_chip_xml(&serialized).expect("round trip");
        assert_eq!(round_trip, snapshot);
    }

    fn input_xml_with_level(level: &str) -> String {
        format!(
            r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">{level}</line>
    <line id="1" name="line1" direction="output" drive="push_pull">H</line>
</gpiochip>"#
        )
    }

    fn request_input_with_edge(
        backend: &MockBackend,
        chip: &MockChip,
        edge: LineEdge,
    ) -> MockLineRequest {
        let mut line_cfg = backend.new_line_config().expect("line config");
        let mut settings = backend.new_line_settings().expect("line settings");
        settings.set_edge_detection(edge).expect("edge detection");
        line_cfg
            .add_line_settings(&[0], &settings)
            .expect("configure input");
        chip.request_lines(None, &line_cfg).expect("request lines")
    }

    fn apply_external_input_level(chip: &MockChip, level: &str) {
        chip.apply_external_file_diff_for_test(&input_xml_with_level(level))
            .expect("apply external file diff");
    }

    fn wait_for_edge_events(
        request: &MockLineRequest,
        backend: &MockBackend,
        timeout: Duration,
    ) -> (usize, super::events::MockEdgeEventBuffer) {
        let deadline = std::time::Instant::now() + timeout;
        let mut buffer = backend.new_edge_event_buffer(0).expect("buffer");
        while std::time::Instant::now() < deadline {
            if let Ok(count) = request.read_edge_events(&mut buffer, 16) {
                return (count, buffer);
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for edge events");
    }

    #[test]
    fn read_edge_events_errors_when_queue_empty() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");
        let request = request_input_with_edge(&backend, &chip, LineEdge::Both);

        let mut buffer = backend.new_edge_event_buffer(0).expect("buffer");
        let error = request.read_edge_events(&mut buffer, 16).unwrap_err();
        assert!(matches!(error, GPIOError::Other(message) if message.contains("no edge events")));
    }

    #[test]
    fn external_input_change_produces_edge_event() {
        let file = write_chip_file(&input_xml_with_level("L"));
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");
        let request = request_input_with_edge(&backend, &chip, LineEdge::Both);

        fs::write(path, input_xml_with_level("H")).expect("external edit");
        apply_external_input_level(&chip, "H");

        let (count, _) = wait_for_edge_events(&request, &backend, Duration::from_secs(2));
        assert_eq!(count, 1);

        let mut buffer = backend.new_edge_event_buffer(0).expect("buffer");
        let error = request.read_edge_events(&mut buffer, 16).unwrap_err();
        assert!(matches!(error, GPIOError::Other(message) if message.contains("no edge events")));
    }

    #[test]
    fn edge_detection_filters_transitions() {
        let file = write_chip_file(&input_xml_with_level("H"));
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");
        let request = request_input_with_edge(&backend, &chip, LineEdge::Rising);

        fs::write(path, input_xml_with_level("L")).expect("external falling edit");
        apply_external_input_level(&chip, "L");

        let mut buffer = backend.new_edge_event_buffer(0).expect("buffer");
        let error = request.read_edge_events(&mut buffer, 16).unwrap_err();
        assert!(matches!(error, GPIOError::Other(message) if message.contains("no edge events")));

        fs::write(path, input_xml_with_level("H")).expect("external rising edit");
        apply_external_input_level(&chip, "H");
        let (count, buffer) = wait_for_edge_events(&request, &backend, Duration::from_secs(2));
        assert_eq!(count, 1);
        let event = buffer.get_event(0).expect("event");
        assert_eq!(event.get_event_type(), EdgeEventType::RisingEdge);
        assert_eq!(event.get_line_offset(), 0);
    }

    #[test]
    fn eventfd_is_readable_when_events_queued_and_drained() {
        use nix::poll::PollFd;
        use nix::poll::PollFlags;
        use nix::poll::PollTimeout;
        use nix::poll::poll;
        use std::os::fd::BorrowedFd;

        let file = write_chip_file(&input_xml_with_level("L"));
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let chip = backend.open_chip(path).expect("open chip");
        let request = request_input_with_edge(&backend, &chip, LineEdge::Both);
        let fd = request.as_raw_fd();

        let is_readable = || {
            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(fd) };
            let mut fds = [PollFd::new(borrowed_fd, PollFlags::POLLIN)];
            let ready = poll(&mut fds, PollTimeout::from(0u8)).unwrap_or(0);
            ready > 0
                && fds[0]
                    .revents()
                    .is_some_and(|flags| flags.contains(PollFlags::POLLIN))
        };

        fs::write(path, input_xml_with_level("H")).expect("external edit");
        apply_external_input_level(&chip, "H");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut saw_readable = false;
        while std::time::Instant::now() < deadline {
            if is_readable() {
                saw_readable = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            saw_readable,
            "eventfd should be readable when events are queued"
        );

        let mut buffer = backend.new_edge_event_buffer(0).expect("buffer");
        let count = request.read_edge_events(&mut buffer, 16).expect("drain");
        assert_eq!(count, 1);
        assert!(
            !is_readable(),
            "eventfd should not be readable after full drain"
        );
    }

    #[test]
    fn write_log_disabled_by_default() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let log_dir = tempfile::tempdir().expect("temp dir");
        let log_path = log_dir.path().join("mock-write.log");
        assert!(!log_path.exists());

        let backend = MockBackend::new();
        assert!(backend.write_log().is_none());
        let chip = backend.open_chip(path).expect("open chip");
        let request = request_outputs(&backend, &chip, &[1]);

        request
            .set_value(1, LineValue::Inactive)
            .expect("write output");

        assert!(
            !log_path.exists(),
            "set_value must not create a write log unless with_write_log is used"
        );
    }

    #[test]
    fn write_log_records_set_value_and_set_values_subset() {
        let xml = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="output" drive="push_pull">L</line>
    <line id="1" name="line1" direction="output" drive="push_pull">H</line>
</gpiochip>"#;
        let file = write_chip_file(xml);
        let path = file.path().to_str().expect("utf8 path");
        let log_file = NamedTempFile::new().expect("temp log file");
        let log_path = log_file.path().to_path_buf();

        let backend = MockBackend::new()
            .with_write_log(&log_path)
            .expect("open write log");
        let chip = backend.open_chip(path).expect("open chip");
        let request = request_outputs(&backend, &chip, &[0, 1]);

        request.set_value(0, LineValue::Active).expect("set_value");
        request
            .set_values_subset(&[1], &[LineValue::Inactive])
            .expect("set_values_subset");

        backend.write_log().expect("write log").flush();

        let content = fs::read_to_string(&log_path).expect("read write log");
        let blocks = parse_write_log_blocks(&content);
        assert_eq!(blocks.len(), 2, "expected one dump per line write");
        assert!(
            blocks[0].0 <= blocks[1].0,
            "timestamps must be non-decreasing"
        );

        let first = parse_chip_xml(&blocks[0].1).expect("parse first dump");
        assert_eq!(
            first.lines.get(&0).expect("line 0").persisted_level,
            LineLevel::High
        );
        assert_eq!(
            first.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::High
        );
        assert!(
            blocks[0].1.contains(">H</line>"),
            "first dump should reflect updated high level"
        );

        let second = parse_chip_xml(&blocks[1].1).expect("parse second dump");
        assert_eq!(
            second.lines.get(&0).expect("line 0").persisted_level,
            LineLevel::High
        );
        assert_eq!(
            second.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::Low
        );
        assert!(
            blocks[1].1.contains(">L</line>"),
            "second dump should reflect updated low level"
        );
    }

    #[test]
    fn write_log_skips_request_and_drop_persist() {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let log_file = NamedTempFile::new().expect("temp log file");
        let log_path = log_file.path().to_path_buf();

        let backend = MockBackend::new()
            .with_write_log(&log_path)
            .expect("open write log");
        let write_log = backend.write_log().expect("write log");
        let chip = backend.open_chip(path).expect("open chip");

        {
            let request = request_outputs(&backend, &chip, &[1]);
            write_log.flush();
            let after_request = fs::read_to_string(&log_path).expect("read write log");
            assert!(
                after_request.is_empty(),
                "request_lines persist must not append dump blocks"
            );

            request
                .set_value(1, LineValue::Inactive)
                .expect("set_value");
            write_log.flush();
            let after_set = fs::read_to_string(&log_path).expect("read write log");
            assert_eq!(parse_write_log_blocks(&after_set).len(), 1);
        }

        write_log.flush();
        let after_drop = fs::read_to_string(&log_path).expect("read write log");
        assert_eq!(
            parse_write_log_blocks(&after_drop).len(),
            1,
            "Drop persist must not append an extra dump block"
        );
    }
}
