//! Bounded iCalendar (`.ics`) event, task, journal, and free/busy preview.
//!
//! RFC 5545 content lines are unfolded before UTF-8 parsing. Calendar time
//! values are displayed as supplied; recurrence rules, alarms, and time-zone
//! rules are not executed or expanded.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ir::Page;

const MAX_ICAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ICAL_PHYSICAL_LINES: usize = 1_000_000;
const MAX_ICAL_LOGICAL_LINES: usize = 500_000;
const MAX_ICAL_PROPERTIES_TOTAL: usize = 250_000;
const MAX_ICAL_LINE_BYTES: usize = 1024 * 1024;
const MAX_ICAL_COMPONENTS: usize = 100_000;
const MAX_ICAL_PROPERTIES_PER_COMPONENT: usize = 20_000;
const MAX_ICAL_PARAMS_PER_PROPERTY: usize = 100;
const MAX_ICAL_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ICAL_COMPONENT_DEPTH: usize = 64;

#[derive(Clone, Debug)]
struct IcalProperty {
    name: String,
    tzid: Option<String>,
    value: String,
}

#[derive(Clone, Debug)]
struct IcalComponent {
    kind: String,
    properties: Vec<IcalProperty>,
    has_alarm: bool,
    has_recurrence: bool,
    has_attachment: bool,
    has_unknown_properties: bool,
}

struct IcalPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    source_format: &'static str,
    page_offset: usize,
    page_count: usize,
    warnings: &'a [String],
}

impl PageConsumer for IcalPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.number = page.number.saturating_add(self.page_offset);
        page.source_format = self.source_format.into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)?;
        self.page_count = self.page_count.saturating_add(1);
        Ok(())
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_versioned(path, options, sink, "ical", "2.0")
}

pub(crate) fn convert_vcalendar(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_versioned(path, options, sink, "vcalendar", "1.0")
}

fn convert_versioned(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    source_format: &'static str,
    required_version: &str,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_ICAL_BYTES);
    let mut bytes = Vec::new();
    Read::take(File::open(path)?, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "calendar input exceeds maximum limit of {max_bytes} bytes"
        )));
    }
    let (calendar_name, components, calendar_warnings) =
        parse_calendar(&bytes, options.max_pages, required_version)?;
    if components.is_empty() {
        return Err(Error::InvalidInput(
            "calendar file contains no VEVENT, VTODO, VJOURNAL, or VFREEBUSY components".into(),
        ));
    }
    if components.len() > options.max_pages.min(MAX_ICAL_COMPONENTS) {
        return Err(Error::LimitExceeded(format!(
            "calendar file contains {} components; maximum is {} pages",
            components.len(),
            options.max_pages.min(MAX_ICAL_COMPONENTS)
        )));
    }

    let mut warnings = calendar_warnings;
    let mut total_pages = 0usize;
    for component in components {
        let (blocks, component_warnings) = render_component(&component, calendar_name.as_deref());
        let remaining_pages = options.max_pages.saturating_sub(total_pages);
        let mut component_options = options.clone();
        component_options.max_pages = remaining_pages;
        let mut page_sink = IcalPageSink {
            inner: sink,
            source_format,
            page_offset: total_pages,
            page_count: 0,
            warnings: &component_warnings,
        };
        render_blocks_to_pages(&blocks, &mut page_sink, &component_options)?;
        if page_sink.page_count == 0 {
            return Err(Error::InvalidInput(
                "calendar component produced no output pages".into(),
            ));
        }
        total_pages = total_pages
            .checked_add(page_sink.page_count)
            .ok_or_else(|| Error::LimitExceeded("calendar page count overflowed".into()))?;
        for warning in component_warnings {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }
    Ok(warnings)
}

