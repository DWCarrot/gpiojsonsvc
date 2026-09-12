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

const DEFAULT_EDGE_EVENT_BUFFER_CAPACITY: usize = 64;

/// Per-line request settings for the mock backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockLineSettings {
    direction: LineDirection,
    edge: LineEdge,
    bias: LineBias,
    drive: LineDrive,
    active_low: bool,
    debounce_period_us: u64,
    event_clock: LineClock,
    /// When `None`, request creation keeps the persisted output level.
    output_value: Option<ValidLineValue>,
}

impl Default for MockLineSettings {
    fn default() -> Self {
        Self {
            direction: LineDirection::default(),
            edge: LineEdge::default(),
            bias: LineBias::default(),
            drive: LineDrive::default(),
            active_low: false,
            debounce_period_us: 0,
            event_clock: LineClock::default(),
            output_value: None,
        }
    }
}

impl MockLineSettings {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn configured_output_value(&self) -> Option<ValidLineValue> {
        self.output_value
    }
}

impl LineSettings for MockLineSettings {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn get_direction(&self) -> LineDirection {
        self.direction
    }

    fn set_direction(&mut self, direction: LineDirection) -> Result<(), GPIOError> {
        self.direction = direction;
        Ok(())
    }

    fn get_edge_detection(&self) -> LineEdge {
        self.edge
    }

    fn set_edge_detection(&mut self, edge: LineEdge) -> Result<(), GPIOError> {
        self.edge = edge;
        Ok(())
    }

    fn get_bias(&self) -> LineBias {
        self.bias
    }

    fn set_bias(&mut self, bias: LineBias) -> Result<(), GPIOError> {
        self.bias = bias;
        Ok(())
    }

    fn get_drive(&self) -> LineDrive {
        self.drive
    }

    fn set_drive(&mut self, drive: LineDrive) -> Result<(), GPIOError> {
        self.drive = drive;
        Ok(())
    }

    fn get_active_low(&self) -> bool {
        self.active_low
    }

    fn set_active_low(&mut self, active_low: bool) {
        self.active_low = active_low;
    }

    fn get_debounce_period_us(&self) -> u64 {
        self.debounce_period_us
    }

    fn set_debounce_period_us(&mut self, period_us: u64) {
        self.debounce_period_us = period_us;
    }

    fn get_event_clock(&self) -> LineClock {
        self.event_clock
    }

    fn set_event_clock(&mut self, clock: LineClock) -> Result<(), GPIOError> {
        self.event_clock = clock;
        Ok(())
    }

    fn get_output_value(&self) -> LineValue {
        self.output_value.unwrap_or(ValidLineValue::Inactive).into()
    }

    fn set_output_value(&mut self, value: ValidLineValue) -> Result<(), GPIOError> {
        self.output_value = Some(value);
        Ok(())
    }
}

/// Offset-to-settings mapping for mock line requests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MockLineConfig {
    entries: Vec<(u32, MockLineSettings)>,
    output_values: Option<Vec<ValidLineValue>>,
}

impl MockLineConfig {
    pub fn new() -> Self {
        Self::default()
    }

    fn index_for_offset(&self, offset: u32) -> Option<usize> {
        self.entries
            .iter()
            .rposition(|(entry_offset, _)| *entry_offset == offset)
    }
}

impl LineConfig for MockLineConfig {
    type LineSettings = MockLineSettings;

    fn reset(&mut self) {
        self.entries.clear();
        self.output_values = None;
    }

    fn add_line_settings(
        &mut self,
        offsets: &[u32],
        settings: &Self::LineSettings,
    ) -> Result<(), GPIOError> {
        for &offset in offsets {
            if let Some(index) = self.index_for_offset(offset) {
                self.entries[index].1 = settings.clone();
            } else {
                self.entries.push((offset, settings.clone()));
            }
        }
        Ok(())
    }

    fn get_line_settings(&self, offset: u32) -> Result<Self::LineSettings, GPIOError> {
        self.index_for_offset(offset)
            .map(|index| self.entries[index].1.clone())
            .ok_or(GPIOError::UnconfiguredOffset { offset })
    }

    fn set_output_values(&mut self, values: &[ValidLineValue]) -> Result<(), GPIOError> {
        self.output_values = Some(values.to_vec());
        Ok(())
    }

    fn get_num_configured_offsets(&self) -> usize {
        self.entries.len()
    }

    fn get_configured_offsets(&self, out: &mut [u32]) -> usize {
        let count = out.len().min(self.entries.len());
        for (destination, (offset, _)) in out.iter_mut().zip(self.entries.iter()).take(count) {
            *destination = *offset;
        }
        count
    }
}

