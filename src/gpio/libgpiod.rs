//! GPIO traits derived from [libgpiod](https://libgpiod.readthedocs.io/) v2.
//!
//! Each trait maps to one `gpiod_*` object family in
//! [`include/gpiod.h`](https://github.com/brgl/libgpiod/blob/master/include/gpiod.h).
//! Method names keep the libgpiod `get_` / `is_` / `set_` prefixes for mechanical
//! cross-reference with upstream docs and `.cursor/redesign/libgpiod-v2-api.md`.
//!
//! # Construction and release
//!
//! [`Backend`] provides every `gpiod_*_open` / `gpiod_*_new` factory. Backend concrete
//! types implement [`Drop`](std::ops::Drop) for matching `*_close` / `*_free` / `*_release`
//! calls — see `.cursor/redesign/libgpiod-v2-api.md` § Object construction and release.
//!
//! # Events
//!
//! Line-info and edge events use libgpiod wait/read pairs. A `read_*` call is valid
//! when either:
//!
//! 1. The matching [`wait_*`](Chip::wait_info_event) returns [`WaitStatus::EventPending`], or
//! 2. The object fd ([`AsRawFd`]) is readable (`poll`, `epoll`, async runtime, …).
//!
//! - Chip: [`Chip::wait_info_event`] / fd readiness → [`Chip::read_info_event`]
//! - Line request: [`LineRequest::wait_edge_events`] / fd readiness →
//!   [`LineRequest::read_edge_events`] with [`EdgeEventBuffer`] from
//!   [`Backend::new_edge_event_buffer`]
//!
//! Both `wait_*` methods take `Option<u64>` timeout nanoseconds; `None` waits indefinitely.

use std::os::fd::AsRawFd;

use thiserror::Error;

// ---------------------------------------------------------------------------
// Shared enums (`@defgroup line_defs` and event types)
// ---------------------------------------------------------------------------

/// Logical line state (`enum gpiod_line_value`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineValue {
    /// `GPIOD_LINE_VALUE_INACTIVE` — line is logically inactive.
    Inactive = 0,
    /// `GPIOD_LINE_VALUE_ACTIVE` — line is logically active.
    Active = 1,
}

/// Direction settings (`enum gpiod_line_direction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineDirection {
    /// `GPIOD_LINE_DIRECTION_AS_IS` — request without changing direction.
    #[default]
    AsIs,
    /// `GPIOD_LINE_DIRECTION_INPUT` — read externally driven GPIO line.
    Input,
    /// `GPIOD_LINE_DIRECTION_OUTPUT` — drive the GPIO line.
    Output,
}

/// Edge detection settings (`enum gpiod_line_edge`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEdge {
    /// `GPIOD_LINE_EDGE_NONE` — edge detection disabled.
    #[default]
    None,
    /// `GPIOD_LINE_EDGE_RISING`.
    Rising,
    /// `GPIOD_LINE_EDGE_FALLING`.
    Falling,
    /// `GPIOD_LINE_EDGE_BOTH`.
    Both,
}

/// Internal bias settings (`enum gpiod_line_bias`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineBias {
    /// `GPIOD_LINE_BIAS_AS_IS` — do not change bias when applying config.
    #[default]
    AsIs,
    /// `GPIOD_LINE_BIAS_UNKNOWN`.
    Unknown,
    /// `GPIOD_LINE_BIAS_DISABLED`.
    Disabled,
    /// `GPIOD_LINE_BIAS_PULL_UP`.
    PullUp,
    /// `GPIOD_LINE_BIAS_PULL_DOWN`.
    PullDown,
}

/// Drive settings (`enum gpiod_line_drive`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineDrive {
    /// `GPIOD_LINE_DRIVE_PUSH_PULL`.
    #[default]
    PushPull,
    /// `GPIOD_LINE_DRIVE_OPEN_DRAIN`.
    OpenDrain,
    /// `GPIOD_LINE_DRIVE_OPEN_SOURCE`.
    OpenSource,
}

/// Clock used for edge-event timestamps (`enum gpiod_line_clock`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineClock {
    /// `GPIOD_LINE_CLOCK_MONOTONIC`.
    #[default]
    Monotonic,
    /// `GPIOD_LINE_CLOCK_REALTIME`.
    Realtime,
    /// `GPIOD_LINE_CLOCK_HTE` — hardware timestamp engine.
    Hte,
}

