//! OpenDocument Presentation (ODP/OTP/FODP) slide preview.
//!
//! The bounded reader preserves page size, slide order, common vector shapes,
//! direct graphic styles, text, and bounded package-linked PNG/JPEG frames.
//! Unsupported media and complex effects are reported instead of being
//! silently presented as complete.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};
use crate::ooxml::{
    ZipPackage, attribute, color_from_hex, decode_xml_reference, local_name, resolve_part_target,
};

const ODP_MIMETYPE_LIMIT: u64 = 256;
const MAX_ODP_XML_DEPTH: usize = 256;
const MAX_ODP_SLIDES: usize = 10_000;
const MAX_ODP_SHAPES: usize = 200_000;
const MAX_ODP_STYLES: usize = 100_000;
const MAX_ODP_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ODP_TEXT_LINES: usize = 200_000;
const MAX_ODP_IMAGES: usize = 10_000;
const MAX_ODP_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ODP_TOTAL_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ODP_TOTAL_DATA_URI_BYTES: usize = 48 * 1024 * 1024;
const MAX_ODP_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_ODP_TOTAL_IMAGE_PIXELS: u64 = 100_000_000;
const DEFAULT_SLIDE_WIDTH: f64 = 960.0;
const DEFAULT_SLIDE_HEIGHT: f64 = 540.0;

#[derive(Clone, Debug, Default)]
struct OdpStyle {
    fill: Option<Paint>,
    stroke: Option<Stroke>,
    font_family: Option<String>,
    font_size: Option<f64>,
    font_color: Option<String>,
    bold: Option<bool>,
    italic: Option<bool>,
}

#[derive(Default)]
struct OdpDefinitions {
    styles: HashMap<String, OdpStyle>,
    style_parents: HashMap<String, String>,
    text_styles: HashMap<String, OdpStyle>,
    text_style_parents: HashMap<String, String>,
    page_layout_sizes: HashMap<String, (f64, f64)>,
    master_page_layouts: HashMap<String, String>,
    page_width: Option<f64>,
    page_height: Option<f64>,
}

#[derive(Clone, Debug)]
struct OdpShape {
    shape_id: usize,
    kind: String,
    name: String,
    style_name: Option<String>,
    text_style_name: Option<String>,
    direct_style: OdpStyle,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    line_end: Option<(f64, f64)>,
    text: String,
    paragraph_depth: usize,
    image_href: Option<String>,
}

#[derive(Default)]
struct OdpImageBudget {
    count: usize,
    decoded_bytes: usize,
    data_uri_bytes: usize,
    pixels: u64,
}

struct OdpMediaContext<'a> {
    package: Option<&'a mut ZipPackage<File>>,
    budget: &'a mut OdpImageBudget,
}

fn attach_odp_image(
    element: &BytesStart<'_>,
    shape: Option<&mut OdpShape>,
    package: Option<&mut ZipPackage<File>>,
    budget: &mut OdpImageBudget,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let Some(shape) = shape else {
        push_odp_warning_once(warnings, "ODP image outside a supported frame was omitted");
        return Ok(());
    };
    let Some(package) = package else {
        push_odp_warning_once(
            warnings,
            "flat OpenDocument images are omitted; external resources are not fetched",
        );
        return Ok(());
    };
    let Some(href) = attribute(element, b"href").filter(|href| !href.trim().is_empty()) else {
        push_odp_warning_once(warnings, "ODP image without a package href was omitted");
        return Ok(());
    };
    if href.contains(':') || href.starts_with("//") {
        push_odp_warning_once(warnings, "external ODP image resources are not fetched");
        return Ok(());
    }
    if budget.count >= MAX_ODP_IMAGES {
        push_odp_warning_once(
            warnings,
            "ODP image count exceeded the supported limit; remaining images were omitted",
        );
        return Ok(());
    }
    budget.count += 1;

    let target = match resolve_part_target("content.xml", &href) {
        Ok(target) => target,
        Err(_) => {
            push_odp_warning_once(
                warnings,
                "ODP image path escaped the package or was invalid and was omitted",
            );
            return Ok(());
        }
    };
    let bytes = match package.read_optional_limited(&target, MAX_ODP_IMAGE_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            push_odp_warning_once(warnings, "missing ODP image parts were omitted");
            return Ok(());
        }
        Err(Error::LimitExceeded(_)) => {
            push_odp_warning_once(
                warnings,
                "ODP image parts exceeding the per-image byte limit were omitted",
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else {
        push_odp_warning_once(
            warnings,
            "unsupported ODP image types were omitted; only PNG and JPEG are embedded",
        );
        return Ok(());
    };
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
        push_odp_warning_once(warnings, "invalid ODP PNG/JPEG images were omitted");
        return Ok(());
    };
    let pixels = u64::from(width) * u64::from(height);
    let next_pixels = budget.pixels.saturating_add(pixels);
    if width == 0
        || height == 0
        || pixels > MAX_ODP_IMAGE_PIXELS
        || next_pixels > MAX_ODP_TOTAL_IMAGE_PIXELS
    {
        push_odp_warning_once(
            warnings,
            "ODP images exceeding the per-image or total pixel limit were omitted",
        );
        return Ok(());
    }
    let next_decoded_bytes = budget.decoded_bytes.saturating_add(bytes.len());
    if next_decoded_bytes > MAX_ODP_TOTAL_IMAGE_BYTES {
        push_odp_warning_once(
            warnings,
            "ODP images exceeding the total decoded image byte limit were omitted",
        );
        return Ok(());
    }
    let uri_prefix = format!("data:{mime};base64,");
    let encoded_bytes = bytes.len().div_ceil(3).saturating_mul(4);
    let uri_bytes = uri_prefix.len().saturating_add(encoded_bytes);
    let next_uri_bytes = budget.data_uri_bytes.saturating_add(uri_bytes);
    if next_uri_bytes > MAX_ODP_TOTAL_DATA_URI_BYTES {
        push_odp_warning_once(
            warnings,
            "ODP images exceeding the total data URI byte limit were omitted",
        );
        return Ok(());
    }

    shape.image_href = Some(format!("{uri_prefix}{}", BASE64_STANDARD.encode(&bytes)));
    budget.decoded_bytes = next_decoded_bytes;
    budget.data_uri_bytes = next_uri_bytes;
    budget.pixels = next_pixels;
    Ok(())
}

fn attach_odp_inline_image(
    data: &str,
    href: Option<&str>,
    shape: Option<&mut OdpShape>,
    budget: &mut OdpImageBudget,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let Some(shape) = shape else {
        push_odp_warning_once(
            warnings,
            "ODP inline image outside a supported frame was omitted",
        );
        return Ok(());
    };
    if data.trim().is_empty() {
        if href.is_some_and(|href| href.contains(':') || href.starts_with("//")) {
            push_odp_warning_once(warnings, "external ODP image resources are not fetched");
        } else {
            push_odp_warning_once(
                warnings,
                "ODP inline office:binary-data image was empty or malformed",
            );
        }
        return Ok(());
    }
    if budget.count >= MAX_ODP_IMAGES {
        push_odp_warning_once(
            warnings,
            "ODP image count exceeded the supported limit; remaining images were omitted",
        );
        return Ok(());
    }
    budget.count += 1;
    let compact: String = data
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    let bytes = match BASE64_STANDARD.decode(compact.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => {
            push_odp_warning_once(
                warnings,
                "malformed ODP inline office:binary-data image was omitted",
            );
            return Ok(());
        }
    };
    if bytes.len() as u64 > MAX_ODP_IMAGE_BYTES {
        push_odp_warning_once(
            warnings,
            "ODP inline image exceeded the per-image byte limit",
        );
        return Ok(());
    }
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else {
        push_odp_warning_once(
            warnings,
            "unsupported ODP inline image type was omitted; only PNG and JPEG are embedded",
        );
        return Ok(());
    };
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
        push_odp_warning_once(warnings, "invalid ODP inline PNG/JPEG image was omitted");
        return Ok(());
    };
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    let next_pixels = budget.pixels.saturating_add(pixels);
    if width == 0
        || height == 0
        || pixels > MAX_ODP_IMAGE_PIXELS
        || next_pixels > MAX_ODP_TOTAL_IMAGE_PIXELS
    {
        push_odp_warning_once(warnings, "ODP inline images exceeded the pixel limit");
        return Ok(());
    }
    let next_bytes = budget.decoded_bytes.saturating_add(bytes.len());
    if next_bytes > MAX_ODP_TOTAL_IMAGE_BYTES {
        push_odp_warning_once(warnings, "ODP inline images exceeded the total byte limit");
        return Ok(());
    }
    let prefix = format!("data:{mime};base64,");
    let next_uri = budget
        .data_uri_bytes
        .saturating_add(prefix.len() + bytes.len().div_ceil(3) * 4);
    if next_uri > MAX_ODP_TOTAL_DATA_URI_BYTES {
        push_odp_warning_once(
            warnings,
            "ODP inline images exceeded the total data URI limit",
        );
        return Ok(());
    }
    shape.image_href = Some(format!("{prefix}{}", BASE64_STANDARD.encode(&bytes)));
    budget.decoded_bytes = next_bytes;
    budget.data_uri_bytes = next_uri;
    budget.pixels = next_pixels;
    Ok(())
}

