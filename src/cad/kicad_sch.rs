//! Bounded KiCad legacy Eeschema schematic (`.sch`) preview.
//!
//! The legacy format stores schematic coordinates in mils and references
//! symbols by library name. This reader draws a deterministic approximation of
//! components, wires, labels, and junctions without opening cache libraries,
//! images, models, or project files.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};

const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LINES: usize = 2_000_000;
const MAX_LINE_BYTES: usize = 1 << 20;
const MAX_COMPONENTS: usize = 250_000;
const MAX_WIRES: usize = 1_000_000;
const MAX_LABELS: usize = 250_000;
const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_COORDINATE_MILS: f64 = 100_000_000.0;
const PAGE_WIDTH: f64 = 842.0;
const PAGE_HEIGHT: f64 = 595.0;
const PAGE_MARGIN: f64 = 28.0;

#[derive(Clone, Debug)]
struct Component {
    library: String,
    reference: String,
    value: String,
    x: f64,
    y: f64,
}
#[derive(Clone, Copy, Debug)]
struct Wire {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}
#[derive(Clone, Debug)]
struct Label {
    text: String,
    x: f64,
    y: f64,
    size: f64,
}

#[derive(Default)]
struct Schematic {
    descriptor_width: Option<f64>,
    descriptor_height: Option<f64>,
    components: Vec<Component>,
    wires: Vec<Wire>,
    labels: Vec<Label>,
    junctions: Vec<(f64, f64)>,
    warnings: Vec<String>,
    text_bytes: usize,
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| {
            line.to_ascii_uppercase()
                .starts_with("EESCHEMA SCHEMATIC FILE VERSION")
        })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_INPUT_BYTES),
        "KiCad legacy schematic input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!(
            "KiCad legacy schematic must be UTF-8/ASCII: {error}"
        ))
    })?;
    let schematic = parse_schematic(&text)?;
    let mut page = render_schematic(&schematic)?;
    page.source_format = "kicad_sch_legacy".into();
    page.title = "KiCad legacy schematic".into();
    page.description = "Legacy Eeschema components and wires are rendered as a bounded approximation; symbol libraries are not opened".into();
    for warning in &schematic.warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(schematic.warnings)
}

