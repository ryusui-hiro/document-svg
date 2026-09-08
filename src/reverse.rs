//! Package SVG pages as vector images in PPTX, DOCX, or XLSX files.
//!
//! This preserves the rendered SVG, not the source document's semantic structure.

use std::fmt::{Display, Formatter};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::Serialize;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::error::{Error, Result};
use crate::ooxml::{attribute, local_name};

const EMU_PER_POINT: f64 = 12_700.0;
const FALLBACK_DPI: f64 = 96.0;
const MAX_FALLBACK_DIMENSION: f64 = 4_096.0;
const MAX_FALLBACK_PIXELS: f64 = 16_777_216.0;
const MAX_NESTED_SVG_DEPTH: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenXmlFormat {
    Pptx,
    Docx,
    Xlsx,
}

impl OpenXmlFormat {
    fn detect(path: &Path) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| Error::InvalidInput("output has no file extension".into()))?;
        match extension.as_str() {
            "pptx" => Ok(Self::Pptx),
            "docx" => Ok(Self::Docx),
            "xlsx" => Ok(Self::Xlsx),
            _ => Err(Error::Unsupported(format!(
                "output extension .{extension}; expected PPTX, DOCX, or XLSX"
            ))),
        }
    }
}

impl Display for OpenXmlFormat {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Pptx => "PPTX",
            Self::Docx => "DOCX",
            Self::Xlsx => "XLSX",
        })
    }
}

#[derive(Clone, Debug)]
pub struct ReverseOptions {
    pub max_input_bytes: u64,
    pub max_pages: usize,
}

impl Default for ReverseOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: 512 * 1024 * 1024,
            max_pages: 10_000,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ReverseReport {
    pub converter: &'static str,
    pub version: &'static str,
    pub source: String,
    pub output: String,
    pub output_format: OpenXmlFormat,
    pub page_count: usize,
    pub input_bytes: u64,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct SvgPage {
    bytes: Vec<u8>,
    fallback_png: Vec<u8>,
    width_points: f64,
    height_points: f64,
}

pub fn svg_to_openxml(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    options: &ReverseOptions,
) -> Result<ReverseReport> {
    let input = input.as_ref();
    let output = output.as_ref();
    if output.exists() {
        return Err(Error::InvalidInput(format!(
            "output already exists: {}",
            output.display()
        )));
    }
    if options.max_pages == 0 {
        return Err(Error::InvalidInput("max_pages must be at least 1".into()));
    }
    let format = OpenXmlFormat::detect(output)?;
    let paths = collect_svg_paths(input, options.max_pages)?;
    let mut pages = Vec::with_capacity(paths.len());
    let mut input_bytes = 0u64;
    let mut render_options = None;
    for path in paths {
        let metadata = fs::metadata(&path)?;
        input_bytes = input_bytes.saturating_add(metadata.len());
        if input_bytes > options.max_input_bytes {
            return Err(Error::LimitExceeded(format!(
                "SVG input is {input_bytes} bytes; maximum is {} bytes",
                options.max_input_bytes
            )));
        }
        let bytes = fs::read(&path)?;
        validate_svg_document(&bytes, 0)?;
        let (width_points, height_points) = svg_dimensions(&bytes)?;
        let render_options = render_options.get_or_insert_with(|| {
            let mut options = resvg::usvg::Options::default();
            options.fontdb_mut().load_system_fonts();
            options
        });
        let fallback_png =
            render_svg_fallback(&bytes, width_points, height_points, render_options)?;
        pages.push(SvgPage {
            bytes,
            fallback_png,
            width_points,
            height_points,
        });
    }

    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let writer = BufWriter::new(temporary.as_file_mut());
    let mut package = Package::new(writer);
    match format {
        OpenXmlFormat::Pptx => write_pptx(&mut package, &pages)?,
        OpenXmlFormat::Docx => write_docx(&mut package, &pages)?,
        OpenXmlFormat::Xlsx => write_xlsx(&mut package, &pages)?,
    }
    package.finish()?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| error.error)?;

    Ok(ReverseReport {
        converter: "document-svg",
        version: env!("CARGO_PKG_VERSION"),
        source: input.to_string_lossy().into_owned(),
        output: output.to_string_lossy().into_owned(),
        output_format: format,
        page_count: pages.len(),
        input_bytes,
        warnings: vec![
            "SVG pages are embedded as vector images; original document semantics are not reconstructed"
                .into(),
        ],
    })
}

