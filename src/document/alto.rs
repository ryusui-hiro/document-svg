//! Bounded ALTO OCR/layout previews.
//!
//! ALTO stores physical page layout and OCR text. This adapter keeps the
//! useful page/line/word structure and confidence values, while omitting
//! source-image filenames, external links, styles and processing payloads.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_ALTO_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ALTO_XML_EVENTS: usize = 1_000_000;
const MAX_ALTO_XML_NODES: usize = 500_000;
const MAX_ALTO_XML_DEPTH: usize = 96;
const MAX_ALTO_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_ALTO_ROWS: usize = 200_000;
const MAX_ALTO_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"alto", None);
    if !root {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<layout") && (text.contains("<page") || text.contains("<string"))
}

struct AltoPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for AltoPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "alto".into();
        if page.title.is_empty() {
            page.title = "ALTO OCR layout".into();
        }
        page.description =
            "ALTO page, line and OCR-word metadata is rendered inertly; source images, external resources and processing payloads are not opened".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    version: String,
    pages: usize,
    lines: usize,
    strings: usize,
    confidence_sum: f64,
    confidence_count: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_ALTO_BYTES),
        "ALTO input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_ALTO_XML_EVENTS),
            max_nodes: MAX_ALTO_XML_NODES,
            max_depth: MAX_ALTO_XML_DEPTH,
            max_text_bytes: MAX_ALTO_TEXT_BYTES,
        },
        "ALTO",
    )?;
    if !root.name.eq_ignore_ascii_case("alto") {
        return Err(Error::InvalidInput("ALTO XML root must be alto".into()));
    }
    let mut summary = Summary {
        version: root
            .attribute("VERSION")
            .or_else(|| root.attribute("version"))
            .unwrap_or("—")
            .to_owned(),
        ..Summary::default()
    };
    for page in descendants_named(&root, "Page") {
        collect_page(page, &mut summary)?;
    }
    if summary.pages == 0 {
        return Err(Error::InvalidInput(
            "ALTO document contains no Page elements".into(),
        ));
    }
    let average = if summary.confidence_count == 0 {
        "—".into()
    } else {
        format!(
            "{:.3}",
            summary.confidence_sum / summary.confidence_count as f64
        )
    };
    let metadata = format!(
        "Version: {}\nPages: {}\nText lines: {}\nOCR strings: {}\nAverage confidence: {}",
        summary.version, summary.pages, summary.lines, summary.strings, average
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "ALTO OCR layout".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Kind".into(),
                "Page".into(),
                "Text".into(),
                "Confidence".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "ALTO page, line and OCR-string metadata are shown; source image filenames, URLs, styles, alternatives and processing details are omitted".into(),
        "ALTO XML traversal and rendered rows are bounded; no image, script, external resource or OCR process is executed".into(),
    ];
    let mut page_sink = AltoPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_page(page: &XmlElement, summary: &mut Summary) -> Result<()> {
    summary.pages = summary.pages.saturating_add(1);
    let page_label = page
        .attribute("PHYSICAL_IMG_NR")
        .or_else(|| page.attribute("ID"))
        .unwrap_or("—");
    push_row(summary, "Page", page_label, "layout".into(), "—".into())?;
    for line in descendants_named(page, "TextLine") {
        summary.lines = summary.lines.saturating_add(1);
        let words: Vec<&XmlElement> = descendants_named(line, "String");
        let mut text = String::new();
        let mut line_sum = 0.0;
        let mut line_count = 0usize;
        for word in words {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(word.attribute("CONTENT").unwrap_or("—"));
            summary.strings = summary.strings.saturating_add(1);
            if let Some(confidence) = word
                .attribute("WC")
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
            {
                summary.confidence_sum += confidence;
                summary.confidence_count = summary.confidence_count.saturating_add(1);
                line_sum += confidence;
                line_count = line_count.saturating_add(1);
            }
        }
        let confidence = if line_count == 0 {
            "—".into()
        } else {
            format!("{:.3}", line_sum / line_count as f64)
        };
        push_row(summary, "Line", page_label, truncate(&text), confidence)?;
    }
    Ok(())
}

fn descendants_named<'a>(element: &'a XmlElement, name: &str) -> Vec<&'a XmlElement> {
    let mut result = Vec::new();
    for child in &element.children {
        if child.name.eq_ignore_ascii_case(name) {
            result.push(child);
        }
        result.extend(descendants_named(child, name));
    }
    result
}

fn push_row(
    summary: &mut Summary,
    kind: &str,
    page: &str,
    text: String,
    confidence: String,
) -> Result<()> {
    if summary.rows.len() >= MAX_ALTO_ROWS {
        return Err(Error::LimitExceeded(format!(
            "ALTO rendered rows exceed {MAX_ALTO_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(kind),
        truncate(page),
        truncate(&text),
        truncate(&confidence),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_ALTO_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_ALTO_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_alto_layout_root() {
        assert!(looks_like_prefix(
            br#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Layout><Page><TextLine><String CONTENT="x"/></TextLine></Page></Layout></alto>"#
        ));
        assert!(!looks_like_prefix(br#"<alto><Description/></alto>"#));
    }

    #[test]
    fn collects_lines_and_confidence() {
        let xml = br#"<alto VERSION="3.1"><Layout><Page PHYSICAL_IMG_NR="1"><PrintSpace><TextBlock><TextLine><String CONTENT="Hello" WC="0.9"/><SP/><String CONTENT="world" WC="1.0"/></TextLine></TextBlock></PrintSpace></Page></Layout></alto>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "ALTO",
        )
        .unwrap();
        let mut summary = Summary::default();
        collect_page(descendants_named(&root, "Page")[0], &mut summary).unwrap();
        assert_eq!(summary.pages, 1);
        assert_eq!(summary.lines, 1);
        assert_eq!(summary.strings, 2);
        assert_eq!(summary.confidence_count, 2);
        assert!(summary.rows[1][2].contains("Hello world"));
    }
}