/// Line status change event types (`enum gpiod_info_event_type`, `@ref line_watch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoEventType {
    /// `GPIOD_INFO_EVENT_LINE_REQUESTED`.
    LineRequested,
    /// `GPIOD_INFO_EVENT_LINE_RELEASED`.
    LineReleased,
    /// `GPIOD_INFO_EVENT_LINE_CONFIG_CHANGED`.
    LineConfigChanged,
}

/// Edge event types (`enum gpiod_edge_event_type`, `@ref edge_event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeEventType {
    /// `GPIOD_EDGE_EVENT_RISING_EDGE`.
    RisingEdge,
    /// `GPIOD_EDGE_EVENT_FALLING_EDGE`.
    FallingEdge,
}

/// Result of `gpiod_chip_wait_info_event` / `gpiod_line_request_wait_edge_events`.
///
/// Maps C return codes: `0` → [`WaitStatus::Timeout`], `1` → [`WaitStatus::EventPending`],
/// `-1` → `Err(GPIOError)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStatus {
    /// Wait timed out (`0`).
    Timeout,
    /// Event is pending (`1`); safe to call the matching `read_*` method (or fd was readable).
    EventPending,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Userspace error type replacing libgpiod `NULL` returns and `-1` status codes.
#[derive(Debug, Error)]
pub enum GPIOError {
    #[error("GPIO chip or line request is closed")]
    Closed,
    #[error("invalid line offset {0}")]
    InvalidOffset(u32),
    #[error("line name not found: {0}")]
    LineNameNotFound(String),
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("length mismatch: expected {expected}, got {actual}")]
    LengthMismatch { expected: usize, actual: usize },
    #[error("line config is empty")]
    EmptyLineConfig,
    #[error("line offset {offset} is not configured")]
    UnconfiguredOffset { offset: u32 },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// gpiod_chip_info → ChipInfo (`@defgroup chip_info`)
// ---------------------------------------------------------------------------

/// Immutable snapshot of chip metadata (`struct gpiod_chip_info`).
///
/// Created by [`Chip::get_info`] (`gpiod_chip_get_info`), not by [`Backend`].
/// String accessors mirror pointers tied to the parent object in C; backends
/// own the storage or implement `Drop` → `gpiod_chip_info_free` when wrapping FFI.
/// `Clone` replaces `gpiod_line_info_copy`-style standalone copies.
pub trait ChipInfo {
    /// `gpiod_chip_info_get_name` — kernel chip name.
    fn get_name(&self) -> &str;

    /// `gpiod_chip_info_get_label` — kernel chip label.
    fn get_label(&self) -> &str;

    /// `gpiod_chip_info_get_num_lines` — number of GPIO lines exposed by the chip.
    fn get_num_lines(&self) -> usize;
}

// ---------------------------------------------------------------------------
// gpiod_line_info → LineInfo (`@defgroup line_info`)
// ---------------------------------------------------------------------------

/// Immutable snapshot of line status (`struct gpiod_line_info`).
///
/// Created by [`Chip::get_line_info`] / [`Chip::watch_line_info`] or `gpiod_line_info_copy`,
/// not by [`Backend`]. Does **not** include the live line value.
/// FFI-backed types: `Drop` → `gpiod_line_info_free`.
pub trait LineInfo {
    /// `gpiod_line_info_get_offset` — offset within the parent chip.
    fn get_offset(&self) -> u32;

    /// `gpiod_line_info_get_name` — kernel line name, or `None` if unnamed.
    fn get_name(&self) -> Option<&str>;

    /// `gpiod_line_info_is_used` — line is in use by another consumer or the kernel.
    fn is_used(&self) -> bool;

    /// `gpiod_line_info_get_consumer` — name of the line consumer, if set.
    fn get_consumer(&self) -> Option<&str>;

    /// `gpiod_line_info_get_direction`.
    fn get_direction(&self) -> LineDirection;

    /// `gpiod_line_info_get_edge_detection`.
    fn get_edge_detection(&self) -> LineEdge;

    /// `gpiod_line_info_get_bias`.
    fn get_bias(&self) -> LineBias;

    /// `gpiod_line_info_get_drive`.
    fn get_drive(&self) -> LineDrive;

    /// `gpiod_line_info_is_active_low` — logical value is inverted vs physical.
    fn is_active_low(&self) -> bool;

    /// `gpiod_line_info_is_debounced` — hardware or kernel software debouncer active.
    fn is_debounced(&self) -> bool;

