//! Bounded RSS 2.0 / Atom 1.0 syndication-feed previews.
//!
//! RSS and Atom entries can contain HTML, links, enclosures, scripts and
//! private author data. This adapter renders title/date and structural counts
//! only; URLs, content payloads and external resources remain inert.

use std::io::Cursor;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_FEED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_FEED_XML_EVENTS: usize = 500_000;
const MAX_FEED_XML_NODES: usize = 300_000;
const MAX_FEED_XML_DEPTH: usize = 96;
const MAX_FEED_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_FEED_ROWS: usize = 100_000;
const MAX_FEED_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let mut reader = quick_xml::Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Start(element))
            | Ok(quick_xml::events::Event::Empty(element)) => {
                let raw_name = element.name();
                let name = crate::ooxml::local_name(raw_name.as_ref());
                let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
                return if name.eq_ignore_ascii_case(b"feed") {
                    has_tag(&text, "title") || has_tag(&text, "entry")
                } else if name.eq_ignore_ascii_case(b"rss") || name.eq_ignore_ascii_case(b"rdf") {
                    has_tag(&text, "channel") || has_tag(&text, "item")
                } else {
                    false
                };
            }
            Ok(quick_xml::events::Event::DocType(_)) => return false,
            Ok(quick_xml::events::Event::Eof) | Err(_) => return false,
            _ => buffer.clear(),
        }
        buffer.clear();
    }
}

fn has_tag(text: &str, tag: &str) -> bool {
    let needle = format!("<{tag}");
    text.match_indices(&needle).any(|(index, _)| {
        text.as_bytes()
            .get(index.saturating_add(needle.len()))
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'))
    })
}

struct FeedPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for FeedPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "feed".into();
        if page.title.is_empty() {
            page.title = "RSS/Atom feed".into();
        }
        page.description =
            "RSS/Atom entry metadata is rendered inertly; links, content, enclosures and external resources are not resolved or fetched".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    kind: String,
    version: String,
    title: String,
    updated: String,
    items: usize,
    linked_items: usize,
    authored_items: usize,
    content_items: usize,
    enclosures: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_FEED_BYTES),
        "RSS/Atom feed input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_FEED_XML_EVENTS),
            max_nodes: MAX_FEED_XML_NODES,
            max_depth: MAX_FEED_XML_DEPTH,
            max_text_bytes: MAX_FEED_TEXT_BYTES,
        },
        "RSS/Atom feed",
    )?;
    let is_atom = root.name.eq_ignore_ascii_case("feed");
    let is_rss = root.name.eq_ignore_ascii_case("rss") || root.name.eq_ignore_ascii_case("rdf");
    if !is_atom && !is_rss {
        return Err(Error::InvalidInput(
            "RSS/Atom root must be rss, rdf:RDF, or feed".into(),
        ));
    }
    let mut summary = Summary {
        kind: if is_atom { "Atom".into() } else { "RSS".into() },
        version: root.attribute("version").unwrap_or_default().to_owned(),
        ..Summary::default()
    };
    let container = if is_atom {
        &root
    } else {
        root.children_named("channel").next().unwrap_or(&root)
    };
    summary.title = child_text(container, "title");
    summary.updated = if is_atom {
        first_child_text(container, &["updated", "published"])
    } else {
        first_child_text(container, &["lastBuildDate", "pubDate"])
    };
    let item_name = if is_atom { "entry" } else { "item" };
    for item in container.children_named(item_name) {
        collect_item(item, is_atom, &mut summary)?;
    }
    if !is_atom && root.name.eq_ignore_ascii_case("rdf") {
        for item in root.children_named("item") {
            collect_item(item, false, &mut summary)?;
        }
    }
    let mut warnings = vec![
        "RSS/Atom titles and structural metadata are shown; links, descriptions, summaries, content, enclosures, author addresses and extension values are omitted".into(),
        "RSS/Atom links, enclosures, images, scripts, stylesheets and external resources are never fetched, opened or executed".into(),
        "RSS/Atom XML traversal and rendered rows are bounded; no feed refresh or network operation runs".into(),
    ];
    if summary.items == 0 {
        warnings.push("feed contains no item/entry elements".into());
    }
    let version = if summary.version.is_empty() {
        "—"
    } else {
        summary.version.as_str()
    };
    let metadata = format!(
        "Format: {}\nVersion: {}\nTitle: {}\nUpdated: {}\nItems/entries: {}\nWith links: {}\nWith authors: {}\nWith content: {}\nEnclosures: {}",
        summary.kind,
        version,
        display_or_dash(&summary.title),
        display_or_dash(&summary.updated),
        summary.items,
        summary.linked_items,
        summary.authored_items,
        summary.content_items,
        summary.enclosures,
    );
    let rows = if summary.rows.is_empty() {
        vec![vec!["—".into(), "—".into(), "—".into(), "0".into()]]
    } else {
        summary.rows
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "RSS/Atom feed".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "No.".into(),
                "Title".into(),
                "Date".into(),
                "Signals".into(),
            ],
            rows,
            alignments: vec![
                TableAlign::Right,
                TableAlign::Left,
                TableAlign::Left,
                TableAlign::Left,
            ],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = FeedPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(std::mem::take(&mut warnings))
}

