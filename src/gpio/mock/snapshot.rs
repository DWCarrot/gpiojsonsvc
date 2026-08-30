use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::BufRead;
use std::io::Write;

use quick_xml::Decoder;
use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::XmlVersion;
use quick_xml::events::BytesEnd;
use quick_xml::events::BytesStart;
use quick_xml::events::BytesText;
use quick_xml::events::Event;
use quick_xml::events::attributes::Attributes;
use thiserror::Error;

use crate::gpio::LineBias;
use crate::gpio::LineDirection;
use crate::gpio::LineDrive;
use crate::gpio::LineValue;

/// One-chip XML document: a single `<gpiochip id="...">` root with `<line>` children.
/// A `<gpiochips>` wrapper is not part of this format.

/// Physical line level persisted in chip XML as `H` or `L`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineLevel {
    High,
    Low,
}

impl LineLevel {
    pub fn from_xml(text: &str) -> Result<Self, LoadError> {
        match text {
            "H" => Ok(LineLevel::High),
            "L" => Ok(LineLevel::Low),
            other => Err(LoadError::InvalidLineLevel(other.to_owned())),
        }
    }

    pub fn as_xml(self) -> &'static str {
        match self {
            LineLevel::High => "H",
            LineLevel::Low => "L",
        }
    }
}

pub fn line_level_to_line_value(level: LineLevel, active_low: bool) -> LineValue {
    match (level, active_low) {
        (LineLevel::High, false) | (LineLevel::Low, true) => LineValue::Active,
        (LineLevel::Low, false) | (LineLevel::High, true) => LineValue::Inactive,
    }
}

pub fn line_value_to_line_level(value: LineValue, active_low: bool) -> LineLevel {
    match (value, active_low) {
        (LineValue::Active, false) | (LineValue::Inactive, true) => LineLevel::High,
        (LineValue::Inactive, false) | (LineValue::Active, true) => LineLevel::Low,
    }
}

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("xml parse error: {0}")]
    XmlParseError(quick_xml::Error),
    #[error("invalid line id `{0}`: {1}")]
    InvalidLineId(String, std::num::ParseIntError),
    #[error("invalid line direction `{0}`")]
    InvalidLineDirection(String),
    #[error("invalid line bias `{0}`")]
    InvalidLineBias(String),
    #[error("invalid line drive `{0}`")]
    InvalidLineDrive(String),
    #[error("invalid line active low `{0}`")]
    InvalidLineActiveLow(String),
    #[error("invalid line level `{0}`")]
    InvalidLineLevel(String),
    #[error("id must not be empty")]
    EmptyChipId,
    #[error("duplicate line id `{0}`")]
    DuplicateLineId(u32),
    #[error("input line must not define drive")]
    InputLineWithDrive,
    #[error("output line must not define bias")]
    OutputLineWithBias,
    #[error("unknown line attribute `{0}`")]
    UnknownLineAttribute(String),
    #[error("unknown chip attribute `{0}`")]
    UnknownChipAttribute(String),
    #[error("invalid root element; expected exactly one <gpiochip>, not a <gpiochips> wrapper")]
    InvalidRootElement,
    #[error("invalid xml document")]
    InvalidXmlDocument,
}

impl From<quick_xml::Error> for LoadError {
    fn from(err: quick_xml::Error) -> Self {
        LoadError::XmlParseError(err)
    }
}

impl From<quick_xml::events::attributes::AttrError> for LoadError {
    fn from(err: quick_xml::events::attributes::AttrError) -> Self {
        LoadError::XmlParseError(quick_xml::Error::from(err))
    }
}

impl From<quick_xml::encoding::EncodingError> for LoadError {
    fn from(err: quick_xml::encoding::EncodingError) -> Self {
        LoadError::XmlParseError(quick_xml::Error::from(err))
    }
}

/// Normalized persisted state for one mock chip file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockChipSnapshot {
    pub name: String,
    pub label: String,
    pub lines: BTreeMap<u32, MockLineSnapshot>,
}

impl MockChipSnapshot {
    pub fn num_lines(&self) -> usize {
        self.lines.len()
    }
}