fn push_odp_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OdfVisualKind {
    Presentation,
    Drawing,
}

impl OdfVisualKind {
    fn flat_extension(self) -> &'static str {
        match self {
            Self::Presentation => "fodp",
            Self::Drawing => "fodg",
        }
    }

    fn source_format(self) -> &'static str {
        match self {
            Self::Presentation => "odp",
            Self::Drawing => "odg",
        }
    }

    fn content_container(self) -> &'static str {
        match self {
            Self::Presentation => "presentation",
            Self::Drawing => "drawing",
        }
    }

    fn mimetype(self, template: bool) -> &'static str {
        match (self, template) {
            (Self::Presentation, false) => "application/vnd.oasis.opendocument.presentation",
            (Self::Presentation, true) => {
                "application/vnd.oasis.opendocument.presentation-template"
            }
            (Self::Drawing, false) => "application/vnd.oasis.opendocument.graphics",
            (Self::Drawing, true) => "application/vnd.oasis.opendocument.graphics-template",
        }
    }

    fn legacy_mimetype(self, template: bool) -> &'static str {
        match (self, template) {
            (Self::Presentation, false) => "application/vnd.sun.xml.impress",
            (Self::Presentation, true) => "application/vnd.sun.xml.impress.template",
            (Self::Drawing, false) => "application/vnd.sun.xml.draw",
            (Self::Drawing, true) => "application/vnd.sun.xml.draw.template",
        }
    }

    fn document_label(self) -> &'static str {
        match self {
            Self::Presentation => "Presentation",
            Self::Drawing => "Drawing",
        }
    }

    fn page_label(self, page_number: usize) -> String {
        match self {
            Self::Presentation => format!("Slide {page_number}"),
            Self::Drawing => format!("Page {page_number}"),
        }
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_odf_visual(path, options, sink, OdfVisualKind::Presentation)
}

pub(crate) fn convert_drawing(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_odf_visual(path, options, sink, OdfVisualKind::Drawing)
}

fn convert_odf_visual(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    kind: OdfVisualKind,
) -> Result<Vec<String>> {
    let is_flat = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(kind.flat_extension()));
    let mut package = None::<ZipPackage<File>>;
    let (content, styles) = if is_flat {
        (read_bounded_file(path, options.max_input_bytes)?, None)
    } else {
        let mut zip_package = ZipPackage::open(path, options.max_zip_entry_bytes)?;
        let mimetype = zip_package.read_limited("mimetype", ODP_MIMETYPE_LIMIT)?;
        let mimetype = std::str::from_utf8(&mimetype)
            .map_err(|error| Error::InvalidInput(format!("ODP mimetype is not UTF-8: {error}")))?
            .trim();
        if mimetype != kind.mimetype(false)
            && mimetype != kind.mimetype(true)
            && mimetype != kind.legacy_mimetype(false)
            && mimetype != kind.legacy_mimetype(true)
        {
            return Err(Error::InvalidInput(format!(
                "unsupported OpenDocument {} mimetype '{mimetype}'",
                kind.document_label().to_ascii_lowercase()
            )));
        }
        let content = zip_package.read("content.xml")?;
        let styles = zip_package.read_optional("styles.xml")?;
        package = Some(zip_package);
        (content, styles)
    };

    let content = String::from_utf8(content).map_err(|error| {
        Error::InvalidInput(format!("OpenDocument content is not UTF-8: {error}"))
    })?;
    let mut definitions = OdpDefinitions::default();
    let mut warnings = Vec::new();
    if let Some(styles) = styles {
        let styles = String::from_utf8(styles)
            .map_err(|error| Error::InvalidInput(format!("ODP styles are not UTF-8: {error}")))?;
        parse_odp_xml(
            &styles,
            kind,
            options.max_xml_events,
            &mut definitions,
            None,
            &mut warnings,
        )?;
    }
    let mut page_count = 0usize;
    let mut image_budget = OdpImageBudget::default();
    parse_odp_xml_with_media(
        &content,
        kind,
        options.max_xml_events,
        &mut definitions,
        Some((&mut page_count, options.max_pages.min(MAX_ODP_SLIDES), sink)),
        OdpMediaContext {
            package: package.as_mut(),
            budget: &mut image_budget,
        },
        &mut warnings,
    )?;
    if page_count == 0 {
        return Err(Error::InvalidInput(format!(
            "OpenDocument {} contains no pages",
            kind.document_label()
        )));
    }
    let warning = match kind {
        OdfVisualKind::Presentation => {
            "ODP text wrapping, master-page backgrounds, animation, and exact font metrics are approximated"
        }
        OdfVisualKind::Drawing => {
            "ODG text wrapping, master-page backgrounds, and exact font metrics are approximated"
        }
    };
    warnings.insert(0, warning.into());
    Ok(warnings)
}

