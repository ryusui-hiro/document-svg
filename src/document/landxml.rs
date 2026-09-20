//! Bounded LandXML 1.2 civil-engineering exchange previews.
//!
//! LandXML carries survey points, TIN surfaces, parcels, road alignments and
//! pipe networks. This adapter summarizes those structures without executing
//! survey references, external schemas or design/measurement calculations.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_LANDXML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LANDXML_XML_EVENTS: usize = 1_000_000;
const MAX_LANDXML_XML_NODES: usize = 500_000;
const MAX_LANDXML_XML_DEPTH: usize = 128;
const MAX_LANDXML_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_LANDXML_ROWS: usize = 200_000;
const MAX_LANDXML_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"LandXML", None)
        || crate::geospatial::xml_tree::looks_like_root(bytes, b"LandXml", None);
    if !root {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("landxml.org/schema/landxml")
        && (text.contains("<surfaces")
            || text.contains("<alignments")
            || text.contains("<cgpoints")
            || text.contains("<pipenetworks")
            || text.contains("<parcels"))
}

struct LandXmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for LandXmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "landxml".into();
        if page.title.is_empty() {
            page.title = "LandXML civil model".into();
        }
        page.description = "LandXML civil-engineering structures are rendered as a bounded inert summary; external references and survey calculations are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    version: String,
    date: String,
    units: String,
    surfaces: usize,
    surface_points: usize,
    surface_faces: usize,
    alignments: usize,
    alignment_segments: usize,
    parcels: usize,
    cpoints: usize,
    pipe_networks: usize,
    pipes: usize,
    structures: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_LANDXML_BYTES),
        "LandXML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_LANDXML_XML_EVENTS),
            max_nodes: MAX_LANDXML_XML_NODES,
            max_depth: MAX_LANDXML_XML_DEPTH,
            max_text_bytes: MAX_LANDXML_TEXT_BYTES,
        },
        "LandXML",
    )?;
    if !root.name.eq_ignore_ascii_case("LandXML") && !root.name.eq_ignore_ascii_case("LandXml") {
        return Err(Error::InvalidInput("LandXML root must LandXML".into()));
    }
    if !root
        .namespace
        .as_deref()
        .is_some_and(|ns| ns.contains("landxml.org/schema/LandXML"))
    {
        return Err(Error::InvalidInput(
            "LandXML namespace is missing or unsupported".into(),
        ));
    }
    let mut summary = Summary {
        version: root.attribute("version").unwrap_or_default().to_owned(),
        date: root.attribute("date").unwrap_or_default().to_owned(),
        units: first_attr_descendant(&root, "Imperial")
            .or_else(|| first_attr_descendant(&root, "Metric"))
            .unwrap_or_else(|| "unspecified".into()),
        surfaces: count_named(&root, "Surface"),
        alignments: count_named(&root, "Alignment"),
        parcels: count_named(&root, "Parcel"),
        cpoints: count_named(&root, "CgPoint"),
        pipe_networks: count_named(&root, "PipeNetwork"),
        pipes: count_named(&root, "Pipe"),
        structures: count_named(&root, "Struct"),
        ..Summary::default()
    };
    for surface in descendants_named(&root, "Surface") {
        let name = surface.attribute("name").unwrap_or("[unnamed surface]");
        let points = count_named(surface, "P").saturating_add(count_named(surface, "Pnt"));
        let faces = count_named(surface, "F").saturating_add(count_named(surface, "PFace"));
        summary.surface_points = summary.surface_points.saturating_add(points);
        summary.surface_faces = summary.surface_faces.saturating_add(faces);
        push_row(
            &mut summary.rows,
            "Surface",
            name,
            &format!("points={points} faces={faces}"),
        )?;
    }
    summary.alignment_segments = descendants_named(&root, "Alignments")
        .first()
        .map(|alignments| {
            count_named(alignments, "Line")
                + count_named(alignments, "Curve")
                + count_named(alignments, "Spiral")
        })
        .unwrap_or(0);
    if summary.surfaces == 0
        && summary.alignments == 0
        && summary.cpoints == 0
        && summary.parcels == 0
        && summary.pipe_networks == 0
    {
        return Err(Error::InvalidInput(
            "LandXML contains no supported civil structures".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Document",
        "LandXML",
        &format!(
            "version={} date={}",
            display_or_dash(&summary.version),
            display_or_dash(&summary.date)
        ),
    )?;
    push_row(&mut summary.rows, "Units", &summary.units, "root units")?;
    push_row(
        &mut summary.rows,
        "Surfaces",
        &summary.surfaces.to_string(),
        &format!(
            "points={} faces={}",
            summary.surface_points, summary.surface_faces
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Alignments",
        &summary.alignments.to_string(),
        &format!("segments={}", summary.alignment_segments),
    )?;
    push_row(
        &mut summary.rows,
        "Survey points",
        &summary.cpoints.to_string(),
        "CgPoint coordinates omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Parcels",
        &summary.parcels.to_string(),
        "boundary geometry omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Pipe networks",
        &summary.pipe_networks.to_string(),
        &format!("pipes={} structures={}", summary.pipes, summary.structures),
    )?;
    let metadata = format!(
        "Version: {}\nDate: {}\nUnits: {}\nSurfaces: {} ({} points / {} faces)\nAlignments: {}\nSurvey points: {}\nParcels: {}\nPipe networks: {}",
        display_or_dash(&summary.version),
        display_or_dash(&summary.date),
        display_or_dash(&summary.units),
        summary.surfaces,
        summary.surface_points,
        summary.surface_faces,
        summary.alignments,
        summary.cpoints,
        summary.parcels,
        summary.pipe_networks
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "LandXML civil model".into(),
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
        "LandXML version/date/units and surface, alignment, survey-point, parcel and pipe-network counts are shown; coordinate payloads, boundary geometry, design profiles, external schema URLs and survey metadata are omitted".into(),
        "LandXML XML traversal and rows are bounded; DTD/entities, XInclude, external schema fetches, file references, terrain calculations, stationing and CAD/field-device operations never run".into(),
    ];
    let mut page_sink = LandXmlPageSink {
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
fn first_attr_descendant(element: &XmlElement, name: &str) -> Option<String> {
    let node = descendants_named(element, name).first().copied()?;
    ["linearUnit", "areaUnit", "volumeUnit"]
        .iter()
        .find_map(|key| node.attribute(key))
        .map(truncate)
}
fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_LANDXML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "LandXML rows exceed {MAX_LANDXML_ROWS}"
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
    if value.len() <= MAX_LANDXML_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_LANDXML_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;
    #[test]
    fn recognizes_landxml_namespace() {
        assert!(looks_like_prefix(
            br#"<LandXML xmlns="http://www.landxml.org/schema/LandXML-1.2"><Surfaces/></LandXML>"#
        ));
    }
    #[test]
    fn rejects_generic_xml() {
        assert!(!looks_like_prefix(br#"<LandXML><Surfaces/></LandXML>"#));
    }
}
