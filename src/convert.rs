use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::ir::Page;
use crate::svg::{SvgOptions, write_page};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceFormat {
    Pdf,
    Pptx,
    Xlsx,
    Docx,
}

impl SourceFormat {
    pub fn detect(path: &Path) -> Result<Self> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| Error::InvalidInput("input has no file extension".into()))?;
        match extension.as_str() {
            "pdf" => Ok(Self::Pdf),
            "pptx" => Ok(Self::Pptx),
            "xlsx" => Ok(Self::Xlsx),
            "docx" => Ok(Self::Docx),
            _ => Err(Error::Unsupported(format!(
                "extension .{extension}; expected PDF, PPTX, XLSX, or DOCX"
            ))),
        }
    }
}

impl std::fmt::Display for SourceFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Pdf => "PDF",
            Self::Pptx => "PPTX",
            Self::Xlsx => "XLSX",
            Self::Docx => "DOCX",
        })
    }
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub max_input_bytes: u64,
    pub max_zip_entry_bytes: u64,
    pub max_pages: usize,
    pub max_xml_events: usize,
    pub include_metadata: bool,
    pub precision: usize,
    pub jobs: usize,
    /// Render embedded PDF fonts as exact glyph outlines instead of editable
    /// substitute-font text. This improves fidelity but requires the caller to
    /// verify the source font's outline/embedding rights.
    pub outline_embedded_pdf_text: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: 512 * 1024 * 1024,
            max_zip_entry_bytes: 128 * 1024 * 1024,
            max_pages: 10_000,
            max_xml_events: 5_000_000,
            include_metadata: true,
            precision: 5,
            jobs: 1,
            outline_embedded_pdf_text: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PageReport {
    pub number: usize,
    pub svg: String,
    pub width_points: f64,
    pub height_points: f64,
    pub node_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<String>,
    pub estimated_ir_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversionReport {
    pub converter: &'static str,
    pub version: &'static str,
    pub source: String,
    pub source_format: SourceFormat,
    pub output_directory: String,
    pub elapsed_ms: u128,
    pub input_bytes: u64,
    pub page_count: usize,
    pub largest_page_ir_bytes: usize,
    pub pages: Vec<PageReport>,
    pub warnings: Vec<String>,
}

pub fn convert_path(
    input: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
    options: &ConvertOptions,
) -> Result<ConversionReport> {
    let started = Instant::now();
    let input = input.as_ref();
    let output_directory = output_directory.as_ref();
    let metadata = fs::metadata(input)?;
    if !metadata.is_file() {
        return Err(Error::InvalidInput(format!(
            "{} is not a regular file",
            input.display()
        )));
    }
    if metadata.len() > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "input is {} bytes; maximum is {} bytes",
            metadata.len(),
            options.max_input_bytes
        )));
    }
    if options.jobs == 0 {
        return Err(Error::InvalidInput("jobs must be at least 1".into()));
    }
    fs::create_dir_all(output_directory)?;
    if fs::read_dir(output_directory)?.next().is_some() {
        return Err(Error::InvalidInput("output directory must be empty".into()));
    }
    let format = SourceFormat::detect(input)?;
    let mut sink = PageSink::new(output_directory, options);
    let warnings = match format {
        SourceFormat::Pdf => crate::pdf::convert(input, options, &mut sink)?,
        SourceFormat::Pptx => crate::ooxml::pptx::convert(input, options, &mut sink)?,
        SourceFormat::Xlsx => crate::ooxml::xlsx::convert(input, options, &mut sink)?,
        SourceFormat::Docx => crate::ooxml::docx::convert(input, options, &mut sink)?,
    };
    let pages = sink.finish()?;
    let largest_page_ir_bytes = pages
        .iter()
        .map(|page| page.estimated_ir_bytes)
        .max()
        .unwrap_or(0);
    let report = ConversionReport {
        converter: "document-svg",
        version: env!("CARGO_PKG_VERSION"),
        source: input.to_string_lossy().into_owned(),
        source_format: format,
        output_directory: output_directory.to_string_lossy().into_owned(),
        elapsed_ms: started.elapsed().as_millis(),
        input_bytes: metadata.len(),
        page_count: pages.len(),
        largest_page_ir_bytes,
        pages,
        warnings,
    };
    let manifest_path = output_directory.join("conversion.json");
    let mut temporary = tempfile::NamedTempFile::new_in(output_directory)?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, &report)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    temporary
        .persist_noclobber(manifest_path)
        .map_err(|error| error.error)?;
    Ok(report)
}

pub(crate) trait PageConsumer {
    fn consume(&mut self, page: Page) -> Result<()>;
}

pub(crate) struct PageSink<'a> {
    output_directory: &'a Path,
    svg_options: SvgOptions,
    reports: Vec<PageReport>,
    max_pages: usize,
}

impl<'a> PageSink<'a> {
    fn new(output_directory: &'a Path, options: &ConvertOptions) -> Self {
        Self {
            output_directory,
            svg_options: SvgOptions {
                include_metadata: options.include_metadata,
                precision: options.precision,
            },
            reports: Vec::new(),
            max_pages: options.max_pages,
        }
    }

    fn finish(self) -> Result<Vec<PageReport>> {
        if self.reports.is_empty() {
            return Err(Error::InvalidInput(
                "input contains no renderable pages".into(),
            ));
        }
        Ok(self.reports)
    }
}

impl PageConsumer for PageSink<'_> {
    fn consume(&mut self, page: Page) -> Result<()> {
        if self.reports.len() >= self.max_pages {
            return Err(Error::LimitExceeded(format!(
                "page count exceeds {}",
                self.max_pages
            )));
        }
        let file_name = format!("page-{:04}.svg", page.number);
        let final_path = self.output_directory.join(&file_name);
        let mut temporary = tempfile::NamedTempFile::new_in(self.output_directory)?;
        {
            let mut writer = BufWriter::with_capacity(64 * 1024, temporary.as_file_mut());
            write_page(&page, &mut writer, self.svg_options)?;
            writer.flush()?;
        }
        temporary
            .persist_noclobber(&final_path)
            .map_err(|error| error.error)?;
        let estimated_ir_bytes = estimate_page_bytes(&page);
        self.reports.push(PageReport {
            number: page.number,
            svg: file_name,
            width_points: page.width,
            height_points: page.height,
            node_count: page.nodes.len(),
            warning_count: page.warnings.len(),
            warnings: page.warnings,
            estimated_ir_bytes,
        });
        Ok(())
    }
}

fn estimate_page_bytes(page: &Page) -> usize {
    let serialized = serde_json::to_vec(page);
    serialized.map_or(0, |bytes| bytes.len())
}
