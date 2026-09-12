//! C ABI conversions and errno mapping for the libgpiod v2 backend.

use std::ffi::CStr;
use std::ffi::CString;
use std::mem::align_of;
use std::mem::size_of;
use std::os::raw::c_char;
use std::os::raw::c_int;
use std::ptr::NonNull;

use nix::errno::Errno;

use super::ffi;
use crate::gpio::EdgeEventType;
use crate::gpio::GPIOError;
use crate::gpio::InfoEventType;
use crate::gpio::LineBias;
use crate::gpio::LineClock;
use crate::gpio::LineDirection;
use crate::gpio::LineDrive;
use crate::gpio::LineEdge;
use crate::gpio::LineValue;
use crate::gpio::ValidLineValue;
use crate::gpio::WaitStatus;

const _: () = {
    assert!(size_of::<LineValue>() == size_of::<ffi::gpiod_line_value>());
    assert!(align_of::<LineValue>() == align_of::<ffi::gpiod_line_value>());
    assert!(size_of::<ValidLineValue>() == size_of::<ffi::gpiod_line_value>());
    assert!(align_of::<ValidLineValue>() == align_of::<ffi::gpiod_line_value>());
    assert!(LineValue::ERROR.as_raw() == ffi::gpiod_line_value_GPIOD_LINE_VALUE_ERROR);
    assert!(LineValue::INACTIVE.as_raw() == ffi::gpiod_line_value_GPIOD_LINE_VALUE_INACTIVE);
    assert!(LineValue::ACTIVE.as_raw() == ffi::gpiod_line_value_GPIOD_LINE_VALUE_ACTIVE);
    assert!(
        ValidLineValue::Inactive as ffi::gpiod_line_value
            == ffi::gpiod_line_value_GPIOD_LINE_VALUE_INACTIVE
    );
    assert!(
        ValidLineValue::Active as ffi::gpiod_line_value
            == ffi::gpiod_line_value_GPIOD_LINE_VALUE_ACTIVE
    );
    assert!(ValidLineValue::Inactive as ffi::gpiod_line_value == LineValue::INACTIVE.as_raw());
    assert!(ValidLineValue::Active as ffi::gpiod_line_value == LineValue::ACTIVE.as_raw());
};

pub(super) fn errno_error(context: &str) -> GPIOError {
    GPIOError::Other(format!("{context}: {}", std::io::Error::last_os_error()))
}

pub(super) fn null_error<T>(ptr: *mut T, context: &str) -> Result<NonNull<T>, GPIOError> {
    NonNull::new(ptr).ok_or_else(|| errno_error(context))
}

pub(super) fn check_status(ret: c_int, context: &str) -> Result<(), GPIOError> {
    if ret == 0 {
        Ok(())
    } else {
        Err(errno_error(context))
    }
}

pub(super) fn wait_status(ret: c_int, context: &str) -> Result<WaitStatus, GPIOError> {
    match ret {
        0 => Ok(WaitStatus::Timeout),
        1 => Ok(WaitStatus::EventPending),
        _ => Err(errno_error(context)),
    }
}

pub(super) fn timeout_ns_to_c(timeout_ns: Option<u64>) -> i64 {
    match timeout_ns {
        None => -1,
        Some(ns) => i64::try_from(ns).unwrap_or(i64::MAX),
    }
}

pub(super) fn cstring(value: &str, what: &str) -> Result<CString, GPIOError> {
    CString::new(value)
        .map_err(|_| GPIOError::InvalidArgument(format!("{what} contains an interior NUL byte")))
}

/// # Safety
/// `ptr` must be null or a valid C string that outlives `'a`.
pub(super) unsafe fn cstr_from_ptr<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        return "";
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().unwrap_or("")
}

/// # Safety
/// `ptr` must be null or a valid C string that outlives `'a`.
pub(super) unsafe fn optional_cstr<'a>(ptr: *const c_char) -> Option<&'a str> {
    let value = unsafe { cstr_from_ptr(ptr) };
    if value.is_empty() { None } else { Some(value) }
}

pub(super) fn line_name_not_found_or_errno(name: &str, context: &str) -> GPIOError {
    if Errno::last() == Errno::ENOENT {
        GPIOError::LineNameNotFound(name.to_owned())
    } else {
        errno_error(context)
    }
}

pub(super) fn line_value_to_c(value: ValidLineValue) -> ffi::gpiod_line_value {
    value as ffi::gpiod_line_value
}

