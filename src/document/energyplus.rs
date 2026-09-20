//! Bounded EnergyPlus IDF/EPW previews.
//!
//! IDF input objects and EPW weather rows are parsed only for structural
//! review. No EnergyPlus executable, IDD, macro, schedule, weather conversion
//! or simulation is invoked, and field values remain inert.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_ENERGYPLUS_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENERGYPLUS_LINES: usize = 2_000_000;
const MAX_ENERGYPLUS_LINE_BYTES: usize = 1024 * 1024;
const MAX_IDF_OBJECTS: usize = 200_000;
const MAX_IDF_FIELDS: usize = 2_000_000;
const MAX_EPW_ROWS: usize = 2_000_000;
const MAX_EPW_COLUMNS: usize = 100;
const MAX_ENERGYPLUS_ROWS: usize = 200_000;
const MAX_ENERGYPLUS_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct IdfSummary {
    objects: usize,
    fields: usize,
    comments: usize,
    object_types: BTreeMap<String, usize>,
    rows: Vec<Vec<String>>,
}

#[derive(Default)]
struct EpwSummary {
    header_rows: usize,
    data_rows: usize,
    columns: usize,
    missing_values: usize,
    years: BTreeMap<String, usize>,
    rows: Vec<Vec<String>>,
}

struct EnergyPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
    source_format: &'static str,
    title: &'static str,
}

impl PageConsumer for EnergyPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = self.source_format.into();
        if page.title.is_empty() {
            page.title = self.title.into();
        }
        page.description =
            "EnergyPlus input/weather structure is rendered as bounded inert metadata; no simulation or weather conversion runs".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_idf_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    text.lines().any(|line| {
        let line = line.split('!').next().unwrap_or("").trim();
        let line = line.to_ascii_lowercase();
        [
            "version,",
            "building,",
            "zone,",
            "material,",
            "construction,",
            "schedule:",
            "output:",
            "simulationcontrol,",
            "site:",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
    })
}

pub(crate) fn looks_like_epw_prefix(prefix: &[u8]) -> bool {
    String::from_utf8_lossy(prefix)
        .lines()
        .next()
        .is_some_and(|line| line.to_ascii_uppercase().starts_with("LOCATION,"))
}

