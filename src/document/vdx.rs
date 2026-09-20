//! Legacy Visio XML Drawing (`.vdx`) page preview.
//!
//! VDX carries the same ShapeSheet vocabulary as VSDX but stores pages in one
//! XML document instead of an OPC ZIP package. Each Page subtree is streamed
//! into the shared bounded Visio page renderer; no macros or external links
//! are evaluated.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesEnd, BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::vsdx;
use crate::error::{Error, Result};
use crate::ooxml::{attribute, local_name};

const MAX_VDX_PAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_VDX_PAGES: usize = 10_000;
const MAX_VDX_DEPTH: usize = 256;

#[derive(Debug)]
struct VdxPage {
    id: String,
    name: String,
    is_background: bool,
    references_background: bool,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(512 * 1024 * 1024);
    let mut bytes = Vec::new();
    Read::take(File::open(path)?, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "VDX input exceeds maximum limit of {max_bytes} bytes"
        )));
    }

    let mut reader = Reader::from_reader(bytes.as_slice());
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut root_namespaces = Vec::<(String, String)>::new();
    let mut root_seen = false;
    let mut depth = 0usize;
    let mut events = 0usize;
    let mut page_entry_count = 0usize;
    let mut rendered_page_count = 0usize;
    let mut writer: Option<Writer<Vec<u8>>> = None;
    let mut page: Option<VdxPage> = None;
    let mut page_depth = 0usize;
    let mut warnings = Vec::new();
    let mut saw_background = false;

    loop {
        events += 1;
        if events > options.max_xml_events {
            return Err(Error::LimitExceeded(format!(
                "VDX document exceeds {} XML events",
                options.max_xml_events
            )));
        }
        let event = reader.read_event_into(&mut buffer)?;
        match event {
            Event::Start(start) => {
                let name = local_name(start.name().as_ref()).to_vec();
                if !root_seen {
                    if name.as_slice() != b"VisioDocument" {
                        return Err(Error::InvalidInput(format!(
                            "VDX root is <{}>; expected <VisioDocument>",
                            String::from_utf8_lossy(&name)
                        )));
                    }
                    root_seen = true;
                    root_namespaces = namespace_attributes(&start);
                }
                depth += 1;
                if depth > MAX_VDX_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "VDX document exceeds XML nesting limit {MAX_VDX_DEPTH}"
                    )));
                }
                if writer.is_none() && name.as_slice() == b"Page" {
                    page_entry_count += 1;
                    if page_entry_count > MAX_VDX_PAGES {
                        return Err(Error::LimitExceeded(format!(
                            "VDX document exceeds {MAX_VDX_PAGES} pages"
                        )));
                    }
                    let parsed_page = page_from_start(&start);
                    saw_background |=
                        parsed_page.is_background || parsed_page.references_background;
                    writer = Some(start_page(&root_namespaces)?);
                    writer
                        .as_mut()
                        .expect("writer was initialized")
                        .write_event(Event::Start(start.into_owned()))?;
                    page = Some(parsed_page);
                    page_depth = 1;
                } else if let Some(page_writer) = writer.as_mut() {
                    page_writer.write_event(Event::Start(start.into_owned()))?;
                    page_depth += 1;
                    if page_writer.get_ref().len() > MAX_VDX_PAGE_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "VDX page exceeds {MAX_VDX_PAGE_BYTES} bytes"
                        )));
                    }
                }
            }
            Event::Empty(start) => {
                let name = local_name(start.name().as_ref()).to_vec();
                if !root_seen && name.as_slice() == b"VisioDocument" {
                    root_seen = true;
                    root_namespaces = namespace_attributes(&start);
                }
                if let Some(page_writer) = writer.as_mut() {
                    page_writer.write_event(Event::Empty(start.into_owned()))?;
                    if page_writer.get_ref().len() > MAX_VDX_PAGE_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "VDX page exceeds {MAX_VDX_PAGE_BYTES} bytes"
                        )));
                    }
                } else if name.as_slice() == b"Page" {
                    page_entry_count += 1;
                    if page_entry_count > MAX_VDX_PAGES {
                        return Err(Error::LimitExceeded(format!(
                            "VDX document exceeds {MAX_VDX_PAGES} pages"
                        )));
                    }
                    let page_meta = page_from_start(&start);
                    saw_background |= page_meta.is_background || page_meta.references_background;
                    let mut page_writer = start_page(&root_namespaces)?;
                    page_writer.write_event(Event::Empty(start.into_owned()))?;
                    let page_xml = finish_page(page_writer)?;
                    render_vdx_page(
                        &page_xml,
                        page_meta,
                        options,
                        sink,
                        &mut rendered_page_count,
                        &mut warnings,
                    )?;
                }
            }
            Event::End(end) => {
                depth = depth.saturating_sub(1);
                if let Some(page_writer) = writer.as_mut() {
                    let is_page_end = local_name(end.name().as_ref()) == b"Page" && page_depth == 1;
                    page_writer.write_event(Event::End(end.to_owned()))?;
                    page_depth = page_depth.saturating_sub(1);
                    if page_writer.get_ref().len() > MAX_VDX_PAGE_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "VDX page exceeds {MAX_VDX_PAGE_BYTES} bytes"
                        )));
                    }
                    if is_page_end {
                        let page_writer = writer.take().expect("page writer exists");
                        let page_xml = finish_page(page_writer)?;
                        let page_meta = page.take().expect("page metadata exists");
                        render_vdx_page(
                            &page_xml,
                            page_meta,
                            options,
                            sink,
                            &mut rendered_page_count,
                            &mut warnings,
                        )?;
                        page_depth = 0;
                    }
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "VDX document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            other => {
                if let Some(page_writer) = writer.as_mut() {
                    page_writer.write_event(other.into_owned())?;
                    if page_writer.get_ref().len() > MAX_VDX_PAGE_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "VDX page exceeds {MAX_VDX_PAGE_BYTES} bytes"
                        )));
                    }
                }
            }
        }
        buffer.clear();
    }
    if !root_seen {
        return Err(Error::InvalidInput("VDX document is empty".into()));
    }
    if writer.is_some() || page.is_some() {
        return Err(Error::InvalidInput(
            "VDX document ended inside a Page entry".into(),
        ));
    }
    if rendered_page_count == 0 {
        return Err(Error::InvalidInput(
            "VDX document contains no foreground drawing pages".into(),
        ));
    }
    if saw_background {
        warnings
            .push("Visio background pages are not rendered or applied to foreground pages".into());
    }
    Ok(warnings)
}

