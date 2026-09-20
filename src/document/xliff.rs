//! Bounded XLIFF 1.2 and 2.x localization-catalog preview.
//!
//! Source, target, notes, and inline-code placeholders are treated as text.
//! DTDs, external entities, skeleton files, and binary data are never loaded.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use quick_xml::NsReader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::ResolveResult;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};

const MAX_XLIFF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XLIFF_DEPTH: usize = 256;
const MAX_XLIFF_TAG_BYTES: usize = 1024 * 1024;
const MAX_XLIFF_FILES: usize = 10_000;
const MAX_XLIFF_UNITS: usize = 100_000;
const MAX_XLIFF_SEGMENTS_PER_UNIT: usize = 10_000;
const MAX_XLIFF_TOTAL_SEGMENTS: usize = 100_000;
const MAX_XLIFF_NOTES_PER_UNIT: usize = 1_024;
const MAX_XLIFF_TEXT_PER_FIELD: usize = 2 * 1024 * 1024;
const MAX_XLIFF_TOTAL_TEXT: usize = 32 * 1024 * 1024;
const MAX_XLIFF_OUTPUT_BLOCKS: usize = 500_000;
const MAX_XLIFF_GROUP_PATH_BYTES: usize = 256;
const MAX_XLIFF_GROUP_LABEL_BYTES: usize = 128;
const XLIFF_12_NAMESPACE: &[u8] = b"urn:oasis:names:tc:xliff:document:1.2";
const XLIFF_20_NAMESPACE: &[u8] = b"urn:oasis:names:tc:xliff:document:2.0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum XliffVersion {
    V12,
    V2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextCapture {
    Source(usize),
    Target(usize),
    Note,
    Context,
}

enum EndAction {
    File,
    Group,
    Unit,
    Capture(TextCapture),
    InlineClose(String),
}

struct ElementFrame {
    name: String,
    core: bool,
    action: Option<EndAction>,
}

#[derive(Default)]
struct Segment {
    source: Option<String>,
    target: Option<String>,
    source_language: Option<String>,
    target_language: Option<String>,
    state: Option<String>,
    target_state: Option<String>,
    ignorable: bool,
}

#[derive(Default)]
struct Unit {
    id: Option<String>,
    name: Option<String>,
    translate: Option<String>,
    approved: Option<String>,
    group_path: String,
    segments: Vec<Segment>,
    notes: Vec<String>,
    contexts: Vec<String>,
}

#[derive(Default)]
struct FileDocument {
    original: Option<String>,
    source_language: Option<String>,
    target_language: Option<String>,
    units: Vec<Unit>,
}

#[derive(Default)]
struct NoteBuilder {
    label: String,
    text: String,
}

#[derive(Default)]
struct ParseWarnings {
    malformed_units: usize,
    empty_targets: usize,
    placeholder_codes: usize,
    skipped_structural_elements: usize,
    truncated_group_paths: usize,
}

#[derive(Default)]
struct XliffParser {
    version: Option<XliffVersion>,
    root_seen: bool,
    root_closed: bool,
    stack: Vec<ElementFrame>,
    source_language: Option<String>,
    target_language: Option<String>,
    current_file: Option<FileDocument>,
    files: Vec<FileDocument>,
    group_path: Vec<String>,
    current_unit: Option<Unit>,
    current_segment: Option<usize>,
    capture: Option<TextCapture>,
    note: Option<NoteBuilder>,
    context: Option<String>,
    warnings: ParseWarnings,
    text_bytes: usize,
    unit_count: usize,
    segment_count: usize,
    events: usize,
}

pub(crate) fn looks_like_xliff_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"xliff", Some(XLIFF_12_NAMESPACE))
        || crate::geospatial::xml_tree::looks_like_root(prefix, b"xliff", Some(XLIFF_20_NAMESPACE))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XLIFF_BYTES),
        "XLIFF input",
    )?;
    let xml = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("XLIFF input must be UTF-8: {error}")))?;
    let (blocks, warnings) = parse_xliff_blocks(&xml, options.max_xml_events)?;
    let mut page_sink = XliffPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

