//! OpenDocument Graphics (ODG/OTG/FODG) vector drawing preview.
//!
//! ODG shares its page and drawing object vocabulary with ODP, but stores pages
//! under `office:drawing`. The bounded shared ODF drawing parser retains common
//! shapes and text while reporting unsupported effects and embedded media.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::Result;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    crate::document::odp::convert_drawing(path, options, sink)
}