fn render_vdx_page(
    page_xml: &[u8],
    page_meta: VdxPage,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    rendered_page_count: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if page_meta.is_background {
        return Ok(());
    }
    if *rendered_page_count >= options.max_pages.min(MAX_VDX_PAGES) {
        return Err(Error::LimitExceeded(format!(
            "VDX document exceeds {} output pages",
            options.max_pages.min(MAX_VDX_PAGES)
        )));
    }
    *rendered_page_count += 1;
    let parsed = vsdx::parse_page(page_xml, options.max_xml_events, "VDX Page")?;
    let mut output =
        vsdx::render_page(parsed, *rendered_page_count, &page_meta.name, &page_meta.id);
    if page_meta.name.is_empty() {
        output.warn(format!(
            "VDX page {} has no name; a generic title is used",
            rendered_page_count
        ));
    }
    if output.nodes.is_empty() {
        output.warn("Visio page contains no supported visible shapes");
    }
    for warning in &output.warnings {
        if !warnings.contains(warning) {
            warnings.push(warning.clone());
        }
    }
    sink.consume(output)
}

fn start_page(namespaces: &[(String, String)]) -> Result<Writer<Vec<u8>>> {
    let mut writer = Writer::new(Vec::new());
    let mut wrapper = BytesStart::new("PageContents");
    for (name, value) in namespaces {
        wrapper.push_attribute((name.as_str(), value.as_str()));
    }
    writer.write_event(Event::Start(wrapper))?;
    Ok(writer)
}

fn finish_page(mut writer: Writer<Vec<u8>>) -> Result<Vec<u8>> {
    writer.write_event(Event::End(BytesEnd::new("PageContents")))?;
    Ok(writer.into_inner())
}

fn namespace_attributes(start: &BytesStart<'_>) -> Vec<(String, String)> {
    start
        .attributes()
        .with_checks(false)
        .flatten()
        .filter_map(|attribute| {
            let name = std::str::from_utf8(attribute.key.as_ref()).ok()?;
            (name == "xmlns" || name.starts_with("xmlns:") || name == "xml:space").then(|| {
                (
                    name.to_owned(),
                    String::from_utf8_lossy(attribute.value.as_ref()).into_owned(),
                )
            })
        })
        .collect()
}

fn page_from_start(start: &BytesStart<'_>) -> VdxPage {
    let back_page = attribute(start, b"BackPage").unwrap_or_default();
    VdxPage {
        id: attribute(start, b"ID").unwrap_or_default(),
        name: attribute(start, b"NameU")
            .filter(|value| !value.is_empty())
            .or_else(|| attribute(start, b"Name"))
            .unwrap_or_default(),
        is_background: attribute(start, b"IsBackground")
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true")),
        references_background: !back_page.is_empty() && back_page != "0",
    }
}