/// Normalized persisted state for one line offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockLineSnapshot {
    pub name: String,
    pub consumer: String,
    pub direction: LineDirection,
    pub bias: LineBias,
    pub drive: LineDrive,
    pub active_low: bool,
    pub persisted_level: LineLevel,
}

fn reject_gpiochips_wrapper(name: &[u8]) -> Result<(), LoadError> {
    if name == b"gpiochips" {
        Err(LoadError::InvalidRootElement)
    } else {
        Ok(())
    }
}

fn parse_chip_start(
    e: &BytesStart<'_>,
    decoder: Decoder,
    xml_version: XmlVersion,
) -> Result<MockChipSnapshot, LoadError> {
    let attr = ParsedChipAttributes::parse(e.attributes(), decoder, xml_version)?;
    let Some(id) = attr.id else {
        return Err(LoadError::InvalidXmlDocument);
    };
    let id = id.into_owned();
    if id.is_empty() {
        return Err(LoadError::EmptyChipId);
    }
    Ok(MockChipSnapshot {
        name: id,
        label: attr.label.unwrap_or_default().into_owned(),
        lines: BTreeMap::new(),
    })
}

fn parse_line_start(
    e: &BytesStart<'_>,
    decoder: Decoder,
    xml_version: XmlVersion,
) -> Result<(u32, MockLineSnapshot), LoadError> {
    let attr = ParsedLineAttributes::parse(e.attributes(), decoder, xml_version)?;
    let Some(offset) = attr.offset else {
        return Err(LoadError::InvalidXmlDocument);
    };
    let direction = attr.direction.unwrap_or(LineDirection::Input);
    match direction {
        LineDirection::Input => {
            if attr.drive.is_some() {
                return Err(LoadError::InputLineWithDrive);
            }
        }
        LineDirection::Output => {
            if attr.bias.is_some() {
                return Err(LoadError::OutputLineWithBias);
            }
        }
        LineDirection::AsIs => {
            return Err(LoadError::InvalidLineDirection("as_is".to_owned()));
        }
    }
    let mut line = MockLineSnapshot {
        name: attr.name.unwrap_or_default().into_owned(),
        consumer: attr.consumer.unwrap_or_default().into_owned(),
        persisted_level: LineLevel::Low,
        direction,
        bias: LineBias::Disabled,
        drive: LineDrive::PushPull,
        active_low: attr.active_low.unwrap_or(false),
    };
    match line.direction {
        LineDirection::Input => {
            line.bias = attr.bias.unwrap_or(LineBias::Disabled);
        }
        LineDirection::Output => {
            line.drive = attr.drive.unwrap_or(LineDrive::PushPull);
        }
        LineDirection::AsIs => {
            return Err(LoadError::InvalidLineDirection("as_is".to_owned()));
        }
    }
    Ok((offset, line))
}

fn begin_chip_element(
    e: &BytesStart<'_>,
    decoder: Decoder,
    xml_version: XmlVersion,
    chip: &mut Option<MockChipSnapshot>,
    chip_closed: bool,
) -> Result<(), LoadError> {
    if chip.is_some() || chip_closed {
        return Err(LoadError::InvalidRootElement);
    }
    *chip = Some(parse_chip_start(e, decoder, xml_version)?);
    Ok(())
}

fn begin_line_element(
    e: &BytesStart<'_>,
    decoder: Decoder,
    xml_version: XmlVersion,
    chip: Option<&MockChipSnapshot>,
    chip_closed: bool,
    current_offset: &mut Option<u32>,
    current_line: &mut Option<MockLineSnapshot>,
) -> Result<(), LoadError> {
    if chip_closed || chip.is_none() {
        return Err(LoadError::InvalidRootElement);
    }
    if current_line.is_some() {
        return Err(LoadError::InvalidXmlDocument);
    }
    let (offset, line) = parse_line_start(e, decoder, xml_version)?;
    *current_offset = Some(offset);
    *current_line = Some(line);
    Ok(())
}

