//! Bounded JaCoCo/Cobertura-compatible coverage XML previews.
//!
//! Coverage reports can include source paths, class internals and raw execution
//! logs. This adapter renders package-level class and counter summaries only;
//! source code, file paths, session payloads, external entities and test
//! execution remain inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_COVERAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_COVERAGE_EVENTS: usize = 500_000;
const MAX_COVERAGE_NODES: usize = 300_000;
const MAX_COVERAGE_DEPTH: usize = 80;
const MAX_COVERAGE_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_COVERAGE_PACKAGES: usize = 100_000;
const MAX_COVERAGE_STRING_BYTES: usize = 512 * 1024;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let mut reader = quick_xml::Reader::from_reader(std::io::Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Start(element))
            | Ok(quick_xml::events::Event::Empty(element)) => {
                let element_name = element.name();
                let name = crate::ooxml::local_name(element_name.as_ref());
                return (name == b"report" || name == b"coverage")
                    && (bytes
                        .windows(b"<package".len())
                        .any(|window| window == b"<package")
                        || bytes
                            .windows(b"<packages".len())
                            .any(|window| window == b"<packages"));
            }
            Ok(quick_xml::events::Event::DocType(_)) => return false,
            Ok(quick_xml::events::Event::Eof) | Err(_) => return false,
            _ => buffer.clear(),
        }
        buffer.clear();
    }
}

struct CoveragePageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for CoveragePageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "coverage".into();
        if page.title.is_empty() {
            page.title = "Coverage report".into();
        }
        page.description =
            "JaCoCo/Cobertura coverage counters are rendered as inert package summaries; source paths and execution payloads are omitted".into();
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
        options.max_input_bytes.min(MAX_COVERAGE_BYTES),
        "coverage XML input",
    )?;
    let root = parse_report(&bytes)?;
    let (table, metadata, warnings) = summarize(&root)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Coverage report".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = CoveragePageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_report(bytes: &[u8]) -> Result<XmlElement> {
    if bytes.len() as u64 > MAX_COVERAGE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "coverage XML exceeds {MAX_COVERAGE_BYTES} bytes"
        )));
    }
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: MAX_COVERAGE_EVENTS,
            max_nodes: MAX_COVERAGE_NODES,
            max_depth: MAX_COVERAGE_DEPTH,
            max_text_bytes: MAX_COVERAGE_TEXT_BYTES,
        },
        "coverage",
    )?;
    if root.name != "report" && root.name != "coverage" {
        return Err(Error::InvalidInput(
            "coverage XML root must be report (JaCoCo) or coverage (Cobertura)".into(),
        ));
    }
    Ok(root)
}

#[derive(Clone, Copy, Default)]
struct Counter {
    missed: usize,
    covered: usize,
}

impl Counter {
    fn total(self) -> usize {
        self.missed.saturating_add(self.covered)
    }

    fn text(self) -> String {
        format!("{}/{}", self.covered, self.missed)
    }

    fn add(&mut self, other: Self) {
        self.missed = self.missed.saturating_add(other.missed);
        self.covered = self.covered.saturating_add(other.covered);
    }
}

#[derive(Clone, Copy, Default)]
struct Counters {
    line: Counter,
    branch: Counter,
    method: Counter,
    class: Counter,
}

impl Counters {
    fn add(&mut self, other: Self) {
        self.line.add(other.line);
        self.branch.add(other.branch);
        self.method.add(other.method);
        self.class.add(other.class);
    }
}