fn collect_svg_paths(input: &Path, max_pages: usize) -> Result<Vec<PathBuf>> {
    let metadata = fs::metadata(input)?;
    let mut paths = if metadata.is_file() {
        if !has_svg_extension(input) {
            return Err(Error::Unsupported(format!(
                "input {}; expected an SVG file or directory",
                input.display()
            )));
        }
        vec![input.to_path_buf()]
    } else if metadata.is_dir() {
        let mut entries = Vec::new();
        for entry in fs::read_dir(input)? {
            let path = entry?.path();
            if path.is_file() && has_svg_extension(&path) {
                entries.push(path);
            }
        }
        entries.sort();
        entries
    } else {
        return Err(Error::InvalidInput(format!(
            "{} is not a regular file or directory",
            input.display()
        )));
    };
    if paths.is_empty() {
        return Err(Error::InvalidInput(format!(
            "{} contains no SVG files",
            input.display()
        )));
    }
    if paths.len() > max_pages {
        return Err(Error::LimitExceeded(format!(
            "SVG input has {} pages; maximum is {max_pages}",
            paths.len()
        )));
    }
    paths.shrink_to_fit();
    Ok(paths)
}

fn has_svg_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("svg"))
}

fn svg_dimensions(bytes: &[u8]) -> Result<(f64, f64)> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) | Event::Empty(start)
                if local_name(start.name().as_ref()) == b"svg" =>
            {
                let width = attribute(&start, b"width").and_then(|value| parse_svg_length(&value));
                let height =
                    attribute(&start, b"height").and_then(|value| parse_svg_length(&value));
                let view_box = attribute(&start, b"viewBox").and_then(|value| {
                    let tokens = value
                        .split(|character: char| {
                            character.is_ascii_whitespace() || character == ','
                        })
                        .filter(|value| !value.is_empty())
                        .collect::<Vec<_>>();
                    if tokens.len() != 4 {
                        return None;
                    }
                    let values = tokens
                        .iter()
                        .map(|value| value.parse::<f64>())
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .ok()?;
                    (values.iter().all(|value| value.is_finite())
                        && values[2] > 0.0
                        && values[3] > 0.0)
                        .then_some((values[2], values[3]))
                });
                let width = width.or_else(|| view_box.map(|value| value.0 * 0.75));
                let height = height.or_else(|| view_box.map(|value| value.1 * 0.75));
                let (width, height) = match (width, height) {
                    (Some(width), Some(height)) if width > 0.0 && height > 0.0 => (width, height),
                    _ => {
                        return Err(Error::InvalidInput(
                            "SVG root needs positive width/height or viewBox dimensions".into(),
                        ));
                    }
                };
                return Ok((width, height));
            }
            Event::Eof => {
                return Err(Error::InvalidInput(
                    "input does not contain an SVG root".into(),
                ));
            }
            _ => {}
        }
        buffer.clear();
    }
}

fn parse_svg_length(value: &str) -> Option<f64> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let (number, unit) = ["pt", "px", "in", "cm", "mm", "pc"]
        .into_iter()
        .find_map(|unit| lower.strip_suffix(unit).map(|number| (number.trim(), unit)))
        .unwrap_or((value, ""));
    let number = number.parse::<f64>().ok()?;
    let points = match unit {
        "pt" => number,
        "px" | "" => number * 0.75,
        "in" => number * 72.0,
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        "pc" => number * 12.0,
        _ => unreachable!(),
    };
    points.is_finite().then_some(points)
}

