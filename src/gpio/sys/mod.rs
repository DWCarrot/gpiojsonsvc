//! Real `libgpiod` FFI backend.
//!
//! Links `libgpiod-sys` (C libgpiod 2.x via pkg-config). Submodules are not
//! `cfg`-gated so rust-analyzer always attaches them to the crate.

mod chip;
mod config;
mod convert;
mod request;

pub use libgpiod_sys as ffi;

pub use chip::SysChip;
pub use chip::SysChipInfo;
pub use chip::SysInfoEvent;
pub use chip::SysLineInfo;
pub use config::SysLineConfig;
pub use config::SysLineSettings;
pub use config::SysRequestConfig;
pub use request::SysEdgeEvent;
pub use request::SysEdgeEventBuffer;
pub use request::SysLineRequest;

use crate::gpio::Backend;
use crate::gpio::GPIOError;

/// Process-wide real GPIO backend entry point.
#[derive(Debug, Clone, Copy, Default)]
pub struct SysBackend;

impl SysBackend {
    /// Creates the libgpiod backend.
    pub fn new() -> Self {
        Self
    }
}

impl Backend for SysBackend {
    type Chip = SysChip;
    type LineSettings = SysLineSettings;
    type LineConfig = SysLineConfig;
    type RequestConfig = SysRequestConfig;
    type EdgeEventBuffer = SysEdgeEventBuffer;

    fn open_chip(&self, path: &str) -> Result<Self::Chip, GPIOError> {
        let c_path = convert::cstring(path, "chip path")?;
        let ptr = unsafe { ffi::gpiod_chip_open(c_path.as_ptr()) };
        SysChip::take(ptr, "gpiod_chip_open")
    }

    fn new_line_settings(&self) -> Result<Self::LineSettings, GPIOError> {
        SysLineSettings::new()
    }

    fn new_line_config(&self) -> Result<Self::LineConfig, GPIOError> {
        SysLineConfig::new()
    }

    fn new_request_config(&self) -> Result<Self::RequestConfig, GPIOError> {
        SysRequestConfig::new()
    }

    fn new_edge_event_buffer(&self, capacity: usize) -> Result<Self::EdgeEventBuffer, GPIOError> {
        SysEdgeEventBuffer::new(capacity)
    }

    fn is_gpiochip_device(&self, path: &str) -> bool {
        let Ok(c_path) = convert::cstring(path, "chip path") else {
            return false;
        };
        unsafe { ffi::gpiod_is_gpiochip_device(c_path.as_ptr()) }
    }

