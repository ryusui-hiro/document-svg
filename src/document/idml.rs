//! Bounded Adobe InDesign Markup Language (IDML) package previews.
//!
//! IDML is an OPC-like ZIP package containing `designmap.xml`, stories,
//! spreads and resources. This adapter inspects package and XML structure
//! in memory only; fonts, images, scripts, links and external resources are
//! never extracted, resolved or executed.

use std::io::{Cursor, Read, Seek};
use std::path::Path;

use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_IDML_BYTES: u64 = 256 * 1024 * 1024;
const MAX_IDML_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IDML_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_IDML_ENTRIES: usize = 100_000;
const MAX_IDML_XML_ENTRIES: usize = 20_000;
const MAX_IDML_XML_EVENTS: usize = 500_000;
const MAX_IDML_XML_NODES: usize = 300_000;
const MAX_IDML_XML_DEPTH: usize = 128;
const MAX_IDML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_IDML_ROWS: usize = 200_000;
const MAX_IDML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    entries: usize,
    xml_parts: usize,
    designmaps: usize,
    stories: usize,
    spreads: usize,
    master_spreads: usize,
    resources: usize,
    images: usize,
    fonts: usize,
    stories_nodes: usize,
    paragraphs: usize,
    characters: usize,
    hyperlinks: usize,
    tables: usize,
    graphics: usize,
    swatches: usize,
    rows: Vec<Vec<String>>,
}

struct IdmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for IdmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "idml".into();
        if page.title.is_empty() {
            page.title = "IDML InDesign package".into();
        }
        page.description =
            "IDML package and XML structure is rendered as bounded inert metadata; layout payloads and external resources are not opened".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_archive(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if metadata.len() > MAX_IDML_BYTES {
        return false;
    }
    let Ok(bytes) = read_limited_file(path, MAX_IDML_BYTES, "IDML sniff") else {
        return false;
    };
    let Ok(mut archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    if archive.len() > MAX_IDML_ENTRIES {
        return false;
    }
    (0..archive.len()).any(|index| {
        archive
            .by_index(index)
            .ok()
            .map(|entry| entry.name().eq_ignore_ascii_case("designmap.xml"))
            .unwrap_or(false)
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_IDML_BYTES),
        "IDML input",
    )?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| Error::InvalidInput(format!("invalid IDML ZIP package: {error}")))?;
    if archive.len() > MAX_IDML_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "IDML entries exceed {MAX_IDML_ENTRIES}"
        )));
    }
    let mut summary = Summary {
        entries: archive.len(),
        ..Summary::default()
    };
    let mut expanded_bytes = 0_u64;
    let mut designmap_seen = false;
    let mut xml_names = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        validate_entry_name(&name)?;
        if name.eq_ignore_ascii_case("designmap.xml") {
            designmap_seen = true;
            summary.designmaps = summary.designmaps.saturating_add(1);
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with("/") {
            continue;
        }
        if lower.starts_with("stories/") {
            summary.stories = summary.stories.saturating_add(1);
        }
        if lower.starts_with("spreads/") {
            summary.spreads = summary.spreads.saturating_add(1);
        }
        if lower.starts_with("masterspreads/") {
            summary.master_spreads = summary.master_spreads.saturating_add(1);
        }
        if lower.starts_with("resources/") {
            summary.resources = summary.resources.saturating_add(1);
            if lower.contains("images/")
                || lower.ends_with(".jpg")
                || lower.ends_with(".png")
                || lower.ends_with(".gif")
            {
                summary.images = summary.images.saturating_add(1);
            }
            if lower.contains("fonts/") || lower.ends_with(".otf") || lower.ends_with(".ttf") {
                summary.fonts = summary.fonts.saturating_add(1);
            }
        }
        if lower.ends_with(".xml") {
            if xml_names.len() >= MAX_IDML_XML_ENTRIES {
                return Err(Error::LimitExceeded(format!(
                    "IDML XML parts exceed {MAX_IDML_XML_ENTRIES}"
                )));
            }
            xml_names.push(name);
        }
    }
    if !designmap_seen {
        return Err(Error::InvalidInput(
            "IDML package has no designmap.xml".into(),
        ));
    }
    for name in xml_names {
        let data = read_entry(&mut archive, &name, &mut expanded_bytes)?;
        summary.xml_parts = summary.xml_parts.saturating_add(1);
        let root = parse_xml_tree(
            &data,
            &XmlLimits {
                max_events: options.max_xml_events.min(MAX_IDML_XML_EVENTS),
                max_nodes: MAX_IDML_XML_NODES,
                max_depth: MAX_IDML_XML_DEPTH,
                max_text_bytes: MAX_IDML_TEXT_BYTES,
            },
            "IDML XML",
        )?;
        summarize_xml(&root, &mut summary);
    }
    push_row(
        &mut summary.rows,
        "Package",
        "IDML",
        &format!("entries={} xmlParts={}", summary.entries, summary.xml_parts),
    )?;
    push_row(
        &mut summary.rows,
        "Layout",
        &summary.spreads.to_string(),
        &format!(
            "stories={} masterSpreads={}",
            summary.stories, summary.master_spreads
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Resources",
        &summary.resources.to_string(),
        &format!(
            "images={} fonts={} swatches={}",
            summary.images, summary.fonts, summary.swatches
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Text",
        &summary.paragraphs.to_string(),
        &format!(
            "characters={} storyNodes={}",
            summary.characters, summary.stories_nodes
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Links",
        &summary.hyperlinks.to_string(),
        &format!("tables={} graphics={}", summary.tables, summary.graphics),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "IDML InDesign package".into(),
        },
        HtmlBlock::Paragraph {
            text: "InDesign Markup Language package structure is summarized without rendering or extracting private layout payloads.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "IDML IDs, text contents, links, image/font payloads, color values and private layout attributes are omitted or redacted; package structure only is shown".into(),
        "IDML ZIP entries are read in memory under expanded-size limits; scripts, hyperlinks, URLs, fonts, images, external files and embedded resources are never executed or fetched".into(),
    ];
    let mut page_sink = IdmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    expanded_bytes: &mut u64,
) -> Result<Vec<u8>> {
    let mut entry = archive.by_name(name)?;
    let declared = entry.size();
    if declared > MAX_IDML_ENTRY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "IDML entry '{name}' exceeds {MAX_IDML_ENTRY_BYTES} bytes"
        )));
    }
    let mut data = Vec::with_capacity(declared.min(MAX_IDML_ENTRY_BYTES) as usize);
    entry
        .by_ref()
        .take(MAX_IDML_ENTRY_BYTES.saturating_add(1))
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_IDML_ENTRY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "IDML entry '{name}' expands beyond {MAX_IDML_ENTRY_BYTES} bytes"
        )));
    }
    *expanded_bytes = expanded_bytes.saturating_add(data.len() as u64);
    if *expanded_bytes > MAX_IDML_EXPANDED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "IDML expanded XML exceeds {MAX_IDML_EXPANDED_BYTES} bytes"
        )));
    }
    Ok(data)
}

