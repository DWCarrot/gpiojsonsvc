//! Line settings, line config, and request config wrappers.

use std::ptr::NonNull;

use super::convert;
use super::ffi;
use crate::gpio::GPIOError;
use crate::gpio::LineBias;
use crate::gpio::LineClock;
use crate::gpio::LineConfig;
use crate::gpio::LineDirection;
use crate::gpio::LineDrive;
use crate::gpio::LineEdge;
use crate::gpio::LineSettings;
use crate::gpio::LineValue;
use crate::gpio::RequestConfig;
use crate::gpio::ValidLineValue;

/// Per-line request settings (`struct gpiod_line_settings`).
#[derive(Debug)]
pub struct SysLineSettings {
    ptr: NonNull<ffi::gpiod_line_settings>,
}

unsafe impl Send for SysLineSettings {}

impl SysLineSettings {
    pub(super) fn new() -> Result<Self, GPIOError> {
        let ptr = unsafe { ffi::gpiod_line_settings_new() };
        Self::take(ptr, "gpiod_line_settings_new")
    }

    fn take(ptr: *mut ffi::gpiod_line_settings, context: &str) -> Result<Self, GPIOError> {
        Ok(Self {
            ptr: convert::null_error(ptr, context)?,
        })
    }

    pub(super) fn as_ptr(&self) -> *mut ffi::gpiod_line_settings {
        self.ptr.as_ptr()
    }
}

impl Drop for SysLineSettings {
    fn drop(&mut self) {
        unsafe { ffi::gpiod_line_settings_free(self.ptr.as_ptr()) }
    }
}

impl LineSettings for SysLineSettings {
    fn reset(&mut self) {
        unsafe { ffi::gpiod_line_settings_reset(self.ptr.as_ptr()) }
    }

    fn get_direction(&self) -> LineDirection {
        convert::line_direction_from_c(unsafe {
            ffi::gpiod_line_settings_get_direction(self.ptr.as_ptr())
        })
    }

    fn set_direction(&mut self, direction: LineDirection) -> Result<(), GPIOError> {
        convert::check_status(
            unsafe {
                ffi::gpiod_line_settings_set_direction(
                    self.ptr.as_ptr(),
                    convert::line_direction_to_c(direction),
                )
            },
            "gpiod_line_settings_set_direction",
        )
    }

    fn get_edge_detection(&self) -> LineEdge {
        convert::line_edge_from_c(unsafe {
            ffi::gpiod_line_settings_get_edge_detection(self.ptr.as_ptr())
        })
    }

    fn set_edge_detection(&mut self, edge: LineEdge) -> Result<(), GPIOError> {
        convert::check_status(
            unsafe {
                ffi::gpiod_line_settings_set_edge_detection(
                    self.ptr.as_ptr(),
                    convert::line_edge_to_c(edge),
                )
            },
            "gpiod_line_settings_set_edge_detection",
        )
    }

    fn get_bias(&self) -> LineBias {
        convert::line_bias_from_c(unsafe { ffi::gpiod_line_settings_get_bias(self.ptr.as_ptr()) })
    }

    fn set_bias(&mut self, bias: LineBias) -> Result<(), GPIOError> {
        convert::check_status(
            unsafe {
                ffi::gpiod_line_settings_set_bias(self.ptr.as_ptr(), convert::line_bias_to_c(bias))
            },
            "gpiod_line_settings_set_bias",
        )
    }

    fn get_drive(&self) -> LineDrive {
        convert::line_drive_from_c(unsafe { ffi::gpiod_line_settings_get_drive(self.ptr.as_ptr()) })
    }

    fn set_drive(&mut self, drive: LineDrive) -> Result<(), GPIOError> {
        convert::check_status(
            unsafe {
                ffi::gpiod_line_settings_set_drive(
                    self.ptr.as_ptr(),
                    convert::line_drive_to_c(drive),
                )
            },
            "gpiod_line_settings_set_drive",
        )
    }

    fn get_active_low(&self) -> bool {
        unsafe { ffi::gpiod_line_settings_get_active_low(self.ptr.as_ptr()) }
    }

    fn set_active_low(&mut self, active_low: bool) {
        unsafe { ffi::gpiod_line_settings_set_active_low(self.ptr.as_ptr(), active_low) }
    }

    fn get_debounce_period_us(&self) -> u64 {
        unsafe { ffi::gpiod_line_settings_get_debounce_period_us(self.ptr.as_ptr()) as u64 }
    }

    fn set_debounce_period_us(&mut self, period_us: u64) {
        unsafe {
            ffi::gpiod_line_settings_set_debounce_period_us(self.ptr.as_ptr(), period_us as _)
        }
    }

    fn get_event_clock(&self) -> LineClock {
        convert::line_clock_from_c(unsafe {
            ffi::gpiod_line_settings_get_event_clock(self.ptr.as_ptr())
        })
    }

    fn set_event_clock(&mut self, clock: LineClock) -> Result<(), GPIOError> {
        convert::check_status(
            unsafe {
                ffi::gpiod_line_settings_set_event_clock(
                    self.ptr.as_ptr(),
                    convert::line_clock_to_c(clock),
                )
            },
            "gpiod_line_settings_set_event_clock",
        )
    }

    fn get_output_value(&self) -> LineValue {
        convert::line_value_from_c(unsafe {
            ffi::gpiod_line_settings_get_output_value(self.ptr.as_ptr())
        })
    }

