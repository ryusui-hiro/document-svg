use std::fs;

use document_svg::{
    ConversionReport as CoreConversionReport, ConvertOptions as CoreConvertOptions,
    PageReport as CorePageReport, ReverseOptions as CoreReverseOptions,
    ReverseReport as CoreReverseReport,
};
use napi::bindgen_prelude::{AsyncTask, Result, Task};
use napi::{Env, Error, Status};
use napi_derive::napi;

#[napi(object)]
pub struct ConvertOptions {
    pub max_input_bytes: Option<f64>,
    pub max_zip_entry_bytes: Option<f64>,
    pub max_pages: Option<f64>,
    pub max_xml_events: Option<f64>,
    pub include_metadata: Option<bool>,
    pub precision: Option<f64>,
    pub jobs: Option<f64>,
    pub outline_embedded_pdf_text: Option<bool>,
}

#[napi(object)]
pub struct ReverseOptions {
    pub max_input_bytes: Option<f64>,
    pub max_pages: Option<f64>,
}

/// Options for an in-memory SVG preview.
///
/// Conversion limits match `ConvertOptions`. The two preview limits bound how
/// much generated SVG is retained in the Node.js process at once.
#[napi(object)]
pub struct PreviewOptions {
    pub max_input_bytes: Option<f64>,
    pub max_zip_entry_bytes: Option<f64>,
    pub max_pages: Option<f64>,
    pub max_xml_events: Option<f64>,
    pub include_metadata: Option<bool>,
    pub precision: Option<f64>,
    pub jobs: Option<f64>,
    pub outline_embedded_pdf_text: Option<bool>,
    /// Maximum UTF-8 byte length of one returned SVG. Default: 64 MiB.
    pub max_svg_bytes: Option<f64>,
    /// Maximum UTF-8 byte length of all returned SVG pages. Default: 256 MiB.
    pub max_total_svg_bytes: Option<f64>,
}

#[napi(object, object_from_js = false)]
pub struct PageReport {
    pub number: f64,
    pub svg: String,
    pub width_points: f64,
    pub height_points: f64,
    pub node_count: f64,
    pub warning_count: f64,
    pub warnings: Vec<String>,
    pub estimated_ir_bytes: f64,
}

#[napi(object, object_from_js = false)]
pub struct ConversionReport {
    pub converter: String,
    pub version: String,
    pub source: String,
    pub source_format: String,
    pub output_directory: String,
    pub elapsed_ms: f64,
    pub input_bytes: f64,
    pub page_count: f64,
    pub largest_page_ir_bytes: f64,
    pub pages: Vec<PageReport>,
    pub warnings: Vec<String>,
}

#[napi(object, object_from_js = false)]
pub struct ReverseReport {
    pub converter: String,
    pub version: String,
    pub source: String,
    pub output: String,
    pub output_format: String,
    pub page_count: f64,
    pub input_bytes: f64,
    pub warnings: Vec<String>,
}

#[napi(object, object_from_js = false)]
pub struct PreviewPage {
    pub number: f64,
    /// Complete UTF-8 SVG markup, ready to put in a Blob for display.
    pub svg: String,
    pub width_points: f64,
    pub height_points: f64,
    pub node_count: f64,
    pub warning_count: f64,
    pub warnings: Vec<String>,
    pub estimated_ir_bytes: f64,
}

#[napi(object, object_from_js = false)]
pub struct PreviewReport {
    pub converter: String,
    pub version: String,
    pub source: String,
    pub source_format: String,
    pub elapsed_ms: f64,
    pub input_bytes: f64,
    pub page_count: f64,
    pub largest_page_ir_bytes: f64,
    pub pages: Vec<PreviewPage>,
    pub warnings: Vec<String>,
    /// True when the document or any page has a conversion warning.
    pub needs_review: bool,
}

pub struct CorePreviewReport {
    report: CoreConversionReport,
    svg_pages: Vec<String>,
}

struct PreviewConfig {
    convert: CoreConvertOptions,
    max_svg_bytes: u64,
    max_total_svg_bytes: u64,
}

