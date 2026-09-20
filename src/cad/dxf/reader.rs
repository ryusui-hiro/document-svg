//! Streaming ASCII DXF reader with safety limits.

#![allow(clippy::collapsible_if, clippy::unnecessary_unwrap)]

use std::io::BufRead;

use crate::cad::color::{aci_to_hex, truecolor_to_hex};
use crate::cad::dxf::geometry::parse_cad_float;
use crate::cad::dxf::types::{Block, DxfDocument, Entity, Layer, LineType, LwVertex};
use crate::error::{Error, Result};

const MAX_DXF_LINES: usize = 5_000_000;
const MAX_ENTITIES: usize = 500_000;
const MAX_VERTICES_PER_POLYLINE: usize = 100_000;

#[inline]
fn parse_f64(s: &str) -> f64 {
    parse_cad_float(s).unwrap_or(0.0)
}

#[inline]
fn parse_f64_default(s: &str, default: f64) -> f64 {
    parse_cad_float(s).unwrap_or(default)
}

#[derive(Clone, Debug, PartialEq)]
pub struct DxfPair {
    pub code: i32,
    pub value: String,
}

fn decode_bytes_to_string(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    // Fallback to Windows-1252 / ANSI_1252 commonly used in AutoCAD DXF
    bytes
        .iter()
        .map(|&b| match b {
            0x80 => '€',
            0x82 => '‚',
            0x83 => 'ƒ',
            0x84 => '„',
            0x85 => '…',
            0x86 => '†',
            0x87 => '‡',
            0x88 => 'ˆ',
            0x89 => '‰',
            0x8A => 'Š',
            0x8B => '‹',
            0x8C => 'Œ',
            0x8E => 'Ž',
            0x91 => '‘',
            0x92 => '’',
            0x93 => '“',
            0x94 => '”',
            0x95 => '•',
            0x96 => '–',
            0x97 => '—',
            0x98 => '˜',
            0x99 => '™',
            0x9A => 'š',
            0x9B => '›',
            0x9C => 'œ',
            0x9E => 'ž',
            0x9F => 'Ÿ',
            _ => b as char,
        })
        .collect()
}

pub struct DxfReader<R> {
    reader: R,
    line_number: usize,
    raw_buffer: Vec<u8>,
}

impl<R: BufRead> DxfReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line_number: 0,
            raw_buffer: Vec::with_capacity(256),
        }
    }

    pub fn next_pair(&mut self) -> Result<Option<DxfPair>> {
        self.raw_buffer.clear();
        let bytes_read = self.reader.read_until(b'\n', &mut self.raw_buffer)?;
        if bytes_read == 0 {
            return Ok(None);
        }
        self.line_number += 1;
        if self.line_number > MAX_DXF_LINES {
            return Err(Error::LimitExceeded(format!(
                "DXF exceeds safety limit of {MAX_DXF_LINES} lines"
            )));
        }

        let code_line = decode_bytes_to_string(&self.raw_buffer);
        let code_str = code_line.trim();
        if code_str.is_empty() {
            return self.next_pair();
        }
        let code = code_str.parse::<i32>().map_err(|_| {
            Error::InvalidInput(format!(
                "DXF invalid group code '{code_str}' at line {}",
                self.line_number
            ))
        })?;

        self.raw_buffer.clear();
        let val_bytes = self.reader.read_until(b'\n', &mut self.raw_buffer)?;
        if val_bytes == 0 {
            return Ok(Some(DxfPair {
                code,
                value: String::new(),
            }));
        }
        self.line_number += 1;

        let val_line = decode_bytes_to_string(&self.raw_buffer);
        let trimmed_val = val_line
            .trim_end_matches(['\r', '\n'])
            .trim_start()
            .to_string();

        Ok(Some(DxfPair {
            code,
            value: trimmed_val,
        }))
    }
}

pub fn parse_dxf<R: BufRead>(reader: R) -> Result<DxfDocument> {
    let mut reader = DxfReader::new(reader);
    let mut doc = DxfDocument::default();
    let mut warnings = Vec::new();

    // Ensure default layer "0" exists
    doc.layers.insert("0".into(), Layer::default());

    let mut current_pair = reader.next_pair()?;

    while let Some(pair) = current_pair {
        if pair.code == 0 && pair.value.eq_ignore_ascii_case("SECTION") {
            current_pair = parse_section(&mut reader, &mut doc, &mut warnings)?;
        } else if pair.code == 0 && pair.value.eq_ignore_ascii_case("EOF") {
            break;
        } else {
            current_pair = reader.next_pair()?;
        }
    }

    Ok(doc)
}