    fn set_output_value(&mut self, value: ValidLineValue) -> Result<(), GPIOError> {
        convert::check_status(
            unsafe {
                ffi::gpiod_line_settings_set_output_value(
                    self.ptr.as_ptr(),
                    convert::line_value_to_c(value),
                )
            },
            "gpiod_line_settings_set_output_value",
        )
    }
}

/// Offset-to-settings mapping (`struct gpiod_line_config`).
#[derive(Debug)]
pub struct SysLineConfig {
    ptr: NonNull<ffi::gpiod_line_config>,
}

unsafe impl Send for SysLineConfig {}

impl SysLineConfig {
    pub(super) fn new() -> Result<Self, GPIOError> {
        let ptr = unsafe { ffi::gpiod_line_config_new() };
        Self::take(ptr, "gpiod_line_config_new")
    }

    fn take(ptr: *mut ffi::gpiod_line_config, context: &str) -> Result<Self, GPIOError> {
        Ok(Self {
            ptr: convert::null_error(ptr, context)?,
        })
    }

    pub(super) fn as_ptr(&self) -> *mut ffi::gpiod_line_config {
        self.ptr.as_ptr()
    }
}

impl Drop for SysLineConfig {
    fn drop(&mut self) {
        unsafe { ffi::gpiod_line_config_free(self.ptr.as_ptr()) }
    }
}

impl LineConfig for SysLineConfig {
    type LineSettings = SysLineSettings;

    fn reset(&mut self) {
        unsafe { ffi::gpiod_line_config_reset(self.ptr.as_ptr()) }
    }

    fn add_line_settings(
        &mut self,
        offsets: &[u32],
        settings: &Self::LineSettings,
    ) -> Result<(), GPIOError> {
        if offsets.is_empty() {
            return Ok(());
        }
        convert::check_status(
            unsafe {
                ffi::gpiod_line_config_add_line_settings(
                    self.ptr.as_ptr(),
                    offsets.as_ptr(),
                    offsets.len(),
                    settings.as_ptr(),
                )
            },
            "gpiod_line_config_add_line_settings",
        )
    }

    fn get_line_settings(&self, offset: u32) -> Result<Self::LineSettings, GPIOError> {
        let ptr = unsafe { ffi::gpiod_line_config_get_line_settings(self.ptr.as_ptr(), offset) };
        SysLineSettings::take(ptr, "gpiod_line_config_get_line_settings")
            .map_err(|_| GPIOError::UnconfiguredOffset { offset })
    }

    fn set_output_values(&mut self, values: &[ValidLineValue]) -> Result<(), GPIOError> {
        let expected = self.get_num_configured_offsets();
        if values.len() != expected {
            return Err(GPIOError::LengthMismatch {
                expected,
                actual: values.len(),
            });
        }
        convert::check_status(
            unsafe {
                ffi::gpiod_line_config_set_output_values(
                    self.ptr.as_ptr(),
                    convert::valid_line_values_as_c(values),
                    values.len(),
                )
            },
            "gpiod_line_config_set_output_values",
        )
    }

    fn get_num_configured_offsets(&self) -> usize {
        unsafe { ffi::gpiod_line_config_get_num_configured_offsets(self.ptr.as_ptr()) }
    }

    fn get_configured_offsets(&self, out: &mut [u32]) -> usize {
        if out.is_empty() {
            return 0;
        }
        unsafe {
            ffi::gpiod_line_config_get_configured_offsets(
                self.ptr.as_ptr(),
                out.as_mut_ptr(),
                out.len(),
            )
        }
    }
}

/// Request-time kernel options (`struct gpiod_request_config`).
#[derive(Debug)]
pub struct SysRequestConfig {
    ptr: NonNull<ffi::gpiod_request_config>,
}

unsafe impl Send for SysRequestConfig {}

impl SysRequestConfig {
    pub(super) fn new() -> Result<Self, GPIOError> {
        let ptr = unsafe { ffi::gpiod_request_config_new() };
        Self::take(ptr, "gpiod_request_config_new")
    }

    fn take(ptr: *mut ffi::gpiod_request_config, context: &str) -> Result<Self, GPIOError> {
        Ok(Self {
            ptr: convert::null_error(ptr, context)?,
        })
    }

    pub(super) fn as_ptr(&self) -> *mut ffi::gpiod_request_config {
        self.ptr.as_ptr()
    }
}

impl Drop for SysRequestConfig {
    fn drop(&mut self) {
        unsafe { ffi::gpiod_request_config_free(self.ptr.as_ptr()) }
    }
}

impl RequestConfig for SysRequestConfig {
    fn set_consumer(&mut self, consumer: &str) {
        match convert::cstring(consumer, "consumer") {
            Ok(c_consumer) => unsafe {
                ffi::gpiod_request_config_set_consumer(self.ptr.as_ptr(), c_consumer.as_ptr())
            },
            Err(_) => unsafe {
                ffi::gpiod_request_config_set_consumer(self.ptr.as_ptr(), c"".as_ptr())
            },
        }
    }

    fn get_consumer(&self) -> &str {
        unsafe { convert::cstr_from_ptr(ffi::gpiod_request_config_get_consumer(self.ptr.as_ptr())) }
    }

    fn set_event_buffer_size(&mut self, event_buffer_size: usize) {
        unsafe {
            ffi::gpiod_request_config_set_event_buffer_size(self.ptr.as_ptr(), event_buffer_size)
        }
    }

    fn get_event_buffer_size(&self) -> usize {
        unsafe { ffi::gpiod_request_config_get_event_buffer_size(self.ptr.as_ptr()) }
    }
}
