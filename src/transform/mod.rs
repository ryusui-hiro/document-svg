//! SVG Transform: Post-processing and tailoring of SVG documents (minify, monochrome, responsive).

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesStart, Event};
use std::borrow::Cow;
use std::io::{Cursor, Write};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Default)]
pub struct TransformOptions {
    pub minify: bool,
    pub monochrome: Option<String>,
    pub responsive: bool,
    pub precision: Option<usize>,
    pub remove_metadata: bool,
    pub clean_paths: bool,
    pub strip_empty_groups: bool,
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
    let mut in_style_tag = false;
    let mut skip_depth = 0usize;

    #[derive(Clone)]
    struct PendingGroup {
        elem: BytesStart<'static>,
        written: bool,
    }
    let mut group_stack: Vec<PendingGroup> = Vec::new();

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

        if skip_depth > 0 {
            match event {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                _ => {}
            }
            continue;
        }

        match event {
            Event::Decl(_) if options.minify => {}
            Event::DocType(_) if options.minify => {}
            Event::Comment(_) if options.minify => {}
            Event::Start(ref e)
                if options.remove_metadata
                    && (e.name().as_ref() == b"metadata" || e.name().as_ref() == b"desc") =>
            {
                skip_depth = 1;
            }
            Event::Empty(ref e)
                if options.remove_metadata
                    && (e.name().as_ref() == b"metadata" || e.name().as_ref() == b"desc") => {}
            Event::Start(ref e) if e.name().as_ref() == b"svg" => {
                let mut elem = BytesStart::new("svg");
                for attr in e.attributes().flatten() {
                    let key = attr.key.as_ref();
                    let val = std::str::from_utf8(&attr.value).unwrap_or("");

                    if options.remove_metadata && key.starts_with(b"data-") {
                        continue;
                    }

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
                        let w_px = parse_svg_dimension_to_px(w);
                        let h_px = parse_svg_dimension_to_px(h);
                        if let (Some(wp), Some(hp)) = (w_px, h_px) {
                            let synthesized = format!("0 0 {wp} {hp}");
                            let final_viewbox = if let Some(prec) = options.precision {
                                round_numbers_in_str(&synthesized, prec)
                            } else {
                                Cow::Borrowed(synthesized.as_str())
                            };
                            elem.push_attribute(("viewBox", final_viewbox.as_ref()));
                        }
                    }
                }

                writer.write_event(Event::Start(elem))?;
            }
            Event::Start(ref e) => {
                let tag_name = e.name();
                if tag_name.as_ref() == b"style" {
                    in_style_tag = true;
                }
                if options.strip_empty_groups && tag_name.as_ref() == b"g" {
                    // Create transformed element before pushing to stack
                    let mut transformed_elem = BytesStart::new("g");
                    for attr in e.attributes().flatten() {
                        let key = attr.key.as_ref();
                        let val = std::str::from_utf8(&attr.value).unwrap_or("");
                        if options.remove_metadata && key.starts_with(b"data-") {
                            continue;
                        }
                        if let Some(ref mono_color) = options.monochrome {
                            if key == b"fill" {
                                if val == "none" {
                                    transformed_elem.push_attribute(("fill", "none"));
                                } else {
                                    transformed_elem.push_attribute(("fill", mono_color.as_str()));
                                }
                                continue;
                            } else if key == b"stroke" {
                                if val == "none" {
                                    transformed_elem.push_attribute(("stroke", "none"));
                                } else {
                                    transformed_elem
                                        .push_attribute(("stroke", mono_color.as_str()));
                                }
                                continue;
                            } else if key == b"color" {
                                transformed_elem.push_attribute(("color", mono_color.as_str()));
                                continue;
                            } else if key == b"style" {
                                let transformed_style =
                                    transform_style_for_monochrome(val, mono_color);
                                transformed_elem
                                    .push_attribute(("style", transformed_style.as_str()));
                                continue;
                            }
                        }
                        if let (Some(prec), true) = (options.precision, is_numeric_attr(key)) {
                            let rounded = round_numbers_in_str(val, prec);
                            transformed_elem.push_attribute((key, rounded.as_bytes()));
                            continue;
                        }
                        transformed_elem.push_attribute(attr);
                    }
                    group_stack.push(PendingGroup {
                        elem: transformed_elem.into_owned(),
                        written: false,
                    });
                } else {
                    for g in group_stack.iter_mut() {
                        if !g.written {
                            writer.write_event(Event::Start(g.elem.clone()))?;
                            g.written = true;
                        }
                    }
                    write_transformed_element(&mut writer, e, false, options)?;
                }
            }
            Event::Empty(ref e) if e.name().as_ref() == b"svg" => {
                let mut elem = BytesStart::new("svg");
                let mut empty_w = None;
                let mut empty_h = None;
                let mut empty_vb = None;
                for attr in e.attributes().flatten() {
                    let key = attr.key.as_ref();
                    let val = std::str::from_utf8(&attr.value).unwrap_or("");
                    if options.remove_metadata && key.starts_with(b"data-") {
                        continue;
                    }
                    if key == b"width" {
                        empty_w = Some(val.to_string());
                    } else if key == b"height" {
                        empty_h = Some(val.to_string());
                    } else if key == b"viewBox" {
                        empty_vb = Some(val.to_string());
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

                if options.responsive && empty_vb.is_none() {
                    let dims = empty_w.as_ref().zip(empty_h.as_ref());
                    if let Some((w, h)) = dims {
                        let w_px = parse_svg_dimension_to_px(w);
                        let h_px = parse_svg_dimension_to_px(h);
                        if let (Some(wp), Some(hp)) = (w_px, h_px) {
                            let synthesized = format!("0 0 {wp} {hp}");
                            let final_viewbox = if let Some(prec) = options.precision {
                                round_numbers_in_str(&synthesized, prec)
                            } else {
                                Cow::Borrowed(synthesized.as_str())
                            };
                            elem.push_attribute(("viewBox", final_viewbox.as_ref()));
                        }
                    }
                }

                writer.write_event(Event::Empty(elem))?;
            }
            Event::Empty(ref e) => {
                for g in group_stack.iter_mut() {
                    if !g.written {
                        writer.write_event(Event::Start(g.elem.clone()))?;
                        g.written = true;
                    }
                }
                write_transformed_element(&mut writer, e, true, options)?;
            }
            Event::Text(ref e) => {
                let slice: &[u8] = e.as_ref();
                let is_whitespace = slice.iter().all(|&b| b.is_ascii_whitespace());
                if in_style_tag
                    && let (Ok(text), Some(mono)) =
                        (std::str::from_utf8(slice), options.monochrome.as_ref())
                {
                    for g in group_stack.iter_mut() {
                        if !g.written {
                            writer.write_event(Event::Start(g.elem.clone()))?;
                            g.written = true;
                        }
                    }
                    let transformed = transform_css_for_monochrome(text, mono);
                    writer.write_event(Event::Text(quick_xml::events::BytesText::new(
                        &transformed,
                    )))?;
                    continue;
                }
                if options.minify && is_whitespace {
                    // Minify: omit whitespace text
                    continue;
                }
                if !is_whitespace {
                    for g in group_stack.iter_mut() {
                        if !g.written {
                            writer.write_event(Event::Start(g.elem.clone()))?;
                            g.written = true;
                        }
                    }
                }
                writer.write_event(Event::Text(e.clone()))?;
            }
            Event::CData(ref e) => {
                let slice: &[u8] = e.as_ref();
                if in_style_tag
                    && let (Ok(text), Some(mono)) =
                        (std::str::from_utf8(slice), options.monochrome.as_ref())
                {
                    for g in group_stack.iter_mut() {
                        if !g.written {
                            writer.write_event(Event::Start(g.elem.clone()))?;
                            g.written = true;
                        }
                    }
                    let transformed = transform_css_for_monochrome(text, mono);
                    writer.write_event(Event::CData(quick_xml::events::BytesCData::new(
                        &transformed,
                    )))?;
                    continue;
                }
                for g in group_stack.iter_mut() {
                    if !g.written {
                        writer.write_event(Event::Start(g.elem.clone()))?;
                        g.written = true;
                    }
                }
                writer.write_event(Event::CData(e.clone()))?;
            }
            Event::End(ref e) => {
                let tag_name = e.name();
                if tag_name.as_ref() == b"style" {
                    in_style_tag = false;
                }
                if options.strip_empty_groups
                    && tag_name.as_ref() == b"g"
                    && !group_stack.is_empty()
                {
                    let top = group_stack.pop().unwrap();
                    if top.written {
                        writer.write_event(Event::End(e.clone()))?;
                    }
                } else {
                    for g in group_stack.iter_mut() {
                        if !g.written {
                            writer.write_event(Event::Start(g.elem.clone()))?;
                            g.written = true;
                        }
                    }
                    writer.write_event(Event::End(e.clone()))?;
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

fn round_numbers_in_str<'a>(s: &'a str, precision: usize) -> Cow<'a, str> {
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
            if (has_dot || has_exp)
                && let Ok(val) = raw.parse::<f64>()
            {
                use std::io::Write as IoWrite;
                let mut fmt_buf = [0u8; 32];
                let fmt_len = {
                    let mut cursor = std::io::Cursor::new(&mut fmt_buf[..]);
                    let _ = write!(cursor, "{val:.precision$}");
                    cursor.position() as usize
                };
                let formatted_str = std::str::from_utf8(&fmt_buf[..fmt_len]).unwrap_or(raw);
                let trimmed = if formatted_str.contains('.') {
                    formatted_str.trim_end_matches('0').trim_end_matches('.')
                } else {
                    formatted_str
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
            Cow::Owned(out)
        }
        None => Cow::Borrowed(s),
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
    let is_path = name_bytes == b"path";

    let mut is_path_empty_after_clean = false;
    let mut cleaned_d = None;

    if options.clean_paths && is_path {
        for attr in e.attributes().flatten() {
            if attr.key.as_ref() == b"d" {
                let val = std::str::from_utf8(&attr.value).unwrap_or("");
                let cleaned = clean_svg_path(val);
                if cleaned.is_empty() {
                    is_path_empty_after_clean = true;
                } else {
                    cleaned_d = Some(cleaned);
                }
                break;
            }
        }
    }

    if is_empty && is_path_empty_after_clean {
        return Ok(());
    }

    let mut elem = BytesStart::new(name);

    for attr in e.attributes().flatten() {
        let key = attr.key.as_ref();
        let val = std::str::from_utf8(&attr.value).unwrap_or("");

        if options.remove_metadata && key.starts_with(b"data-") {
            continue;
        }

        if key == b"d"
            && is_path
            && let Some(ref d_str) = cleaned_d
        {
            if let Some(prec) = options.precision {
                let rounded = round_numbers_in_str(d_str, prec);
                elem.push_attribute((key, rounded.as_bytes()));
            } else {
                elem.push_attribute((key, d_str.as_bytes()));
            }
            continue;
        }

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
            } else if key == b"stop-color" {
                elem.push_attribute(("stop-color", mono_color.as_str()));
                continue;
            } else if key == b"color" {
                elem.push_attribute(("color", mono_color.as_str()));
                continue;
            } else if key == b"style" {
                let transformed_style = transform_style_for_monochrome(val, mono_color);
                elem.push_attribute(("style", transformed_style.as_str()));
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

fn transform_style_for_monochrome(style: &str, mono: &str) -> String {
    let mut parts = Vec::new();
    for decl in style.split(';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }
        if let Some((prop, val)) = decl.split_once(':') {
            let p = prop.trim();
            let v = val.trim();
            if (p == "fill" || p == "stroke" || p == "stop-color" || p == "color") && v != "none" {
                parts.push(format!("{p}: {mono}"));
            } else {
                parts.push(decl.to_string());
            }
        } else {
            parts.push(decl.to_string());
        }
    }
    parts.join("; ")
}

fn parse_svg_dimension_to_px(s: &str) -> Option<f64> {
    let value = s.trim();
    let lower = value.to_ascii_lowercase();
    let (number, unit) = ["px", "pt", "in", "cm", "mm", "pc"]
        .into_iter()
        .find_map(|unit| lower.strip_suffix(unit).map(|number| (number.trim(), unit)))
        .unwrap_or((value, ""));
    let number = number.parse::<f64>().ok()?;
    let px = match unit {
        "px" | "" => number,
        "pt" => number * 96.0 / 72.0,
        "in" => number * 96.0,
        "cm" => number * 96.0 / 2.54,
        "mm" => number * 96.0 / 25.4,
        "pc" => number * 16.0,
        _ => number,
    };
    Some(px)
}

pub(crate) fn transform_css_for_monochrome(css: &str, mono: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut in_block = false;
    let mut block_buf = String::new();

    for ch in css.chars() {
        if ch == '{' {
            in_block = true;
            out.push('{');
            block_buf.clear();
        } else if ch == '}' {
            in_block = false;
            let transformed = transform_style_for_monochrome(&block_buf, mono);
            out.push_str(&transformed);
            out.push('}');
            block_buf.clear();
        } else if in_block {
            block_buf.push(ch);
        } else {
            out.push(ch);
        }
    }
    if !block_buf.is_empty() {
        out.push_str(&block_buf);
    }
    out
}

#[derive(Debug, PartialEq, Clone)]
enum PathToken {
    Command(char),
    Number(f64),
}

fn tokenize_svg_path(d: &str) -> Vec<PathToken> {
    let mut tokens = Vec::new();
    let bytes = d.as_bytes();
    let n = bytes.len();
    let mut i = 0;

    while i < n {
        let ch = bytes[i];
        if ch.is_ascii_whitespace() || ch == b',' {
            i += 1;
            continue;
        }
        if ch.is_ascii_alphabetic() {
            tokens.push(PathToken::Command(ch as char));
            i += 1;
            continue;
        }
        let start = i;
        if ch == b'+' || ch == b'-' {
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
        if start < i
            && let Ok(num) = d[start..i].parse::<f64>()
            && num.is_finite()
        {
            tokens.push(PathToken::Number(num));
        }
    }
    tokens
}

pub fn clean_svg_path(d: &str) -> String {
    let tokens = tokenize_svg_path(d);
    if tokens.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(d.len());
    let mut i = 0;
    let n = tokens.len();

    let mut curr_x = 0.0f64;
    let mut curr_y = 0.0f64;
    let mut start_x = 0.0f64;
    let mut start_y = 0.0f64;
    let mut last_cmd = ' ';
    let mut current_cmd = ' ';
    let mut has_drawn_segment = false;

    while i < n {
        match tokens[i] {
            PathToken::Command(cmd) => {
                current_cmd = cmd;
                i += 1;
                match cmd {
                    'Z' | 'z' => {
                        if last_cmd != 'Z' && last_cmd != 'z' {
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push('Z');
                            last_cmd = 'Z';
                            curr_x = start_x;
                            curr_y = start_y;
                            has_drawn_segment = true;
                        }
                    }
                    _ => {}
                }
            }
            PathToken::Number(num) => {
                let cmd = current_cmd;
                match cmd {
                    'M' => {
                        if i + 1 < n
                            && let PathToken::Number(y_val) = tokens[i + 1]
                        {
                            curr_x = num;
                            curr_y = y_val;
                            start_x = curr_x;
                            start_y = curr_y;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!(
                                "M {} {}",
                                format_coord(num),
                                format_coord(y_val)
                            ));
                            last_cmd = 'M';
                            current_cmd = 'L';
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    'm' => {
                        if i + 1 < n
                            && let PathToken::Number(dy) = tokens[i + 1]
                        {
                            curr_x += num;
                            curr_y += dy;
                            start_x = curr_x;
                            start_y = curr_y;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!("m {} {}", format_coord(num), format_coord(dy)));
                            last_cmd = 'm';
                            current_cmd = 'l';
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    'L' => {
                        if i + 1 < n
                            && let PathToken::Number(y_val) = tokens[i + 1]
                        {
                            if (curr_x - num).abs() > 1e-6 || (curr_y - y_val).abs() > 1e-6 {
                                curr_x = num;
                                curr_y = y_val;
                                if !out.is_empty() {
                                    out.push(' ');
                                }
                                out.push_str(&format!(
                                    "L {} {}",
                                    format_coord(num),
                                    format_coord(y_val)
                                ));
                                last_cmd = 'L';
                                has_drawn_segment = true;
                            }
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    'l' => {
                        if i + 1 < n
                            && let PathToken::Number(dy) = tokens[i + 1]
                        {
                            if num.abs() > 1e-6 || dy.abs() > 1e-6 {
                                curr_x += num;
                                curr_y += dy;
                                if !out.is_empty() {
                                    out.push(' ');
                                }
                                out.push_str(&format!(
                                    "l {} {}",
                                    format_coord(num),
                                    format_coord(dy)
                                ));
                                last_cmd = 'l';
                                has_drawn_segment = true;
                            }
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    'H' => {
                        if (curr_x - num).abs() > 1e-6 {
                            curr_x = num;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!("H {}", format_coord(num)));
                            last_cmd = 'H';
                            has_drawn_segment = true;
                        }
                        i += 1;
                    }
                    'h' => {
                        if num.abs() > 1e-6 {
                            curr_x += num;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!("h {}", format_coord(num)));
                            last_cmd = 'h';
                            has_drawn_segment = true;
                        }
                        i += 1;
                    }
                    'V' => {
                        if (curr_y - num).abs() > 1e-6 {
                            curr_y = num;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!("V {}", format_coord(num)));
                            last_cmd = 'V';
                            has_drawn_segment = true;
                        }
                        i += 1;
                    }
                    'v' => {
                        if num.abs() > 1e-6 {
                            curr_y += num;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!("v {}", format_coord(num)));
                            last_cmd = 'v';
                            has_drawn_segment = true;
                        }
                        i += 1;
                    }
                    'C' => {
                        if i + 5 < n
                            && let (
                                PathToken::Number(y1),
                                PathToken::Number(x2),
                                PathToken::Number(y2),
                                PathToken::Number(x),
                                PathToken::Number(y),
                            ) = (
                                &tokens[i + 1],
                                &tokens[i + 2],
                                &tokens[i + 3],
                                &tokens[i + 4],
                                &tokens[i + 5],
                            )
                        {
                            curr_x = *x;
                            curr_y = *y;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!(
                                "C {} {} {} {} {} {}",
                                format_coord(num),
                                format_coord(*y1),
                                format_coord(*x2),
                                format_coord(*y2),
                                format_coord(*x),
                                format_coord(*y)
                            ));
                            last_cmd = 'C';
                            has_drawn_segment = true;
                            i += 6;
                        } else {
                            i += 1;
                        }
                    }
                    'c' => {
                        if i + 5 < n
                            && let (
                                PathToken::Number(y1),
                                PathToken::Number(x2),
                                PathToken::Number(y2),
                                PathToken::Number(dx),
                                PathToken::Number(dy),
                            ) = (
                                &tokens[i + 1],
                                &tokens[i + 2],
                                &tokens[i + 3],
                                &tokens[i + 4],
                                &tokens[i + 5],
                            )
                        {
                            curr_x += *dx;
                            curr_y += *dy;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!(
                                "c {} {} {} {} {} {}",
                                format_coord(num),
                                format_coord(*y1),
                                format_coord(*x2),
                                format_coord(*y2),
                                format_coord(*dx),
                                format_coord(*dy)
                            ));
                            last_cmd = 'c';
                            has_drawn_segment = true;
                            i += 6;
                        } else {
                            i += 1;
                        }
                    }
                    'S' => {
                        if i + 3 < n
                            && let (
                                PathToken::Number(y2),
                                PathToken::Number(x),
                                PathToken::Number(y),
                            ) = (&tokens[i + 1], &tokens[i + 2], &tokens[i + 3])
                        {
                            curr_x = *x;
                            curr_y = *y;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!(
                                "S {} {} {} {}",
                                format_coord(num),
                                format_coord(*y2),
                                format_coord(*x),
                                format_coord(*y)
                            ));
                            last_cmd = 'S';
                            has_drawn_segment = true;
                            i += 4;
                        } else {
                            i += 1;
                        }
                    }
                    's' => {
                        if i + 3 < n
                            && let (
                                PathToken::Number(y2),
                                PathToken::Number(dx),
                                PathToken::Number(dy),
                            ) = (&tokens[i + 1], &tokens[i + 2], &tokens[i + 3])
                        {
                            curr_x += *dx;
                            curr_y += *dy;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!(
                                "s {} {} {} {}",
                                format_coord(num),
                                format_coord(*y2),
                                format_coord(*dx),
                                format_coord(*dy)
                            ));
                            last_cmd = 's';
                            has_drawn_segment = true;
                            i += 4;
                        } else {
                            i += 1;
                        }
                    }
                    'Q' => {
                        if i + 3 < n
                            && let (
                                PathToken::Number(y1),
                                PathToken::Number(x),
                                PathToken::Number(y),
                            ) = (&tokens[i + 1], &tokens[i + 2], &tokens[i + 3])
                        {
                            curr_x = *x;
                            curr_y = *y;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!(
                                "Q {} {} {} {}",
                                format_coord(num),
                                format_coord(*y1),
                                format_coord(*x),
                                format_coord(*y)
                            ));
                            last_cmd = 'Q';
                            has_drawn_segment = true;
                            i += 4;
                        } else {
                            i += 1;
                        }
                    }
                    'q' => {
                        if i + 3 < n
                            && let (
                                PathToken::Number(y1),
                                PathToken::Number(dx),
                                PathToken::Number(dy),
                            ) = (&tokens[i + 1], &tokens[i + 2], &tokens[i + 3])
                        {
                            curr_x += *dx;
                            curr_y += *dy;
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!(
                                "q {} {} {} {}",
                                format_coord(num),
                                format_coord(*y1),
                                format_coord(*dx),
                                format_coord(*dy)
                            ));
                            last_cmd = 'q';
                            has_drawn_segment = true;
                            i += 4;
                        } else {
                            i += 1;
                        }
                    }
                    'A' | 'a' => {
                        if i + 6 < n
                            && let (
                                PathToken::Number(ry),
                                PathToken::Number(x_rot),
                                PathToken::Number(large_arc),
                                PathToken::Number(sweep),
                                PathToken::Number(x),
                                PathToken::Number(y),
                            ) = (
                                &tokens[i + 1],
                                &tokens[i + 2],
                                &tokens[i + 3],
                                &tokens[i + 4],
                                &tokens[i + 5],
                                &tokens[i + 6],
                            )
                        {
                            if cmd == 'A' {
                                curr_x = *x;
                                curr_y = *y;
                            } else {
                                curr_x += *x;
                                curr_y += *y;
                            }
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(&format!(
                                "{} {} {} {} {} {} {} {}",
                                cmd,
                                format_coord(num),
                                format_coord(*ry),
                                format_coord(*x_rot),
                                format_coord(*large_arc),
                                format_coord(*sweep),
                                format_coord(*x),
                                format_coord(*y)
                            ));
                            last_cmd = cmd;
                            has_drawn_segment = true;
                            i += 7;
                        } else {
                            i += 1;
                        }
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
        }
    }

    if !has_drawn_segment {
        return String::new();
    }
    out
}

fn format_coord(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e9 {
        format!("{:.0}", v)
    } else {
        let s = format!("{:.6}", v);
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        if trimmed.is_empty() || trimmed == "-0" {
            "0".to_string()
        } else {
            trimmed.to_string()
        }
    }
}
