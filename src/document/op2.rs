//! Bounded Nastran Output2 (OP2) binary preflight.
//!
//! OP2 files contain solver model/results tables in a record-framed binary
//! stream.  Full interpretation depends on table-specific element and result
//! schemas, so this adapter validates only conservative record framing in a
//! bounded prefix and reports recognizable table names.  It never decodes
//! result vectors, executes a solver, or follows referenced files.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_OP2_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_OP2_SCAN_BYTES: usize = 16 * 1024 * 1024;
const MAX_OP2_RECORD_BYTES: usize = 64 * 1024 * 1024;
const MAX_OP2_TABLES: usize = 1000;

const TABLE_NAMES: &[&[u8]] = &[
    b"GEOM1",
    b"GEOM2",
    b"GEOM3",
    b"GEOM4",
    b"GEOMM1",
    b"OUGV1",
    b"OES1",
    b"OQG1",
    b"OGPWG",
    b"OEF1",
    b"OAG1",
    b"OPG1",
    b"MAT1",
    b"KELM",
    b"XSOP2DIR",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn name(self) -> &'static str {
        match self {
            Self::Little => "little-endian",
            Self::Big => "big-endian",
        }
    }
    fn u32(self, bytes: &[u8]) -> Option<u32> {
        let array: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
        Some(match self {
            Self::Little => u32::from_le_bytes(array),
            Self::Big => u32::from_be_bytes(array),
        })
    }
}

struct Op2PageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for Op2PageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "op2".into();
        if page.title.is_empty() {
            page.title = "Nastran OP2 preflight".into();
        }
        page.description = "Nastran Output2 record metadata is rendered as bounded inert rows; model/result payloads are not decoded".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    TABLE_NAMES
        .iter()
        .any(|name| prefix.windows(name.len()).any(|window| window == *name))
        && detect_endian(prefix).is_some()
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let metadata = fs::metadata(path)?;
    let max_bytes = options.max_input_bytes.min(MAX_OP2_BYTES);
    if metadata.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "OP2 input exceeds maximum bytes ({max_bytes})"
        )));
    }
    let scan_len = metadata.len().min(MAX_OP2_SCAN_BYTES as u64) as usize;
    let mut file = File::open(path)?;
    let mut bytes = vec![0u8; scan_len];
    file.read_exact(&mut bytes)?;
    let endian = detect_endian(&bytes).ok_or_else(|| {
        Error::InvalidInput("OP2 record framing was not recognized in the bounded prefix".into())
    })?;
    let (records, scanned_bytes) = scan_records(&bytes, endian);
    let mut tables = Vec::new();
    for name in TABLE_NAMES {
        if bytes.windows(name.len()).any(|window| window == *name) {
            tables.push(std::str::from_utf8(name).unwrap_or("table"));
            if tables.len() >= MAX_OP2_TABLES {
                break;
            }
        }
    }
    let table_text = if tables.is_empty() {
        "none recognized".into()
    } else {
        tables.join(", ")
    };
    let rows = vec![
        vec!["File bytes".into(), metadata.len().to_string()],
        vec!["Scan bytes".into(), scanned_bytes.to_string()],
        vec!["Record byte order".into(), endian.name().into()],
        vec!["Framed records".into(), records.to_string()],
        vec!["Recognized tables".into(), table_text],
    ];
    let mut warnings = vec![
        "OP2 table schemas, model geometry, element connectivity, result vectors, precision variants, and subcase semantics are not decoded".into(),
        "External references, solver commands, DMAP/user code, file paths, and result post-processing remain inert and are never executed".into(),
    ];
    if scan_len < metadata.len() as usize {
        warnings.push(format!(
            "OP2 preflight scanned only the first {MAX_OP2_SCAN_BYTES} bytes"
        ));
    }
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Nastran OP2 preflight".into(),
        },
        HtmlBlock::Paragraph {
            text: "The bounded prefix is checked for conservative OP2 record framing and known table labels. Binary model and result payloads are never expanded.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows,
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = Op2PageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn detect_endian(bytes: &[u8]) -> Option<Endian> {
    [Endian::Little, Endian::Big].into_iter().find(|endian| {
        (0..bytes.len().saturating_sub(12))
            .step_by(4)
            .take(256)
            .any(|offset| valid_record(bytes, *endian, offset).is_some())
    })
}

fn valid_record(bytes: &[u8], endian: Endian, offset: usize) -> Option<usize> {
    let record_len = usize::try_from(endian.u32(bytes.get(offset..offset + 4)?)?).ok()?;
    if record_len == 0 || record_len > MAX_OP2_RECORD_BYTES {
        return None;
    }
    let trailer_offset = offset.checked_add(4)?.checked_add(record_len)?;
    let trailer = endian.u32(bytes.get(trailer_offset..trailer_offset + 4)?)?;
    (trailer as usize == record_len).then_some(record_len + 8)
}

fn scan_records(bytes: &[u8], endian: Endian) -> (usize, usize) {
    let mut offset = 0usize;
    let mut records = 0usize;
    while offset.saturating_add(8) <= bytes.len() && records < MAX_OP2_TABLES * 100 {
        if let Some(consumed) = valid_record(bytes, endian, offset) {
            records = records.saturating_add(1);
            offset = offset.saturating_add(consumed);
        } else {
            offset = offset.saturating_add(4);
        }
    }
    (records, offset.min(bytes.len()))
}
