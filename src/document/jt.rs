//! Bounded Siemens JT header previews.
//!
//! JT files begin with an 80-character version string followed by byte-order
//! and table-of-contents metadata.  This adapter reads only that fixed header
//! and file size, keeping LSG/TOC segments, tessellation, Parasolid XT B-Rep,
//! PMI and external resources inert.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_JT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const JT_HEADER_BYTES: usize = 96;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    // JT's version string reads e.g. "Version 10.5 JT" or "Version 9.5 JT"
    // (Siemens' JT File Format Reference) — the " JT" marker, not just the
    // leading "Version " word, is what actually identifies the format.
    // Matching on "Version " alone previously misdetected any plain-text
    // format that happens to start a line with that word — e.g. LTspice's
    // ".asc" schematics, which start with a bare "Version 4" line — as JT
    // whenever the input reached this content-sniffing path (no file
    // extension to disambiguate by).
    prefix.starts_with(b"Version ")
        && prefix[..prefix.len().min(JT_HEADER_BYTES)]
            .windows(3)
            .any(|window| window == b" JT")
}

struct JtPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for JtPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "jt".into();
        if page.title.is_empty() {
            page.title = "Siemens JT header".into();
        }
        page.description = "JT fixed-header metadata is rendered as bounded inert rows; model segments and external resources are not decoded".into();
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
    let metadata = fs::metadata(path)?;
    let max_bytes = options.max_input_bytes.min(MAX_JT_BYTES);
    if metadata.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "JT input exceeds maximum bytes ({max_bytes})"
        )));
    }
    let read_len = metadata.len().min(JT_HEADER_BYTES as u64) as usize;
    if read_len < 80 {
        return Err(Error::InvalidInput(
            "JT file is shorter than its 80-byte version header".into(),
        ));
    }
    let mut file = File::open(path)?;
    let mut header = vec![0u8; read_len];
    file.read_exact(&mut header)?;
    if !looks_like_prefix(&header) {
        return Err(Error::InvalidInput(
            "JT version header does not start with 'Version '".into(),
        ));
    }
    let version = String::from_utf8_lossy(&header[..80])
        .trim_matches('\0')
        .trim()
        .to_owned();
    let byte_order = header
        .get(80)
        .map(|value| match value {
            0 => "little-endian marker 0".into(),
            1 => "big-endian marker 1".into(),
            other => format!("unknown marker {other}"),
        })
        .unwrap_or_else(|| "not present".into());
    let rows = vec![
        vec!["Version".into(), version],
        vec!["Byte order".into(), byte_order],
        vec!["File bytes".into(), metadata.len().to_string()],
        vec!["Header bytes read".into(), read_len.to_string()],
    ];
    let warnings = vec![
        "JT TOC/LSG segments, tessellation, Parasolid XT B-Rep, PMI, attributes, compression and precise geometry are not decoded".into(),
        "External references, plug-ins, scripts, textures, solver operations and file paths remain inert and are never opened".into(),
    ];
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Siemens JT header".into(),
        },
        HtmlBlock::Paragraph {
            text: "Only the fixed JT version header, byte-order marker and bounded file metadata are inspected. Model segments are never expanded or executed.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows,
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = JtPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_real_jt_version_headers() {
        assert!(looks_like_prefix(b"Version 10.5 JT"));
        assert!(looks_like_prefix(b"Version 9.5 JT"));
        assert!(looks_like_prefix(
            b"Version 10.6 synthetic JT fixture                                              \0\0\0"
        ));
    }

    /// LTspice `.asc` schematics start their first line with the bare word
    /// "Version" (e.g. "Version 4"), which used to satisfy this sniffer's old
    /// `starts_with(b"Version ")`-only check and get misdetected as JT
    /// whenever the file reached content-sniffing (no extension to
    /// disambiguate by). Real JT's version string always carries a " JT"
    /// marker within its fixed header; plain "Version N" text does not.
    #[test]
    fn does_not_match_other_formats_that_merely_start_with_version() {
        assert!(!looks_like_prefix(b"Version 4\nSHEET 1 880 680\n"));
        assert!(!looks_like_prefix(b"Version 1.0\nsome config file\n"));
        assert!(!looks_like_prefix(b"Version"));
        assert!(!looks_like_prefix(b""));
    }
}