pub fn load_snapshot<R: BufRead>(
    reader: &mut Reader<R>,
    xml_version: XmlVersion,
) -> Result<MockChipSnapshot, LoadError> {
    let mut chip = None;
    let mut chip_closed = false;
    let mut current_offset = None;
    let mut current_line = None;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => {
                break;
            }
            Event::Start(e) => {
                reject_gpiochips_wrapper(e.name().as_ref())?;
                if e.name().as_ref() == b"gpiochip" {
                    begin_chip_element(&e, reader.decoder(), xml_version, &mut chip, chip_closed)?;
                } else if e.name().as_ref() == b"line" {
                    begin_line_element(
                        &e,
                        reader.decoder(),
                        xml_version,
                        chip.as_ref(),
                        chip_closed,
                        &mut current_offset,
                        &mut current_line,
                    )?;
                } else if chip.is_none() {
                    return Err(LoadError::InvalidRootElement);
                } else {
                    return Err(LoadError::InvalidXmlDocument);
                }
            }
            Event::Empty(e) => {
                reject_gpiochips_wrapper(e.name().as_ref())?;
                if e.name().as_ref() == b"gpiochip" {
                    begin_chip_element(&e, reader.decoder(), xml_version, &mut chip, chip_closed)?;
                    chip_closed = true;
                } else if chip.is_none() {
                    return Err(LoadError::InvalidRootElement);
                } else {
                    // Line levels are element text (`H`/`L`), so a self-closing <line/> is invalid.
                    return Err(LoadError::InvalidXmlDocument);
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == b"gpiochip" {
                    if chip.is_none() || current_line.is_some() || current_offset.is_some() {
                        return Err(LoadError::InvalidXmlDocument);
                    }
                    chip_closed = true;
                } else if e.name().as_ref() == b"line" {
                    let Some(chip) = chip.as_mut() else {
                        return Err(LoadError::InvalidXmlDocument);
                    };
                    let Some(offset) = current_offset.take() else {
                        return Err(LoadError::InvalidXmlDocument);
                    };
                    let Some(line) = current_line.take() else {
                        return Err(LoadError::InvalidXmlDocument);
                    };
                    if chip.lines.insert(offset, line).is_some() {
                        return Err(LoadError::DuplicateLineId(offset));
                    }
                }
            }
            Event::Text(e) => {
                if let Some(line) = &mut current_line {
                    let level_text = e.decode()?;
                    line.persisted_level = LineLevel::from_xml(level_text.as_ref())?;
                }
            }
            _ => {}
        }
    }
    if current_offset.is_some() || current_line.is_some() {
        return Err(LoadError::InvalidXmlDocument);
    }
    chip.ok_or(LoadError::InvalidRootElement)
}

pub fn save_snapshot<W: Write>(
    snapshot: &MockChipSnapshot,
    writer: &mut Writer<W>,
) -> Result<(), std::io::Error> {
    let mut chip = BytesStart::new("gpiochip");
    chip.push_attribute(("id", snapshot.name.as_str()));
    if !snapshot.label.is_empty() {
        chip.push_attribute(("label", snapshot.label.as_str()));
    }
    writer.write_event(Event::Start(chip))?;

    for (offset, line) in &snapshot.lines {
        write_line_element(writer, *offset, line)?;
    }

    writer.write_event(Event::End(BytesEnd::new("gpiochip")))?;
    Ok(())
}