fn parse_calendar(
    bytes: &[u8],
    max_pages: usize,
    required_version: &str,
) -> Result<(Option<String>, Vec<IcalComponent>, Vec<String>)> {
    let lines = unfold_lines(bytes)?;
    let mut stack = Vec::<String>::new();
    let mut active_component: Option<IcalComponent> = None;
    let mut components = Vec::new();
    let mut calendar_name = None;
    let mut calendar_version = None;
    let mut warnings = Vec::new();
    let mut has_timezone_component = false;
    let mut has_unknown_component = false;
    let mut calendar_started = false;
    let mut calendar_ended = false;
    let mut total_text_bytes = 0usize;
    let mut total_properties = 0usize;

    for (line_number, line) in lines.iter().enumerate() {
        let line = std::str::from_utf8(line).map_err(|error| {
            Error::InvalidInput(format!(
                "calendar content line {} is not valid UTF-8: {error}",
                line_number + 1
            ))
        })?;
        let Some(property) = parse_property(line, line_number + 1)? else {
            continue;
        };
        total_properties = total_properties.saturating_add(1);
        if total_properties > MAX_ICAL_PROPERTIES_TOTAL {
            return Err(Error::LimitExceeded(format!(
                "calendar file exceeds {MAX_ICAL_PROPERTIES_TOTAL} content properties"
            )));
        }
        total_text_bytes = total_text_bytes
            .checked_add(property.value.len())
            .ok_or_else(|| Error::LimitExceeded("calendar text size overflowed".into()))?;
        if total_text_bytes > MAX_ICAL_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "calendar values exceed {MAX_ICAL_TEXT_BYTES} bytes"
            )));
        }

        if property.name == "BEGIN" {
            let component_name = property.value.trim().to_ascii_uppercase();
            if stack.len() >= MAX_ICAL_COMPONENT_DEPTH {
                return Err(Error::LimitExceeded(format!(
                    "calendar component nesting exceeds {MAX_ICAL_COMPONENT_DEPTH}"
                )));
            }
            if component_name == "VCALENDAR" {
                if calendar_started || !stack.is_empty() {
                    return Err(Error::InvalidInput(
                        "calendar has a nested or repeated VCALENDAR".into(),
                    ));
                }
                calendar_started = true;
            } else if matches!(
                component_name.as_str(),
                "VEVENT" | "VTODO" | "VJOURNAL" | "VFREEBUSY"
            ) && stack.last().is_some_and(|parent| parent == "VCALENDAR")
            {
                if active_component.is_some() {
                    return Err(Error::InvalidInput(
                        "calendar components are unexpectedly nested".into(),
                    ));
                }
                let component_limit = max_pages.min(MAX_ICAL_COMPONENTS);
                if components.len() >= component_limit {
                    return Err(Error::LimitExceeded(format!(
                        "calendar file exceeds {component_limit} output components"
                    )));
                }
                active_component = Some(IcalComponent {
                    kind: component_name.clone(),
                    properties: Vec::new(),
                    has_alarm: false,
                    has_recurrence: false,
                    has_attachment: false,
                    has_unknown_properties: false,
                });
            } else if component_name == "VTIMEZONE" {
                has_timezone_component = true;
            } else if component_name == "VALARM" && active_component.is_some() {
                active_component
                    .as_mut()
                    .expect("active component exists")
                    .has_alarm = true;
            } else if stack.last().is_some_and(|parent| parent == "VCALENDAR") {
                has_unknown_component = true;
            }
            stack.push(component_name);
            continue;
        }

        if property.name == "END" {
            let component_name = property.value.trim().to_ascii_uppercase();
            let Some(open_component) = stack.pop() else {
                return Err(Error::InvalidInput(format!(
                    "calendar END:{component_name} has no matching BEGIN"
                )));
            };
            if open_component != component_name {
                return Err(Error::InvalidInput(format!(
                    "calendar component {open_component} ended as {component_name}"
                )));
            }
            if component_name == "VCALENDAR" {
                calendar_ended = true;
            } else if matches!(
                component_name.as_str(),
                "VEVENT" | "VTODO" | "VJOURNAL" | "VFREEBUSY"
            ) {
                let Some(component) = active_component.take() else {
                    return Err(Error::InvalidInput(format!(
                        "calendar END:{component_name} has no parsed calendar component"
                    )));
                };
                if component.kind != component_name {
                    return Err(Error::InvalidInput(format!(
                        "calendar component {} ended as {component_name}",
                        component.kind
                    )));
                }
                let component_limit = max_pages.min(MAX_ICAL_COMPONENTS);
                if components.len() >= component_limit {
                    return Err(Error::LimitExceeded(format!(
                        "calendar file exceeds {component_limit} output components"
                    )));
                }
                components.push(component);
            }
            continue;
        }

        if active_component.is_none() && stack.last().is_some_and(|item| item == "VCALENDAR") {
            match property.name.as_str() {
                "VERSION" => calendar_version = Some(property.value.clone()),
                "X-WR-CALNAME" => calendar_name = Some(unescape_text(&property.value)),
                _ => {}
            }
            continue;
        }
        let Some(component) = active_component.as_mut() else {
            continue;
        };
        if stack.last().is_some_and(|item| item == "VALARM") {
            continue;
        }
        if component.properties.len() >= MAX_ICAL_PROPERTIES_PER_COMPONENT {
            return Err(Error::LimitExceeded(format!(
                "calendar component exceeds {MAX_ICAL_PROPERTIES_PER_COMPONENT} properties"
            )));
        }
        match property.name.as_str() {
            "RRULE" | "RDATE" | "EXDATE" | "RECURRENCE-ID" => {
                component.has_recurrence = true;
                component.properties.push(property);
            }
            "ATTACH" => component.has_attachment = true,
            "UID" | "DTSTAMP" | "DTSTART" | "DTEND" | "DUE" | "DURATION" | "SUMMARY"
            | "DESCRIPTION" | "LOCATION" | "ORGANIZER" | "ATTENDEE" | "STATUS" | "PRIORITY"
            | "URL" | "CATEGORIES" | "CLASS" | "TRANSP" | "FREEBUSY" | "CREATED"
            | "LAST-MODIFIED" | "COMPLETED" | "PERCENT-COMPLETE" | "COMMENT" | "CONTACT"
            | "GEO" | "RESOURCES" | "SEQUENCE" => {
                component.properties.push(property);
            }
            _ => component.has_unknown_properties = true,
        }
    }
    if !calendar_started || !calendar_ended || !stack.is_empty() || active_component.is_some() {
        return Err(Error::InvalidInput(
            "calendar file is missing a balanced VCALENDAR component".into(),
        ));
    }
    if calendar_version.as_deref() != Some(required_version) {
        return Err(Error::Unsupported(format!(
            "calendar version {:?} is unsupported; version {required_version} is required",
            calendar_version,
        )));
    }
    if has_timezone_component {
        warnings.push(
            "calendar VTIMEZONE rules are retained as source values; timezone conversion is not performed".into(),
        );
    }
    if has_unknown_component {
        warnings.push("one or more unsupported calendar components were omitted".into());
    }
    Ok((calendar_name, components, warnings))
}