fn parse_section<R: BufRead>(
    reader: &mut DxfReader<R>,
    doc: &mut DxfDocument,
    warnings: &mut Vec<String>,
) -> Result<Option<DxfPair>> {
    let section_name_pair = reader.next_pair()?;
    let section_name = match section_name_pair {
        Some(p) if p.code == 2 => p.value.to_ascii_uppercase(),
        other => return Ok(other),
    };

    let mut next_pair = reader.next_pair()?;

    match section_name.as_str() {
        "HEADER" => {
            next_pair = parse_header(reader, doc, next_pair)?;
        }
        "TABLES" => {
            next_pair = parse_tables(reader, doc, next_pair)?;
        }
        "BLOCKS" => {
            next_pair = parse_blocks(reader, doc, next_pair, warnings)?;
        }
        "ENTITIES" => {
            next_pair = parse_entities_section(reader, &mut doc.entities, next_pair, warnings)?;
        }
        _ => {
            // Skip unknown section until ENDSEC or EOF
            while let Some(pair) = next_pair {
                if pair.code == 0 && pair.value.eq_ignore_ascii_case("ENDSEC") {
                    return reader.next_pair();
                }
                if pair.code == 0 && pair.value.eq_ignore_ascii_case("EOF") {
                    return Ok(Some(pair));
                }
                next_pair = reader.next_pair()?;
            }
        }
    }

    Ok(next_pair)
}

fn parse_header<R: BufRead>(
    reader: &mut DxfReader<R>,
    doc: &mut DxfDocument,
    mut current_pair: Option<DxfPair>,
) -> Result<Option<DxfPair>> {
    let mut current_var = String::new();
    let mut x_val = 0.0;

    while let Some(pair) = current_pair {
        if pair.code == 0
            && (pair.value.eq_ignore_ascii_case("ENDSEC") || pair.value.eq_ignore_ascii_case("EOF"))
        {
            return reader.next_pair();
        }
        if pair.code == 9 {
            current_var = pair.value.to_ascii_uppercase();
        } else if current_var == "$EXTMIN" {
            if pair.code == 10 {
                x_val = parse_f64(&pair.value);
            } else if pair.code == 20 {
                let y = parse_f64(&pair.value);
                doc.ext_min = Some((x_val, y));
            }
        } else if current_var == "$EXTMAX" {
            if pair.code == 10 {
                x_val = parse_f64(&pair.value);
            } else if pair.code == 20 {
                let y = parse_f64(&pair.value);
                doc.ext_max = Some((x_val, y));
            }
        } else if current_var == "$INSUNITS" && pair.code == 70 {
            doc.ins_units = pair.value.parse().unwrap_or(0);
        }
        current_pair = reader.next_pair()?;
    }
    Ok(None)
}

fn parse_tables<R: BufRead>(
    reader: &mut DxfReader<R>,
    doc: &mut DxfDocument,
    mut current_pair: Option<DxfPair>,
) -> Result<Option<DxfPair>> {
    while let Some(pair) = current_pair {
        if pair.code == 0
            && (pair.value.eq_ignore_ascii_case("ENDSEC") || pair.value.eq_ignore_ascii_case("EOF"))
        {
            return reader.next_pair();
        }
        if pair.code == 0 && pair.value.eq_ignore_ascii_case("TABLE") {
            let table_type_pair = reader.next_pair()?;
            let table_type = match table_type_pair {
                Some(p) if p.code == 2 => p.value.to_ascii_uppercase(),
                _ => String::new(),
            };
            current_pair = parse_table(reader, doc, &table_type)?;
        } else {
            current_pair = reader.next_pair()?;
        }
    }
    Ok(None)
}

fn parse_table<R: BufRead>(
    reader: &mut DxfReader<R>,
    doc: &mut DxfDocument,
    table_type: &str,
) -> Result<Option<DxfPair>> {
    let mut current_pair = reader.next_pair()?;
    while let Some(pair) = current_pair {
        if pair.code == 0
            && (pair.value.eq_ignore_ascii_case("ENDTAB")
                || pair.value.eq_ignore_ascii_case("ENDSEC"))
        {
            return reader.next_pair();
        }
        if pair.code == 0 {
            if table_type == "LAYER" && pair.value.eq_ignore_ascii_case("LAYER") {
                current_pair = parse_layer_record(reader, doc)?;
                continue;
            } else if table_type == "LTYPE" && pair.value.eq_ignore_ascii_case("LTYPE") {
                current_pair = parse_ltype_record(reader, doc)?;
                continue;
            }
        }
        current_pair = reader.next_pair()?;
    }
    Ok(None)
}

