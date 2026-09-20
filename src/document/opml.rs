//! Bounded OPML 1.0/2.0 outline previews.
//!
//! OPML is an XML interchange format for ordered, hierarchical outlines and
//! feed subscription lists. This adapter renders outline text and inert type
//! metadata only; feed URLs, HTML links, owner addresses and external
//! resources are never displayed, resolved, or fetched.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_OPML_BYTES: u64 = 32 * 1024 * 1024;
const MAX_OPML_XML_EVENTS: usize = 500_000;
const MAX_OPML_XML_NODES: usize = 300_000;
const MAX_OPML_XML_DEPTH: usize = 96;
const MAX_OPML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_OPML_ROWS: usize = 200_000;
const MAX_OPML_DISPLAY_BYTES: usize = 512;

/// Content sniffing for extensionless XML. OPML 1.0/2.0 has an unnamespaced
/// `opml` root with a required version attribute and head/body children.
pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    if !crate::geospatial::xml_tree::looks_like_root(bytes, b"opml", None) {
        return false;
    }
    let text = String::from_utf8_lossy(bytes);
    let lower = text.to_ascii_lowercase();
    let version = lower.contains("version=\"1.0\"")
        || lower.contains("version='1.0'")
        || lower.contains("version=\"2.0\"")
        || lower.contains("version='2.0'");
    version && has_tag(&lower, "head") && has_tag(&lower, "body")
}

fn has_tag(text: &str, tag: &str) -> bool {
    let needle = format!("<{tag}");
    text.match_indices(&needle).any(|(index, _)| {
        text.as_bytes()
            .get(index.saturating_add(needle.len()))
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'))
    })
}

struct OpmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OpmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "opml".into();
        if page.title.is_empty() {
            page.title = "OPML outline".into();
        }
        page.description =
            "OPML outline and feed metadata is rendered inertly; URLs, owner addresses and external resources are not resolved or fetched".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    version: String,
    title: String,
    date_created: String,
    outlines: usize,
    feeds: usize,
    max_depth: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_OPML_BYTES),
        "OPML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_OPML_XML_EVENTS),
            max_nodes: MAX_OPML_XML_NODES,
            max_depth: MAX_OPML_XML_DEPTH,
            max_text_bytes: MAX_OPML_TEXT_BYTES,
        },
        "OPML",
    )?;
    if root.name != "opml" {
        return Err(Error::InvalidInput("OPML XML root must be opml".into()));
    }
    let version = root.attribute("version").unwrap_or_default();
    if version != "1.0" && version != "2.0" {
        return Err(Error::Unsupported(format!(
            "OPML version '{version}' is unsupported; expected 1.0 or 2.0"
        )));
    }
    let head = root.children_named("head").next();
    let body = root
        .children_named("body")
        .next()
        .ok_or_else(|| Error::InvalidInput("OPML document requires a body element".into()))?;
    let mut summary = Summary {
        version: version.to_owned(),
        ..Summary::default()
    };
    if let Some(head) = head {
        summary.title = child_text(head, "title");
        summary.date_created = child_text(head, "dateCreated");
    }
    for outline in body.children_named("outline") {
        collect_outline(outline, 0, &mut summary)?;
    }
    let mut warnings = vec![
        "OPML outline text and type metadata are shown; feed URLs, HTML links, owner email, descriptions and arbitrary extension values are omitted".into(),
        "OPML XML traversal and rendered rows are bounded; no linked feed, enclosure, image, script or external resource is fetched or executed".into(),
    ];
    if summary.outlines == 0 {
        warnings.push("OPML body contains no outline elements".into());
    }
    let metadata = format!(
        "Version: {}\nTitle: {}\nCreated: {}\nOutlines: {}\nFeed outlines: {}\nMaximum depth: {}",
        summary.version,
        display_or_dash(&summary.title),
        display_or_dash(&summary.date_created),
        summary.outlines,
        summary.feeds,
        summary.max_depth,
    );
    let rows = if summary.rows.is_empty() {
        vec![vec!["—".into(), "—".into(), "0".into(), "outline".into()]]
    } else {
        summary.rows
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "OPML outline".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Outline".into(),
                "Type".into(),
                "Depth".into(),
                "Kind".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = OpmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(std::mem::take(&mut warnings))
}

fn child_text(parent: &XmlElement, name: &str) -> String {
    parent
        .children_named(name)
        .next()
        .map(|child| truncate(child.text.trim()))
        .unwrap_or_default()
}

fn collect_outline(element: &XmlElement, depth: usize, summary: &mut Summary) -> Result<()> {
    if summary.rows.len() >= MAX_OPML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "OPML outline rows exceed {MAX_OPML_ROWS}"
        )));
    }
    let level = depth.saturating_add(1);
    summary.outlines = summary.outlines.saturating_add(1);
    summary.max_depth = summary.max_depth.max(level);
    let outline_type = element.attribute("type").unwrap_or("outline");
    let is_feed = outline_type.eq_ignore_ascii_case("rss")
        || outline_type.eq_ignore_ascii_case("atom")
        || element.attribute("xmlUrl").is_some();
    if is_feed {
        summary.feeds = summary.feeds.saturating_add(1);
    }
    let text = element
        .attribute("text")
        .or_else(|| element.attribute("title"))
        .unwrap_or("—");
    let indent = "  ".repeat(depth.min(32));
    summary.rows.push(vec![
        format!("{indent}{}", truncate(text)),
        truncate(outline_type),
        level.to_string(),
        if is_feed {
            "feed".into()
        } else {
            "outline".into()
        },
    ]);
    for child in element.children_named("outline") {
        collect_outline(child, level, summary)?;
    }
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OPML_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_OPML_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_opml_roots_with_required_shape() {
        assert!(looks_like_prefix(
            br#"<?xml version="1.0"?><opml version="2.0"><head/><body><outline text="x"/></body></opml>"#
        ));
        assert!(!looks_like_prefix(br#"<opml><head/><body/></opml>"#));
        assert!(!looks_like_prefix(
            br#"<opml version="2.0"><header/><body/></opml>"#
        ));
        assert!(!looks_like_prefix(
            br#"<description><head/><body/></description>"#
        ));
    }

    #[test]
    fn counts_nested_outlines_and_feeds_without_urls() {
        let xml = br#"<opml version="2.0"><head><title>Feeds</title></head><body><outline text="News"><outline text="Private" type="rss" xmlUrl="https://private.example.invalid/feed"/></outline></body></opml>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "OPML",
        )
        .unwrap();
        let body = root.children_named("body").next().unwrap();
        let mut summary = Summary::default();
        collect_outline(
            body.children_named("outline").next().unwrap(),
            0,
            &mut summary,
        )
        .unwrap();
        assert_eq!(summary.outlines, 2);
        assert_eq!(summary.feeds, 1);
        assert_eq!(summary.max_depth, 2);
        assert!(
            !summary
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("private.example"))
        );
    }
}
