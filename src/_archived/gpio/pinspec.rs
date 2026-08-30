use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinSpecParseError {
    InvalidFormat,
    EmptyChipName,
    InvalidLineOffset,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PinSpec {
    pub chip_name: String,
    pub line_offset: u32,
}

impl fmt::Display for PinSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.chip_name, self.line_offset)
    }
}

impl FromStr for PinSpec {
    type Err = PinSpecParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (chip_name, line_offset) = value
            .split_once(':')
            .ok_or(PinSpecParseError::InvalidFormat)?;
        if chip_name.trim().is_empty() {
            return Err(PinSpecParseError::EmptyChipName);
        }
        let line_offset = line_offset
            .parse::<u32>()
            .map_err(|_| PinSpecParseError::InvalidLineOffset)?;
        Ok(Self {
            chip_name: chip_name.to_owned(),
            line_offset,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::PinSpec;
    use super::PinSpecParseError;

    #[test]
    fn parses_valid_pin_spec() {
        let pin = "gpiochip0:12".parse::<PinSpec>().expect("pin should parse");
        assert_eq!(pin.chip_name, "gpiochip0");
        assert_eq!(pin.line_offset, 12);
    }

    #[test]
    fn rejects_pin_spec_without_separator() {
        let error = "gpiochip0-12"
            .parse::<PinSpec>()
            .expect_err("pin should fail");
        assert_eq!(error, PinSpecParseError::InvalidFormat);
    }
}
