//! Bounded Flat OPC (Open XML package serialized as one XML document).
//!
//! Flat OPC stores the parts of a DOCX/XLSX/PPTX package as `pkg:part`
//! elements. This adapter reconstructs an in-memory ZIP only after validating
//! the package namespace, part names, XML/base64 payloads and aggregate byte
//! limits, then delegates to the existing Office Open XML renderers.

use std::collections::HashSet;
use std::io::{Cursor, Write};
use std::path::Path;

use base64::Engine;
use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ooxml::local_name;

const FLAT_OPC_NAMESPACE: &[u8] = b"http://schemas.microsoft.com/office/2006/xmlPackage";
const MAX_FLAT_OPC_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FLAT_OPC_ENTRY_BYTES: usize = 16 * 1024 * 1024;
const MAX_FLAT_OPC_EXPANDED_BYTES: usize = 128 * 1024 * 1024;
const MAX_FLAT_OPC_PARTS: usize = 100_000;
const MAX_FLAT_OPC_EVENTS: usize = 500_000;
const MAX_FLAT_OPC_NAME_BYTES: usize = 1_024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OfficeKind {
    Docx,
    Xlsx,
    Pptx,
}

struct Part {
    name: String,
    bytes: Vec<u8>,
}

struct FlatOpcPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for FlatOpcPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "flat-opc".into();
        page.description = "Flat OPC was validated and reconstructed as a bounded inert Open XML package; macros and external resources are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.contains("xmlPackage")
        && text.contains("package")
        && (text.contains("<pkg:part") || text.contains(":part"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_FLAT_OPC_BYTES),
        "Flat OPC input",
    )?;
    convert_bytes(&bytes, options, sink)
}

pub(crate) fn convert_bytes(
    bytes: &[u8],
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    if bytes.len() as u64 > options.max_input_bytes.min(MAX_FLAT_OPC_BYTES) {
        return Err(Error::LimitExceeded("Flat OPC input exceeds limit".into()));
    }
    let (zip_bytes, kind) = build_zip(bytes)?;
    let warnings = vec![
        "Flat OPC parts were reconstructed as a bounded DOCX/XLSX/PPTX package; package XML is rendered but macros, external relationships, URLs and embedded active content remain inert".into(),
        "Flat OPC XML and base64 parts are validated before reconstruction; part names stay confined to the in-memory package and no filesystem extraction occurs".into(),
    ];
    let mut page_sink = FlatOpcPageSink {
        inner: sink,
        warnings: &warnings,
    };
    let mut renderer_warnings = match kind {
        OfficeKind::Docx => crate::ooxml::docx::convert_bytes(&zip_bytes, options, &mut page_sink)?,
        OfficeKind::Xlsx => crate::ooxml::xlsx::convert_bytes(&zip_bytes, options, &mut page_sink)?,
        OfficeKind::Pptx => crate::ooxml::pptx::convert_bytes(&zip_bytes, options, &mut page_sink)?,
    };
    renderer_warnings.extend(warnings);
    renderer_warnings.sort();
    renderer_warnings.dedup();
    Ok(renderer_warnings)
}

