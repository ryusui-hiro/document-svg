use std::path::PathBuf;

use document_svg::{ConvertOptions, Error as DocumentSvgError, ReverseOptions, convert_path};
use pyo3::exceptions::{PyOSError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;

#[derive(Default)]
struct PythonOptions {
    max_input_bytes: Option<u64>,
    max_zip_entry_bytes: Option<u64>,
    max_pages: Option<usize>,
    max_xml_events: Option<usize>,
    include_metadata: Option<bool>,
    precision: Option<usize>,
    jobs: Option<usize>,
    outline_embedded_pdf_text: Option<bool>,
    embed_drawio_source: Option<bool>,
    stencil_paths: Option<Vec<PathBuf>>,
}

impl PythonOptions {
    fn apply(self) -> ConvertOptions {
        let mut options = ConvertOptions::default();
        if let Some(value) = self.max_input_bytes {
            options.max_input_bytes = value;
        }
        if let Some(value) = self.max_zip_entry_bytes {
            options.max_zip_entry_bytes = value;
        }
        if let Some(value) = self.max_pages {
            options.max_pages = value;
        }
        if let Some(value) = self.max_xml_events {
            options.max_xml_events = value;
        }
        if let Some(value) = self.include_metadata {
            options.include_metadata = value;
        }
        if let Some(value) = self.precision {
            options.precision = value;
        }
        if let Some(value) = self.jobs {
            options.jobs = value;
        }
        if let Some(value) = self.outline_embedded_pdf_text {
            options.outline_embedded_pdf_text = value;
        }
        if let Some(value) = self.embed_drawio_source {
            options.embed_drawio_source = value;
        }
        if let Some(value) = self.stencil_paths {
            options.stencil_paths = value;
        }
        options
    }
}

#[pyfunction]
#[pyo3(name = "_convert_json")]
#[pyo3(signature = (
    input_path,
    output_directory,
    *,
    max_input_bytes=None,
    max_zip_entry_bytes=None,
    max_pages=None,
    max_xml_events=None,
    include_metadata=None,
    precision=None,
    jobs=None,
    outline_embedded_pdf_text=None,
    embed_drawio_source=None,
    stencil_paths=None
))]
#[allow(clippy::too_many_arguments)]
fn convert_json(
    py: Python<'_>,
    input_path: PathBuf,
    output_directory: PathBuf,
    max_input_bytes: Option<u64>,
    max_zip_entry_bytes: Option<u64>,
    max_pages: Option<usize>,
    max_xml_events: Option<usize>,
    include_metadata: Option<bool>,
    precision: Option<usize>,
    jobs: Option<usize>,
    outline_embedded_pdf_text: Option<bool>,
    embed_drawio_source: Option<bool>,
    stencil_paths: Option<Vec<PathBuf>>,
) -> PyResult<String> {
    let options = PythonOptions {
        max_input_bytes,
        max_zip_entry_bytes,
        max_pages,
        max_xml_events,
        include_metadata,
        precision,
        jobs,
        outline_embedded_pdf_text,
        embed_drawio_source,
        stencil_paths,
    }
    .apply();

    let report = py
        .detach(|| convert_path(input_path, output_directory, &options))
        .map_err(document_error_to_python)?;
    serde_json::to_string(&report)
        .map_err(|error| PyRuntimeError::new_err(format!("failed to serialize report: {error}")))
}

#[pyfunction]
#[pyo3(name = "_reverse_json")]
#[pyo3(signature = (input_path, output_path, *, max_input_bytes=None, max_pages=None))]
fn reverse_json(
    py: Python<'_>,
    input_path: PathBuf,
    output_path: PathBuf,
    max_input_bytes: Option<u64>,
    max_pages: Option<usize>,
) -> PyResult<String> {
    let mut options = ReverseOptions::default();
    if let Some(value) = max_input_bytes {
        options.max_input_bytes = value;
    }
    if let Some(value) = max_pages {
        options.max_pages = value;
    }
    let report = py
        .detach(|| document_svg::svg_to_document(input_path, output_path, &options))
        .map_err(document_error_to_python)?;
    serde_json::to_string(&report)
        .map_err(|error| PyRuntimeError::new_err(format!("failed to serialize report: {error}")))
}

const DEFAULT_MAX_SVG_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_SVG_BYTES: u64 = 256 * 1024 * 1024;

fn source_format_name(source_format: document_svg::SourceFormat) -> String {
    match source_format {
        document_svg::SourceFormat::ProjectXml => "projectxml".into(),
        document_svg::SourceFormat::Properties => "properties".into(),
        document_svg::SourceFormat::KicadPcb => "kicad_pcb".into(),
        document_svg::SourceFormat::KicadSchLegacy => "kicad_sch_legacy".into(),
        document_svg::SourceFormat::KicadSch => "kicad_sch".into(),
        document_svg::SourceFormat::LtspiceAsc => "ltspice_asc".into(),
        document_svg::SourceFormat::EagleSch => "eagle_sch".into(),
        document_svg::SourceFormat::Ifc => "ifc".into(),
        document_svg::SourceFormat::EsriAsciiGrid => "esri_ascii_grid".into(),
        document_svg::SourceFormat::Iso19115 => "iso19115".into(),
        document_svg::SourceFormat::DicomSr => "dicom-sr".into(),
        other => other.to_string().to_ascii_lowercase(),
    }
}

