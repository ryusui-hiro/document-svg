//! Bounded ASAM OpenCRG road-surface header previews.
//!
//! OpenCRG files contain a clear-text section header followed by optional ASCII
//! or binary road-surface data. This adapter parses only the bounded header and
//! section structure; road samples, binary payloads, file references and
//! simulation/evaluation APIs are never opened or executed.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_OPENCRG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OPENCRG_LINES: usize = 1_000_000;
const MAX_OPENCRG_LINE_BYTES: usize = 1_024 * 1_024;
const MAX_OPENCRG_SECTIONS: usize = 100_000;
const MAX_OPENCRG_DISPLAY_BYTES: usize = 256;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    text.lines().take(64).any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("$CT")
            || trimmed.starts_with("$ROAD_CRG")
            || trimmed.starts_with("$KD_")
    })
}

struct OpenCrgPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OpenCrgPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "opencrg".into();
        if page.title.is_empty() {
            page.title = "ASAM OpenCRG".into();
        }
        page.description =
            "OpenCRG clear-text header metadata is rendered inertly; road surface payloads and referenced files are not decoded or opened".into();
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
        options.max_input_bytes.min(MAX_OPENCRG_BYTES),
        "OpenCRG input",
    )?;
    let (table, metadata, warnings) = parse(&bytes)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "ASAM OpenCRG".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = OpenCrgPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(bytes: &[u8]) -> Result<(TableData, String, Vec<String>)> {
    if bytes.len() as u64 > MAX_OPENCRG_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OpenCRG input exceeds {MAX_OPENCRG_BYTES} bytes"
        )));
    }
    let text = String::from_utf8_lossy(bytes);
    let mut rows = Vec::new();
    let mut section: Option<String> = None;
    let mut section_lines = 0usize;
    let mut section_count = 0usize;
    let mut total_lines = 0usize;
    let mut road_data_lines = 0usize;
    let mut binary_tail = false;
    let mut warnings = Vec::new();
    for raw_line in text.lines() {
        total_lines = total_lines.saturating_add(1);
        if total_lines > MAX_OPENCRG_LINES {
            return Err(Error::LimitExceeded(format!(
                "OpenCRG lines exceed {MAX_OPENCRG_LINES}"
            )));
        }
        if raw_line.len() > MAX_OPENCRG_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "OpenCRG line exceeds {MAX_OPENCRG_LINE_BYTES} bytes"
            )));
        }
        let line = raw_line.trim();
        if line.starts_with('$') {
            if let Some(current) = section.take() {
                rows.push(vec![
                    current,
                    section_lines.to_string(),
                    "header section".into(),
                ]);
            }
            if line == "$" {
                section_lines = 0;
                continue;
            }
            let keyword = line
                .trim_start_matches('$')
                .split_whitespace()
                .next()
                .unwrap_or_default();
            if keyword.is_empty() {
                continue;
            }
            section_count = section_count.saturating_add(1);
            if section_count > MAX_OPENCRG_SECTIONS {
                return Err(Error::LimitExceeded(format!(
                    "OpenCRG sections exceed {MAX_OPENCRG_SECTIONS}"
                )));
            }
            section = Some(truncate(keyword));
            section_lines = 0;
            continue;
        }
        if section.is_some() {
            section_lines = section_lines.saturating_add(1);
            continue;
        }
        if !line.is_empty() {
            road_data_lines = road_data_lines.saturating_add(1);
            if raw_line.bytes().any(|byte| byte == 0) {
                binary_tail = true;
            }
        }
    }
    if let Some(current) = section.take() {
        rows.push(vec![
            current,
            section_lines.to_string(),
            "header section".into(),
        ]);
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "OpenCRG contains no clear-text header sections".into(),
        ));
    }
    if rows.len() > MAX_OPENCRG_SECTIONS {
        return Err(Error::LimitExceeded(format!(
            "OpenCRG rendered sections exceed {MAX_OPENCRG_SECTIONS}"
        )));
    }
    let metadata = format!(
        "Sections: {}\nLines: {}\nUnsectioned road-data lines: {}\nBinary bytes detected: {}",
        rows.len(),
        total_lines,
        road_data_lines,
        if binary_tail { "yes" } else { "no" }
    );
    warnings.push("OpenCRG road-surface samples and binary payloads are omitted; `$ROAD_CRG_FILE` references are never followed and no tire/vehicle/surface evaluation runs".into());
    if binary_tail {
        warnings.push(
            "binary-looking bytes were detected outside the clear-text header and were not decoded"
                .into(),
        );
    }
    Ok((
        TableData {
            headers: vec!["Section".into(), "Lines".into(), "Content".into()],
            rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OPENCRG_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_OPENCRG_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_header_sections() {
        assert!(looks_like_prefix(b"$CT\nOpenCRG header\n$\n$ROAD_CRG\n"));
        assert!(!looks_like_prefix(b"<OpenDRIVE/>"));
    }

    #[test]
    fn summarizes_header_without_road_payload() {
        let (table, metadata, warnings) = parse(
            b"$CT\nexample\n$\n$ROAD_CRG\nreference_line=0\n$\n$KD_Definition\nchannel=1\n$\n$ROAD_CRG_FILE\nfile=private.crg\n$\n",
        )
        .unwrap();
        assert_eq!(table.rows.len(), 4);
        assert!(metadata.contains("Sections: 4"));
        assert!(warnings.iter().any(|warning| warning.contains("payloads")));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("private"))
        );
    }
}
