//! Standalone EMF and WMF (Windows Metafile) to SVG converter using `emf-core`.

#![allow(clippy::collapsible_if)]

use std::path::Path;

use base64::Engine;
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, SourceMeta};
use crate::ooxml::{attribute, local_name};

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "metafile")?;

    let svg_bytes = emf_core::converter::convert_to_svg(bytes.as_slice())
        .map_err(|e| Error::InvalidInput(format!("failed to convert metafile to SVG: {e}")))?;

    let (width, height) = std::str::from_utf8(&svg_bytes)
        .ok()
        .and_then(parse_svg_dimensions)
        .unwrap_or((612.0, 792.0));

    let mut page = Page::new(1, width, height, "metafile");
    let base64_data = base64::engine::general_purpose::STANDARD.encode(&svg_bytes);

    page.nodes.push(Node::Image {
        id: "metafile-0".into(),
        href: format!("data:image/svg+xml;base64,{base64_data}"),
        x: 0.0,
        y: 0.0,
        width,
        height,
        transform: IDENTITY,
        opacity: 1.0,
        clip_id: None,
        meta: SourceMeta {
            kind: "converted-metafile".into(),
            ..Default::default()
        },
    });

    sink.consume(page)?;
    Ok(Vec::new())
}

fn parse_svg_dimensions(svg: &str) -> Option<(f64, f64)> {
    let mut reader = Reader::from_str(svg);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    while let Ok(event) = reader.read_event_into(&mut buf) {
        match event {
            Event::Start(ref e) | Event::Empty(ref e)
                if local_name(e.name().as_ref()) == b"svg" =>
            {
                let width = attribute(e, b"width").and_then(|v| parse_length(&v));
                let height = attribute(e, b"height").and_then(|v| parse_length(&v));
                if let (Some(w), Some(h)) = (width, height)
                    && w > 0.0
                    && h > 0.0
                {
                    return Some((w, h));
                }
                if let Some(vb) = attribute(e, b"viewBox") {
                    let parts: Vec<f64> = vb
                        .split(|c: char| c.is_whitespace() || c == ',')
                        .filter_map(|s| s.parse::<f64>().ok())
                        .collect();
                    if parts.len() == 4 && parts[2] > 0.0 && parts[3] > 0.0 {
                        return Some((parts[2], parts[3]));
                    }
                }
                return None;
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

fn parse_length(s: &str) -> Option<f64> {
    let value = s.trim();
    let lower = value.to_ascii_lowercase();
    let (number, unit) = ["pt", "px", "in", "cm", "mm", "pc"]
        .into_iter()
        .find_map(|unit| lower.strip_suffix(unit).map(|number| (number.trim(), unit)))
        .unwrap_or((value, ""));
    let number = number.parse::<f64>().ok()?;
    let points = match unit {
        "pt" | "" => number,
        "px" => number * 0.75,
        "in" => number * 72.0,
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        "pc" => number * 12.0,
        _ => number,
    };
    Some(points)
}