pub fn diff_snapshot<R: BufRead>(
    reader: &mut Reader<R>,
    xml_version: XmlVersion,
    baseline: &MockChipSnapshot,
) -> Result<BTreeMap<u32, LineValue>, LoadError> {
    let mut result: BTreeMap<u32, LineValue> = BTreeMap::new();
    let mut seen_chip = false;
    let mut current_offset = None;
    let mut current_line_level = None;
    let mut current_line_baseline = None;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => {
                break;
            }
            Event::Start(e) => {
                reject_gpiochips_wrapper(e.name().as_ref())?;
                if e.name().as_ref() == b"gpiochip" {
                    if seen_chip {
                        return Err(LoadError::InvalidRootElement);
                    }
                    seen_chip = true;
                } else if e.name().as_ref() == b"line" {
                    if !seen_chip {
                        return Err(LoadError::InvalidRootElement);
                    }
                    let raw_attr = e.attributes();
                    for attribute in raw_attr {
                        let attribute = attribute?;
                        match attribute.key.as_ref() {
                            b"id" => {
                                let value = attribute
                                    .decoded_and_normalized_value(xml_version, reader.decoder())?;
                                let offset = value.parse::<u32>().map_err(|err| {
                                    LoadError::InvalidLineId(value.into_owned(), err)
                                })?;
                                current_offset = Some(offset);
                                current_line_baseline = baseline.lines.get(&offset);
                            }
                            _ => {}
                        }
                    }
                } else if !seen_chip {
                    return Err(LoadError::InvalidRootElement);
                }
            }
            Event::Empty(e) => {
                reject_gpiochips_wrapper(e.name().as_ref())?;
                if e.name().as_ref() == b"gpiochip" {
                    if seen_chip {
                        return Err(LoadError::InvalidRootElement);
                    }
                    seen_chip = true;
                } else if !seen_chip {
                    return Err(LoadError::InvalidRootElement);
                } else {
                    return Err(LoadError::InvalidXmlDocument);
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == b"line" {
                    if let Some(offset) = current_offset.take() {
                        if let Some(line) = current_line_baseline.take() {
                            if let Some(line_level) = current_line_level.take() {
                                if line.direction == LineDirection::Input
                                    && line_level != line.persisted_level
                                {
                                    result.insert(
                                        offset,
                                        line_level_to_line_value(line_level, line.active_low),
                                    );
                                }
                            } else {
                                return Err(LoadError::InvalidXmlDocument);
                            }
                        } else {
                            return Err(LoadError::InvalidXmlDocument);
                        }
                    } else {
                        return Err(LoadError::InvalidXmlDocument);
                    }
                }
            }
            Event::Text(e) => {
                if current_line_baseline.is_some() {
                    let level_text = e.decode()?;
                    current_line_level = Some(LineLevel::from_xml(level_text.as_ref())?);
                }
            }
            _ => {}
        }
    }
    if !seen_chip {
        return Err(LoadError::InvalidRootElement);
    }
    Ok(result)
}

fn write_line_element<W: Write>(
    writer: &mut Writer<W>,
    offset: u32,
    line: &MockLineSnapshot,
) -> Result<(), std::io::Error> {
    let mut line_start = BytesStart::new("line");
    let id = offset.to_string();
    line_start.push_attribute(("id", id.as_str()));
    if !line.name.is_empty() {
        line_start.push_attribute(("name", line.name.as_str()));
    }
    if !line.consumer.is_empty() {
        line_start.push_attribute(("consumer", line.consumer.as_str()));
    }

    match line.direction {
        LineDirection::Input => {
            line_start.push_attribute(("direction", "input"));
            let bias_value = match line.bias {
                LineBias::AsIs => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "invalid line bias `as_is`",
                    ));
                }
                LineBias::Unknown => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "invalid line bias `unknown`",
                    ));
                }
                LineBias::Disabled => "disabled",
                LineBias::PullUp => "pull_up",
                LineBias::PullDown => "pull_down",
            };
            line_start.push_attribute(("bias", bias_value));
        }
        LineDirection::Output => {
            line_start.push_attribute(("direction", "output"));
            let drive_value = match line.drive {
                LineDrive::PushPull => "push_pull",
                LineDrive::OpenDrain => "open_drain",
                LineDrive::OpenSource => "open_source",
            };
            line_start.push_attribute(("drive", drive_value));
        }
        LineDirection::AsIs => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid line direction `as_is`",
            ));
        }
    }
    line_start.push_attribute(("active_low", if line.active_low { "true" } else { "false" }));

    writer.write_event(Event::Start(line_start))?;
    let level_text = line.persisted_level.as_xml();
    writer.write_event(Event::Text(BytesText::new(level_text)))?;
    writer.write_event(Event::End(BytesEnd::new("line")))?;
    Ok(())
}