fn unfold_lines(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut physical_count = 0usize;
    let mut logical_lines = Vec::new();
    let mut current = Vec::new();
    for raw_line in bytes.split(|byte| *byte == b'\n') {
        physical_count += 1;
        if physical_count > MAX_ICAL_PHYSICAL_LINES {
            return Err(Error::LimitExceeded(format!(
                "calendar input exceeds {MAX_ICAL_PHYSICAL_LINES} physical lines"
            )));
        }
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.len() > MAX_ICAL_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "calendar physical line exceeds {MAX_ICAL_LINE_BYTES} bytes"
            )));
        }
        if line
            .first()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            if current.is_empty() {
                return Err(Error::InvalidInput(
                    "calendar folded content line has no previous line".into(),
                ));
            }
            if current.len().saturating_add(line.len().saturating_sub(1)) > MAX_ICAL_LINE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "unfolded calendar content line exceeds {MAX_ICAL_LINE_BYTES} bytes"
                )));
            }
            current.extend_from_slice(&line[1..]);
        } else {
            if !current.is_empty() {
                logical_lines.push(std::mem::take(&mut current));
                if logical_lines.len() > MAX_ICAL_LOGICAL_LINES {
                    return Err(Error::LimitExceeded(format!(
                        "calendar input exceeds {MAX_ICAL_LOGICAL_LINES} logical lines"
                    )));
                }
            }
            current.extend_from_slice(line);
        }
    }
    if !current.is_empty() {
        logical_lines.push(current);
    }
    if logical_lines.len() > MAX_ICAL_LOGICAL_LINES {
        return Err(Error::LimitExceeded(format!(
            "calendar input exceeds {MAX_ICAL_LOGICAL_LINES} logical lines"
        )));
    }
    if logical_lines
        .first()
        .is_some_and(|line| line.starts_with(&[0xef, 0xbb, 0xbf]))
    {
        logical_lines[0].drain(..3);
    }
    Ok(logical_lines)
}

