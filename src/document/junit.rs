//! Bounded JUnit-compatible XML test report previews.
//!
//! JUnit XML is a de-facto interchange format used by Ant, Maven Surefire,
//! JUnit Platform and CI servers. This adapter summarizes suites and testcase
//! outcomes while omitting failure logs, stdout/stderr, properties and other
//! potentially sensitive payloads. It never runs tests or resolves external XML.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_JUNIT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_JUNIT_EVENTS: usize = 500_000;
const MAX_JUNIT_NODES: usize = 300_000;
const MAX_JUNIT_DEPTH: usize = 80;
const MAX_JUNIT_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_JUNIT_SUITES: usize = 100_000;
const MAX_JUNIT_STRING_BYTES: usize = 512 * 1024;

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
                return name == b"testsuite" || name == b"testsuites";
            }
            Ok(quick_xml::events::Event::DocType(_)) => return false,
            Ok(quick_xml::events::Event::Eof) | Err(_) => return false,
            _ => buffer.clear(),
        }
        buffer.clear();
    }
}

struct JunitPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for JunitPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "junit".into();
        if page.title.is_empty() {
            page.title = "JUnit test report".into();
        }
        page.description =
            "JUnit-compatible test suites are rendered as inert result metadata; test execution and log payloads are not accessed".into();
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
        options.max_input_bytes.min(MAX_JUNIT_BYTES),
        "JUnit XML input",
    )?;
    let root = parse_report(&bytes)?;
    let (table, metadata, warnings) = summarize(&root)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "JUnit test report".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = JunitPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_report(bytes: &[u8]) -> Result<XmlElement> {
    if bytes.len() as u64 > MAX_JUNIT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JUnit XML exceeds {MAX_JUNIT_BYTES} bytes"
        )));
    }
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: MAX_JUNIT_EVENTS,
            max_nodes: MAX_JUNIT_NODES,
            max_depth: MAX_JUNIT_DEPTH,
            max_text_bytes: MAX_JUNIT_TEXT_BYTES,
        },
        "JUnit",
    )?;
    if root.name != "testsuite" && root.name != "testsuites" {
        return Err(Error::InvalidInput(
            "JUnit XML root must be testsuite or testsuites".into(),
        ));
    }
    Ok(root)
}