    /// `gpiod_line_info_get_debounce_period_us` — `0` if not debounced.
    fn get_debounce_period_us(&self) -> u64;

    /// `gpiod_line_info_get_event_clock` — clock used for edge-event timestamps.
    fn get_event_clock(&self) -> LineClock;
}

// ---------------------------------------------------------------------------
// gpiod_info_event → InfoEvent (`@defgroup line_watch`)
// ---------------------------------------------------------------------------

/// Line status change notification (`struct gpiod_info_event`).
///
/// Created by [`Chip::read_info_event`] (`gpiod_chip_read_info_event`).
pub trait InfoEvent {
    type LineInfo<'a>: LineInfo + 'a
    where
        Self: 'a;

    /// `gpiod_info_event_get_event_type`.
    fn get_event_type(&self) -> InfoEventType;

    /// `gpiod_info_event_get_timestamp_ns` — monotonic clock, nanoseconds.
    fn get_timestamp_ns(&self) -> u64;

    /// `gpiod_info_event_get_line_info` — owned copy of the associated line snapshot.
    fn get_line_info<'a>(&'a self) -> Self::LineInfo<'a>;
}

// ---------------------------------------------------------------------------
// gpiod_line_settings → LineSettings (`@defgroup line_settings`)
// ---------------------------------------------------------------------------

/// Per-line properties for request or reconfigure (`struct gpiod_line_settings`).
///
/// Construct with [`Backend::new_line_settings`] (`gpiod_line_settings_new`).
/// Backend concrete types must implement `Drop` → `gpiod_line_settings_free`.
/// Mutators may fail on invalid values; the object stays valid with sane defaults.
pub trait LineSettings {
    /// `gpiod_line_settings_reset`.
    fn reset(&mut self);

    /// `gpiod_line_settings_get_direction`.
    fn get_direction(&self) -> LineDirection;

    /// `gpiod_line_settings_set_direction`.
    fn set_direction(&mut self, direction: LineDirection) -> Result<(), GPIOError>;

    /// `gpiod_line_settings_get_edge_detection`.
    fn get_edge_detection(&self) -> LineEdge;

    /// `gpiod_line_settings_set_edge_detection`.
    fn set_edge_detection(&mut self, edge: LineEdge) -> Result<(), GPIOError>;

    /// `gpiod_line_settings_get_bias`.
    fn get_bias(&self) -> LineBias;

    /// `gpiod_line_settings_set_bias`.
    fn set_bias(&mut self, bias: LineBias) -> Result<(), GPIOError>;

    /// `gpiod_line_settings_get_drive`.
    fn get_drive(&self) -> LineDrive;

    /// `gpiod_line_settings_set_drive`.
    fn set_drive(&mut self, drive: LineDrive) -> Result<(), GPIOError>;

    /// `gpiod_line_settings_get_active_low`.
    fn get_active_low(&self) -> bool;

    /// `gpiod_line_settings_set_active_low`.
    fn set_active_low(&mut self, active_low: bool);

    /// `gpiod_line_settings_get_debounce_period_us`.
    fn get_debounce_period_us(&self) -> u64;

    /// `gpiod_line_settings_set_debounce_period_us`.
    fn set_debounce_period_us(&mut self, period_us: u64);

    /// `gpiod_line_settings_get_event_clock`.
    fn get_event_clock(&self) -> LineClock;

    /// `gpiod_line_settings_set_event_clock`.
    fn set_event_clock(&mut self, clock: LineClock) -> Result<(), GPIOError>;

    /// `gpiod_line_settings_get_output_value`.
    fn get_output_value(&self) -> LineValue;

    /// `gpiod_line_settings_set_output_value`.
    fn set_output_value(&mut self, value: LineValue) -> Result<(), GPIOError>;
}

// ---------------------------------------------------------------------------
// gpiod_line_config → LineConfig (`@defgroup line_config`)
// ---------------------------------------------------------------------------

/// Offset-to-settings mapping for request or reconfigure (`struct gpiod_line_config`).
///
/// Construct with [`Backend::new_line_config`] (`gpiod_line_config_new`).
/// Backend concrete types must implement `Drop` → `gpiod_line_config_free`.
/// Empty config is invalid for request. Duplicate offsets: last mapping wins.
/// Request order follows assignment order from `add_line_settings`.
pub trait LineConfig {
    type LineSettings: LineSettings;