const DEFAULT_MAX_SVG_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_SVG_BYTES: u64 = 256 * 1024 * 1024;

pub struct ConvertTask {
    input_path: String,
    output_directory: String,
    options: CoreConvertOptions,
}

pub struct PreviewTask {
    input_path: String,
    config: PreviewConfig,
}

pub struct ReverseTask {
    input_path: String,
    output_path: String,
    options: CoreReverseOptions,
}

#[napi]
impl Task for ConvertTask {
    type Output = CoreConversionReport;
    type JsValue = ConversionReport;

    fn compute(&mut self) -> Result<Self::Output> {
        document_svg::convert_path(&self.input_path, &self.output_directory, &self.options)
            .map_err(|error| Error::new(Status::GenericFailure, error.to_string()))
    }

    fn resolve(&mut self, _env: Env, report: Self::Output) -> Result<Self::JsValue> {
        Ok(report.into())
    }
}

#[napi]
impl Task for PreviewTask {
    type Output = CorePreviewReport;
    type JsValue = PreviewReport;

    fn compute(&mut self) -> Result<Self::Output> {
        let temporary = tempfile::Builder::new()
            .prefix("document-svg-preview-")
            .tempdir()
            .map_err(io_error)?;
        let report =
            document_svg::convert_path(&self.input_path, temporary.path(), &self.config.convert)
                .map_err(document_error)?;

        let mut total_svg_bytes = 0_u64;
        let mut svg_pages = Vec::with_capacity(report.pages.len());
        for page in &report.pages {
            let path = temporary.path().join(&page.svg);
            let byte_length = fs::metadata(&path).map_err(io_error)?.len();
            if byte_length > self.config.max_svg_bytes {
                return Err(Error::new(
                    Status::GenericFailure,
                    format!(
                        "preview page {} is {byte_length} bytes; maximum is {} bytes",
                        page.number, self.config.max_svg_bytes
                    ),
                ));
            }
            total_svg_bytes = total_svg_bytes.checked_add(byte_length).ok_or_else(|| {
                Error::new(Status::GenericFailure, "preview SVG byte count overflow")
            })?;
            if total_svg_bytes > self.config.max_total_svg_bytes {
                return Err(Error::new(
                    Status::GenericFailure,
                    format!(
                        "preview SVG output is {total_svg_bytes} bytes; maximum is {} bytes",
                        self.config.max_total_svg_bytes
                    ),
                ));
            }
            svg_pages.push(fs::read_to_string(path).map_err(io_error)?);
        }

        Ok(CorePreviewReport { report, svg_pages })
    }

    fn resolve(&mut self, _env: Env, preview: Self::Output) -> Result<Self::JsValue> {
        Ok(preview.into())
    }
}

#[napi]
impl Task for ReverseTask {
    type Output = CoreReverseReport;
    type JsValue = ReverseReport;

    fn compute(&mut self) -> Result<Self::Output> {
        document_svg::svg_to_openxml(&self.input_path, &self.output_path, &self.options)
            .map_err(document_error)
    }

    fn resolve(&mut self, _env: Env, report: Self::Output) -> Result<Self::JsValue> {
        Ok(report.into())
    }
}

#[napi]
pub fn convert(
    input_path: String,
    output_directory: String,
    options: Option<ConvertOptions>,
) -> Result<AsyncTask<ConvertTask>> {
    let options = options.map_or_else(
        || Ok(CoreConvertOptions::default()),
        CoreConvertOptions::try_from,
    )?;
    Ok(AsyncTask::new(ConvertTask {
        input_path,
        output_directory,
        options,
    }))
}

/// Package one SVG or a directory of SVG pages as PPTX, DOCX, or XLSX.
///
/// SVG pages remain vector images; Office semantic structure is not reconstructed.
#[napi]
pub fn reverse(
    input_path: String,
    output_path: String,
    options: Option<ReverseOptions>,
) -> Result<AsyncTask<ReverseTask>> {
    let options = options.map_or_else(
        || Ok(CoreReverseOptions::default()),
        CoreReverseOptions::try_from,
    )?;
    Ok(AsyncTask::new(ReverseTask {
        input_path,
        output_path,
        options,
    }))
}