fn parse_layer_record<R: BufRead>(
    reader: &mut DxfReader<R>,
    doc: &mut DxfDocument,
) -> Result<Option<DxfPair>> {
    let mut layer = Layer::default();
    let mut pair = reader.next_pair()?;

    while let Some(p) = pair {
        if p.code == 0 {
            if !layer.name.is_empty() {
                doc.layers.insert(layer.name.clone(), layer);
            }
            return Ok(Some(p));
        }
        match p.code {
            2 => layer.name = p.value,
            70 => {
                let flags: u16 = p.value.parse().unwrap_or(0);
                layer.is_frozen = (flags & 1) != 0;
            }
            62 => {
                let aci: i16 = p.value.parse().unwrap_or(7);
                if aci < 0 {
                    layer.is_off = true;
                }
                layer.color_hex = aci_to_hex(aci.abs());
            }
            420 => {
                let tc: u32 = p.value.parse().unwrap_or(0);
                layer.color_hex = truecolor_to_hex(tc);
            }
            6 => layer.linetype = p.value.to_ascii_uppercase(),
            370 => {
                let lw: i32 = p.value.parse().unwrap_or(-1);
                if lw >= 0 {
                    layer.line_weight = Some(lw as f64 / 100.0);
                }
            }
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    if !layer.name.is_empty() {
        doc.layers.insert(layer.name.clone(), layer);
    }
    Ok(None)
}

fn parse_ltype_record<R: BufRead>(
    reader: &mut DxfReader<R>,
    doc: &mut DxfDocument,
) -> Result<Option<DxfPair>> {
    let mut ltype = LineType {
        name: String::new(),
        description: String::new(),
        pattern: Vec::new(),
    };
    let mut pair = reader.next_pair()?;

    while let Some(p) = pair {
        if p.code == 0 {
            if !ltype.name.is_empty() {
                doc.linetypes.insert(ltype.name.to_ascii_uppercase(), ltype);
            }
            return Ok(Some(p));
        }
        match p.code {
            2 => ltype.name = p.value,
            3 => ltype.description = p.value,
            49 => {
                if let Some(v) = parse_cad_float(&p.value) {
                    ltype.pattern.push(v);
                }
            }
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    if !ltype.name.is_empty() {
        doc.linetypes.insert(ltype.name.to_ascii_uppercase(), ltype);
    }
    Ok(None)
}

fn parse_blocks<R: BufRead>(
    reader: &mut DxfReader<R>,
    doc: &mut DxfDocument,
    mut current_pair: Option<DxfPair>,
    warnings: &mut Vec<String>,
) -> Result<Option<DxfPair>> {
    while let Some(pair) = current_pair {
        if pair.code == 0
            && (pair.value.eq_ignore_ascii_case("ENDSEC") || pair.value.eq_ignore_ascii_case("EOF"))
        {
            return reader.next_pair();
        }
        if pair.code == 0 && pair.value.eq_ignore_ascii_case("BLOCK") {
            current_pair = parse_single_block(reader, doc, warnings)?;
        } else {
            current_pair = reader.next_pair()?;
        }
    }
    Ok(None)
}

fn parse_single_block<R: BufRead>(
    reader: &mut DxfReader<R>,
    doc: &mut DxfDocument,
    warnings: &mut Vec<String>,
) -> Result<Option<DxfPair>> {
    let mut block = Block {
        name: String::new(),
        base_point: (0.0, 0.0),
        entities: Vec::new(),
    };

    let mut pair = reader.next_pair()?;

    // Read BLOCK header attributes
    while let Some(p) = pair {
        if p.code == 0 {
            pair = Some(p);
            break;
        }
        match p.code {
            2 => block.name = p.value,
            10 => block.base_point.0 = parse_f64(&p.value),
            20 => block.base_point.1 = parse_f64(&p.value),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    // Read block entities until ENDBLK
    while let Some(p) = pair {
        if p.code == 0 {
            if p.value.eq_ignore_ascii_case("ENDBLK") {
                if !block.name.is_empty() {
                    doc.blocks.insert(block.name.clone(), block);
                }
                return reader.next_pair();
            }
            pair = parse_entity(reader, p.value.as_str(), &mut block.entities, warnings)?;
        } else {
            pair = reader.next_pair()?;
        }
    }

    if !block.name.is_empty() {
        doc.blocks.insert(block.name.clone(), block);
    }
    Ok(None)
}

fn parse_entities_section<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
    mut current_pair: Option<DxfPair>,
    warnings: &mut Vec<String>,
) -> Result<Option<DxfPair>> {
    while let Some(pair) = current_pair {
        if pair.code == 0 {
            let entity_type = pair.value.to_ascii_uppercase();
            if entity_type == "ENDSEC" || entity_type == "EOF" {
                return reader.next_pair();
            }
            current_pair = parse_entity(reader, entity_type.as_str(), entities, warnings)?;
        } else {
            current_pair = reader.next_pair()?;
        }
    }
    Ok(None)
}

fn parse_entity<R: BufRead>(
    reader: &mut DxfReader<R>,
    entity_type: &str,
    entities: &mut Vec<Entity>,
    _warnings: &mut Vec<String>,
) -> Result<Option<DxfPair>> {
    if entities.len() >= MAX_ENTITIES {
        return Err(Error::LimitExceeded(format!(
            "DXF exceeds safety limit of {MAX_ENTITIES} entities"
        )));
    }

    match entity_type {
        "LINE" => parse_line(reader, entities),
        "POINT" => parse_point(reader, entities),
        "CIRCLE" => parse_circle(reader, entities),
        "ARC" => parse_arc(reader, entities),
        "ELLIPSE" => parse_ellipse(reader, entities),
        "LWPOLYLINE" => parse_lwpolyline(reader, entities),
        "POLYLINE" => parse_old_polyline(reader, entities),
        "SOLID" | "TRACE" => parse_solid(reader, entities),
        "TEXT" => parse_text(reader, entities),
        "MTEXT" => parse_mtext(reader, entities),
        "INSERT" => parse_insert(reader, entities),
        "SPLINE" => parse_spline(reader, entities),
        "HATCH" => parse_hatch(reader, entities),
        "DIMENSION" => parse_dimension(reader, entities),
        "LEADER" | "MULTILEADER" => parse_leader(reader, entities),
        _ => {
            // Unsupported or ignored entity
            skip_entity(reader)
        }
    }
}

fn skip_entity<R: BufRead>(reader: &mut DxfReader<R>) -> Result<Option<DxfPair>> {
    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            return Ok(Some(p));
        }
        pair = reader.next_pair()?;
    }
    Ok(None)
}

fn parse_line<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut line_weight = None;
    let mut linetype = None;
    let mut x1 = 0.0;
    let mut y1 = 0.0;
    let mut x2 = 0.0;
    let mut y2 = 0.0;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::Line {
                start: (x1, y1),
                end: (x2, y2),
                layer,
                color,
                line_weight,
                linetype,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            370 => {
                if let Ok(lw) = p.value.parse::<i32>() {
                    if lw >= 0 {
                        line_weight = Some(lw as f64 / 100.0);
                    }
                }
            }
            6 => linetype = Some(p.value.to_ascii_uppercase()),
            10 => x1 = parse_f64(&p.value),
            20 => y1 = parse_f64(&p.value),
            11 => x2 = parse_f64(&p.value),
            21 => y2 = parse_f64(&p.value),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::Line {
        start: (x1, y1),
        end: (x2, y2),
        layer,
        color,
        line_weight,
        linetype,
    });
    Ok(None)
}

fn parse_point<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut x = 0.0;
    let mut y = 0.0;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::Point {
                pt: (x, y),
                layer,
                color,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            10 => x = parse_f64(&p.value),
            20 => y = parse_f64(&p.value),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::Point {
        pt: (x, y),
        layer,
        color,
    });
    Ok(None)
}

fn parse_circle<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut line_weight = None;
    let mut linetype = None;
    let mut cx = 0.0;
    let mut cy = 0.0;
    let mut radius = 0.0;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::Circle {
                center: (cx, cy),
                radius,
                layer,
                color,
                line_weight,
                linetype,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            370 => {
                if let Ok(lw) = p.value.parse::<i32>() {
                    if lw >= 0 {
                        line_weight = Some(lw as f64 / 100.0);
                    }
                }
            }
            6 => linetype = Some(p.value.to_ascii_uppercase()),
            10 => cx = parse_f64(&p.value),
            20 => cy = parse_f64(&p.value),
            40 => radius = parse_f64(&p.value),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::Circle {
        center: (cx, cy),
        radius,
        layer,
        color,
        line_weight,
        linetype,
    });
    Ok(None)
}