    /// `gpiod_line_config_reset`.
    fn reset(&mut self);

    /// `gpiod_line_config_add_line_settings`.
    fn add_line_settings(
        &mut self,
        offsets: &[u32],
        settings: &Self::LineSettings,
    ) -> Result<(), GPIOError>;

    /// `gpiod_line_config_get_line_settings` — returns a copy of settings for `offset`.
    fn get_line_settings(&self, offset: u32) -> Result<Self::LineSettings, GPIOError>;

    /// `gpiod_line_config_set_output_values` — overrides per-line output values;
    /// `values` align with configured offset order from `get_configured_offsets`.
    fn set_output_values(&mut self, values: &[LineValue]) -> Result<(), GPIOError>;

    /// `gpiod_line_config_get_num_configured_offsets`.
    fn get_num_configured_offsets(&self) -> usize;

    /// `gpiod_line_config_get_configured_offsets` — writes up to `out.len()` offsets;
    /// returns the number stored (may truncate if `out` is too small).
    fn get_configured_offsets(&self, out: &mut [u32]) -> usize;
}

// ---------------------------------------------------------------------------
// gpiod_request_config → RequestConfig (`@defgroup request_config`)
// ---------------------------------------------------------------------------

/// Request-time options passed to the kernel (`struct gpiod_request_config`).
///
/// Construct with [`Backend::new_request_config`] (`gpiod_request_config_new`).
/// Backend concrete types must implement `Drop` → `gpiod_request_config_free`.
/// Pass `None` to [`Chip::request_lines`] for C `NULL` (default settings).
/// Kernel event buffer size (`gpiod_request_config_set_event_buffer_size`) is
/// independent of userspace [`EdgeEventBuffer`].
pub trait RequestConfig {
    /// `gpiod_request_config_set_consumer` — truncated by kernel if too long.
    fn set_consumer(&mut self, consumer: &str);

    /// `gpiod_request_config_get_consumer`.
    fn get_consumer(&self) -> &str;

    /// `gpiod_request_config_set_event_buffer_size` — `0` uses kernel default
    /// (`16 * num_lines`); kernel may adjust if too high.
    fn set_event_buffer_size(&mut self, event_buffer_size: usize);

    /// `gpiod_request_config_get_event_buffer_size`.
    fn get_event_buffer_size(&self) -> usize;
}

// ---------------------------------------------------------------------------
// gpiod_edge_event → EdgeEvent (`@defgroup edge_event`)
// ---------------------------------------------------------------------------

/// Single line edge event (`struct gpiod_edge_event`).
///
/// Obtained from [`EdgeEventBuffer::get_event`]. Borrows are valid until the
/// buffer is cleared or overwritten by [`LineRequest::read_edge_events`].
pub trait EdgeEvent {
    /// `gpiod_edge_event_get_event_type`.
    fn get_event_type(&self) -> EdgeEventType;

    /// `gpiod_edge_event_get_timestamp_ns` — clock source from line `event_clock` setting.
    fn get_timestamp_ns(&self) -> u64;

    /// `gpiod_edge_event_get_line_offset`.
    fn get_line_offset(&self) -> u32;

    /// `gpiod_edge_event_get_global_seqno` — sequence across all lines in the request.
    fn get_global_seqno(&self) -> u64;

    /// `gpiod_edge_event_get_line_seqno` — sequence for this line only.
    fn get_line_seqno(&self) -> u64;
}

// ---------------------------------------------------------------------------
// gpiod_edge_event_buffer → EdgeEventBuffer (`@defgroup edge_event`)
// ---------------------------------------------------------------------------

/// Userspace batch buffer for edge events (`struct gpiod_edge_event_buffer`).
///
/// Construct with [`Backend::new_edge_event_buffer`] (`gpiod_edge_event_buffer_new`).
/// Fill via [`LineRequest::read_edge_events`]; inspect with [`get_event`](EdgeEventBuffer::get_event).
/// Backend concrete types must implement `Drop` → `gpiod_edge_event_buffer_free`.
pub trait EdgeEventBuffer: Send {
    type EdgeEvent<'a>: EdgeEvent + 'a
    where
        Self: 'a;

    /// `gpiod_edge_event_buffer_get_capacity`.
    fn get_capacity(&self) -> usize;

    /// `gpiod_edge_event_buffer_get_num_events`.
    fn get_num_events(&self) -> usize;

