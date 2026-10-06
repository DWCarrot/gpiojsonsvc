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
use super::compiled::CompiledPins;
use super::compiled::ResolvedPin;
use super::initialized::InitializedSession;
use super::state::SessionError;

pub fn compile_get_batch<'a, I>(
    compiled: &'a CompiledPins,
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
    compiled: &'a CompiledPins,
    target_slot: usize,
    selector: &'a str,
) -> Result<(), SessionError<'a>> {
    let pin = resolve_pin(compiled, selector, false)?;
    batch
        .add(pin.chip_index, pin.offset, CollectRule { target_slot })
        .map_err(batch_error_to_session_error)
}

pub fn compile_set_batch<'n, I>(
    compiled: &CompiledPins,
    writes: I,
    chip_count: usize,
) -> Result<CombinedOffsets<ValidLineValue>, SessionError<'n>>
where
    I: IntoIterator<Item = (&'n str, u8)>,
{
    let mut batch = CombinedOffsets::new(chip_count);
    for (target_name, value) in writes {
        add_set_target(&mut batch, compiled, target_name, value)?;
    }
    Ok(batch)
}

pub fn add_set_target<'n>(
    batch: &mut CombinedOffsets<ValidLineValue>,
    compiled: &CompiledPins,
    selector: &'n str,
    value: u8,
) -> Result<(), SessionError<'n>> {
    let pin = resolve_pin(compiled, selector, true)?;
    if value > 1 {
        return Err(SessionError::InvalidParameters(format!(
            "pin `{selector}` value must be 0 or 1"
        )));
    }
    let value = if value == 1 {
        ValidLineValue::Active
    } else {
        ValidLineValue::Inactive
    };
    batch
        .add_set(pin.chip_index, pin.offset, value)
        .map_err(batch_error_to_session_error)
}

fn resolve_pin<'a>(
    compiled: &CompiledPins,
    name: &'a str,
    writing: bool,
) -> Result<ResolvedPin, SessionError<'a>> {
    crate::protocol::request::validate_pin_name(name).map_err(SessionError::InvalidParameters)?;
    let pin = compiled
        .pin(name)
        .ok_or(SessionError::UnknownTarget { target: name })?;
    if writing && !pin.is_writable() {
        return Err(SessionError::TargetNotWritable { target: name });
    }
    if !writing && !pin.is_readable() {
        return Err(SessionError::TargetNotReadable { target: name });
    }
    Ok(pin.pin)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GetResultSize {
    Single,
    Multiple(usize),
}

impl From<&TargetSelector> for GetResultSize {
    fn from(targets: &TargetSelector) -> Self {
        match targets {
            TargetSelector::Single(_) => Self::Single,
            TargetSelector::Multiple(names) => Self::Multiple(names.len()),
        }
    }
}

pub fn apply_get_batch<B: Backend>(
    session: &InitializedSession<B>,
    batch: &CombinedOffsets<CollectRule>,
    size: GetResultSize,
) -> Result<PinValuePayload, SessionError<'static>> {
    let mut payload = match size {
        GetResultSize::Single => PinValuePayload::Value(0),
        GetResultSize::Multiple(count) => PinValuePayload::Values(vec![0u8; count]),
    };
    let slots = match &mut payload {
        PinValuePayload::Value(value) => std::slice::from_mut(value),
        PinValuePayload::Values(values) => values.as_mut_slice(),
    };
    let mut buffer = vec![LineValue::INACTIVE; batch.max_chip_size()];
    for (chip_index, offsets, rules) in batch.iter() {
        let chip = &session.chips[chip_index as usize];
        let values = &mut buffer[..offsets.len()];
        chip.request.get_values_subset(offsets, values)?;
        for (rule, value) in rules.iter().zip(values.iter().cloned()) {
            slots[rule.target_slot] = ValidLineValue::try_from(value)? as u8;
        }
    }
    Ok(payload)
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