fn parse_arc<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut line_weight = None;
    let mut linetype = None;
    let mut cx = 0.0;
    let mut cy = 0.0;
    let mut radius = 0.0;
    let mut start_deg = 0.0;
    let mut end_deg = 360.0;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::Arc {
                center: (cx, cy),
                radius,
                start_deg,
                end_deg,
                layer,
                color,
                line_weight,
                linetype,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            370 => {
                if let Ok(lw) = p.value.parse::<i32>() {
                    if lw >= 0 {
                        line_weight = Some(lw as f64 / 100.0);
                    }
                }
            }
            6 => linetype = Some(p.value.to_ascii_uppercase()),
            10 => cx = parse_f64(&p.value),
            20 => cy = parse_f64(&p.value),
            40 => radius = parse_f64(&p.value),
            50 => start_deg = parse_f64(&p.value),
            51 => end_deg = parse_f64_default(&p.value, 360.0),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::Arc {
        center: (cx, cy),
        radius,
        start_deg,
        end_deg,
        layer,
        color,
        line_weight,
        linetype,
    });
    Ok(None)
}

fn parse_ellipse<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut line_weight = None;
    let mut linetype = None;
    let mut cx = 0.0;
    let mut cy = 0.0;
    let mut mx = 1.0;
    let mut my = 0.0;
    let mut ratio = 1.0;
    let mut start_param = 0.0;
    let mut end_param = std::f64::consts::TAU;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::Ellipse {
                center: (cx, cy),
                major_axis: (mx, my),
                axis_ratio: ratio,
                start_param,
                end_param,
                layer,
                color,
                line_weight,
                linetype,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            370 => {
                if let Ok(lw) = p.value.parse::<i32>() {
                    if lw >= 0 {
                        line_weight = Some(lw as f64 / 100.0);
                    }
                }
            }
            6 => linetype = Some(p.value.to_ascii_uppercase()),
            10 => cx = parse_f64(&p.value),
            20 => cy = parse_f64(&p.value),
            11 => mx = parse_f64_default(&p.value, 1.0),
            21 => my = parse_f64(&p.value),
            40 => ratio = parse_f64_default(&p.value, 1.0),
            41 => start_param = parse_f64(&p.value),
            42 => end_param = parse_f64_default(&p.value, std::f64::consts::TAU),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::Ellipse {
        center: (cx, cy),
        major_axis: (mx, my),
        axis_ratio: ratio,
        start_param,
        end_param,
        layer,
        color,
        line_weight,
        linetype,
    });
    Ok(None)
}

