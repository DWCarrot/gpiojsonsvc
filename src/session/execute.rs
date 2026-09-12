use crate::gpio::Backend;
use crate::gpio::Chip;
use crate::gpio::LineRequest;
use crate::gpio::LineValue;
use crate::gpio::ValidLineValue;
use crate::protocol::request::TargetSelector;
use crate::protocol::response::PinValuePayload;

use super::batch::CollectRule;
use super::batch::CombinedOffsets;
use super::batch::CombinedOffsetsError;
use super::compiled::CombinedResolvedPinsIter;
use super::compiled::CompiledTargets;
use super::compiled::ResolvedPins;
use super::initialized::InitializedSession;
use super::state::SessionError;

pub fn compile_get_batch<'a, I>(
    compiled: &'a CompiledTargets,
    targets: I,
    chip_count: usize,
) -> Result<CombinedOffsets<CollectRule>, SessionError<'a>>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut batch = CombinedOffsets::new(chip_count);
    for (target_slot, target_name) in targets.into_iter().enumerate() {
        add_get_target(&mut batch, compiled, target_slot, target_name)?;
    }
    Ok(batch)
}

pub fn add_get_target<'a>(
    batch: &mut CombinedOffsets<CollectRule>,
    compiled: &'a CompiledTargets,
    target_slot: usize,
    target_name: &'a str,
) -> Result<(), SessionError<'a>> {
    let target = compiled
        .target(target_name)
        .ok_or(SessionError::UnknownTarget {
            target: target_name,
        })?;
    if !target.is_readable() {
        return Err(SessionError::TargetNotReadable {
            target: target_name,
        });
    }
    append_get_pins(batch, target_slot, &target.pins)
}

pub fn compile_set_batch<'a, I>(
    compiled: &'a CompiledTargets,
    writes: I,
    chip_count: usize,
) -> Result<CombinedOffsets<ValidLineValue>, SessionError<'a>>
where
    I: IntoIterator<Item = (&'a str, u8)>,
{
    let mut batch = CombinedOffsets::new(chip_count);
    for (target_name, value) in writes {
        add_set_target(&mut batch, compiled, target_name, value)?;
    }
    Ok(batch)
}

pub fn add_set_target<'a>(
    batch: &mut CombinedOffsets<ValidLineValue>,
    compiled: &'a CompiledTargets,
    target_name: &'a str,
    value: u8,
) -> Result<(), SessionError<'a>> {
    let target = compiled
        .target(target_name)
        .ok_or(SessionError::UnknownTarget {
            target: target_name,
        })?;
    if !target.is_writable() {
        return Err(SessionError::TargetNotWritable {
            target: target_name,
        });
    }
    let width = target.width();
    if width < 8 && value as usize >= (1usize << width) {
        return Err(SessionError::TargetValueOutOfRange {
            target: target_name,
            value,
            bits: width,
        });
    }
    append_set_pins(batch, &target.pins, value)
}

pub fn fold_get_results(
    targets: &TargetSelector,
    rules_and_values: &[(CollectRule, ValidLineValue)],
) -> PinValuePayload {
    match targets {
        TargetSelector::Single(_) => {
            let value = rules_and_values
                .iter()
                .fold(0u8, |accumulated, (rule, line_value)| {
                    accumulate_bit(accumulated, rule.bit_index, line_value_to_bit(*line_value))
                });
            PinValuePayload::Value(value)
        }
        TargetSelector::Multiple(names) => {
            let mut values = vec![0u8; names.len()];
            for (rule, line_value) in rules_and_values {
                values[rule.target_slot] = accumulate_bit(
                    values[rule.target_slot],
                    rule.bit_index,
                    line_value_to_bit(*line_value),
                );
            }
            PinValuePayload::Values(values)
        }
    }
}

pub fn apply_get_batch<B: Backend>(
    session: &InitializedSession<B>,
    batch: &CombinedOffsets<CollectRule>,
) -> Result<Vec<(CollectRule, ValidLineValue)>, SessionError<'static>> {
    let mut readings = Vec::new();
    for (chip_index, offsets, rules) in batch.iter() {
        let chip = &session.chips[chip_index as usize];
        let mut values = vec![LineValue::INACTIVE; offsets.len()];
        chip.request.get_values_subset(offsets, &mut values)?;
        for (rule, value) in rules.iter().zip(values) {
            readings.push((*rule, ValidLineValue::try_from(value)?));
        }
    }
    Ok(readings)
}

