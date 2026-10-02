use std::collections::BTreeMap;
use std::mem::MaybeUninit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    Input,
    Output,
    Trigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPin {
    pub chip_index: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPin {
    pub mode: PinMode,
    pub pin: ResolvedPin,
}

impl CompiledPin {
    pub fn is_readable(&self) -> bool {
        matches!(self.mode, PinMode::Input | PinMode::Trigger)
    }

    pub fn is_writable(&self) -> bool {
        matches!(self.mode, PinMode::Output)
    }

    pub fn is_event_capable(&self) -> bool {
        matches!(self.mode, PinMode::Trigger)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPins {
    pub by_name: BTreeMap<String, CompiledPin>,
    pub trigger_by_pin: BTreeMap<(u32, u32), String>,
}

impl CompiledPins {
    pub fn pin(&self, name: &str) -> Option<&CompiledPin> {
        self.by_name.get(name)
    }

    pub fn trigger_pin_name(&self, chip_index: u32, offset: u32) -> Option<&str> {
        self.trigger_by_pin
            .get(&(chip_index, offset))
            .map(String::as_str)
    }
}

struct ChipData<T> {
    index: u32,
    data: Option<T>,
}

pub struct ChipIndices<'a, T> {
    inner: BTreeMap<&'a str, ChipData<T>>,
}

impl<'a, T> ChipIndices<'a, T> {
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }

    pub fn get_chip_index(&mut self, device: &'a str) -> (u32, &mut Option<T>) {
        let len = self.inner.len();
        let chip_data = self.inner.entry(device).or_insert_with(|| ChipData {
            index: len as u32,
            data: None,
        });
        (chip_data.index, &mut chip_data.data)
    }

    /// Assign a session-local `chip_index` per distinct configured device path.
    pub fn resolve(&mut self, device: &'a str, line: u32) -> (ResolvedPin, &mut Option<T>) {
        let (chip_index, data) = self.get_chip_index(device);
        (
            ResolvedPin {
                chip_index,
                offset: line,
            },
            data,
        )
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn collect(self) -> Vec<(&'a str, Option<T>)> {
        unsafe {
            let mut devices: Vec<MaybeUninit<(&str, Option<T>)>> =
                Vec::with_capacity(self.inner.len());
            devices.set_len(self.inner.len());
            for (device, chip_data) in self.inner.into_iter() {
                devices
                    .get_unchecked_mut(chip_data.index as usize)
                    .write((device, chip_data.data));
            }
            std::mem::transmute(devices)
        }
    }
}

#[cfg(test)]
mod tests {

    use super::ChipIndices;
    use super::ResolvedPin;

    #[test]
    fn chip_indices_group_by_device_and_use_configured_line() {
        let mut chips = ChipIndices::<()>::new();
        let (first, _) = chips.resolve("/tmp/chip-a.xml", 7);
        let (second, _) = chips.resolve("/tmp/chip-a.xml", 8);
        let (third, _) = chips.resolve("/tmp/chip-b.xml", 0);

        assert_eq!(
            first,
            ResolvedPin {
                chip_index: 0,
                offset: 7,
            }
        );
        assert_eq!(
            second,
            ResolvedPin {
                chip_index: 0,
                offset: 8,
            }
        );
        assert_eq!(
            third,
            ResolvedPin {
                chip_index: 1,
                offset: 0,
            }
        );

        let collected = chips.collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].0, "/tmp/chip-a.xml");
        assert_eq!(collected[1].0, "/tmp/chip-b.xml");
    }
}
