//! SVG Transform: Post-processing and tailoring of SVG documents (minify, monochrome, responsive).

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};
use std::io::{Cursor, Write};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Default)]
pub struct TransformOptions {
    pub minify: bool,
    pub monochrome: Option<String>,
    pub responsive: bool,
    pub precision: Option<usize>,
}

pub fn transform_svg(svg_bytes: &[u8], options: &TransformOptions) -> Result<Vec<u8>> {
    let svg_str = std::str::from_utf8(svg_bytes)
        .map_err(|e| Error::InvalidInput(format!("SVG is not valid UTF-8: {e}")))?;

    let mut reader = Reader::from_str(svg_str);
    if options.minify {
        reader.config_mut().trim_text(true);
    }

    let mut out = Cursor::new(Vec::with_capacity(svg_bytes.len()));
    let mut writer = Writer::new(&mut out);

    let mut width_val: Option<String> = None;
    let mut height_val: Option<String> = None;
    let mut viewbox_val: Option<String> = None;

    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(event) => event,
            Err(err) => {
                return Err(Error::InvalidInput(format!(
                    "failed to parse SVG input: {err}"
                )));
            }
        };

        match event {
            Event::Decl(_) if options.minify => {}
            Event::Comment(_) if options.minify => {}
            Event::Start(ref e) if e.name().as_ref() == b"svg" => {
                let mut elem = BytesStart::new("svg");
                for attr in e.attributes().flatten() {
                    let key = attr.key.as_ref();
                    let val = std::str::from_utf8(&attr.value).unwrap_or("");

                    if key == b"width" {
                        width_val = Some(val.to_string());
                    } else if key == b"height" {
                        height_val = Some(val.to_string());
                    } else if key == b"viewBox" {
                        viewbox_val = Some(val.to_string());
                    }

                    if options.responsive && (key == b"width" || key == b"height") {
                        continue;
                    }

                    if let (Some(prec), true) = (options.precision, is_numeric_attr(key)) {
                        let rounded = round_numbers_in_str(val, prec);
                        elem.push_attribute((key, rounded.as_bytes()));
                        continue;
                    }

                    elem.push_attribute(attr);
                }

                if options.responsive && viewbox_val.is_none() {
                    let dims = width_val.as_ref().zip(height_val.as_ref());
                    if let Some((w, h)) = dims {
                        let w_num = w.trim_end_matches("px").trim_end_matches("pt");
                        let h_num = h.trim_end_matches("px").trim_end_matches("pt");
                        let synthesized = format!("0 0 {w_num} {h_num}");
                        let final_viewbox = if let Some(prec) = options.precision {
                            round_numbers_in_str(&synthesized, prec)
                        } else {
                            synthesized
                        };
                        elem.push_attribute(("viewBox", final_viewbox.as_str()));
                    }
                }

                writer.write_event(Event::Start(elem))?;
            }
            Event::Start(ref e) => {
                write_transformed_element(&mut writer, e, false, options)?;
            }
            Event::Empty(ref e) if e.name().as_ref() == b"svg" => {
                let mut elem = BytesStart::new("svg");
                for attr in e.attributes().flatten() {
                    let key = attr.key.as_ref();
                    let val = std::str::from_utf8(&attr.value).unwrap_or("");
                    if options.responsive && (key == b"width" || key == b"height") {
                        continue;
                    }
                    if let (Some(prec), true) = (options.precision, is_numeric_attr(key)) {
                        let rounded = round_numbers_in_str(val, prec);
                        elem.push_attribute((key, rounded.as_bytes()));
                        continue;
                    }
                    elem.push_attribute(attr);
                }
                writer.write_event(Event::Empty(elem))?;
            }
            Event::Empty(ref e) => {
                write_transformed_element(&mut writer, e, true, options)?;
            }
            Event::Text(ref e) if options.minify => {
                let slice: &[u8] = e.as_ref();
                if !slice.iter().all(|&b| b.is_ascii_whitespace()) {
                    writer.write_event(Event::Text(e.clone()))?;
                }
            }
            other => {
                writer.write_event(other)?;
            }
        }
    }

    Ok(out.into_inner())
}

