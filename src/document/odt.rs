//! OpenDocument Text (ODT/OTT/FODT) paragraph, list, table, and text-style preview.
//!
//! The parser follows `office:text` content, keeps heading levels and table cell
//! text, embeds bounded package-linked PNG/JPEG images as centered flow blocks,
//! and uses the shared bounded HTML-like page composer for SVG pagination. A
//! bounded named/automatic style subset resolves font family, size, weight,
//! italic, and color; paragraph layout styles, objects, and external resources
//! are not fully reproduced.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{
    HtmlBlock, HtmlTextBlockKind, HtmlTextRun, HtmlTextStyle, render_blocks_to_pages,
};
use crate::error::{Error, Result};
use crate::ooxml::{
    ZipPackage, attribute, color_from_hex, decode_xml_reference, local_name, resolve_part_target,
    sniff_image_mime,
};
use crate::table::{TableAlign, TableData};

const ODT_MIMETYPE_LIMIT: u64 = 256;
const MAX_ODT_REPEAT: usize = 10_000;
const MAX_ODT_TABLE_CELLS: usize = 200_000;
const MAX_ODT_XML_DEPTH: usize = 256;
const MAX_ODT_STYLES: usize = 100_000;
const MAX_ODT_STYLE_NAME_BYTES: usize = 1_024;
const MAX_ODT_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ODT_TEXT_RUNS: usize = 200_000;
const MAX_ODT_EXPANDED_TABLE_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ODT_IMAGES: usize = 10_000;
const MAX_ODT_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ODT_TOTAL_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ODT_TOTAL_DATA_URI_BYTES: usize = 48 * 1024 * 1024;
const MAX_ODT_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_ODT_TOTAL_IMAGE_PIXELS: u64 = 100_000_000;

#[derive(Clone, Debug)]
enum ParagraphKind {
    Heading(u8),
    Paragraph,
    ListItem,
}

#[derive(Clone, Debug)]
struct Paragraph {
    kind: ParagraphKind,
    base_style: HtmlTextStyle,
    current_style: HtmlTextStyle,
    runs: Vec<HtmlTextRun>,
}

#[derive(Clone, Default)]
struct OdtNamedStyle {
    properties: HtmlTextStyle,
    parent: Option<String>,
}

#[derive(Default)]
struct OdtStyles {
    paragraph: HashMap<String, OdtNamedStyle>,
    text: HashMap<String, OdtNamedStyle>,
    default_paragraph: HtmlTextStyle,
    default_text: HtmlTextStyle,
    font_faces: HashMap<String, String>,
    count: usize,
    unsupported_text_properties: bool,
    unsupported_paragraph_properties: bool,
}

struct OdtStyleBuilder {
    family: String,
    name: Option<String>,
    parent: Option<String>,
    properties: HtmlTextStyle,
    is_default: bool,
    depth: usize,
}

impl OdtStyles {
    fn resolve_paragraph(&self, name: Option<&str>) -> (HtmlTextStyle, bool) {
        let mut style = self.default_paragraph.clone();
        merge_text_style(&mut style, self.default_text.clone());
        let missing = name.is_some_and(|name| {
            resolve_odt_named_style(name, &self.paragraph, &self.font_faces, &mut style)
        });
        if let Some(font_name) = style.font_family.as_deref()
            && let Some(font_family) = self.font_faces.get(font_name)
        {
            style.font_family = Some(font_family.clone());
        }
        (style, missing)
    }

    fn resolve_text(&self, name: Option<&str>) -> (HtmlTextStyle, bool) {
        let mut style = HtmlTextStyle::default();
        let missing = name.is_some_and(|name| {
            resolve_odt_named_style(name, &self.text, &self.font_faces, &mut style)
        });
        if let Some(font_name) = style.font_family.as_deref()
            && let Some(font_family) = self.font_faces.get(font_name)
        {
            style.font_family = Some(font_family.clone());
        }
        (style, missing)
    }
}

fn merge_text_style(target: &mut HtmlTextStyle, source: HtmlTextStyle) {
    if source.font_family.is_some() {
        target.font_family = source.font_family;
    }
    if source.font_size.is_some() {
        target.font_size = source.font_size;
    }
    if source.bold.is_some() {
        target.bold = source.bold;
    }
    if source.italic.is_some() {
        target.italic = source.italic;
    }
    if source.color.is_some() {
        target.color = source.color;
    }
}

fn resolve_odt_named_style(
    name: &str,
    styles: &HashMap<String, OdtNamedStyle>,
    font_faces: &HashMap<String, String>,
    target: &mut HtmlTextStyle,
) -> bool {
    let mut chain = Vec::new();
    let mut seen = HashMap::new();
    let mut current = Some(name.to_owned());
    let mut missing = false;
    while let Some(style_name) = current {
        if chain.len() >= 32 || seen.insert(style_name.clone(), ()).is_some() {
            missing = true;
            break;
        }
        let Some(style) = styles.get(&style_name) else {
            missing = true;
            break;
        };
        chain.push(style.properties.clone());
        current = style.parent.clone();
    }
    for properties in chain.into_iter().rev() {
        merge_text_style(target, properties);
    }
    if let Some(font_name) = target.font_family.as_deref()
        && let Some(font_family) = font_faces.get(font_name)
    {
        target.font_family = Some(font_family.clone());
    }
    missing
}