pub fn apply_set_batch<B: Backend>(
    session: &InitializedSession<B>,
    batch: &CombinedOffsets<ValidLineValue>,
) -> Result<(), SessionError<'static>> {
    for (chip_index, offsets, values) in batch.iter() {
        session.chips[chip_index as usize]
            .request
            .set_values_subset(offsets, values)?;
    }
    Ok(())
}

fn append_get_pins(
    batch: &mut CombinedOffsets<CollectRule>,
    target_slot: usize,
    pins: &ResolvedPins,
) -> Result<(), SessionError<'static>> {
    match pins {
        ResolvedPins::Single(pin) => batch
            .add(
                pin.chip_index,
                pin.offset,
                CollectRule {
                    target_slot,
                    bit_index: 0,
                },
            )
            .map_err(batch_error_to_session_error),
        ResolvedPins::Combined(pins) => {
            for (pin, bit_index) in CombinedResolvedPinsIter::new(pins.as_slice()) {
                batch
                    .add(
                        pin.chip_index,
                        pin.offset,
                        CollectRule {
                            target_slot,
                            bit_index: bit_index as usize,
                        },
                    )
                    .map_err(batch_error_to_session_error)?;
            }
            Ok(())
        }
    }
}

fn append_set_pins(
    batch: &mut CombinedOffsets<ValidLineValue>,
    pins: &ResolvedPins,
    value: u8,
) -> Result<(), SessionError<'static>> {
    match pins {
        ResolvedPins::Single(pin) => {
            let line_value = bit_to_line_value(value & 1);
            batch
                .add_set(pin.chip_index, pin.offset, line_value)
                .map_err(batch_error_to_session_error)
        }
        ResolvedPins::Combined(pins) => {
            for (pin, bit_index) in CombinedResolvedPinsIter::new(pins.as_slice()) {
                let bit = (value >> bit_index) & 1;
                let line_value = bit_to_line_value(bit);
                batch
                    .add_set(pin.chip_index, pin.offset, line_value)
                    .map_err(batch_error_to_session_error)?;
            }
            Ok(())
        }
    }
}

fn line_value_to_bit(value: ValidLineValue) -> u8 {
    match value {
        ValidLineValue::Active => 1,
        ValidLineValue::Inactive => 0,
    }
}

fn bit_to_line_value(bit: u8) -> ValidLineValue {
    if bit != 0 {
        ValidLineValue::Active
    } else {
        ValidLineValue::Inactive
    }
}

fn accumulate_bit(value: u8, bit_index: usize, bit: u8) -> u8 {
    value | (bit << bit_index)
}