struct XliffPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for XliffPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xliff".into();
        if page.title.is_empty() {
            page.title = "XLIFF translation catalog".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn parse_xliff_blocks(
    xml: &str,
    max_events: usize,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    if xml.len() as u64 > MAX_XLIFF_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XLIFF input exceeds {MAX_XLIFF_BYTES} bytes"
        )));
    }
    let mut reader = NsReader::from_reader(Cursor::new(xml.as_bytes()));
    reader.config_mut().trim_text(false);
    let mut parser = XliffParser::default();
    let mut buffer = Vec::new();

    loop {
        parser.events = parser.events.saturating_add(1);
        if parser.events > max_events {
            return Err(Error::LimitExceeded(format!(
                "XLIFF input exceeds {max_events} XML events"
            )));
        }
        let decoder = reader.decoder();
        let (namespace, event) = reader.read_resolved_event_into(&mut buffer)?;
        match event {
            Event::Start(start) => {
                let raw_tag: &[u8] = start.as_ref();
                if raw_tag.len() > MAX_XLIFF_TAG_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "XLIFF XML tag exceeds {MAX_XLIFF_TAG_BYTES} bytes"
                    )));
                }
                let frame = parser.start_element(&start, &namespace, decoder, false)?;
                if parser.stack.len() >= MAX_XLIFF_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "XLIFF nesting exceeds {MAX_XLIFF_DEPTH} elements"
                    )));
                }
                parser.stack.push(frame);
            }
            Event::Empty(start) => {
                let raw_tag: &[u8] = start.as_ref();
                if raw_tag.len() > MAX_XLIFF_TAG_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "XLIFF XML tag exceeds {MAX_XLIFF_TAG_BYTES} bytes"
                    )));
                }
                let frame = parser.start_element(&start, &namespace, decoder, true)?;
                if frame.core && parser.capture.is_some() && is_placeholder(&frame.name) {
                    let attributes = read_attributes(&start, reader.decoder())?;
                    parser.append_text(&placeholder_text(&frame.name, &attributes))?;
                    parser.warnings.placeholder_codes =
                        parser.warnings.placeholder_codes.saturating_add(1);
                }
                parser.end_element(frame)?;
            }
            Event::Text(text) => {
                let decoded = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid XLIFF text encoding: {error}"))
                })?;
                let unescaped = quick_xml::escape::unescape(&decoded).map_err(|error| {
                    Error::InvalidInput(format!("invalid XLIFF text entity: {error}"))
                })?;
                parser.append_text(&unescaped)?;
            }
            Event::CData(text) => {
                let decoded = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid XLIFF CDATA encoding: {error}"))
                })?;
                parser.append_text(&decoded)?;
            }
            Event::GeneralRef(reference) => {
                let decoded = crate::ooxml::decode_xml_reference(&reference, "XLIFF text")?;
                parser.append_text(&decoded)?;
            }
            Event::End(end) => {
                let name = local_name(end.name().as_ref())?;
                let Some(frame) = parser.stack.pop() else {
                    return Err(Error::InvalidInput(
                        "XLIFF XML has an unmatched closing tag".into(),
                    ));
                };
                if frame.name != name {
                    return Err(Error::InvalidInput(
                        "XLIFF XML element nesting is malformed".into(),
                    ));
                }
                parser.end_element(frame)?;
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "XLIFF document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    if !parser.root_seen || !parser.root_closed || !parser.stack.is_empty() {
        return Err(Error::InvalidInput("XLIFF document is incomplete".into()));
    }
    if parser.current_unit.is_some() || parser.current_file.is_some() || parser.capture.is_some() {
        return Err(Error::InvalidInput(
            "XLIFF document ended inside a localization unit".into(),
        ));
    }
    if parser.unit_count == 0 {
        return Err(Error::InvalidInput(
            "XLIFF document contains no localization units".into(),
        ));
    }
    let blocks = render_documents(
        &parser.files,
        parser.source_language.as_deref(),
        parser.target_language.as_deref(),
    )?;
    if blocks.len() <= 1 {
        return Err(Error::InvalidInput(
            "XLIFF document contains no readable source/target units".into(),
        ));
    }
    Ok((blocks, warning_messages(parser.warnings)))
}