fn parse_style_definitions(xml: &str, max_events: usize, styles: &mut OdtStyles) -> Result<()> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut current = None::<OdtStyleBuilder>;
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "ODT style XML exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                if matches!(name.as_str(), "style" | "default-style") && current.is_none() {
                    if let Some(family) = attribute(&element, b"family")
                        && matches!(family.as_str(), "paragraph" | "text")
                    {
                        current = Some(OdtStyleBuilder {
                            family,
                            name: bounded_odt_style_name(&element, b"name")?,
                            parent: bounded_odt_style_name(&element, b"parent-style-name")?,
                            properties: HtmlTextStyle::default(),
                            is_default: name == "default-style",
                            depth: stack.len() + 1,
                        });
                    }
                } else if let Some(builder) = current.as_mut() {
                    if name == "text-properties" {
                        merge_text_style(
                            &mut builder.properties,
                            parse_text_properties(&element, styles),
                        );
                    } else if name == "paragraph-properties" && has_xml_attributes(&element) {
                        styles.unsupported_paragraph_properties = true;
                    }
                }
                if name == "font-face"
                    && let (Some(name), Some(family)) = (
                        attribute(&element, b"name"),
                        attribute(&element, b"font-family"),
                    )
                    && name.len() <= 256
                    && family.len() <= 256
                    && !family.chars().any(char::is_control)
                {
                    if styles.font_faces.len() >= MAX_ODT_STYLES
                        && !styles.font_faces.contains_key(&name)
                    {
                        return Err(Error::LimitExceeded(format!(
                            "ODT font face count exceeds {MAX_ODT_STYLES}"
                        )));
                    }
                    styles.font_faces.insert(name, unquote_font_family(&family));
                }
                if stack.len() >= MAX_ODT_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "ODT style XML nesting exceeds {MAX_ODT_XML_DEPTH} elements"
                    )));
                }
                stack.push(name);
            }
            Event::Empty(element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                if matches!(name.as_str(), "style" | "default-style") && current.is_none() {
                    if let Some(family) = attribute(&element, b"family")
                        && matches!(family.as_str(), "paragraph" | "text")
                    {
                        finish_odt_style(
                            OdtStyleBuilder {
                                family,
                                name: bounded_odt_style_name(&element, b"name")?,
                                parent: bounded_odt_style_name(&element, b"parent-style-name")?,
                                properties: HtmlTextStyle::default(),
                                is_default: name == "default-style",
                                depth: stack.len(),
                            },
                            styles,
                        )?;
                    }
                } else if let Some(builder) = current.as_mut() {
                    if name == "text-properties" {
                        merge_text_style(
                            &mut builder.properties,
                            parse_text_properties(&element, styles),
                        );
                    } else if name == "paragraph-properties" && has_xml_attributes(&element) {
                        styles.unsupported_paragraph_properties = true;
                    }
                }
                if name == "font-face"
                    && let (Some(name), Some(family)) = (
                        attribute(&element, b"name"),
                        attribute(&element, b"font-family"),
                    )
                    && name.len() <= 256
                    && family.len() <= 256
                    && !family.chars().any(char::is_control)
                {
                    if styles.font_faces.len() >= MAX_ODT_STYLES
                        && !styles.font_faces.contains_key(&name)
                    {
                        return Err(Error::LimitExceeded(format!(
                            "ODT font face count exceeds {MAX_ODT_STYLES}"
                        )));
                    }
                    styles.font_faces.insert(name, unquote_font_family(&family));
                }
            }
            Event::End(element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                if matches!(name.as_str(), "style" | "default-style")
                    && current
                        .as_ref()
                        .is_some_and(|builder| builder.depth == stack.len())
                    && let Some(builder) = current.take()
                {
                    finish_odt_style(builder, styles)?;
                }
                if stack.pop().as_deref() != Some(name.as_str()) {
                    return Err(Error::InvalidInput(format!(
                        "mismatched ODT style XML end tag '{name}'"
                    )));
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "ODT style XML must not contain a document type declaration".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if current.is_some() || !stack.is_empty() {
        return Err(Error::InvalidInput(
            "ODT style XML ended with an incomplete element".into(),
        ));
    }
    Ok(())
}

fn finish_odt_style(builder: OdtStyleBuilder, styles: &mut OdtStyles) -> Result<()> {
    if builder.is_default {
        let target = if builder.family == "paragraph" {
            &mut styles.default_paragraph
        } else {
            &mut styles.default_text
        };
        merge_text_style(target, builder.properties);
        return Ok(());
    }
    let Some(name) = builder.name else {
        return Ok(());
    };
    let map = if builder.family == "paragraph" {
        &mut styles.paragraph
    } else {
        &mut styles.text
    };
    if !map.contains_key(&name) {
        styles.count = styles.count.saturating_add(1);
        if styles.count > MAX_ODT_STYLES {
            return Err(Error::LimitExceeded(format!(
                "ODT style count exceeds {MAX_ODT_STYLES}"
            )));
        }
    }
    map.insert(
        name,
        OdtNamedStyle {
            properties: builder.properties,
            parent: builder.parent,
        },
    );
    Ok(())
}

fn parse_text_properties(element: &BytesStart<'_>, styles: &mut OdtStyles) -> HtmlTextStyle {
    let mut result = HtmlTextStyle::default();
    let supported = [
        "font-name",
        "font-family",
        "font-size",
        "font-weight",
        "font-style",
        "color",
    ];
    for attribute_value in element.attributes().with_checks(false).flatten() {
        let name = local_name(attribute_value.key.as_ref());
        if !supported
            .iter()
            .any(|supported| name == supported.as_bytes())
        {
            styles.unsupported_text_properties = true;
        }
    }
    let font_family =
        attribute(element, b"font-name").or_else(|| attribute(element, b"font-family"));
    if let Some(value) = font_family {
        if value.len() <= 256 && !value.chars().any(char::is_control) {
            let value = unquote_font_family(&value);
            if !value.is_empty() {
                result.font_family = Some(value);
            }
        } else {
            styles.unsupported_text_properties = true;
        }
    }
    if let Some(value) = attribute(element, b"font-size") {
        result.font_size = parse_odt_font_size(&value).filter(|size| (1.0..=512.0).contains(size));
        if result.font_size.is_none() {
            styles.unsupported_text_properties = true;
        }
    }
    if let Some(value) = attribute(element, b"font-weight") {
        result.bold = match value.to_ascii_lowercase().as_str() {
            "bold" | "bolder" => Some(true),
            "normal" | "lighter" => Some(false),
            _ => value.parse::<u16>().ok().map(|weight| weight >= 600),
        };
        if result.bold.is_none() {
            styles.unsupported_text_properties = true;
        }
    }
    if let Some(value) = attribute(element, b"font-style") {
        result.italic = match value.to_ascii_lowercase().as_str() {
            "normal" => Some(false),
            "italic" | "oblique" => Some(true),
            _ => None,
        };
        if result.italic.is_none() {
            styles.unsupported_text_properties = true;
        }
    }
    if let Some(value) = attribute(element, b"color") {
        if is_odt_hex_color(&value) {
            result.color = Some(color_from_hex(&value, "#334155"));
        } else {
            styles.unsupported_text_properties = true;
        }
    }
    result
}

fn has_xml_attributes(element: &BytesStart<'_>) -> bool {
    element.attributes().with_checks(false).next().is_some()
}

fn bounded_odt_style_name(element: &BytesStart<'_>, name: &[u8]) -> Result<Option<String>> {
    let value = attribute(element, name);
    if value
        .as_ref()
        .is_some_and(|value| value.len() > MAX_ODT_STYLE_NAME_BYTES)
    {
        return Err(Error::LimitExceeded(format!(
            "ODT style identifier exceeds {MAX_ODT_STYLE_NAME_BYTES} bytes"
        )));
    }
    Ok(value)
}

fn parse_odt_font_size(value: &str) -> Option<f64> {
    let value = value.trim().to_ascii_lowercase();
    for (unit, scale) in [
        ("pt", 1.0),
        ("pc", 12.0),
        ("in", 72.0),
        ("cm", 72.0 / 2.54),
        ("mm", 72.0 / 25.4),
        ("px", 0.75),
    ] {
        if let Some(number) = value.strip_suffix(unit) {
            return number
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .map(|number| number * scale);
        }
    }
    None
}

fn unquote_font_family(value: &str) -> String {
    value.trim().trim_matches(['"', '\'']).trim().to_owned()
}

fn is_odt_hex_color(value: &str) -> bool {
    value.starts_with('#')
        && matches!(value.len(), 4 | 7)
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone, Debug, Default)]
struct TableBuilder {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    total_cells: usize,
    total_text_bytes: usize,
}

