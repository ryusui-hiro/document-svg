use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use document_svg::{ConvertOptions, ReverseOptions, convert_path, svg_to_openxml};

#[derive(Debug, Parser)]
#[command(
    name = "docsvg",
    version,
    about = "Convert PDF or Office documents to SVG pages; use 'reverse' for SVG to OOXML",
    after_help = "Reverse conversion: docsvg reverse INPUT.svg --output OUTPUT.pptx\nINPUT may also be a directory containing SVG pages; OUTPUT may end in .pptx, .docx, or .xlsx."
)]
struct Cli {
    /// Source PDF, PPTX, XLSX, or DOCX file.
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
}

#[derive(Debug, Parser)]
#[command(
    name = "docsvg reverse",
    about = "Package SVG pages as vector images in PPTX, DOCX, or XLSX"
)]
struct ReverseCli {
    /// Source SVG file or directory containing SVG pages.
    input: PathBuf,

    /// Destination PPTX, DOCX, or XLSX file.
    #[arg(short, long)]
    output: PathBuf,

    /// Maximum combined SVG input size in MiB.
    #[arg(long, default_value_t = 512)]
    max_input_mib: u64,

    /// Maximum number of SVG pages.
    #[arg(long, default_value_t = 10_000)]
    max_pages: usize,
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
    let report = svg_to_openxml(&cli.input, &cli.output, &options).with_context(|| {
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