impl XliffParser {
    fn start_element(
        &mut self,
        start: &BytesStart<'_>,
        namespace: &ResolveResult<'_>,
        decoder: quick_xml::encoding::Decoder,
        is_empty: bool,
    ) -> Result<ElementFrame> {
        if self.root_closed {
            return Err(Error::InvalidInput(
                "XLIFF XML contains content after its root element".into(),
            ));
        }
        let name = local_name(start.name().as_ref())?;
        let attributes = read_attributes(start, decoder)?;
        let is_root = !self.root_seen;
        let version = if is_root {
            if name != "xliff" {
                return Err(Error::InvalidInput(
                    "XLIFF root element must be <xliff>".into(),
                ));
            }
            let version =
                determine_version(namespace, attributes.get("version").map(String::as_str))?;
            self.version = Some(version);
            self.source_language = attribute_local(&attributes, "srcLang").cloned();
            self.target_language = attribute_local(&attributes, "trgLang").cloned();
            self.root_seen = true;
            version
        } else {
            self.version.ok_or_else(|| {
                Error::InvalidInput("XLIFF elements appeared before the root".into())
            })?
        };
        let core = namespace_matches(namespace, version);
        let mut action = None;

        if core {
            match name.as_str() {
                "file" if self.current_file.is_none() => {
                    if self.files.len() >= MAX_XLIFF_FILES {
                        return Err(Error::LimitExceeded(format!(
                            "XLIFF exceeds {MAX_XLIFF_FILES} files"
                        )));
                    }
                    self.current_file = Some(FileDocument {
                        original: attribute_local(&attributes, "original").cloned(),
                        source_language: attribute_local(&attributes, "source-language")
                            .cloned()
                            .or_else(|| self.source_language.clone()),
                        target_language: attribute_local(&attributes, "target-language")
                            .cloned()
                            .or_else(|| self.target_language.clone()),
                        ..FileDocument::default()
                    });
                    action = Some(EndAction::File);
                }
                "group" if self.current_file.is_some() => {
                    let label = attribute_local(&attributes, "name")
                        .or_else(|| attribute_local(&attributes, "resname"))
                        .or_else(|| attribute_local(&attributes, "id"))
                        .filter(|value| !value.is_empty())
                        .cloned();
                    if let Some(label) = label {
                        self.group_path
                            .push(truncate(&label, MAX_XLIFF_GROUP_LABEL_BYTES));
                    } else {
                        self.group_path.push(String::new());
                    }
                    action = Some(EndAction::Group);
                }
                "trans-unit" if version == XliffVersion::V12 && self.current_unit.is_none() => {
                    self.start_unit(&attributes, true)?;
                    action = Some(EndAction::Unit);
                }
                "unit" if version == XliffVersion::V2 && self.current_unit.is_none() => {
                    self.start_unit(&attributes, false)?;
                    action = Some(EndAction::Unit);
                }
                "segment" | "ignorable"
                    if version == XliffVersion::V2
                        && self.current_unit.is_some()
                        && !self.inside_alt_trans() =>
                {
                    let unit = self.current_unit.as_mut().expect("unit was checked");
                    if unit.segments.len() >= MAX_XLIFF_SEGMENTS_PER_UNIT {
                        return Err(Error::LimitExceeded(format!(
                            "XLIFF unit exceeds {MAX_XLIFF_SEGMENTS_PER_UNIT} segments"
                        )));
                    }
                    unit.segments.push(Segment {
                        state: attribute_local(&attributes, "state").cloned(),
                        ignorable: name == "ignorable",
                        ..Segment::default()
                    });
                    self.current_segment = Some(unit.segments.len() - 1);
                    self.segment_count = self.segment_count.saturating_add(1);
                    if self.segment_count > MAX_XLIFF_TOTAL_SEGMENTS {
                        return Err(Error::LimitExceeded(format!(
                            "XLIFF exceeds {MAX_XLIFF_TOTAL_SEGMENTS} total segments"
                        )));
                    }
                }
                "source" | "target" if self.can_capture_source_target(&name, version) => {
                    let index = self.ensure_segment(version)?;
                    let segment = &mut self
                        .current_unit
                        .as_mut()
                        .expect("unit was checked")
                        .segments[index];
                    let capture = if name == "source" {
                        segment.source = Some(String::new());
                        segment.source_language = attribute_local(&attributes, "lang").cloned();
                        TextCapture::Source(index)
                    } else {
                        segment.target = Some(String::new());
                        segment.target_language = attribute_local(&attributes, "lang").cloned();
                        segment.target_state = attribute_local(&attributes, "state").cloned();
                        TextCapture::Target(index)
                    };
                    self.capture = Some(capture);
                    action = Some(EndAction::Capture(capture));
                }
                "g" | "pc"
                    if !is_empty
                        && matches!(
                            self.capture,
                            Some(TextCapture::Source(_) | TextCapture::Target(_))
                        ) =>
                {
                    let id = attribute_local(&attributes, "id")
                        .or_else(|| attribute_local(&attributes, "dataRefStart"))
                        .map(|value| truncate(value, 128))
                        .unwrap_or_default();
                    let marker = if id.is_empty() {
                        format!("[{name}]")
                    } else {
                        format!("[{name}:{id}]")
                    };
                    self.append_text(&marker)?;
                    self.warnings.placeholder_codes =
                        self.warnings.placeholder_codes.saturating_add(1);
                    let close = format!("[/{name}]");
                    action = Some(EndAction::InlineClose(close));
                }
                "note" if self.current_unit.is_some() && self.capture.is_none() => {
                    if let Some(unit) = self.current_unit.as_ref()
                        && unit.notes.len() >= MAX_XLIFF_NOTES_PER_UNIT
                    {
                        return Err(Error::LimitExceeded(format!(
                            "XLIFF unit exceeds {MAX_XLIFF_NOTES_PER_UNIT} notes"
                        )));
                    }
                    self.note = Some(NoteBuilder {
                        label: attribute_local(&attributes, "from")
                            .or_else(|| attribute_local(&attributes, "category"))
                            .cloned()
                            .unwrap_or_default(),
                        text: String::new(),
                    });
                    self.capture = Some(TextCapture::Note);
                    action = Some(EndAction::Capture(TextCapture::Note));
                }
                "context"
                    if version == XliffVersion::V12
                        && self.current_unit.is_some()
                        && self.capture.is_none() =>
                {
                    let context_type = attribute_local(&attributes, "context-type")
                        .cloned()
                        .unwrap_or_else(|| "context".into());
                    self.context = Some(format!("{context_type}: "));
                    self.capture = Some(TextCapture::Context);
                    action = Some(EndAction::Capture(TextCapture::Context));
                }
                "bin-unit" | "skeleton" | "external-file" | "data" => {
                    self.warnings.skipped_structural_elements =
                        self.warnings.skipped_structural_elements.saturating_add(1);
                }
                _ => {}
            }
        }

        Ok(ElementFrame { name, core, action })
    }

