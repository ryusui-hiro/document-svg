//! Bounded Microsoft Access ACE/Jet database header previews.
//!
//! ACCDB and MDB files are page-oriented database files whose first page
//! identifies the storage engine (`Standard ACE DB` or `Standard Jet DB`).
//! This adapter reads only that fixed header page and a bounded version-marker
//! search.  Tables, queries, forms, attachments, VBA, linked databases, and
//! encrypted payloads are never opened or executed.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_ACCESS_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const ACCESS_HEADER_BYTES: usize = 4096;
const ACE_MARKER: &[u8] = b"Standard ACE DB";
const JET_MARKER: &[u8] = b"Standard Jet DB";
const MSISAM_MARKER: &[u8] = b"MSISAM Database";

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    prefix.len() >= 4
        && prefix[..4] == [0, 1, 0, 0]
        && (prefix
            .windows(ACE_MARKER.len())
            .any(|window| window == ACE_MARKER)
            || prefix
                .windows(JET_MARKER.len())
                .any(|window| window == JET_MARKER)
            || prefix
                .windows(MSISAM_MARKER.len())
                .any(|window| window == MSISAM_MARKER))
}

struct AccessPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for AccessPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "access".into();
        if page.title.is_empty() {
            page.title = "Microsoft Access database header".into();
        }
        page.description = "ACE/Jet header metadata is rendered as bounded inert rows; database objects and executable content are not opened".into();
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
    let max_bytes = options.max_input_bytes.min(MAX_ACCESS_BYTES);
    if metadata.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "Access input exceeds maximum bytes ({max_bytes})"
        )));
    }
    let mut file = File::open(path)?;
    let read_len = metadata.len().min(ACCESS_HEADER_BYTES as u64) as usize;
    let mut header = vec![0u8; read_len];
    file.read_exact(&mut header)?;
    if !looks_like_prefix(&header) {
        return Err(Error::InvalidInput(
            "Access input has no recognized ACE/Jet database header".into(),
        ));
    }
    let (engine, page_size) = if header
        .windows(ACE_MARKER.len())
        .any(|window| window == ACE_MARKER)
    {
        ("ACE / ACCDB", "4096 bytes (ACE default)")
    } else if header
        .windows(JET_MARKER.len())
        .any(|window| window == JET_MARKER)
    {
        ("Jet / MDB", "2048 or 4096 bytes (Jet generation dependent)")
    } else {
        ("MSISAM", "engine-defined")
    };
    let rows = vec![
        vec!["Storage engine".into(), engine.into()],
        vec!["Header marker offset".into(), "4".into()],
        vec!["Page size".into(), page_size.into()],
        vec!["File bytes".into(), metadata.len().to_string()],
        vec![
            "AccessVersion marker".into(),
            if header
                .windows(b"AccessVersion".len())
                .any(|window| window == b"AccessVersion")
            {
                "present"
            } else {
                "not in bounded header"
            }
            .into(),
        ],
        vec!["Header bytes read".into(), read_len.to_string()],
    ];
    let warnings = vec![
        "Access tables, rows, columns, indexes, queries, forms, reports, relationships, attachments, and linked databases are not decoded".into(),
        "VBA/data macros, OLE/ActiveX payloads, passwords, encryption, external paths, and database operations remain inert and are never bypassed or executed".into(),
    ];
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Microsoft Access database header".into(),
        },
        HtmlBlock::Paragraph {
            text: "Only the ACE/Jet engine marker and bounded first-page metadata are inspected. Database pages and executable content are never expanded.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows,
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = AccessPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}