fn parse_schematic(text: &str) -> Result<Schematic> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_LINES {
        return Err(Error::LimitExceeded(format!(
            "KiCad legacy schematic exceeds {MAX_LINES} lines"
        )));
    }
    let mut schematic = Schematic::default();
    let mut index = 0usize;
    let mut saw_header = false;
    while index < lines.len() {
        let original = lines[index].trim_end_matches('\r');
        if original.len() > MAX_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "KiCad legacy schematic line {} exceeds {MAX_LINE_BYTES} bytes",
                index + 1
            )));
        }
        let line = original.trim();
        if line.is_empty() {
            index += 1;
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EESCHEMA SCHEMATIC FILE VERSION") {
            saw_header = true;
            index += 1;
            continue;
        }
        if line.starts_with("$Descr ") {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.len() >= 4 {
                schematic.descriptor_width = parse_coordinate(fields[2], index + 1).ok();
                schematic.descriptor_height = parse_coordinate(fields[3], index + 1).ok();
            }
            index += 1;
            continue;
        }
        if line == "$Comp" {
            let (component, next) = parse_component(&lines, index)?;
            if schematic.components.len() >= MAX_COMPONENTS {
                return Err(Error::LimitExceeded(format!(
                    "KiCad legacy schematic exceeds {MAX_COMPONENTS} components"
                )));
            }
            schematic.text_bytes = schematic
                .text_bytes
                .checked_add(
                    component.library.len() + component.reference.len() + component.value.len(),
                )
                .ok_or_else(|| {
                    Error::LimitExceeded("KiCad schematic text byte count overflowed".into())
                })?;
            if schematic.text_bytes > MAX_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "KiCad legacy schematic text exceeds {MAX_TEXT_BYTES} bytes"
                )));
            }
            schematic.components.push(component);
            index = next;
            continue;
        }
        if line == "Wire Wire Line" || line == "Wire Bus Line" {
            let next = lines
                .get(index + 1)
                .ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "KiCad wire on line {} is missing coordinates",
                        index + 1
                    ))
                })?
                .trim();
            let fields = next.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.len() != 4 {
                return Err(Error::InvalidInput(format!(
                    "KiCad wire on line {} must contain four coordinates",
                    index + 2
                )));
            }
            let wire = Wire {
                x1: parse_coordinate(fields[0], index + 2)?,
                y1: parse_coordinate(fields[1], index + 2)?,
                x2: parse_coordinate(fields[2], index + 2)?,
                y2: parse_coordinate(fields[3], index + 2)?,
            };
            if schematic.wires.len() >= MAX_WIRES {
                return Err(Error::LimitExceeded(format!(
                    "KiCad legacy schematic exceeds {MAX_WIRES} wires"
                )));
            }
            schematic.wires.push(wire);
            index += 2;
            continue;
        }
        if upper.starts_with("TEXT LABEL ")
            || upper.starts_with("TEXT NOTES ")
            || upper.starts_with("TEXT GLABEL ")
            || upper.starts_with("TEXT HLABEL ")
        {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.len() < 4 {
                return Err(Error::InvalidInput(format!(
                    "KiCad text line {} is incomplete",
                    index + 1
                )));
            }
            let label = Label {
                text: lines
                    .get(index + 1)
                    .copied()
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
                x: parse_coordinate(fields[2], index + 1)?,
                y: parse_coordinate(fields[3], index + 1)?,
                size: fields
                    .get(5)
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .unwrap_or(50.0),
            };
            if label.text.is_empty() {
                schematic
                    .warnings
                    .push(format!("KiCad text label on line {} is empty", index + 1));
            }
            schematic.text_bytes = schematic
                .text_bytes
                .checked_add(label.text.len())
                .ok_or_else(|| {
                    Error::LimitExceeded("KiCad schematic text byte count overflowed".into())
                })?;
            if schematic.text_bytes > MAX_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "KiCad legacy schematic text exceeds {MAX_TEXT_BYTES} bytes"
                )));
            }
            if schematic.labels.len() >= MAX_LABELS {
                return Err(Error::LimitExceeded(format!(
                    "KiCad legacy schematic exceeds {MAX_LABELS} labels"
                )));
            }
            schematic.labels.push(label);
            index += 2;
            continue;
        }
        if upper.starts_with("CONNECTION ~") {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.len() == 4 {
                schematic.junctions.push((
                    parse_coordinate(fields[2], index + 1)?,
                    parse_coordinate(fields[3], index + 1)?,
                ));
            }
            index += 1;
            continue;
        }
        if line == "$EndDescr"
            || line == "$EndSCHEMATC"
            || line.starts_with("EELAYER")
            || line.starts_with("LIBS:")
            || line.starts_with("Sheet ")
            || line.starts_with("Title ")
            || line.starts_with("Date ")
            || line.starts_with("Rev ")
            || line.starts_with("Comp ")
            || line.starts_with("Comment")
            || line.starts_with("F ")
            || line.starts_with("NoConn ")
            || line.starts_with("Entry ")
            || line.starts_with("$Sheet")
            || line == "$EndSheet"
        {
            index += 1;
            continue;
        }
        if line.starts_with("$EndComp") {
            index += 1;
            continue;
        }
        if line.starts_with("Text ") {
            schematic.warnings.push(format!(
                "unsupported KiCad text primitive on line {} was omitted",
                index + 1
            ));
            index += 1;
            continue;
        }
        if line.starts_with("$") || line.starts_with("F ") {
            index += 1;
            continue;
        }
        schematic.warnings.push(format!(
            "unsupported KiCad legacy schematic line {} was omitted",
            index + 1
        ));
        index += 1;
    }
    if !saw_header {
        return Err(Error::InvalidInput(
            "KiCad legacy schematic header is missing".into(),
        ));
    }
    if schematic.components.is_empty() && schematic.wires.is_empty() && schematic.labels.is_empty()
    {
        return Err(Error::InvalidInput(
            "KiCad legacy schematic contains no drawable items".into(),
        ));
    }
    Ok(schematic)
}

fn parse_component(lines: &[&str], start: usize) -> Result<(Component, usize)> {
    let mut library = String::new();
    let mut reference = String::new();
    let mut value = String::new();
    let mut x = None;
    let mut y = None;
    let mut index = start + 1;
    while let Some(raw) = lines.get(index) {
        let line = raw.trim();
        if line == "$EndComp" {
            break;
        }
        if let Some(rest) = line.strip_prefix("L ") {
            let mut fields = rest.split_ascii_whitespace();
            library = fields.next().unwrap_or_default().to_owned();
            reference = fields.next().unwrap_or_default().to_owned();
        } else if let Some(rest) = line.strip_prefix("P ") {
            let fields = rest.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.len() >= 2 {
                x = Some(parse_coordinate(fields[0], index + 1)?);
                y = Some(parse_coordinate(fields[1], index + 1)?);
            }
        } else if line.starts_with("F 0 ") {
            reference = quoted_text(line).unwrap_or_else(|| reference.clone());
        } else if line.starts_with("F 1 ") {
            value = quoted_text(line).unwrap_or_default();
        }
        index += 1;
    }
    if lines.get(index).is_none() {
        return Err(Error::InvalidInput(format!(
            "KiCad component at line {} is unterminated",
            start + 1
        )));
    }
    let x = x.ok_or_else(|| {
        Error::InvalidInput(format!(
            "KiCad component at line {} has no position",
            start + 1
        ))
    })?;
    let y = y.ok_or_else(|| {
        Error::InvalidInput(format!(
            "KiCad component at line {} has no position",
            start + 1
        ))
    })?;
    if library.is_empty() {
        return Err(Error::InvalidInput(format!(
            "KiCad component at line {} has no library identifier",
            start + 1
        )));
    }
    Ok((
        Component {
            library,
            reference,
            value,
            x,
            y,
        },
        index + 1,
    ))
}

fn quoted_text(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].replace("\\\"", "\""))
}

