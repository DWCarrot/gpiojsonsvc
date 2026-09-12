//! Line request and edge-event buffer wrappers.

use std::marker::PhantomData;
use std::os::fd::AsRawFd;
use std::os::fd::RawFd;
use std::ptr::NonNull;

use super::SysLineConfig;
use super::convert;
use super::ffi;
use crate::gpio::EdgeEvent;
use crate::gpio::EdgeEventBuffer;
use crate::gpio::EdgeEventType;
use crate::gpio::GPIOError;
use crate::gpio::LineRequest;
use crate::gpio::LineValue;
use crate::gpio::ValidLineValue;
use crate::gpio::WaitStatus;

/// Exclusive line request (`struct gpiod_line_request`).
#[derive(Debug)]
pub struct SysLineRequest {
    ptr: NonNull<ffi::gpiod_line_request>,
}

unsafe impl Send for SysLineRequest {}

impl SysLineRequest {
    pub(super) fn take(
        ptr: *mut ffi::gpiod_line_request,
        context: &str,
    ) -> Result<Self, GPIOError> {
        Ok(Self {
            ptr: convert::null_error(ptr, context)?,
        })
    }
}

impl Drop for SysLineRequest {
    fn drop(&mut self) {
        unsafe { ffi::gpiod_line_request_release(self.ptr.as_ptr()) }
    }
}

impl AsRawFd for SysLineRequest {
    fn as_raw_fd(&self) -> RawFd {
        unsafe { ffi::gpiod_line_request_get_fd(self.ptr.as_ptr()) }
    }
}

impl LineRequest for SysLineRequest {
    type LineConfig = SysLineConfig;
    type EdgeEventBuffer = SysEdgeEventBuffer;

    fn get_chip_name(&self) -> &str {
        unsafe { convert::cstr_from_ptr(ffi::gpiod_line_request_get_chip_name(self.ptr.as_ptr())) }
    }

    fn get_num_requested_lines(&self) -> usize {
        unsafe { ffi::gpiod_line_request_get_num_requested_lines(self.ptr.as_ptr()) }
    }

    fn get_requested_offsets(&self, out: &mut [u32]) -> usize {
        if out.is_empty() {
            return 0;
        }
        unsafe {
            ffi::gpiod_line_request_get_requested_offsets(
                self.ptr.as_ptr(),
                out.as_mut_ptr(),
                out.len(),
            )
        }
    }

    fn get_value(&self, offset: u32) -> Result<LineValue, GPIOError> {
        convert::require_inactive_or_active(
            convert::line_value_from_c(unsafe {
                ffi::gpiod_line_request_get_value(self.ptr.as_ptr(), offset)
            }),
            "gpiod_line_request_get_value",
        )
    }

    fn get_values_subset(
        &self,
        offsets: &[u32],
        values: &mut [LineValue],
    ) -> Result<(), GPIOError> {
        if offsets.len() != values.len() {
            return Err(GPIOError::LengthMismatch {
                expected: offsets.len(),
                actual: values.len(),
            });
        }
        if offsets.is_empty() {
            return Ok(());
        }
        convert::check_status(
            unsafe {
                ffi::gpiod_line_request_get_values_subset(
                    self.ptr.as_ptr(),
                    values.len(),
                    offsets.as_ptr(),
                    convert::line_values_as_c_mut(values),
                )
            },
            "gpiod_line_request_get_values_subset",
        )
    }

    fn get_values(&self, values: &mut [LineValue]) -> Result<(), GPIOError> {
        let expected = self.get_num_requested_lines();
        if values.len() != expected {
            return Err(GPIOError::LengthMismatch {
                expected,
                actual: values.len(),
            });
        }
        if values.is_empty() {
            return Ok(());
        }
        convert::check_status(
            unsafe {
                ffi::gpiod_line_request_get_values(
                    self.ptr.as_ptr(),
                    convert::line_values_as_c_mut(values),
                )
            },
            "gpiod_line_request_get_values",
        )
    }

    fn set_value(&self, offset: u32, value: ValidLineValue) -> Result<(), GPIOError> {
        convert::check_status(
            unsafe {
                ffi::gpiod_line_request_set_value(
                    self.ptr.as_ptr(),
                    offset,
                    convert::line_value_to_c(value),
                )
            },
            "gpiod_line_request_set_value",
        )
    }

    fn set_values_subset(
        &self,
        offsets: &[u32],
        values: &[ValidLineValue],
    ) -> Result<(), GPIOError> {
        if offsets.len() != values.len() {
            return Err(GPIOError::LengthMismatch {
                expected: offsets.len(),
                actual: values.len(),
            });
        }
        if offsets.is_empty() {
            return Ok(());
        }
        convert::check_status(
            unsafe {
                ffi::gpiod_line_request_set_values_subset(
                    self.ptr.as_ptr(),
                    values.len(),
                    offsets.as_ptr(),
                    convert::valid_line_values_as_c(values),
                )
            },
            "gpiod_line_request_set_values_subset",
        )
    }

    fn set_values(&self, values: &[ValidLineValue]) -> Result<(), GPIOError> {
        let expected = self.get_num_requested_lines();
        if values.len() != expected {
            return Err(GPIOError::LengthMismatch {
                expected,
                actual: values.len(),
            });
        }
        if values.is_empty() {
            return Ok(());
        }
        convert::check_status(
            unsafe {
                ffi::gpiod_line_request_set_values(
                    self.ptr.as_ptr(),
                    convert::valid_line_values_as_c(values),
                )
            },
            "gpiod_line_request_set_values",
        )
    }

