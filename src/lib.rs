//! Bounded conversion of PDF, Office, web, text, diagram, CAD/CAM/3D, simulation,
//! raster, chart, table, and math documents to deterministic SVG pages, plus SVG
//! transforms and reverse packaging into Office, CAD, diagram, data, and UI-code formats.
//!
//! Use [`convert_path`] for normal file conversion. It detects supported inputs
//! from their extensions or a bounded content prefix, writes one SVG per output
//! page, and writes `conversion.json` beside the SVG files.
//!
//! Use the public [`cad`], [`ir`] and [`svg`] modules when working directly with
//! CAD entities or constructing a page from custom drawing data.
//!
//! Call [`svg_to_document`] with one SVG or a directory of SVG pages to package
//! Office, draw.io, CAD/CAM/3D, simulation, diagram, table, math, image, or UI-code
//! output. Office exports preserve page appearance; they do not reconstruct the
//! source application's paragraphs, cells, formulas, or editable shapes.

#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]

pub mod cad;
pub mod chart;
pub mod code;
mod convert;
pub mod diagram;
pub mod document;
mod drawio;
mod error;
mod geospatial;
pub mod ir;
mod jpeg2000;
mod local_resource;
pub mod math;
pub mod metafile;
mod ooxml;
mod pdf;
mod pdf_base14;
pub mod qr;
mod reverse;
pub mod svg;
pub mod table;
pub mod transform;
pub mod vectorize;

pub use convert::{
    ByteConversionReport, ByteConvertLimits, ConvertOptions, SourceFormat, SvgPage, TextSpan,
    convert_bytes,
};
#[cfg(not(target_arch = "wasm32"))]
pub use convert::{ConversionReport, PageReport, convert_path};
pub use error::{Error, Result};
/// Former name of [`ReverseFormat`], kept so existing callers still compile.
#[cfg(not(target_arch = "wasm32"))]
pub use reverse::ReverseFormat as OpenXmlFormat;
/// Former name of [`svg_to_document`], kept so existing callers still compile.
#[cfg(not(target_arch = "wasm32"))]
pub use reverse::svg_to_document as svg_to_openxml;
#[cfg(not(target_arch = "wasm32"))]
pub use reverse::{ReverseFormat, ReverseOptions, ReverseReport, svg_to_document};
pub use transform::{TransformOptions, transform_svg};