    fn start_unit(&mut self, attributes: &HashMap<String, String>, version12: bool) -> Result<()> {
        if self.current_file.is_none() {
            return Err(Error::InvalidInput(
                "XLIFF localization unit is outside a file".into(),
            ));
        }
        self.unit_count = self.unit_count.saturating_add(1);
        if self.unit_count > MAX_XLIFF_UNITS {
            return Err(Error::LimitExceeded(format!(
                "XLIFF exceeds {MAX_XLIFF_UNITS} units"
            )));
        }
        self.current_unit = Some(Unit {
            id: attribute_local(attributes, "id").cloned(),
            name: attribute_local(attributes, "name")
                .or_else(|| attribute_local(attributes, "resname"))
                .cloned(),
            translate: attribute_local(attributes, "translate").cloned(),
            approved: attribute_local(attributes, "approved").cloned(),
            group_path: {
                let mut path = String::new();
                for part in self.group_path.iter().filter(|part| !part.is_empty()) {
                    let separator = if path.is_empty() { "" } else { " / " };
                    if path.len() + separator.len() + part.len() > MAX_XLIFF_GROUP_PATH_BYTES {
                        self.warnings.truncated_group_paths =
                            self.warnings.truncated_group_paths.saturating_add(1);
                        break;
                    }
                    path.push_str(separator);
                    path.push_str(part);
                }
                path
            },
            segments: if version12 {
                vec![Segment::default()]
            } else {
                Vec::new()
            },
            ..Unit::default()
        });
        self.current_segment = version12.then_some(0);
        if version12 {
            self.segment_count = self.segment_count.saturating_add(1);
            if self.segment_count > MAX_XLIFF_TOTAL_SEGMENTS {
                return Err(Error::LimitExceeded(format!(
                    "XLIFF exceeds {MAX_XLIFF_TOTAL_SEGMENTS} total segments"
                )));
            }
        }
        Ok(())
    }

    fn can_capture_source_target(&self, name: &str, version: XliffVersion) -> bool {
        if self.current_unit.is_none() || self.capture.is_some() || self.inside_alt_trans() {
            return false;
        }
        match version {
            XliffVersion::V12 => self
                .stack
                .last()
                .is_some_and(|frame| frame.core && frame.name == "trans-unit"),
            XliffVersion::V2 => {
                self.stack.last().is_some_and(|frame| {
                    frame.core && matches!(frame.name.as_str(), "segment" | "ignorable")
                }) && self.current_segment.is_some()
                    && matches!(name, "source" | "target")
            }
        }
    }

    fn ensure_segment(&mut self, version: XliffVersion) -> Result<usize> {
        if let Some(index) = self.current_segment {
            return Ok(index);
        }
        if version == XliffVersion::V12 {
            return Err(Error::InvalidInput(
                "XLIFF 1.2 source/target has no translation unit".into(),
            ));
        }
        Err(Error::InvalidInput(
            "XLIFF 2.x source/target is outside a segment".into(),
        ))
    }

    fn inside_alt_trans(&self) -> bool {
        self.stack
            .iter()
            .any(|frame| frame.core && frame.name == "alt-trans")
    }