/// Convert a PDF/PPTX/XLSX/DOCX file and return complete SVG markup for UI
/// preview without leaving output files behind.
///
/// This runs on a libuv worker. It is intended for Node.js, Electron main
/// processes, and server-side TypeScript; it is not a browser-WASM API.
#[napi]
pub fn preview(
    input_path: String,
    options: Option<PreviewOptions>,
) -> Result<AsyncTask<PreviewTask>> {
    let config = options.map_or_else(
        || {
            Ok(PreviewConfig {
                convert: CoreConvertOptions::default(),
                max_svg_bytes: DEFAULT_MAX_SVG_BYTES,
                max_total_svg_bytes: DEFAULT_MAX_TOTAL_SVG_BYTES,
            })
        },
        PreviewConfig::try_from,
    )?;
    Ok(AsyncTask::new(PreviewTask { input_path, config }))
}

impl TryFrom<ConvertOptions> for CoreConvertOptions {
    type Error = Error;

    fn try_from(value: ConvertOptions) -> Result<Self> {
        let mut options = Self::default();
        if let Some(number) = value.max_input_bytes {
            options.max_input_bytes = number_to_u64("maxInputBytes", number)?;
        }
        if let Some(number) = value.max_zip_entry_bytes {
            options.max_zip_entry_bytes = number_to_u64("maxZipEntryBytes", number)?;
        }
        if let Some(number) = value.max_pages {
            options.max_pages = number_to_usize("maxPages", number)?;
        }
        if let Some(number) = value.max_xml_events {
            options.max_xml_events = number_to_usize("maxXmlEvents", number)?;
        }
        if let Some(include_metadata) = value.include_metadata {
            options.include_metadata = include_metadata;
        }
        if let Some(number) = value.precision {
            options.precision = number_to_usize("precision", number)?;
        }
        if let Some(number) = value.jobs {
            options.jobs = number_to_usize("jobs", number)?;
        }
        if let Some(outline) = value.outline_embedded_pdf_text {
            options.outline_embedded_pdf_text = outline;
        }
        Ok(options)
    }
}

impl TryFrom<ReverseOptions> for CoreReverseOptions {
    type Error = Error;

    fn try_from(value: ReverseOptions) -> Result<Self> {
        let mut options = Self::default();
        if let Some(number) = value.max_input_bytes {
            options.max_input_bytes = number_to_u64("maxInputBytes", number)?;
        }
        if let Some(number) = value.max_pages {
            options.max_pages = number_to_usize("maxPages", number)?;
        }
        Ok(options)
    }
}

impl TryFrom<PreviewOptions> for PreviewConfig {
    type Error = Error;

    fn try_from(value: PreviewOptions) -> Result<Self> {
        let mut convert = CoreConvertOptions::default();
        if let Some(number) = value.max_input_bytes {
            convert.max_input_bytes = number_to_u64("maxInputBytes", number)?;
        }
        if let Some(number) = value.max_zip_entry_bytes {
            convert.max_zip_entry_bytes = number_to_u64("maxZipEntryBytes", number)?;
        }
        if let Some(number) = value.max_pages {
            convert.max_pages = number_to_usize("maxPages", number)?;
        }
        if let Some(number) = value.max_xml_events {
            convert.max_xml_events = number_to_usize("maxXmlEvents", number)?;
        }
        if let Some(include_metadata) = value.include_metadata {
            convert.include_metadata = include_metadata;
        }
        if let Some(number) = value.precision {
            convert.precision = number_to_usize("precision", number)?;
        }
        if let Some(number) = value.jobs {
            convert.jobs = number_to_usize("jobs", number)?;
        }
        if let Some(outline) = value.outline_embedded_pdf_text {
            convert.outline_embedded_pdf_text = outline;
        }
        let max_svg_bytes = value
            .max_svg_bytes
            .map_or(Ok(DEFAULT_MAX_SVG_BYTES), |number| {
                number_to_u64("maxSvgBytes", number)
            })?;
        let max_total_svg_bytes = value
            .max_total_svg_bytes
            .map_or(Ok(DEFAULT_MAX_TOTAL_SVG_BYTES), |number| {
                number_to_u64("maxTotalSvgBytes", number)
            })?;
        Ok(Self {
            convert,
            max_svg_bytes,
            max_total_svg_bytes,
        })
    }
}