fn build_zip(bytes: &[u8]) -> Result<(Vec<u8>, OfficeKind)> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut events = 0usize;
    let root = loop {
        let event = next_event(&mut reader, &mut buffer, &mut events)?;
        match event {
            Event::Decl(_) | Event::Comment(_) | Event::PI(_) => continue,
            Event::Text(text)
                if text
                    .decode()
                    .map_err(|error| {
                        Error::InvalidInput(format!("invalid Flat OPC text: {error}"))
                    })?
                    .trim()
                    .is_empty() =>
            {
                continue;
            }
            other => break other,
        }
    };
    let Event::Start(root_start) = root else {
        return Err(Error::InvalidInput(
            "Flat OPC document has no package root".into(),
        ));
    };
    if local_name(root_start.name().as_ref()) != b"package"
        || !has_flat_opc_namespace(&root_start, reader.decoder())?
    {
        return Err(Error::InvalidInput(
            "Flat OPC root must be pkg:package in the Office XML package namespace".into(),
        ));
    }
    let mut parts = Vec::new();
    let mut names = HashSet::new();
    let mut expanded_bytes = 0usize;
    loop {
        let event = next_event(&mut reader, &mut buffer, &mut events)?;
        match event {
            Event::Start(start) if local_name(start.name().as_ref()) == b"part" => {
                if parts.len() >= MAX_FLAT_OPC_PARTS {
                    return Err(Error::LimitExceeded(format!(
                        "Flat OPC parts exceed {MAX_FLAT_OPC_PARTS}"
                    )));
                }
                let part = parse_part(
                    &mut reader,
                    &mut buffer,
                    &mut events,
                    start,
                    &mut expanded_bytes,
                )?;
                if !names.insert(part.name.clone()) {
                    return Err(Error::InvalidInput(format!(
                        "Flat OPC contains duplicate part {}",
                        part.name
                    )));
                }
                parts.push(part);
            }
            Event::End(end) if local_name(end.name().as_ref()) == b"package" => break,
            Event::Text(text)
                if text
                    .decode()
                    .map_err(|error| {
                        Error::InvalidInput(format!("invalid Flat OPC text: {error}"))
                    })?
                    .trim()
                    .is_empty() => {}
            Event::Comment(_) | Event::PI(_) => {}
            Event::DocType(_) | Event::GeneralRef(_) => {
                return Err(Error::InvalidInput(
                    "Flat OPC document type declarations are unsupported".into(),
                ));
            }
            Event::Eof => {
                return Err(Error::InvalidInput(
                    "Flat OPC package root is not closed".into(),
                ));
            }
            _ => {
                return Err(Error::InvalidInput(
                    "Flat OPC package contains an unexpected child".into(),
                ));
            }
        }
        buffer.clear();
    }
    loop {
        let tail = next_event(&mut reader, &mut buffer, &mut events)?;
        match tail {
            Event::Eof => break,
            Event::Comment(_) | Event::PI(_) => {}
            Event::Text(text)
                if text
                    .decode()
                    .map_err(|error| {
                        Error::InvalidInput(format!("invalid Flat OPC text: {error}"))
                    })?
                    .trim()
                    .is_empty() => {}
            _ => {
                return Err(Error::InvalidInput(
                    "Flat OPC contains data after the package root".into(),
                ));
            }
        }
        buffer.clear();
    }
    let kind = choose_kind(&names)?;
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    for part in parts {
        writer.start_file(&part.name, options)?;
        writer.write_all(&part.bytes)?;
    }
    Ok((writer.finish()?.into_inner(), kind))
}

