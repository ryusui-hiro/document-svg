//! Memory-bounded conversion of PDF, OOXML, and draw.io documents to
//! deterministic SVG, plus appearance-preserving SVG packaging into OOXML.
//!
//! Use [`convert_path`] for normal file conversion. It detects PDF, PPTX, XLSX,
//! DOCX, or draw.io from the input extension, writes one SVG per output page,
//! and writes `conversion.json` beside the SVG files.
//!
//! Use the public [`ir`] and [`svg`] modules only when constructing a page from
//! custom drawing data. Build an [`ir::Page`], append [`ir::Node`] values in
//! paint order, then pass it to [`svg::write_page`]. The format-specific `pdf`,
//! `ooxml`, and `drawio` modules are internal implementation details.
//!
//! Call [`svg_to_document`] with one SVG or a directory of SVG pages to create
//! PPTX, DOCX, XLSX or draw.io files containing those SVGs as vector images.
//! This does not reconstruct the original Office document's semantic structure.
//! A draw.io output is the exception: SVG pages that still carry the diagram
//! source draw.io writes into its own exports are restored as editable
//! diagrams rather than packaged as pictures.

mod convert;
mod drawio;
mod error;
pub mod ir;
mod ooxml;
mod pdf;
mod pdf_base14;
mod reverse;
pub mod svg;

pub use convert::{ConversionReport, ConvertOptions, PageReport, SourceFormat, convert_path};
pub use error::{Error, Result};
/// Former name of [`ReverseFormat`], kept so existing callers still compile.
pub use reverse::ReverseFormat as OpenXmlFormat;
/// Former name of [`svg_to_document`], kept so existing callers still compile.
pub use reverse::svg_to_document as svg_to_openxml;
pub use reverse::{ReverseFormat, ReverseOptions, ReverseReport, svg_to_document};