fn read_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    Read::take(&mut file, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "FODP input exceeds maximum bytes ({max_bytes})"
        )));
    }
    Ok(bytes)
}

fn parse_odp_xml(
    xml: &str,
    kind: OdfVisualKind,
    max_events: usize,
    definitions: &mut OdpDefinitions,
    render: Option<(&mut usize, usize, &mut dyn PageConsumer)>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut image_budget = OdpImageBudget::default();
    parse_odp_xml_with_media(
        xml,
        kind,
        max_events,
        definitions,
        render,
        OdpMediaContext {
            package: None,
            budget: &mut image_budget,
        },
        warnings,
    )
}

fn parse_odp_xml_with_media(
    xml: &str,
    kind: OdfVisualKind,
    max_events: usize,
    definitions: &mut OdpDefinitions,
    mut render: Option<(&mut usize, usize, &mut dyn PageConsumer)>,
    mut media: OdpMediaContext<'_>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut style_name: Option<String> = None;
    let mut style_family: Option<String> = None;
    let mut style_parent: Option<String> = None;
    let mut page_layout_name: Option<String> = None;
    let mut style_value: Option<OdpStyle> = None;
    let mut page: Option<Page> = None;
    let mut shape: Option<OdpShape> = None;
    let mut shape_count = 0usize;
    let mut total_text_bytes = 0usize;
    let mut total_text_lines = 0usize;
    let mut event_count = 0usize;
    let mut warned_media = false;
    let mut warned_group = false;
    let mut warned_unknown_shape = false;
    let mut warned_missing_style = false;
    let mut warned_notes = false;
    let mut inline_image_data = None::<String>;
    let mut inline_image_href = None::<String>;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_events {
            return Err(Error::LimitExceeded(format!(
                "ODP XML exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(ref element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                let local = String::from_utf8_lossy(name).into_owned();
                if name == b"style"
                    && let Some(family) = attribute(element, b"family")
                    && matches!(family.as_str(), "graphic" | "paragraph" | "presentation")
                {
                    style_name = attribute(element, b"name");
                    style_family = Some(family);
                    style_parent = attribute(element, b"parent-style-name");
                    style_value = Some(OdpStyle::default());
                } else if name == b"graphic-properties" {
                    if let Some(style) = style_value.as_mut() {
                        merge_style(style, style_from_properties(element, false));
                    }
                } else if name == b"text-properties" {
                    if let Some(style) = style_value.as_mut() {
                        merge_style(style, style_from_properties(element, true));
                    }
                } else if name == b"page-layout" {
                    page_layout_name = attribute(element, b"name");
                } else if name == b"master-page" {
                    if let (Some(name), Some(layout)) = (
                        attribute(element, b"name"),
                        attribute(element, b"page-layout-name"),
                    ) {
                        definitions.master_page_layouts.insert(name, layout);
                    }
                } else if name == b"page-layout-properties" {
                    let width = attribute(element, b"page-width")
                        .and_then(|value| parse_odf_length(&value))
                        .filter(|value| (72.0..=100_000.0).contains(value));
                    let height = attribute(element, b"page-height")
                        .and_then(|value| parse_odf_length(&value))
                        .filter(|value| (72.0..=100_000.0).contains(value));
                    if let (Some(width), Some(height)) = (width, height) {
                        if let Some(name) = page_layout_name.clone() {
                            definitions.page_layout_sizes.insert(name, (width, height));
                        }
                        if definitions.page_width.is_none() {
                            definitions.page_width = Some(width);
                            definitions.page_height = Some(height);
                        }
                    }
                }

                let in_notes = stack.iter().any(|item| item == "notes");
                if page.is_some() && name == b"notes" && !warned_notes {
                    warnings.push("ODP speaker notes and slide thumbnails are omitted".into());
                    warned_notes = true;
                }
                if page.is_some()
                    && !in_notes
                    && matches!(name, b"object" | b"object-ole" | b"chart" | b"table")
                    && !warned_media
                {
                    warnings
                        .push("ODP embedded charts, tables, and OLE objects are omitted".into());
                    warned_media = true;
                }
                if page.is_some() && !in_notes && name == b"image" {
                    if media.package.is_some() {
                        attach_odp_image(
                            element,
                            shape.as_mut(),
                            media.package.as_deref_mut(),
                            &mut *media.budget,
                            warnings,
                        )?;
                    } else {
                        inline_image_href = attribute(element, b"href");
                        inline_image_data = Some(String::new());
                    }
                } else if page.is_some()
                    && !in_notes
                    && name == b"binary-data"
                    && stack.iter().any(|item| item == "image")
                {
                    inline_image_data.get_or_insert_with(String::new);
                }
                if page.is_some() && !in_notes && name == b"g" && !warned_group {
                    warnings.push(
                        "ODP grouped-shape transforms are approximated; child shapes use their local coordinates".into(),
                    );
                    warned_group = true;
                }
                if page.is_some()
                    && !in_notes
                    && matches!(
                        name,
                        b"path" | b"polygon" | b"polyline" | b"connector" | b"custom-shape"
                    )
                    && !warned_unknown_shape
                {
                    warnings.push(
                        "ODP custom paths, connectors, and polygons are not reconstructed".into(),
                    );
                    warned_unknown_shape = true;
                }
                if page.is_some()
                    && name == b"p"
                    && let Some(shape) = shape.as_mut()
                {
                    if !shape.text.is_empty() && !shape.text.ends_with('\n') {
                        append_shape_text(
                            shape,
                            "\n",
                            &mut total_text_bytes,
                            &mut total_text_lines,
                        )?;
                    }
                    shape.paragraph_depth = shape.paragraph_depth.saturating_add(1);
                }

                if let Some((page_number, max_pages, _)) = render.as_mut()
                    && name == b"page"
                    && stack.iter().any(|item| item == kind.content_container())
                {
                    if **page_number >= *max_pages {
                        return Err(Error::LimitExceeded(format!(
                            "ODP slide count exceeds {max_pages}"
                        )));
                    }
                    **page_number += 1;
                    let (width, height) = page_dimensions(element, definitions);
                    let mut new_page =
                        Page::new(**page_number, width, height, kind.source_format());
                    new_page.nodes.push(slide_background(width, height));
                    let slide_name = attribute(element, b"name")
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or_else(|| kind.page_label(**page_number));
                    new_page.title = slide_name;
                    new_page.description = format!(
                        "OpenDocument {} page",
                        kind.document_label().to_ascii_lowercase()
                    );
                    page = Some(new_page);
                } else if page.is_some() && !in_notes && shape.is_none() && is_supported_shape(name)
                {
                    shape_count = shape_count.saturating_add(1);
                    if shape_count > MAX_ODP_SHAPES {
                        return Err(Error::LimitExceeded(format!(
                            "ODP shape count exceeds {MAX_ODP_SHAPES}"
                        )));
                    }
                    if let Some(parsed) = shape_from_element(element, name, shape_count)? {
                        if has_unresolved_shape_style(&parsed, definitions) && !warned_missing_style
                        {
                            warnings.push(
                                "some ODP named graphic or text styles were not found; default visual properties were used".into(),
                            );
                            warned_missing_style = true;
                        }
                        shape = Some(parsed);
                    }
                }
                if stack.len() >= MAX_ODP_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "ODP XML nesting exceeds {MAX_ODP_XML_DEPTH} elements"
                    )));
                }
                stack.push(local);
            }
            Event::Empty(ref element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if name == b"graphic-properties" {
                    if let Some(style) = style_value.as_mut() {
                        merge_style(style, style_from_properties(element, false));
                    }
                } else if name == b"text-properties" {
                    if let Some(style) = style_value.as_mut() {
                        merge_style(style, style_from_properties(element, true));
                    }
                } else if name == b"master-page" {
                    if let (Some(name), Some(layout)) = (
                        attribute(element, b"name"),
                        attribute(element, b"page-layout-name"),
                    ) {
                        definitions.master_page_layouts.insert(name, layout);
                    }
                } else if name == b"page-layout-properties" {
                    let width = attribute(element, b"page-width")
                        .and_then(|value| parse_odf_length(&value))
                        .filter(|value| (72.0..=100_000.0).contains(value));
                    let height = attribute(element, b"page-height")
                        .and_then(|value| parse_odf_length(&value))
                        .filter(|value| (72.0..=100_000.0).contains(value));
                    if let (Some(width), Some(height)) = (width, height) {
                        if let Some(name) = page_layout_name.clone() {
                            definitions.page_layout_sizes.insert(name, (width, height));
                        }
                        if definitions.page_width.is_none() {
                            definitions.page_width = Some(width);
                            definitions.page_height = Some(height);
                        }
                    }
                } else if name == b"image"
                    && page.is_some()
                    && !stack.iter().any(|item| item == "notes")
                {
                    attach_odp_image(
                        element,
                        shape.as_mut(),
                        media.package.as_deref_mut(),
                        &mut *media.budget,
                        warnings,
                    )?;
                } else if name == b"binary-data"
                    && page.is_some()
                    && stack.iter().any(|item| item == "image")
                {
                    if let Some(shape) = shape.as_mut() {
                        shape.image_href = None;
                    }
                    push_odp_warning_once(
                        warnings,
                        "ODP inline office:binary-data images are omitted; package-linked PNG/JPEG images are supported",
                    );
                } else if name == b"page" {
                    if let Some((page_number, max_pages, sink)) = render.as_mut()
                        && stack.iter().any(|item| item == kind.content_container())
                    {
                        if **page_number >= *max_pages {
                            return Err(Error::LimitExceeded(format!(
                                "ODP slide count exceeds {max_pages}"
                            )));
                        }
                        **page_number += 1;
                        let (width, height) = page_dimensions(element, definitions);
                        let mut page =
                            Page::new(**page_number, width, height, kind.source_format());
                        page.nodes.push(slide_background(width, height));
                        page.title = attribute(element, b"name")
                            .unwrap_or_else(|| kind.page_label(**page_number));
                        page.description = format!(
                            "OpenDocument {} page",
                            kind.document_label().to_ascii_lowercase()
                        );
                        sink.consume(page)?;
                    }
                } else if page.is_some()
                    && !stack.iter().any(|item| item == "notes")
                    && is_supported_shape(name)
                {
                    shape_count = shape_count.saturating_add(1);
                    if shape_count > MAX_ODP_SHAPES {
                        return Err(Error::LimitExceeded(format!(
                            "ODP shape count exceeds {MAX_ODP_SHAPES}"
                        )));
                    }
                    if let Some(shape) = shape_from_element(element, name, shape_count)? {
                        if has_unresolved_shape_style(&shape, definitions) && !warned_missing_style
                        {
                            warnings.push(
                                "some ODP named graphic or text styles were not found; default visual properties were used".into(),
                            );
                            warned_missing_style = true;
                        }
                        let rendered = render_shape(shape, definitions);
                        if let Some(page) = page.as_mut() {
                            page.nodes.extend(rendered);
                        }
                    }
                } else if page.is_some()
                    && matches!(name, b"s" | b"tab" | b"line-break")
                    && let Some(shape) = shape.as_mut()
                {
                    match name {
                        b"s" => {
                            let count = attribute(element, b"c")
                                .and_then(|value| value.parse::<usize>().ok())
                                .unwrap_or(1)
                                .clamp(1, 1000);
                            append_shape_text(
                                shape,
                                &" ".repeat(count),
                                &mut total_text_bytes,
                                &mut total_text_lines,
                            )?;
                        }
                        b"tab" => append_shape_text(
                            shape,
                            "\t",
                            &mut total_text_bytes,
                            &mut total_text_lines,
                        )?,
                        b"line-break" => append_shape_text(
                            shape,
                            "\n",
                            &mut total_text_bytes,
                            &mut total_text_lines,
                        )?,
                        _ => {}
                    }
                }
            }
            Event::Text(ref value) => {
                if stack.iter().any(|item| item == "binary-data") {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODP inline image encoding: {error}"))
                    })?;
                    if let Some(data) = inline_image_data.as_mut() {
                        if data.len().saturating_add(decoded.len())
                            > (MAX_ODP_IMAGE_BYTES as usize).saturating_mul(2)
                        {
                            return Err(Error::LimitExceeded(
                                "ODP inline image base64 exceeds the per-image limit".into(),
                            ));
                        }
                        data.push_str(&decoded);
                    }
                } else if let Some(shape) = shape.as_mut().filter(|shape| shape.paragraph_depth > 0)
                {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODP text encoding: {error}"))
                    })?;
                    let decoded = quick_xml::escape::unescape(&decoded).map_err(|error| {
                        Error::InvalidInput(format!("invalid ODP XML text: {error}"))
                    })?;
                    append_shape_text(
                        shape,
                        &decoded,
                        &mut total_text_bytes,
                        &mut total_text_lines,
                    )?;
                }
            }
            Event::CData(ref value) => {
                if stack.iter().any(|item| item == "binary-data") {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODP inline image encoding: {error}"))
                    })?;
                    if let Some(data) = inline_image_data.as_mut() {
                        if data.len().saturating_add(decoded.len())
                            > (MAX_ODP_IMAGE_BYTES as usize).saturating_mul(2)
                        {
                            return Err(Error::LimitExceeded(
                                "ODP inline image base64 exceeds the per-image limit".into(),
                            ));
                        }
                        data.push_str(&decoded);
                    }
                } else if let Some(shape) = shape.as_mut().filter(|shape| shape.paragraph_depth > 0)
                {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODP CDATA encoding: {error}"))
                    })?;
                    append_shape_text(
                        shape,
                        &decoded,
                        &mut total_text_bytes,
                        &mut total_text_lines,
                    )?;
                }
            }
            Event::GeneralRef(ref reference) => {
                if let Some(shape) = shape.as_mut().filter(|shape| shape.paragraph_depth > 0) {
                    let text = decode_xml_reference(reference, "ODP text")?;
                    append_shape_text(shape, &text, &mut total_text_bytes, &mut total_text_lines)?;
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "ODP document type declarations are not supported".into(),
                ));
            }
            Event::End(ref element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if name == b"style" {
                    if let (Some(name), Some(family), Some(style)) =
                        (style_name.take(), style_family.take(), style_value.take())
                    {
                        let (styles, parents) = if family == "paragraph" {
                            (
                                &mut definitions.text_styles,
                                &mut definitions.text_style_parents,
                            )
                        } else {
                            (&mut definitions.styles, &mut definitions.style_parents)
                        };
                        if !styles.contains_key(&name) && styles.len() >= MAX_ODP_STYLES {
                            return Err(Error::LimitExceeded(format!(
                                "ODP style count exceeds {MAX_ODP_STYLES}"
                            )));
                        }
                        styles.insert(name.clone(), style);
                        if let Some(parent) = style_parent.take() {
                            parents.insert(name, parent);
                        }
                    }
                } else if name == b"page-layout" {
                    page_layout_name = None;
                } else if page.is_some()
                    && name == b"p"
                    && let Some(shape) = shape.as_mut()
                {
                    shape.paragraph_depth = shape.paragraph_depth.saturating_sub(1);
                } else if page.is_some() && name == b"image" && inline_image_data.is_some() {
                    let data = inline_image_data.take().unwrap_or_default();
                    let href = inline_image_href.take();
                    attach_odp_inline_image(
                        data.as_str(),
                        href.as_deref(),
                        shape.as_mut(),
                        &mut *media.budget,
                        warnings,
                    )?;
                } else if page.is_some()
                    && shape
                        .as_ref()
                        .is_some_and(|shape| shape.kind.as_bytes() == name)
                {
                    let finished = shape.take().ok_or_else(|| {
                        Error::InvalidInput("ODP shape close has no active shape".into())
                    })?;
                    let rendered = render_shape(finished, definitions);
                    if let Some(page) = page.as_mut() {
                        page.nodes.extend(rendered);
                    }
                } else if name == b"page" && page.is_some() {
                    let finished = page.take().ok_or_else(|| {
                        Error::InvalidInput("ODP slide close has no open slide".into())
                    })?;
                    if let Some((_, _, sink)) = render.as_mut() {
                        sink.consume(finished)?;
                    }
                }
                if stack.pop().as_deref() != Some(String::from_utf8_lossy(name).as_ref()) {
                    return Err(Error::InvalidInput(format!(
                        "mismatched ODP XML end tag '{}'",
                        String::from_utf8_lossy(name)
                    )));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if page.is_some() || shape.is_some() {
        return Err(Error::InvalidInput("incomplete ODP XML document".into()));
    }
    Ok(())
}

fn is_supported_shape(name: &[u8]) -> bool {
    matches!(
        name,
        b"rect" | b"ellipse" | b"circle" | b"line" | b"frame" | b"text-box"
    )
}

fn page_dimensions(element: &BytesStart<'_>, definitions: &OdpDefinitions) -> (f64, f64) {
    let page_size = attribute(element, b"master-page-name")
        .and_then(|master| definitions.master_page_layouts.get(&master))
        .and_then(|layout| definitions.page_layout_sizes.get(layout))
        .copied();
    page_size.unwrap_or((
        definitions.page_width.unwrap_or(DEFAULT_SLIDE_WIDTH),
        definitions.page_height.unwrap_or(DEFAULT_SLIDE_HEIGHT),
    ))
}

fn slide_background(width: f64, height: f64) -> Node {
    Node::Path {
        id: "odp-slide-background".into(),
        d: format!("M 0 0 H {} V {} H 0 Z", fmt(width), fmt(height)),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#ffffff"),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            kind: "office:drawing".into(),
            semantic_role: "office:slide-background".into(),
            ..SourceMeta::default()
        },
    }
}

