use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use document_svg::{ConvertOptions, ReverseOptions, convert_path, svg_to_document};

#[derive(Debug, Parser)]
#[command(
    name = "docsvg",
    version,
    about = "Convert PDF, Office, draw.io, CAD (DXF/Gerber/HP-GL/G-code/Excellon/STL/STEP/OBJ/Simulation), Diagram (DOT/Mermaid), LaTeX, Markdown table, or Chart documents to SVG pages; use 'reverse' to go back",
    after_help = "Reverse conversion: docsvg reverse INPUT.svg --output OUTPUT.pptx\nINPUT may also be a directory containing SVG pages; OUTPUT may end in .pptx, .docx, .xlsx, .drawio, .dxf, .gcode, .nc, .gbr, .plt, .dot, .mmd, .md, .csv, .tex, .datauri, .png, .jsx, .tsx, .vue."
)]
struct Cli {
    /// Source PDF, PPTX, XLSX, DOCX, draw.io, DXF, Gerber, HP-GL, G-code, Excellon, STL, STEP, OBJ, Gmsh/VTK, DOT, Mermaid, Markdown, Chart, or LaTeX file.
    input: PathBuf,

    /// Directory that receives page-NNNN.svg and conversion.json.
    #[arg(short, long)]
    output: PathBuf,

    /// PDF page worker count. Office XML stays page-streamed.
    #[arg(short = 'j', long, default_value_t = 1)]
    jobs: usize,

    /// Maximum input size in MiB.
    #[arg(long, default_value_t = 512)]
    max_input_mib: u64,

    /// Maximum expanded PDF page stream or OOXML ZIP entry in MiB.
    #[arg(long, default_value_t = 128)]
    max_entry_mib: u64,

    /// Maximum number of output pages/slides/sheets.
    #[arg(long, default_value_t = 10_000)]
    max_pages: usize,

    /// Omit provenance metadata from SVG output.
    #[arg(long)]
    no_metadata: bool,

    /// Floating-point precision for SVG coordinates.
    #[arg(long, default_value_t = 5)]
    precision: usize,

    /// Outline embedded PDF fonts for maximum fidelity instead of editable text.
    #[arg(long)]
    outline_embedded_pdf_text: bool,

    /// Keep a copy of the draw.io source in each SVG so it can be opened as an
    /// editable diagram again, in draw.io or with `docsvg reverse`.
    #[arg(long)]
    embed_drawio_source: bool,

    /// draw.io stencil XML file or directory to draw shape libraries with.
    /// Repeat for more than one.
    #[arg(long = "stencils", value_name = "PATH")]
    stencil_paths: Vec<PathBuf>,
}

#[derive(Debug, Parser)]
#[command(
    name = "docsvg reverse",
    about = "Package SVG pages into PPTX, DOCX, XLSX, draw.io, CAD (DXF, G-code, Gerber, HP-GL), DOT/Mermaid, Markdown, CSV, LaTeX, React JSX/TSX, Vue, DataURI, or PNG"
)]
struct ReverseCli {
    /// Source SVG file or directory containing SVG pages.
    input: PathBuf,

    /// Destination PPTX, DOCX, XLSX, .drawio, .dxf, .gcode, .nc, .gbr, .plt, .dot, .mmd, .md, .csv, .tex, .datauri, .png, .jsx, .tsx, or .vue file.
    #[arg(short, long)]
    output: PathBuf,

    /// Maximum combined SVG input size in MiB.
    #[arg(long, default_value_t = 512)]
    max_input_mib: u64,

    /// Maximum number of SVG pages.
    #[arg(long, default_value_t = 10_000)]
    max_pages: usize,
}

#[derive(Debug, Parser)]
#[command(
    name = "docsvg transform",
    about = "Transform and optimize SVG files (minify, monochrome, responsive)"
)]
struct TransformCli {
    /// Source SVG file.
    input: PathBuf,

    /// Destination transformed SVG file.
    #[arg(short, long)]
    output: PathBuf,

    /// Remove comments, empty spaces, and minify SVG output.
    #[arg(long)]
    minify: bool,

    /// Unify all fill and stroke colors to a single monochrome color (e.g. "#000000").
    #[arg(long)]
    monochrome: Option<String>,

    /// Remove fixed width/height and ensure viewBox is present for responsive scaling.
    #[arg(long)]
    responsive: bool,

    /// Round coordinates and path numbers to N decimal digits.
    #[arg(long)]
    precision: Option<usize>,
}

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "reverse")
    {
        arguments.remove(1);
        arguments[0] = "docsvg reverse".into();
        return reverse(ReverseCli::parse_from(arguments));
    }
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "transform")
    {
        arguments.remove(1);
        arguments[0] = "docsvg transform".into();
        return transform(TransformCli::parse_from(arguments));
    }
    convert(Cli::parse_from(arguments))
}

fn convert(cli: Cli) -> anyhow::Result<()> {
    let options = ConvertOptions {
        max_input_bytes: cli.max_input_mib.saturating_mul(1024 * 1024),
        max_zip_entry_bytes: cli.max_entry_mib.saturating_mul(1024 * 1024),
        max_pages: cli.max_pages,
        include_metadata: !cli.no_metadata,
        precision: cli.precision.min(12),
        jobs: cli.jobs,
        outline_embedded_pdf_text: cli.outline_embedded_pdf_text,
        embed_drawio_source: cli.embed_drawio_source,
        stencil_paths: cli.stencil_paths,
        ..ConvertOptions::default()
    };
    let report = convert_path(&cli.input, &cli.output, &options).with_context(|| {
        format!(
            "failed to convert {} into {}",
            cli.input.display(),
            cli.output.display()
        )
    })?;
    println!(
        "converted {} {} page(s) to {} in {} ms",
        report.source_format, report.page_count, report.output_directory, report.elapsed_ms
    );
    if !report.warnings.is_empty() {
        eprintln!(
            "completed with {} distinct warning(s); see conversion.json",
            report.warnings.len()
        );
    }
    Ok(())
}

fn reverse(cli: ReverseCli) -> anyhow::Result<()> {
    let options = ReverseOptions {
        max_input_bytes: cli.max_input_mib.saturating_mul(1024 * 1024),
        max_pages: cli.max_pages,
    };
    let report = svg_to_document(&cli.input, &cli.output, &options).with_context(|| {
        format!(
            "failed to package {} into {}",
            cli.input.display(),
            cli.output.display()
        )
    })?;
    println!(
        "packaged {} SVG page(s) into {} ({})",
        report.page_count, report.output, report.output_format
    );
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

fn transform(cli: TransformCli) -> anyhow::Result<()> {
    let bytes = std::fs::read(&cli.input)
        .with_context(|| format!("failed to read {}", cli.input.display()))?;
    let options = document_svg::TransformOptions {
        minify: cli.minify,
        monochrome: cli.monochrome,
        responsive: cli.responsive,
        precision: cli.precision,
    };
    let transformed = document_svg::transform_svg(&bytes, &options)
        .with_context(|| format!("failed to transform {}", cli.input.display()))?;
    if let Some(parent) = cli.output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&cli.output, &transformed)
        .with_context(|| format!("failed to write {}", cli.output.display()))?;
    println!(
        "transformed {} ({} bytes) -> {} ({} bytes)",
        cli.input.display(),
        bytes.len(),
        cli.output.display(),
        transformed.len()
    );
    Ok(())
}
