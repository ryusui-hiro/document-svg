//! Memory-bounded conversion of PDF and OOXML documents to deterministic SVG,
//! plus appearance-preserving SVG packaging into OOXML.
//!
//! Use [`convert_path`] for normal file conversion. It detects PDF, PPTX, XLSX,
//! or DOCX from the input extension, writes one SVG per output page, and writes
//! `conversion.json` beside the SVG files.
//!
//! Use the public [`ir`] and [`svg`] modules only when constructing a page from
//! custom drawing data. Build an [`ir::Page`], append [`ir::Node`] values in
//! paint order, then pass it to [`svg::write_page`]. The format-specific `pdf`
//! and `ooxml` modules are internal implementation details.
//!
//! Call [`svg_to_openxml`] with one SVG or a directory of SVG pages to create
//! PPTX, DOCX, or XLSX files containing those SVGs as vector images. This does
//! not reconstruct the original Office document's semantic structure.

mod convert;
mod error;
pub mod ir;
mod ooxml;
mod pdf;
mod pdf_base14;
mod reverse;
pub mod svg;

pub use convert::{ConversionReport, ConvertOptions, PageReport, SourceFormat, convert_path};
pub use error::{Error, Result};
pub use reverse::{OpenXmlFormat, ReverseOptions, ReverseReport, svg_to_openxml};