fn shape_from_element(
    element: &BytesStart<'_>,
    name: &[u8],
    shape_id: usize,
) -> Result<Option<OdpShape>> {
    let kind = String::from_utf8_lossy(name).into_owned();
    let x_attribute = if name == b"line" {
        b"x1".as_slice()
    } else {
        b"x"
    };
    let y_attribute = if name == b"line" {
        b"y1".as_slice()
    } else {
        b"y"
    };
    let x = read_geometry_attribute(element, x_attribute, 0.0)?;
    let y = read_geometry_attribute(element, y_attribute, 0.0)?;
    let width = read_geometry_attribute(element, b"width", 0.0)?;
    let height = read_geometry_attribute(element, b"height", 0.0)?;
    let line_end = if name == b"line" {
        Some((
            read_geometry_attribute(element, b"x2", x + width)?,
            read_geometry_attribute(element, b"y2", y + height)?,
        ))
    } else {
        None
    };
    if name != b"line" && (width <= 0.0 || height <= 0.0) {
        return Ok(None);
    }
    Ok(Some(OdpShape {
        shape_id,
        kind,
        name: attribute(element, b"name").unwrap_or_default(),
        style_name: attribute(element, b"style-name"),
        text_style_name: attribute(element, b"text-style-name"),
        direct_style: style_from_properties(element, false),
        x,
        y,
        width,
        height,
        line_end,
        text: String::new(),
        paragraph_depth: 0,
        image_href: None,
    }))
}