#[pyfunction]
#[pyo3(name = "_preview_json")]
#[pyo3(signature = (
    input_path,
    *,
    max_svg_bytes=None,
    max_total_svg_bytes=None,
    max_input_bytes=None,
    max_zip_entry_bytes=None,
    max_pages=None,
    max_xml_events=None,
    include_metadata=None,
    precision=None,
    jobs=None,
    outline_embedded_pdf_text=None,
    embed_drawio_source=None,
    stencil_paths=None
))]
#[allow(clippy::too_many_arguments)]
fn preview_json(
    py: Python<'_>,
    input_path: PathBuf,
    max_svg_bytes: Option<u64>,
    max_total_svg_bytes: Option<u64>,
    max_input_bytes: Option<u64>,
    max_zip_entry_bytes: Option<u64>,
    max_pages: Option<usize>,
    max_xml_events: Option<usize>,
    include_metadata: Option<bool>,
    precision: Option<usize>,
    jobs: Option<usize>,
    outline_embedded_pdf_text: Option<bool>,
    embed_drawio_source: Option<bool>,
    stencil_paths: Option<Vec<PathBuf>>,
) -> PyResult<String> {
    let options = PythonOptions {
        max_input_bytes,
        max_zip_entry_bytes,
        max_pages,
        max_xml_events,
        include_metadata,
        precision,
        jobs,
        outline_embedded_pdf_text,
        embed_drawio_source,
        stencil_paths,
    }
    .apply();

    let max_single_svg = max_svg_bytes.unwrap_or(DEFAULT_MAX_SVG_BYTES);
    let max_total_svg = max_total_svg_bytes.unwrap_or(DEFAULT_MAX_TOTAL_SVG_BYTES);

    let report_val = py
        .detach(|| -> Result<String, DocumentSvgError> {
            let temp_dir = tempfile::tempdir()?;
            let report = convert_path(&input_path, temp_dir.path(), &options)?;
            let mut total_svg_bytes = 0u64;
            let mut pages = Vec::with_capacity(report.pages.len());
            for page in report.pages {
                let svg_filename = format!("page-{:04}.svg", page.number);
                let svg_path = temp_dir.path().join(svg_filename);
                let meta = std::fs::metadata(&svg_path)?;
                if meta.len() > max_single_svg {
                    return Err(DocumentSvgError::LimitExceeded(format!(
                        "preview page {} is {} bytes; maximum is {max_single_svg} bytes",
                        page.number, meta.len()
                    )));
                }
                total_svg_bytes = total_svg_bytes.saturating_add(meta.len());
                if total_svg_bytes > max_total_svg {
                    return Err(DocumentSvgError::LimitExceeded(format!(
                        "preview SVG output is {total_svg_bytes} bytes; maximum is {max_total_svg} bytes"
                    )));
                }
                let svg = std::fs::read_to_string(&svg_path)?;
                pages.push(serde_json::json!({
                    "number": page.number,
                    "svg": svg,
                    "width_points": page.width_points,
                    "height_points": page.height_points,
                    "node_count": page.node_count,
                    "warning_count": page.warning_count,
                    "warnings": page.warnings,
                    "estimated_ir_bytes": page.estimated_ir_bytes,
                }));
            }
            let needs_review = !report.warnings.is_empty()
                || pages
                    .iter()
                    .any(|p| p.get("warning_count").and_then(|c| c.as_u64()).unwrap_or(0) > 0);
            let full_report = serde_json::json!({
                "converter": report.converter,
                "version": report.version,
                "source": report.source,
                "source_format": source_format_name(report.source_format),
                "elapsed_ms": report.elapsed_ms,
                "input_bytes": report.input_bytes,
                "page_count": report.page_count,
                "largest_page_ir_bytes": report.largest_page_ir_bytes,
                "pages": pages,
                "warnings": report.warnings,
                "needs_review": needs_review,
            });
            serde_json::to_string(&full_report)
                .map_err(|e| DocumentSvgError::InvalidInput(e.to_string()))
        })
        .map_err(document_error_to_python)?;
    Ok(report_val)
}

#[pyfunction]
#[pyo3(name = "_transform_svg")]
#[pyo3(signature = (
    svg_bytes,
    *,
    minify=false,
    monochrome=None,
    responsive=false,
    precision=None,
    remove_metadata=false,
    clean_paths=false,
    strip_empty_groups=false
))]
#[allow(clippy::too_many_arguments)]
fn transform_svg_py(
    py: Python<'_>,
    svg_bytes: Vec<u8>,
    minify: bool,
    monochrome: Option<String>,
    responsive: bool,
    precision: Option<usize>,
    remove_metadata: bool,
    clean_paths: bool,
    strip_empty_groups: bool,
) -> PyResult<Vec<u8>> {
    let options = document_svg::TransformOptions {
        minify,
        monochrome,
        responsive,
        precision,
        remove_metadata,
        clean_paths,
        strip_empty_groups,
    };
    py.detach(|| document_svg::transform_svg(&svg_bytes, &options))
        .map_err(document_error_to_python)
}

fn document_error_to_python(error: DocumentSvgError) -> PyErr {
    match error {
        DocumentSvgError::InvalidInput(_)
        | DocumentSvgError::Unsupported(_)
        | DocumentSvgError::LimitExceeded(_) => PyValueError::new_err(error.to_string()),
        DocumentSvgError::Io(_) => PyOSError::new_err(error.to_string()),
        _ => PyRuntimeError::new_err(error.to_string()),
    }
}

#[pymodule]
#[pyo3(name = "_native")]
fn document_svg_python(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(convert_json, module)?)?;
    module.add_function(wrap_pyfunction!(reverse_json, module)?)?;
    module.add_function(wrap_pyfunction!(preview_json, module)?)?;
    module.add_function(wrap_pyfunction!(transform_svg_py, module)?)?;
    Ok(())
}
