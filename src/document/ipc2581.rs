//! Bounded IPC-2581 PCB manufacturing-data previews.
//!
//! IPC-2581 is an XML exchange model for printed-board design, fabrication,
//! assembly and inspection.  This adapter reports section and object counts
//! from the bounded XML tree while keeping coordinates, material values,
//! external references, manufacturing commands, and binary payloads inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_IPC_BYTES: u64 = 128 * 1024 * 1024;
const MAX_IPC_EVENTS: usize = 1_000_000;
const MAX_IPC_NODES: usize = 500_000;
const MAX_IPC_DEPTH: usize = 128;
const MAX_IPC_ROWS: usize = 200_000;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("<ipc-2581") || text.contains(":ipc-2581")
}

struct IpcPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for IpcPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "ipc2581".into();
        if page.title.is_empty() {
            page.title = "IPC-2581 PCB exchange".into();
        }
        page.description = "IPC-2581 XML structure is rendered as bounded inert metadata; board geometry and manufacturing payloads are not executed".into();
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
        options.max_input_bytes.min(MAX_IPC_BYTES),
        "IPC-2581 input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_IPC_EVENTS),
            max_nodes: MAX_IPC_NODES,
            max_depth: MAX_IPC_DEPTH,
            max_text_bytes: 64 * 1024 * 1024,
        },
        "IPC-2581",
    )?;
    if !root.name.eq_ignore_ascii_case("IPC-2581") {
        return Err(Error::InvalidInput(
            "IPC-2581 root must be <IPC-2581>".into(),
        ));
    }
    let rows = vec![
        vec![
            "Revision".into(),
            root.attribute("revision").unwrap_or("unspecified").into(),
        ],
        vec![
            "Version".into(),
            root.attribute("version").unwrap_or("unspecified").into(),
        ],
        vec![
            "Content sections".into(),
            count_named(&root, "Content").to_string(),
        ],
        vec![
            "Logistic headers".into(),
            count_named(&root, "LogisticHeader").to_string(),
        ],
        vec![
            "History records".into(),
            count_named(&root, "HistoryRec").to_string(),
        ],
        vec![
            "BOM items".into(),
            count_named(&root, "BomItem").to_string(),
        ],
        vec![
            "ECAD sections".into(),
            count_named(&root, "Ecad").to_string(),
        ],
        vec![
            "AVL items".into(),
            count_named(&root, "AvlItem").to_string(),
        ],
        vec!["Boards".into(), count_named(&root, "Board").to_string()],
        vec!["Layers".into(), count_named(&root, "Layer").to_string()],
        vec![
            "Components".into(),
            count_named(&root, "Component").to_string(),
        ],
        vec!["Nets".into(), count_named(&root, "Net").to_string()],
        vec!["Routes".into(), count_named(&root, "Route").to_string()],
        vec![
            "Padstacks".into(),
            count_named(&root, "PadstackDef").to_string(),
        ],
        vec!["Packages".into(), count_named(&root, "Package").to_string()],
        vec!["Stackups".into(), count_named(&root, "Stackup").to_string()],
        vec!["Features".into(), count_named(&root, "Feature").to_string()],
    ];
    if rows.len() > MAX_IPC_ROWS {
        return Err(Error::LimitExceeded(format!(
            "IPC-2581 rows exceed {MAX_IPC_ROWS}"
        )));
    }
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "IPC-2581 PCB exchange".into(),
        },
        HtmlBlock::Paragraph {
            text: "IPC-2581 Content, BOM, ECAD and AVL structure is summarized without exposing board coordinates, materials, credentials, external files or manufacturing commands.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows,
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "IPC-2581 coordinates, stackup/material values, component attributes, tool paths, inspection values, external references and binary attachments are not rendered".into(),
        "No manufacturing, assembly, inspection, network or file operation is executed".into(),
    ];
    let mut page_sink = IpcPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn count_named(element: &XmlElement, name: &str) -> usize {
    usize::from(element.name.eq_ignore_ascii_case(name))
        + element
            .children
            .iter()
            .map(|child| count_named(child, name))
            .sum::<usize>()
}
