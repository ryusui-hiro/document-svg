//! Bounded ASAM OpenSCENARIO XML scenario-structure previews.
//!
//! OpenSCENARIO describes dynamic driving scenarios as entities and a
//! storyboard of stories, acts, maneuvers and actions. This adapter renders
//! hierarchy/count metadata only; catalogs, controllers, expressions, triggers,
//! road files and simulator behavior are never resolved or executed.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_OPENSCENARIO_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OPENSCENARIO_XML_NODES: usize = 500_000;
const MAX_OPENSCENARIO_XML_EVENTS: usize = 1_000_000;
const MAX_OPENSCENARIO_XML_DEPTH: usize = 96;
const MAX_OPENSCENARIO_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_OPENSCENARIO_ROWS: usize = 200_000;
const MAX_OPENSCENARIO_DISPLAY_BYTES: usize = 256;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"OpenSCENARIO", None)
}

struct OpenScenarioPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OpenScenarioPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "openscenario".into();
        if page.title.is_empty() {
            page.title = "ASAM OpenSCENARIO XML".into();
        }
        page.description =
            "OpenSCENARIO scenario structure is rendered inertly; catalogs, controllers, expressions and simulation behavior are not resolved or executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    rows: Vec<Vec<String>>,
    entities: usize,
    stories: usize,
    acts: usize,
    maneuvers: usize,
    events: usize,
    actions: usize,
    catalogs: usize,
    parameters: usize,
    road_network: bool,
    file_header: bool,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_OPENSCENARIO_BYTES),
        "OpenSCENARIO input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_OPENSCENARIO_XML_EVENTS),
            max_nodes: MAX_OPENSCENARIO_XML_NODES,
            max_depth: MAX_OPENSCENARIO_XML_DEPTH,
            max_text_bytes: MAX_OPENSCENARIO_TEXT_BYTES,
        },
        "OpenSCENARIO",
    )?;
    if root.name != "OpenSCENARIO" {
        return Err(Error::InvalidInput(
            "OpenSCENARIO XML root must be OpenSCENARIO".into(),
        ));
    }
    let mut summary = Summary::default();
    let mut warnings = Vec::new();
    let header = root.children_named("FileHeader").next();
    if let Some(header) = header {
        summary.file_header = true;
        let revision = format!(
            "rev {}.{}",
            header.attribute("revMajor").unwrap_or("—"),
            header.attribute("revMinor").unwrap_or("—")
        );
        push_row(
            &mut summary,
            vec![
                "FileHeader".into(),
                revision,
                "1".into(),
                "description/author omitted".into(),
            ],
        )?;
    } else {
        warnings.push("OpenSCENARIO FileHeader is missing".into());
    }
    visit(&root, &mut summary)?;
    if !summary.road_network {
        warnings.push("OpenSCENARIO RoadNetwork is missing".into());
    }
    if summary.stories == 0 {
        warnings.push("OpenSCENARIO Storyboard contains no Story elements".into());
    }
    if summary.rows.is_empty() {
        return Err(Error::InvalidInput(
            "OpenSCENARIO contains no recognized scenario structure".into(),
        ));
    }
    let metadata = format!(
        "Entities: {}\nStories: {}\nActs: {}\nManeuvers: {}\nEvents: {}\nActions: {}\nCatalog references: {}\nParameters: {}\nRoadNetwork: {}",
        summary.entities,
        summary.stories,
        summary.acts,
        summary.maneuvers,
        summary.events,
        summary.actions,
        summary.catalogs,
        summary.parameters,
        if summary.road_network {
            "present"
        } else {
            "missing"
        }
    );
    warnings.push("OpenSCENARIO CatalogLocations, CatalogReference paths, RoadNetwork LogicFile/SceneGraphFile, controllers, trajectories, parameter values, triggers, expressions and entity attributes remain inert; no external file or simulator is opened".into());
    warnings.push("Only bounded XML structure and selected names/counts are displayed; maneuvers, actions, dynamics and conditions are never evaluated".into());
    let mut page_sink = OpenScenarioPageSink {
        inner: sink,
        warnings: &warnings,
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "ASAM OpenSCENARIO XML".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Kind".into(),
                "Name / revision".into(),
                "N".into(),
                "Detail".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn visit(element: &XmlElement, summary: &mut Summary) -> Result<()> {
    for child in &element.children {
        let kind = match child.name.as_str() {
            "ScenarioObject" => {
                summary.entities = summary.entities.saturating_add(1);
                Some((
                    "Entity",
                    child.attribute("name").unwrap_or("(unnamed)"),
                    "object definition",
                ))
            }
            "Story" => {
                summary.stories = summary.stories.saturating_add(1);
                Some((
                    "Story",
                    child.attribute("name").unwrap_or("(unnamed)"),
                    "story",
                ))
            }
            "Act" => {
                summary.acts = summary.acts.saturating_add(1);
                Some(("Act", child.attribute("name").unwrap_or("(unnamed)"), "act"))
            }
            "Maneuver" => {
                summary.maneuvers = summary.maneuvers.saturating_add(1);
                Some((
                    "Maneuver",
                    child.attribute("name").unwrap_or("(unnamed)"),
                    "maneuver",
                ))
            }
            "Event" => {
                summary.events = summary.events.saturating_add(1);
                Some((
                    "Event",
                    child.attribute("name").unwrap_or("(unnamed)"),
                    "event",
                ))
            }
            "Action" => {
                summary.actions = summary.actions.saturating_add(1);
                Some((
                    "Action",
                    child.attribute("name").unwrap_or("(unnamed)"),
                    "action",
                ))
            }
            "CatalogReference" => {
                summary.catalogs = summary.catalogs.saturating_add(1);
                Some((
                    "Catalog",
                    child.attribute("catalogName").unwrap_or("(reference)"),
                    "external reference",
                ))
            }
            "ParameterDeclaration" => {
                summary.parameters = summary.parameters.saturating_add(1);
                Some((
                    "Parameter",
                    child.attribute("name").unwrap_or("(unnamed)"),
                    "declaration",
                ))
            }
            "RoadNetwork" => {
                summary.road_network = true;
                None
            }
            _ => None,
        };
        if let Some((kind, name, detail)) = kind {
            push_row(
                summary,
                vec![kind.into(), truncate(name), "1".into(), detail.into()],
            )?;
        }
        visit(child, summary)?;
    }
    Ok(())
}

fn push_row(summary: &mut Summary, row: Vec<String>) -> Result<()> {
    if summary.rows.len() >= MAX_OPENSCENARIO_ROWS {
        return Err(Error::LimitExceeded(format!(
            "OpenSCENARIO rows exceed {MAX_OPENSCENARIO_ROWS}"
        )));
    }
    summary.rows.push(row);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OPENSCENARIO_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_OPENSCENARIO_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_openscenario_root() {
        assert!(looks_like_prefix(
            br#"<OpenSCENARIO><FileHeader revMajor=\"1\" revMinor=\"2\"/></OpenSCENARIO>"#
        ));
        assert!(!looks_like_prefix(br#"<OpenDRIVE/>"#));
    }

    #[test]
    fn counts_storyboard_entities_and_actions() {
        let xml = br#"<OpenSCENARIO><FileHeader revMajor="1" revMinor="2"/><RoadNetwork/><Entities><ScenarioObject name="Ego"/></Entities><Storyboard><Story name="S"><Act name="A"><ManeuverGroup><Maneuver name="M"><Event name="E"><Action name="Do"/></Event></Maneuver></ManeuverGroup></Act></Story></Storyboard></OpenSCENARIO>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "OpenSCENARIO",
        )
        .unwrap();
        let mut summary = Summary::default();
        visit(&root, &mut summary).unwrap();
        assert_eq!(summary.entities, 1);
        assert_eq!(summary.stories, 1);
        assert_eq!(summary.acts, 1);
        assert_eq!(summary.maneuvers, 1);
        assert_eq!(summary.events, 1);
        assert_eq!(summary.actions, 1);
        assert!(summary.road_network);
    }
}