fn summarize(root: &XmlElement) -> Result<(TableData, String, Vec<String>)> {
    let mut suites = Vec::new();
    collect_suites(root, &mut suites)?;
    if suites.is_empty() {
        return Err(Error::InvalidInput(
            "JUnit XML contains no testsuite elements".into(),
        ));
    }
    let mut rows = Vec::with_capacity(suites.len());
    let mut totals = Counts::default();
    let mut total_time = 0.0f64;
    for suite in suites {
        let counts = suite_counts(suite);
        totals.add(counts);
        total_time += parse_time(suite.attribute("time"));
        let name = suite
            .attribute("name")
            .filter(|value| !value.is_empty())
            .unwrap_or("(unnamed suite)");
        let time = suite.attribute("time").map_or_else(|| "—".into(), truncate);
        rows.push(vec![
            truncate(name),
            counts.tests.to_string(),
            counts.passed.to_string(),
            counts.failures.to_string(),
            format!("{} / {}", counts.errors, counts.skipped),
            time,
        ]);
    }
    let mut warnings = vec![
        "JUnit testcase failure/error details, stdout/stderr, properties, system output and attachment payloads are omitted; tests, actions and external resources are never executed or fetched".into(),
        "JUnit XML is treated as a de-facto report format; producer-specific attributes and aggregate values are summarized conservatively".into(),
    ];
    if totals.failures > 0 || totals.errors > 0 {
        warnings.push(format!(
            "{} failing and {} error testcase(s) are summarized without their log payloads",
            totals.failures, totals.errors
        ));
    }
    let metadata = format!(
        "Suites: {}\nTests: {}\nPassed: {}\nFailures: {}\nErrors: {}\nSkipped: {}\nTime: {:.3}s",
        rows.len(),
        totals.tests,
        totals.passed,
        totals.failures,
        totals.errors,
        totals.skipped,
        total_time
    );
    Ok((
        TableData {
            headers: vec![
                "Suite".into(),
                "Tests".into(),
                "Pass".into(),
                "Fail".into(),
                "Error / skip".into(),
                "Time".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 6],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

#[derive(Clone, Copy, Default)]
struct Counts {
    tests: usize,
    passed: usize,
    failures: usize,
    errors: usize,
    skipped: usize,
}

impl Counts {
    fn add(&mut self, other: Self) {
        self.tests = self.tests.saturating_add(other.tests);
        self.passed = self.passed.saturating_add(other.passed);
        self.failures = self.failures.saturating_add(other.failures);
        self.errors = self.errors.saturating_add(other.errors);
        self.skipped = self.skipped.saturating_add(other.skipped);
    }
}

fn collect_suites<'a>(element: &'a XmlElement, suites: &mut Vec<&'a XmlElement>) -> Result<()> {
    if element.name == "testsuite" {
        if suites.len() >= MAX_JUNIT_SUITES {
            return Err(Error::LimitExceeded(format!(
                "JUnit suites exceed {MAX_JUNIT_SUITES}"
            )));
        }
        suites.push(element);
    }
    for child in &element.children {
        collect_suites(child, suites)?;
    }
    Ok(())
}

fn suite_counts(suite: &XmlElement) -> Counts {
    let mut counts = Counts::default();
    for testcase in suite.children_named("testcase") {
        counts.tests = counts.tests.saturating_add(1);
        if testcase
            .children
            .iter()
            .any(|child| child.name == "failure")
        {
            counts.failures = counts.failures.saturating_add(1);
        } else if testcase.children.iter().any(|child| child.name == "error") {
            counts.errors = counts.errors.saturating_add(1);
        } else if testcase
            .children
            .iter()
            .any(|child| child.name == "skipped")
        {
            counts.skipped = counts.skipped.saturating_add(1);
        } else {
            counts.passed = counts.passed.saturating_add(1);
        }
    }
    if counts.tests == 0 {
        counts.tests = parse_usize(suite.attribute("tests"));
        counts.failures = parse_usize(suite.attribute("failures"));
        counts.errors = parse_usize(suite.attribute("errors"));
        counts.skipped = parse_usize(suite.attribute("skipped"));
        counts.passed = counts
            .tests
            .saturating_sub(counts.failures)
            .saturating_sub(counts.errors)
            .saturating_sub(counts.skipped);
    }
    counts
}

fn parse_usize(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

fn parse_time(value: Option<&str>) -> f64 {
    value
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(0.0)
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_JUNIT_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_JUNIT_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_junit_roots() {
        assert!(looks_like_prefix(b"<?xml version=\"1.0\"?><testsuites/>"));
        assert!(looks_like_prefix(b"<testsuite name=\"unit\"/>"));
        assert!(!looks_like_prefix(
            b"<configuration><testsuite/></configuration>"
        ));
    }

    #[test]
    fn summarizes_outcomes_without_logs() {
        let root = parse_report(
            br#"<testsuites><testsuite name="unit" tests="3" failures="1" errors="0" skipped="1" time="1.25"><testcase classname="A" name="ok" time="0.1"/><testcase classname="A" name="bad"><failure message="very-secret"/></testcase><testcase classname="A" name="skip"><skipped/></testcase><system-out>secret output</system-out></testsuite></testsuites>"#,
        )
        .unwrap();
        let (table, metadata, warnings) = summarize(&root).unwrap();
        assert!(metadata.contains("Tests: 3"));
        assert_eq!(table.rows[0][2], "1");
        assert_eq!(table.rows[0][3], "1");
        assert_eq!(table.rows[0][4], "0 / 1");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("omitted")));
    }

    #[test]
    fn rejects_external_doctype() {
        assert!(
            parse_report(
                br#"<!DOCTYPE testsuite SYSTEM "file:///etc/passwd"><testsuite name="x"/>"#,
            )
            .is_err()
        );
    }
}