fn parse_property(line: &str, line_number: usize) -> Result<Option<IcalProperty>> {
    let Some(colon) = find_unquoted_colon(line) else {
        if line.is_empty() {
            return Ok(None);
        }
        return Err(Error::InvalidInput(format!(
            "calendar content line {line_number} has no property separator"
        )));
    };
    let head = &line[..colon];
    let value = &line[colon + 1..];
    let mut parts = split_unquoted(head, ';').into_iter();
    let name = parts.next().unwrap_or_default().trim().to_ascii_uppercase();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(Error::InvalidInput(format!(
            "calendar content line {line_number} has an invalid property name"
        )));
    }
    if parts.len() > MAX_ICAL_PARAMS_PER_PROPERTY {
        return Err(Error::LimitExceeded(format!(
            "calendar content line {line_number} exceeds {MAX_ICAL_PARAMS_PER_PROPERTY} parameters"
        )));
    }
    let mut tzid = None;
    for param in parts {
        if let Some((key, value)) = param.split_once('=')
            && key.trim().eq_ignore_ascii_case("TZID")
        {
            tzid = Some(unquote(value.trim()));
        }
    }
    Ok(Some(IcalProperty {
        name,
        tzid,
        value: value.to_owned(),
    }))
}

fn find_unquoted_colon(line: &str) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == ':' && !quoted {
            return Some(index);
        }
    }
    None
}

fn split_unquoted(text: &str, delimiter: char) -> Vec<&str> {
    let mut quoted = false;
    let mut escaped = false;
    let mut starts = Vec::new();
    starts.push(0);
    for (index, character) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == delimiter && !quoted {
            starts.push(index + character.len_utf8());
        }
    }
    let mut parts = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().enumerate() {
        let end = starts
            .get(index + 1)
            .map(|next| next - delimiter.len_utf8())
            .unwrap_or(text.len());
        parts.push(&text[*start..end]);
    }
    parts
}

fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].replace("\\\"", "\"")
    } else {
        value.to_owned()
    }
}

fn unescape_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some('n' | 'N') => output.push('\n'),
                Some('\\') => output.push('\\'),
                Some(',') => output.push(','),
                Some(';') => output.push(';'),
                Some(other) => {
                    output.push('\\');
                    output.push(other);
                }
                None => output.push('\\'),
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn render_component(
    component: &IcalComponent,
    calendar_name: Option<&str>,
) -> (Vec<HtmlBlock>, Vec<String>) {
    let mut warnings = Vec::new();
    let summary = find_text_property(component, "SUMMARY")
        .unwrap_or_else(|| format!("{} component", component.kind));
    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: summary,
    }];
    if let Some(name) = calendar_name.filter(|name| !name.trim().is_empty()) {
        blocks.push(HtmlBlock::Paragraph {
            text: format!("Calendar: {name}"),
        });
    }
    for property in &component.properties {
        let (label, value) = match property.name.as_str() {
            "DTSTART" => ("Start", format_date_property(property)),
            "DTEND" => ("End", format_date_property(property)),
            "DUE" => ("Due", format_date_property(property)),
            "DTSTAMP" => ("Created", format_date_property(property)),
            "CREATED" => ("Created", format_date_property(property)),
            "LAST-MODIFIED" => ("Updated", format_date_property(property)),
            "COMPLETED" => ("Completed", format_date_property(property)),
            "PERCENT-COMPLETE" => ("Complete", format!("{}%", property.value)),
            "DURATION" => ("Duration", property.value.clone()),
            "SUMMARY" => continue,
            "DESCRIPTION" => {
                let decoded = unescape_text(&property.value);
                for paragraph in decoded
                    .split('\n')
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                {
                    blocks.push(HtmlBlock::Paragraph {
                        text: paragraph.to_owned(),
                    });
                }
                continue;
            }
            "LOCATION" => ("Location", unescape_text(&property.value)),
            "ORGANIZER" => ("Organizer", property.value.clone()),
            "ATTENDEE" => ("Attendee", property.value.clone()),
            "STATUS" => ("Status", property.value.clone()),
            "PRIORITY" => ("Priority", property.value.clone()),
            "URL" => ("URL", property.value.clone()),
            "CATEGORIES" => ("Categories", unescape_text(&property.value)),
            "FREEBUSY" => ("Free/busy", property.value.clone()),
            "COMMENT" => ("Comment", unescape_text(&property.value)),
            "CONTACT" => ("Contact", unescape_text(&property.value)),
            "GEO" => ("Coordinates", property.value.clone()),
            "RESOURCES" => ("Resources", unescape_text(&property.value)),
            "CLASS" => ("Access class", property.value.clone()),
            "TRANSP" => ("Transparency", property.value.clone()),
            "ATTACH" | "RRULE" | "RDATE" | "EXDATE" | "RECURRENCE-ID" | "UID" | "SEQUENCE" => {
                continue;
            }
            _ => continue,
        };
        let value = clean_text_value(&value);
        if !value.trim().is_empty() {
            blocks.push(HtmlBlock::Paragraph {
                text: format!("{label}: {value}"),
            });
        }
        if property.tzid.is_some() {
            warnings.push(
                "TZID values are shown without applying VTIMEZONE transitions or converting time zones".into(),
            );
        }
    }
    if component.has_recurrence {
        warnings.push(format!(
            "{} recurrence rules are displayed only as source properties and are not expanded",
            component.kind
        ));
        for property in &component.properties {
            if matches!(
                property.name.as_str(),
                "RRULE" | "RDATE" | "EXDATE" | "RECURRENCE-ID"
            ) {
                blocks.push(HtmlBlock::Paragraph {
                    text: format!("{}: {}", property.name, property.value),
                });
            }
        }
    }
    if component.has_alarm {
        warnings.push("VALARM triggers and alarm actions are not executed or rendered".into());
    }
    if component.has_attachment {
        warnings.push("calendar ATTACH properties are not fetched or rendered".into());
    }
    if component.has_unknown_properties {
        warnings.push("one or more unsupported calendar properties were omitted".into());
    }
    (blocks, warnings)
}