fn parse_coordinate(value: &str, line: usize) -> Result<f64> {
    let coordinate = value
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("KiCad coordinate on line {line} is invalid")))?;
    if !coordinate.is_finite() || coordinate.abs() > MAX_COORDINATE_MILS {
        return Err(Error::LimitExceeded(format!(
            "KiCad coordinate on line {line} exceeds configured range"
        )));
    }
    Ok(coordinate)
}

fn render_schematic(schematic: &Schematic) -> Result<Page> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut include = |x: f64, y: f64| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    for wire in &schematic.wires {
        include(wire.x1, wire.y1);
        include(wire.x2, wire.y2);
    }
    for component in &schematic.components {
        include(component.x - 100.0, component.y - 70.0);
        include(component.x + 100.0, component.y + 70.0);
    }
    for label in &schematic.labels {
        include(label.x, label.y);
    }
    if !min_x.is_finite() {
        return Err(Error::InvalidInput(
            "KiCad schematic has no finite geometry".into(),
        ));
    }
    let source_width = schematic
        .descriptor_width
        .unwrap_or(max_x - min_x)
        .max(max_x - min_x)
        .max(1.0);
    let source_height = schematic
        .descriptor_height
        .unwrap_or(max_y - min_y)
        .max(max_y - min_y)
        .max(1.0);
    let scale = ((PAGE_WIDTH - 2.0 * PAGE_MARGIN) / source_width)
        .min((PAGE_HEIGHT - 2.0 * PAGE_MARGIN) / source_height)
        .min(2.0);
    let transform = [
        scale,
        0.0,
        0.0,
        scale,
        PAGE_MARGIN - min_x * scale,
        PAGE_MARGIN - min_y * scale,
    ];
    let mut page = Page::new(1, PAGE_WIDTH, PAGE_HEIGHT, "kicad_sch_legacy");
    let wire_stroke = Stroke {
        paint: Paint::solid("#1d4ed8"),
        width: 1.4,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    for (index, wire) in schematic.wires.iter().enumerate() {
        page.nodes.push(Node::Path {
            id: format!("kicad-sch-wire-{index}"),
            d: format!("M {} {} L {} {}", wire.x1, wire.y1, wire.x2, wire.y2),
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: wire_stroke.clone(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "kicad_schematic_wire".into(),
                ..SourceMeta::default()
            },
        });
    }
    let component_stroke = Stroke {
        paint: Paint::solid("#111827"),
        width: 1.2,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    for (index, component) in schematic.components.iter().enumerate() {
        let x = component.x - 90.0;
        let y = component.y - 55.0;
        page.nodes.push(Node::Path {
            id: format!("kicad-sch-component-{index}"),
            d: format!("M {x} {y} h 180 v 110 h -180 Z"),
            fill_rule: "nonzero".into(),
            fill: Paint::solid("#f8fafc"),
            stroke: component_stroke.clone(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "kicad_schematic_component".into(),
                source_id: component.library.clone(),
                ..SourceMeta::default()
            },
        });
        push_text(
            &mut page,
            &format!("kicad-sch-ref-{index}"),
            &component.reference,
            component.x,
            component.y - 12.0,
            50.0 * scale,
            transform,
            "kicad_schematic_reference",
        );
        push_text(
            &mut page,
            &format!("kicad-sch-value-{index}"),
            &component.value,
            component.x,
            component.y + 24.0,
            42.0 * scale,
            transform,
            "kicad_schematic_value",
        );
    }
    for (index, label) in schematic.labels.iter().enumerate() {
        push_text(
            &mut page,
            &format!("kicad-sch-label-{index}"),
            &label.text,
            label.x,
            label.y,
            (label.size * 0.72) * scale,
            transform,
            "kicad_schematic_label",
        );
    }
    let junction_stroke = Stroke {
        paint: Paint::solid("#b91c1c"),
        width: 1.0,
        ..Stroke::default()
    };
    for (index, (x, y)) in schematic.junctions.iter().enumerate() {
        page.nodes.push(Node::Path {
            id: format!("kicad-sch-junction-{index}"),
            d: format!(
                "M {} {} m -18 0 a 18 18 0 1 0 36 0 a 18 18 0 1 0 -36 0",
                x, y
            ),
            fill_rule: "nonzero".into(),
            fill: Paint::solid("#dc2626"),
            stroke: junction_stroke.clone(),
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "kicad_schematic_junction".into(),
                ..SourceMeta::default()
            },
        });
    }
    Ok(page)
}

#[allow(clippy::too_many_arguments)]
fn push_text(
    page: &mut Page,
    id: &str,
    text: &str,
    x: f64,
    y: f64,
    size: f64,
    transform: [f64; 6],
    role: &str,
) {
    if text.is_empty() {
        return;
    }
    page.nodes.push(Node::Text {
        id: id.into(),
        x,
        y,
        runs: vec![TextRun {
            text: text.into(),
            font_family: "sans-serif".into(),
            font_size: size.max(2.0),
            fill: Paint::solid("#111827"),
            ..TextRun::default()
        }],
        anchor: TextAnchor::Middle,
        transform,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            kind: role.into(),
            ..SourceMeta::default()
        },
    });
}
