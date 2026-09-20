use std::cell::RefCell;

use document_svg::{ByteConvertLimits, ConvertOptions, SvgPage, TextSpan};
use js_sys::Function;
use serde::Serialize;
use wasm_bindgen::prelude::*;

const DEFAULT_MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_ZIP_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const DEFAULT_MAX_PAGES: u64 = 1_000;
const DEFAULT_MAX_XML_EVENTS: u64 = 2_000_000;
const DEFAULT_MAX_PAGE_SVG_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_SVG_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_MAX_PAGE_TEXT_SPANS: u64 = 5_000;
const DEFAULT_MAX_TOTAL_TEXT_SPANS: u64 = 50_000;
const DEFAULT_MAX_TOTAL_TEXT_BYTES: u64 = 8 * 1024 * 1024;

const HARD_MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_ZIP_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
const HARD_MAX_PAGES: u64 = 10_000;
const HARD_MAX_XML_EVENTS: u64 = 5_000_000;
const HARD_MAX_PAGE_SVG_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_TOTAL_SVG_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_PAGE_TEXT_SPANS: u64 = 50_000;
const HARD_MAX_TOTAL_TEXT_SPANS: u64 = 500_000;
const HARD_MAX_TOTAL_TEXT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserPage {
    number: usize,
    svg: String,
    width_points: f64,
    height_points: f64,
    node_count: usize,
    warning_count: usize,
    warnings: Vec<String>,
    estimated_ir_bytes: usize,
    text_spans: Vec<BrowserTextSpan>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserTextSpan {
    text: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    font_size: f64,
    transform: [f64; 6],
}

impl From<TextSpan> for BrowserTextSpan {
    fn from(span: TextSpan) -> Self {
        Self {
            text: span.text,
            x: span.x,
            y: span.y,
            width: span.width,
            height: span.height,
            font_size: span.font_size,
            transform: span.transform,
        }
    }
}

impl From<SvgPage> for BrowserPage {
    fn from(page: SvgPage) -> Self {
        Self {
            number: page.number,
            svg: page.svg,
            width_points: page.width_points,
            height_points: page.height_points,
            node_count: page.node_count,
            warning_count: page.warning_count,
            warnings: page.warnings,
            estimated_ir_bytes: page.estimated_ir_bytes,
            text_spans: page.text_spans.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserReport {
    converter: &'static str,
    version: &'static str,
    source: String,
    source_format: String,
    elapsed_ms: f64,
    input_bytes: f64,
    page_count: usize,
    largest_page_ir_bytes: usize,
    warnings: Vec<String>,
}

fn option_limit(
    options: &JsValue,
    name: &str,
    default: u64,
    hard_maximum: u64,
) -> Result<u64, JsValue> {
    if options.is_undefined() || options.is_null() {
        return Ok(default);
    }
    let value = js_sys::Reflect::get(options, &JsValue::from_str(name))?;
    if value.is_undefined() || value.is_null() {
        return Ok(default);
    }
    let number = value
        .as_f64()
        .filter(|number| number.is_finite() && *number >= 1.0 && number.fract() == 0.0)
        .ok_or_else(|| JsValue::from_str(&format!("{name} must be a positive integer")))?;
    if number > hard_maximum as f64 {
        return Err(JsValue::from_str(&format!(
            "{name} exceeds the browser safety maximum of {hard_maximum}"
        )));
    }
    Ok(number as u64)
}

fn option_bool(options: &JsValue, name: &str, default: bool) -> Result<bool, JsValue> {
    if options.is_undefined() || options.is_null() {
        return Ok(default);
    }
    let value = js_sys::Reflect::get(options, &JsValue::from_str(name))?;
    if value.is_undefined() || value.is_null() {
        return Ok(default);
    }
    value
        .as_bool()
        .ok_or_else(|| JsValue::from_str(&format!("{name} must be a boolean")))
}

fn browser_options(options: &JsValue) -> Result<(ConvertOptions, ByteConvertLimits), JsValue> {
    let convert = ConvertOptions {
        max_input_bytes: option_limit(
            options,
            "maxInputBytes",
            DEFAULT_MAX_INPUT_BYTES,
            HARD_MAX_INPUT_BYTES,
        )?,
        max_zip_entry_bytes: option_limit(
            options,
            "maxZipEntryBytes",
            DEFAULT_MAX_ZIP_ENTRY_BYTES,
            HARD_MAX_ZIP_ENTRY_BYTES,
        )?,
        max_pages: usize::try_from(option_limit(
            options,
            "maxPages",
            DEFAULT_MAX_PAGES,
            HARD_MAX_PAGES,
        )?)
        .map_err(|_| JsValue::from_str("maxPages is outside this browser's integer range"))?,
        max_xml_events: usize::try_from(option_limit(
            options,
            "maxXmlEvents",
            DEFAULT_MAX_XML_EVENTS,
            HARD_MAX_XML_EVENTS,
        )?)
        .map_err(|_| JsValue::from_str("maxXmlEvents is outside this browser's integer range"))?,
        include_metadata: option_bool(options, "includeMetadata", true)?,
        precision: usize::try_from(option_limit(options, "precision", 4, 12)?)
            .map_err(|_| JsValue::from_str("precision is outside this browser's integer range"))?,
        // Page rendering is intentionally single-threaded in the Worker. Parallel
        // wasm32 browser builds require shared-memory/COOP/COEP deployment headers.
        jobs: 1,
        outline_embedded_pdf_text: option_bool(options, "outlineEmbeddedPdfText", false)?,
        ..ConvertOptions::default()
    };

    let max_page_svg_bytes = option_limit(
        options,
        "maxPageSvgBytes",
        DEFAULT_MAX_PAGE_SVG_BYTES,
        HARD_MAX_PAGE_SVG_BYTES,
    )?;
    let max_total_svg_bytes = option_limit(
        options,
        "maxTotalSvgBytes",
        DEFAULT_MAX_TOTAL_SVG_BYTES,
        HARD_MAX_TOTAL_SVG_BYTES,
    )?;
    let limits = ByteConvertLimits {
        max_page_svg_bytes: usize::try_from(max_page_svg_bytes)
            .map_err(|_| JsValue::from_str("maxPageSvgBytes is too large"))?,
        max_total_svg_bytes: usize::try_from(max_total_svg_bytes)
            .map_err(|_| JsValue::from_str("maxTotalSvgBytes is too large"))?,
        max_page_text_spans: usize::try_from(option_limit(
            options,
            "maxPageTextSpans",
            DEFAULT_MAX_PAGE_TEXT_SPANS,
            HARD_MAX_PAGE_TEXT_SPANS,
        )?)
        .map_err(|_| {
            JsValue::from_str("maxPageTextSpans is outside this browser's integer range")
        })?,
        max_total_text_spans: usize::try_from(option_limit(
            options,
            "maxTotalTextSpans",
            DEFAULT_MAX_TOTAL_TEXT_SPANS,
            HARD_MAX_TOTAL_TEXT_SPANS,
        )?)
        .map_err(|_| {
            JsValue::from_str("maxTotalTextSpans is outside this browser's integer range")
        })?,
        max_total_text_bytes: usize::try_from(option_limit(
            options,
            "maxTotalTextBytes",
            DEFAULT_MAX_TOTAL_TEXT_BYTES,
            HARD_MAX_TOTAL_TEXT_BYTES,
        )?)
        .map_err(|_| {
            JsValue::from_str("maxTotalTextBytes is outside this browser's integer range")
        })?,
    };
    Ok((convert, limits))
}

fn serialize<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value).map_err(|error| JsValue::from_str(&error.to_string()))
}

/// Convert a PDF, DOCX, XLSX, or PPTX from browser memory and synchronously
/// emit each completed page to the supplied callback. Call this from a Worker
/// so parsing and SVG generation never block the page's UI thread.
#[wasm_bindgen]
pub fn convert_document(
    file_name: &str,
    bytes: &[u8],
    options: JsValue,
    on_page: &Function,
) -> Result<JsValue, JsValue> {
    console_error_panic_hook::set_once();
    let (convert, limits) = browser_options(&options)?;
    let callback_error = RefCell::new(None::<JsValue>);
    let report = document_svg::convert_bytes(file_name, bytes, &convert, limits, |page| {
        let page = BrowserPage::from(page);
        let value = serialize(&page).map_err(|error| {
            document_svg::Error::InvalidInput(format!("cannot encode page event: {error:?}"))
        })?;
        if let Err(error) = on_page.call1(&JsValue::UNDEFINED, &value) {
            *callback_error.borrow_mut() = Some(error);
            return Err(document_svg::Error::InvalidInput(
                "the page callback failed".into(),
            ));
        }
        Ok(())
    });
    if let Some(error) = callback_error.into_inner() {
        return Err(error);
    }
    let report = report.map_err(|error| JsValue::from_str(&error.to_string()))?;
    let report = BrowserReport {
        converter: report.converter,
        version: report.version,
        source: report.source,
        source_format: report.source_format.to_string().to_ascii_lowercase(),
        elapsed_ms: report.elapsed_ms as f64,
        input_bytes: report.input_bytes as f64,
        page_count: report.page_count,
        largest_page_ir_bytes: report.largest_page_ir_bytes,
        warnings: report.warnings,
    };
    serialize(&report)
}
