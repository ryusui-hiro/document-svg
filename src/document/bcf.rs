//! Bounded BIM Collaboration Format (BCFZIP/BCF-XML) issue previews.
//!
//! BCF packages exchange coordination topics, markups and viewpoints alongside
//! IFC models. This adapter reads only small XML metadata entries; snapshots,
//! referenced models, document URLs and collaboration actions remain inert.

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_BCF_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BCF_EXPANDED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BCF_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_BCF_ENTRIES: usize = 100_000;
const MAX_BCF_XML_EVENTS: usize = 500_000;
const MAX_BCF_XML_NODES: usize = 300_000;
const MAX_BCF_XML_DEPTH: usize = 96;
const MAX_BCF_TEXT_BYTES: usize = 24 * 1024 * 1024;
const MAX_BCF_ROWS: usize = 100_000;
const MAX_BCF_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_archive(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if metadata.len() > MAX_BCF_BYTES {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut signature = [0_u8; 4];
    if file.read_exact(&mut signature).is_err() || signature != [0x50, 0x4b, 0x03, 0x04] {
        return false;
    }
    let Ok(bytes) = read_limited_file(path, MAX_BCF_BYTES, "BCFZIP sniff") else {
        return false;
    };
    let Ok(mut archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    if archive.len() > MAX_BCF_ENTRIES {
        return false;
    }
    let mut has_version = false;
    let mut has_markup = false;
    for index in 0..archive.len() {
        let Ok(entry) = archive.by_index(index) else {
            return false;
        };
        let name = entry.name().to_ascii_lowercase();
        has_version |= name == "bcf.version";
        has_markup |= name.ends_with("markup.bcf");
        if has_version && has_markup {
            return true;
        }
    }
    false
}

struct BcfPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for BcfPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "bcfzip".into();
        if page.title.is_empty() {
            page.title = "BCF issue package".into();
        }
        page.description = "BCF topics and markup metadata are rendered as a bounded inert summary; snapshots, models and external references are not opened".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    version: String,
    project: String,
    topics: usize,
    comments: usize,
    viewpoints: usize,
    document_refs: usize,
    components: usize,
    snapshots: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_BCF_BYTES),
        "BCFZIP input",
    )?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| Error::InvalidInput(format!("invalid BCFZIP archive: {error}")))?;
    if archive.len() > MAX_BCF_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "BCFZIP entries exceed {MAX_BCF_ENTRIES}"
        )));
    }
    let mut summary = Summary::default();
    let mut markups = Vec::new();
    let mut has_version = false;
    let mut expanded_bytes = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            Error::InvalidInput(format!("invalid BCFZIP entry {index}: {error}"))
        })?;
        let name = entry.name().to_owned();
        let lower = name.to_ascii_lowercase();
        if lower == "bcf.version" {
            has_version = true;
            if entry.size() > MAX_BCF_ENTRY_BYTES {
                return Err(Error::LimitExceeded(
                    "BCF.version exceeds entry limit".into(),
                ));
            }
            reserve_expanded(&mut expanded_bytes, entry.size())?;
            let mut data = Vec::new();
            entry
                .take(MAX_BCF_ENTRY_BYTES.saturating_add(1))
                .read_to_end(&mut data)?;
            if data.len() as u64 > MAX_BCF_ENTRY_BYTES {
                return Err(Error::LimitExceeded(
                    "BCF.version exceeds entry limit".into(),
                ));
            }
            summary.version = truncate(&String::from_utf8_lossy(&data));
        } else if lower.ends_with("project.bcfp") {
            reserve_expanded(&mut expanded_bytes, entry.size())?;
            let data = read_entry(&mut entry, &name)?;
            if let Ok(root) = parse_bcf_xml(&data, options) {
                summary.project = first_text(&root, "Name");
            }
        } else if lower.ends_with("markup.bcf") {
            reserve_expanded(&mut expanded_bytes, entry.size())?;
            let data = read_entry(&mut entry, &name)?;
            markups.push((name, data));
        } else if lower.ends_with("snapshot.png")
            || lower.ends_with("snapshot.jpg")
            || lower.ends_with("snapshot.jpeg")
        {
            summary.snapshots = summary.snapshots.saturating_add(1);
        }
    }
    if !has_version {
        return Err(Error::InvalidInput(
            "BCFZIP is missing the root bcf.version entry".into(),
        ));
    }
    for (name, data) in markups {
        let root = parse_bcf_xml(&data, options).map_err(|error| {
            Error::InvalidInput(format!("BCF markup {name} is invalid: {error}"))
        })?;
        if !root.name.eq_ignore_ascii_case("Markup") {
            return Err(Error::InvalidInput(format!(
                "BCF markup {name} must have a Markup root"
            )));
        }
        summary.topics = summary.topics.saturating_add(count_named(&root, "Topic"));
        summary.comments = summary
            .comments
            .saturating_add(count_named_with_attr(&root, "Comment", "Guid"));
        summary.viewpoints = summary
            .viewpoints
            .saturating_add(count_named(&root, "Viewpoint"));
        summary.document_refs = summary
            .document_refs
            .saturating_add(count_named(&root, "DocumentReference"));
        summary.components = summary
            .components
            .saturating_add(count_named(&root, "Component"));
        let topic = descendants_named(&root, "Topic").first().copied();
        let title = topic
            .map(|node| first_text(node, "Title"))
            .unwrap_or_default();
        let topic_type = topic
            .and_then(|node| attr_local(node, "TopicType"))
            .map(str::to_owned)
            .unwrap_or_default();
        let status = topic
            .and_then(|node| attr_local(node, "TopicStatus"))
            .map(str::to_owned)
            .unwrap_or_default();
        let priority = topic
            .and_then(|node| attr_local(node, "Priority"))
            .map(str::to_owned)
            .unwrap_or_default();
        push_row(
            &mut summary.rows,
            "Topic",
            &title,
            &format!(
                "type={} status={} priority={}",
                display_or_dash(&topic_type),
                display_or_dash(&status),
                display_or_dash(&priority)
            ),
        )?;
    }
    if summary.topics == 0 {
        return Err(Error::InvalidInput(
            "BCFZIP contains no markup topics".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Package",
        "BCFZIP",
        &format!(
            "version={} project={}",
            display_or_dash(&summary.version),
            display_or_dash(&summary.project)
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Topics",
        &summary.topics.to_string(),
        &format!(
            "comments={} viewpoints={}",
            summary.comments, summary.viewpoints
        ),
    )?;
    push_row(
        &mut summary.rows,
        "References",
        &summary.document_refs.to_string(),
        &format!(
            "components={} snapshots={}",
            summary.components, summary.snapshots
        ),
    )?;
    let metadata = format!(
        "BCF version: {}\nProject: {}\nTopics: {}\nComments: {}\nViewpoints: {}\nDocument references: {}\nComponents: {}\nSnapshots skipped: {}",
        display_or_dash(&summary.version),
        display_or_dash(&summary.project),
        summary.topics,
        summary.comments,
        summary.viewpoints,
        summary.document_refs,
        summary.components,
        summary.snapshots
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "BCF issue package".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "BCF version/project/topic status/title/priority and comment/viewpoint/reference/component counts are shown; snapshot images, IFC/model references, document URLs, GUID payloads and markup extensions are omitted or redacted".into(),
        "BCFZIP entry and XML traversal are bounded; external models, URI dereferencing, collaboration API calls, scripts and issue mutations never run".into(),
    ];
    let mut page_sink = BcfPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn read_entry<R: Read>(entry: &mut zip::read::ZipFile<'_, R>, name: &str) -> Result<Vec<u8>> {
    if entry.size() > MAX_BCF_ENTRY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "BCF entry {name} exceeds {MAX_BCF_ENTRY_BYTES} bytes"
        )));
    }
    let mut data = Vec::new();
    entry
        .take(MAX_BCF_ENTRY_BYTES.saturating_add(1))
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_BCF_ENTRY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "BCF entry {name} exceeds {MAX_BCF_ENTRY_BYTES} bytes"
        )));
    }
    Ok(data)
}
fn reserve_expanded(total: &mut u64, entry_size: u64) -> Result<()> {
    let next = total
        .checked_add(entry_size)
        .ok_or_else(|| Error::LimitExceeded("BCFZIP expanded entry size overflow".into()))?;
    if next > MAX_BCF_EXPANDED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "BCFZIP expanded entries exceed {MAX_BCF_EXPANDED_BYTES} bytes"
        )));
    }
    *total = next;
    Ok(())
}
fn parse_bcf_xml(bytes: &[u8], options: &ConvertOptions) -> Result<XmlElement> {
    parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_BCF_XML_EVENTS),
            max_nodes: MAX_BCF_XML_NODES,
            max_depth: MAX_BCF_XML_DEPTH,
            max_text_bytes: MAX_BCF_TEXT_BYTES,
        },
        "BCF",
    )
}
fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
}
fn first_text(element: &XmlElement, name: &str) -> String {
    descendants_named(element, name)
        .first()
        .map(|node| truncate(&text_content(node)))
        .unwrap_or_default()
}
fn count_named(element: &XmlElement, name: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| usize::from(child.name.eq_ignore_ascii_case(name)) + count_named(child, name))
        .sum()
}
fn count_named_with_attr(element: &XmlElement, name: &str, attr: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| {
            usize::from(child.name.eq_ignore_ascii_case(name) && attr_local(child, attr).is_some())
                + count_named_with_attr(child, name, attr)
        })
        .sum()
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
fn text_content(element: &XmlElement) -> String {
    let mut parts = Vec::new();
    if !element.text.trim().is_empty() {
        parts.push(element.text.trim().to_owned());
    }
    for child in &element.children {
        let value = text_content(child);
        if !value.is_empty() {
            parts.push(value);
        }
    }
    parts.join(" ")
}
fn display_or_dash(value: &str) -> String {
    if value.is_empty() {
        "-".into()
    } else {
        truncate(value)
    }
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_BCF_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_BCF_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_BCF_ROWS {
        return Err(Error::LimitExceeded(format!(
            "BCF rows exceed {MAX_BCF_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::looks_like_archive;
    #[test]
    fn missing_archive_is_not_bcf() {
        assert!(!looks_like_archive(std::path::Path::new(
            "does-not-exist.bcfzip"
        )));
    }
}