struct ParsedChipAttributes<'a> {
    id: Option<Cow<'a, str>>,
    label: Option<Cow<'a, str>>,
}

impl<'a> ParsedChipAttributes<'a> {
    pub fn parse(
        attributes: Attributes<'a>,
        decoder: Decoder,
        xml_version: XmlVersion,
    ) -> Result<Self, LoadError> {
        let mut result = ParsedChipAttributes {
            id: None,
            label: None,
        };
        for attribute in attributes {
            let attribute = attribute?;
            match attribute.key.as_ref() {
                b"id" => {
                    result.id = Some(
                        attribute
                            .decoded_and_normalized_value(xml_version, decoder)
                            .map_err(|err| LoadError::XmlParseError(err))?,
                    );
                }
                b"label" => {
                    result.label = Some(
                        attribute
                            .decoded_and_normalized_value(xml_version, decoder)
                            .map_err(|err| LoadError::XmlParseError(err))?,
                    );
                }
                other => {
                    let name = String::from_utf8_lossy(other).into_owned();
                    return Err(LoadError::UnknownChipAttribute(name));
                }
            }
        }
        Ok(result)
    }
}

#[derive(Debug)]
struct ParsedLineAttributes<'a> {
    offset: Option<u32>,
    name: Option<Cow<'a, str>>,
    consumer: Option<Cow<'a, str>>,
    direction: Option<LineDirection>,
    bias: Option<LineBias>,
    drive: Option<LineDrive>,
    active_low: Option<bool>,
}