    fn append_text(&mut self, text: &str) -> Result<()> {
        let Some(capture) = self.capture else {
            if self.stack.is_empty() && !text.trim_matches('\u{feff}').trim().is_empty() {
                return Err(Error::InvalidInput(
                    "XLIFF XML has text outside its root element".into(),
                ));
            }
            return Ok(());
        };
        let total = self.text_bytes.saturating_add(text.len());
        if total > MAX_XLIFF_TOTAL_TEXT {
            return Err(Error::LimitExceeded(format!(
                "XLIFF extracted text exceeds {MAX_XLIFF_TOTAL_TEXT} bytes"
            )));
        }
        self.text_bytes = total;
        let field = match capture {
            TextCapture::Source(index) => self
                .current_unit
                .as_mut()
                .and_then(|unit| unit.segments.get_mut(index))
                .and_then(|segment| segment.source.as_mut()),
            TextCapture::Target(index) => self
                .current_unit
                .as_mut()
                .and_then(|unit| unit.segments.get_mut(index))
                .and_then(|segment| segment.target.as_mut()),
            TextCapture::Note => self.note.as_mut().map(|note| &mut note.text),
            TextCapture::Context => self.context.as_mut(),
        }
        .ok_or_else(|| Error::InvalidInput("XLIFF text capture lost its container".into()))?;
        if field.len().saturating_add(text.len()) > MAX_XLIFF_TEXT_PER_FIELD {
            return Err(Error::LimitExceeded(format!(
                "XLIFF text field exceeds {MAX_XLIFF_TEXT_PER_FIELD} bytes"
            )));
        }
        field.push_str(text);
        Ok(())
    }

    fn end_element(&mut self, frame: ElementFrame) -> Result<()> {
        if frame.core {
            match frame.action {
                Some(EndAction::Capture(capture)) => {
                    if self.capture == Some(capture) {
                        self.capture = None;
                    }
                    match capture {
                        TextCapture::Note => {
                            if let Some(note) = self.note.take() {
                                let text = note.text.trim();
                                if !text.is_empty() {
                                    let formatted = if note.label.is_empty() {
                                        text.to_owned()
                                    } else {
                                        format!("{}: {text}", truncate(&note.label, 256))
                                    };
                                    if let Some(unit) = self.current_unit.as_mut() {
                                        unit.notes.push(formatted);
                                    }
                                }
                            }
                        }
                        TextCapture::Context => {
                            if let Some(context) = self.context.take()
                                && !context.trim().is_empty()
                                && let Some(unit) = self.current_unit.as_mut()
                            {
                                unit.contexts.push(context.trim().to_owned());
                            }
                        }
                        TextCapture::Source(_) | TextCapture::Target(_) => {}
                    }
                }
                Some(EndAction::Unit) => self.finish_unit()?,
                Some(EndAction::InlineClose(marker)) => self.append_text(&marker)?,
                Some(EndAction::Group) => {
                    self.group_path.pop();
                }
                Some(EndAction::File) => self.finish_file()?,
                None => {}
            }
        }
        if self.stack.is_empty() {
            self.root_closed = true;
        }
        Ok(())
    }

    fn finish_unit(&mut self) -> Result<()> {
        let Some(unit) = self.current_unit.take() else {
            return Err(Error::InvalidInput(
                "XLIFF unit ended without an open unit".into(),
            ));
        };
        if unit.segments.is_empty() {
            self.warnings.malformed_units = self.warnings.malformed_units.saturating_add(1);
        }
        self.warnings.empty_targets = self.warnings.empty_targets.saturating_add(
            unit.segments
                .iter()
                .filter(|segment| {
                    segment
                        .target
                        .as_deref()
                        .is_some_and(|target| target.trim().is_empty())
                })
                .count(),
        );
        if let Some(file) = self.current_file.as_mut() {
            file.units.push(unit);
        } else {
            return Err(Error::InvalidInput(
                "XLIFF unit ended outside a file".into(),
            ));
        }
        self.current_segment = None;
        Ok(())
    }

    fn finish_file(&mut self) -> Result<()> {
        let Some(file) = self.current_file.take() else {
            return Err(Error::InvalidInput(
                "XLIFF file ended without an open file".into(),
            ));
        };
        self.files.push(file);
        self.group_path.clear();
        Ok(())
    }
}

fn determine_version(namespace: &ResolveResult<'_>, version: Option<&str>) -> Result<XliffVersion> {
    let namespace = match namespace {
        ResolveResult::Bound(namespace) => namespace.as_ref(),
        ResolveResult::Unbound => {
            return Err(Error::InvalidInput(
                "XLIFF root has no recognized namespace".into(),
            ));
        }
        ResolveResult::Unknown(prefix) => {
            return Err(Error::InvalidInput(format!(
                "XLIFF root uses undeclared namespace prefix '{}'",
                String::from_utf8_lossy(prefix)
            )));
        }
    };
    match namespace {
        XLIFF_12_NAMESPACE if version.is_none_or(|value| value == "1.2") => Ok(XliffVersion::V12),
        XLIFF_20_NAMESPACE if version.is_none_or(|value| value.starts_with("2.")) => {
            Ok(XliffVersion::V2)
        }
        _ => Err(Error::Unsupported(
            "unsupported XLIFF namespace/version combination".into(),
        )),
    }
}

