//! Chip, chip-info, line-info, and info-event wrappers.

use std::os::fd::AsRawFd;
use std::os::fd::RawFd;
use std::ptr::NonNull;

use super::SysEdgeEventBuffer;
use super::SysLineConfig;
use super::SysLineSettings;
use super::SysRequestConfig;
use super::convert;
use super::ffi;
use crate::gpio::Chip;
use crate::gpio::ChipInfo;
use crate::gpio::GPIOError;
use crate::gpio::InfoEvent;
use crate::gpio::InfoEventType;
use crate::gpio::LineBias;
use crate::gpio::LineClock;
use crate::gpio::LineConfig;
use crate::gpio::LineDirection;
use crate::gpio::LineDrive;
use crate::gpio::LineEdge;
use crate::gpio::LineInfo;
use crate::gpio::WaitStatus;

use super::SysLineRequest;

/// Open GPIO character device (`struct gpiod_chip`).
#[derive(Debug)]
pub struct SysChip {
    ptr: NonNull<ffi::gpiod_chip>,
}

unsafe impl Send for SysChip {}

impl SysChip {
    pub(super) fn take(ptr: *mut ffi::gpiod_chip, context: &str) -> Result<Self, GPIOError> {
        Ok(Self {
            ptr: convert::null_error(ptr, context)?,
        })
    }
}

impl Drop for SysChip {
    fn drop(&mut self) {
        unsafe { ffi::gpiod_chip_close(self.ptr.as_ptr()) }
    }
}

impl AsRawFd for SysChip {
    fn as_raw_fd(&self) -> RawFd {
        unsafe { ffi::gpiod_chip_get_fd(self.ptr.as_ptr()) }
    }
}

impl Chip for SysChip {
    type ChipInfoOwned = SysChipInfo;
    type LineInfoOwned = SysLineInfo;
    type InfoEventOwned = SysInfoEvent;
    type LineRequestOwned = SysLineRequest;
    type LineSettings = SysLineSettings;
    type LineConfig = SysLineConfig;
    type RequestConfig = SysRequestConfig;
    type EdgeEventBuffer = SysEdgeEventBuffer;

    fn get_info(&self) -> Result<Self::ChipInfoOwned, GPIOError> {
        let ptr = unsafe { ffi::gpiod_chip_get_info(self.ptr.as_ptr()) };
        SysChipInfo::take(ptr, "gpiod_chip_get_info")
    }

    fn get_path(&self) -> &str {
        unsafe { convert::cstr_from_ptr(ffi::gpiod_chip_get_path(self.ptr.as_ptr())) }
    }

    fn get_line_info(&self, offset: u32) -> Result<Self::LineInfoOwned, GPIOError> {
        let ptr = unsafe { ffi::gpiod_chip_get_line_info(self.ptr.as_ptr(), offset) };
        match SysLineInfo::take(ptr, "gpiod_chip_get_line_info") {
            Ok(info) => Ok(info),
            Err(_) if nix::errno::Errno::last() == nix::errno::Errno::EINVAL => {
                Err(GPIOError::InvalidOffset(offset))
            }
            Err(error) => Err(error),
        }
    }

    fn watch_line_info(&self, offset: u32) -> Result<Self::LineInfoOwned, GPIOError> {
        let ptr = unsafe { ffi::gpiod_chip_watch_line_info(self.ptr.as_ptr(), offset) };
        SysLineInfo::take(ptr, "gpiod_chip_watch_line_info")
    }

    fn unwatch_line_info(&self, offset: u32) -> Result<(), GPIOError> {
        convert::check_status(
            unsafe { ffi::gpiod_chip_unwatch_line_info(self.ptr.as_ptr(), offset) },
            "gpiod_chip_unwatch_line_info",
        )
    }

    fn wait_info_event(&self, timeout_ns: Option<u64>) -> Result<WaitStatus, GPIOError> {
        convert::wait_status(
            unsafe {
                ffi::gpiod_chip_wait_info_event(
                    self.ptr.as_ptr(),
                    convert::timeout_ns_to_c(timeout_ns),
                )
            },
            "gpiod_chip_wait_info_event",
        )
    }

    fn read_info_event(&self) -> Result<Self::InfoEventOwned, GPIOError> {
        let ptr = unsafe { ffi::gpiod_chip_read_info_event(self.ptr.as_ptr()) };
        SysInfoEvent::take(ptr, "gpiod_chip_read_info_event")
    }

    fn get_line_offset_from_name(&self, name: &str) -> Result<u32, GPIOError> {
        let c_name = convert::cstring(name, "line name")?;
        let offset = unsafe {
            ffi::gpiod_chip_get_line_offset_from_name(self.ptr.as_ptr(), c_name.as_ptr())
        };
        if offset < 0 {
            return Err(convert::line_name_not_found_or_errno(
                name,
                "gpiod_chip_get_line_offset_from_name",
            ));
        }
        Ok(offset as u32)
    }

    fn request_lines(
        &self,
        req_cfg: Option<&Self::RequestConfig>,
        line_cfg: &Self::LineConfig,
    ) -> Result<Self::LineRequestOwned, GPIOError> {
        if line_cfg.get_num_configured_offsets() == 0 {
            return Err(GPIOError::EmptyLineConfig);
        }
        let req_ptr = req_cfg
            .map(SysRequestConfig::as_ptr)
            .unwrap_or(std::ptr::null_mut());
        let ptr =
            unsafe { ffi::gpiod_chip_request_lines(self.ptr.as_ptr(), req_ptr, line_cfg.as_ptr()) };
        SysLineRequest::take(ptr, "gpiod_chip_request_lines")
    }
}