    fn reconfigure_lines(&self, config: &Self::LineConfig) -> Result<(), GPIOError> {
        convert::check_status(
            unsafe {
                ffi::gpiod_line_request_reconfigure_lines(self.ptr.as_ptr(), config.as_ptr())
            },
            "gpiod_line_request_reconfigure_lines",
        )
    }

    fn wait_edge_events(&self, timeout_ns: Option<u64>) -> Result<WaitStatus, GPIOError> {
        convert::wait_status(
            unsafe {
                ffi::gpiod_line_request_wait_edge_events(
                    self.ptr.as_ptr(),
                    convert::timeout_ns_to_c(timeout_ns),
                )
            },
            "gpiod_line_request_wait_edge_events",
        )
    }

    fn read_edge_events(
        &self,
        buffer: &mut Self::EdgeEventBuffer,
        max_events: usize,
    ) -> Result<usize, GPIOError> {
        if max_events == 0 {
            buffer.cleared = false;
            return Ok(0);
        }
        if max_events > buffer.get_capacity() {
            return Err(GPIOError::InvalidArgument(format!(
                "max_events {max_events} exceeds edge buffer capacity {}",
                buffer.get_capacity()
            )));
        }
        let count = unsafe {
            ffi::gpiod_line_request_read_edge_events(self.ptr.as_ptr(), buffer.as_ptr(), max_events)
        };
        if count < 0 {
            return Err(convert::errno_error("gpiod_line_request_read_edge_events"));
        }
        buffer.cleared = false;
        Ok(count as usize)
    }
}

/// Userspace edge-event batch buffer (`struct gpiod_edge_event_buffer`).
#[derive(Debug)]
pub struct SysEdgeEventBuffer {
    ptr: NonNull<ffi::gpiod_edge_event_buffer>,
    cleared: bool,
}

unsafe impl Send for SysEdgeEventBuffer {}

impl SysEdgeEventBuffer {
    pub(super) fn new(capacity: usize) -> Result<Self, GPIOError> {
        let ptr = unsafe { ffi::gpiod_edge_event_buffer_new(capacity) };
        Ok(Self {
            ptr: convert::null_error(ptr, "gpiod_edge_event_buffer_new")?,
            cleared: false,
        })
    }

    fn as_ptr(&self) -> *mut ffi::gpiod_edge_event_buffer {
        self.ptr.as_ptr()
    }
}

impl Drop for SysEdgeEventBuffer {
    fn drop(&mut self) {
        unsafe { ffi::gpiod_edge_event_buffer_free(self.ptr.as_ptr()) }
    }
}

impl EdgeEventBuffer for SysEdgeEventBuffer {
    type EdgeEvent<'a>
        = SysEdgeEvent<'a>
    where
        Self: 'a;

    fn get_capacity(&self) -> usize {
        unsafe { ffi::gpiod_edge_event_buffer_get_capacity(self.ptr.as_ptr()) }
    }

    fn get_num_events(&self) -> usize {
        if self.cleared {
            0
        } else {
            unsafe { ffi::gpiod_edge_event_buffer_get_num_events(self.ptr.as_ptr()) }
        }
    }

    fn get_event(&self, index: usize) -> Result<Self::EdgeEvent<'_>, GPIOError> {
        let count = self.get_num_events();
        if index >= count {
            return Err(GPIOError::InvalidArgument(format!(
                "edge event index {index} out of range (len {count})"
            )));
        }
        let ptr = unsafe { ffi::gpiod_edge_event_buffer_get_event(self.ptr.as_ptr(), index as _) };
        let ptr = convert::null_error(ptr, "gpiod_edge_event_buffer_get_event")?;
        Ok(SysEdgeEvent {
            ptr,
            _buffer: PhantomData,
        })
    }

    fn clear(&mut self) {
        self.cleared = true;
    }
}

/// Borrowed edge event stored in [`SysEdgeEventBuffer`].
#[derive(Debug)]
pub struct SysEdgeEvent<'a> {
    ptr: NonNull<ffi::gpiod_edge_event>,
    _buffer: PhantomData<&'a SysEdgeEventBuffer>,
}

impl EdgeEvent for SysEdgeEvent<'_> {
    fn get_event_type(&self) -> EdgeEventType {
        convert::edge_event_type_from_c(unsafe {
            ffi::gpiod_edge_event_get_event_type(self.ptr.as_ptr())
        })
    }

    fn get_timestamp_ns(&self) -> u64 {
        unsafe { ffi::gpiod_edge_event_get_timestamp_ns(self.ptr.as_ptr()) }
    }

    fn get_line_offset(&self) -> u32 {
        unsafe { ffi::gpiod_edge_event_get_line_offset(self.ptr.as_ptr()) }
    }

    fn get_global_seqno(&self) -> u64 {
        unsafe { ffi::gpiod_edge_event_get_global_seqno(self.ptr.as_ptr()) as u64 }
    }

    fn get_line_seqno(&self) -> u64 {
        unsafe { ffi::gpiod_edge_event_get_line_seqno(self.ptr.as_ptr()) as u64 }
    }
}