fn namespace_matches(namespace: &ResolveResult<'_>, version: XliffVersion) -> bool {
    let expected = match version {
        XliffVersion::V12 => XLIFF_12_NAMESPACE,
        XliffVersion::V2 => XLIFF_20_NAMESPACE,
    };
    matches!(namespace, ResolveResult::Bound(actual) if actual.as_ref() == expected)
}

fn read_attributes(
    start: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
) -> Result<HashMap<String, String>> {
    let mut attributes = HashMap::new();
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| {
            Error::InvalidInput(format!("invalid XLIFF XML attribute: {error}"))
        })?;
        let name = std::str::from_utf8(attribute.key.as_ref())
            .map_err(|error| Error::InvalidInput(format!("invalid XLIFF attribute name: {error}")))?
            .to_owned();
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
            .map_err(|error| {
                Error::InvalidInput(format!("invalid XLIFF attribute value: {error}"))
            })?
            .into_owned();
        attributes.insert(name, value);
    }
    Ok(attributes)
}

fn local_name(name: &[u8]) -> Result<String> {
    let local = crate::ooxml::local_name(name);
    std::str::from_utf8(local)
        .map(str::to_owned)
        .map_err(|error| Error::InvalidInput(format!("invalid XLIFF XML name: {error}")))
}

fn attribute_local<'a>(attributes: &'a HashMap<String, String>, name: &str) -> Option<&'a String> {
    attributes.iter().find_map(|(key, value)| {
        (crate::ooxml::local_name(key.as_bytes()) == name.as_bytes()).then_some(value)
    })
}

fn is_placeholder(name: &str) -> bool {
    matches!(
        name,
        "x" | "ph" | "bx" | "ex" | "it" | "sc" | "ec" | "cp" | "sm" | "em" | "g" | "pc"
    )
}

fn placeholder_text(name: &str, attributes: &HashMap<String, String>) -> String {
    let id = attribute_local(attributes, "id")
        .or_else(|| attribute_local(attributes, "equiv"))
        .or_else(|| attribute_local(attributes, "dataRef"))
        .map(|value| truncate(value, 128))
        .unwrap_or_default();
    if id.is_empty() {
        format!("[{name}]")
    } else {
        format!("[{name}:{id}]")
    }
}