#[derive(Default)]
struct OdtImageBudget {
    count: usize,
    decoded_bytes: usize,
    data_uri_bytes: usize,
    pixels: u64,
}

struct PendingImage {
    depth: usize,
    href: Option<String>,
    alt: String,
    has_inline_binary: bool,
    inline_data: Option<String>,
    was_in_table: bool,
    over_limit: bool,
}

struct OdtImageContext<'a> {
    package: Option<&'a mut ZipPackage<File>>,
    budget: &'a mut OdtImageBudget,
    warnings: &'a mut Vec<String>,
    blocks: &'a mut Vec<HtmlBlock>,
    cell: &'a mut Option<String>,
    warned_table_images: &'a mut bool,
    text_bytes: &'a mut usize,
    text_run_count: &'a mut usize,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let (bytes, styles_xml, mut package) = if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fodt"))
    {
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        Read::take(&mut file, options.max_input_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > options.max_input_bytes {
            return Err(Error::LimitExceeded(format!(
                "FODT input exceeds maximum bytes ({})",
                options.max_input_bytes
            )));
        }
        (bytes, None, None)
    } else {
        let mut package = ZipPackage::open(path, options.max_zip_entry_bytes)?;
        let mimetype = package.read_limited("mimetype", ODT_MIMETYPE_LIMIT)?;
        let mimetype = std::str::from_utf8(&mimetype)
            .map_err(|error| Error::InvalidInput(format!("ODT mimetype is not UTF-8: {error}")))?
            .trim();
        if !matches!(
            mimetype,
            "application/vnd.oasis.opendocument.text"
                | "application/vnd.oasis.opendocument.text-template"
                | "application/vnd.sun.xml.writer"
                | "application/vnd.sun.xml.writer.template"
        ) {
            return Err(Error::InvalidInput(format!(
                "unsupported OpenDocument text mimetype '{mimetype}'"
            )));
        }
        let content = package.read("content.xml")?;
        let styles = package.read_optional("styles.xml")?;
        (content, styles, Some(package))
    };

    let xml = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("ODT content is not UTF-8: {error}")))?;
    let mut styles = OdtStyles::default();
    if let Some(styles_xml) = styles_xml {
        let styles_xml = String::from_utf8(styles_xml)
            .map_err(|error| Error::InvalidInput(format!("ODT styles are not UTF-8: {error}")))?;
        parse_style_definitions(&styles_xml, options.max_xml_events, &mut styles)?;
    }
    parse_style_definitions(&xml, options.max_xml_events, &mut styles)?;
    let (blocks, mut warnings) =
        parse_content_with_styles(&xml, options.max_xml_events, package.as_mut(), &styles)?;
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "OpenDocument Text contains no renderable text or tables".into(),
        ));
    }
    warnings.insert(
        0,
        "ODT bold, italic, text color, font size, and font family text styles are applied where supported; paragraph spacing/alignment, table styles, exact page geometry, and pagination are approximated".into(),
    );
    render_blocks_to_pages(&blocks, sink, options)?;
    Ok(warnings)
}

#[cfg(test)]
fn parse_content(
    xml: &str,
    max_events: usize,
    package: Option<&mut ZipPackage<File>>,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    parse_content_with_styles(xml, max_events, package, &OdtStyles::default())
}

