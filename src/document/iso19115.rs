//! Bounded ISO 19115/19139 geographic metadata previews.
//!
//! ISO 19115 metadata is commonly encoded as ISO 19139 XML (`gmd:MD_Metadata`).
//! This adapter exposes concise identification, reference-system, extent,
//! quality and distribution counts while keeping contacts, URLs and descriptive
//! payloads inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_ISO19115_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ISO19115_XML_EVENTS: usize = 1_000_000;
const MAX_ISO19115_XML_NODES: usize = 500_000;
const MAX_ISO19115_XML_DEPTH: usize = 96;
const MAX_ISO19115_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_ISO19115_ROWS: usize = 200_000;
const MAX_ISO19115_DISPLAY_BYTES: usize = 512;

/// Conservative content sniffing for ISO 19115/19139 XML.
pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    if !crate::geospatial::xml_tree::looks_like_root(bytes, b"MD_Metadata", None) {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    (text.contains("isotc211.org/2005/gmd")
        || text.contains("standards.iso.org/iso/19115")
        || text.contains("www.isotc211.org/2005/gmd"))
        && (text.contains("identificationinfo")
            || text.contains("referencesysteminfo")
            || text.contains("dataqualityinfo"))
}

struct Iso19115PageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for Iso19115PageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "iso19115".into();
        if page.title.is_empty() {
            page.title = "ISO 19115 metadata".into();
        }
        page.description =
            "ISO 19115/19139 metadata is rendered as a bounded inert summary; contacts, URLs and descriptive payloads are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    title: String,
    dates: Vec<String>,
    hierarchy_levels: Vec<String>,
    language: String,
    character_set: String,
    topics: Vec<String>,
    crs: Vec<String>,
    bbox: Option<[f64; 4]>,
    identification_info: usize,
    reference_system_info: usize,
    data_quality_info: usize,
    distribution_info: usize,
    contacts: usize,
    online_resources: usize,
    constraints: usize,
    graphic_overviews: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_ISO19115_BYTES),
        "ISO 19115 input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_ISO19115_XML_EVENTS),
            max_nodes: MAX_ISO19115_XML_NODES,
            max_depth: MAX_ISO19115_XML_DEPTH,
            max_text_bytes: MAX_ISO19115_TEXT_BYTES,
        },
        "ISO 19115",
    )?;
    if !root.name.eq_ignore_ascii_case("MD_Metadata") {
        return Err(Error::InvalidInput(
            "ISO 19115 XML root must MD_Metadata".into(),
        ));
    }

    let mut summary = Summary {
        identification_info: count_named(&root, "identificationInfo"),
        reference_system_info: count_named(&root, "referenceSystemInfo"),
        data_quality_info: count_named(&root, "dataQualityInfo"),
        distribution_info: count_named(&root, "distributionInfo"),
        contacts: count_named(&root, "contact"),
        online_resources: count_named(&root, "onlineResource")
            + count_named(&root, "CI_OnlineResource"),
        constraints: count_named(&root, "resourceConstraints") + count_named(&root, "constraint"),
        graphic_overviews: count_named(&root, "graphicOverview"),
        ..Summary::default()
    };

    summary.title = first_text_descendant(&root, "title");
    summary.dates = collect_leaf_values(&root, "date");
    summary.hierarchy_levels = collect_scope_values(&root, "hierarchyLevel");
    summary.language = first_text_descendant(&root, "language");
    summary.character_set = first_text_descendant(&root, "characterSet");
    summary.topics = collect_texts(&root, "topicCategory");
    summary.crs = collect_crs_codes(&root);
    summary.bbox = find_bbox(&root);

    if summary.title.is_empty()
        && summary.identification_info == 0
        && summary.reference_system_info == 0
        && summary.data_quality_info == 0
    {
        return Err(Error::InvalidInput(
            "ISO 19115 metadata contains no identification, reference-system or quality information".into(),
        ));
    }

    push_row(
        &mut summary.rows,
        "Title",
        &summary.title.clone(),
        "identification",
    )?;
    for date in summary.dates.clone() {
        push_row(&mut summary.rows, "Date", &date, "citation")?;
    }
    for level in summary.hierarchy_levels.clone() {
        push_row(&mut summary.rows, "Hierarchy level", &level, "scope")?;
    }
    if !summary.language.is_empty() {
        push_row(
            &mut summary.rows,
            "Language",
            &summary.language.clone(),
            "metadata",
        )?;
    }
    if !summary.character_set.is_empty() {
        push_row(
            &mut summary.rows,
            "Character set",
            &summary.character_set.clone(),
            "metadata",
        )?;
    }
    for topic in summary.topics.clone() {
        push_row(&mut summary.rows, "Topic", &topic, "identification")?;
    }
    for crs in summary.crs.clone() {
        push_row(&mut summary.rows, "Reference system", &crs, "CRS code")?;
    }
    if let Some([west, east, south, north]) = summary.bbox {
        push_row(
            &mut summary.rows,
            "Geographic bounding box",
            &format!("west={west}, east={east}, south={south}, north={north}"),
            "coordinates shown without transformation",
        )?;
    }
    push_row(
        &mut summary.rows,
        "Section counts",
        &format!(
            "identification={} reference-system={} quality={} distribution={}",
            summary.identification_info,
            summary.reference_system_info,
            summary.data_quality_info,
            summary.distribution_info
        ),
        "metadata structure",
    )?;
    push_row(
        &mut summary.rows,
        "Resource links",
        &summary.online_resources.to_string(),
        "URLs counted but omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Contacts",
        &summary.contacts.to_string(),
        "contact payload omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Quality/constraints",
        &format!(
            "quality={} constraints={} graphics={}",
            summary.data_quality_info, summary.constraints, summary.graphic_overviews
        ),
        "payloads omitted",
    )?;

    let metadata = format!(
        "Title: {}\nIdentification sections: {}\nReference systems: {}\nData quality sections: {}\nDistribution sections: {}\nGeographic bounding box: {}",
        display_or_dash(&summary.title),
        summary.identification_info,
        summary.reference_system_info,
        summary.data_quality_info,
        summary.distribution_info,
        if summary.bbox.is_some() {
            "present"
        } else {
            "none"
        },
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "ISO 19115 metadata".into(),
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
        "ISO 19115/19139 titles, dates, topics, CRS codes, bounding boxes and section counts are shown; contacts, abstract/lineage text, identifiers and online-resource URLs are omitted or redacted".into(),
        "ISO 19115 XML traversal and rendered rows are bounded; DTD/entities, XInclude, network retrieval, URL dereferencing and CRS coordinate transformation never run".into(),
    ];
    let mut page_sink = Iso19115PageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn count_named(element: &XmlElement, name: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| usize::from(child.name.eq_ignore_ascii_case(name)) + count_named(child, name))
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
    let mut pieces = Vec::new();
    if !element.text.trim().is_empty() {
        pieces.push(element.text.trim().to_owned());
    }
    for child in &element.children {
        let text = text_content(child);
        if !text.is_empty() {
            pieces.push(text);
        }
    }
    pieces.join(" ")
}