fn parse_part<R: std::io::BufRead>(
    reader: &mut Reader<R>,
    buffer: &mut Vec<u8>,
    events: &mut usize,
    start: BytesStart<'static>,
    expanded_bytes: &mut usize,
) -> Result<Part> {
    let name = attribute_value(&start, b"name", reader)?;
    let _content_type = attribute_value(&start, b"contentType", reader)?;
    let name = normalize_part_name(&name)?;
    let mut xml_data = None;
    let mut binary_data = None;
    loop {
        let event = next_event(reader, buffer, events)?;
        match event {
            Event::Start(child) if local_name(child.name().as_ref()) == b"xmlData" => {
                if xml_data.is_some() || binary_data.is_some() {
                    return Err(Error::InvalidInput(
                        "Flat OPC part has multiple payloads".into(),
                    ));
                }
                xml_data = Some(capture_xml_data(reader, buffer, events)?);
            }
            Event::Start(child) if local_name(child.name().as_ref()) == b"binaryData" => {
                if xml_data.is_some() || binary_data.is_some() {
                    return Err(Error::InvalidInput(
                        "Flat OPC part has multiple payloads".into(),
                    ));
                }
                binary_data = Some(capture_binary_data(reader, buffer, events)?);
            }
            Event::End(end) if local_name(end.name().as_ref()) == b"part" => break,
            Event::Text(text)
                if text
                    .decode()
                    .map_err(|error| {
                        Error::InvalidInput(format!("invalid Flat OPC text: {error}"))
                    })?
                    .trim()
                    .is_empty() => {}
            Event::Comment(_) | Event::PI(_) => {}
            Event::DocType(_) | Event::GeneralRef(_) => {
                return Err(Error::InvalidInput(
                    "Flat OPC part contains a document type declaration".into(),
                ));
            }
            _ => {
                return Err(Error::InvalidInput(
                    "Flat OPC part contains an unexpected child".into(),
                ));
            }
        }
        buffer.clear();
    }
    let bytes = xml_data.or(binary_data).ok_or_else(|| {
        Error::InvalidInput(format!("Flat OPC part {name} has no XML or binary payload"))
    })?;
    *expanded_bytes = expanded_bytes
        .checked_add(bytes.len())
        .ok_or_else(|| Error::LimitExceeded("Flat OPC expanded bytes overflow".into()))?;
    if *expanded_bytes > MAX_FLAT_OPC_EXPANDED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Flat OPC expanded parts exceed {MAX_FLAT_OPC_EXPANDED_BYTES} bytes"
        )));
    }
    Ok(Part { name, bytes })
}

fn capture_xml_data<R: std::io::BufRead>(
    reader: &mut Reader<R>,
    buffer: &mut Vec<u8>,
    events: &mut usize,
) -> Result<Vec<u8>> {
    let mut writer = Writer::new(Vec::new());
    let mut depth = 0usize;
    let mut roots = 0usize;
    loop {
        let event = next_event(reader, buffer, events)?;
        match event {
            Event::Start(start) => {
                if depth == 0 {
                    roots += 1;
                }
                depth = depth.saturating_add(1);
                writer.write_event(Event::Start(start))?;
            }
            Event::Empty(empty) => {
                if depth == 0 {
                    roots += 1;
                }
                writer.write_event(Event::Empty(empty))?;
            }
            Event::End(end) => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                writer.write_event(Event::End(end))?;
            }
            Event::Text(text) => writer.write_event(Event::Text(text))?,
            Event::CData(data) => writer.write_event(Event::CData(data))?,
            Event::Comment(comment) => writer.write_event(Event::Comment(comment))?,
            Event::PI(pi) => writer.write_event(Event::PI(pi))?,
            Event::Decl(decl) => writer.write_event(Event::Decl(decl))?,
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "Flat OPC XML payload contains a document type declaration".into(),
                ));
            }
            Event::GeneralRef(_) => {
                return Err(Error::InvalidInput(
                    "Flat OPC XML payload contains a general entity reference".into(),
                ));
            }
            Event::Eof => {
                return Err(Error::InvalidInput(
                    "Flat OPC XML payload is not closed".into(),
                ));
            }
        }
        if writer.get_ref().len() > MAX_FLAT_OPC_ENTRY_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Flat OPC XML part exceeds {MAX_FLAT_OPC_ENTRY_BYTES} bytes"
            )));
        }
        buffer.clear();
    }
    if roots != 1 || writer.get_ref().is_empty() {
        return Err(Error::InvalidInput(
            "Flat OPC xmlData must contain exactly one XML root".into(),
        ));
    }
    Ok(writer.into_inner())
}