fn validate_svg_document(bytes: &[u8], depth: usize) -> Result<()> {
    if depth > MAX_NESTED_SVG_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "nested SVG depth exceeds {MAX_NESTED_SVG_DEPTH}"
        )));
    }
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut saw_root = false;
    let mut root_closed = false;
    let mut element_depth = 0usize;
    let mut style_depth = None::<usize>;
    loop {
        match reader.read_event_into(&mut buffer)? {
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "SVG document types and entities are not allowed".into(),
                ));
            }
            Event::PI(_) => {
                return Err(Error::InvalidInput(
                    "SVG processing instructions are not allowed".into(),
                ));
            }
            Event::GeneralRef(_) if style_depth.is_some() => {
                return Err(Error::InvalidInput(
                    "SVG CSS entity references are not allowed".into(),
                ));
            }
            Event::Start(start) => {
                if root_closed {
                    return Err(Error::InvalidInput(
                        "input contains content after the SVG root".into(),
                    ));
                }
                if !saw_root {
                    if local_name(start.name().as_ref()) != b"svg" {
                        return Err(Error::InvalidInput(
                            "input must have an <svg> root element".into(),
                        ));
                    }
                    validate_svg_root_namespace(&start)?;
                    saw_root = true;
                }
                validate_svg_element(&start, reader.decoder(), depth)?;
                if local_name(start.name().as_ref()).eq_ignore_ascii_case(b"style") {
                    style_depth = Some(element_depth + 1);
                }
                element_depth += 1;
            }
            Event::Empty(start) => {
                if root_closed {
                    return Err(Error::InvalidInput(
                        "input contains content after the SVG root".into(),
                    ));
                }
                if !saw_root {
                    if local_name(start.name().as_ref()) != b"svg" {
                        return Err(Error::InvalidInput(
                            "input must have an <svg> root element".into(),
                        ));
                    }
                    validate_svg_root_namespace(&start)?;
                    saw_root = true;
                    root_closed = true;
                }
                validate_svg_element(&start, reader.decoder(), depth)?;
            }
            Event::Text(text) => {
                let value = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid SVG text encoding: {error}"))
                })?;
                if (!saw_root || root_closed) && !value.trim().is_empty() {
                    return Err(Error::InvalidInput(
                        "input contains text outside the SVG root".into(),
                    ));
                }
                if style_depth.is_some() {
                    validate_css_references(&value)?;
                }
            }
            Event::CData(text) => {
                if !saw_root || root_closed {
                    return Err(Error::InvalidInput(
                        "input contains CDATA outside the SVG root".into(),
                    ));
                }
                if style_depth.is_some() {
                    let value = String::from_utf8_lossy(text.as_ref());
                    validate_css_references(&value)?;
                }
            }
            Event::End(end) => {
                element_depth = element_depth.saturating_sub(1);
                if element_depth == 0 {
                    root_closed = true;
                }
                if local_name(end.name().as_ref()).eq_ignore_ascii_case(b"style") {
                    style_depth = None;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !saw_root || !root_closed {
        return Err(Error::InvalidInput(
            "input does not contain an SVG root".into(),
        ));
    }
    Ok(())
}

fn validate_svg_root_namespace(start: &quick_xml::events::BytesStart<'_>) -> Result<()> {
    let raw_name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let namespace_key = raw_name.split_once(':').map_or_else(
        || "xmlns".to_owned(),
        |(prefix, _)| format!("xmlns:{prefix}"),
    );
    let mut namespace = None::<String>;
    for item in start.attributes().with_checks(true) {
        let item = item
            .map_err(|error| Error::InvalidInput(format!("invalid SVG root attribute: {error}")))?;
        if item.key.as_ref() == namespace_key.as_bytes() {
            namespace = Some(String::from_utf8_lossy(item.value.as_ref()).into_owned());
        }
    }
    if raw_name.contains(':') && namespace.as_deref() != Some("http://www.w3.org/2000/svg") {
        return Err(Error::InvalidInput(
            "prefixed SVG root must use the SVG namespace".into(),
        ));
    }
    if namespace
        .as_deref()
        .is_some_and(|value| value != "http://www.w3.org/2000/svg")
    {
        return Err(Error::InvalidInput(
            "SVG root uses an unsupported namespace".into(),
        ));
    }
    Ok(())
}

fn validate_svg_element(
    start: &quick_xml::events::BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
    depth: usize,
) -> Result<()> {
    let name = String::from_utf8_lossy(local_name(start.name().as_ref())).to_ascii_lowercase();
    if matches!(
        name.as_str(),
        "script"
            | "foreignobject"
            | "iframe"
            | "object"
            | "embed"
            | "animate"
            | "animatetransform"
            | "animatemotion"
            | "discard"
            | "set"
    ) {
        return Err(Error::InvalidInput(format!(
            "active SVG element <{name}> is not allowed"
        )));
    }
    for item in start.attributes().with_checks(true) {
        let item =
            item.map_err(|error| Error::InvalidInput(format!("invalid SVG attribute: {error}")))?;
        let key = String::from_utf8_lossy(local_name(item.key.as_ref())).to_ascii_lowercase();
        let value = item
            .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)?
            .into_owned();
        if key.starts_with("on") || key == "base" {
            return Err(Error::InvalidInput(format!(
                "SVG event attribute {key} is not allowed"
            )));
        }
        if key == "href" {
            validate_svg_href(&value, depth)?;
        }
        validate_css_references(&value)?;
    }
    Ok(())
}

