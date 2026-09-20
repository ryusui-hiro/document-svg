//! Bounded Translation Memory eXchange (TMX) previews.
//!
//! TMX translation units and language variants are useful document metadata,
//! but segment text may contain confidential source material. This adapter
//! validates the bounded XML structure and renders counts and language
//! metadata only; it never exposes segment values or follows external links.

use std::collections::BTreeSet;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_TMX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TMX_EVENTS: usize = 1_000_000;
const MAX_TMX_NODES: usize = 500_000;
const MAX_TMX_DEPTH: usize = 128;
const MAX_TMX_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_TMX_ROWS: usize = 200_000;
const MAX_TMX_LANGUAGES: usize = 10_000;
const MAX_TMX_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    version: String,
    units: usize,
    variants: usize,
    segments: usize,
    headers: usize,
    bodies: usize,
    notes: usize,
    properties: usize,
    languages: BTreeSet<String>,
    rows: Vec<Vec<String>>,
}

struct TmxPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for TmxPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "tmx".into();
        if page.title.is_empty() {
            page.title = "TMX translation memory".into();
        }
        page.description =
            "TMX translation-memory structure is rendered as bounded inert metadata; segment text and external resources are not exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"tmx", None)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_TMX_BYTES),
        "TMX input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_TMX_EVENTS),
            max_nodes: MAX_TMX_NODES,
            max_depth: MAX_TMX_DEPTH,
            max_text_bytes: MAX_TMX_TEXT_BYTES,
        },
        "TMX",
    )?;
    if !root.name.eq_ignore_ascii_case("tmx") {
        return Err(Error::InvalidInput("TMX root must be <tmx>".into()));
    }
    let version = root.attribute("version").unwrap_or_default();
    if version.is_empty() {
        return Err(Error::InvalidInput(
            "TMX root must declare a version".into(),
        ));
    }
    let mut summary = Summary {
        version: truncate(version),
        units: count_named(&root, "tu"),
        variants: count_named(&root, "tuv"),
        segments: count_named(&root, "seg"),
        headers: count_named(&root, "header"),
        bodies: count_named(&root, "body"),
        notes: count_named(&root, "note"),
        properties: count_named(&root, "prop"),
        ..Summary::default()
    };
    collect_languages(&root, &mut summary.languages)?;
    push_row(
        &mut summary.rows,
        "Document",
        "TMX",
        &format!("version={}", summary.version),
    )?;
    push_row(
        &mut summary.rows,
        "Translation units",
        &summary.units.to_string(),
        &format!(
            "variants={} segments={}",
            summary.variants, summary.segments
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Languages",
        &summary.languages.len().to_string(),
        &display_languages(&summary.languages),
    )?;
    push_row(
        &mut summary.rows,
        "Metadata",
        &format!("headers={} bodies={}", summary.headers, summary.bodies),
        &format!("notes={} properties={}", summary.notes, summary.properties),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "TMX translation memory".into(),
        },
        HtmlBlock::Paragraph {
            text: "Translation Memory eXchange units and language variants are summarized without displaying confidential segment text.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "TMX segment text, IDs, notes, properties, URLs and proprietary metadata are omitted or redacted; only bounded counts and language tags are shown".into(),
        "TMX DTDs, external entities, skeleton files, references and translation services are never loaded or executed".into(),
    ];
    let mut page_sink = TmxPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_languages(element: &XmlElement, languages: &mut BTreeSet<String>) -> Result<()> {
    if element.name.eq_ignore_ascii_case("tuv")
        && let Some(language) = attr_local(element, "lang")
    {
        let language = truncate(language);
        if !language.is_empty() {
            if languages.len() >= MAX_TMX_LANGUAGES && !languages.contains(&language) {
                return Err(Error::LimitExceeded(format!(
                    "TMX language tags exceed {MAX_TMX_LANGUAGES}"
                )));
            }
            languages.insert(language);
        }
    }
    for child in &element.children {
        collect_languages(child, languages)?;
    }
    Ok(())
}

fn count_named(element: &XmlElement, name: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| usize::from(child.name.eq_ignore_ascii_case(name)) + count_named(child, name))
        .sum()
}

fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
}

fn display_languages(languages: &BTreeSet<String>) -> String {
    if languages.is_empty() {
        return "none".into();
    }
    let mut values = languages.iter().take(16).cloned().collect::<Vec<_>>();
    if languages.len() > values.len() {
        values.push(format!("+{} more", languages.len() - values.len()));
    }
    values.join(", ")
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_TMX_ROWS {
        return Err(Error::LimitExceeded(format!(
            "TMX rows exceed {MAX_TMX_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_TMX_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_TMX_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