fn parse_content_with_styles(
    xml: &str,
    max_events: usize,
    mut package: Option<&mut ZipPackage<File>>,
    styles: &OdtStyles,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    if styles.unsupported_text_properties {
        warnings.push("some ODT character style properties are not rendered".into());
    }
    if styles.unsupported_paragraph_properties {
        warnings.push("ODT paragraph spacing/alignment style properties are approximated".into());
    }
    let mut in_text_body = false;
    let mut list_depth = 0usize;
    let mut header_rows_depth = 0usize;
    let mut paragraph: Option<Paragraph> = None;
    let mut text_style_stack: Vec<HtmlTextStyle> = Vec::new();
    let mut table: Option<TableBuilder> = None;
    let mut row: Option<Vec<String>> = None;
    let mut row_text_bytes = 0usize;
    let mut row_is_header = false;
    let mut row_repeats = 1usize;
    let mut cell: Option<String> = None;
    let mut cell_repeats = 1usize;
    let mut event_count = 0usize;
    let mut image_budget = OdtImageBudget::default();
    let mut pending_image: Option<PendingImage> = None;
    let mut frame_alt: Vec<String> = Vec::new();
    let mut warned_image_flow = false;
    let mut warned_table_images = false;
    let mut warned_spans = false;
    let mut warned_table_styles = false;
    let mut warned_missing_style = false;
    let mut text_bytes = 0usize;
    let mut text_run_count = 0usize;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_events {
            return Err(Error::LimitExceeded(format!(
                "ODT content.xml exceeds {max_events} parser events"
            )));
        }

        match reader.read_event_into(&mut buffer)? {
            Event::Start(ref element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                if name == "text" && stack.last().is_some_and(|parent| parent == "body") {
                    in_text_body = true;
                }
                if in_text_body {
                    match name.as_str() {
                        "frame" => frame_alt.push(attribute(element, b"name").unwrap_or_default()),
                        "list" => list_depth = list_depth.saturating_add(1),
                        "list-item" => {}
                        "table-header-rows" => {
                            header_rows_depth = header_rows_depth.saturating_add(1)
                        }
                        "table" => {
                            if table.is_some() {
                                return Err(Error::Unsupported(
                                    "nested OpenDocument tables are not supported".into(),
                                ));
                            }
                            flush_paragraph(
                                &mut paragraph,
                                &mut cell,
                                &mut blocks,
                                &mut warnings,
                                &mut warned_table_styles,
                            );
                            table = Some(TableBuilder::default());
                        }
                        "table-row" => {
                            if row.is_some() {
                                return Err(Error::InvalidInput(
                                    "nested OpenDocument table rows are invalid".into(),
                                ));
                            }
                            row = Some(Vec::new());
                            row_text_bytes = 0;
                            row_is_header = header_rows_depth > 0;
                            row_repeats = parse_repeat(element, b"number-rows-repeated")?;
                        }
                        "table-cell" => {
                            if cell.is_some() {
                                return Err(Error::InvalidInput(
                                    "nested OpenDocument table cells are invalid".into(),
                                ));
                            }
                            let has_spanned_cells = [
                                attribute(element, b"number-columns-spanned"),
                                attribute(element, b"number-rows-spanned"),
                            ]
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value.parse::<usize>().ok())
                            .any(|span| span > 1);
                            if has_spanned_cells && !warned_spans {
                                warnings.push(
                                    "ODT merged table cells are not expanded in the preview".into(),
                                );
                                warned_spans = true;
                            }
                            cell = Some(String::new());
                            cell_repeats = parse_repeat(element, b"number-columns-repeated")?;
                        }
                        "covered-table-cell" => {
                            cell = Some(String::new());
                            cell_repeats = 1;
                        }
                        "p" => {
                            let kind = if list_depth > 0 {
                                ParagraphKind::ListItem
                            } else {
                                ParagraphKind::Paragraph
                            };
                            let style_name = bounded_odt_style_name(element, b"style-name")?;
                            let (style, missing) = styles.resolve_paragraph(style_name.as_deref());
                            if missing && !warned_missing_style {
                                warnings.push("some ODT paragraph styles were missing or had cyclic/deep inheritance; default text properties were used".into());
                                warned_missing_style = true;
                            }
                            paragraph = Some(new_paragraph(kind, style));
                            text_style_stack.clear();
                        }
                        "h" => {
                            let level = attribute(element, b"outline-level")
                                .and_then(|value| value.parse::<u8>().ok())
                                .unwrap_or(1)
                                .clamp(1, 6);
                            let style_name = bounded_odt_style_name(element, b"style-name")?;
                            let (style, missing) = styles.resolve_paragraph(style_name.as_deref());
                            if missing && !warned_missing_style {
                                warnings.push("some ODT paragraph styles were missing or had cyclic/deep inheritance; default text properties were used".into());
                                warned_missing_style = true;
                            }
                            paragraph = Some(new_paragraph(ParagraphKind::Heading(level), style));
                            text_style_stack.clear();
                        }
                        "span" if paragraph.is_some() => {
                            let style_name = bounded_odt_style_name(element, b"style-name")?;
                            let (span_style, missing) = styles.resolve_text(style_name.as_deref());
                            if missing && !warned_missing_style {
                                warnings.push("some ODT character styles were missing or had cyclic/deep inheritance; inherited text properties were used".into());
                                warned_missing_style = true;
                            }
                            if let Some(paragraph) = paragraph.as_mut() {
                                let mut merged = paragraph.current_style.clone();
                                merge_text_style(&mut merged, span_style);
                                paragraph.current_style = merged.clone();
                                text_style_stack.push(merged);
                            }
                        }
                        "image" => {
                            if pending_image.is_some() {
                                return Err(Error::InvalidInput(
                                    "nested OpenDocument images are invalid".into(),
                                ));
                            }
                            let (kind, base_style, current_style) = paragraph
                                .as_ref()
                                .map(|paragraph| {
                                    (
                                        paragraph.kind.clone(),
                                        paragraph.base_style.clone(),
                                        paragraph.current_style.clone(),
                                    )
                                })
                                .unwrap_or_else(|| {
                                    let (style, missing) = styles.resolve_paragraph(None);
                                    if missing && !warned_missing_style {
                                        warnings.push("some ODT paragraph styles were missing or had cyclic/deep inheritance; default text properties were used".into());
                                        warned_missing_style = true;
                                    }
                                    (
                                        if list_depth > 0 {
                                            ParagraphKind::ListItem
                                        } else {
                                            ParagraphKind::Paragraph
                                        },
                                        style.clone(),
                                        style,
                                    )
                                });
                            flush_paragraph(
                                &mut paragraph,
                                &mut cell,
                                &mut blocks,
                                &mut warnings,
                                &mut warned_table_styles,
                            );
                            let over_limit = !reserve_odt_image(&mut image_budget, &mut warnings);
                            pending_image = Some(PendingImage {
                                depth: stack.len() + 1,
                                href: attribute(element, b"href"),
                                alt: attribute(element, b"name")
                                    .filter(|value| !value.trim().is_empty())
                                    .or_else(|| {
                                        frame_alt
                                            .last()
                                            .filter(|value| !value.trim().is_empty())
                                            .cloned()
                                    })
                                    .unwrap_or_else(|| "Embedded image".into()),
                                has_inline_binary: false,
                                inline_data: None,
                                was_in_table: table.is_some(),
                                over_limit,
                            });
                            if !warned_image_flow && cell.is_none() {
                                warnings.push("ODT inline image anchors, wrapping, and frame placement are approximated as centered flow blocks".into());
                                warned_image_flow = true;
                            }
                            if cell.is_none() {
                                // Preserve the paragraph kind for text following an inline frame.
                                paragraph = Some(Paragraph {
                                    kind,
                                    base_style,
                                    current_style,
                                    runs: Vec::new(),
                                });
                            }
                        }
                        "binary-data" if pending_image.is_some() => {
                            if let Some(image) = pending_image.as_mut() {
                                image.has_inline_binary = true;
                                image.inline_data = Some(String::new());
                            }
                        }
                        _ => {}
                    }
                }
                if stack.len() >= MAX_ODT_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "ODT XML nesting exceeds {MAX_ODT_XML_DEPTH} elements"
                    )));
                }
                stack.push(name);
            }
            Event::Empty(ref element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if in_text_body {
                    match name {
                        b"s" => append_spaces(
                            &mut paragraph,
                            &mut cell,
                            element,
                            &mut text_bytes,
                            &mut text_run_count,
                        )?,
                        b"tab" => append_text(
                            &mut paragraph,
                            &mut cell,
                            "\t",
                            &mut text_bytes,
                            &mut text_run_count,
                        )?,
                        b"line-break" => append_text(
                            &mut paragraph,
                            &mut cell,
                            "\n",
                            &mut text_bytes,
                            &mut text_run_count,
                        )?,
                        b"covered-table-cell" => {
                            append_cell("".into(), 1, &mut row, &mut row_text_bytes)?
                        }
                        b"table-cell" => {
                            let repeats = parse_repeat(element, b"number-columns-repeated")?;
                            append_cell(String::new(), repeats, &mut row, &mut row_text_bytes)?;
                        }
                        b"image" => {
                            let (kind, base_style, current_style) = paragraph
                                .as_ref()
                                .map(|paragraph| {
                                    (
                                        paragraph.kind.clone(),
                                        paragraph.base_style.clone(),
                                        paragraph.current_style.clone(),
                                    )
                                })
                                .unwrap_or_else(|| {
                                    let (style, _) = styles.resolve_paragraph(None);
                                    (
                                        if list_depth > 0 {
                                            ParagraphKind::ListItem
                                        } else {
                                            ParagraphKind::Paragraph
                                        },
                                        style.clone(),
                                        style,
                                    )
                                });
                            let alt = attribute(element, b"name")
                                .filter(|value| !value.trim().is_empty())
                                .or_else(|| {
                                    frame_alt
                                        .last()
                                        .filter(|value| !value.trim().is_empty())
                                        .cloned()
                                })
                                .unwrap_or_else(|| "Embedded image".into());
                            let was_in_table = table.is_some();
                            flush_paragraph(
                                &mut paragraph,
                                &mut cell,
                                &mut blocks,
                                &mut warnings,
                                &mut warned_table_styles,
                            );
                            if !warned_image_flow && cell.is_none() {
                                warnings.push("ODT inline image anchors, wrapping, and frame placement are approximated as centered flow blocks".into());
                                warned_image_flow = true;
                            }
                            if reserve_odt_image(&mut image_budget, &mut warnings) {
                                attach_odt_image(
                                    attribute(element, b"href").as_deref(),
                                    None,
                                    &alt,
                                    was_in_table,
                                    OdtImageContext {
                                        package: package.as_deref_mut(),
                                        budget: &mut image_budget,
                                        warnings: &mut warnings,
                                        blocks: &mut blocks,
                                        cell: &mut cell,
                                        warned_table_images: &mut warned_table_images,
                                        text_bytes: &mut text_bytes,
                                        text_run_count: &mut text_run_count,
                                    },
                                )?;
                            }
                            if cell.is_none() {
                                paragraph = Some(Paragraph {
                                    kind,
                                    base_style,
                                    current_style,
                                    runs: Vec::new(),
                                });
                            }
                        }
                        b"binary-data" if pending_image.is_some() => {
                            if let Some(image) = pending_image.as_mut() {
                                image.has_inline_binary = true;
                                image.inline_data = Some(String::new());
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::Text(ref value) => {
                if in_text_body && stack.iter().any(|element| element == "binary-data") {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODT inline image encoding: {error}"))
                    })?;
                    if let Some(image) = pending_image.as_mut()
                        && let Some(data) = image.inline_data.as_mut()
                    {
                        if data.len().saturating_add(decoded.len())
                            > (MAX_ODT_IMAGE_BYTES as usize).saturating_mul(2)
                        {
                            return Err(Error::LimitExceeded(format!(
                                "ODT inline image base64 exceeds the {}-byte limit",
                                MAX_ODT_IMAGE_BYTES.saturating_mul(2)
                            )));
                        }
                        data.push_str(&decoded);
                    }
                } else if in_text_body {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODT text encoding: {error}"))
                    })?;
                    let text = quick_xml::escape::unescape(&decoded).map_err(|error| {
                        Error::InvalidInput(format!("invalid ODT XML text: {error}"))
                    })?;
                    append_text(
                        &mut paragraph,
                        &mut cell,
                        &text,
                        &mut text_bytes,
                        &mut text_run_count,
                    )?;
                }
            }
            Event::GeneralRef(ref reference) => {
                if in_text_body && !stack.iter().any(|element| element == "binary-data") {
                    let text = decode_xml_reference(reference, "ODT text")?;
                    append_text(
                        &mut paragraph,
                        &mut cell,
                        &text,
                        &mut text_bytes,
                        &mut text_run_count,
                    )?;
                }
            }
            Event::CData(ref value) => {
                if in_text_body && stack.iter().any(|element| element == "binary-data") {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODT inline image encoding: {error}"))
                    })?;
                    if let Some(image) = pending_image.as_mut()
                        && let Some(data) = image.inline_data.as_mut()
                    {
                        if data.len().saturating_add(decoded.len())
                            > (MAX_ODT_IMAGE_BYTES as usize).saturating_mul(2)
                        {
                            return Err(Error::LimitExceeded(format!(
                                "ODT inline image base64 exceeds the {}-byte limit",
                                MAX_ODT_IMAGE_BYTES.saturating_mul(2)
                            )));
                        }
                        data.push_str(&decoded);
                    }
                } else if in_text_body {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODT CDATA encoding: {error}"))
                    })?;
                    append_text(
                        &mut paragraph,
                        &mut cell,
                        &decoded,
                        &mut text_bytes,
                        &mut text_run_count,
                    )?;
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "OpenDocument content must not contain a document type declaration".into(),
                ));
            }
            Event::End(ref element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                match name {
                    b"image" => {
                        let pending = pending_image.take().ok_or_else(|| {
                            Error::InvalidInput("OpenDocument image ended without opening".into())
                        })?;
                        if pending.depth != stack.len() {
                            return Err(Error::InvalidInput(
                                "mismatched OpenDocument image nesting".into(),
                            ));
                        }
                        if pending.over_limit {
                            // The common warning was added when the reference was counted.
                        } else if pending.has_inline_binary {
                            attach_odt_image(
                                pending.href.as_deref(),
                                pending.inline_data.as_deref(),
                                &pending.alt,
                                pending.was_in_table,
                                OdtImageContext {
                                    package: package.as_deref_mut(),
                                    budget: &mut image_budget,
                                    warnings: &mut warnings,
                                    blocks: &mut blocks,
                                    cell: &mut cell,
                                    warned_table_images: &mut warned_table_images,
                                    text_bytes: &mut text_bytes,
                                    text_run_count: &mut text_run_count,
                                },
                            )?;
                        } else {
                            attach_odt_image(
                                pending.href.as_deref(),
                                None,
                                &pending.alt,
                                pending.was_in_table,
                                OdtImageContext {
                                    package: package.as_deref_mut(),
                                    budget: &mut image_budget,
                                    warnings: &mut warnings,
                                    blocks: &mut blocks,
                                    cell: &mut cell,
                                    warned_table_images: &mut warned_table_images,
                                    text_bytes: &mut text_bytes,
                                    text_run_count: &mut text_run_count,
                                },
                            )?;
                        }
                    }
                    b"frame" => {
                        frame_alt.pop();
                    }
                    b"span" => {
                        if let Some(paragraph) = paragraph.as_mut() {
                            text_style_stack.pop();
                            paragraph.current_style = text_style_stack
                                .last()
                                .cloned()
                                .unwrap_or_else(|| paragraph.base_style.clone());
                        }
                    }
                    b"p" | b"h" => {
                        flush_paragraph(
                            &mut paragraph,
                            &mut cell,
                            &mut blocks,
                            &mut warnings,
                            &mut warned_table_styles,
                        );
                        text_style_stack.clear();
                    }
                    b"table-cell" => {
                        let value = cell.take().ok_or_else(|| {
                            Error::InvalidInput("ODT table cell ended without opening".into())
                        })?;
                        append_cell(value, cell_repeats, &mut row, &mut row_text_bytes)?;
                        cell_repeats = 1;
                    }
                    b"covered-table-cell" => {
                        let value = cell.take().ok_or_else(|| {
                            Error::InvalidInput("ODT covered cell ended without opening".into())
                        })?;
                        append_cell(value, cell_repeats, &mut row, &mut row_text_bytes)?;
                        cell_repeats = 1;
                    }
                    b"table-row" => {
                        let row_values = row.take().ok_or_else(|| {
                            Error::InvalidInput("ODT table row ended without opening".into())
                        })?;
                        append_row(
                            row_values,
                            row_text_bytes,
                            row_repeats,
                            row_is_header,
                            &mut table,
                        )?;
                        row_repeats = 1;
                        row_is_header = false;
                    }
                    b"table-header-rows" => {
                        header_rows_depth = header_rows_depth.saturating_sub(1);
                    }
                    b"table" => {
                        let builder = table.take().ok_or_else(|| {
                            Error::InvalidInput("ODT table ended without opening".into())
                        })?;
                        if !builder.headers.is_empty() || !builder.rows.is_empty() {
                            let mut headers = builder.headers;
                            let mut rows = builder.rows;
                            if headers.is_empty() && !rows.is_empty() {
                                headers = rows.remove(0);
                            }
                            let columns = headers
                                .len()
                                .max(rows.iter().map(Vec::len).max().unwrap_or(0));
                            blocks.push(HtmlBlock::Table(TableData {
                                headers,
                                rows,
                                alignments: vec![TableAlign::Left; columns],
                                raw_source: String::new(),
                            }));
                        }
                    }
                    b"list" => list_depth = list_depth.saturating_sub(1),
                    b"text" if stack.len() > 1 => in_text_body = false,
                    _ => {}
                }
                let closing = String::from_utf8_lossy(name).into_owned();
                if stack.pop().as_deref() != Some(closing.as_str()) {
                    return Err(Error::InvalidInput(format!(
                        "mismatched OpenDocument XML end tag '{closing}'"
                    )));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    if paragraph.is_some() || cell.is_some() || row.is_some() || table.is_some() {
        return Err(Error::InvalidInput(
            "incomplete OpenDocument content XML structure".into(),
        ));
    }
    if pending_image.is_some() {
        return Err(Error::InvalidInput(
            "incomplete OpenDocument image element".into(),
        ));
    }
    Ok((blocks, warnings))
}

fn attach_odt_image(
    href: Option<&str>,
    inline_data: Option<&str>,
    alt: &str,
    was_in_table: bool,
    context: OdtImageContext<'_>,
) -> Result<()> {
    let OdtImageContext {
        package,
        budget,
        warnings,
        blocks,
        cell,
        warned_table_images,
        text_bytes,
        text_run_count,
    } = context;
    if was_in_table {
        if !*warned_table_images {
            push_odt_warning_once(
                warnings,
                "ODT images inside tables are represented by alt text because table-cell image layout is unavailable",
            );
            *warned_table_images = true;
        }
        append_text(
            &mut None,
            cell,
            &format!("[{}]", alt),
            text_bytes,
            text_run_count,
        )?;
        return Ok(());
    }
    if let Some(inline_data) = inline_data {
        return attach_odt_inline_image(inline_data, alt, warnings, budget, blocks);
    }
    let Some(package) = package else {
        push_odt_warning_once(
            warnings,
            "flat OpenDocument images and external resources are omitted; package-linked PNG/JPEG images are supported",
        );
        return Ok(());
    };
    let Some(href) = href.filter(|href| !href.trim().is_empty()) else {
        push_odt_warning_once(warnings, "ODT image without a package href was omitted");
        return Ok(());
    };
    if href.contains(':') || href.starts_with("//") || href.starts_with('\\') {
        push_odt_warning_once(warnings, "external ODT image resources are not fetched");
        return Ok(());
    }
    // A single leading slash is package-root-relative. Strip it before the
    // common resolver so its normalization still rejects `..` traversal.
    let package_href = href.strip_prefix('/').unwrap_or(href);
    let target = match resolve_part_target("content.xml", package_href) {
        Ok(target) => target,
        Err(_) => {
            push_odt_warning_once(
                warnings,
                "ODT image path escaped the package or was invalid and was omitted",
            );
            return Ok(());
        }
    };
    let bytes = match package.read_optional_limited(&target, MAX_ODT_IMAGE_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            push_odt_warning_once(warnings, "missing ODT image parts were omitted");
            return Ok(());
        }
        Err(Error::LimitExceeded(_)) => {
            push_odt_warning_once(
                warnings,
                "ODT image parts exceeding the per-image byte limit were omitted",
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let Some(mime) =
        sniff_image_mime(&bytes).filter(|mime| matches!(*mime, "image/png" | "image/jpeg"))
    else {
        push_odt_warning_once(
            warnings,
            "unsupported ODT image types were omitted; only PNG and JPEG are embedded",
        );
        return Ok(());
    };
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
        push_odt_warning_once(warnings, "invalid ODT PNG/JPEG images were omitted");
        return Ok(());
    };
    let pixels = u64::from(width) * u64::from(height);
    let next_pixels = budget.pixels.saturating_add(pixels);
    if width == 0
        || height == 0
        || pixels > MAX_ODT_IMAGE_PIXELS
        || next_pixels > MAX_ODT_TOTAL_IMAGE_PIXELS
    {
        push_odt_warning_once(
            warnings,
            "ODT images exceeding the per-image or total pixel limit were omitted",
        );
        return Ok(());
    }
    let next_bytes = budget.decoded_bytes.saturating_add(bytes.len());
    if next_bytes > MAX_ODT_TOTAL_IMAGE_BYTES {
        push_odt_warning_once(
            warnings,
            "ODT images exceeding the total decoded image byte limit were omitted",
        );
        return Ok(());
    }
    let prefix = format!("data:{mime};base64,");
    let uri_bytes = prefix
        .len()
        .saturating_add(bytes.len().div_ceil(3).saturating_mul(4));
    let next_uri_bytes = budget.data_uri_bytes.saturating_add(uri_bytes);
    if next_uri_bytes > MAX_ODT_TOTAL_DATA_URI_BYTES {
        push_odt_warning_once(
            warnings,
            "ODT images exceeding the total data URI byte limit were omitted",
        );
        return Ok(());
    }
    let href = format!("{prefix}{}", BASE64_STANDARD.encode(&bytes));
    blocks.push(HtmlBlock::Image {
        href,
        pixel_width: width,
        pixel_height: height,
        alt: alt.to_owned(),
    });
    budget.decoded_bytes = next_bytes;
    budget.data_uri_bytes = next_uri_bytes;
    budget.pixels = next_pixels;
    Ok(())
}

fn attach_odt_inline_image(
    data: &str,
    alt: &str,
    warnings: &mut Vec<String>,
    budget: &mut OdtImageBudget,
    blocks: &mut Vec<HtmlBlock>,
) -> Result<()> {
    let compact: String = data
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    let bytes = match BASE64_STANDARD.decode(compact.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => {
            push_odt_warning_once(
                warnings,
                "malformed ODT inline office:binary-data image was omitted",
            );
            return Ok(());
        }
    };
    if bytes.len() as u64 > MAX_ODT_IMAGE_BYTES {
        push_odt_warning_once(
            warnings,
            "ODT inline image exceeded the per-image byte limit",
        );
        return Ok(());
    }
    let Some(mime) =
        sniff_image_mime(&bytes).filter(|mime| matches!(*mime, "image/png" | "image/jpeg"))
    else {
        push_odt_warning_once(
            warnings,
            "unsupported ODT inline image type was omitted; only PNG and JPEG are embedded",
        );
        return Ok(());
    };
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
        push_odt_warning_once(warnings, "invalid ODT inline PNG/JPEG image was omitted");
        return Ok(());
    };
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    let next_pixels = budget.pixels.saturating_add(pixels);
    if width == 0
        || height == 0
        || pixels > MAX_ODT_IMAGE_PIXELS
        || next_pixels > MAX_ODT_TOTAL_IMAGE_PIXELS
    {
        push_odt_warning_once(warnings, "ODT inline images exceeded the pixel limit");
        return Ok(());
    }
    let next_bytes = budget.decoded_bytes.saturating_add(bytes.len());
    if next_bytes > MAX_ODT_TOTAL_IMAGE_BYTES {
        push_odt_warning_once(warnings, "ODT inline images exceeded the total byte limit");
        return Ok(());
    }
    let prefix = format!("data:{mime};base64,");
    let uri_bytes = prefix
        .len()
        .saturating_add(bytes.len().div_ceil(3).saturating_mul(4));
    let next_uri = budget.data_uri_bytes.saturating_add(uri_bytes);
    if next_uri > MAX_ODT_TOTAL_DATA_URI_BYTES {
        push_odt_warning_once(
            warnings,
            "ODT inline images exceeded the total data URI limit",
        );
        return Ok(());
    }
    blocks.push(HtmlBlock::Image {
        href: format!("{prefix}{}", BASE64_STANDARD.encode(&bytes)),
        pixel_width: width,
        pixel_height: height,
        alt: alt.to_owned(),
    });
    budget.decoded_bytes = next_bytes;
    budget.data_uri_bytes = next_uri;
    budget.pixels = next_pixels;
    Ok(())
}

fn reserve_odt_image(budget: &mut OdtImageBudget, warnings: &mut Vec<String>) -> bool {
    if budget.count >= MAX_ODT_IMAGES {
        push_odt_warning_once(
            warnings,
            "ODT image count exceeded the supported limit; remaining images were omitted",
        );
        false
    } else {
        budget.count += 1;
        true
    }
}

fn push_odt_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn parse_repeat(element: &BytesStart<'_>, name: &[u8]) -> Result<usize> {
    let count = attribute(element, name)
        .map(|value| {
            value.parse::<usize>().map_err(|_| {
                Error::InvalidInput(format!(
                    "invalid OpenDocument repeat count for '{}'",
                    String::from_utf8_lossy(name)
                ))
            })
        })
        .transpose()?
        .unwrap_or(1);
    if count == 0 || count > MAX_ODT_REPEAT {
        return Err(Error::LimitExceeded(format!(
            "OpenDocument repeat count must be between 1 and {MAX_ODT_REPEAT}"
        )));
    }
    Ok(count)
}

fn append_spaces(
    paragraph: &mut Option<Paragraph>,
    cell: &mut Option<String>,
    element: &BytesStart<'_>,
    text_bytes: &mut usize,
    text_run_count: &mut usize,
) -> Result<()> {
    let count = parse_repeat(element, b"c")?;
    let spaces = " ".repeat(count);
    append_text(paragraph, cell, &spaces, text_bytes, text_run_count)?;
    Ok(())
}

fn new_paragraph(kind: ParagraphKind, style: HtmlTextStyle) -> Paragraph {
    Paragraph {
        kind,
        base_style: style.clone(),
        current_style: style,
        runs: Vec::new(),
    }
}

fn append_text(
    paragraph: &mut Option<Paragraph>,
    cell: &mut Option<String>,
    text: &str,
    text_bytes: &mut usize,
    text_run_count: &mut usize,
) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    *text_bytes = text_bytes.saturating_add(text.len());
    if *text_bytes > MAX_ODT_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "ODT rendered text exceeds {MAX_ODT_TEXT_BYTES} bytes"
        )));
    }
    if let Some(paragraph) = paragraph.as_mut() {
        if let Some(last) = paragraph.runs.last_mut()
            && last.style == paragraph.current_style
        {
            last.text.push_str(text);
        } else {
            *text_run_count = text_run_count.saturating_add(1);
            if *text_run_count > MAX_ODT_TEXT_RUNS {
                return Err(Error::LimitExceeded(format!(
                    "ODT text exceeds {MAX_ODT_TEXT_RUNS} style runs"
                )));
            }
            paragraph.runs.push(HtmlTextRun {
                text: text.to_owned(),
                style: paragraph.current_style.clone(),
            });
        }
    } else if let Some(cell) = cell.as_mut() {
        cell.push_str(text);
    }
    Ok(())
}