fn batch_error_to_session_error(error: CombinedOffsetsError) -> SessionError<'static> {
    match error {
        CombinedOffsetsError::DuplicateSetOffset { .. } => {
            SessionError::InvalidParameters(error.to_string())
        }
        CombinedOffsetsError::InvalidChipIndex { .. } => SessionError::Other(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use tempfile::NamedTempFile;

    use crate::config::GPIODPinSpec;
    use crate::gpio::LineRequest;
    use crate::gpio::ValidLineValue;
    use crate::gpio::mock::MockBackend;
    use crate::protocol::common::ArrayMap;
    use crate::protocol::request::EdgeMode;
    use crate::protocol::request::PinConfigRequest;
    use crate::protocol::request::PinSelector;
    use crate::protocol::request::TargetSelector;
    use crate::protocol::response::PinValuePayload;
    use crate::session::SessionError;

    use super::CollectRule;
    use super::CombinedOffsets;
    use super::CombinedOffsetsError;
    use super::GetResultSize;
    use super::add_get_target;
    use super::add_set_target;
    use super::apply_get_batch;
    use super::apply_set_batch;
    use super::batch_error_to_session_error;
    use super::compile_get_batch;
    use super::compile_set_batch;
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
                        id: *line,
                        device: path.to_owned(),
                        line: *line,
                    },
                )
            })
            .collect()
    }

    fn selector(value: &str) -> PinSelector {
        PinSelector::parse(value.to_owned()).unwrap()
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
        let init_request: ArrayMap<PinSelector, PinConfigRequest> = [
            (
                "gpiochip0:0".to_owned(),
                PinConfigRequest::Input { bias: None },
            ),
            (
                "gpiochip0:1".to_owned(),
                PinConfigRequest::Input { bias: None },
            ),
            (
                "gpiochip0:2".to_owned(),
                PinConfigRequest::Output {
                    drive: None,
                    initial_value: None,
                    final_value: None,
                },
            ),
            (
                "gpiochip0:3".to_owned(),
                PinConfigRequest::Output {
                    drive: None,
                    initial_value: None,
                    final_value: None,
                },
            ),
            (
                "gpiochip0:4".to_owned(),
                PinConfigRequest::Trigger {
                    edge: EdgeMode::Both,
                },
            ),
        ]
        .into_iter()
        .map(|(name, config)| (selector(&name), config))
        .collect();

        InitializedSession::initialize("init-1".to_owned(), &init_request, &backend, &pins, "svc_1")
            .expect("initialize")
    }

    #[test]
    fn reads_single_and_multiple_pins_in_request_order() {
        let session = sample_session();
        let batch = compile_get_batch(
            &session.compiled_pins,
            ["gpiochip0:0"],
            session.chip_count(),
        )
        .unwrap();
        assert_eq!(
            apply_get_batch(&session, &batch, GetResultSize::Single).unwrap(),
            PinValuePayload::Value(0)
        );
        let target = TargetSelector::Multiple(vec![
            "gpiochip0:1".to_owned(),
            "gpiochip0:4".to_owned(),
            "gpiochip0:0".to_owned(),
        ]);
        let batch = compile_get_batch(
            &session.compiled_pins,
            target.as_slice().iter().map(String::as_str),
            session.chip_count(),
        )
        .unwrap();
        assert_eq!(
            apply_get_batch(&session, &batch, GetResultSize::from(&target)).unwrap(),
            PinValuePayload::Values(vec![1, 0, 0])
        );
    }

    #[test]
    fn writes_independent_pin_values_in_one_batch() {
        let session = sample_session();
        let batch = compile_set_batch(
            &session.compiled_pins,
            [("gpiochip0:3", 0), ("gpiochip0:2", 1)],
            session.chip_count(),
        )
        .unwrap();
        assert_eq!(batch.offsets(0).unwrap(), &[3, 2]);
        apply_set_batch(&session, &batch).unwrap();
        assert_eq!(
            session.chips[0].request.get_value(2).unwrap(),
            ValidLineValue::Active
        );
        assert_eq!(
            session.chips[0].request.get_value(3).unwrap(),
            ValidLineValue::Inactive
        );
    }

    #[test]
    fn rejects_combined_names_unknown_pins_and_wrong_modes() {
        let session = sample_session();
        assert!(matches!(
            compile_get_batch(
                &session.compiled_pins,
                ["gpiochip0:0|gpiochip0:1"],
                session.chip_count()
            ),
            Err(SessionError::InvalidParameters(_))
        ));
        for name in ["MISSING", "gpiochip0:2"] {
            assert!(
                compile_get_batch(&session.compiled_pins, [name], session.chip_count()).is_err()
            );
        }
        for name in [
            "gpiochip0:2|gpiochip0:3",
            "MISSING",
            "gpiochip0:0",
            "gpiochip0:4",
        ] {
            assert!(
                compile_set_batch(&session.compiled_pins, [(name, 0)], session.chip_count())
                    .is_err()
            );
        }
    }

    #[test]
    fn rejects_non_bit_values_before_applying_any_writes() {
        let session = sample_session();
        for value in [2, 4, 255] {
            assert!(matches!(
                compile_set_batch(
                    &session.compiled_pins,
                    [("gpiochip0:2", 1), ("gpiochip0:3", value)],
                    session.chip_count()
                ),
                Err(SessionError::InvalidParameters(_))
            ));
        }
        assert_eq!(
            session.chips[0].request.get_value(2).unwrap(),
            ValidLineValue::Inactive
        );
    }

    #[test]
    fn adding_individual_pins_preserves_read_slots_and_write_conflict_checks() {
        let session = sample_session();
        let mut reads = CombinedOffsets::new(session.chip_count());
        add_get_target(&mut reads, &session.compiled_pins, 0, "gpiochip0:1").unwrap();
        add_get_target(&mut reads, &session.compiled_pins, 1, "gpiochip0:1").unwrap();
        assert_eq!(
            apply_get_batch(&session, &reads, GetResultSize::Multiple(2)).unwrap(),
            PinValuePayload::Values(vec![1, 1])
        );
        let mut writes = CombinedOffsets::new(session.chip_count());
        add_set_target(&mut writes, &session.compiled_pins, "gpiochip0:2", 1).unwrap();
        assert!(matches!(
            add_set_target(&mut writes, &session.compiled_pins, "gpiochip0:2", 0),
            Err(SessionError::InvalidParameters(_))
        ));
    }

    #[test]
    fn batch_error_retains_typed_source() {
        let error =
            batch_error_to_session_error(CombinedOffsetsError::InvalidChipIndex { chip_index: 1 });
        assert!(matches!(
            std::error::Error::source(&error)
                .and_then(|source| source.downcast_ref::<CombinedOffsetsError>()),
            Some(CombinedOffsetsError::InvalidChipIndex { chip_index: 1 })
        ));
    }
}
