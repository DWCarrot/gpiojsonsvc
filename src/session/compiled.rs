use std::collections::BTreeMap;
use std::mem::MaybeUninit;
use std::slice::Iter as SliceIter;

use smallvec::SmallVec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
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
pub enum ResolvedPins {
    Single(ResolvedPin),
    Combined(SmallVec<[ResolvedPin; 8]>),
}

impl ResolvedPins {
    pub fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Combined(pins) => pins.len(),
        }
    }

    pub fn iter(&self) -> ResolvedPinIter<'_> {
        ResolvedPinIter::new(self)
    }

    pub fn width(&self) -> usize {
        self.len()
    }
}

pub struct ResolvedPinIter<'a> {
    fixed: [Option<&'a ResolvedPin>; 2],
    fixed_len: usize,
    combined: &'a [ResolvedPin],
    index: usize,
}

impl<'a> ResolvedPinIter<'a> {
    pub fn new(pins: &'a ResolvedPins) -> Self {
        match pins {
            ResolvedPins::Single(pin) => Self {
                fixed: [Some(pin), None],
                fixed_len: 1,
                combined: &[],
                index: 0,
            },
            ResolvedPins::Combined(pins) => Self {
                fixed: [None, None],
                fixed_len: 0,
                combined: pins.as_slice(),
                index: 0,
            },
        }
    }

    pub fn len(&self) -> usize {
        if self.fixed_len > 0 {
            self.fixed_len
        } else {
            self.combined.len()
        }
    }

    pub fn reset(&mut self) {
        self.index = 0;
    }
}

impl<'a> Iterator for ResolvedPinIter<'a> {
    type Item = &'a ResolvedPin;

    fn next(&mut self) -> Option<Self::Item> {
        if self.fixed_len > 0 {
            if self.index < self.fixed_len {
                let value = unsafe { self.fixed.get_unchecked(self.index).unwrap_unchecked() };
                self.index += 1;
                Some(value)
            } else {
                None
            }
        } else if let Some(value) = self.combined.get(self.index) {
            self.index += 1;
            Some(value)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

pub struct CombinedResolvedPinsIter<'a> {
    pins: SliceIter<'a, ResolvedPin>,
    bit_offset: u32,
}

impl<'a> CombinedResolvedPinsIter<'a> {
    pub fn new(pins: &'a [ResolvedPin]) -> Self {
        let bit_offset = pins.len() as u32;
        Self {
            pins: pins.iter(),
            bit_offset,
        }
    }
}

impl<'a> Iterator for CombinedResolvedPinsIter<'a> {
    type Item = (&'a ResolvedPin, u32); // (pin {chip_index, offset}, bit_offset)

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(pin) = self.pins.next() {
            self.bit_offset -= 1;
            Some((pin, self.bit_offset))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledTarget {
    pub mode: TargetMode,
    pub pins: ResolvedPins,
}

impl CompiledTarget {
    pub fn width(&self) -> usize {
        self.pins.width()
    }

    pub fn is_readable(&self) -> bool {
        matches!(self.mode, TargetMode::Input | TargetMode::Trigger)
    }

    pub fn is_writable(&self) -> bool {
        matches!(self.mode, TargetMode::Output)
    }

    pub fn is_event_capable(&self) -> bool {
        matches!(self.mode, TargetMode::Trigger)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledTargets {
    pub by_name: BTreeMap<String, CompiledTarget>,
    pub trigger_by_pin: BTreeMap<(u32, u32), String>,
}

impl CompiledTargets {
    pub fn target<'a>(&'a self, name: &'a str) -> Option<&'a CompiledTarget> {
        self.by_name.get(name)
    }

    pub fn trigger_target_name(&self, chip_index: u32, offset: u32) -> Option<&str> {
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
    use smallvec::smallvec;

    use super::ChipIndices;
    use super::CombinedResolvedPinsIter;
    use super::ResolvedPin;
    use super::ResolvedPins;

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

    #[test]
    fn resolved_pin_iter_matches_selector_order() {
        let pins = ResolvedPins::Combined(smallvec![
            ResolvedPin {
                chip_index: 0,
                offset: 2,
            },
            ResolvedPin {
                chip_index: 0,
                offset: 3,
            },
        ]);
        let mut it = pins.iter();
        assert_eq!(
            it.next(),
            Some(&ResolvedPin {
                chip_index: 0,
                offset: 2
            })
        );
        assert_eq!(
            it.next(),
            Some(&ResolvedPin {
                chip_index: 0,
                offset: 3
            })
        );
        assert_eq!(it.next(), None);
    }

    #[test]
    fn combined_resolved_pins_iter_matches_selector_order() {
        let pins = ResolvedPins::Combined(smallvec![
            ResolvedPin {
                chip_index: 0,
                offset: 2,
            },
            ResolvedPin {
                chip_index: 0,
                offset: 3,
            },
        ]);
        let pins_combined = match pins {
            ResolvedPins::Combined(combined) => combined,
            ResolvedPins::Single(_) => panic!("expected combined pins"),
        };
        let mut it = CombinedResolvedPinsIter::new(&pins_combined);
        assert_eq!(
            it.next(),
            Some((
                &ResolvedPin {
                    chip_index: 0,
                    offset: 2
                },
                1
            ))
        );
        assert_eq!(
            it.next(),
            Some((
                &ResolvedPin {
                    chip_index: 0,
                    offset: 3
                },
                0
            ))
        );
        assert_eq!(it.next(), None);
    }
}
