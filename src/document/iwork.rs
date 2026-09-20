//! Bounded Apple iWork package previews.
//!
//! Pages, Numbers, and Keynote documents are commonly distributed as package
//! directories or ZIP-backed packages containing opaque IWA protobuf parts.
//! This adapter handles the ZIP-backed form only.  It validates the archive
//! through the shared bounded package reader and reports known marker entries;
//! IWA protobuf payloads, previews, external links, and embedded media are
//! never decoded or opened.

use std::fs;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ooxml::ZipPackage;
use crate::table::{TableAlign, TableData};

const MAX_IWORK_BYTES: u64 = 512 * 1024 * 1024;
const MAX_IWORK_ENTRIES: usize = 100_000;

#[derive(Clone, Copy)]
enum Kind {
    Pages,
    Numbers,
    Keynote,
}

impl Kind {
    fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "pages" => Some(Self::Pages),
            "numbers" => Some(Self::Numbers),
            "key" => Some(Self::Keynote),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pages => "Pages",
            Self::Numbers => "Numbers",
            Self::Keynote => "Keynote",
        }
    }
}

struct IworkPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
    title: &'static str,
}

impl PageConsumer for IworkPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "iwork".into();
        if page.title.is_empty() {
            page.title = self.title.into();
        }
        page.description = "Apple iWork package structure is rendered as bounded inert metadata; IWA protobuf payloads and embedded resources are not decoded".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

/// Recognize a ZIP-backed iWork package by known marker entries.  This avoids
/// treating arbitrary ZIP files as Pages/Numbers/Keynote when the extension is
/// missing.
pub(crate) fn looks_like_archive(path: &Path) -> bool {
    if fs::metadata(path)
        .ok()
        .is_none_or(|metadata| metadata.len() > MAX_IWORK_BYTES)
    {
        return false;
    }
    let Ok(mut package) = ZipPackage::open(path, 32 * 1024 * 1024) else {
        return false;
    };
    package.entry_count() <= MAX_IWORK_ENTRIES
        && [
            "Index/Document.iwa",
            "Index/Presentation.iwa",
            "Index/Sheet.iwa",
            "Metadata/Properties.plist",
            "QuickLook/Preview.pdf",
        ]
        .into_iter()
        .any(|name| package.contains(name))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let kind = Kind::from_path(path).ok_or_else(|| {
        Error::InvalidInput("iWork package extension must be .pages, .numbers, or .key".into())
    })?;
    let metadata = fs::metadata(path)?;
    let max_bytes = options.max_input_bytes.min(MAX_IWORK_BYTES);
    if metadata.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "iWork input exceeds maximum bytes ({max_bytes})"
        )));
    }
    let mut package = ZipPackage::open(path, options.max_zip_entry_bytes)?;
    let entry_count = package.entry_count();
    if entry_count > MAX_IWORK_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "iWork package entries exceed {MAX_IWORK_ENTRIES}"
        )));
    }
    let marker_names = [
        ("Index/Document.iwa", "document payload"),
        ("Index/Presentation.iwa", "presentation payload"),
        ("Index/Sheet.iwa", "spreadsheet payload"),
        ("Metadata/Properties.plist", "metadata plist"),
        ("QuickLook/Preview.pdf", "QuickLook preview"),
        ("QuickLook/Thumbnail.png", "QuickLook thumbnail"),
    ];
    let mut rows = vec![
        vec!["Application".into(), kind.label().into()],
        vec!["Package bytes".into(), metadata.len().to_string()],
        vec!["ZIP entries".into(), entry_count.to_string()],
    ];
    let mut marker_count = 0usize;
    for (name, label) in marker_names {
        if package.contains(name) {
            marker_count += 1;
            rows.push(vec![label.into(), "present".into()]);
        }
    }
    rows.push(vec!["Known markers".into(), marker_count.to_string()]);
    if marker_count == 0 {
        return Err(Error::InvalidInput(
            "iWork ZIP package does not contain a recognized Index/Metadata/QuickLook marker"
                .into(),
        ));
    }
    let title = match kind {
        Kind::Pages => "Apple Pages package",
        Kind::Numbers => "Apple Numbers package",
        Kind::Keynote => "Apple Keynote package",
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: title.into(),
        },
        HtmlBlock::Paragraph {
            text: "The ZIP-backed iWork package is valid. Opaque IWA records are summarized by bounded marker metadata without decoding document content or opening linked resources.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows,
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "IWA protobuf records, document text, formulas, slide geometry, styles, previews, and embedded media are not decoded".into(),
        "External links, package file references, scripts, macros, and application operations remain inert and are never opened".into(),
    ];
    let mut page_sink = IworkPageSink {
        inner: sink,
        warnings: &warnings,
        title,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}