fn read_geometry_attribute(element: &BytesStart<'_>, name: &[u8], default: f64) -> Result<f64> {
    let value = match attribute(element, name) {
        Some(value) => parse_odf_length(&value).ok_or_else(|| {
            Error::InvalidInput(format!(
                "ODP shape geometry {} has an invalid unit or number",
                String::from_utf8_lossy(name)
            ))
        })?,
        None => default,
    };
    if !value.is_finite() || value.abs() > 1.0e7 {
        return Err(Error::InvalidInput(format!(
            "ODP shape {} has invalid geometry",
            String::from_utf8_lossy(name)
        )));
    }
    Ok(value)
}

fn has_unresolved_shape_style(shape: &OdpShape, definitions: &OdpDefinitions) -> bool {
    shape
        .style_name
        .as_ref()
        .is_some_and(|name| !definitions.styles.contains_key(name))
        || shape
            .text_style_name
            .as_ref()
            .is_some_and(|name| !definitions.text_styles.contains_key(name))
}

fn append_shape_text(
    shape: &mut OdpShape,
    text: &str,
    total_bytes: &mut usize,
    total_lines: &mut usize,
) -> Result<()> {
    *total_bytes = (*total_bytes).saturating_add(text.len());
    if *total_bytes > MAX_ODP_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "ODP slide text exceeds {MAX_ODP_TEXT_BYTES} bytes"
        )));
    }
    *total_lines =
        (*total_lines).saturating_add(text.bytes().filter(|byte| *byte == b'\n').count());
    if *total_lines > MAX_ODP_TEXT_LINES {
        return Err(Error::LimitExceeded(format!(
            "ODP slide text exceeds {MAX_ODP_TEXT_LINES} lines"
        )));
    }
    shape.text.push_str(text);
    Ok(())
}