fn parse_lwpolyline<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut line_weight = None;
    let mut linetype = None;
    let mut is_closed = false;
    let mut vertices = Vec::new();
    let mut cur_x: Option<f64> = None;
    let mut cur_y: Option<f64> = None;
    let mut cur_bulge = 0.0;

    let flush_vertex =
        |verts: &mut Vec<LwVertex>, x: &mut Option<f64>, y: &mut Option<f64>, bulge: &mut f64| {
            if let (Some(vx), Some(vy)) = (*x, *y) {
                if verts.len() < MAX_VERTICES_PER_POLYLINE {
                    verts.push(LwVertex {
                        x: vx,
                        y: vy,
                        bulge: *bulge,
                    });
                }
            }
            *x = None;
            *y = None;
            *bulge = 0.0;
        };

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            flush_vertex(&mut vertices, &mut cur_x, &mut cur_y, &mut cur_bulge);
            entities.push(Entity::LwPolyline {
                vertices,
                is_closed,
                layer,
                color,
                line_weight,
                linetype,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            70 => {
                let flag: u16 = p.value.parse().unwrap_or(0);
                is_closed = (flag & 1) != 0;
            }
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            370 => {
                if let Ok(lw) = p.value.parse::<i32>() {
                    if lw >= 0 {
                        line_weight = Some(lw as f64 / 100.0);
                    }
                }
            }
            6 => linetype = Some(p.value.to_ascii_uppercase()),
            10 => {
                if cur_x.is_some() && cur_y.is_some() {
                    flush_vertex(&mut vertices, &mut cur_x, &mut cur_y, &mut cur_bulge);
                }
                cur_x = Some(parse_f64(&p.value));
            }
            20 => cur_y = Some(parse_f64(&p.value)),
            42 => cur_bulge = parse_f64(&p.value),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    flush_vertex(&mut vertices, &mut cur_x, &mut cur_y, &mut cur_bulge);
    entities.push(Entity::LwPolyline {
        vertices,
        is_closed,
        layer,
        color,
        line_weight,
        linetype,
    });
    Ok(None)
}

fn parse_old_polyline<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut line_weight = None;
    let mut linetype = None;
    let mut is_closed = false;
    let mut vertices = Vec::new();

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            if p.value.eq_ignore_ascii_case("VERTEX") {
                let (v, next_p) = parse_vertex_entity(reader)?;
                if let Some(vtx) = v {
                    if vertices.len() < MAX_VERTICES_PER_POLYLINE {
                        vertices.push(vtx);
                    }
                }
                pair = next_p;
                continue;
            } else if p.value.eq_ignore_ascii_case("SEQEND") {
                entities.push(Entity::LwPolyline {
                    vertices,
                    is_closed,
                    layer,
                    color,
                    line_weight,
                    linetype,
                });
                return reader.next_pair();
            } else {
                entities.push(Entity::LwPolyline {
                    vertices,
                    is_closed,
                    layer,
                    color,
                    line_weight,
                    linetype,
                });
                return Ok(Some(p));
            }
        }
        match p.code {
            8 => layer = p.value,
            70 => {
                let flag: u16 = p.value.parse().unwrap_or(0);
                is_closed = (flag & 1) != 0;
            }
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            370 => {
                if let Ok(lw) = p.value.parse::<i32>() {
                    if lw >= 0 {
                        line_weight = Some(lw as f64 / 100.0);
                    }
                }
            }
            6 => linetype = Some(p.value.to_ascii_uppercase()),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::LwPolyline {
        vertices,
        is_closed,
        layer,
        color,
        line_weight,
        linetype,
    });
    Ok(None)
}