fn truncate(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn warning_messages(warnings: ParseWarnings) -> Vec<String> {
    let mut messages = Vec::new();
    if warnings.malformed_units > 0 {
        messages.push(format!(
            "{} XLIFF unit(s) with no segments were omitted",
            warnings.malformed_units
        ));
    }
    if warnings.empty_targets > 0 {
        messages.push(format!(
            "{} XLIFF target segment(s) are empty",
            warnings.empty_targets
        ));
    }
    if warnings.placeholder_codes > 0 {
        messages.push(format!(
            "{} XLIFF inline placeholder code(s) are shown as bracketed labels",
            warnings.placeholder_codes
        ));
    }
    if warnings.skipped_structural_elements > 0 {
        messages.push(format!(
            "{} XLIFF binary/skeleton structural element(s) were omitted",
            warnings.skipped_structural_elements
        ));
    }
    if warnings.truncated_group_paths > 0 {
        messages.push(format!(
            "{} XLIFF group path(s) were truncated to fit the preview text budget",
            warnings.truncated_group_paths
        ));
    }
    messages
}

fn render_documents(
    files: &[FileDocument],
    root_source_language: Option<&str>,
    root_target_language: Option<&str>,
) -> Result<Vec<HtmlBlock>> {
    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: "XLIFF translation catalog".into(),
    }];
    let mut emitted_units = 0usize;
    for (file_index, file) in files.iter().enumerate() {
        if files.len() > 1 || file.original.is_some() {
            let title = file
                .original
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(|value| format!("File: {}", truncate(value, 1_024)))
                .unwrap_or_else(|| format!("File {}", file_index + 1));
            push_output_block(
                &mut blocks,
                HtmlBlock::Heading {
                    level: 2,
                    text: title,
                },
            )?;
        }
        let source_language = file
            .source_language
            .as_deref()
            .or(root_source_language)
            .unwrap_or("source");
        let target_language = file
            .target_language
            .as_deref()
            .or(root_target_language)
            .unwrap_or("target");
        push_output_block(
            &mut blocks,
            HtmlBlock::Paragraph {
                text: format!("Languages: {source_language} → {target_language}"),
            },
        )?;
        for unit in &file.units {
            emitted_units += 1;
            if emitted_units > MAX_XLIFF_UNITS {
                return Err(Error::LimitExceeded(format!(
                    "XLIFF exceeds {MAX_XLIFF_UNITS} rendered units"
                )));
            }
            let mut heading = String::from("Unit");
            if let Some(id) = unit.id.as_deref().filter(|value| !value.is_empty()) {
                heading.push(' ');
                heading.push_str(&truncate(id, 1_024));
            }
            if let Some(name) = unit.name.as_deref().filter(|value| !value.is_empty()) {
                heading.push_str(" — ");
                heading.push_str(&truncate(name, 1_024));
            }
            push_output_block(
                &mut blocks,
                HtmlBlock::Heading {
                    level: 3,
                    text: heading,
                },
            )?;
            if !unit.group_path.is_empty() {
                push_output_block(
                    &mut blocks,
                    HtmlBlock::Paragraph {
                        text: format!("Group: {}", unit.group_path),
                    },
                )?;
            }
            if unit
                .translate
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("no"))
            {
                push_output_block(
                    &mut blocks,
                    HtmlBlock::Paragraph {
                        text: "Translation: disabled for this unit".into(),
                    },
                )?;
            }
            if unit
                .approved
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("no"))
            {
                push_output_block(
                    &mut blocks,
                    HtmlBlock::Paragraph {
                        text: "Review: not approved".into(),
                    },
                )?;
            }
            for (segment_index, segment) in unit.segments.iter().enumerate() {
                if unit.segments.len() > 1
                    || segment.ignorable
                    || segment.state.is_some()
                    || segment.target_state.is_some()
                {
                    let label = if segment.ignorable {
                        "Ignorable segment".to_owned()
                    } else {
                        format!("Segment {}", segment_index + 1)
                    };
                    let label = segment
                        .state
                        .as_deref()
                        .or(segment.target_state.as_deref())
                        .filter(|value| !value.is_empty())
                        .map_or(label.clone(), |state| format!("{label} [{state}]"));
                    push_output_block(&mut blocks, HtmlBlock::Paragraph { text: label })?;
                }
                let source = segment.source.as_deref().unwrap_or("[no source text]");
                let source_language = segment
                    .source_language
                    .as_deref()
                    .unwrap_or(source_language);
                push_output_block(
                    &mut blocks,
                    HtmlBlock::Paragraph {
                        text: format!("Source ({source_language}): {source}"),
                    },
                )?;
                let target_language = segment
                    .target_language
                    .as_deref()
                    .unwrap_or(target_language);
                let target = match segment.target.as_deref() {
                    None => "[untranslated]",
                    Some("") => "[empty target]",
                    Some(value) => value,
                };
                push_output_block(
                    &mut blocks,
                    HtmlBlock::Paragraph {
                        text: format!("Target ({target_language}): {target}"),
                    },
                )?;
            }
            for context in &unit.contexts {
                push_output_block(
                    &mut blocks,
                    HtmlBlock::Paragraph {
                        text: format!("Context: {context}"),
                    },
                )?;
            }
            for note in &unit.notes {
                push_output_block(
                    &mut blocks,
                    HtmlBlock::Paragraph {
                        text: format!("Note: {note}"),
                    },
                )?;
            }
        }
    }
    Ok(blocks)
}