fn style_from_properties(element: &BytesStart<'_>, text_properties: bool) -> OdpStyle {
    let mut style = OdpStyle::default();
    if text_properties {
        style.font_family = attribute(element, b"font-name")
            .or_else(|| attribute(element, b"font-family"))
            .filter(|value| !value.is_empty());
        style.font_size = attribute(element, b"font-size")
            .as_deref()
            .and_then(parse_odf_length)
            .filter(|size| (1.0..=512.0).contains(size));
        style.font_color =
            attribute(element, b"color").map(|color| color_from_hex(&color, "#111827"));
        style.bold = attribute(element, b"font-weight").map(|value| {
            value.eq_ignore_ascii_case("bold")
                || value.parse::<u16>().is_ok_and(|weight| weight >= 600)
        });
        style.italic =
            attribute(element, b"font-style").map(|value| !value.eq_ignore_ascii_case("normal"));
        return style;
    }

    let fill_mode = attribute(element, b"fill");
    let fill_color = attribute(element, b"fill-color");
    if fill_mode
        .as_deref()
        .is_some_and(|mode| mode.eq_ignore_ascii_case("none"))
    {
        style.fill = Some(Paint::None);
    } else if let Some(color) = fill_color {
        style.fill = Some(Paint::solid(color_from_hex(&color, "#ffffff")));
    }
    let stroke_mode = attribute(element, b"stroke");
    let stroke_color = attribute(element, b"stroke-color");
    if stroke_mode
        .as_deref()
        .is_some_and(|mode| mode.eq_ignore_ascii_case("none"))
    {
        style.stroke = Some(Stroke::default());
    } else if let Some(color) = stroke_color {
        let width = attribute(element, b"stroke-width")
            .as_deref()
            .and_then(parse_odf_length)
            .filter(|width| (0.0..=512.0).contains(width))
            .unwrap_or(1.0);
        style.stroke = Some(Stroke {
            paint: Paint::solid(color_from_hex(&color, "#1f2937")),
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            miter_limit: 4.0,
            ..Stroke::default()
        });
    }
    style
}

fn merge_style(target: &mut OdpStyle, source: OdpStyle) {
    if source.fill.is_some() {
        target.fill = source.fill;
    }
    if source.stroke.is_some() {
        target.stroke = source.stroke;
    }
    if source.font_family.is_some() {
        target.font_family = source.font_family;
    }
    if source.font_size.is_some() {
        target.font_size = source.font_size;
    }
    if source.font_color.is_some() {
        target.font_color = source.font_color;
    }
    if source.bold.is_some() {
        target.bold = source.bold;
    }
    if source.italic.is_some() {
        target.italic = source.italic;
    }
}

fn resolve_style(shape: &OdpShape, definitions: &OdpDefinitions) -> OdpStyle {
    let mut style = shape
        .style_name
        .as_ref()
        .map_or_else(OdpStyle::default, |name| {
            resolve_named_style(name, &definitions.styles, &definitions.style_parents)
        });
    merge_style(&mut style, shape.direct_style.clone());
    style
}

