//! Bounded Green Building XML (gbXML) building/energy-model previews.
//!
//! gbXML transfers BIM building geometry and analysis metadata. This adapter
//! reports the model's structural inventory without evaluating schedules,
//! geometry, formulas, URLs or external weather/material resources.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_GBXML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GBXML_EVENTS: usize = 1_000_000;
const MAX_GBXML_NODES: usize = 500_000;
const MAX_GBXML_DEPTH: usize = 128;
const MAX_GBXML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_GBXML_ROWS: usize = 200_000;
const MAX_GBXML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    version: String,
    campuses: usize,
    buildings: usize,
    spaces: usize,
    surfaces: usize,
    openings: usize,
    zones: usize,
    constructions: usize,
    materials: usize,
    schedules: usize,
    systems: usize,
    locations: usize,
    shell_geometries: usize,
    occupants: usize,
    rows: Vec<Vec<String>>,
}

struct GbxmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for GbxmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "gbxml".into();
        if page.title.is_empty() {
            page.title = "gbXML building model".into();
        }
        page.description =
            "gbXML BIM and building-energy structure is rendered as bounded inert metadata; geometry, schedules and external resources are not evaluated".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"gbXML", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("gbxml.org/schema")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GBXML_BYTES),
        "gbXML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_GBXML_EVENTS),
            max_nodes: MAX_GBXML_NODES,
            max_depth: MAX_GBXML_DEPTH,
            max_text_bytes: MAX_GBXML_TEXT_BYTES,
        },
        "gbXML",
    )?;
    if !root.name.eq_ignore_ascii_case("gbxml") {
        return Err(Error::InvalidInput("gbXML root must be <gbXML>".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| !namespace.to_ascii_lowercase().contains("gbxml.org/schema"))
    {
        return Err(Error::InvalidInput(
            "gbXML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        version: attr_local(&root, "version")
            .map(truncate)
            .unwrap_or_default(),
        campuses: count_named(&root, "Campus"),
        buildings: count_named(&root, "Building"),
        spaces: count_named(&root, "Space"),
        surfaces: count_named(&root, "Surface"),
        openings: count_named(&root, "Opening"),
        zones: count_named(&root, "Zone"),
        constructions: count_named(&root, "Construction"),
        materials: count_named(&root, "Material"),
        schedules: count_named(&root, "Schedule") + count_named(&root, "DaySchedule"),
        systems: count_named(&root, "AirLoop")
            + count_named(&root, "ZoneEquipments")
            + count_named(&root, "BuildingStorey")
            + count_named(&root, "System"),
        locations: count_named(&root, "Location") + count_named(&root, "WeatherStation"),
        shell_geometries: count_named(&root, "ShellGeometry")
            + count_named(&root, "RectangularGeometry"),
        occupants: count_named(&root, "People")
            + count_named(&root, "Occupant")
            + count_named(&root, "Occupants"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Model",
        "gbXML",
        &format!("version={}", display_or_dash(&summary.version)),
    )?;
    push_row(
        &mut summary.rows,
        "Building",
        &summary.buildings.to_string(),
        &format!(
            "campuses={} spaces={} zones={}",
            summary.campuses, summary.spaces, summary.zones
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Envelope",
        &summary.surfaces.to_string(),
        &format!(
            "openings={} shellGeometry={}",
            summary.openings, summary.shell_geometries
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Materials",
        &summary.materials.to_string(),
        &format!(
            "constructions={} schedules={}",
            summary.constructions, summary.schedules
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Systems",
        &summary.systems.to_string(),
        &format!(
            "locations={} occupants={}",
            summary.locations, summary.occupants
        ),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "gbXML building model".into(),
        },
        HtmlBlock::Paragraph {
            text: "Green Building XML structure is summarized for BIM and energy-analysis review; geometry, schedules, formulas and external resources remain inert.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "gbXML IDs, coordinates, material properties, schedules, formulas, URLs and weather/resource payloads are omitted or redacted; only bounded structural counts are shown".into(),
        "gbXML schema locations, external references, geometry calculations, energy simulation and CRS/unit conversion never run".into(),
    ];
    let mut page_sink = GbxmlPageSink {
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

fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_GBXML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "gbXML rows exceed {MAX_GBXML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_GBXML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_GBXML_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
