//! Data structures representing parsed DXF elements and drawing hierarchy.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub name: String,
    pub color_hex: String,
    pub linetype: String,
    pub is_off: bool,
    pub is_frozen: bool,
    pub line_weight: Option<f64>,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            name: "0".into(),
            color_hex: "#000000".into(),
            linetype: "CONTINUOUS".into(),
            is_off: false,
            is_frozen: false,
            line_weight: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineType {
    pub name: String,
    pub description: String,
    /// Positive values: dash lengths. Negative values: space lengths. Zero: dot.
    pub pattern: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LwVertex {
    pub x: f64,
    pub y: f64,
    pub bulge: f64,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum HatchBoundary {
    Polyline {
        vertices: Vec<LwVertex>,
        is_closed: bool,
    },
    Edges(Vec<Entity>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entity {
    Line {
        start: (f64, f64),
        end: (f64, f64),
        layer: String,
        color: Option<String>,
        line_weight: Option<f64>,
        linetype: Option<String>,
    },
    Point {
        pt: (f64, f64),
        layer: String,
        color: Option<String>,
    },
    Circle {
        center: (f64, f64),
        radius: f64,
        layer: String,
        color: Option<String>,
        line_weight: Option<f64>,
        linetype: Option<String>,
    },
    Arc {
        center: (f64, f64),
        radius: f64,
        start_deg: f64,
        end_deg: f64,
        layer: String,
        color: Option<String>,
        line_weight: Option<f64>,
        linetype: Option<String>,
    },
    Ellipse {
        center: (f64, f64),
        major_axis: (f64, f64),
        axis_ratio: f64,
        start_param: f64,
        end_param: f64,
        layer: String,
        color: Option<String>,
        line_weight: Option<f64>,
        linetype: Option<String>,
    },
    LwPolyline {
        vertices: Vec<LwVertex>,
        is_closed: bool,
        layer: String,
        color: Option<String>,
        line_weight: Option<f64>,
        linetype: Option<String>,
    },
    Spline {
        degree: usize,
        control_points: Vec<(f64, f64)>,
        knots: Vec<f64>,
        is_closed: bool,
        layer: String,
        color: Option<String>,
        line_weight: Option<f64>,
    },
    Solid {
        points: [(f64, f64); 4],
        layer: String,
        color: Option<String>,
    },
    Text {
        text: String,
        insert: (f64, f64),
        height: f64,
        rotation_deg: f64,
        layer: String,
        color: Option<String>,
        h_align: u8,
        v_align: u8,
    },
    MText {
        text: String,
        insert: (f64, f64),
        height: f64,
        rotation_deg: f64,
        layer: String,
        color: Option<String>,
        attachment: u8,
    },
    Insert {
        block_name: String,
        insert: (f64, f64),
        scale: (f64, f64),
        rotation_deg: f64,
        layer: String,
        color: Option<String>,
        line_weight: Option<f64>,
    },
    Hatch {
        boundaries: Vec<HatchBoundary>,
        is_solid: bool,
        pattern_name: String,
        layer: String,
        color: Option<String>,
    },
    Dimension {
        block_name: Option<String>,
        text: String,
        insert: (f64, f64),
        layer: String,
        color: Option<String>,
    },
    Leader {
        vertices: Vec<(f64, f64)>,
        layer: String,
        color: Option<String>,
    },
}

impl Entity {
    #[must_use]
    pub fn layer(&self) -> &str {
        match self {
            Self::Line { layer, .. }
            | Self::Point { layer, .. }
            | Self::Circle { layer, .. }
            | Self::Arc { layer, .. }
            | Self::Ellipse { layer, .. }
            | Self::LwPolyline { layer, .. }
            | Self::Spline { layer, .. }
            | Self::Solid { layer, .. }
            | Self::Text { layer, .. }
            | Self::MText { layer, .. }
            | Self::Insert { layer, .. }
            | Self::Hatch { layer, .. }
            | Self::Dimension { layer, .. }
            | Self::Leader { layer, .. } => layer.as_str(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub name: String,
    pub base_point: (f64, f64),
    pub entities: Vec<Entity>,
}

#[derive(Clone, Debug, Default)]
pub struct DxfDocument {
    pub layers: HashMap<String, Layer>,
    pub linetypes: HashMap<String, LineType>,
    pub blocks: HashMap<String, Block>,
    pub entities: Vec<Entity>,
    pub ext_min: Option<(f64, f64)>,
    pub ext_max: Option<(f64, f64)>,
    pub ins_units: u16,
}

impl DxfDocument {
    /// Finds a layer by name, ignoring ASCII case according to DXF specification.
    #[must_use]
    pub fn find_layer(&self, name: &str) -> Option<&Layer> {
        if let Some(l) = self.layers.get(name) {
            return Some(l);
        }
        let lower = name.to_ascii_lowercase();
        self.layers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&lower))
            .map(|(_, v)| v)
    }

    /// Finds a block by name, ignoring ASCII case.
    #[must_use]
    pub fn find_block(&self, name: &str) -> Option<&Block> {
        if let Some(b) = self.blocks.get(name) {
            return Some(b);
        }
        let lower = name.to_ascii_lowercase();
        self.blocks
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&lower))
            .map(|(_, v)| v)
    }

    /// Finds a linetype by name, ignoring ASCII case.
    #[must_use]
    pub fn find_linetype(&self, name: &str) -> Option<&LineType> {
        if let Some(lt) = self.linetypes.get(name) {
            return Some(lt);
        }
        let lower = name.to_ascii_lowercase();
        self.linetypes
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&lower))
            .map(|(_, v)| v)
    }
}