/// Chip metadata snapshot (`struct gpiod_chip_info`).
#[derive(Debug)]
pub struct SysChipInfo {
    ptr: NonNull<ffi::gpiod_chip_info>,
}

unsafe impl Send for SysChipInfo {}

impl SysChipInfo {
    fn take(ptr: *mut ffi::gpiod_chip_info, context: &str) -> Result<Self, GPIOError> {
        Ok(Self {
            ptr: convert::null_error(ptr, context)?,
        })
    }
}

impl Drop for SysChipInfo {
    fn drop(&mut self) {
        unsafe { ffi::gpiod_chip_info_free(self.ptr.as_ptr()) }
    }
}

impl ChipInfo for SysChipInfo {
    fn get_name(&self) -> &str {
        unsafe { convert::cstr_from_ptr(ffi::gpiod_chip_info_get_name(self.ptr.as_ptr())) }
    }

    fn get_label(&self) -> &str {
        unsafe { convert::cstr_from_ptr(ffi::gpiod_chip_info_get_label(self.ptr.as_ptr())) }
    }

    fn get_num_lines(&self) -> usize {
        unsafe { ffi::gpiod_chip_info_get_num_lines(self.ptr.as_ptr()) }
    }
}

/// Line status snapshot (`struct gpiod_line_info`).
#[derive(Debug)]
pub struct SysLineInfo {
    ptr: NonNull<ffi::gpiod_line_info>,
}

unsafe impl Send for SysLineInfo {}

impl SysLineInfo {
    fn take(ptr: *mut ffi::gpiod_line_info, context: &str) -> Result<Self, GPIOError> {
        Ok(Self {
            ptr: convert::null_error(ptr, context)?,
        })
    }
}

impl Drop for SysLineInfo {
    fn drop(&mut self) {
        unsafe { ffi::gpiod_line_info_free(self.ptr.as_ptr()) }
    }
}

impl LineInfo for SysLineInfo {
    fn get_offset(&self) -> u32 {
        unsafe { ffi::gpiod_line_info_get_offset(self.ptr.as_ptr()) }
    }

    fn get_name(&self) -> Option<&str> {
        unsafe { convert::optional_cstr(ffi::gpiod_line_info_get_name(self.ptr.as_ptr())) }
    }

    fn is_used(&self) -> bool {
        unsafe { ffi::gpiod_line_info_is_used(self.ptr.as_ptr()) }
    }

    fn get_consumer(&self) -> Option<&str> {
        unsafe { convert::optional_cstr(ffi::gpiod_line_info_get_consumer(self.ptr.as_ptr())) }
    }

    fn get_direction(&self) -> LineDirection {
        convert::line_direction_from_c(unsafe {
            ffi::gpiod_line_info_get_direction(self.ptr.as_ptr())
        })
    }

    fn get_edge_detection(&self) -> LineEdge {
        convert::line_edge_from_c(unsafe {
            ffi::gpiod_line_info_get_edge_detection(self.ptr.as_ptr())
        })
    }

    fn get_bias(&self) -> LineBias {
        convert::line_bias_from_c(unsafe { ffi::gpiod_line_info_get_bias(self.ptr.as_ptr()) })
    }

    fn get_drive(&self) -> LineDrive {
        convert::line_drive_from_c(unsafe { ffi::gpiod_line_info_get_drive(self.ptr.as_ptr()) })
    }

    fn is_active_low(&self) -> bool {
        unsafe { ffi::gpiod_line_info_is_active_low(self.ptr.as_ptr()) }
    }

    fn is_debounced(&self) -> bool {
        unsafe { ffi::gpiod_line_info_is_debounced(self.ptr.as_ptr()) }
    }

    fn get_debounce_period_us(&self) -> u64 {
        unsafe { ffi::gpiod_line_info_get_debounce_period_us(self.ptr.as_ptr()) as u64 }
    }

    fn get_event_clock(&self) -> LineClock {
        convert::line_clock_from_c(unsafe {
            ffi::gpiod_line_info_get_event_clock(self.ptr.as_ptr())
        })
    }
}

/// Line status change event (`struct gpiod_info_event`).
#[derive(Debug)]
pub struct SysInfoEvent {
    ptr: NonNull<ffi::gpiod_info_event>,
}

unsafe impl Send for SysInfoEvent {}

impl SysInfoEvent {
    fn take(ptr: *mut ffi::gpiod_info_event, context: &str) -> Result<Self, GPIOError> {
        Ok(Self {
            ptr: convert::null_error(ptr, context)?,
        })
    }
}

impl Drop for SysInfoEvent {
    fn drop(&mut self) {
        unsafe { ffi::gpiod_info_event_free(self.ptr.as_ptr()) }
    }
}

impl InfoEvent for SysInfoEvent {
    type LineInfo<'a>
        = SysLineInfo
    where
        Self: 'a;

    fn get_event_type(&self) -> InfoEventType {
        convert::info_event_type_from_c(unsafe {
            ffi::gpiod_info_event_get_event_type(self.ptr.as_ptr())
        })
    }

    fn get_timestamp_ns(&self) -> u64 {
        unsafe { ffi::gpiod_info_event_get_timestamp_ns(self.ptr.as_ptr()) }
    }

    fn get_line_info(&self) -> Self::LineInfo<'_> {
        let borrowed = unsafe { ffi::gpiod_info_event_get_line_info(self.ptr.as_ptr()) };
        let copied = unsafe { ffi::gpiod_line_info_copy(borrowed) };
        SysLineInfo::take(copied, "gpiod_line_info_copy")
            .expect("gpiod_line_info_copy should succeed for a valid info event")
    }
}