    /// `gpiod_edge_event_buffer_get_event` — borrow valid until `clear` or next read.
    fn get_event<'a>(&'a self, index: usize) -> Result<Self::EdgeEvent<'a>, GPIOError>;

    /// Discards buffered events (backend-defined; no direct C equivalent).
    fn clear(&mut self);
}

// ---------------------------------------------------------------------------
// Backend — `*_open` / `*_new` factories
// ---------------------------------------------------------------------------

/// GPIO backend entry point: all libgpiod `*_open` and `*_new` constructors.
///
/// Each associated type is a trait object family; the backend's concrete type
/// (e.g. `MockLineSettings`) implements that trait and `Drop` for the matching
/// `*_close` / `*_free` / `*_release`.
pub trait Backend: Send + Sync {
    type Chip: Chip<
            LineSettings = Self::LineSettings,
            LineConfig = Self::LineConfig,
            RequestConfig = Self::RequestConfig,
            EdgeEventBuffer = Self::EdgeEventBuffer,
        >;
    type LineSettings: LineSettings + Send;
    type LineConfig: LineConfig<LineSettings = Self::LineSettings> + Send;
    type RequestConfig: RequestConfig + Send;
    type EdgeEventBuffer: EdgeEventBuffer + Send;

    /// `gpiod_chip_open`.
    ///
    /// [`Chip`] must implement `Drop` → `gpiod_chip_close`.
    fn open_chip(&self, path: &str) -> Result<Self::Chip, GPIOError>;

    /// `gpiod_line_settings_new`.
    fn new_line_settings(&self) -> Result<Self::LineSettings, GPIOError>;

    /// `gpiod_line_config_new`.
    fn new_line_config(&self) -> Result<Self::LineConfig, GPIOError>;

    /// `gpiod_request_config_new`.
    fn new_request_config(&self) -> Result<Self::RequestConfig, GPIOError>;

    /// `gpiod_edge_event_buffer_new` — `capacity` `0` uses libgpiod default (64).
    fn new_edge_event_buffer(&self, capacity: usize) -> Result<Self::EdgeEventBuffer, GPIOError>;

    /// `gpiod_is_gpiochip_device` — checks if a path is a GPIO character device.
    fn is_gpiochip_device(&self, path: &str) -> bool;

    /// `gpiod_api_version` — returns the libgpiod API version.
    fn api_version(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// gpiod_chip → Chip (`@defgroup chips`)
// ---------------------------------------------------------------------------

/// Open handle to a GPIO character device (`struct gpiod_chip`).
///
/// Open via [`Backend::open_chip`] (`gpiod_chip_open`).
/// Backend concrete types must implement `Drop` → `gpiod_chip_close`.
/// Associated with an open file descriptor; exposes metadata, line-info lookup,
/// line watching, and line requests.
pub trait Chip: AsRawFd + Send {
    type ChipInfoOwned: ChipInfo + Send;
    type LineInfoOwned: LineInfo + Send;
    type InfoEventOwned: InfoEvent + Send;
    type LineRequestOwned: LineRequest<LineConfig = Self::LineConfig, EdgeEventBuffer = Self::EdgeEventBuffer>
        + Send;
    type LineSettings: LineSettings + Send;
    type LineConfig: LineConfig<LineSettings = Self::LineSettings> + Send;
    type RequestConfig: RequestConfig + Send;
    /// Same buffer type produced by [`Backend::new_edge_event_buffer`] and
    /// consumed by [`LineRequest::read_edge_events`].
    type EdgeEventBuffer: EdgeEventBuffer + Send;

    /// `gpiod_chip_get_info`.
    fn get_info(&self) -> Result<Self::ChipInfoOwned, GPIOError>;

    /// `gpiod_chip_get_path` — valid for the lifetime of the chip.
    fn get_path<'a>(&'a self) -> &'a str;

    /// `gpiod_chip_get_line_info` — snapshot; does not include line value.
    fn get_line_info(&self, offset: u32) -> Result<Self::LineInfoOwned, GPIOError>;

    /// `gpiod_chip_watch_line_info` — snapshot and start watching for status changes.
    fn watch_line_info(&self, offset: u32) -> Result<Self::LineInfoOwned, GPIOError>;

    /// `gpiod_chip_unwatch_line_info`.
    fn unwatch_line_info(&self, offset: u32) -> Result<(), GPIOError>;

    /// `gpiod_chip_wait_info_event` — `timeout_ns` `None` waits indefinitely.
    fn wait_info_event(&self, timeout_ns: Option<u64>) -> Result<WaitStatus, GPIOError>;

    /// `gpiod_chip_read_info_event`.
    ///
    /// Call when [`wait_info_event`](Chip::wait_info_event) returns
    /// [`WaitStatus::EventPending`], or when this chip's fd ([`AsRawFd`]) is readable.
    fn read_info_event(&self) -> Result<Self::InfoEventOwned, GPIOError>;

    /// `gpiod_chip_get_line_offset_from_name` — `ENOENT` → [`GPIOError::LineNameNotFound`].
    fn get_line_offset_from_name(&self, name: &str) -> Result<u32, GPIOError>;

    /// `gpiod_chip_request_lines` — `req_cfg` may be `None` for defaults.
    ///
    /// [`LineRequest`] must implement `Drop` → `gpiod_line_request_release`.
    /// Edge events: wait or fd poll, then [`LineRequest::read_edge_events`].
    fn request_lines(
        &self,
        req_cfg: Option<&Self::RequestConfig>,
        line_cfg: &Self::LineConfig,
    ) -> Result<Self::LineRequestOwned, GPIOError>;
}

// ---------------------------------------------------------------------------
// gpiod_line_request → LineRequest (`@defgroup line_request`)
// ---------------------------------------------------------------------------

/// Exclusive line request scoped to one chip (`struct gpiod_line_request`).
///
/// Created by [`Chip::request_lines`] (`gpiod_chip_request_lines`), not by [`Backend`].
/// Backend concrete types must implement `Drop` → `gpiod_line_request_release`.
/// Edge events: [`LineRequest::wait_edge_events`] or fd readiness, then
/// [`LineRequest::read_edge_events`] with [`EdgeEventBuffer`].
pub trait LineRequest: AsRawFd + Send {
    type LineConfig: LineConfig;
    type EdgeEventBuffer: EdgeEventBuffer;