fn validate_svg_href(value: &str, depth: usize) -> Result<()> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('#') {
        return Ok(());
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("data:image/png;")
        || lower.starts_with("data:image/jpeg;")
        || lower.starts_with("data:image/gif;")
        || lower.starts_with("data:image/webp;")
    {
        return Ok(());
    }
    if lower.starts_with("data:image/svg+xml;base64,") {
        let encoded = value
            .split_once(',')
            .map(|(_, data)| data)
            .unwrap_or_default();
        let compact = encoded
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect::<Vec<_>>();
        let nested = base64::engine::general_purpose::STANDARD
            .decode(compact)
            .map_err(|error| {
                Error::InvalidInput(format!("invalid nested SVG data URI: {error}"))
            })?;
        return validate_svg_document(&nested, depth + 1);
    }
    Err(Error::InvalidInput(format!(
        "external or active SVG reference is not allowed: {}",
        value.chars().take(80).collect::<String>()
    )))
}

fn validate_css_references(value: &str) -> Result<()> {
    let lower = value.to_ascii_lowercase();
    if lower.contains('\\') || lower.contains("/*") {
        return Err(Error::InvalidInput(
            "SVG CSS escapes and comments are not allowed".into(),
        ));
    }
    if lower.contains("@import") || lower.contains("javascript:") {
        return Err(Error::InvalidInput(
            "external or active SVG CSS is not allowed".into(),
        ));
    }
    let mut remainder = value;
    while let Some(index) = remainder.to_ascii_lowercase().find("url(") {
        let after = &remainder[index + 4..];
        let Some(end) = after.find(')') else {
            return Err(Error::InvalidInput("unterminated SVG CSS url()".into()));
        };
        let target = after[..end]
            .trim()
            .trim_matches(|character| matches!(character, '\'' | '"'));
        if !target.starts_with('#') {
            return Err(Error::InvalidInput(
                "external SVG CSS url() reference is not allowed".into(),
            ));
        }
        remainder = &after[end + 1..];
    }
    Ok(())
}

fn render_svg_fallback(
    bytes: &[u8],
    width_points: f64,
    height_points: f64,
    options: &resvg::usvg::Options<'_>,
) -> Result<Vec<u8>> {
    let tree = resvg::usvg::Tree::from_data(bytes, options)
        .map_err(|error| Error::InvalidInput(format!("SVG fallback parse failed: {error}")))?;
    let (pixel_width, pixel_height) = fallback_pixel_size(width_points, height_points);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixel_width, pixel_height).ok_or_else(|| {
        Error::LimitExceeded(format!(
            "SVG fallback raster allocation failed for {pixel_width}x{pixel_height} pixels"
        ))
    })?;
    let source = tree.size();
    let transform = resvg::tiny_skia::Transform::from_scale(
        pixel_width as f32 / source.width(),
        pixel_height as f32 / source.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map_err(|error| Error::InvalidInput(format!("SVG fallback PNG encoding failed: {error}")))
}

fn fallback_pixel_size(width_points: f64, height_points: f64) -> (u32, u32) {
    let mut width = (width_points * FALLBACK_DPI / 72.0).max(1.0);
    let mut height = (height_points * FALLBACK_DPI / 72.0).max(1.0);
    let scale = (MAX_FALLBACK_DIMENSION / width)
        .min(MAX_FALLBACK_DIMENSION / height)
        .min((MAX_FALLBACK_PIXELS / (width * height)).sqrt())
        .min(1.0);
    width *= scale;
    height *= scale;
    (
        width.round().max(1.0) as u32,
        height.round().max(1.0) as u32,
    )
}

struct Package<W: Write + std::io::Seek> {
    zip: ZipWriter<W>,
    options: SimpleFileOptions,
}

impl<W: Write + std::io::Seek> Package<W> {
    fn new(writer: W) -> Self {
        Self {
            zip: ZipWriter::new(writer),
            options: SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644),
        }
    }

    fn part(&mut self, name: &str, bytes: impl AsRef<[u8]>) -> Result<()> {
        self.zip.start_file(name, self.options)?;
        self.zip.write_all(bytes.as_ref())?;
        Ok(())
    }

    fn finish(self) -> Result<()> {
        self.zip.finish()?;
        Ok(())
    }
}

