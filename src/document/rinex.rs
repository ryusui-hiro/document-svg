//! Bounded RINEX GNSS observation/navigation previews.
//!
//! RINEX is a fixed-width exchange format for receiver-independent satellite
//! observations and navigation records. This adapter validates headers and
//! counts epochs/satellite rows without exposing coordinates, timestamps,
//! receiver identifiers or measurement values.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_RINEX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_RINEX_LINES: usize = 5_000_000;
const MAX_RINEX_LINE_BYTES: usize = 1024 * 1024;
const MAX_RINEX_EPOCHS: usize = 2_000_000;
const MAX_RINEX_SATELLITES: usize = 20_000_000;
const MAX_RINEX_ROWS: usize = 200_000;
const MAX_RINEX_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    version: String,
    file_type: String,
    header_lines: usize,
    epochs: usize,
    satellite_rows: usize,
    observation_types: usize,
    navigation_records: usize,
    continuation_lines: usize,
    rows: Vec<Vec<String>>,
}

struct RinexPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for RinexPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "rinex".into();
        if page.title.is_empty() {
            page.title = "RINEX GNSS data".into();
        }
        page.description =
            "RINEX GNSS header and record structure is rendered as bounded inert metadata; coordinates and measurements are not exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    String::from_utf8_lossy(prefix)
        .lines()
        .take(64)
        .any(|line| line.to_ascii_uppercase().contains("RINEX VERSION / TYPE"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_RINEX_BYTES),
        "RINEX input",
    )?;
    let text = String::from_utf8_lossy(&bytes);
    let summary = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "RINEX GNSS data".into(),
        },
        HtmlBlock::Paragraph {
            text: "Receiver Independent Exchange Format structure is summarized without displaying coordinates, timestamps, station data or measurements.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "RINEX station names, antenna/receiver identifiers, coordinates, epochs, satellite observation values and navigation payloads are omitted or redacted".into(),
        "RINEX external references, receiver processing, cycle-slip detection, geodetic calculations and network resources never run".into(),
    ];
    let mut page_sink = RinexPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<Summary> {
    let mut summary = Summary::default();
    let mut in_header = true;
    let mut saw_header_end = false;
    for (line_no, line) in text.lines().enumerate() {
        if line_no >= MAX_RINEX_LINES {
            return Err(Error::LimitExceeded(format!(
                "RINEX lines exceed {MAX_RINEX_LINES}"
            )));
        }
        if line.len() > MAX_RINEX_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "RINEX line exceeds {MAX_RINEX_LINE_BYTES} bytes"
            )));
        }
        let upper = line.to_ascii_uppercase();
        if in_header {
            summary.header_lines = summary.header_lines.saturating_add(1);
            if upper.contains("RINEX VERSION / TYPE") {
                summary.version = truncate(line.get(..9).unwrap_or(line).trim());
                summary.file_type = truncate(line.get(20..21).unwrap_or("").trim());
            }
            if upper.contains("# / TYPES OF OBSERV") || upper.contains("SYS / # / OBS TYPES") {
                summary.observation_types = summary.observation_types.saturating_add(1);
            }
            if upper.contains("END OF HEADER") {
                in_header = false;
                saw_header_end = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('>') {
            summary.epochs = summary.epochs.saturating_add(1);
            if summary.epochs > MAX_RINEX_EPOCHS {
                return Err(Error::LimitExceeded(format!(
                    "RINEX epochs exceed {MAX_RINEX_EPOCHS}"
                )));
            }
        }
        if is_satellite_record(line) {
            summary.satellite_rows = summary.satellite_rows.saturating_add(1);
            if summary.satellite_rows > MAX_RINEX_SATELLITES {
                return Err(Error::LimitExceeded(format!(
                    "RINEX satellite rows exceed {MAX_RINEX_SATELLITES}"
                )));
            }
        }
        if line.starts_with(' ') && line.len() > 80 {
            summary.continuation_lines = summary.continuation_lines.saturating_add(1);
        }
        if line.chars().take(3).all(|c| c.is_ascii_digit() || c == ' ') {
            summary.navigation_records = summary.navigation_records.saturating_add(1);
        }
    }
    if !saw_header_end {
        return Err(Error::InvalidInput(
            "RINEX input has no END OF HEADER".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Header",
        &summary.version,
        &format!(
            "type={} lines={} obsHeaders={}",
            display_or_dash(&summary.file_type),
            summary.header_lines,
            summary.observation_types
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Observations",
        &summary.epochs.to_string(),
        &format!(
            "epochs satelliteRows={} continuations={}",
            summary.satellite_rows, summary.continuation_lines
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Navigation",
        &summary.navigation_records.to_string(),
        "record values omitted",
    )?;
    Ok(summary)
}

fn is_satellite_record(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() >= 3
        && matches!(bytes[0], b'G' | b'R' | b'E' | b'C' | b'J' | b'I' | b'S')
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit()
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_RINEX_ROWS {
        return Err(Error::LimitExceeded(format!(
            "RINEX rows exceed {MAX_RINEX_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_RINEX_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_RINEX_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
