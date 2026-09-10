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
    Drawio,
    Dxf,
    Gerber,
    Hpgl,
    Dot,
    Mermaid,
    Markdown,
    Chart,
    Tex,
    Qr,
    Raster,
    Gcode,
    Excellon,
    Stl,
    Simulation,
    Step,
    Obj,
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
            "drawio" | "dio" | "xml" => Ok(Self::Drawio),
            "dxf" => Ok(Self::Dxf),
            "gbr" | "gerber" => Ok(Self::Gerber),
            "plt" | "hpgl" => Ok(Self::Hpgl),
            "gcode" | "nc" | "ngc" | "tap" => Ok(Self::Gcode),
            "drl" | "drd" | "xln" => Ok(Self::Excellon),
            "stl" => Ok(Self::Stl),
            "step" | "stp" => Ok(Self::Step),
            "obj" => Ok(Self::Obj),
            "msh" | "vtk" => Ok(Self::Simulation),
            "dot" | "gv" => Ok(Self::Dot),
            "mmd" | "mermaid" => Ok(Self::Mermaid),
            "md" | "markdown" => Ok(Self::Markdown),
            "chart" => Ok(Self::Chart),
            "tex" | "latex" => Ok(Self::Tex),
            "qr" | "qrcode" => Ok(Self::Qr),
            "png" | "jpg" | "jpeg" => Ok(Self::Raster),
            "json" => {
                let filename = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if filename.ends_with(".chart.json") {
                    Ok(Self::Chart)
                } else {
                    Err(Error::Unsupported(format!(
                        "unsupported json format: {filename}"
                    )))
                }
            }
            _ => Err(Error::Unsupported(format!(
                "extension .{extension}; expected PDF, PPTX, XLSX, DOCX, DRAWIO, DXF, GBR, PLT, GCODE, DRL, STL, STEP, OBJ, MSH/VTK, DOT, MMD, MD, CHART, TEX, QR, or PNG/JPG"
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
            Self::Drawio => "DRAWIO",
            Self::Dxf => "DXF",
            Self::Gerber => "GERBER",
            Self::Hpgl => "HPGL",
            Self::Gcode => "GCODE",
            Self::Excellon => "EXCELLON",
            Self::Stl => "STL",
            Self::Step => "STEP",
            Self::Obj => "OBJ",
            Self::Simulation => "SIMULATION",
            Self::Dot => "DOT",
            Self::Mermaid => "MERMAID",
            Self::Markdown => "MARKDOWN",
            Self::Chart => "CHART",
            Self::Tex => "TEX",
            Self::Qr => "QR",
            Self::Raster => "RASTER",
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
    /// draw.io shape libraries to draw `shape=mxgraph.<library>.<name>` with.
    ///
    /// Each entry is a stencil XML file or a directory of them, as shipped in
    /// draw.io's own `stencils` folder. Without one, a shape from a library is
    /// drawn as a labelled placeholder, because the outline lives in the
    /// library rather than in the diagram. A shape the diagram carries inline,
    /// as `shape=stencil(...)`, is always drawn.
    pub stencil_paths: Vec<std::path::PathBuf>,
    /// Keep a copy of a draw.io page's own source in the SVG it produces, in
    /// the `content` attribute draw.io itself uses.
    ///
    /// The SVG stays a picture for every renderer, and both draw.io and
    /// [`crate::svg_to_document`] can restore the editable diagram from it.
    /// Off by default: it roughly doubles the output and puts the source
    /// document inside a file that is usually shared as an image.
    pub embed_drawio_source: bool,
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
            stencil_paths: Vec::new(),
            embed_drawio_source: false,
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
        SourceFormat::Drawio => crate::drawio::convert(input, options, &mut sink)?,
        SourceFormat::Dxf
        | SourceFormat::Gerber
        | SourceFormat::Hpgl
        | SourceFormat::Gcode
        | SourceFormat::Excellon
        | SourceFormat::Stl
        | SourceFormat::Step
        | SourceFormat::Obj
        | SourceFormat::Simulation => crate::cad::convert(input, options, &mut sink)?,
        SourceFormat::Dot | SourceFormat::Mermaid => {
            crate::diagram::convert(input, options, &mut sink)?
        }
        SourceFormat::Markdown => crate::table::convert(input, options, &mut sink)?,
        SourceFormat::Chart => crate::chart::convert(input, options, &mut sink)?,
        SourceFormat::Tex => crate::math::convert(input, options, &mut sink)?,
        SourceFormat::Qr => crate::qr::convert(input, options, &mut sink)?,
        SourceFormat::Raster => crate::vectorize::convert(input, options, &mut sink)?,
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
    // Keep the existing serialized-size metric without allocating a second
    // copy of image data and paths for every page.
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, page).map_or(0, |()| counter.0)
}

#[derive(Default)]
struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("serialized page size overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_size_counter_matches_serialized_bytes_without_retaining_json() {
        let mut page = Page::new(1, 612.0, 792.0, "test");
        page.title = "日本語 / 中文 / quotes: \" \\ \n".into();
        page.description = "large path or image data".repeat(100_000);
        page.warnings.push("control: \t\r".into());
        assert_eq!(
            estimate_page_bytes(&page),
            serde_json::to_vec(&page).unwrap().len()
        );
    }
}