pub(super) fn line_value_from_c(value: ffi::gpiod_line_value) -> LineValue {
    LineValue::from_raw(value)
}

pub(super) fn require_inactive_or_active(
    value: LineValue,
    context: &str,
) -> Result<LineValue, GPIOError> {
    if value.is_error() {
        return Err(errno_error(context));
    }
    let _ = ValidLineValue::try_from(value)?;
    Ok(value)
}

/// Borrow [`ValidLineValue`] bits as a C `gpiod_line_value` array.
pub(super) fn valid_line_values_as_c(values: &[ValidLineValue]) -> *const ffi::gpiod_line_value {
    values.as_ptr().cast()
}

/// Borrow a [`LineValue`] buffer as a C `gpiod_line_value` array for get APIs.
pub(super) fn line_values_as_c_mut(values: &mut [LineValue]) -> *mut ffi::gpiod_line_value {
    values.as_mut_ptr().cast()
}

pub(super) fn line_direction_to_c(value: LineDirection) -> ffi::gpiod_line_direction {
    match value {
        LineDirection::AsIs => ffi::gpiod_line_direction_GPIOD_LINE_DIRECTION_AS_IS,
        LineDirection::Input => ffi::gpiod_line_direction_GPIOD_LINE_DIRECTION_INPUT,
        LineDirection::Output => ffi::gpiod_line_direction_GPIOD_LINE_DIRECTION_OUTPUT,
    }
}

pub(super) fn line_direction_from_c(value: ffi::gpiod_line_direction) -> LineDirection {
    match value {
        ffi::gpiod_line_direction_GPIOD_LINE_DIRECTION_AS_IS => LineDirection::AsIs,
        ffi::gpiod_line_direction_GPIOD_LINE_DIRECTION_INPUT => LineDirection::Input,
        ffi::gpiod_line_direction_GPIOD_LINE_DIRECTION_OUTPUT => LineDirection::Output,
        _ => LineDirection::AsIs,
    }
}

pub(super) fn line_edge_to_c(value: LineEdge) -> ffi::gpiod_line_edge {
    match value {
        LineEdge::None => ffi::gpiod_line_edge_GPIOD_LINE_EDGE_NONE,
        LineEdge::Rising => ffi::gpiod_line_edge_GPIOD_LINE_EDGE_RISING,
        LineEdge::Falling => ffi::gpiod_line_edge_GPIOD_LINE_EDGE_FALLING,
        LineEdge::Both => ffi::gpiod_line_edge_GPIOD_LINE_EDGE_BOTH,
    }
}

pub(super) fn line_edge_from_c(value: ffi::gpiod_line_edge) -> LineEdge {
    match value {
        ffi::gpiod_line_edge_GPIOD_LINE_EDGE_NONE => LineEdge::None,
        ffi::gpiod_line_edge_GPIOD_LINE_EDGE_RISING => LineEdge::Rising,
        ffi::gpiod_line_edge_GPIOD_LINE_EDGE_FALLING => LineEdge::Falling,
        ffi::gpiod_line_edge_GPIOD_LINE_EDGE_BOTH => LineEdge::Both,
        _ => LineEdge::None,
    }
}

pub(super) fn line_bias_to_c(value: LineBias) -> ffi::gpiod_line_bias {
    match value {
        LineBias::AsIs => ffi::gpiod_line_bias_GPIOD_LINE_BIAS_AS_IS,
        LineBias::Unknown => ffi::gpiod_line_bias_GPIOD_LINE_BIAS_UNKNOWN,
        LineBias::Disabled => ffi::gpiod_line_bias_GPIOD_LINE_BIAS_DISABLED,
        LineBias::PullUp => ffi::gpiod_line_bias_GPIOD_LINE_BIAS_PULL_UP,
        LineBias::PullDown => ffi::gpiod_line_bias_GPIOD_LINE_BIAS_PULL_DOWN,
    }
}

pub(super) fn line_bias_from_c(value: ffi::gpiod_line_bias) -> LineBias {
    match value {
        ffi::gpiod_line_bias_GPIOD_LINE_BIAS_AS_IS => LineBias::AsIs,
        ffi::gpiod_line_bias_GPIOD_LINE_BIAS_UNKNOWN => LineBias::Unknown,
        ffi::gpiod_line_bias_GPIOD_LINE_BIAS_DISABLED => LineBias::Disabled,
        ffi::gpiod_line_bias_GPIOD_LINE_BIAS_PULL_UP => LineBias::PullUp,
        ffi::gpiod_line_bias_GPIOD_LINE_BIAS_PULL_DOWN => LineBias::PullDown,
        _ => LineBias::Unknown,
    }
}