fn flush_paragraph(
    paragraph: &mut Option<Paragraph>,
    cell: &mut Option<String>,
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut Vec<String>,
    warned_table_styles: &mut bool,
) {
    let Some(mut paragraph) = paragraph.take() else {
        return;
    };
    trim_odt_runs(&mut paragraph.runs);
    let text = paragraph
        .runs
        .iter()
        .map(|run| run.text.as_str())
        .collect::<String>();
    if text.is_empty() {
        return;
    }
    if let Some(cell) = cell.as_mut() {
        if (!odt_style_is_empty(&paragraph.base_style)
            || paragraph
                .runs
                .iter()
                .any(|run| !odt_style_is_empty(&run.style)))
            && !*warned_table_styles
        {
            warnings
                .push("ODT rich text styles inside table cells are flattened to plain text".into());
            *warned_table_styles = true;
        }
        if !cell.is_empty() {
            cell.push('\n');
        }
        cell.push_str(&text);
        return;
    }
    let styled = !odt_style_is_empty(&paragraph.base_style)
        || paragraph
            .runs
            .iter()
            .any(|run| !odt_style_is_empty(&run.style));
    if styled {
        let kind = match paragraph.kind {
            ParagraphKind::Heading(level) => HtmlTextBlockKind::Heading(level),
            ParagraphKind::Paragraph => HtmlTextBlockKind::Paragraph,
            ParagraphKind::ListItem => HtmlTextBlockKind::ListItem {
                bullet: "•".into()
            },
        };
        blocks.push(HtmlBlock::StyledText {
            kind,
            runs: paragraph.runs,
        });
        return;
    }
    blocks.push(match paragraph.kind {
        ParagraphKind::Heading(level) => HtmlBlock::Heading { level, text },
        ParagraphKind::Paragraph => HtmlBlock::Paragraph { text },
        ParagraphKind::ListItem => HtmlBlock::ListItem {
            bullet: "•".into(),
            text,
        },
    });
}