fn parse_vertex_entity<R: BufRead>(
    reader: &mut DxfReader<R>,
) -> Result<(Option<LwVertex>, Option<DxfPair>)> {
    let mut x = 0.0;
    let mut y = 0.0;
    let mut bulge = 0.0;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            return Ok((Some(LwVertex { x, y, bulge }), Some(p)));
        }
        match p.code {
            10 => x = parse_f64(&p.value),
            20 => y = parse_f64(&p.value),
            42 => bulge = parse_f64(&p.value),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    Ok((Some(LwVertex { x, y, bulge }), None))
}

fn parse_solid<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut pts = [(0.0, 0.0); 4];

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::Solid {
                points: pts,
                layer,
                color,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            10 => pts[0].0 = parse_f64(&p.value),
            20 => pts[0].1 = parse_f64(&p.value),
            11 => pts[1].0 = parse_f64(&p.value),
            21 => pts[1].1 = parse_f64(&p.value),
            12 => pts[2].0 = parse_f64(&p.value),
            22 => pts[2].1 = parse_f64(&p.value),
            13 => pts[3].0 = parse_f64(&p.value),
            23 => pts[3].1 = parse_f64(&p.value),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::Solid {
        points: pts,
        layer,
        color,
    });
    Ok(None)
}

fn parse_text<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut text = String::new();
    let mut layer = "0".to_string();
    let mut color = None;
    let mut insert = (0.0, 0.0);
    let mut height = 2.5;
    let mut rotation_deg = 0.0;
    let mut h_align = 0;
    let mut v_align = 0;
    let mut align_pt: Option<(f64, f64)> = None;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            let final_insert = if h_align > 0 || v_align > 0 {
                align_pt.unwrap_or(insert)
            } else {
                insert
            };
            entities.push(Entity::Text {
                text,
                insert: final_insert,
                height,
                rotation_deg,
                layer,
                color,
                h_align,
                v_align,
            });
            return Ok(Some(p));
        }
        match p.code {
            1 => text = p.value,
            8 => layer = p.value,
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            10 => insert.0 = parse_f64(&p.value),
            20 => insert.1 = parse_f64(&p.value),
            11 => {
                let x = parse_f64(&p.value);
                align_pt = Some((x, align_pt.map_or(0.0, |pt| pt.1)));
            }
            21 => {
                let y = parse_f64(&p.value);
                align_pt = Some((align_pt.map_or(0.0, |pt| pt.0), y));
            }
            40 => height = parse_f64_default(&p.value, 2.5),
            50 => rotation_deg = parse_f64(&p.value),
            72 => h_align = p.value.parse().unwrap_or(0),
            73 => v_align = p.value.parse().unwrap_or(0),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    let final_insert = if h_align > 0 || v_align > 0 {
        align_pt.unwrap_or(insert)
    } else {
        insert
    };
    entities.push(Entity::Text {
        text,
        insert: final_insert,
        height,
        rotation_deg,
        layer,
        color,
        h_align,
        v_align,
    });
    Ok(None)
}