pub(super) fn line_drive_to_c(value: LineDrive) -> ffi::gpiod_line_drive {
    match value {
        LineDrive::PushPull => ffi::gpiod_line_drive_GPIOD_LINE_DRIVE_PUSH_PULL,
        LineDrive::OpenDrain => ffi::gpiod_line_drive_GPIOD_LINE_DRIVE_OPEN_DRAIN,
        LineDrive::OpenSource => ffi::gpiod_line_drive_GPIOD_LINE_DRIVE_OPEN_SOURCE,
    }
}

pub(super) fn line_drive_from_c(value: ffi::gpiod_line_drive) -> LineDrive {
    match value {
        ffi::gpiod_line_drive_GPIOD_LINE_DRIVE_PUSH_PULL => LineDrive::PushPull,
        ffi::gpiod_line_drive_GPIOD_LINE_DRIVE_OPEN_DRAIN => LineDrive::OpenDrain,
        ffi::gpiod_line_drive_GPIOD_LINE_DRIVE_OPEN_SOURCE => LineDrive::OpenSource,
        _ => LineDrive::PushPull,
    }
}

pub(super) fn line_clock_to_c(value: LineClock) -> ffi::gpiod_line_clock {
    match value {
        LineClock::Monotonic => ffi::gpiod_line_clock_GPIOD_LINE_CLOCK_MONOTONIC,
        LineClock::Realtime => ffi::gpiod_line_clock_GPIOD_LINE_CLOCK_REALTIME,
        LineClock::Hte => ffi::gpiod_line_clock_GPIOD_LINE_CLOCK_HTE,
    }
}

pub(super) fn line_clock_from_c(value: ffi::gpiod_line_clock) -> LineClock {
    match value {
        ffi::gpiod_line_clock_GPIOD_LINE_CLOCK_MONOTONIC => LineClock::Monotonic,
        ffi::gpiod_line_clock_GPIOD_LINE_CLOCK_REALTIME => LineClock::Realtime,
        ffi::gpiod_line_clock_GPIOD_LINE_CLOCK_HTE => LineClock::Hte,
        _ => LineClock::Monotonic,
    }
}

pub(super) fn info_event_type_from_c(value: ffi::gpiod_info_event_type) -> InfoEventType {
    match value {
        ffi::gpiod_info_event_type_GPIOD_INFO_EVENT_LINE_REQUESTED => InfoEventType::LineRequested,
        ffi::gpiod_info_event_type_GPIOD_INFO_EVENT_LINE_RELEASED => InfoEventType::LineReleased,
        ffi::gpiod_info_event_type_GPIOD_INFO_EVENT_LINE_CONFIG_CHANGED => {
            InfoEventType::LineConfigChanged
        }
        _ => InfoEventType::LineConfigChanged,
    }
}