fn resolve_named_style(
    name: &str,
    styles: &HashMap<String, OdpStyle>,
    parents: &HashMap<String, String>,
) -> OdpStyle {
    let mut chain = Vec::new();
    let mut current = Some(name.to_owned());
    while let Some(name) = current {
        if chain.len() >= 32 || chain.contains(&name) {
            break;
        }
        chain.push(name.clone());
        current = parents.get(&name).cloned();
    }
    let mut resolved = OdpStyle::default();
    for name in chain.into_iter().rev() {
        if let Some(style) = styles.get(&name) {
            merge_style(&mut resolved, style.clone());
        }
    }
    resolved
}

fn render_shape(shape: OdpShape, definitions: &OdpDefinitions) -> Vec<Node> {
    let style = resolve_style(&shape, definitions);
    let mut text_style = style.clone();
    if let Some(name) = shape.text_style_name.as_deref() {
        let resolved = resolve_named_style(
            name,
            &definitions.text_styles,
            &definitions.text_style_parents,
        );
        merge_style(&mut text_style, resolved);
    }
    let mut fill = style.fill.clone().unwrap_or(Paint::None);
    let mut stroke = style.stroke.clone().unwrap_or_default();
    let path;
    match shape.kind.as_str() {
        "line" => {
            let (x2, y2) = shape
                .line_end
                .unwrap_or((shape.x + shape.width, shape.y + shape.height));
            path = format!(
                "M {} {} L {} {}",
                fmt(shape.x),
                fmt(shape.y),
                fmt(x2),
                fmt(y2)
            );
            fill = Paint::None;
            if matches!(stroke, Stroke { width: 0.0, .. }) || matches!(stroke.paint, Paint::None) {
                stroke = default_stroke();
            }
        }
        "ellipse" | "circle" => {
            let rx = if shape.kind == "circle" {
                shape.width.min(shape.height) / 2.0
            } else {
                shape.width / 2.0
            };
            let ry = if shape.kind == "circle" {
                rx
            } else {
                shape.height / 2.0
            };
            let cx = shape.x + shape.width / 2.0;
            let cy = shape.y + shape.height / 2.0;
            let k = 0.552_284_749_830_793_6;
            path = format!(
                "M {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} Z",
                fmt(cx + rx),
                fmt(cy),
                fmt(cx + rx),
                fmt(cy + k * ry),
                fmt(cx + k * rx),
                fmt(cy + ry),
                fmt(cx),
                fmt(cy + ry),
                fmt(cx - k * rx),
                fmt(cy + ry),
                fmt(cx - rx),
                fmt(cy + k * ry),
                fmt(cx - rx),
                fmt(cy),
                fmt(cx - rx),
                fmt(cy - k * ry),
                fmt(cx - k * rx),
                fmt(cy - ry),
                fmt(cx),
                fmt(cy - ry),
                fmt(cx + k * rx),
                fmt(cy - ry),
                fmt(cx + rx),
                fmt(cy - k * ry),
                fmt(cx + rx),
                fmt(cy),
            );
        }
        _ => {
            path = format!(
                "M {} {} H {} V {} H {} Z",
                fmt(shape.x),
                fmt(shape.y),
                fmt(shape.x + shape.width),
                fmt(shape.y + shape.height),
                fmt(shape.x)
            );
        }
    }

    let mut nodes = Vec::new();
    if let Some(href) = shape.image_href.clone() {
        nodes.push(Node::Image {
            id: format!("odp-image-{}", shape.shape_id),
            href,
            x: shape.x,
            y: shape.y,
            width: shape.width,
            height: shape.height,
            transform: IDENTITY,
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                kind: "office:drawing".into(),
                source_id: shape.name.clone(),
                semantic_role: "office:image".into(),
                ..SourceMeta::default()
            },
        });
        if !matches!(&stroke.paint, Paint::None) && stroke.width > 0.0 {
            nodes.push(Node::Path {
                id: format!("odp-shape-{}-outline", shape.shape_id),
                d: path,
                fill_rule: "nonzero".into(),
                fill: Paint::None,
                stroke,
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    kind: "office:drawing".into(),
                    source_id: shape.name.clone(),
                    semantic_role: "office:shape-outline".into(),
                    ..SourceMeta::default()
                },
            });
        }
    } else {
        nodes.push(Node::Path {
            id: format!("odp-shape-{}", shape.shape_id),
            d: path,
            fill_rule: "nonzero".into(),
            fill,
            stroke,
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                kind: "office:drawing".into(),
                source_id: shape.name.clone(),
                semantic_role: "office:shape".into(),
                ..SourceMeta::default()
            },
        });
    }
    let normalized_text = shape
        .text
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    let text = normalized_text.trim_matches('\n');
    if !text.is_empty() {
        let font_size = text_style.font_size.unwrap_or(24.0);
        let font_family = text_style
            .font_family
            .unwrap_or_else(|| "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into());
        let fill = Paint::solid(text_style.font_color.unwrap_or_else(|| "#111827".into()));
        for (line_index, line) in text.lines().enumerate() {
            nodes.push(Node::Text {
                id: format!("odp-text-{}-{line_index}", shape.shape_id),
                x: shape.x + 6.0,
                y: shape.y + font_size + 5.0 + line_index as f64 * font_size * 1.25,
                runs: vec![TextRun {
                    text: line.to_owned(),
                    font_family: font_family.clone(),
                    font_size,
                    bold: text_style.bold.unwrap_or(false),
                    italic: text_style.italic.unwrap_or(false),
                    fill: fill.clone(),
                    ..TextRun::default()
                }],
                anchor: TextAnchor::Start,
                transform: IDENTITY,
                opacity: 1.0,
                stroke: Stroke::default(),
                clip_id: None,
                meta: SourceMeta {
                    kind: "office:text".into(),
                    source_id: shape.name.clone(),
                    semantic_role: "office:slide-text".into(),
                    ..SourceMeta::default()
                },
            });
        }
    }
    nodes
}

fn default_stroke() -> Stroke {
    Stroke {
        paint: Paint::solid("#1f2937"),
        width: 1.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        miter_limit: 4.0,
        ..Stroke::default()
    }
}

