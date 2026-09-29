use std::io::Read;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use document_svg::{ConvertOptions, ReverseOptions, convert_path, svg_to_document};
use serde::{Deserialize, Serialize};

const MAX_TRANSFORM_INPUT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "docsvg",
    version,
    about = "Convert documents, comic archives, diagrams, CAD/CAM/3D, simulation, raster, chart, and math inputs to SVG pages; use 'reverse' to export SVG pages",
    after_help = "Examples:\n  docsvg report.pdf --output preview/report\n  docsvg reverse preview/report --output report.pptx\n  docsvg transform diagram.svg --output small.svg --minify\n  docsvg formats pdf --details\n\nAll supported formats and fidelity notes: https://ryusui-hiro.github.io/document-svg/formats.html"
)]
struct Cli {
    /// Source document, diagram, image, data, or SVG file. See the format reference for details.
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

    /// Maximum expanded PDF stream or ZIP part/image (Office, EPUB, 3MF, CBZ) in MiB.
    #[arg(long, default_value_t = 128)]
    max_entry_mib: u64,

    /// Maximum number of output pages/slides/sheets.
    #[arg(long, default_value_t = 10_000)]
    max_pages: usize,

    /// Maximum XML-like parser events per input part.
    #[arg(long, default_value_t = 5_000_000)]
    max_xml_events: usize,

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

    /// Write the complete conversion report as JSON to standard output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
#[command(
    name = "docsvg reverse",
    about = "Package SVG pages into Office, draw.io, CAD/CAM/3D, simulation, diagram, table, math, image, or UI-code formats"
)]
struct ReverseCli {
    /// Source SVG file or directory containing SVG pages.
    input: PathBuf,

    /// Destination Office, draw.io, CAD/CAM/3D, simulation, diagram, table, math, PNG, WebP, HTML, UI-code, or Data URI file.
    #[arg(short, long)]
    output: PathBuf,

    /// Maximum combined SVG input size in MiB.
    #[arg(long, default_value_t = 512)]
    max_input_mib: u64,

    /// Maximum number of SVG pages.
    #[arg(long, default_value_t = 10_000)]
    max_pages: usize,

    /// Write the complete reverse report as JSON to standard output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
#[command(
    name = "docsvg transform",
    about = "Transform and optimize SVG files (minify, monochrome, responsive, clean paths, strip empty groups)"
)]
struct TransformCli {
    /// Source SVG file (maximum 512 MiB), or '-' for standard input.
    input: PathBuf,

    /// Destination transformed SVG file, or '-' for standard output.
    #[arg(short, long)]
    output: PathBuf,

    /// Remove comments, empty spaces, and minify SVG output.
    #[arg(long)]
    minify: bool,

    /// Unify all fill and stroke colors to a single monochrome color (e.g. "#000000").
    #[arg(long)]
    monochrome: Option<String>,

    /// Remove fixed width/height; requires a viewBox or positive absolute dimensions.
    #[arg(long)]
    responsive: bool,

    /// Round coordinates and path numbers to N decimal digits (0-12).
    #[arg(long)]
    precision: Option<usize>,

    /// Remove <metadata>, <desc>, and data-* attributes.
    #[arg(long)]
    remove_metadata: bool,

    /// Clean and optimize SVG path data (remove redundant zero-length segments and duplicate closes).
    #[arg(long)]
    clean_paths: bool,

    /// Strip empty <g> groups that contain no child rendering elements.
    #[arg(long)]
    strip_empty_groups: bool,
}

#[derive(Debug, Parser)]
#[command(
    name = "docsvg formats",
    about = "Search supported formats and fidelity notes"
)]
struct FormatsCli {
    /// File extension, product name, or format category to search. Omit to list categories.
    query: Option<String>,

    /// Language for names and notes.
    #[arg(long, default_value = "en", value_parser = ["en", "ja", "zh"])]
    lang: String,

    /// Show the fidelity notes for each result.
    #[arg(long)]
    details: bool,
}

#[derive(Deserialize)]
struct FormatCatalog {
    categories: Vec<FormatCategory>,
}

#[derive(Deserialize)]
struct FormatCategory {
    id: String,
    name: std::collections::HashMap<String, String>,
    items: Vec<FormatEntry>,
}

#[derive(Deserialize)]
struct FormatEntry {
    ext: String,
    #[serde(default)]
    keywords: Vec<String>,
    forward: Option<String>,
    reverse: Option<String>,
    note: std::collections::HashMap<String, String>,
}

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments.get(1).is_some_and(|arg| arg == "convert") {
        arguments.remove(1);
    } else if arguments.get(1).is_some_and(|arg| arg == "reverse") {
        arguments.remove(1);
        arguments[0] = "docsvg reverse".into();
        return reverse(ReverseCli::parse_from(arguments));
    } else if arguments.get(1).is_some_and(|arg| arg == "transform") {
        arguments.remove(1);
        arguments[0] = "docsvg transform".into();
        return transform(TransformCli::parse_from(arguments));
    } else if arguments.get(1).is_some_and(|arg| arg == "formats") {
        arguments.remove(1);
        arguments[0] = "docsvg formats".into();
        return formats(FormatsCli::parse_from(arguments));
    }
    convert(Cli::parse_from(arguments))
}