fn parse_mtext<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut text = String::new();
    let mut layer = "0".to_string();
    let mut color = None;
    let mut insert = (0.0, 0.0);
    let mut height = 2.5;
    let mut rotation_deg = 0.0;
    let mut attachment = 1;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::MText {
                text,
                insert,
                height,
                rotation_deg,
                layer,
                color,
                attachment,
            });
            return Ok(Some(p));
        }
        match p.code {
            1 | 3 => {
                text.push_str(&p.value);
            }
            8 => layer = p.value,
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            10 => insert.0 = parse_f64(&p.value),
            20 => insert.1 = parse_f64(&p.value),
            40 => height = parse_f64_default(&p.value, 2.5),
            50 => rotation_deg = parse_f64(&p.value),
            71 => attachment = p.value.parse().unwrap_or(1),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::MText {
        text,
        insert,
        height,
        rotation_deg,
        layer,
        color,
        attachment,
    });
    Ok(None)
}

fn parse_insert<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut block_name = String::new();
    let mut layer = "0".to_string();
    let mut color = None;
    let mut line_weight = None;
    let mut insert = (0.0, 0.0);
    let mut scale = (1.0, 1.0);
    let mut rotation_deg = 0.0;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            if !block_name.is_empty() {
                entities.push(Entity::Insert {
                    block_name,
                    insert,
                    scale,
                    rotation_deg,
                    layer,
                    color,
                    line_weight,
                });
            }
            return Ok(Some(p));
        }
        match p.code {
            2 => block_name = p.value,
            8 => layer = p.value,
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            370 => {
                if let Ok(lw) = p.value.parse::<i32>() {
                    if lw >= 0 {
                        line_weight = Some(lw as f64 / 100.0);
                    }
                }
            }
            10 => insert.0 = parse_f64(&p.value),
            20 => insert.1 = parse_f64(&p.value),
            41 => scale.0 = parse_f64_default(&p.value, 1.0),
            42 => scale.1 = parse_f64_default(&p.value, 1.0),
            50 => rotation_deg = parse_f64(&p.value),
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    if !block_name.is_empty() {
        entities.push(Entity::Insert {
            block_name,
            insert,
            scale,
            rotation_deg,
            layer,
            color,
            line_weight,
        });
    }
    Ok(None)
}

fn parse_spline<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut line_weight = None;
    let mut degree = 3;
    let mut is_closed = false;
    let mut control_points = Vec::new();
    let mut knots = Vec::new();
    let mut cur_x: Option<f64> = None;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::Spline {
                degree,
                control_points,
                knots,
                is_closed,
                layer,
                color,
                line_weight,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            70 => {
                let flag: u16 = p.value.parse().unwrap_or(0);
                is_closed = (flag & 1) != 0;
            }
            71 => degree = p.value.parse().unwrap_or(3),
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            370 => {
                if let Ok(lw) = p.value.parse::<i32>() {
                    if lw >= 0 {
                        line_weight = Some(lw as f64 / 100.0);
                    }
                }
            }
            40 => {
                if let Some(k) = parse_cad_float(&p.value) {
                    knots.push(k);
                }
            }
            10 => cur_x = Some(parse_f64(&p.value)),
            20 => {
                if let Some(x) = cur_x.take() {
                    let y = parse_f64(&p.value);
                    control_points.push((x, y));
                }
            }
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::Spline {
        degree,
        control_points,
        knots,
        is_closed,
        layer,
        color,
        line_weight,
    });
    Ok(None)
}

fn parse_hatch<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut is_solid = false;
    let mut pattern_name = String::new();
    let boundaries = Vec::new();

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::Hatch {
                boundaries,
                is_solid,
                pattern_name,
                layer,
                color,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            2 => pattern_name = p.value,
            70 => {
                let flag: u16 = p.value.parse().unwrap_or(0);
                is_solid = (flag & 1) != 0;
            }
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::Hatch {
        boundaries,
        is_solid,
        pattern_name,
        layer,
        color,
    });
    Ok(None)
}