fn parse_odf_length(value: &str) -> Option<f64> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let (number, unit) = ["pt", "px", "in", "cm", "mm", "pc"]
        .into_iter()
        .find_map(|unit| lower.strip_suffix(unit).map(|number| (number.trim(), unit)))
        .unwrap_or((value, ""));
    let number = number.parse::<f64>().ok()?;
    let points = match unit {
        "pt" | "" => number,
        "px" => number * 0.75,
        "in" => number * 72.0,
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        "pc" => number * 12.0,
        _ => return None,
    };
    points.is_finite().then_some(points)
}

fn fmt(value: f64) -> String {
    let rounded = (value * 1000.0).round() / 1000.0;
    if rounded.fract().abs() < 1e-6 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Pages(Vec<Page>);

    impl PageConsumer for Pages {
        fn consume(&mut self, page: Page) -> Result<()> {
            self.0.push(page);
            Ok(())
        }
    }

    #[test]
    fn parses_flat_odp_slide_geometry_styles_and_text() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
 xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0"
 xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"
 xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0">
 <office:automatic-styles>
  <style:page-layout style:name="portrait"><style:page-layout-properties fo:page-width="21cm" fo:page-height="29.7cm"/></style:page-layout>
  <style:page-layout style:name="wide"><style:page-layout-properties fo:page-width="20cm" fo:page-height="11.25cm"/></style:page-layout>
  <style:style style:name="base-shape" style:family="graphic">
   <style:graphic-properties draw:fill="solid" draw:fill-color="#cccccc" draw:stroke="solid" svg:stroke-color="#0F172A" svg:stroke-width="1pt"/>
  </style:style>
  <style:style style:name="accent" style:family="graphic" style:parent-style-name="base-shape">
   <style:graphic-properties draw:fill="solid" draw:fill-color="#2563EB"/>
  </style:style>
  <style:style style:name="title-text" style:family="paragraph">
   <style:text-properties fo:font-size="14pt" fo:font-weight="bold" fo:color="#FFFFFF"/>
  </style:style>
 </office:automatic-styles>
 <office:master-styles><style:master-page style:name="wide-master" style:page-layout-name="wide"/></office:master-styles>
 <office:body><office:presentation>
  <draw:page draw:name="Opening" draw:master-page-name="wide-master">
   <draw:rect draw:name="title-box" draw:style-name="accent" draw:text-style-name="title-text" svg:x="1cm" svg:y="2cm" svg:width="10cm" svg:height="3cm">
    <text:p>Hello &amp; <text:span>world</text:span></text:p><text:p>Second line</text:p>
   </draw:rect>
   <draw:ellipse svg:x="12cm" svg:y="2cm" svg:width="3cm" svg:height="3cm"/>
   <presentation:notes><draw:frame draw:style-name="notes-style"><draw:text-box><text:p>Speaker notes omitted</text:p></draw:text-box></draw:frame></presentation:notes>
  </draw:page>
  <draw:page draw:name="Closing"><draw:line svg:x1="1cm" svg:y1="1cm" svg:x2="8cm" svg:y2="4cm"/></draw:page>
 </office:presentation></office:body>
</office:document>"##;
        let mut definitions = OdpDefinitions::default();
        let mut pages = Pages::default();
        let mut page_count = 0;
        let mut warnings = Vec::new();
        parse_odp_xml(
            xml,
            OdfVisualKind::Presentation,
            10_000,
            &mut definitions,
            Some((&mut page_count, 10, &mut pages)),
            &mut warnings,
        )
        .unwrap();

        assert_eq!(pages.0.len(), 2);
        assert!((pages.0[0].width - 20.0 * 72.0 / 2.54).abs() < 0.01);
        assert!((pages.0[0].height - 11.25 * 72.0 / 2.54).abs() < 0.01);
        assert_eq!(pages.0[0].title, "Opening");
        assert_eq!(pages.0[0].nodes.len(), 5);
        let Node::Path { fill, stroke, .. } = &pages.0[0].nodes[1] else {
            panic!("expected the rectangle vector path");
        };
        assert!(matches!(fill, Paint::Solid { color, .. } if color == "#2563EB"));
        assert!(matches!(&stroke.paint, Paint::Solid { color, .. } if color == "#0F172A"));
        let Node::Text { runs, .. } = &pages.0[0].nodes[2] else {
            panic!("expected editable slide text");
        };
        assert_eq!(runs[0].text, "Hello & world");
        assert!(runs[0].bold);
        assert_eq!(runs[0].font_size, 14.0);
        let Node::Text { runs, .. } = &pages.0[0].nodes[3] else {
            panic!("expected a second paragraph line");
        };
        assert_eq!(runs[0].text, "Second line");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("speaker notes and slide thumbnails are omitted"))
        );
    }

    #[test]
    fn parses_open_document_drawing_pages_from_office_drawing() {
        let xml = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:drawing><draw:page draw:name="Diagram"><draw:rect svg:x="1cm" svg:y="2cm" svg:width="5cm" svg:height="3cm"><text:p>Process</text:p></draw:rect></draw:page></office:drawing></office:body></office:document-content>"#;
        let mut definitions = OdpDefinitions::default();
        let mut pages = Pages::default();
        let mut page_count = 0;
        let mut warnings = Vec::new();
        parse_odp_xml(
            xml,
            OdfVisualKind::Drawing,
            10_000,
            &mut definitions,
            Some((&mut page_count, 10, &mut pages)),
            &mut warnings,
        )
        .unwrap();

        assert_eq!(pages.0.len(), 1);
        assert_eq!(pages.0[0].source_format, "odg");
        assert_eq!(pages.0[0].title, "Diagram");
        assert!(pages.0[0].nodes.iter().any(|node| matches!(
            node,
            Node::Text { runs, .. } if runs.first().is_some_and(|run| run.text == "Process")
        )));
    }

    #[test]
    fn rejects_odp_document_types_and_excessive_xml_events() {
        let xml = "<!DOCTYPE office:document><office:document/>";
        let mut definitions = OdpDefinitions::default();
        let mut warnings = Vec::new();
        assert!(matches!(
            parse_odp_xml(
                xml,
                OdfVisualKind::Presentation,
                100,
                &mut definitions,
                None,
                &mut warnings
            ),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            parse_odp_xml(
                "<office:document/>",
                OdfVisualKind::Presentation,
                1,
                &mut definitions,
                None,
                &mut warnings
            ),
            Err(Error::LimitExceeded(_))
        ));
    }
}
