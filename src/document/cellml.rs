//! Bounded CellML model previews.
//!
//! CellML describes reusable computational physiology models. This adapter
//! reports component/variable/connection structure without evaluating MathML,
//! units, imports or simulation experiments.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_CELLML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CELLML_EVENTS: usize = 1_000_000;
const MAX_CELLML_NODES: usize = 500_000;
const MAX_CELLML_DEPTH: usize = 128;
const MAX_CELLML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_CELLML_ROWS: usize = 200_000;
const MAX_CELLML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    models: usize,
    components: usize,
    variables: usize,
    units: usize,
    connections: usize,
    mappings: usize,
    imports: usize,
    encapsulations: usize,
    resets: usize,
    maths: usize,
    annotations: usize,
    rows: Vec<Vec<String>>,
}

struct CellmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for CellmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "cellml".into();
        if page.title.is_empty() {
            page.title = "CellML model".into();
        }
        page.description =
            "CellML model structure is rendered as bounded inert metadata; equations, imports and simulations are not evaluated".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"model", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("cellml.org/cellml")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_CELLML_BYTES),
        "CellML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_CELLML_EVENTS),
            max_nodes: MAX_CELLML_NODES,
            max_depth: MAX_CELLML_DEPTH,
            max_text_bytes: MAX_CELLML_TEXT_BYTES,
        },
        "CellML",
    )?;
    if !root.name.eq_ignore_ascii_case("model") {
        return Err(Error::InvalidInput("CellML root must be <model>".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| !namespace.to_ascii_lowercase().contains("cellml.org/cellml"))
    {
        return Err(Error::InvalidInput(
            "CellML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        models: 1,
        components: count_named(&root, "component"),
        variables: count_named(&root, "variable"),
        units: count_named(&root, "units") + count_named(&root, "unit"),
        connections: count_named(&root, "connection"),
        mappings: count_named(&root, "map_components") + count_named(&root, "map_variables"),
        imports: count_named(&root, "import"),
        encapsulations: count_named(&root, "encapsulation")
            + count_named(&root, "encapsulation_2_0"),
        resets: count_named(&root, "reset"),
        maths: count_named(&root, "math"),
        annotations: count_named(&root, "rdf") + count_named(&root, "annotation"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Model",
        &summary.models.to_string(),
        &format!(
            "components={} variables={}",
            summary.components, summary.variables
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Units",
        &summary.units.to_string(),
        &format!(
            "connections={} mappings={}",
            summary.connections, summary.mappings
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Imports",
        &summary.imports.to_string(),
        &format!(
            "encapsulation={} resets={}",
            summary.encapsulations, summary.resets
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Equations",
        &summary.maths.to_string(),
        &format!("annotations={} MathML omitted", summary.annotations),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "CellML model".into(),
        },
        HtmlBlock::Paragraph {
            text: "CellML component and connection structure is summarized without evaluating MathML or simulation metadata.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "CellML component names, variable values, units, MathML equations, annotations, URLs and private model data are omitted or redacted".into(),
        "CellML imports, external models, MathML, metadata schemas and simulation experiments are never fetched or executed".into(),
    ];
    let mut page_sink = CellmlPageSink {
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

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_CELLML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "CellML rows exceed {MAX_CELLML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_CELLML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_CELLML_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