fn capture_binary_data<R: std::io::BufRead>(
    reader: &mut Reader<R>,
    buffer: &mut Vec<u8>,
    events: &mut usize,
) -> Result<Vec<u8>> {
    let mut encoded = Vec::new();
    loop {
        let event = next_event(reader, buffer, events)?;
        match event {
            Event::Text(text) => encoded.extend_from_slice(text.as_ref()),
            Event::CData(data) => encoded.extend_from_slice(data.as_ref()),
            Event::End(_) => break,
            Event::Comment(_) | Event::PI(_) => {}
            Event::Start(_) | Event::Empty(_) => {
                return Err(Error::InvalidInput(
                    "Flat OPC binaryData contains nested XML".into(),
                ));
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "Flat OPC binaryData contains a document type declaration".into(),
                ));
            }
            Event::GeneralRef(_) => {
                return Err(Error::InvalidInput(
                    "Flat OPC binaryData contains a general entity reference".into(),
                ));
            }
            Event::Eof => {
                return Err(Error::InvalidInput(
                    "Flat OPC binaryData is not closed".into(),
                ));
            }
            Event::Decl(_) => {
                return Err(Error::InvalidInput(
                    "Flat OPC binaryData contains an XML declaration".into(),
                ));
            }
        }
        if encoded.len() > MAX_FLAT_OPC_ENTRY_BYTES.saturating_mul(2) {
            return Err(Error::LimitExceeded(
                "Flat OPC base64 payload exceeds entry limit".into(),
            ));
        }
        buffer.clear();
    }
    let compact: Vec<u8> = encoded
        .into_iter()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|error| {
            Error::InvalidInput(format!("invalid Flat OPC base64 payload: {error}"))
        })?;
    if decoded.len() > MAX_FLAT_OPC_ENTRY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Flat OPC binary part exceeds {MAX_FLAT_OPC_ENTRY_BYTES} bytes"
        )));
    }
    Ok(decoded)
}

fn next_event<R: std::io::BufRead>(
    reader: &mut Reader<R>,
    buffer: &mut Vec<u8>,
    events: &mut usize,
) -> Result<Event<'static>> {
    *events = events.saturating_add(1);
    if *events > MAX_FLAT_OPC_EVENTS {
        return Err(Error::LimitExceeded(format!(
            "Flat OPC XML events exceed {MAX_FLAT_OPC_EVENTS}"
        )));
    }
    Ok(reader.read_event_into(buffer)?.into_owned())
}

fn has_flat_opc_namespace(
    start: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
) -> Result<bool> {
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| {
            Error::InvalidInput(format!("invalid Flat OPC package attribute: {error}"))
        })?;
        let key = attribute.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            let value = attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
                .map_err(|error| {
                    Error::InvalidInput(format!("invalid Flat OPC namespace: {error}"))
                })?;
            if value.as_bytes() == FLAT_OPC_NAMESPACE {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn attribute_value<R: std::io::BufRead>(
    start: &BytesStart<'_>,
    wanted: &[u8],
    reader: &Reader<R>,
) -> Result<String> {
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| {
            Error::InvalidInput(format!("invalid Flat OPC part attribute: {error}"))
        })?;
        if local_name(attribute.key.as_ref()) == wanted {
            return Ok(attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                .map_err(|error| {
                    Error::InvalidInput(format!("invalid Flat OPC part attribute value: {error}"))
                })?
                .into_owned());
        }
    }
    Err(Error::InvalidInput(format!(
        "Flat OPC part is missing {}",
        String::from_utf8_lossy(wanted)
    )))
}

fn normalize_part_name(value: &str) -> Result<String> {
    let name = value.trim_start_matches('/');
    if name.is_empty()
        || name.len() > MAX_FLAT_OPC_NAME_BYTES
        || name.contains('\\')
        || name
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(Error::InvalidInput(format!(
            "unsafe Flat OPC part name {value:?}"
        )));
    }
    Ok(name.to_owned())
}

fn choose_kind(names: &HashSet<String>) -> Result<OfficeKind> {
    let docx = names.contains("word/document.xml");
    let xlsx = names.contains("xl/workbook.xml");
    let pptx = names.contains("ppt/presentation.xml");
    match (docx, xlsx, pptx) {
        (true, false, false) => Ok(OfficeKind::Docx),
        (false, true, false) => Ok(OfficeKind::Xlsx),
        (false, false, true) => Ok(OfficeKind::Pptx),
        _ => Err(Error::InvalidInput(
            "Flat OPC must contain exactly one DOCX, XLSX or PPTX main part".into(),
        )),
    }
}
