//! Bounded TermBase eXchange (TBX/MARTIF) previews.
//!
//! Terminology entries often contain proprietary definitions and references.
//! This adapter validates the TBX core structure and renders element counts and
//! language metadata while keeping term text, identifiers and links inert.

use std::collections::BTreeSet;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_TBX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TBX_EVENTS: usize = 1_000_000;
const MAX_TBX_NODES: usize = 500_000;
const MAX_TBX_DEPTH: usize = 128;
const MAX_TBX_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_TBX_ROWS: usize = 200_000;
const MAX_TBX_LANGUAGES: usize = 10_000;
const MAX_TBX_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    dialect: String,
    entries: usize,
    terms: usize,
    language_sets: usize,
    term_items: usize,
    descriptions: usize,
    administrative: usize,
    notes: usize,
    transactions: usize,
    references: usize,
    languages: BTreeSet<String>,
    rows: Vec<Vec<String>>,
}

struct TbxPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for TbxPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "tbx".into();
        if page.title.is_empty() {
            page.title = "TBX terminology base".into();
        }
        page.description =
            "TBX terminology structure is rendered as bounded inert metadata; term text and external resources are not exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"tbx", None)
        || crate::geospatial::xml_tree::looks_like_root(prefix, b"martif", None)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_TBX_BYTES),
        "TBX input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_TBX_EVENTS),
            max_nodes: MAX_TBX_NODES,
            max_depth: MAX_TBX_DEPTH,
            max_text_bytes: MAX_TBX_TEXT_BYTES,
        },
        "TBX",
    )?;
    if !root.name.eq_ignore_ascii_case("martif") && !root.name.eq_ignore_ascii_case("tbx") {
        return Err(Error::InvalidInput(
            "TBX root must be <tbx> or legacy <martif>".into(),
        ));
    }
    let dialect = attr_local(&root, "type")
        .or_else(|| attr_local(&root, "style"))
        .unwrap_or(root.name.as_str());
    let mut summary = Summary {
        dialect: truncate(dialect),
        entries: count_named(&root, "termEntry"),
        terms: count_named(&root, "term"),
        language_sets: count_named(&root, "langSet"),
        term_items: count_named(&root, "tig") + count_named(&root, "termSec"),
        descriptions: count_named(&root, "descrip") + count_named(&root, "definition"),
        administrative: count_named(&root, "admin"),
        notes: count_named(&root, "note"),
        transactions: count_named(&root, "transac") + count_named(&root, "transacGrp"),
        references: count_named(&root, "ref") + count_named(&root, "xref"),
        ..Summary::default()
    };
    collect_languages(&root, &mut summary.languages)?;
    push_row(
        &mut summary.rows,
        "Document",
        &root.name,
        &format!("dialect={}", summary.dialect),
    )?;
    push_row(
        &mut summary.rows,
        "Terminology",
        &summary.entries.to_string(),
        &format!(
            "terms={} languageSets={}",
            summary.terms, summary.language_sets
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
        &format!(
            "termItems={} descriptions={}",
            summary.term_items, summary.descriptions
        ),
        &format!(
            "admin={} notes={} transactions={} references={}",
            summary.administrative, summary.notes, summary.transactions, summary.references
        ),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "TBX terminology base".into(),
        },
        HtmlBlock::Paragraph {
            text: "TermBase eXchange entries and language structure are summarized without displaying proprietary terminology text.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "TBX terms, definitions, IDs, notes, references, URLs and proprietary data categories are omitted or redacted; only bounded counts and language tags are shown".into(),
        "TBX DTDs, XCS/Schematron resources, external entities, references and terminology services are never loaded or executed".into(),
    ];
    let mut page_sink = TbxPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_languages(element: &XmlElement, languages: &mut BTreeSet<String>) -> Result<()> {
    if (element.name.eq_ignore_ascii_case("langSet") || element.name.eq_ignore_ascii_case("lang"))
        && let Some(language) =
            attr_local(element, "lang").or_else(|| attr_local(element, "xml:lang"))
    {
        let language = truncate(language);
        if !language.is_empty() {
            if languages.len() >= MAX_TBX_LANGUAGES && !languages.contains(&language) {
                return Err(Error::LimitExceeded(format!(
                    "TBX language tags exceed {MAX_TBX_LANGUAGES}"
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
    if rows.len() >= MAX_TBX_ROWS {
        return Err(Error::LimitExceeded(format!(
            "TBX rows exceed {MAX_TBX_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_TBX_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_TBX_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