impl<'a> ParsedLineAttributes<'a> {
    pub fn parse(
        attributes: Attributes<'a>,
        decoder: Decoder,
        xml_version: XmlVersion,
    ) -> Result<Self, LoadError> {
        let mut result = ParsedLineAttributes {
            offset: None,
            name: None,
            consumer: None,
            direction: None,
            bias: None,
            drive: None,
            active_low: None,
        };

        for attribute in attributes {
            let attribute = attribute?;
            match attribute.key.as_ref() {
                b"id" => {
                    let value = attribute.decoded_and_normalized_value(xml_version, decoder)?;
                    let offset = value
                        .parse::<u32>()
                        .map_err(|err| LoadError::InvalidLineId(value.into_owned(), err))?;
                    result.offset = Some(offset);
                }
                b"name" => {
                    let value = attribute.decoded_and_normalized_value(xml_version, decoder)?;
                    result.name = Some(value);
                }
                b"consumer" => {
                    let value = attribute.decoded_and_normalized_value(xml_version, decoder)?;
                    result.consumer = Some(value);
                }
                b"direction" => {
                    let value = attribute.decoded_and_normalized_value(xml_version, decoder)?;
                    match value.as_ref() {
                        "input" => result.direction = Some(LineDirection::Input),
                        "output" => result.direction = Some(LineDirection::Output),
                        other => return Err(LoadError::InvalidLineDirection(other.to_owned())),
                    }
                }
                b"bias" => {
                    let value = attribute.decoded_and_normalized_value(xml_version, decoder)?;
                    match value.as_ref() {
                        "disabled" => result.bias = Some(LineBias::Disabled),
                        "pull_up" => result.bias = Some(LineBias::PullUp),
                        "pull_down" => result.bias = Some(LineBias::PullDown),
                        other => return Err(LoadError::InvalidLineBias(other.to_owned())),
                    }
                }
                b"drive" => {
                    let value = attribute.decoded_and_normalized_value(xml_version, decoder)?;
                    match value.as_ref() {
                        "push_pull" => result.drive = Some(LineDrive::PushPull),
                        "open_drain" => result.drive = Some(LineDrive::OpenDrain),
                        "open_source" => result.drive = Some(LineDrive::OpenSource),
                        other => return Err(LoadError::InvalidLineDrive(other.to_owned())),
                    }
                }
                b"active_low" => {
                    let value = attribute.decoded_and_normalized_value(xml_version, decoder)?;
                    match value.as_ref() {
                        "true" => result.active_low = Some(true),
                        "false" => result.active_low = Some(false),
                        other => return Err(LoadError::InvalidLineActiveLow(other.to_owned())),
                    }
                }
                other => {
                    let name = String::from_utf8_lossy(other).into_owned();
                    return Err(LoadError::UnknownLineAttribute(name));
                }
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Cursor;

    use quick_xml::Reader;
    use quick_xml::Writer;
    use quick_xml::XmlVersion;

    use crate::gpio::LineBias;
    use crate::gpio::LineDirection;
    use crate::gpio::LineDrive;
    use crate::gpio::LineValue;

    use super::LineLevel;
    use super::LoadError;
    use super::MockChipSnapshot;
    use super::MockLineSnapshot;
    use super::diff_snapshot;
    use super::line_level_to_line_value;
    use super::load_snapshot;
    use super::save_snapshot;

    fn load_xml(content: &str) -> MockChipSnapshot {
        try_load_xml(content).expect("load snapshot")
    }

    fn try_load_xml(content: &str) -> Result<MockChipSnapshot, LoadError> {
        let mut reader = Reader::from_str(content);
        load_snapshot(&mut reader, XmlVersion::Explicit1_0)
    }

    fn try_diff_xml(
        baseline: &MockChipSnapshot,
        content: &str,
    ) -> Result<BTreeMap<u32, LineValue>, LoadError> {
        let mut reader = Reader::from_str(content);
        diff_snapshot(&mut reader, XmlVersion::Explicit1_0, baseline)
    }

    fn save_xml(snapshot: &MockChipSnapshot) -> String {
        let mut buffer = Vec::new();
        {
            let mut writer = Writer::new_with_indent(&mut buffer, b' ', 4);
            save_snapshot(snapshot, &mut writer).expect("save snapshot");
        }
        String::from_utf8(buffer).expect("utf8 snapshot xml")
    }

    fn diff_xml(baseline: &MockChipSnapshot, content: &str) -> BTreeMap<u32, LineValue> {
        try_diff_xml(baseline, content).expect("diff snapshot")
    }

    fn sample_snapshot() -> MockChipSnapshot {
        MockChipSnapshot {
            name: "gpiochip0".to_owned(),
            label: "mock gpiochip0".to_owned(),
            lines: BTreeMap::from([
                (
                    0,
                    MockLineSnapshot {
                        name: "line0".to_owned(),
                        consumer: String::new(),
                        direction: LineDirection::Input,
                        bias: LineBias::PullUp,
                        drive: LineDrive::PushPull,
                        active_low: false,
                        persisted_level: LineLevel::Low,
                    },
                ),
                (
                    1,
                    MockLineSnapshot {
                        name: "line1".to_owned(),
                        consumer: String::new(),
                        direction: LineDirection::Output,
                        bias: LineBias::Disabled,
                        drive: LineDrive::OpenDrain,
                        active_low: true,
                        persisted_level: LineLevel::Low,
                    },
                ),
            ]),
        }
    }

    #[test]
    fn load_save_round_trip_preserves_normalized_snapshot() {
        let snapshot = sample_snapshot();
        let xml = save_xml(&snapshot);
        let loaded = load_xml(&xml);
        assert_eq!(loaded, snapshot);
        assert!(xml.contains("<gpiochip "));
        assert!(!xml.contains("<gpiochips"));
    }

    #[test]
    fn load_snapshot_rejects_gpiochips_wrapper() {
        let xml = r#"<gpiochips>
    <gpiochip id="gpiochip0">
        <line id="0" direction="input">L</line>
    </gpiochip>
</gpiochips>"#;
        assert!(matches!(
            try_load_xml(xml),
            Err(LoadError::InvalidRootElement)
        ));
    }

    #[test]
    fn load_snapshot_rejects_a_second_gpiochip() {
        let xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input">L</line>
</gpiochip>
<gpiochip id="gpiochip1">
    <line id="0" direction="input">L</line>
</gpiochip>"#;
        assert!(matches!(
            try_load_xml(xml),
            Err(LoadError::InvalidRootElement)
        ));
    }

    #[test]
    fn load_snapshot_keeps_chip_id_as_metadata() {
        let snapshot = load_xml(
            r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" direction="input">L</line>
</gpiochip>"#,
        );
        assert_eq!(snapshot.name, "gpiochip0");
        assert_eq!(snapshot.label, "mock gpiochip0");
    }

    #[test]
    fn diff_snapshot_rejects_gpiochips_wrapper() {
        let baseline = sample_snapshot();
        let xml = r#"<gpiochips>
    <gpiochip id="gpiochip0">
        <line id="0" direction="input" bias="pull_up">H</line>
        <line id="1" direction="output" drive="open_drain" active_low="true">L</line>
    </gpiochip>
</gpiochips>"#;
        assert!(matches!(
            try_diff_xml(&baseline, xml),
            Err(LoadError::InvalidRootElement)
        ));
    }

    #[test]
    fn load_save_round_trip_through_reader_writer_handles() {
        let snapshot = sample_snapshot();
        let xml = save_xml(&snapshot);

        let mut reader = Reader::from_reader(Cursor::new(xml.as_bytes()));
        let loaded = load_snapshot(&mut reader, XmlVersion::Explicit1_0).expect("load");

        let mut buffer = Vec::new();
        {
            let mut writer = Writer::new_with_indent(&mut buffer, b' ', 4);
            save_snapshot(&loaded, &mut writer).expect("save");
        }
        let round_trip_xml = String::from_utf8(buffer).expect("utf8");
        assert_eq!(load_xml(&round_trip_xml), snapshot);
    }

    #[test]
    fn load_snapshot_stores_physical_levels_without_active_low_conversion() {
        let xml = r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" bias="disabled" active_low="true">H</line>
    <line id="1" direction="input" bias="disabled" active_low="true">L</line>
    <line id="2" direction="input" bias="disabled" active_low="false">L</line>
    <line id="3" direction="input" bias="disabled" active_low="false">H</line>
</gpiochip>"#;
        let snapshot = load_xml(xml);

        assert_eq!(
            snapshot.lines.get(&0).expect("line 0").persisted_level,
            LineLevel::High
        );
        assert_eq!(
            snapshot.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::Low
        );
        assert_eq!(
            snapshot.lines.get(&2).expect("line 2").persisted_level,
            LineLevel::Low
        );
        assert_eq!(
            snapshot.lines.get(&3).expect("line 3").persisted_level,
            LineLevel::High
        );
    }

    #[test]
    fn save_snapshot_writes_physical_levels_directly() {
        let snapshot = MockChipSnapshot {
            name: "gpiochip0".to_owned(),
            label: String::new(),
            lines: BTreeMap::from([
                (
                    0,
                    MockLineSnapshot {
                        name: String::new(),
                        consumer: String::new(),
                        direction: LineDirection::Input,
                        bias: LineBias::Disabled,
                        drive: LineDrive::PushPull,
                        active_low: true,
                        persisted_level: LineLevel::High,
                    },
                ),
                (
                    1,
                    MockLineSnapshot {
                        name: String::new(),
                        consumer: String::new(),
                        direction: LineDirection::Input,
                        bias: LineBias::Disabled,
                        drive: LineDrive::PushPull,
                        active_low: true,
                        persisted_level: LineLevel::Low,
                    },
                ),
                (
                    2,
                    MockLineSnapshot {
                        name: String::new(),
                        consumer: String::new(),
                        direction: LineDirection::Output,
                        bias: LineBias::Disabled,
                        drive: LineDrive::PushPull,
                        active_low: false,
                        persisted_level: LineLevel::High,
                    },
                ),
            ]),
        };

        let xml = save_xml(&snapshot);
        assert!(xml.contains(r#"active_low="true"#));
        assert!(xml.contains(r#"active_low="false"#));
        assert!(xml.contains(">H</line>"));
        assert!(xml.contains(">L</line>"));

        let round_trip = load_xml(&xml);
        assert_eq!(
            round_trip.lines.get(&0).expect("line 0").persisted_level,
            LineLevel::High
        );
        assert_eq!(
            round_trip.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::Low
        );
        assert_eq!(
            round_trip.lines.get(&2).expect("line 2").persisted_level,
            LineLevel::High
        );
    }

    #[test]
    fn diff_snapshot_reports_only_changed_input_values() {
        let baseline = sample_snapshot();

        let unchanged = diff_xml(
            &baseline,
            r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" bias="pull_up">L</line>
    <line id="1" direction="output" drive="open_drain" active_low="true">L</line>
</gpiochip>"#,
        );
        assert!(unchanged.is_empty());

        let input_change = diff_xml(
            &baseline,
            r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" bias="pull_up">H</line>
    <line id="1" direction="output" drive="open_drain" active_low="true">L</line>
</gpiochip>"#,
        );
        assert_eq!(input_change.len(), 1);
        assert_eq!(input_change.get(&0), Some(&LineValue::Active));

        let output_only_change = diff_xml(
            &baseline,
            r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" bias="pull_up">L</line>
    <line id="1" direction="output" drive="open_drain" active_low="true">H</line>
</gpiochip>"#,
        );
        assert!(output_only_change.is_empty());
    }

    #[test]
    fn diff_snapshot_ignores_metadata_only_edits() {
        let baseline = sample_snapshot();

        let metadata_edit = diff_xml(
            &baseline,
            r#"<gpiochip id="gpiochip0" label="renamed">
    <line id="0" name="renamed-input" direction="output" drive="push_pull" active_low="true">L</line>
    <line id="1" name="renamed-output" direction="input" bias="pull_down" active_low="false">L</line>
</gpiochip>"#,
        );
        assert!(metadata_edit.is_empty());
    }

    #[test]
    fn diff_snapshot_converts_physical_changes_with_baseline_active_low() {
        let baseline = MockChipSnapshot {
            name: "gpiochip0".to_owned(),
            label: String::new(),
            lines: BTreeMap::from([(
                0,
                MockLineSnapshot {
                    name: String::new(),
                    consumer: String::new(),
                    direction: LineDirection::Input,
                    bias: LineBias::Disabled,
                    drive: LineDrive::PushPull,
                    active_low: true,
                    persisted_level: LineLevel::Low,
                },
            )]),
        };

        let same_physical_level = diff_xml(
            &baseline,
            r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" bias="disabled" active_low="false">L</line>
</gpiochip>"#,
        );
        assert!(same_physical_level.is_empty());

        let changed_logical_level = diff_xml(
            &baseline,
            r#"<gpiochip id="gpiochip0">
    <line id="0" direction="input" bias="disabled" active_low="false">H</line>
</gpiochip>"#,
        );
        assert_eq!(changed_logical_level.get(&0), Some(&LineValue::Inactive));
    }

    #[test]
    fn line_level_conversion_respects_active_low() {
        assert_eq!(
            line_level_to_line_value(LineLevel::High, false),
            LineValue::Active
        );
        assert_eq!(
            line_level_to_line_value(LineLevel::High, true),
            LineValue::Inactive
        );
        assert_eq!(
            line_level_to_line_value(LineLevel::Low, true),
            LineValue::Active
        );
        assert_eq!(
            line_level_to_line_value(LineLevel::Low, false),
            LineValue::Inactive
        );
    }

    #[test]
    fn active_low_xml_round_trip_preserves_attribute_and_levels() {
        let xml = r#"<gpiochip id="gpiochip0" label="mock">
    <line id="0" name="in" direction="input" bias="pull_up" active_low="true">L</line>
    <line id="1" name="out" direction="output" drive="open_drain" active_low="true">L</line>
</gpiochip>"#;
        let loaded = load_xml(xml);
        assert!(loaded.lines.get(&0).expect("line 0").active_low);
        assert!(loaded.lines.get(&1).expect("line 1").active_low);
        assert_eq!(
            loaded.lines.get(&0).expect("line 0").persisted_level,
            LineLevel::Low
        );
        assert_eq!(
            loaded.lines.get(&1).expect("line 1").persisted_level,
            LineLevel::Low
        );

        let round_trip = load_xml(&save_xml(&loaded));
        assert_eq!(round_trip, loaded);
    }
}