    fn api_version(&self) -> &'static str {
        unsafe { convert::cstr_from_ptr(ffi::gpiod_api_version()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpio::Chip;
    use crate::gpio::ChipInfo;
    use crate::gpio::EdgeEventBuffer;
    use crate::gpio::LineConfig;
    use crate::gpio::LineDirection;
    use crate::gpio::LineEdge;
    use crate::gpio::LineSettings;
    use crate::gpio::RequestConfig;
    use crate::gpio::ValidLineValue;
    use std::fs;
    use std::os::fd::AsRawFd;
    use tempfile::NamedTempFile;

    fn first_gpiochip() -> Option<String> {
        let backend = SysBackend::new();
        (0..8)
            .map(|n| format!("/dev/gpiochip{n}"))
            .find(|path| backend.is_gpiochip_device(path))
    }

    #[test]
    fn api_version_is_nonempty() {
        let version = SysBackend::new().api_version();
        assert!(!version.is_empty(), "empty libgpiod version");
    }

    #[test]
    fn is_gpiochip_device_rejects_regular_file() {
        let file = NamedTempFile::new().expect("tempfile");
        let path = file.path().to_str().expect("utf8");
        assert!(!SysBackend::new().is_gpiochip_device(path));
        assert!(!SysBackend::new().is_gpiochip_device("/no/such/gpiochip"));
    }

    #[test]
    fn line_settings_round_trip() {
        let backend = SysBackend::new();
        let mut settings = backend.new_line_settings().expect("settings");
        settings
            .set_direction(LineDirection::Output)
            .expect("direction");
        settings.set_edge_detection(LineEdge::Both).expect("edge");
        settings.set_active_low(true);
        settings.set_debounce_period_us(100);
        settings
            .set_output_value(ValidLineValue::Active)
            .expect("output");
        assert_eq!(settings.get_direction(), LineDirection::Output);
        assert_eq!(settings.get_edge_detection(), LineEdge::Both);
        assert!(settings.get_active_low());
        assert_eq!(settings.get_debounce_period_us(), 100);
        assert_eq!(settings.get_output_value(), ValidLineValue::Active);
        settings.reset();
        assert_eq!(settings.get_direction(), LineDirection::AsIs);
    }

    #[test]
    fn line_config_tracks_offsets_and_rejects_unconfigured() {
        let backend = SysBackend::new();
        let mut config = backend.new_line_config().expect("config");
        assert_eq!(config.get_num_configured_offsets(), 0);
        assert!(matches!(
            config.get_line_settings(3),
            Err(GPIOError::UnconfiguredOffset { offset: 3 })
        ));

        let mut settings = backend.new_line_settings().expect("settings");
        settings
            .set_direction(LineDirection::Input)
            .expect("direction");
        config
            .add_line_settings(&[3, 5], &settings)
            .expect("add settings");
        assert_eq!(config.get_num_configured_offsets(), 2);
        let mut offsets = [0, 0];
        assert_eq!(config.get_configured_offsets(&mut offsets), 2);
        assert_eq!(offsets, [3, 5]);
        assert_eq!(
            config
                .get_line_settings(5)
                .expect("settings")
                .get_direction(),
            LineDirection::Input
        );
        assert!(matches!(
            config.set_output_values(&[ValidLineValue::Active]),
            Err(GPIOError::LengthMismatch {
                expected: 2,
                actual: 1
            })
        ));
    }

    #[test]
    fn request_config_consumer_and_buffer_size() {
        let backend = SysBackend::new();
        let mut config = backend.new_request_config().expect("request config");
        config.set_consumer("gpiojsonsvc");
        assert_eq!(config.get_consumer(), "gpiojsonsvc");
        config.set_event_buffer_size(32);
        assert_eq!(config.get_event_buffer_size(), 32);
    }

    #[test]
    fn edge_buffer_default_capacity_is_64() {
        let buffer = SysBackend::new().new_edge_event_buffer(0).expect("buffer");
        assert_eq!(buffer.get_capacity(), 64);
        assert_eq!(buffer.get_num_events(), 0);
        assert!(buffer.get_event(0).is_err());
    }

    #[test]
    fn empty_line_config_is_rejected_on_request() {
        let Some(path) = first_gpiochip() else {
            return;
        };
        let backend = SysBackend::new();
        let chip = backend.open_chip(&path).expect("open chip");
        let config = backend.new_line_config().expect("config");
        let error = chip.request_lines(None, &config).unwrap_err();
        assert!(matches!(error, GPIOError::EmptyLineConfig));
    }

    #[test]
    fn open_chip_and_info_when_gpiochip_present() {
        let Some(path) = first_gpiochip() else {
            return;
        };
        let backend = SysBackend::new();
        let chip = backend.open_chip(&path).expect("open chip");
        let info = chip.get_info().expect("chip info");
        assert!(!info.get_name().is_empty());
        assert!(info.get_num_lines() > 0);
        assert_eq!(chip.get_path(), path);
        assert!(chip.as_raw_fd() >= 0);
    }

    #[test]
    fn missing_line_name_maps_to_not_found() {
        let Some(path) = first_gpiochip() else {
            return;
        };
        let backend = SysBackend::new();
        let chip = backend.open_chip(&path).expect("open chip");
        let error = chip
            .get_line_offset_from_name("gpiojsonsvc-no-such-line")
            .unwrap_err();
        assert!(matches!(error, GPIOError::LineNameNotFound(_)));
    }

    #[test]
    fn open_chip_rejects_regular_file() {
        let file = NamedTempFile::new().expect("tempfile");
        fs::write(file.path(), b"not a chip").expect("write");
        let path = file.path().to_str().expect("utf8");
        assert!(SysBackend::new().open_chip(path).is_err());
    }
}