fn parse_dimension<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut block_name = None;
    let mut text = String::new();
    let mut insert = (0.0, 0.0);

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            entities.push(Entity::Dimension {
                block_name,
                text,
                insert,
                layer,
                color,
            });
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            2 => block_name = Some(p.value),
            1 => text = p.value,
            10 => insert.0 = parse_f64(&p.value),
            20 => insert.1 = parse_f64(&p.value),
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    entities.push(Entity::Dimension {
        block_name,
        text,
        insert,
        layer,
        color,
    });
    Ok(None)
}

fn parse_leader<R: BufRead>(
    reader: &mut DxfReader<R>,
    entities: &mut Vec<Entity>,
) -> Result<Option<DxfPair>> {
    let mut layer = "0".to_string();
    let mut color = None;
    let mut vertices = Vec::new();
    let mut cur_x = 0.0;

    let mut pair = reader.next_pair()?;
    while let Some(p) = pair {
        if p.code == 0 {
            if !vertices.is_empty() {
                entities.push(Entity::Leader {
                    vertices,
                    layer,
                    color,
                });
            }
            return Ok(Some(p));
        }
        match p.code {
            8 => layer = p.value,
            10 => cur_x = parse_f64(&p.value),
            20 => {
                let y = parse_f64(&p.value);
                vertices.push((cur_x, y));
            }
            62 => {
                if let Ok(aci) = p.value.parse::<i16>() {
                    color = Some(aci_to_hex(aci.abs()));
                }
            }
            420 => {
                if let Ok(tc) = p.value.parse::<u32>() {
                    color = Some(truecolor_to_hex(tc));
                }
            }
            _ => {}
        }
        pair = reader.next_pair()?;
    }

    if !vertices.is_empty() {
        entities.push(Entity::Leader {
            vertices,
            layer,
            color,
        });
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_simple_dxf_entities() {
        let dxf_content = r#"  0
SECTION
  2
ENTITIES
  0
LINE
  8
Walls
 10
0.0
 20
10.0
 11
100.0
 21
50.0
  0
CIRCLE
  8
Columns
 10
200.0
 20
150.0
 40
25.0
  0
ENDSEC
  0
EOF
"#;
        let doc = parse_dxf(Cursor::new(dxf_content)).expect("parse DXF");
        assert_eq!(doc.entities.len(), 2);
        match &doc.entities[0] {
            Entity::Line {
                start, end, layer, ..
            } => {
                assert_eq!(start, &(0.0, 10.0));
                assert_eq!(end, &(100.0, 50.0));
                assert_eq!(layer, "Walls");
            }
            _ => panic!("expected Line"),
        }
        match &doc.entities[1] {
            Entity::Circle {
                center,
                radius,
                layer,
                ..
            } => {
                assert_eq!(center, &(200.0, 150.0));
                assert_eq!(*radius, 25.0);
                assert_eq!(layer, "Columns");
            }
            _ => panic!("expected Circle"),
        }
    }

    #[test]
    fn parses_text_with_partial_alignment_point() {
        let dxf_content = r#"0
SECTION
2
ENTITIES
0
TEXT
1
NoAlign
72
1
73
1
10
10.0
20
20.0
11
99.0
0
ENDSEC
0
EOF
"#;
        let doc = parse_dxf(Cursor::new(dxf_content)).expect("parse DXF");
        assert_eq!(doc.entities.len(), 1);
        match &doc.entities[0] {
            Entity::Text { insert, .. } => {
                assert_eq!(*insert, (99.0, 0.0));
            }
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn parses_dxf_entities_with_omitted_leading_zeros() {
        let dxf_content = r#"0
SECTION
2
ENTITIES
0
LINE
8
0
10
.5
20
-.75
11
+.25
21
-1.5
0
CIRCLE
8
0
10
.125
20
.875
40
.5
0
ENDSEC
0
EOF
"#;
        let doc = parse_dxf(Cursor::new(dxf_content)).expect("parse DXF");
        assert_eq!(doc.entities.len(), 2);
        match &doc.entities[0] {
            Entity::Line { start, end, .. } => {
                assert_eq!(start, &(0.5, -0.75));
                assert_eq!(end, &(0.25, -1.5));
            }
            _ => panic!("expected Line"),
        }
        match &doc.entities[1] {
            Entity::Circle { center, radius, .. } => {
                assert_eq!(center, &(0.125, 0.875));
                assert_eq!(*radius, 0.5);
            }
            _ => panic!("expected Circle"),
        }
    }
}