fn number_to_u64(name: &str, number: f64) -> Result<u64> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > MAX_SAFE_INTEGER {
        return Err(Error::new(
            Status::InvalidArg,
            format!("{name} must be a non-negative safe integer"),
        ));
    }
    Ok(number as u64)
}

fn number_to_usize(name: &str, number: f64) -> Result<usize> {
    if number > usize::MAX as f64 {
        return Err(Error::new(
            Status::InvalidArg,
            format!("{name} is too large on this platform"),
        ));
    }
    number_to_u64(name, number).map(|value| value as usize)
}

fn io_error(error: std::io::Error) -> Error {
    Error::new(Status::GenericFailure, format!("I/O error: {error}"))
}

fn document_error(error: document_svg::Error) -> Error {
    Error::new(Status::GenericFailure, error.to_string())
}

impl From<CorePageReport> for PageReport {
    fn from(report: CorePageReport) -> Self {
        Self {
            number: report.number as f64,
            svg: report.svg,
            width_points: report.width_points,
            height_points: report.height_points,
            node_count: report.node_count as f64,
            warning_count: report.warning_count as f64,
            warnings: report.warnings,
            estimated_ir_bytes: report.estimated_ir_bytes as f64,
        }
    }
}

impl From<CoreConversionReport> for ConversionReport {
    fn from(report: CoreConversionReport) -> Self {
        Self {
            converter: report.converter.to_owned(),
            version: report.version.to_owned(),
            source: report.source,
            source_format: report.source_format.to_string().to_ascii_lowercase(),
            output_directory: report.output_directory,
            elapsed_ms: report.elapsed_ms as f64,
            input_bytes: report.input_bytes as f64,
            page_count: report.page_count as f64,
            largest_page_ir_bytes: report.largest_page_ir_bytes as f64,
            pages: report.pages.into_iter().map(Into::into).collect(),
            warnings: report.warnings,
        }
    }
}

impl From<CoreReverseReport> for ReverseReport {
    fn from(report: CoreReverseReport) -> Self {
        Self {
            converter: report.converter.to_owned(),
            version: report.version.to_owned(),
            source: report.source,
            output: report.output,
            output_format: report.output_format.to_string().to_ascii_lowercase(),
            page_count: report.page_count as f64,
            input_bytes: report.input_bytes as f64,
            warnings: report.warnings,
        }
    }
}

impl From<CorePreviewReport> for PreviewReport {
    fn from(preview: CorePreviewReport) -> Self {
        let report = preview.report;
        let needs_review = !report.warnings.is_empty()
            || report.pages.iter().any(|page| !page.warnings.is_empty());
        let pages = report
            .pages
            .into_iter()
            .zip(preview.svg_pages)
            .map(|(page, svg)| PreviewPage {
                number: page.number as f64,
                svg,
                width_points: page.width_points,
                height_points: page.height_points,
                node_count: page.node_count as f64,
                warning_count: page.warning_count as f64,
                warnings: page.warnings,
                estimated_ir_bytes: page.estimated_ir_bytes as f64,
            })
            .collect();
        Self {
            converter: report.converter.to_owned(),
            version: report.version.to_owned(),
            source: report.source,
            source_format: report.source_format.to_string().to_ascii_lowercase(),
            elapsed_ms: report.elapsed_ms as f64,
            input_bytes: report.input_bytes as f64,
            page_count: report.page_count as f64,
            largest_page_ir_bytes: report.largest_page_ir_bytes as f64,
            pages,
            warnings: report.warnings,
            needs_review,
        }
    }
}
