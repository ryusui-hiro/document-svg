//! Bounded OGC CityGML 2.0/3.0 XML city-model previews.
//!
//! CityGML represents semantic 3D urban objects such as buildings, roads,
//! terrain and water bodies at multiple levels of detail. This adapter renders
//! thematic object and LoD counts plus envelope/CRS presence; geometry payloads,
//! XLinks, textures and external resources remain inert and are never fetched.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_CITYGML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CITYGML_XML_NODES: usize = 500_000;
const MAX_CITYGML_XML_EVENTS: usize = 1_000_000;
const MAX_CITYGML_XML_DEPTH: usize = 96;
const MAX_CITYGML_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_CITYGML_ROWS: usize = 200_000;
const MAX_CITYGML_DISPLAY_BYTES: usize = 256;

const THEMATIC_OBJECTS: &[&str] = &[
    "Building",
    "Bridge",
    "Road",
    "Railway",
    "Track",
    "Square",
    "WaterBody",
    "ReliefFeature",
    "VegetationObject",
    "PlantCover",
    "SolitaryVegetationObject",
    "CityFurniture",
    "LandUse",
    "GenericCityObject",
    "Tunnel",
    "TransportationComplex",
    "AuxiliaryTrafficArea",
    "TrafficArea",
    "WaterBoundarySurface",
    "ClosureSurface",
];

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"CityModel", None)
}