fn batch_error_to_session_error(error: CombinedOffsetsError) -> SessionError<'static> {
    SessionError::Other(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use smallvec::smallvec;
    use tempfile::NamedTempFile;

    use crate::config::GPIODPinSpec;
    use crate::gpio::LineRequest;
    use crate::gpio::LineValue;
    use crate::gpio::ValidLineValue;
    use crate::gpio::mock::MockBackend;
    use crate::protocol::request::EdgeMode;
    use crate::protocol::request::PinSelector;
    use crate::protocol::request::TargetConfigRequest;
    use crate::protocol::request::TargetSelector;
    use crate::protocol::response::PinValuePayload;

    use super::CollectRule;
    use super::CombinedOffsets;
    use super::add_get_target;
    use super::add_set_target;
    use super::apply_get_batch;
    use super::apply_set_batch;
    use super::compile_get_batch;
    use super::compile_set_batch;
    use super::fold_get_results;
    use crate::session::InitializedSession;

    const SAMPLE_XML: &str = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">L</line>
    <line id="1" name="line1" direction="input" bias="pull_down">H</line>
    <line id="2" name="line2" direction="output" drive="push_pull">L</line>
    <line id="3" name="line3" direction="output" drive="push_pull">H</line>
    <line id="4" name="line4" direction="input">L</line>
</gpiochip>"#;

    fn write_chip_file(contents: &str) -> NamedTempFile {
        let file = NamedTempFile::new().expect("temp file");
        fs::write(file.path(), contents).expect("write chip xml");
        file
    }

    fn pin_map(path: &str, mappings: &[(&str, u32)]) -> BTreeMap<String, GPIODPinSpec> {
        mappings
            .iter()
            .map(|(pin, line)| {
                (
                    (*pin).to_owned(),
                    GPIODPinSpec {
                        device: path.to_owned(),
                        line: *line,
                    },
                )
            })
            .collect()
    }

    fn sample_session() -> InitializedSession<MockBackend> {
        let file = write_chip_file(SAMPLE_XML);
        let path = file.path().to_str().expect("utf8 path");
        let backend = MockBackend::new();
        let pins = pin_map(
            path,
            &[
                ("gpiochip0:0", 0),
                ("gpiochip0:1", 1),
                ("gpiochip0:2", 2),
                ("gpiochip0:3", 3),
                ("gpiochip0:4", 4),
            ],
        );
        let init_request = BTreeMap::from([
            (
                "IN".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Single("gpiochip0:0".to_owned()),
                    bias: None,
                },
            ),
            (
                "IN2".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Combined(smallvec![
                        "gpiochip0:1".to_owned(),
                        "gpiochip0:0".to_owned(),
                    ]),
                    bias: None,
                },
            ),
            (
                "OUT".to_owned(),
                TargetConfigRequest::Output {
                    pin: PinSelector::Single("gpiochip0:2".to_owned()),
                    drive: None,
                },
            ),
            (
                "OUT2".to_owned(),
                TargetConfigRequest::Output {
                    pin: PinSelector::Combined(smallvec![
                        "gpiochip0:2".to_owned(),
                        "gpiochip0:3".to_owned(),
                    ]),
                    drive: None,
                },
            ),
            (
                "TRIG".to_owned(),
                TargetConfigRequest::Trigger {
                    pin: "gpiochip0:4".to_owned(),
                    edge: EdgeMode::Both,
                },
            ),
        ]);

        InitializedSession::initialize("init-1".to_owned(), &init_request, &backend, &pins)
            .expect("initialize")
    }

    fn read_get_batch(
        session: &InitializedSession<MockBackend>,
        batch: &crate::session::CombinedOffsets<CollectRule>,
    ) -> Vec<(CollectRule, ValidLineValue)> {
        apply_get_batch(session, batch).expect("apply get batch")
    }

    #[test]
    fn compile_get_batch_collects_rules_for_single_and_combined_targets() {
        let session = sample_session();
        let batch = compile_get_batch(
            &session.compiled_targets,
            ["IN", "IN2"],
            session.chip_count(),
        )
        .expect("compile get batch");

        assert_eq!(batch.offsets(0).expect("offsets"), &[0, 1, 0]);
        let rules = batch.attachments(0).expect("rules");
        assert_eq!(
            rules,
            &[
                CollectRule {
                    target_slot: 0,
                    bit_index: 0,
                },
                CollectRule {
                    target_slot: 1,
                    bit_index: 1,
                },
                CollectRule {
                    target_slot: 1,
                    bit_index: 0,
                },
            ]
        );
    }

    #[test]
    fn compile_get_batch_rejects_unknown_and_non_readable_targets() {
        let session = sample_session();
        assert!(matches!(
            compile_get_batch(&session.compiled_targets, ["MISSING"], session.chip_count(),),
            Err(crate::session::SessionError::UnknownTarget { .. })
        ));
        assert!(matches!(
            compile_get_batch(&session.compiled_targets, ["OUT"], session.chip_count(),),
            Err(crate::session::SessionError::TargetNotReadable { .. })
        ));
    }

    #[test]
    fn compile_get_batch_allows_trigger_targets() {
        let session = sample_session();
        let batch = compile_get_batch(&session.compiled_targets, ["TRIG"], session.chip_count())
            .expect("compile trigger get");
        assert_eq!(batch.offsets(0).expect("offsets"), &[4]);
    }

    #[test]
    fn compile_set_batch_packs_bits_for_combined_output_target() {
        let session = sample_session();
        let batch = compile_set_batch(
            &session.compiled_targets,
            [("OUT2", 0b10)],
            session.chip_count(),
        )
        .expect("compile set batch");

        assert_eq!(batch.offsets(0).expect("offsets"), &[2, 3]);
        assert_eq!(
            batch.attachments(0).expect("values"),
            &[ValidLineValue::Active, ValidLineValue::Inactive]
        );
    }

    #[test]
    fn compile_set_batch_accumulates_multiple_targets_into_one_batch() {
        let session = sample_session();
        let batch = compile_set_batch(
            &session.compiled_targets,
            [("OUT", 1), ("OUT2", 0b10)],
            session.chip_count(),
        )
        .expect("compile set batch");

        assert_eq!(batch.offsets(0).expect("offsets"), &[2, 3]);
        assert_eq!(
            batch.attachments(0).expect("values"),
            &[ValidLineValue::Active, ValidLineValue::Inactive]
        );
    }

    #[test]
    fn compile_set_batch_rejects_conflicting_writes_across_targets() {
        let session = sample_session();
        let error = compile_set_batch(
            &session.compiled_targets,
            [("OUT", 0), ("OUT2", 0b10)],
            session.chip_count(),
        )
        .expect_err("conflicting writes");
        assert!(matches!(error, crate::session::SessionError::Other(_)));
        assert!(error.to_string().contains("conflicting set values"));
    }

    #[test]
    fn add_set_target_appends_to_existing_batch() {
        let session = sample_session();
        let mut batch = CombinedOffsets::new(session.chip_count());
        add_set_target(&mut batch, &session.compiled_targets, "OUT", 1).expect("first write");
        add_set_target(&mut batch, &session.compiled_targets, "OUT2", 0b10).expect("second write");
        assert_eq!(batch.offsets(0).expect("offsets"), &[2, 3]);
    }

    #[test]
    fn add_get_target_appends_to_existing_batch() {
        let session = sample_session();
        let mut batch = CombinedOffsets::new(session.chip_count());
        add_get_target(&mut batch, &session.compiled_targets, 0, "IN").expect("first");
        add_get_target(&mut batch, &session.compiled_targets, 1, "IN2").expect("second");
        assert_eq!(batch.offsets(0).expect("offsets"), &[0, 1, 0]);
    }

    #[test]
    fn compile_set_batch_rejects_out_of_range_values() {
        let session = sample_session();
        assert!(matches!(
            compile_set_batch(
                &session.compiled_targets,
                [("OUT2", 0b100)],
                session.chip_count(),
            ),
            Err(crate::session::SessionError::TargetValueOutOfRange { .. })
        ));
    }

    #[test]
    fn fold_get_results_packs_single_and_multiple_targets() {
        let single = fold_get_results(
            &TargetSelector::Single("IN".to_owned()),
            &[(
                CollectRule {
                    target_slot: 0,
                    bit_index: 0,
                },
                ValidLineValue::Inactive,
            )],
        );
        assert_eq!(single, PinValuePayload::Value(0));

        let combined = fold_get_results(
            &TargetSelector::Single("IN2".to_owned()),
            &[
                (
                    CollectRule {
                        target_slot: 0,
                        bit_index: 1,
                    },
                    ValidLineValue::Active,
                ),
                (
                    CollectRule {
                        target_slot: 0,
                        bit_index: 0,
                    },
                    ValidLineValue::Inactive,
                ),
            ],
        );
        assert_eq!(combined, PinValuePayload::Value(0b10));

        let multiple = fold_get_results(
            &TargetSelector::Multiple(vec!["IN".to_owned(), "IN2".to_owned()]),
            &[
                (
                    CollectRule {
                        target_slot: 0,
                        bit_index: 0,
                    },
                    ValidLineValue::Inactive,
                ),
                (
                    CollectRule {
                        target_slot: 1,
                        bit_index: 1,
                    },
                    ValidLineValue::Active,
                ),
                (
                    CollectRule {
                        target_slot: 1,
                        bit_index: 0,
                    },
                    ValidLineValue::Inactive,
                ),
            ],
        );
        assert_eq!(multiple, PinValuePayload::Values(vec![0, 0b10]));
    }

    #[test]
    fn get_batch_reads_and_folds_mock_session_values() {
        let session = sample_session();
        let selector = TargetSelector::Multiple(vec!["IN".to_owned(), "IN2".to_owned()]);
        let batch = compile_get_batch(
            &session.compiled_targets,
            selector.as_slice().iter().map(String::as_str),
            session.chip_count(),
        )
        .expect("compile get batch");
        let readings = read_get_batch(&session, &batch);
        let payload = fold_get_results(&selector, &readings);
        assert_eq!(payload, PinValuePayload::Values(vec![0, 0b10]));
    }

    #[test]
    fn set_batch_writes_mock_session_outputs() {
        let session = sample_session();
        let batch = compile_set_batch(
            &session.compiled_targets,
            [("OUT2", 0b01)],
            session.chip_count(),
        )
        .expect("compile set batch");

        apply_set_batch(&session, &batch).expect("apply set batch");

        assert_eq!(
            session.chips[0].request.get_value(2).expect("line 2"),
            ValidLineValue::Inactive
        );
        assert_eq!(
            session.chips[0].request.get_value(3).expect("line 3"),
            ValidLineValue::Active
        );
    }
}
