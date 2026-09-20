//! Backwards-compatible re-export of SVG Core reading and geometry primitives.
//!
//! Core SVG primitives have been moved to [`crate::svg`] as part of the universal SVG hub refactoring.
//! CAD writers and external callers continue to access them through this module without breaking changes.

pub use crate::svg::color::*;
pub use crate::svg::geometry::*;
pub use crate::svg::reader::*;
