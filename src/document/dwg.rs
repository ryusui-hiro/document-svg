//! Autodesk DWG previews.
//!
//! DWG is a binary CAD database whose specification Autodesk does not
//! publish. Its first six ASCII bytes identify the drawing version (for
//! example `AC1027`). For the AC1009 (AutoCAD R11/R12) variant, the model
//! space entities are decoded into real vector geometry by
//! [`crate::cad::dwg`] (see that module for the format sources consulted).
//! Every other version, and any AC1009 file whose binary layout does not
//! validate, falls back to this bounded header-only preview: only the
//! fixed six-byte version identifier and file size are inspected, and the
//! binary drawing database is never executed or expanded.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_DWG_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    if prefix.len() < 6 {
        return false;
    }
    let code = String::from_utf8_lossy(&prefix[..6]);
    known_version(&code).is_some()
        || (code.starts_with("AC10") && code[4..].bytes().all(|b| b.is_ascii_digit()))
}

struct DwgPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for DwgPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "dwg".into();
        if page.title.is_empty() {
            page.title = "Autodesk DWG header".into();
        }
        page.description = "DWG version/header metadata is rendered as bounded inert rows; binary entities and external resources are not decoded".into();
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
    let max_bytes = options.max_input_bytes.min(MAX_DWG_BYTES);
    if metadata.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "DWG input exceeds maximum bytes ({max_bytes})"
        )));
    }
    let mut file = File::open(path)?;
    let mut header = [0u8; 6];
    file.read_exact(&mut header)?;
    let code = std::str::from_utf8(&header)
        .map_err(|_| Error::InvalidInput("DWG version header is not ASCII".into()))?;
    if !looks_like_prefix(&header) {
        return Err(Error::InvalidInput(format!(
            "unsupported or invalid DWG version header '{code}'"
        )));
    }

    let mut r12_decode_failed = false;
    if code == "AC1009" {
        let bytes = read_limited_file(path, max_bytes, "DWG input")?;
        if let Some(warnings) = crate::cad::dwg::convert(&bytes, sink)? {
            return Ok(warnings);
        }
        // Version matched but the binary layout didn't validate as a
        // well-formed AC1009 file (corrupt, nonstandard, or a pre-release
        // variant this reader doesn't recognize); fall through to the
        // header-only preview below rather than failing outright.
        r12_decode_failed = true;
    }

    let release = known_version(code).unwrap_or("unknown AutoCAD release");
    let rows = vec![
        vec!["Version code".into(), code.into()],
        vec!["Release".into(), release.into()],
        vec!["File bytes".into(), metadata.len().to_string()],
        vec!["Header bytes read".into(), "6".into()],
    ];
    let warnings = if r12_decode_failed {
        vec![
            "This file's version header is AC1009 (R11/R12), but its binary layout did not match the expected structure, so entities were not decoded".into(),
            "Embedded objects, VBA/ActiveX content, external references, file paths, and plotting/solver operations remain inert".into(),
        ]
    } else {
        vec![
            "DWG sections, entities, blocks, layers, proxy graphics, text, styles, thumbnails, and object maps are not decoded".into(),
            "Embedded objects, VBA/ActiveX content, external references, file paths, and plotting/solver operations remain inert".into(),
        ]
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Autodesk DWG header".into(),
        },
        HtmlBlock::Paragraph {
            text: "Only the fixed six-byte DWG version identifier and bounded file metadata are inspected. The binary drawing database is never executed or expanded.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows,
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = DwgPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn known_version(code: &str) -> Option<&'static str> {
    Some(match code {
        "AC1002" => "AutoCAD R2.5",
        "AC1003" => "AutoCAD R2.6",
        "AC1004" => "AutoCAD R9",
        "AC1006" => "AutoCAD R10",
        "AC1009" => "AutoCAD R11/R12",
        "AC1012" => "AutoCAD R13",
        "AC1014" => "AutoCAD R14",
        "AC1015" => "AutoCAD 2000/2000i/2002",
        "AC1018" => "AutoCAD 2004/2005/2006",
        "AC1021" => "AutoCAD 2007/2008/2009",
        "AC1024" => "AutoCAD 2010/2011/2012",
        "AC1027" => "AutoCAD 2013–2017",
        "AC1032" => "AutoCAD 2018–2024",
        _ => return None,
    })
}