    /// `gpiod_line_request_get_chip_name` — valid for the lifetime of the request.
    fn get_chip_name<'a>(&'a self) -> &'a str;

    /// `gpiod_line_request_get_num_requested_lines`.
    fn get_num_requested_lines(&self) -> usize;

    /// `gpiod_line_request_get_requested_offsets` — all requested offsets in order.
    fn get_requested_offsets(&self, out: &mut [u32]) -> usize;

    /// `gpiod_line_request_get_value` — `GPIOD_LINE_VALUE_ERROR` → `Err`.
    fn get_value(&self, offset: u32) -> Result<LineValue, GPIOError>;

    /// `gpiod_line_request_get_values_subset`.
    fn get_values_subset(&self, offsets: &[u32], values: &mut [LineValue])
    -> Result<(), GPIOError>;

    /// `gpiod_line_request_get_values` — order matches `get_requested_offsets`.
    fn get_values(&self, values: &mut [LineValue]) -> Result<(), GPIOError>;

    /// `gpiod_line_request_set_value`.
    fn set_value(&self, offset: u32, value: LineValue) -> Result<(), GPIOError>;

    /// `gpiod_line_request_set_values_subset`.
    fn set_values_subset(&self, offsets: &[u32], values: &[LineValue]) -> Result<(), GPIOError>;

    /// `gpiod_line_request_set_values` — order matches `get_requested_offsets`.
    fn set_values(&self, values: &[LineValue]) -> Result<(), GPIOError>;

    /// `gpiod_line_request_reconfigure_lines` — replaces entire config; unrequested
    /// offsets in `config` are ignored.
    fn reconfigure_lines(&self, config: &Self::LineConfig) -> Result<(), GPIOError>;

    /// `gpiod_line_request_wait_edge_events` — `timeout_ns` `None` waits indefinitely.
    fn wait_edge_events(&self, timeout_ns: Option<u64>) -> Result<WaitStatus, GPIOError>;

    /// `gpiod_line_request_read_edge_events` — fills `buffer` with up to `max_events`.
    ///
    /// Call when [`wait_edge_events`](LineRequest::wait_edge_events) returns
    /// [`WaitStatus::EventPending`], or when this request's fd ([`AsRawFd`]) is readable.
    /// Returns the number of events stored.
    fn read_edge_events(
        &self,
        buffer: &mut Self::EdgeEventBuffer,
        max_events: usize,
    ) -> Result<usize, GPIOError>;
}