fn root_relationships(target: &str, relationship_type: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{relationship_type}" Target="{target}"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#
    )
}

fn property_content_type_overrides() -> &'static str {
    r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#
}

fn write_common_properties<W: Write + std::io::Seek>(package: &mut Package<W>) -> Result<()> {
    package.part(
        "docProps/core.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Document SVG</dc:title><dc:creator>document-svg</dc:creator><cp:lastModifiedBy>document-svg</cp:lastModifiedBy></cp:coreProperties>"#,
    )?;
    package.part(
        "docProps/app.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>document-svg</Application><AppVersion>0.1</AppVersion></Properties>"#,
    )?;
    Ok(())
}

fn write_pptx<W: Write + std::io::Seek>(package: &mut Package<W>, pages: &[SvgPage]) -> Result<()> {
    let mut overrides = String::new();
    let mut slide_ids = String::new();
    let mut presentation_rels = String::new();
    for (index, _) in pages.iter().enumerate() {
        let number = index + 1;
        overrides.push_str(&format!(r#"<Override PartName="/ppt/slides/slide{number}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#));
        slide_ids.push_str(&format!(
            r#"<p:sldId id="{}" r:id="rId{}"/>"#,
            256 + index,
            number + 1
        ));
        presentation_rels.push_str(&format!(r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{number}.xml"/>"#, number + 1));
    }
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="svg" ContentType="image/svg+xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/presProps.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presProps+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/>{property_overrides}{overrides}</Types>"#,
        property_overrides = property_content_type_overrides(),
    );
    package.part("[Content_Types].xml", content_types)?;
    package.part(
        "_rels/.rels",
        root_relationships("ppt/presentation.xml", "officeDocument"),
    )?;
    write_common_properties(package)?;
    let first = &pages[0];
    let slide_width = points_to_emu(first.width_points);
    let slide_height = points_to_emu(first.height_points);
    let presentation = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst><p:sldIdLst>{slide_ids}</p:sldIdLst><p:sldSz cx="{slide_width}" cy="{slide_height}" type="custom"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
    );
    package.part("ppt/presentation.xml", presentation)?;
    let presentation_relationships = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/>{presentation_rels}<Relationship Id="rId{pres_props_id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/presProps" Target="presProps.xml"/></Relationships>"#,
        pres_props_id = pages.len() + 2,
    );
    package.part(
        "ppt/_rels/presentation.xml.rels",
        presentation_relationships,
    )?;
    package.part("ppt/slideMasters/slideMaster1.xml", pptx_slide_master())?;
    package.part("ppt/slideMasters/_rels/slideMaster1.xml.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#)?;
    package.part("ppt/slideLayouts/slideLayout1.xml", pptx_slide_layout())?;
    package.part("ppt/slideLayouts/_rels/slideLayout1.xml.rels", r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#)?;
    package.part("ppt/theme/theme1.xml", pptx_theme())?;
    package.part(
        "ppt/presProps.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentationPr xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:showPr useTimings="0"/></p:presentationPr>"#,
    )?;
    for (index, page) in pages.iter().enumerate() {
        let number = index + 1;
        let (x, y, cx, cy) = fit_rect(
            page.width_points,
            page.height_points,
            first.width_points,
            first.height_points,
        );
        let slide = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:asvg="http://schemas.microsoft.com/office/drawing/2016/SVG/main"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr><mc:AlternateContent><mc:Choice Requires="asvg">{choice}</mc:Choice><mc:Fallback>{fallback}</mc:Fallback></mc:AlternateContent></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>"#,
            choice = pptx_svg_picture(number, x, y, cx, cy, true),
            fallback = pptx_svg_picture(number, x, y, cx, cy, false),
        );
        package.part(&format!("ppt/slides/slide{number}.xml"), slide)?;
        let relationships = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image{number}.svg"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image{number}-fallback.png"/></Relationships>"#
        );
        package.part(
            &format!("ppt/slides/_rels/slide{number}.xml.rels"),
            relationships,
        )?;
        package.part(&format!("ppt/media/image{number}.svg"), &page.bytes)?;
        package.part(
            &format!("ppt/media/image{number}-fallback.png"),
            &page.fallback_png,
        )?;
    }
    Ok(())
}

fn pptx_svg_picture(number: usize, x: i64, y: i64, cx: i64, cy: i64, svg: bool) -> String {
    let extension = if svg {
        r#"<a:extLst><a:ext uri="{96DAC541-7B7A-43D3-8B79-37D633B846F1}"><asvg:svgBlip r:embed="rId2"/></a:ext></a:extLst>"#
    } else {
        ""
    };
    format!(
        r#"<p:pic><p:nvPicPr><p:cNvPr id="2" name="SVG page {number}"/><p:cNvPicPr><a:picLocks noChangeAspect="1"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId3">{extension}</a:blip><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></p:spPr></p:pic>"#
    )
}

fn pptx_slide_master() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMap accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" bg1="lt1" bg2="lt2" folHlink="folHlink" hlink="hlink" tx1="dk1" tx2="dk2"/><p:sldLayoutIdLst><p:sldLayoutId id="1" r:id="rId1"/></p:sldLayoutIdLst></p:sldMaster>"#
}