fn safe_text(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(trimmed)
    }
}

fn first_text_descendant(element: &XmlElement, name: &str) -> String {
    descendants_named(element, name)
        .into_iter()
        .map(text_content)
        .map(|value| safe_text(&value))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn collect_texts(element: &XmlElement, name: &str) -> Vec<String> {
    descendants_named(element, name)
        .into_iter()
        .map(text_content)
        .map(|value| safe_text(&value))
        .filter(|value| !value.is_empty())
        .take(MAX_ISO19115_ROWS)
        .collect()
}

fn collect_leaf_values(element: &XmlElement, name: &str) -> Vec<String> {
    descendants_named(element, name)
        .into_iter()
        .flat_map(|node| {
            let values = descendants_named(node, "Date")
                .into_iter()
                .chain(descendants_named(node, "DateTime"))
                .map(text_content)
                .map(|value| safe_text(&value))
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            if values.is_empty() {
                let value = safe_text(&text_content(node));
                if value.is_empty() {
                    Vec::new()
                } else {
                    vec![value]
                }
            } else {
                values
            }
        })
        .take(MAX_ISO19115_ROWS)
        .fold(Vec::new(), |mut values, value| {
            if !values.iter().any(|existing| existing == &value) {
                values.push(value);
            }
            values
        })
}

fn collect_scope_values(element: &XmlElement, name: &str) -> Vec<String> {
    descendants_named(element, name)
        .into_iter()
        .map(|node| {
            node.children
                .iter()
                .find_map(|child| {
                    child
                        .attribute("codeListValue")
                        .or_else(|| child.attribute("codeList"))
                })
                .map(str::to_owned)
                .unwrap_or_else(|| safe_text(&text_content(node)))
        })
        .filter(|value| !value.is_empty())
        .take(MAX_ISO19115_ROWS)
        .collect()
}

fn collect_crs_codes(element: &XmlElement) -> Vec<String> {
    descendants_named(element, "referenceSystemInfo")
        .into_iter()
        .flat_map(|reference| descendants_named(reference, "code"))
        .map(text_content)
        .map(|value| safe_text(&value))
        .filter(|value| !value.is_empty())
        .take(MAX_ISO19115_ROWS)
        .collect()
}

fn find_bbox(element: &XmlElement) -> Option<[f64; 4]> {
    let extent = descendants_named(element, "EX_GeographicBoundingBox")
        .into_iter()
        .next()
        .or_else(|| {
            descendants_named(element, "geographicElement")
                .into_iter()
                .next()
        })?;
    let coordinate = |name: &str| {
        descendants_named(extent, name)
            .into_iter()
            .find_map(|node| text_content(node).trim().parse::<f64>().ok())
    };
    let west = coordinate("westBoundLongitude")?;
    let east = coordinate("eastBoundLongitude")?;
    let south = coordinate("southBoundLatitude")?;
    let north = coordinate("northBoundLatitude")?;
    if [west, east, south, north]
        .iter()
        .all(|value| value.is_finite())
    {
        Some([west, east, south, north])
    } else {
        None
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_ISO19115_ROWS {
        return Err(Error::LimitExceeded(format!(
            "ISO 19115 rendered rows exceed {MAX_ISO19115_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn display_or_dash(value: &str) -> String {
    if value.is_empty() {
        "-".into()
    } else {
        truncate(value)
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_ISO19115_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_ISO19115_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;

    #[test]
    fn recognizes_iso_19139_metadata_namespace() {
        let xml = br#"<gmd:MD_Metadata xmlns:gmd="http://www.isotc211.org/2005/gmd"><gmd:identificationInfo><gmd:MD_DataIdentification/></gmd:identificationInfo></gmd:MD_Metadata>"#;
        assert!(looks_like_prefix(xml));
    }

    #[test]
    fn rejects_generic_metadata_root() {
        assert!(!looks_like_prefix(
            br#"<MD_Metadata><title>generic</title></MD_Metadata>"#
        ));
    }
}
