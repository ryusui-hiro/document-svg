//! Bounded HDF5 and CGNS/HDF5 container previews.
//!
//! HDF5 is a hierarchical binary container whose superblock can be preceded
//! by a user block.  This adapter validates the official signature and reads
//! only the small superblock header; it does not walk arbitrary object
//! headers, decompress datasets, follow links, or evaluate CGNS payloads.
//! That makes large scientific and CFD files cheap to inspect while keeping
//! untrusted metadata and external references inert.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ir::Page;
use crate::table::{TableAlign, TableData};

const HDF5_SIGNATURE: [u8; 8] = [0x89, b'H', b'D', b'F', 0x0d, 0x0a, 0x1a, 0x0a];
const MAX_HDF5_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_SIGNATURE_PROBES: usize = 64;

#[derive(Clone, Copy, Debug)]
struct Superblock {
    offset: u64,
    version: u8,
    offset_size: u8,
    length_size: u8,
    consistency_flags: Option<u32>,
}

struct Hdf5PageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    source_format: &'static str,
    title: &'static str,
    description: &'static str,
    warnings: &'a [String],
}

impl PageConsumer for Hdf5PageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = self.source_format.into();
        if page.title.is_empty() {
            page.title = self.title.into();
        }
        page.description = self.description.into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

/// Detect an HDF5 signature at the permitted user-block offsets without
/// reading the whole file into memory.
pub(crate) fn looks_like_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() < HDF5_SIGNATURE.len() as u64 || metadata.len() > MAX_HDF5_BYTES {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    find_signature(&mut file, metadata.len()).is_ok_and(|value| value.is_some())
}

pub(crate) fn convert_hdf5(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_kind(
        path,
        options,
        sink,
        "hdf5",
        "HDF5 scientific dataset",
        "HDF5 superblock metadata is rendered as bounded inert rows; groups, datasets, attributes, links and data filters are not traversed",
        "HDF5 dataset values, compression filters, virtual datasets, external links and user-defined callbacks are never loaded or executed",
    )
}

pub(crate) fn convert_cgns(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_kind(
        path,
        options,
        sink,
        "cgns",
        "CGNS/HDF5 CFD database",
        "CGNS/HDF5 superblock metadata is rendered as bounded inert rows; CGNS SIDS nodes, grids and solution arrays are not traversed",
        "CGNS datasets, links, external files, compression filters and solver operations are never loaded or executed",
    )
}

fn convert_kind(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    source_format: &'static str,
    title: &'static str,
    description: &'static str,
    warning: &'static str,
) -> Result<Vec<String>> {
    let metadata = fs::metadata(path)?;
    let max_bytes = options.max_input_bytes.min(MAX_HDF5_BYTES);
    if metadata.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "{source_format} input exceeds maximum bytes ({max_bytes})"
        )));
    }
    let mut file = File::open(path)?;
    let superblock = find_signature(&mut file, metadata.len())?.ok_or_else(|| {
        Error::InvalidInput(format!(
            "{source_format} input does not contain an HDF5 signature at a permitted offset"
        ))
    })?;
    let mut rows = vec![
        vec!["Container bytes".into(), metadata.len().to_string()],
        vec!["Signature offset".into(), superblock.offset.to_string()],
        vec!["Superblock version".into(), superblock.version.to_string()],
        vec![
            "Offset width".into(),
            format!("{} bytes", superblock.offset_size),
        ],
        vec![
            "Length width".into(),
            format!("{} bytes", superblock.length_size),
        ],
    ];
    if let Some(flags) = superblock.consistency_flags {
        rows.push(vec![
            "File consistency flags".into(),
            format!("0x{flags:08x}"),
        ]);
    }
    let mut warnings = vec![warning.to_owned()];
    if superblock.version > 3 {
        warnings.push(format!(
            "HDF5 superblock version {} is newer than the documented 0–3 range",
            superblock.version
        ));
    }
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: title.into(),
        },
        HtmlBlock::Paragraph {
            text: description.into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows: std::mem::take(&mut rows),
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = Hdf5PageSink {
        inner: sink,
        source_format,
        title,
        description,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn find_signature(file: &mut File, file_len: u64) -> Result<Option<Superblock>> {
    let mut offset = 0u64;
    let mut signature = [0u8; 8];
    for _ in 0..MAX_SIGNATURE_PROBES {
        if offset.saturating_add(8) > file_len {
            break;
        }
        file.seek(SeekFrom::Start(offset))?;
        if file.read_exact(&mut signature).is_ok() && signature == HDF5_SIGNATURE {
            return parse_superblock(file, offset, file_len).map(Some);
        }
        offset = if offset == 0 {
            512
        } else {
            offset.saturating_mul(2)
        };
    }
    Ok(None)
}

fn parse_superblock(file: &mut File, offset: u64, file_len: u64) -> Result<Superblock> {
    let mut header = [0u8; 64];
    file.seek(SeekFrom::Start(offset))?;
    let available = file_len.saturating_sub(offset).min(header.len() as u64) as usize;
    file.read_exact(&mut header[..available])?;
    if available <= 8 {
        return Err(Error::InvalidInput(
            "HDF5 signature is missing a superblock version".into(),
        ));
    }
    let version = header[8];
    let (offset_size, length_size, consistency_flags) = match version {
        0 | 1 => {
            if available < 24 {
                return Err(Error::InvalidInput(
                    "HDF5 v0/v1 superblock is truncated".into(),
                ));
            }
            (
                header[13],
                header[14],
                Some(u32::from_le_bytes(header[20..24].try_into().unwrap())),
            )
        }
        2 | 3 => {
            if available < 12 {
                return Err(Error::InvalidInput(
                    "HDF5 v2/v3 superblock is truncated".into(),
                ));
            }
            (header[9], header[10], Some(u32::from(header[11])))
        }
        _ => (0, 0, None),
    };
    if version <= 3
        && (!matches!(offset_size, 2 | 4 | 8 | 16) || !matches!(length_size, 2 | 4 | 8 | 16))
    {
        return Err(Error::InvalidInput(format!(
            "HDF5 superblock has unsupported offset/length widths ({offset_size}/{length_size})"
        )));
    }
    Ok(Superblock {
        offset,
        version,
        offset_size,
        length_size,
        consistency_flags,
    })
}
