//! Bounded SED-ML simulation-experiment-description previews.
//!
//! SED-ML describes how models and simulations are combined. This adapter
//! reports experiment structure only; model changes, MathML, algorithm URIs,
//! external model files and simulation engines remain inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_SEDML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SEDML_EVENTS: usize = 1_000_000;
const MAX_SEDML_NODES: usize = 500_000;
const MAX_SEDML_DEPTH: usize = 128;
const MAX_SEDML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SEDML_ROWS: usize = 200_000;
const MAX_SEDML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    level: String,
    version: String,
    models: usize,
    simulations: usize,
    tasks: usize,
    repeated_tasks: usize,
    data_descriptions: usize,
    data_generators: usize,
    outputs: usize,
    plots: usize,
    reports: usize,
    changes: usize,
    ranges: usize,
    variables: usize,
    parameters: usize,
    algorithms: usize,
    rows: Vec<Vec<String>>,
}

struct SedmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for SedmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "sedml".into();
        if page.title.is_empty() {
            page.title = "SED-ML experiment".into();
        }
        page.description =
            "SED-ML experiment structure is rendered as bounded inert metadata; models, algorithms and simulations are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"sedML", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("sed-ml.org/sed-ml")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SEDML_BYTES),
        "SED-ML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_SEDML_EVENTS),
            max_nodes: MAX_SEDML_NODES,
            max_depth: MAX_SEDML_DEPTH,
            max_text_bytes: MAX_SEDML_TEXT_BYTES,
        },
        "SED-ML",
    )?;
    if !root.name.eq_ignore_ascii_case("sedML") {
        return Err(Error::InvalidInput("SED-ML root must be <sedML>".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| !namespace.to_ascii_lowercase().contains("sed-ml.org/sed-ml"))
    {
        return Err(Error::InvalidInput(
            "SED-ML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        level: attr_local(&root, "level").map(truncate).unwrap_or_default(),
        version: attr_local(&root, "version")
            .map(truncate)
            .unwrap_or_default(),
        models: count_named(&root, "model"),
        simulations: count_named(&root, "simulation")
            + count_named(&root, "uniformTimeCourse")
            + count_named(&root, "oneStep")
            + count_named(&root, "steadyState")
            + count_named(&root, "uniformTimeCourseSimulation"),
        tasks: count_named(&root, "task"),
        repeated_tasks: count_named(&root, "repeatedTask"),
        data_descriptions: count_named(&root, "dataDescription"),
        data_generators: count_named(&root, "dataGenerator"),
        outputs: count_named(&root, "output"),
        plots: count_named(&root, "plot2D") + count_named(&root, "plot3D"),
        reports: count_named(&root, "report"),
        changes: count_named(&root, "changeAttribute")
            + count_named(&root, "computeChange")
            + count_named(&root, "addXML")
            + count_named(&root, "removeXML")
            + count_named(&root, "changeXML"),
        ranges: count_named(&root, "uniformRange")
            + count_named(&root, "vectorRange")
            + count_named(&root, "functionalRange"),
        variables: count_named(&root, "variable"),
        parameters: count_named(&root, "parameter"),
        algorithms: count_named(&root, "algorithm"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Experiment",
        &format!(
            "level={} version={}",
            display_or_dash(&summary.level),
            display_or_dash(&summary.version)
        ),
        &format!(
            "models={} simulations={}",
            summary.models, summary.simulations
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Tasks",
        &summary.tasks.to_string(),
        &format!(
            "repeatedTasks={} changes={} ranges={}",
            summary.repeated_tasks, summary.changes, summary.ranges
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Analysis",
        &summary.data_generators.to_string(),
        &format!(
            "dataDescriptions={} variables={} parameters={}",
            summary.data_descriptions, summary.variables, summary.parameters
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Outputs",
        &summary.outputs.to_string(),
        &format!(
            "plots={} reports={} algorithms={}",
            summary.plots, summary.reports, summary.algorithms
        ),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "SED-ML experiment".into(),
        },
        HtmlBlock::Paragraph {
            text: "Simulation Experiment Description Markup Language structure is summarized without executing models, changes, algorithms or simulations.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "SED-ML IDs, model paths, algorithm URIs, MathML, variable/parameter values, XPath changes and output data are omitted or redacted".into(),
        "SED-ML model imports, external resources, numerical solvers, plotting, data processing and simulation execution never run".into(),
    ];
    let mut page_sink = SedmlPageSink {
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
    if rows.len() >= MAX_SEDML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "SED-ML rows exceed {MAX_SEDML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_SEDML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_SEDML_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
