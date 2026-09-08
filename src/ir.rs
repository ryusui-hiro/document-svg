//! Output-independent page model used by all input converters.
//!
//! Coordinates are expressed in points with the origin at the page's top-left.
//! [`Page::nodes`] is paint-ordered: later nodes are drawn above earlier nodes.
//! Matrix values use SVG's `[a, b, c, d, e, f]` affine convention.

use serde::Serialize;

pub type Matrix = [f64; 6];
pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OuterShadow {
    pub color: String,
    pub opacity: f64,
    pub blur_radius: f64,
    pub distance: f64,
    pub direction_degrees: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GlowEffect {
    pub color: String,
    pub opacity: f64,
    pub radius: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageColorEffect {
    Duotone {
        dark: String,
        light: String,
    },
    Grayscale,
    Luminance {
        brightness: f64,
        contrast: f64,
    },
    ColorChange {
        from: String,
        to: String,
        to_opacity: f64,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SourceMeta {
    pub kind: String,
    pub source_id: String,
    pub semantic_role: String,
    pub alt_text: String,
    pub blend_mode: String,
    pub mask_id: String,
    pub image_rendering: String,
    pub isolation: bool,
    pub alpha_is_shape: bool,
    pub shape_rendering: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outer_shadow: Option<OuterShadow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glow: Option<GlowEffect>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub image_effects: Vec<ImageColorEffect>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Paint {
    #[default]
    None,
    Solid {
        color: String,
        opacity: f64,
    },
    LinearGradient(Box<LinearGradient>),
    RadialGradient(Box<RadialGradient>),
    PatternRef {
        id: String,
        opacity: f64,
    },
}

impl Paint {
    #[must_use]
    pub fn solid(color: impl Into<String>) -> Self {
        Self::Solid {
            color: color.into(),
            opacity: 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GradientStop {
    pub offset: f64,
    pub color: String,
    pub opacity: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LinearGradient {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
    pub stops: Vec<GradientStop>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RadialGradient {
    pub fx: f64,
    pub fy: f64,
    pub fr: f64,
    pub cx: f64,
    pub cy: f64,
    pub radius: f64,
    pub transform: Matrix,
    pub stops: Vec<GradientStop>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Stroke {
    pub paint: Paint,
    pub width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f64,
    pub dash_array: Vec<f64>,
    pub dash_offset: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TextRun {
    pub text: String,
    pub font_family: String,
    pub font_size: f64,
    pub bold: bool,
    pub italic: bool,
    pub fill: Paint,
    pub baseline_shift: f64,
    /// Optional glyph origins in the text node's local coordinate space.
    ///
    /// PDF text frequently carries exact per-glyph advances that cannot be
    /// reproduced by a substitute font. When this has one entry per Unicode
    /// scalar in `text`, the SVG writer emits positioned child tspans.
    pub glyph_x_offsets: Vec<f64>,
    /// Optional expected advance in the text node's local coordinate space.
    ///
    /// PDF fonts frequently use widths that differ materially from the
    /// browser fallback font. SVG `textLength` keeps the editable text aligned
    /// to the source document without forcing all text to outlines.
    pub target_advance: Option<f64>,
}

impl Default for TextRun {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_family: "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into(),
            font_size: 12.0,
            bold: false,
            italic: false,
            fill: Paint::solid("#000000"),
            baseline_shift: 0.0,
            glyph_x_offsets: Vec::new(),
            target_advance: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAnchor {
    #[default]
    Start,
    Middle,
    End,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    Path {
        id: String,
        d: String,
        fill_rule: String,
        fill: Paint,
        stroke: Stroke,
        transform: Matrix,
        clip_id: Option<String>,
        meta: SourceMeta,
    },
    Text {
        id: String,
        x: f64,
        y: f64,
        runs: Vec<TextRun>,
        anchor: TextAnchor,
        transform: Matrix,
        opacity: f64,
        stroke: Stroke,
        clip_id: Option<String>,
        meta: SourceMeta,
    },
    Image {
        id: String,
        href: String,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        transform: Matrix,
        opacity: f64,
        clip_id: Option<String>,
        meta: SourceMeta,
    },
    Group {
        id: String,
        nodes: Vec<Node>,
        transform: Matrix,
        opacity: f64,
        clip_id: Option<String>,
        meta: SourceMeta,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClipPath {
    pub id: String,
    pub d: String,
    pub transform: Matrix,
    pub fill_rule: String,
    pub parent_id: Option<String>,
    pub additional_paths: Vec<ClipMember>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ClipMember {
    pub d: String,
    pub transform: Matrix,
    pub fill_rule: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MaskDefinition {
    pub id: String,
    pub mask_type: String,
    pub nodes: Vec<Node>,
    pub transfer_values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TilingPatternDefinition {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub transform: Matrix,
    pub nodes: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Page {
    pub number: usize,
    pub width: f64,
    pub height: f64,
    pub source_format: String,
    pub title: String,
    pub description: String,
    pub nodes: Vec<Node>,
    pub clips: Vec<ClipPath>,
    pub masks: Vec<MaskDefinition>,
    pub patterns: Vec<TilingPatternDefinition>,
    pub warnings: Vec<String>,
}

impl Page {
    #[must_use]
    pub fn new(number: usize, width: f64, height: f64, source_format: &str) -> Self {
        Self {
            number,
            width,
            height,
            source_format: source_format.into(),
            title: String::new(),
            description: String::new(),
            nodes: Vec::new(),
            clips: Vec::new(),
            masks: Vec::new(),
            patterns: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn warn(&mut self, warning: impl Into<String>) {
        let warning = warning.into();
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }
}

#[must_use]
pub fn compose(left: Matrix, right: Matrix) -> Matrix {
    let [la, lb, lc, ld, le, lf] = left;
    let [ra, rb, rc, rd, re, rf] = right;
    [
        la * ra + lc * rb,
        lb * ra + ld * rb,
        la * rc + lc * rd,
        lb * rc + ld * rd,
        la * re + lc * rf + le,
        lb * re + ld * rf + lf,
    ]
}

#[must_use]
pub fn transform_point(matrix: Matrix, x: f64, y: f64) -> (f64, f64) {
    let [a, b, c, d, e, f] = matrix;
    (a * x + c * y + e, b * x + d * y + f)
}