fn is_numeric_attr(key: &[u8]) -> bool {
    matches!(
        key,
        b"d" | b"points"
            | b"x"
            | b"y"
            | b"width"
            | b"height"
            | b"cx"
            | b"cy"
            | b"r"
            | b"rx"
            | b"ry"
            | b"x1"
            | b"y1"
            | b"x2"
            | b"y2"
            | b"stroke-width"
            | b"viewBox"
            | b"transform"
    )
}

fn round_numbers_in_str(s: &str, precision: usize) -> String {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut output: Option<String> = None;
    let mut token_start = 0;

    while i < n {
        let ch = bytes[i];
        let is_num_start = ch.is_ascii_digit()
            || ((ch == b'-' || ch == b'+' || ch == b'.')
                && i + 1 < n
                && (bytes[i + 1].is_ascii_digit()
                    || (ch != b'.'
                        && bytes[i + 1] == b'.'
                        && i + 2 < n
                        && bytes[i + 2].is_ascii_digit())));

        if is_num_start {
            let start = i;
            if bytes[i] == b'-' || bytes[i] == b'+' {
                i += 1;
            }
            let mut has_dot = false;
            let mut has_exp = false;
            while i < n {
                let c = bytes[i];
                if c.is_ascii_digit() {
                    i += 1;
                } else if c == b'.' && !has_dot && !has_exp {
                    has_dot = true;
                    i += 1;
                } else if (c == b'e' || c == b'E') && !has_exp {
                    has_exp = true;
                    i += 1;
                    if i < n && (bytes[i] == b'+' || bytes[i] == b'-') {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            let raw = &s[start..i];
            let mut rounded = None;
            if let Ok(val) = raw.parse::<f64>() {
                if has_dot || has_exp {
                    let formatted = format!("{val:.precision$}");
                    let trimmed = if formatted.contains('.') {
                        formatted.trim_end_matches('0').trim_end_matches('.')
                    } else {
                        formatted.as_str()
                    };
                    let normalized = if trimmed.is_empty() || trimmed == "-0" {
                        "0"
                    } else {
                        trimmed
                    };
                    if normalized != raw {
                        rounded = Some(normalized.to_string());
                    }
                }
            }

            if let Some(token_out) = rounded {
                if output.is_none() {
                    let mut out = String::with_capacity(s.len());
                    out.push_str(&s[token_start..start]);
                    output = Some(out);
                }
                output.as_mut().unwrap().push_str(&token_out);
                token_start = i;
            }
        } else {
            i += 1;
        }
    }

    match output {
        Some(mut out) => {
            out.push_str(&s[token_start..]);
            out
        }
        None => s.to_owned(),
    }
}

fn write_transformed_element<W: Write>(
    writer: &mut Writer<W>,
    e: &BytesStart,
    is_empty: bool,
    options: &TransformOptions,
) -> Result<()> {
    let name_bytes = e.name().into_inner();
    let name = std::str::from_utf8(name_bytes).unwrap_or("g");
    let mut elem = BytesStart::new(name);

    for attr in e.attributes().flatten() {
        let key = attr.key.as_ref();
        let val = std::str::from_utf8(&attr.value).unwrap_or("");

        if let Some(ref mono_color) = options.monochrome {
            if key == b"fill" {
                if val == "none" {
                    elem.push_attribute(("fill", "none"));
                } else {
                    elem.push_attribute(("fill", mono_color.as_str()));
                }
                continue;
            } else if key == b"stroke" {
                if val == "none" {
                    elem.push_attribute(("stroke", "none"));
                } else {
                    elem.push_attribute(("stroke", mono_color.as_str()));
                }
                continue;
            }
        }

        if let (Some(prec), true) = (options.precision, is_numeric_attr(key)) {
            let rounded = round_numbers_in_str(val, prec);
            elem.push_attribute((key, rounded.as_bytes()));
            continue;
        }

        elem.push_attribute(attr);
    }

    if is_empty {
        writer.write_event(Event::Empty(elem))?;
    } else {
        writer.write_event(Event::Start(elem))?;
    }
    Ok(())
}
