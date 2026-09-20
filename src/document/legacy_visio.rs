//! Bounded previews for legacy Visio Compound File Binary documents.
//!
//! Visio 2010 and earlier uses a proprietary CFB container (`.vsd`, `.vss`,
//! `.vst`, and `.vsw`).  The binary drawing streams are intentionally not
//! decoded here: the adapter validates the container, identifies the Visio
//! document stream, and renders bounded stream/storage metadata.  This keeps
//! macros, embedded objects, external links, and opaque geometry inert while
//! still giving users a useful inspection page for old Office files.

use std::fs::{self, File};
use std::io::Cursor;
use std::path::Path;

use cfb::CompoundFile;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ir::Page;
use crate::table::{TableAlign, TableData};

const CFB_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const MAX_VSD_BYTES: u64 = 256 * 1024 * 1024;
const MAX_VSD_ENTRIES: usize = 50_000;
const MAX_VSD_ROWS: usize = 20_000;
const MAX_VSD_PATH_BYTES: usize = 1024;
const MAX_VSD_TOTAL_PATH_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn looks_like_legacy_visio(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() < 512 || metadata.len() > MAX_VSD_BYTES {
        return false;
    }
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(compound) = CompoundFile::open(file) else {
        return false;
    };
    compound
        .walk()
        .any(|entry| entry.is_stream() && entry.name().eq_ignore_ascii_case("VisioDocument"))
}

struct LegacyVisioPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    source_format: &'static str,
    title: &'static str,
    warnings: &'a [String],
}

impl PageConsumer for LegacyVisioPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = self.source_format.into();
        if page.title.is_empty() {
            page.title = self.title.into();
        }
        page.description = "Legacy Visio CFB stream metadata is rendered as bounded inert rows; binary geometry and embedded resources are not decoded".into();
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
        options.max_input_bytes.min(MAX_VSD_BYTES),
        "legacy Visio input",
    )?;
    if bytes.len() < 512 || bytes[..8] != CFB_SIGNATURE {
        return Err(Error::InvalidInput(
            "legacy Visio input is not a Compound File Binary document".into(),
        ));
    }
    let compound = CompoundFile::open(Cursor::new(bytes.as_slice())).map_err(|error| {
        Error::InvalidInput(format!("legacy Visio compound file is invalid: {error}"))
    })?;

    let mut rows = Vec::new();
    let mut stream_count = 0usize;
    let mut storage_count = 0usize;
    let mut total_path_bytes = 0usize;
    let mut has_visio_document = false;
    for (index, entry) in compound.walk().enumerate() {
        if index >= MAX_VSD_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "legacy Visio compound entries exceed {MAX_VSD_ENTRIES}"
            )));
        }
        let path_text = entry.path().to_string_lossy();
        if path_text.len() > MAX_VSD_PATH_BYTES
            || total_path_bytes.saturating_add(path_text.len()) > MAX_VSD_TOTAL_PATH_BYTES
        {
            return Err(Error::LimitExceeded(
                "legacy Visio compound paths exceed the bounded metadata budget".into(),
            ));
        }
        total_path_bytes = total_path_bytes.saturating_add(path_text.len());
        if entry.is_stream() {
            stream_count = stream_count.saturating_add(1);
            if entry.name().eq_ignore_ascii_case("VisioDocument") {
                has_visio_document = true;
            }
        } else if entry.is_storage() {
            storage_count = storage_count.saturating_add(1);
        }
        if rows.len() < MAX_VSD_ROWS && !entry.is_root() {
            rows.push(vec![
                if entry.is_stream() {
                    "stream"
                } else {
                    "storage"
                }
                .into(),
                truncate(&path_text),
                if entry.is_stream() {
                    format!("{} bytes", entry.len())
                } else {
                    "container".into()
                },
            ]);
        }
    }
    if !has_visio_document {
        return Err(Error::InvalidInput(
            "legacy Visio CFB document does not contain a VisioDocument stream".into(),
        ));
    }

    let mut warnings = vec![
        "Legacy Visio binary drawing streams, ShapeSheets, text, styles, page geometry, macros, and embedded objects are not decoded".into(),
        "External links, OLE payloads, ActiveX controls, and VBA projects remain inert and are never opened".into(),
    ];
    if rows.len() == MAX_VSD_ROWS {
        warnings.push(format!(
            "Legacy Visio stream metadata was truncated at {MAX_VSD_ROWS} rows"
        ));
    }
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Legacy Visio binary document".into(),
        },
        HtmlBlock::Paragraph {
            text: "The Compound File Binary container is valid and contains a VisioDocument stream. Opaque drawing payloads are summarized without execution or external resource access.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Path".into(), "Detail".into()],
            rows,
            alignments: vec![TableAlign::Left, TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows: vec![
                vec!["VisioDocument stream".into(), "present".into()],
                vec!["Streams".into(), stream_count.to_string()],
                vec!["Storages".into(), storage_count.to_string()],
                vec!["Container bytes".into(), bytes.len().to_string()],
            ],
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = LegacyVisioPageSink {
        inner: sink,
        source_format: "vsd",
        title: "Legacy Visio binary document",
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_VSD_PATH_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_VSD_PATH_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