fn trim_odt_runs(runs: &mut Vec<HtmlTextRun>) {
    let mut first_nonempty = 0usize;
    while first_nonempty < runs.len() {
        runs[first_nonempty].text = runs[first_nonempty].text.trim_start().to_owned();
        if runs[first_nonempty].text.is_empty() {
            first_nonempty += 1;
        } else {
            break;
        }
    }
    if first_nonempty > 0 {
        runs.drain(..first_nonempty);
    }
    let mut end = runs.len();
    while end > 0 {
        runs[end - 1].text = runs[end - 1].text.trim_end().to_owned();
        if runs[end - 1].text.is_empty() {
            end -= 1;
        } else {
            break;
        }
    }
    runs.truncate(end);
}

fn odt_style_is_empty(style: &HtmlTextStyle) -> bool {
    style.font_family.is_none()
        && style.font_size.is_none()
        && style.bold.is_none()
        && style.italic.is_none()
        && style.color.is_none()
}

fn append_cell(
    value: String,
    repeats: usize,
    row: &mut Option<Vec<String>>,
    row_text_bytes: &mut usize,
) -> Result<()> {
    let row = row
        .as_mut()
        .ok_or_else(|| Error::InvalidInput("ODT table cell appears outside a table row".into()))?;
    if row.len().saturating_add(repeats) > MAX_ODT_TABLE_CELLS {
        return Err(Error::LimitExceeded(format!(
            "ODT table row exceeds {MAX_ODT_TABLE_CELLS} cells"
        )));
    }
    let added_text_bytes = value
        .len()
        .checked_mul(repeats)
        .ok_or_else(|| Error::LimitExceeded("ODT table text size overflowed".into()))?;
    *row_text_bytes = row_text_bytes
        .checked_add(added_text_bytes)
        .ok_or_else(|| Error::LimitExceeded("ODT table text size overflowed".into()))?;
    if *row_text_bytes > MAX_ODT_EXPANDED_TABLE_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "ODT expanded table text exceeds {MAX_ODT_EXPANDED_TABLE_TEXT_BYTES} bytes"
        )));
    }
    row.extend(std::iter::repeat_n(value, repeats));
    Ok(())
}