fn pptx_slide_layout() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" type="blank" preserve="1"><p:cSld name="Blank"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#
}

fn pptx_theme() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Document SVG"><a:themeElements><a:clrScheme name="Document SVG"><a:dk1><a:srgbClr val="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="1F1F1F"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="Document SVG"><a:majorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Document SVG"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w="9525"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="25400"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="38100"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>"#
}

fn write_docx<W: Write + std::io::Seek>(package: &mut Package<W>, pages: &[SvgPage]) -> Result<()> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="svg" ContentType="image/svg+xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    package.part("[Content_Types].xml", content_types)?;
    package.part(
        "_rels/.rels",
        root_relationships("word/document.xml", "officeDocument"),
    )?;
    write_common_properties(package)?;
    let mut body = String::new();
    let mut relationships = String::new();
    for (index, page) in pages.iter().enumerate() {
        let number = index + 1;
        let cx = points_to_emu(page.width_points);
        let cy = points_to_emu(page.height_points);
        body.push_str("<w:p><w:pPr><w:spacing w:before=\"0\" w:after=\"0\"/>");
        if number < pages.len() {
            body.push_str(&docx_section_properties(page, true));
        }
        body.push_str("</w:pPr>");
        let png_rel = number * 2 - 1;
        let svg_rel = number * 2;
        body.push_str(&format!(r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="{number}" name="SVG page {number}"/><wp:cNvGraphicFramePr/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="{number}" name="SVG page {number}"/><pic:cNvPicPr><a:picLocks noChangeAspect="1"/></pic:cNvPicPr></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId{png_rel}"><a:extLst><a:ext uri="{{96DAC541-7B7A-43D3-8B79-37D633B846F1}}"><asvg:svgBlip r:embed="rId{svg_rel}"/></a:ext></a:extLst></a:blip><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#));
        relationships.push_str(&format!(r#"<Relationship Id="rId{png_rel}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image{number}-fallback.png"/><Relationship Id="rId{svg_rel}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image{number}.svg"/>"#));
        package.part(&format!("word/media/image{number}.svg"), &page.bytes)?;
        package.part(
            &format!("word/media/image{number}-fallback.png"),
            &page.fallback_png,
        )?;
    }
    let final_section = docx_section_properties(&pages[pages.len() - 1], false);
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:asvg="http://schemas.microsoft.com/office/drawing/2016/SVG/main"><w:body>{body}{final_section}</w:body></w:document>"#
    );
    package.part("word/document.xml", document)?;
    package.part("word/_rels/document.xml.rels", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{relationships}</Relationships>"#))?;
    Ok(())
}

fn docx_section_properties(page: &SvgPage, section_break: bool) -> String {
    let page_width = points_to_twips(page.width_points);
    let page_height = points_to_twips(page.height_points);
    let orientation = if page.width_points > page.height_points {
        r#" w:orient="landscape""#
    } else {
        ""
    };
    let section_type = if section_break {
        r#"<w:type w:val="nextPage"/>"#
    } else {
        ""
    };
    format!(
        r#"<w:sectPr>{section_type}<w:pgSz w:w="{page_width}" w:h="{page_height}"{orientation}/><w:pgMar w:top="0" w:right="0" w:bottom="0" w:left="0" w:header="0" w:footer="0" w:gutter="0"/></w:sectPr>"#
    )
}

fn write_xlsx<W: Write + std::io::Seek>(package: &mut Package<W>, pages: &[SvgPage]) -> Result<()> {
    let mut overrides = String::new();
    let mut sheets = String::new();
    let mut workbook_rels = String::new();
    for (index, _) in pages.iter().enumerate() {
        let number = index + 1;
        overrides.push_str(&format!(r#"<Override PartName="/xl/worksheets/sheet{number}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/drawings/drawing{number}.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml"/>"#));
        sheets.push_str(&format!(
            r#"<sheet name="Page {number}" sheetId="{number}" r:id="rId{number}"/>"#
        ));
        workbook_rels.push_str(&format!(r#"<Relationship Id="rId{number}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{number}.xml"/>"#));
    }
    package.part("[Content_Types].xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="svg" ContentType="image/svg+xml"/><Default Extension="png" ContentType="image/png"/>{property_overrides}<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>{overrides}</Types>"#,
        property_overrides = property_content_type_overrides(),
    ))?;
    package.part(
        "_rels/.rels",
        root_relationships("xl/workbook.xml", "officeDocument"),
    )?;
    write_common_properties(package)?;
    package.part("xl/workbook.xml", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>{sheets}</sheets></workbook>"#))?;
    package.part("xl/_rels/workbook.xml.rels", format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{workbook_rels}</Relationships>"#))?;
    for (index, page) in pages.iter().enumerate() {
        let number = index + 1;
        let orientation = if page.width_points > page.height_points {
            "landscape"
        } else {
            "portrait"
        };
        package.part(&format!("xl/worksheets/sheet{number}.xml"), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetPr><pageSetUpPr fitToPage="1"/></sheetPr><dimension ref="A1"/><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetFormatPr defaultRowHeight="15"/><sheetData/><pageMargins left="0.25" right="0.25" top="0.25" bottom="0.25" header="0" footer="0"/><pageSetup paperSize="9" orientation="{orientation}" fitToWidth="1" fitToHeight="1"/><drawing r:id="rId1"/></worksheet>"#))?;
        package.part(&format!("xl/worksheets/_rels/sheet{number}.xml.rels"), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing{number}.xml"/></Relationships>"#))?;
        let cx = points_to_emu(page.width_points);
        let cy = points_to_emu(page.height_points);
        package.part(&format!("xl/drawings/drawing{number}.xml"), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:asvg="http://schemas.microsoft.com/office/drawing/2016/SVG/main"><xdr:absoluteAnchor><xdr:pos x="0" y="0"/><xdr:ext cx="{cx}" cy="{cy}"/><xdr:pic><xdr:nvPicPr><xdr:cNvPr id="1" name="SVG page {number}"/><xdr:cNvPicPr><a:picLocks noChangeAspect="1"/></xdr:cNvPicPr></xdr:nvPicPr><xdr:blipFill><a:blip r:embed="rId1"><a:extLst><a:ext uri="{{96DAC541-7B7A-43D3-8B79-37D633B846F1}}"><asvg:svgBlip r:embed="rId2"/></a:ext></a:extLst></a:blip><a:stretch><a:fillRect/></a:stretch></xdr:blipFill><xdr:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></xdr:spPr></xdr:pic><xdr:clientData/></xdr:absoluteAnchor></xdr:wsDr>"#))?;
        package.part(&format!("xl/drawings/_rels/drawing{number}.xml.rels"), format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image{number}-fallback.png"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image{number}.svg"/></Relationships>"#))?;
        package.part(&format!("xl/media/image{number}.svg"), &page.bytes)?;
        package.part(
            &format!("xl/media/image{number}-fallback.png"),
            &page.fallback_png,
        )?;
    }
    Ok(())
}

fn points_to_emu(points: f64) -> i64 {
    (points * EMU_PER_POINT).round().clamp(1.0, i64::MAX as f64) as i64
}

fn points_to_twips(points: f64) -> i64 {
    (points * 20.0).round().clamp(1.0, 31_680.0) as i64
}

fn fit_rect(
    width: f64,
    height: f64,
    container_width: f64,
    container_height: f64,
) -> (i64, i64, i64, i64) {
    let scale = (container_width / width).min(container_height / height);
    let width = width * scale;
    let height = height * scale;
    let x = (container_width - width) / 2.0;
    let y = (container_height - height) / 2.0;
    (
        points_to_emu(x),
        points_to_emu(y),
        points_to_emu(width),
        points_to_emu(height),
    )
}