fn find_text_property(component: &IcalComponent, name: &str) -> Option<String> {
    component
        .properties
        .iter()
        .find(|property| property.name == name)
        .map(|property| clean_text_value(&unescape_text(&property.value)))
}

fn format_date_property(property: &IcalProperty) -> String {
    let raw = property.value.as_str();
    let bytes = raw.as_bytes();
    if bytes.len() == 8 && bytes.iter().all(u8::is_ascii_digit) {
        format!("{}-{}-{} (all-day)", &raw[..4], &raw[4..6], &raw[6..8])
    } else if bytes.len() >= 15
        && bytes.get(8) == Some(&b'T')
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && (bytes.len() == 15 || (bytes.len() == 16 && bytes[15] == b'Z'))
    {
        let zone = if bytes.len() == 16 {
            " UTC".to_owned()
        } else {
            property
                .tzid
                .as_ref()
                .map(|tzid| format!(" (TZID={tzid})"))
                .unwrap_or_else(|| " (floating local time)".into())
        };
        format!(
            "{}-{}-{} {}:{}:{}{}",
            &raw[..4],
            &raw[4..6],
            &raw[6..8],
            &raw[9..11],
            &raw[11..13],
            &raw[13..15],
            zone
        )
    } else {
        raw.to_owned()
    }
}

fn clean_text_value(value: &str) -> String {
    value
        .chars()
        .map(|character| if character == '\r' { '\n' } else { character })
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_calendar, parse_property, render_component};
    use crate::document::html::HtmlBlock;

    #[test]
    fn unfolds_a_physical_line_split_inside_a_utf8_character() {
        let bytes = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Caf\xc3\r\n \xa9 plan\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (_, components, _) = parse_calendar(bytes, 100, "2.0").unwrap();
        let (blocks, _) = render_component(&components[0], None);
        assert!(matches!(
            &blocks[0],
            HtmlBlock::Heading { text, .. } if text == "Café plan"
        ));
    }

    #[test]
    fn parses_quoted_parameters_and_escaped_calendar_text() {
        let property = parse_property(
            r#"ATTENDEE;CN="Doe, Jane";ROLE=REQ-PARTICIPANT:mailto:jane@example.test"#,
            1,
        )
        .unwrap()
        .unwrap();
        assert_eq!(property.tzid, None);
        assert_eq!(property.value, "mailto:jane@example.test");

        let bytes = b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Planning\\, review\\; final\\nSecond line\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (_, components, _) = parse_calendar(bytes, 100, "2.0").unwrap();
        let (blocks, _) = render_component(&components[0], None);
        assert!(matches!(
            &blocks[0],
            HtmlBlock::Heading { text, .. } if text == "Planning, review; final\nSecond line"
        ));
    }
}
