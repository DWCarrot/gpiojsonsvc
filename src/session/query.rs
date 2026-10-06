use std::collections::BTreeMap;

use crate::gpio::Backend;
use crate::gpio::Chip;
use crate::gpio::GPIOError;
use crate::gpio::LineDirection;
use crate::gpio::LineInfo;
use crate::protocol::request::TargetSelector;
use crate::protocol::response::GpioDirection;
use crate::protocol::response::GpioPinInfo;
use crate::protocol::response::QueryResultPayload;

use super::initialized::SessionConfig;
use super::state::SessionError;

/// Inspect configured lines without requesting them or changing session state.
pub fn collect_gpio_info<'a, B: Backend>(
    backend: &B,
    config: &'a dyn SessionConfig,
    pin: Option<&'a TargetSelector>,
) -> Result<QueryResultPayload, SessionError<'a>> {
    match pin {
        Some(TargetSelector::Single(pin)) => {
            collect_specific_gpio_info(backend, config, std::iter::once(pin.as_str()))
        }
        Some(TargetSelector::Multiple(pins)) => {
            collect_specific_gpio_info(backend, config, pins.iter().map(String::as_str))
        }
        None => collect_specific_gpio_info(
            backend,
            config,
            config.gpiod_pins().keys().map(String::as_str),
        ),
    }
}

fn collect_specific_gpio_info<'a, B, I>(
    backend: &B,
    config: &'a dyn SessionConfig,
    pins: I,
) -> Result<QueryResultPayload, SessionError<'a>>
where
    B: Backend,
    I: Iterator<Item = &'a str>,
{
    let mut chips = BTreeMap::new();
    let mut result = BTreeMap::new();
    for pin in pins {
        let spec = config.resolve_gpiod_pin(pin).ok_or(SessionError::UnmappedPin { pin })?;
        let chip = match chips.entry(spec.device.as_str()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(backend.open_chip(&spec.device)?)
            }
        };
        let info = chip.get_line_info(spec.line)?;
        let direction = match info.get_direction() {
            LineDirection::Input => GpioDirection::Input,
            LineDirection::Output => GpioDirection::Output,
            LineDirection::AsIs => {
                return Err(SessionError::Other(
                    format!(
                        "pin `{pin}` on device `{}` line {} has unexpected direction `as_is`",
                        spec.device, spec.line,
                    )
                    .into(),
                ));
            }
        };
        result.insert(pin.to_owned(), GpioPinInfo {
            id: spec.id,
            is_used: info.is_used(),
            consumer: info.get_consumer().map(str::to_owned),
            direction,
        });
    }

    Ok(QueryResultPayload::Gpio { pins: result })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GPIODPinSpec;
    use crate::gpio::mock::MockBackend;
    use tempfile::NamedTempFile;

    #[test]
    fn queries_selected_opaque_keys_aliases_and_multiple_chips_without_writes() {
        let first = NamedTempFile::new().unwrap();
        let second = NamedTempFile::new().unwrap();
        let xml = "<gpiochip id=\"chip\"><line id=\"7\" direction=\"output\">H</line></gpiochip>";
        std::fs::write(first.path(), xml).unwrap();
        std::fs::write(second.path(), xml).unwrap();
        let mut config = BTreeMap::new();
        for (name, id, file) in [
            (" opaque ", 26, &first),
            ("alias", 99, &first),
            ("other", 10, &second),
        ] {
            config.insert(
                name.to_owned(),
                GPIODPinSpec {
                    id,
                    device: file.path().to_str().unwrap().to_owned(),
                    line: 7,
                },
            );
        }
        let log = NamedTempFile::new().unwrap();
        let backend = MockBackend::new().with_write_log(log.path()).unwrap();
        let QueryResultPayload::Gpio { pins } = collect_gpio_info(&backend, &config, None).unwrap();
        assert_eq!(pins.len(), 3);
        assert_eq!(pins[" opaque "].id, 26);
        assert_eq!(pins["alias"].id, 99);
        assert_eq!(pins["other"].id, 10);
        assert!(!pins[" opaque "].is_used);
        assert_eq!(pins[" opaque "].consumer, None);
        assert_eq!(pins[" opaque "].direction, GpioDirection::Output);
        let filter =
            TargetSelector::Multiple(vec!["other".into(), " opaque ".into(), "other".into()]);
        let QueryResultPayload::Gpio { pins } =
            collect_gpio_info(&backend, &config, Some(&filter)).unwrap();
        assert_eq!(
            pins.keys().map(String::as_str).collect::<Vec<_>>(),
            [" opaque ", "other"]
        );
        assert_eq!(std::fs::read_to_string(first.path()).unwrap(), xml);
        assert_eq!(std::fs::read_to_string(second.path()).unwrap(), xml);
        backend.write_log().unwrap().flush();
        assert!(std::fs::read(log.path()).unwrap().is_empty());
    }

    #[test]
    fn resolves_all_names_before_io_and_does_not_open_unselected_devices() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            "<gpiochip id=\"chip\"><line id=\"0\">L</line></gpiochip>",
        )
        .unwrap();
        let config = BTreeMap::from([
            (
                "A".into(),
                GPIODPinSpec {
                    id: 1,
                    device: "/no/such/chip".into(),
                    line: 0,
                },
            ),
            (
                "B".into(),
                GPIODPinSpec {
                    id: 2,
                    device: file.path().to_str().unwrap().into(),
                    line: 0,
                },
            ),
            (
                "C".into(),
                GPIODPinSpec {
                    id: 3,
                    device: file.path().to_str().unwrap().into(),
                    line: 9,
                },
            ),
        ]);
        let backend = MockBackend::new();
        let filter = TargetSelector::Multiple(vec!["A".into(), "UNKNOWN".into()]);
        let error = collect_gpio_info(&backend, &config, Some(&filter)).unwrap_err();
        assert!(matches!(error, SessionError::GPIO(..)));
        let filter = TargetSelector::Single("B".into());
        let QueryResultPayload::Gpio { pins } =
            collect_gpio_info(&backend, &config, Some(&filter)).unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins["B"].direction, GpioDirection::Input);
        let error = collect_gpio_info(&backend, &config, None).unwrap_err();
        assert!(matches!(error, SessionError::GPIO(..)));
        let filter = TargetSelector::Multiple(vec!["B".into(), "C".into()]);
        let error = collect_gpio_info(&backend, &config, Some(&filter)).unwrap_err();
        assert!(matches!(error, SessionError::GPIO(..)));
    }
}