pub(crate) fn convert_idf(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_ENERGYPLUS_BYTES),
        "EnergyPlus IDF input",
    )?;
    let text = String::from_utf8_lossy(&bytes);
    let summary = parse_idf(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "EnergyPlus IDF input".into(),
        },
        HtmlBlock::Paragraph {
            text: "EnergyPlus Input Data File objects are summarized without exposing model field values or running a simulation.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Object".into(), "Count".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "EnergyPlus IDF object names are shown but field values, schedules, formulas, paths, secrets and weather references are omitted or redacted".into(),
        "EnergyPlus IDD lookup, EPMacro, ExpandObjects, external files, scripts, weather conversion and simulation never run".into(),
    ];
    let mut page_sink = EnergyPageSink {
        inner: sink,
        warnings: &warnings,
        source_format: "energyplus-idf",
        title: "EnergyPlus IDF input",
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

pub(crate) fn convert_epw(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_ENERGYPLUS_BYTES),
        "EnergyPlus EPW input",
    )?;
    let text = String::from_utf8_lossy(&bytes);
    let summary = parse_epw(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "EnergyPlus EPW weather".into(),
        },
        HtmlBlock::Paragraph {
            text: "EnergyPlus Weather File headers and hourly-row structure are summarized without exposing weather values or running conversion.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "EPW location metadata, timestamps, temperature, radiation, wind, precipitation and station values are omitted or redacted; only bounded row structure is shown".into(),
        "EnergyPlus weather conversion, design-day synthesis, external resources and simulation never run".into(),
    ];
    let mut page_sink = EnergyPageSink {
        inner: sink,
        warnings: &warnings,
        source_format: "energyplus-epw",
        title: "EnergyPlus EPW weather",
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_idf(text: &str) -> Result<IdfSummary> {
    let mut summary = IdfSummary::default();
    let mut statement = String::new();
    for (line_no, line) in text.lines().enumerate() {
        if line_no >= MAX_ENERGYPLUS_LINES {
            return Err(Error::LimitExceeded(format!(
                "EnergyPlus IDF lines exceed {MAX_ENERGYPLUS_LINES}"
            )));
        }
        if line.len() > MAX_ENERGYPLUS_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "EnergyPlus IDF line exceeds {MAX_ENERGYPLUS_LINE_BYTES} bytes"
            )));
        }
        let mut content = line;
        if let Some((before, _)) = line.split_once('!') {
            content = before;
            if before.trim().is_empty() {
                summary.comments = summary.comments.saturating_add(1);
            }
        }
        if content.trim().is_empty() {
            continue;
        }
        statement.push_str(content);
        while let Some(end) = statement.find(';') {
            let record = statement[..end].to_owned();
            statement.drain(..=end);
            process_idf_record(&mut summary, &record)?;
        }
        if !statement.is_empty() {
            statement.push('\n');
        }
    }
    if !statement.trim().is_empty() {
        return Err(Error::InvalidInput(
            "EnergyPlus IDF ended with an unterminated object".into(),
        ));
    }
    if summary.objects == 0 {
        return Err(Error::InvalidInput(
            "EnergyPlus IDF contains no object records".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Objects",
        &summary.objects.to_string(),
        &format!("fields={} comments={}", summary.fields, summary.comments),
    )?;
    for (kind, count) in summary
        .object_types
        .iter()
        .take(MAX_ENERGYPLUS_ROWS.saturating_sub(1))
    {
        push_row(
            &mut summary.rows,
            kind,
            &count.to_string(),
            "field values omitted",
        )?;
    }
    Ok(summary)
}

fn process_idf_record(summary: &mut IdfSummary, record: &str) -> Result<()> {
    let record = record.trim();
    if record.is_empty() {
        return Ok(());
    }
    let mut fields = record.split(',');
    let Some(kind) = fields.next().map(str::trim).filter(|kind| !kind.is_empty()) else {
        return Ok(());
    };
    summary.objects = summary.objects.saturating_add(1);
    if summary.objects > MAX_IDF_OBJECTS {
        return Err(Error::LimitExceeded(format!(
            "EnergyPlus IDF objects exceed {MAX_IDF_OBJECTS}"
        )));
    }
    let field_count = fields.count();
    summary.fields = summary.fields.saturating_add(field_count);
    if summary.fields > MAX_IDF_FIELDS {
        return Err(Error::LimitExceeded(format!(
            "EnergyPlus IDF fields exceed {MAX_IDF_FIELDS}"
        )));
    }
    let key = truncate(kind);
    *summary.object_types.entry(key).or_default() += 1;
    Ok(())
}

fn parse_epw(text: &str) -> Result<EpwSummary> {
    let mut summary = EpwSummary::default();
    for (line_no, line) in text.lines().enumerate() {
        if line_no >= MAX_ENERGYPLUS_LINES {
            return Err(Error::LimitExceeded(format!(
                "EnergyPlus EPW lines exceed {MAX_ENERGYPLUS_LINES}"
            )));
        }
        if line.len() > MAX_ENERGYPLUS_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "EnergyPlus EPW line exceeds {MAX_ENERGYPLUS_LINE_BYTES} bytes"
            )));
        }
        let columns = line.split(',').count();
        if columns > MAX_EPW_COLUMNS {
            return Err(Error::LimitExceeded(format!(
                "EnergyPlus EPW columns exceed {MAX_EPW_COLUMNS}"
            )));
        }
        if summary.header_rows < 8 {
            summary.header_rows = summary.header_rows.saturating_add(1);
            if line_no == 7 {
                summary.columns = columns;
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        summary.data_rows = summary.data_rows.saturating_add(1);
        if summary.data_rows > MAX_EPW_ROWS {
            return Err(Error::LimitExceeded(format!(
                "EnergyPlus EPW rows exceed {MAX_EPW_ROWS}"
            )));
        }
        let mut fields = line.split(',');
        if let Some(year) = fields.next() {
            *summary.years.entry(truncate(year.trim())).or_default() += 1;
        }
        for field in fields {
            if field.trim() == "999" || field.trim() == "9999" || field.trim() == "99999" {
                summary.missing_values = summary.missing_values.saturating_add(1);
            }
        }
    }
    if summary.header_rows < 8 || summary.data_rows == 0 {
        return Err(Error::InvalidInput(
            "EnergyPlus EPW requires eight header rows and hourly data".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Headers",
        &summary.header_rows.to_string(),
        &format!("hourly columns={}", summary.columns),
    )?;
    push_row(
        &mut summary.rows,
        "Hourly rows",
        &summary.data_rows.to_string(),
        &format!(
            "missing sentinels={} years={}",
            summary.missing_values,
            summary.years.len()
        ),
    )?;
    Ok(summary)
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_ENERGYPLUS_ROWS {
        return Err(Error::LimitExceeded(format!(
            "EnergyPlus rows exceed {MAX_ENERGYPLUS_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_ENERGYPLUS_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_ENERGYPLUS_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