struct CityGmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for CityGmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "citygml".into();
        if page.title.is_empty() {
            page.title = "OGC CityGML".into();
        }
        page.description =
            "CityGML thematic and LoD metadata is rendered inertly; geometry, XLinks, textures and external resources are not displayed or resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_CITYGML_BYTES),
        "CityGML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_CITYGML_XML_EVENTS),
            max_nodes: MAX_CITYGML_XML_NODES,
            max_depth: MAX_CITYGML_XML_DEPTH,
            max_text_bytes: MAX_CITYGML_TEXT_BYTES,
        },
        "CityGML",
    )?;
    if root.name != "CityModel" {
        return Err(Error::InvalidInput(
            "CityGML XML root must be CityModel".into(),
        ));
    }
    if let Some(namespace) = root.namespace.as_deref()
        && !namespace.contains("citygml")
    {
        return Err(Error::Unsupported(format!(
            "CityGML namespace '{namespace}' is unsupported"
        )));
    }
    let mut summary = Summary::default();
    visit(&root, &mut summary)?;
    let mut rows = vec![vec![
        "CityModel".into(),
        "—".into(),
        summary.objects.to_string(),
        format!("{} thematic types", summary.by_type.len()),
    ]];
    for (kind, counts) in &summary.by_type {
        if rows.len() >= MAX_CITYGML_ROWS {
            return Err(Error::LimitExceeded(format!(
                "CityGML rows exceed {MAX_CITYGML_ROWS}"
            )));
        }
        rows.push(vec![
            truncate(kind),
            counts.ids.to_string(),
            counts.lod.to_string(),
            format!(
                "lod0 {} · lod1 {} · lod2 {} · lod3 {} · lod4 {}",
                counts.lod0, counts.lod1, counts.lod2, counts.lod3, counts.lod4
            ),
        ]);
    }
    let mut warnings = Vec::new();
    if summary.unknown_objects > 0 {
        warnings.push(format!(
            "{} CityGML thematic/extension element(s) were not in the supported object inventory",
            summary.unknown_objects
        ));
    }
    if summary.xlinks > 0 {
        warnings.push(format!(
            "{} CityGML XLink/external reference(s) were counted but not resolved",
            summary.xlinks
        ));
    }
    warnings.push("CityGML geometry, appearance/textures, address data, names, attributes, XLinks and external schemas/resources remain omitted; no CRS transformation or 3D operation runs".into());
    warnings.push("CityGML XML is bounded by input, XML event/node/depth/text, object and rendered-row limits; LoD tags are counted without expanding geometry".into());
    let metadata = format!(
        "Objects: {}\nThematic types: {}\nLoD-tagged objects: {}\nEnvelope: {}\nCRS metadata: {}",
        summary.objects,
        summary.by_type.len(),
        summary.lod_objects,
        if summary.envelope {
            "present"
        } else {
            "missing"
        },
        if summary.crs { "present" } else { "missing" }
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "OGC CityGML".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Kind".into(),
                "Objects".into(),
                "LoD".into(),
                "LoD breakdown".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = CityGmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct Counts {
    ids: usize,
    lod: usize,
    lod0: usize,
    lod1: usize,
    lod2: usize,
    lod3: usize,
    lod4: usize,
}

#[derive(Default)]
struct Summary {
    by_type: BTreeMap<String, Counts>,
    objects: usize,
    lod_objects: usize,
    envelope: bool,
    crs: bool,
    xlinks: usize,
    unknown_objects: usize,
}

fn visit(element: &XmlElement, summary: &mut Summary) -> Result<()> {
    if element.name == "Envelope" {
        summary.envelope = true;
        if element.attribute("srsName").is_some() {
            summary.crs = true;
        }
    }
    if element.name == "href"
        || element.attribute("href").is_some()
        || element.attribute("xlink:href").is_some()
    {
        summary.xlinks = summary.xlinks.saturating_add(1);
    }
    if THEMATIC_OBJECTS.contains(&element.name.as_str()) {
        summary.objects = summary.objects.saturating_add(1);
        let entry = summary.by_type.entry(element.name.clone()).or_default();
        entry.ids = entry.ids.saturating_add(usize::from(
            element.attribute("id").is_some() || element.attribute("gml:id").is_some(),
        ));
        for child in &element.children {
            if let Some(level) = lod_level(&child.name) {
                entry.lod = entry.lod.saturating_add(1);
                summary.lod_objects = summary.lod_objects.saturating_add(1);
                match level {
                    0 => entry.lod0 = entry.lod0.saturating_add(1),
                    1 => entry.lod1 = entry.lod1.saturating_add(1),
                    2 => entry.lod2 = entry.lod2.saturating_add(1),
                    3 => entry.lod3 = entry.lod3.saturating_add(1),
                    4 => entry.lod4 = entry.lod4.saturating_add(1),
                    _ => {}
                }
            }
        }
    } else if is_extension_like(element) {
        summary.unknown_objects = summary.unknown_objects.saturating_add(1);
    }
    for child in &element.children {
        visit(child, summary)?;
    }
    Ok(())
}

fn lod_level(name: &str) -> Option<u8> {
    let level = name.strip_prefix("lod")?.chars().next()?.to_digit(10)? as u8;
    (level <= 4).then_some(level)
}

fn is_extension_like(element: &XmlElement) -> bool {
    element.name.ends_with("Surface")
        || element.name.ends_with("Feature")
        || element.name.ends_with("Object")
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_CITYGML_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_CITYGML_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_citymodel_root() {
        assert!(looks_like_prefix(
            br#"<CityModel xmlns="http://www.opengis.net/citygml/3.0"/>"#
        ));
        assert!(!looks_like_prefix(br#"<OpenDRIVE/>"#));
    }

    #[test]
    fn counts_thematic_objects_and_lod() {
        let xml = br#"<CityModel xmlns="http://www.opengis.net/citygml/3.0"><boundedBy><Envelope srsName="urn:ogc:def:crs:EPSG::4326"><lowerCorner>0 0</lowerCorner><upperCorner>1 1</upperCorner></Envelope></boundedBy><cityObjectMember><Building id="b1"><lod2Solid/><lod3MultiSurface/></Building></cityObjectMember><cityObjectMember><Road id="r1"><lod0MultiSurface/></Road></cityObjectMember></CityModel>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "CityGML",
        )
        .unwrap();
        let mut summary = Summary::default();
        visit(&root, &mut summary).unwrap();
        assert_eq!(summary.objects, 2);
        assert_eq!(summary.lod_objects, 3);
        assert!(summary.envelope);
        assert!(summary.crs);
    }
}