pub(super) fn edge_event_type_from_c(value: ffi::gpiod_edge_event_type) -> EdgeEventType {
    match value {
        ffi::gpiod_edge_event_type_GPIOD_EDGE_EVENT_RISING_EDGE => EdgeEventType::RisingEdge,
        ffi::gpiod_edge_event_type_GPIOD_EDGE_EVENT_FALLING_EDGE => EdgeEventType::FallingEdge,
        _ => EdgeEventType::RisingEdge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_status_maps_c_return_codes() {
        assert_eq!(wait_status(0, "wait").unwrap(), WaitStatus::Timeout);
        assert_eq!(wait_status(1, "wait").unwrap(), WaitStatus::EventPending);
        assert!(wait_status(-1, "wait").is_err());
    }

    #[test]
    fn timeout_none_is_negative_one() {
        assert_eq!(timeout_ns_to_c(None), -1);
        assert_eq!(timeout_ns_to_c(Some(0)), 0);
        assert_eq!(timeout_ns_to_c(Some(42)), 42);
    }

    #[test]
    fn cstring_rejects_interior_nul() {
        let error = cstring("a\0b", "name").unwrap_err();
        assert!(matches!(error, GPIOError::InvalidArgument(_)));
    }

    #[test]
    fn line_value_round_trips() {
        for value in [ValidLineValue::Inactive, ValidLineValue::Active] {
            assert_eq!(
                line_value_from_c(line_value_to_c(value)),
                LineValue::from(value)
            );
        }
        assert_eq!(
            line_value_from_c(ffi::gpiod_line_value_GPIOD_LINE_VALUE_ERROR),
            LineValue::ERROR
        );
        assert_eq!(line_value_from_c(2), LineValue::from_raw(2));
        assert!(require_inactive_or_active(LineValue::ERROR, "test").is_err());
        assert!(matches!(
            require_inactive_or_active(LineValue::from_raw(2), "test"),
            Err(GPIOError::InvalidLineValue(2))
        ));
        assert_eq!(
            require_inactive_or_active(LineValue::ACTIVE, "test").unwrap(),
            ValidLineValue::Active
        );
    }

    #[test]
    fn line_value_layout_matches_libgpiod_c_abi() {
        assert_eq!(size_of::<LineValue>(), size_of::<ffi::gpiod_line_value>());
        assert_eq!(align_of::<LineValue>(), align_of::<ffi::gpiod_line_value>());
        assert_eq!(
            size_of::<ValidLineValue>(),
            size_of::<ffi::gpiod_line_value>()
        );
        assert_eq!(
            align_of::<ValidLineValue>(),
            align_of::<ffi::gpiod_line_value>()
        );
        assert_eq!(
            LineValue::INACTIVE.as_raw(),
            ffi::gpiod_line_value_GPIOD_LINE_VALUE_INACTIVE
        );
        assert_eq!(
            LineValue::ACTIVE.as_raw(),
            ffi::gpiod_line_value_GPIOD_LINE_VALUE_ACTIVE
        );
        assert_eq!(
            ValidLineValue::Inactive as ffi::gpiod_line_value,
            ffi::gpiod_line_value_GPIOD_LINE_VALUE_INACTIVE
        );
        assert_eq!(
            ValidLineValue::Active as ffi::gpiod_line_value,
            ffi::gpiod_line_value_GPIOD_LINE_VALUE_ACTIVE
        );
        assert_ne!(
            ffi::gpiod_line_value_GPIOD_LINE_VALUE_ERROR,
            LineValue::INACTIVE.as_raw()
        );
        assert_ne!(
            ffi::gpiod_line_value_GPIOD_LINE_VALUE_ERROR,
            LineValue::ACTIVE.as_raw()
        );
    }

    #[test]
    fn valid_line_values_cast_to_c_without_copy() {
        let values = [ValidLineValue::Inactive, ValidLineValue::Active];
        let ptr = valid_line_values_as_c(&values);
        let copied = unsafe { std::slice::from_raw_parts(ptr, values.len()) };
        assert_eq!(
            copied,
            [
                ffi::gpiod_line_value_GPIOD_LINE_VALUE_INACTIVE,
                ffi::gpiod_line_value_GPIOD_LINE_VALUE_ACTIVE
            ]
        );
    }

    #[test]
    fn line_values_mut_cast_lets_c_write_error_sentinel() {
        let mut values = [LineValue::INACTIVE, LineValue::INACTIVE];
        let ptr = line_values_as_c_mut(&mut values);
        unsafe {
            *ptr = ffi::gpiod_line_value_GPIOD_LINE_VALUE_ACTIVE;
            *ptr.add(1) = ffi::gpiod_line_value_GPIOD_LINE_VALUE_ERROR;
        }
        assert_eq!(values[0], ValidLineValue::Active);
        assert_eq!(values[1], LineValue::ERROR);
    }

    #[test]
    fn direction_edge_bias_drive_clock_round_trip() {
        for value in [
            LineDirection::AsIs,
            LineDirection::Input,
            LineDirection::Output,
        ] {
            assert_eq!(line_direction_from_c(line_direction_to_c(value)), value);
        }
        for value in [
            LineEdge::None,
            LineEdge::Rising,
            LineEdge::Falling,
            LineEdge::Both,
        ] {
            assert_eq!(line_edge_from_c(line_edge_to_c(value)), value);
        }
        for value in [
            LineBias::AsIs,
            LineBias::Unknown,
            LineBias::Disabled,
            LineBias::PullUp,
            LineBias::PullDown,
        ] {
            assert_eq!(line_bias_from_c(line_bias_to_c(value)), value);
        }
        for value in [
            LineDrive::PushPull,
            LineDrive::OpenDrain,
            LineDrive::OpenSource,
        ] {
            assert_eq!(line_drive_from_c(line_drive_to_c(value)), value);
        }
        for value in [LineClock::Monotonic, LineClock::Realtime, LineClock::Hte] {
            assert_eq!(line_clock_from_c(line_clock_to_c(value)), value);
        }
    }
}