fn summarize(root: &XmlElement) -> Result<(TableData, String, Vec<String>)> {
    let mut packages = Vec::new();
    collect_packages(root, &mut packages)?;
    if packages.is_empty() {
        return Err(Error::InvalidInput(
            "coverage XML contains no package elements".into(),
        ));
    }
    let mut rows = Vec::with_capacity(packages.len());
    let mut total = Counters::default();
    let mut class_count = 0usize;
    for package in packages {
        let classes = count_descendants(package, "class");
        class_count = class_count.saturating_add(classes);
        let counters = package_counters(package);
        total.add(counters);
        let name = package
            .attribute("name")
            .filter(|value| !value.is_empty())
            .unwrap_or("(unnamed package)");
        rows.push(vec![
            truncate(name),
            classes.to_string(),
            counters.line.text(),
            counters.branch.text(),
            counters.method.text(),
            rate(counters.line),
        ]);
    }
    let report_name = root.attribute("name").unwrap_or("—");
    let metadata = format!(
        "Name: {}\nPackages: {}\nClasses: {class_count}\nLines covered/missed: {}\nBranches covered/missed: {}\nMethods covered/missed: {}\nLine coverage: {}",
        truncate(report_name),
        rows.len(),
        total.line.text(),
        total.branch.text(),
        total.method.text(),
        rate(total.line)
    );
    let warnings = vec![
        "Coverage source paths, class/method details, session information, execution logs, XML properties and attachments are omitted; tests and external resources are never executed or fetched".into(),
        "JaCoCo/Cobertura producer-specific counters are summarized conservatively; no coverage threshold or quality gate is evaluated".into(),
    ];
    Ok((
        TableData {
            headers: vec![
                "Package".into(),
                "Cls".into(),
                "Line C/M".into(),
                "Br C/M".into(),
                "Meth C/M".into(),
                "Line%".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 6],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn collect_packages<'a>(element: &'a XmlElement, packages: &mut Vec<&'a XmlElement>) -> Result<()> {
    if element.name == "package" {
        if packages.len() >= MAX_COVERAGE_PACKAGES {
            return Err(Error::LimitExceeded(format!(
                "coverage packages exceed {MAX_COVERAGE_PACKAGES}"
            )));
        }
        packages.push(element);
    }
    for child in &element.children {
        collect_packages(child, packages)?;
    }
    Ok(())
}

fn package_counters(package: &XmlElement) -> Counters {
    let mut counters = Counters::default();
    let mut direct_counter = false;
    for child in &package.children {
        if child.name == "counter" {
            direct_counter = true;
            apply_counter(&mut counters, child);
        }
    }
    if !direct_counter {
        let line_rate = package
            .attribute("line-rate")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0);
        if let Some(rate) = line_rate {
            counters.line.covered = (rate * 1000.0).round() as usize;
            counters.line.missed = 1000usize.saturating_sub(counters.line.covered);
        }
        let branch_rate = package
            .attribute("branch-rate")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0);
        if let Some(rate) = branch_rate {
            counters.branch.covered = (rate * 1000.0).round() as usize;
            counters.branch.missed = 1000usize.saturating_sub(counters.branch.covered);
        }
    }
    counters
}

fn apply_counter(counters: &mut Counters, element: &XmlElement) {
    let counter = Counter {
        missed: parse_usize(element.attribute("missed")),
        covered: parse_usize(element.attribute("covered")),
    };
    match element
        .attribute("type")
        .unwrap_or_default()
        .to_ascii_uppercase()
        .as_str()
    {
        "LINE" => counters.line = counter,
        "BRANCH" => counters.branch = counter,
        "METHOD" | "FUNCTION" => counters.method = counter,
        "CLASS" => counters.class = counter,
        _ => {}
    }
}

fn count_descendants(element: &XmlElement, name: &str) -> usize {
    let own = usize::from(element.name == name);
    own.saturating_add(
        element
            .children
            .iter()
            .map(|child| count_descendants(child, name))
            .sum(),
    )
}

fn parse_usize(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

fn rate(counter: Counter) -> String {
    if counter.total() == 0 {
        return "—".into();
    }
    format!(
        "{:.1}%",
        (counter.covered as f64 / counter.total() as f64) * 100.0
    )
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_COVERAGE_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_COVERAGE_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_jacoco_and_cobertura_roots() {
        assert!(looks_like_prefix(b"<report><package name=\"x\"/></report>"));
        assert!(looks_like_prefix(
            b"<coverage><packages><package name=\"x\"/></packages></coverage>"
        ));
        assert!(!looks_like_prefix(b"<report><other/></report>"));
    }

    #[test]
    fn summarizes_package_counters_without_source_payloads() {
        let root = parse_report(
            br#"<report name="unit"><sessioninfo id="secret"/><package name="com/example"><class name="A"><sourcefile name="private.java"/></class><counter type="LINE" missed="2" covered="5"/><counter type="BRANCH" missed="1" covered="3"/><counter type="METHOD" missed="1" covered="4"/><system-out>very-secret</system-out></package></report>"#,
        )
        .unwrap();
        let (table, metadata, warnings) = summarize(&root).unwrap();
        assert!(metadata.contains("Packages: 1"));
        assert_eq!(table.rows[0][2], "5/2");
        assert_eq!(table.rows[0][5], "71.4%");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret") || value.contains("private"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("omitted")));
    }
}