fn collect_item(element: &XmlElement, is_atom: bool, summary: &mut Summary) -> Result<()> {
    if summary.rows.len() >= MAX_FEED_ROWS {
        return Err(Error::LimitExceeded(format!(
            "RSS/Atom rows exceed {MAX_FEED_ROWS}"
        )));
    }
    summary.items = summary.items.saturating_add(1);
    let title = child_text(element, "title");
    let date = if is_atom {
        first_child_text(element, &["updated", "published"])
    } else {
        first_child_text(element, &["pubDate", "date"])
    };
    let link_count = element.children_named("link").count();
    let author_count = element.children_named("author").count()
        + element.children_named("creator").count()
        + element.children_named("dc:creator").count();
    let content_count = if is_atom {
        element.children_named("summary").count() + element.children_named("content").count()
    } else {
        element.children_named("description").count()
            + element.children_named("encoded").count()
            + element.children_named("content").count()
    };
    let enclosure_count = element.children_named("enclosure").count()
        + element
            .children_named("link")
            .filter(|link| link.attribute("rel") == Some("enclosure"))
            .count();
    if link_count > 0 {
        summary.linked_items = summary.linked_items.saturating_add(1);
    }
    if author_count > 0 {
        summary.authored_items = summary.authored_items.saturating_add(1);
    }
    if content_count > 0 {
        summary.content_items = summary.content_items.saturating_add(1);
    }
    summary.enclosures = summary.enclosures.saturating_add(enclosure_count);
    let mut signals = Vec::new();
    if link_count > 0 {
        signals.push(format!("links:{link_count}"));
    }
    if author_count > 0 {
        signals.push("author".into());
    }
    if content_count > 0 {
        signals.push("content".into());
    }
    if enclosure_count > 0 {
        signals.push(format!("enclosure:{enclosure_count}"));
    }
    summary.rows.push(vec![
        summary.items.to_string(),
        display_or_dash(&truncate(&title)).to_owned(),
        display_or_dash(&truncate(&date)).to_owned(),
        if signals.is_empty() {
            "—".into()
        } else {
            signals.join(", ")
        },
    ]);
    Ok(())
}

fn first_child_text(parent: &XmlElement, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|name| {
            parent
                .children_named(name)
                .next()
                .map(|child| truncate(child.text.trim()))
        })
        .unwrap_or_default()
}

fn child_text(parent: &XmlElement, name: &str) -> String {
    parent
        .children_named(name)
        .next()
        .map(|child| truncate(child.text.trim()))
        .unwrap_or_default()
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_FEED_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_FEED_DISPLAY_BYTES;
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
    fn recognizes_rss_atom_and_rejects_generic_xml() {
        assert!(looks_like_prefix(
            br#"<rss version="2.0"><channel><title>Feed</title></channel></rss>"#
        ));
        assert!(looks_like_prefix(
            br#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Feed</title><entry/></feed>"#
        ));
        assert!(!looks_like_prefix(
            br#"<document><channel/><item/></document>"#
        ));
    }

    #[test]
    fn counts_entries_and_omits_sensitive_values() {
        let xml = br#"<rss version="2.0"><channel><title>News</title><item><title>One</title><link>https://private.example.invalid/one</link><description>secret body</description><enclosure url="https://private.example.invalid/a.mp3"/></item></channel></rss>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "RSS/Atom feed",
        )
        .unwrap();
        let channel = root.children_named("channel").next().unwrap();
        let mut summary = Summary::default();
        collect_item(
            channel.children_named("item").next().unwrap(),
            false,
            &mut summary,
        )
        .unwrap();
        assert_eq!(summary.items, 1);
        assert_eq!(summary.linked_items, 1);
        assert_eq!(summary.content_items, 1);
        assert_eq!(summary.enclosures, 1);
        assert!(
            !summary
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("private.example"))
        );
        assert!(
            !summary
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret body"))
        );
    }
}