fn push_output_block(blocks: &mut Vec<HtmlBlock>, block: HtmlBlock) -> Result<()> {
    if blocks.len() >= MAX_XLIFF_OUTPUT_BLOCKS {
        return Err(Error::LimitExceeded(format!(
            "XLIFF preview exceeds {MAX_XLIFF_OUTPUT_BLOCKS} output blocks"
        )));
    }
    blocks.push(block);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const XLIFF_12: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xliff version="1.2" xmlns="urn:oasis:names:tc:xliff:document:1.2">
  <file original="menu.xml" source-language="en" target-language="fr" datatype="plaintext">
    <body>
      <group resname="Navigation">
        <trans-unit id="open" approved="no">
          <source>Open <g id="1">file</g><x id="2"/></source>
          <target state="needs-review-translation">Ouvrir <g id="1">fichier</g><x id="2"/></target>
          <context-group><context context-type="location">Main menu</context></context-group>
          <note from="developer">Use a short label.</note>
          <alt-trans><source>Ignored alternate</source><target>Ignored alternate</target></alt-trans>
        </trans-unit>
        <trans-unit id="save"><source>Save</source></trans-unit>
      </group>
    </body>
  </file>
</xliff>"#;

    const XLIFF_20: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xliff xmlns="urn:oasis:names:tc:xliff:document:2.0" version="2.0" srcLang="en" trgLang="ja">
  <file id="f1" original="strings.json">
    <unit id="greeting" name="Greeting">
      <notes><note category="location">Home screen</note></notes>
      <segment state="translated">
        <source>Hello <pc id="1">world</pc><ph id="2" equiv="{count}"/>!</source>
        <target>こんにちは <pc id="1">世界</pc><ph id="2" equiv="{count}"/>！</target>
      </segment>
      <segment state="initial"><source>Goodbye</source></segment>
    </unit>
  </file>
</xliff>"#;

    #[test]
    fn parses_xliff12_source_target_groups_notes_context_and_alternates() {
        let (blocks, warnings) = parse_xliff_blocks(XLIFF_12, 10_000).unwrap();
        let paragraphs = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Paragraph { text } => Some(text.as_str()),
                HtmlBlock::Heading { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            paragraphs
                .iter()
                .any(|text| text.contains("Languages: en → fr"))
        );
        assert!(paragraphs.iter().any(|text| text == &"Group: Navigation"));
        assert!(
            paragraphs
                .iter()
                .any(|text| text.contains("Source (en): Open [g:1]file[/g][x:2]"))
        );
        assert!(
            paragraphs
                .iter()
                .any(|text| { text.contains("Target (fr): Ouvrir [g:1]fichier[/g][x:2]") })
        );
        assert!(
            paragraphs
                .iter()
                .any(|text| text.contains("Segment 1 [needs-review-translation]"))
        );
        assert!(
            paragraphs
                .iter()
                .any(|text| text.contains("Context: location: Main menu"))
        );
        assert!(
            paragraphs
                .iter()
                .any(|text| { text.contains("Note: developer: Use a short label.") })
        );
        assert!(
            paragraphs
                .iter()
                .any(|text| text.contains("Review: not approved"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("placeholder code"))
        );
        assert!(
            !paragraphs
                .iter()
                .any(|text| text.contains("Ignored alternate"))
        );
    }

    #[test]
    fn parses_xliff20_segments_and_untranslated_targets() {
        let (blocks, warnings) = parse_xliff_blocks(XLIFF_20, 10_000).unwrap();
        let paragraphs = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Paragraph { text } => Some(text.as_str()),
                HtmlBlock::Heading { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(paragraphs.iter().any(|text| text == &"Languages: en → ja"));
        assert!(
            paragraphs
                .iter()
                .any(|text| text == &"Unit greeting — Greeting")
        );
        assert!(
            paragraphs
                .iter()
                .any(|text| text.contains("Source (en): Hello [pc:1]world[/pc][ph:2]!"))
        );
        assert!(paragraphs.iter().any(|text| {
            text.contains("Target (ja): こんにちは [pc:1]世界[/pc][ph:2]！")
        }));
        assert!(
            paragraphs
                .iter()
                .any(|text| text.contains("Segment 2 [initial]"))
        );
        assert!(
            paragraphs
                .iter()
                .any(|text| text.contains("Target (ja): [untranslated]"))
        );
        assert!(
            paragraphs
                .iter()
                .any(|text| text.contains("Note: location: Home screen"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("placeholder code"))
        );
    }

    #[test]
    fn detects_only_supported_xliff_root_namespaces_and_rejects_dtds() {
        assert!(looks_like_xliff_prefix(XLIFF_12.as_bytes()));
        assert!(looks_like_xliff_prefix(XLIFF_20.as_bytes()));
        assert!(!looks_like_xliff_prefix(b"<xliff xmlns=\"urn:other\"/>"));
        let doctype = r#"<!DOCTYPE xliff [<!ENTITY x "danger">]><xliff version="1.2" xmlns="urn:oasis:names:tc:xliff:document:1.2"><file><body/></file></xliff>"#;
        assert!(parse_xliff_blocks(doctype, 1_000).is_err());
    }

    #[test]
    fn enforces_text_field_and_xml_event_budgets() {
        let long_source = "x".repeat(MAX_XLIFF_TEXT_PER_FIELD + 1);
        let source = format!(
            "<xliff version=\"1.2\" xmlns=\"urn:oasis:names:tc:xliff:document:1.2\"><file><body><trans-unit id=\"u1\"><source>{long_source}</source></trans-unit></body></file></xliff>"
        );
        assert!(matches!(
            parse_xliff_blocks(&source, 10_000),
            Err(Error::LimitExceeded(_))
        ));
        assert!(matches!(
            parse_xliff_blocks(XLIFF_20, 1),
            Err(Error::LimitExceeded(_))
        ));
    }
}