fn formats(cli: FormatsCli) -> anyhow::Result<()> {
    let catalog: FormatCatalog =
        serde_json::from_str(include_str!("../site/assets/data/formats.json"))?;
    let Some(query) = cli.query.as_ref().map(|query| query.to_lowercase()) else {
        println!("Format categories (search with `docsvg formats QUERY`):");
        for category in &catalog.categories {
            let name = category.name.get(&cli.lang).unwrap_or(&category.id);
            println!("  {name}: {} formats", category.items.len());
        }
        return Ok(());
    };

    let mut count = 0;
    for category in &catalog.categories {
        let name = category.name.get(&cli.lang).unwrap_or(&category.id);
        let category_matches =
            category.id.to_lowercase().contains(&query) || name.to_lowercase().contains(&query);
        for item in &category.items {
            if !category_matches
                && !item.ext.to_lowercase().contains(&query)
                && !item
                    .keywords
                    .iter()
                    .any(|keyword| keyword.to_lowercase().contains(&query))
            {
                continue;
            }
            let forward = item.forward.as_deref().unwrap_or("-");
            let reverse = item.reverse.as_deref().unwrap_or("-");
            println!(
                "{}  |  to SVG: {forward}  |  from SVG: {reverse}  |  {name}",
                item.ext
            );
            if cli.details
                && let Some(note) = item.note.get(&cli.lang)
            {
                println!("  {note}");
            }
            count += 1;
        }
    }
    if count == 0 {
        anyhow::bail!("no supported formats match {query:?}");
    }
    println!("{count} format(s) matched. A = faithful, B = traced, C = rebuilt; - = unavailable.");
    Ok(())
}

fn convert(cli: Cli) -> anyhow::Result<()> {
    let options = ConvertOptions {
        max_input_bytes: cli.max_input_mib.saturating_mul(1024 * 1024),
        max_zip_entry_bytes: cli.max_entry_mib.saturating_mul(1024 * 1024),
        max_pages: cli.max_pages,
        max_xml_events: cli.max_xml_events,
        include_metadata: !cli.no_metadata,
        precision: cli.precision.min(12),
        jobs: cli.jobs,
        outline_embedded_pdf_text: cli.outline_embedded_pdf_text,
        embed_drawio_source: cli.embed_drawio_source,
        stencil_paths: cli.stencil_paths,
    };
    let report = convert_path(&cli.input, &cli.output, &options).with_context(|| {
        format!(
            "failed to convert {} into {}",
            cli.input.display(),
            cli.output.display()
        )
    })?;
    if cli.json {
        write_json_report(&report)?;
    } else {
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
    if cli.json {
        write_json_report(&report)?;
    } else {
        println!(
            "packaged {} SVG page(s) into {} ({})",
            report.page_count, report.output, report.output_format
        );
        for warning in &report.warnings {
            eprintln!("warning: {warning}");
        }
    }
    Ok(())
}

fn write_json_report<T: Serialize>(report: &T) -> anyhow::Result<()> {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer(&mut writer, report)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn transform(cli: TransformCli) -> anyhow::Result<()> {
    if cli.precision.is_some_and(|precision| precision > 12) {
        anyhow::bail!("precision must be between 0 and 12");
    }
    let mut bytes = Vec::new();
    let is_stdin = cli.input.as_os_str() == "-";
    let is_stdout = cli.output.as_os_str() == "-";

    if is_stdin {
        Read::take(
            &mut std::io::stdin(),
            MAX_TRANSFORM_INPUT_BYTES.saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .context("failed to read from standard input")?;
    } else {
        let mut file = std::fs::File::open(&cli.input)
            .with_context(|| format!("failed to open {}", cli.input.display()))?;
        Read::take(&mut file, MAX_TRANSFORM_INPUT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read {}", cli.input.display()))?;
    }

    if bytes.len() as u64 > MAX_TRANSFORM_INPUT_BYTES {
        anyhow::bail!("SVG transform input exceeds maximum bytes ({MAX_TRANSFORM_INPUT_BYTES})");
    }

    let options = document_svg::TransformOptions {
        minify: cli.minify,
        monochrome: cli.monochrome,
        responsive: cli.responsive,
        precision: cli.precision,
        remove_metadata: cli.remove_metadata,
        clean_paths: cli.clean_paths,
        strip_empty_groups: cli.strip_empty_groups,
    };
    let transformed = document_svg::transform_svg(&bytes, &options).with_context(|| {
        format!(
            "failed to transform {}",
            if is_stdin {
                "stdin".to_string()
            } else {
                cli.input.display().to_string()
            }
        )
    })?;

    if is_stdout {
        use std::io::Write;
        std::io::stdout().write_all(&transformed)?;
    } else {
        // Keep an existing symlink in place while replacing its target.
        let destination = if cli.output.is_symlink() {
            cli.output.canonicalize()?
        } else {
            cli.output.clone()
        };
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        use std::io::Write;
        temporary.write_all(&transformed)?;
        if let Ok(metadata) = std::fs::metadata(&destination) {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
        }
        temporary
            .persist(&destination)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to write {}", cli.output.display()))?;
        println!(
            "transformed {} ({} bytes) -> {} ({} bytes)",
            cli.input.display(),
            bytes.len(),
            cli.output.display(),
            transformed.len()
        );
    }
    Ok(())
}
