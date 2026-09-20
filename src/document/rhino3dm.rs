//! Bounded Rhino OpenNURBS 3DM header previews.
//!
//! A 3DM file is a structured binary archive containing NURBS geometry,
//! attributes, and document tables.  The openNURBS format intentionally
//! evolves with archive versions, so this adapter only validates the stable
//! human-readable start marker and bounded file metadata.  It never walks
//! chunks, decodes geometry, opens textures, or executes plug-in payloads.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_3DM_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const START_MARKER: &[u8] = b"3D Geometry File Format ";

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    prefix
        .windows(START_MARKER.len())
        .any(|window| window == START_MARKER)
}

struct RhinoPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for RhinoPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "3dm".into();
        if page.title.is_empty() {
            page.title = "Rhino 3DM header".into();
        }
        page.description = "OpenNURBS 3DM marker and file metadata are rendered as bounded inert rows; model chunks and external resources are not decoded".into();
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
    let max_bytes = options.max_input_bytes.min(MAX_3DM_BYTES);
    if metadata.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "3DM input exceeds maximum bytes ({max_bytes})"
        )));
    }
    let mut file = File::open(path)?;
    let header_len = metadata.len().min(MAX_HEADER_BYTES as u64) as usize;
    let mut header = vec![0u8; header_len];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut header)?;
    let marker_offset = header
        .windows(START_MARKER.len())
        .position(|window| window == START_MARKER)
        .ok_or_else(|| {
            Error::InvalidInput("3DM start marker was not found in the bounded header".into())
        })?;
    let rows = vec![
        vec!["Start marker".into(), "3D Geometry File Format".into()],
        vec!["Marker offset".into(), marker_offset.to_string()],
        vec!["File bytes".into(), metadata.len().to_string()],
        vec!["Header bytes read".into(), header_len.to_string()],
    ];
    let warnings = vec![
        "3DM OpenNURBS chunks, NURBS curves/surfaces, meshes, layers, materials, annotations, thumbnails, and plug-in data are not decoded".into(),
        "Textures, external references, scripts, plug-in execution, and file-path resources remain inert and are never opened".into(),
    ];
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Rhino 3DM header".into(),
        },
        HtmlBlock::Paragraph {
            text: "Only the stable OpenNURBS start marker and bounded file metadata are inspected. The binary model archive is never expanded or executed.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows,
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = RhinoPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}
