use std::path::PathBuf;

use document_svg::{
    ConvertOptions, Error as DocumentSvgError, ReverseOptions, convert_path, svg_to_openxml,
};
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
        .detach(|| svg_to_openxml(input_path, output_path, &options))
        .map_err(document_error_to_python)?;
    serde_json::to_string(&report)
        .map_err(|error| PyRuntimeError::new_err(format!("failed to serialize report: {error}")))
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
    Ok(())
}