fn validate_entry_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    if path.is_absolute()
        || name.starts_with('\\')
        || name.split('/').any(|component| component == "..")
    {
        return Err(Error::InvalidInput(format!(
            "IDML entry path escapes package root: {name}"
        )));
    }
    Ok(())
}

fn summarize_xml(root: &XmlElement, summary: &mut Summary) {
    summary.stories_nodes = summary
        .stories_nodes
        .saturating_add(usize::from(root.name.eq_ignore_ascii_case("Story")));
    summary.paragraphs = summary
        .paragraphs
        .saturating_add(usize::from(root.name.eq_ignore_ascii_case("Paragraph")));
    summary.characters = summary
        .characters
        .saturating_add(usize::from(root.name.eq_ignore_ascii_case("Character")));
    summary.hyperlinks = summary
        .hyperlinks
        .saturating_add(usize::from(root.name.eq_ignore_ascii_case("Hyperlink")));
    summary.tables = summary
        .tables
        .saturating_add(usize::from(root.name.eq_ignore_ascii_case("Table")));
    summary.graphics = summary.graphics.saturating_add(
        usize::from(root.name.eq_ignore_ascii_case("Rectangle"))
            + usize::from(root.name.eq_ignore_ascii_case("Graphic"))
            + usize::from(root.name.eq_ignore_ascii_case("Image")),
    );
    summary.swatches = summary
        .swatches
        .saturating_add(usize::from(root.name.eq_ignore_ascii_case("Color")));
    for child in &root.children {
        summarize_xml(child, summary);
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_IDML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "IDML rows exceed {MAX_IDML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_IDML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_IDML_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