impl MockLineConfig {
    pub(crate) fn configured_output_values(&self) -> Option<Vec<ValidLineValue>> {
        self.output_values.clone()
    }
}

/// Request-time options for mock line requests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MockRequestConfig {
    consumer: String,
    event_buffer_size: usize,
}

impl MockRequestConfig {
    pub fn new() -> Self {
        Self::default()
    }
}

impl RequestConfig for MockRequestConfig {
    fn set_consumer(&mut self, consumer: &str) {
        self.consumer = consumer.to_owned();
    }

    fn get_consumer(&self) -> &str {
        &self.consumer
    }

    fn set_event_buffer_size(&mut self, event_buffer_size: usize) {
        self.event_buffer_size = event_buffer_size;
    }

    fn get_event_buffer_size(&self) -> usize {
        self.event_buffer_size
    }
}

/// Effective request-local line state not persisted directly in XML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockRequestLineState {
    pub direction: LineDirection,
    pub edge: LineEdge,
    pub bias: LineBias,
    pub drive: LineDrive,
    pub active_low: bool,
    pub debounce_period_us: u64,
    pub event_clock: LineClock,
    /// When `None`, request creation keeps the persisted output level.
    pub output_value: Option<ValidLineValue>,
}

impl MockRequestLineState {
    pub fn from_settings(settings: &MockLineSettings) -> Self {
        Self {
            direction: settings.get_direction(),
            edge: settings.get_edge_detection(),
            bias: settings.get_bias(),
            drive: settings.get_drive(),
            active_low: settings.get_active_low(),
            debounce_period_us: settings.get_debounce_period_us(),
            event_clock: settings.get_event_clock(),
            output_value: settings.configured_output_value(),
        }
    }
}

pub fn default_edge_event_buffer_capacity(requested: usize) -> usize {
    if requested == 0 {
        DEFAULT_EDGE_EVENT_BUFFER_CAPACITY
    } else {
        requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_config_preserves_assignment_order() {
        let mut config = MockLineConfig::new();
        let mut settings_a = MockLineSettings::new();
        settings_a.set_direction(LineDirection::Input).unwrap();
        let mut settings_b = MockLineSettings::new();
        settings_b.set_direction(LineDirection::Output).unwrap();

        config.add_line_settings(&[2, 0], &settings_a).unwrap();
        config.add_line_settings(&[1], &settings_b).unwrap();

        let mut offsets = [0; 3];
        let count = config.get_configured_offsets(&mut offsets);
        assert_eq!(count, 3);
        assert_eq!(offsets, [2, 0, 1]);
    }

    #[test]
    fn default_output_value_is_unset() {
        let settings = MockLineSettings::new();
        assert_eq!(settings.configured_output_value(), None);
        assert_eq!(settings.get_output_value(), ValidLineValue::Inactive);
    }

    #[test]
    fn set_output_values_accepts_partial_and_extra_lengths() {
        let mut config = MockLineConfig::new();
        let settings = MockLineSettings::new();
        config.add_line_settings(&[0, 1, 2], &settings).unwrap();

        config
            .set_output_values(&[ValidLineValue::Active])
            .expect("partial override");
        assert_eq!(
            config.configured_output_values(),
            Some(vec![ValidLineValue::Active])
        );

        config
            .set_output_values(&[
                ValidLineValue::Inactive,
                ValidLineValue::Active,
                ValidLineValue::Inactive,
                ValidLineValue::Active,
            ])
            .expect("extra override values are stored");
        assert_eq!(
            config.configured_output_values(),
            Some(vec![
                ValidLineValue::Inactive,
                ValidLineValue::Active,
                ValidLineValue::Inactive,
                ValidLineValue::Active,
            ])
        );
    }

    #[test]
    fn duplicate_offset_last_mapping_wins() {
        let mut config = MockLineConfig::new();
        let mut input = MockLineSettings::new();
        input.set_direction(LineDirection::Input).unwrap();
        let mut output = MockLineSettings::new();
        output.set_direction(LineDirection::Output).unwrap();

        config.add_line_settings(&[3], &input).unwrap();
        config.add_line_settings(&[3], &output).unwrap();

        assert_eq!(
            config.get_line_settings(3).unwrap().get_direction(),
            LineDirection::Output
        );

        let mut offsets = [0; 1];
        assert_eq!(config.get_configured_offsets(&mut offsets), 1);
        assert_eq!(offsets, [3]);
    }
}
