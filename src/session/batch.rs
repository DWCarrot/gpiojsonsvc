use thiserror::Error;

use crate::gpio::LineValue;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CombinedOffsetsError {
    #[error("chip index {chip_index} is out of range")]
    InvalidChipIndex { chip_index: u32 },
    #[error("conflicting set values for chip {chip_index} offset {offset}")]
    DuplicateSetOffset { chip_index: u32, offset: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectRule {
    pub target_slot: usize,
    pub bit_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombinedOffsets<T> {
    chips: Vec<PerChipOffsets<T>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PerChipOffsets<T> {
    offsets: Vec<u32>,
    attachments: Vec<T>,
}

impl<T> PerChipOffsets<T> {
    fn new() -> Self {
        Self {
            offsets: Vec::new(),
            attachments: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }
}

impl<T> CombinedOffsets<T> {
    pub fn new(chip_count: usize) -> Self {
        Self {
            chips: (0..chip_count).map(|_| PerChipOffsets::new()).collect(),
        }
    }

    pub fn chip_count(&self) -> usize {
        self.chips.len()
    }

    pub fn add(
        &mut self,
        chip_index: u32,
        offset: u32,
        attachment: T,
    ) -> Result<(), CombinedOffsetsError> {
        let slot = chip_slot(chip_index, self.chips.len())?;
        let chip = &mut self.chips[slot];
        chip.offsets.push(offset);
        chip.attachments.push(attachment);
        Ok(())
    }

    pub fn offsets(&self, chip_index: u32) -> Result<&[u32], CombinedOffsetsError> {
        let slot = chip_slot(chip_index, self.chips.len())?;
        Ok(self.chips[slot].offsets.as_slice())
    }

    pub fn attachments(&self, chip_index: u32) -> Result<&[T], CombinedOffsetsError> {
        let slot = chip_slot(chip_index, self.chips.len())?;
        Ok(self.chips[slot].attachments.as_slice())
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, &[u32], &[T])> {
        self.chips
            .iter()
            .enumerate()
            .filter(|(_, chip)| !chip.is_empty())
            .map(|(chip_index, chip)| {
                (
                    chip_index as u32,
                    chip.offsets.as_slice(),
                    chip.attachments.as_slice(),
                )
            })
    }
}

impl CombinedOffsets<LineValue> {
    pub fn add_set(
        &mut self,
        chip_index: u32,
        offset: u32,
        value: LineValue,
    ) -> Result<(), CombinedOffsetsError> {
        let slot = chip_slot(chip_index, self.chips.len())?;
        let chip = &mut self.chips[slot];
        if let Some(existing_index) = chip.offsets.iter().position(|existing| *existing == offset) {
            let existing_value = chip.attachments[existing_index];
            if existing_value != value {
                return Err(CombinedOffsetsError::DuplicateSetOffset { chip_index, offset });
            }
            return Ok(());
        }
        chip.offsets.push(offset);
        chip.attachments.push(value);
        Ok(())
    }
}

fn chip_slot(chip_index: u32, chip_count: usize) -> Result<usize, CombinedOffsetsError> {
    let slot = usize::try_from(chip_index)
        .map_err(|_| CombinedOffsetsError::InvalidChipIndex { chip_index })?;
    if slot >= chip_count {
        return Err(CombinedOffsetsError::InvalidChipIndex { chip_index });
    }
    Ok(slot)
}

#[cfg(test)]
mod tests {
    use crate::gpio::LineValue;

    use super::CollectRule;
    use super::CombinedOffsets;
    use super::CombinedOffsetsError;

    #[test]
    fn preserves_insertion_order_per_chip() {
        let mut batch = CombinedOffsets::<CollectRule>::new(2);
        batch
            .add(
                0,
                1,
                CollectRule {
                    target_slot: 0,
                    bit_index: 0,
                },
            )
            .expect("add");
        batch
            .add(
                0,
                2,
                CollectRule {
                    target_slot: 0,
                    bit_index: 1,
                },
            )
            .expect("add");
        batch
            .add(
                1,
                4,
                CollectRule {
                    target_slot: 1,
                    bit_index: 0,
                },
            )
            .expect("add");

        assert_eq!(batch.offsets(0).expect("offsets"), &[1, 2]);
        assert_eq!(batch.offsets(1).expect("offsets"), &[4]);
    }

    #[test]
    fn iter_used_chips_skips_empty_entries() {
        let mut batch = CombinedOffsets::<u8>::new(3);
        batch.add(1, 7, 1).expect("add");

        let used: Vec<_> = batch.iter().collect();
        assert_eq!(used.len(), 1);
        assert_eq!(used[0].0, 1);
        assert_eq!(used[0].1, &[7]);
        assert_eq!(used[0].2, &[1]);
    }

    #[test]
    fn set_batch_rejects_conflicting_duplicate_offsets() {
        let mut batch = CombinedOffsets::<LineValue>::new(1);
        batch.add_set(0, 2, LineValue::Active).expect("first write");
        assert_eq!(
            batch.add_set(0, 2, LineValue::Inactive),
            Err(CombinedOffsetsError::DuplicateSetOffset {
                chip_index: 0,
                offset: 2,
            })
        );
    }

    #[test]
    fn set_batch_allows_identical_duplicate_offsets() {
        let mut batch = CombinedOffsets::<LineValue>::new(1);
        batch.add_set(0, 2, LineValue::Active).expect("first write");
        batch
            .add_set(0, 2, LineValue::Active)
            .expect("duplicate write");
        assert_eq!(batch.offsets(0).expect("offsets"), &[2]);
    }

    #[test]
    fn invalid_chip_index_is_reported() {
        let batch = CombinedOffsets::<u8>::new(1);
        assert_eq!(
            batch.offsets(3),
            Err(CombinedOffsetsError::InvalidChipIndex { chip_index: 3 })
        );
    }
}