fn append_row(
    values: Vec<String>,
    row_text_bytes: usize,
    repeats: usize,
    is_header: bool,
    table: &mut Option<TableBuilder>,
) -> Result<()> {
    let table = table
        .as_mut()
        .ok_or_else(|| Error::InvalidInput("ODT table row appears outside a table".into()))?;
    let total_rows = table.rows.len() + usize::from(!table.headers.is_empty());
    if total_rows.saturating_add(repeats) > MAX_ODT_TABLE_CELLS {
        return Err(Error::LimitExceeded(format!(
            "ODT table exceeds {MAX_ODT_TABLE_CELLS} rows"
        )));
    }
    let added_cells = values
        .len()
        .checked_mul(repeats)
        .ok_or_else(|| Error::LimitExceeded("ODT table cell count overflowed".into()))?;
    table.total_cells = table
        .total_cells
        .checked_add(added_cells)
        .ok_or_else(|| Error::LimitExceeded("ODT table cell count overflowed".into()))?;
    if table.total_cells > MAX_ODT_TABLE_CELLS {
        return Err(Error::LimitExceeded(format!(
            "ODT table exceeds {MAX_ODT_TABLE_CELLS} cells"
        )));
    }
    let added_text_bytes = row_text_bytes
        .checked_mul(repeats)
        .ok_or_else(|| Error::LimitExceeded("ODT expanded table text size overflowed".into()))?;
    table.total_text_bytes = table
        .total_text_bytes
        .checked_add(added_text_bytes)
        .ok_or_else(|| Error::LimitExceeded("ODT expanded table text size overflowed".into()))?;
    if table.total_text_bytes > MAX_ODT_EXPANDED_TABLE_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "ODT expanded table text exceeds {MAX_ODT_EXPANDED_TABLE_TEXT_BYTES} bytes"
        )));
    }
    for _ in 0..repeats {
        if is_header && table.headers.is_empty() {
            table.headers = values.clone();
        } else {
            table.rows.push(values.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_document_type_declarations_and_limits_xml_depth() {
        let doctype = r#"<!DOCTYPE office:document-content [<!ENTITY x "expanded">]><office:document-content/>"#;
        assert!(matches!(
            parse_content(doctype, 100, None),
            Err(Error::InvalidInput(_))
        ));

        let nested = format!("{}<office:text/>{}", "<x>".repeat(300), "</x>".repeat(300));
        assert!(matches!(
            parse_content(&nested, 2_000, None),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    fn repeated_cells_cannot_expand_past_the_table_budget() {
        let xml = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"><office:body><office:text><table:table><table:table-row><table:table-cell table:number-columns-repeated="10001"/></table:table-row></table:table></office:text></office:body></office:document-content>"#;
        assert!(matches!(
            parse_content(xml, 1_000, None),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    fn repeated_rows_cannot_amplify_large_text_into_unbounded_memory() {
        let content = "T".repeat(40_000);
        let xml = format!(
            "<office:document-content xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\"><office:body><office:text><table:table><table:table-row table:number-rows-repeated=\"1000\"><table:table-cell><text:p>{content}</text:p></table:table-cell></table:table-row></table:table></office:text></office:body></office:document-content>"
        );
        let error = parse_content(&xml, 10_000, None).unwrap_err();
        assert!(error.to_string().contains("expanded table text"));
    }

    #[test]
    fn inline_binary_image_payload_is_embedded_without_becoming_text() {
        let xml = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:text><text:p>before<draw:frame draw:name="inline"><draw:image xlink:href="Pictures/fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame>after</text:p></office:text></office:body></office:document-content>"#;
        let (blocks, warnings) = parse_content(xml, 1_000, None).unwrap();
        let rendered_text = format!("{blocks:?}");

        assert!(rendered_text.contains("before"));
        assert!(rendered_text.contains("after"));
        assert!(rendered_text.contains("data:image/png;base64,"));
        assert!(
            blocks
                .iter()
                .any(|block| matches!(block, HtmlBlock::Image { .. }))
        );
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("binary-data"))
        );
    }
}
