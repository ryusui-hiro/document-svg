//! Cell labels: the HTML subset draw.io writes into them, word wrapping by
//! script-aware character widths, and placement inside the label's own box.

use super::*;

pub(super) fn font_family(style: &Style) -> String {
    let requested = style.text("fontfamily", "Helvetica");
    let requested = requested.trim();
    let primary = if requested.is_empty() {
        "Helvetica".to_owned()
    } else if requested.contains(' ') && !requested.starts_with('\'') {
        format!("'{requested}'")
    } else {
        requested.to_owned()
    };
    format!("{primary}, Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif")
}

pub(super) fn base_run(style: &Style) -> TextRun {
    let bits = style.number("fontstyle", 0.0) as i64;
    let opacity = (style.number("textopacity", 100.0) / 100.0).clamp(0.0, 1.0);
    TextRun {
        text: String::new(),
        font_family: font_family(style),
        font_size: style.number("fontsize", DEFAULT_FONT_SIZE).max(1.0),
        bold: bits & 1 != 0,
        italic: bits & 2 != 0,
        fill: Paint::Solid {
            color: style
                .color("fontcolor")
                .unwrap_or_else(|| "#000000".to_owned()),
            opacity,
        },
        ..TextRun::default()
    }
}

pub(super) fn run_width(run: &TextRun) -> f64 {
    run.text
        .chars()
        .map(|character| run.font_size * text_advance_factor(character))
        .sum()
}

pub(super) fn line_width(line: &[TextRun]) -> f64 {
    line.iter().map(run_width).sum()
}

pub(super) fn line_height(line: &[TextRun], fallback: f64) -> f64 {
    line.iter()
        .map(|run| run.font_size)
        .fold(fallback, f64::max)
}

/// Split runs into lines, breaking on explicit newlines and, when `wrap` is
/// set, on the last word boundary that still fits `width`.
pub(super) fn wrap_runs(runs: &[TextRun], width: f64, wrap: bool) -> Vec<Vec<TextRun>> {
    let mut lines: Vec<Vec<TextRun>> = vec![Vec::new()];
    let mut used = 0.0;
    let mut word: Vec<(char, TextRun)> = Vec::new();
    let mut word_width = 0.0;
    let mut space: Vec<(char, TextRun)> = Vec::new();
    let mut space_width = 0.0;
    let flush_word = |lines: &mut Vec<Vec<TextRun>>,
                      used: &mut f64,
                      word: &mut Vec<(char, TextRun)>,
                      word_width: &mut f64,
                      space: &mut Vec<(char, TextRun)>,
                      space_width: &mut f64| {
        if word.is_empty() {
            return;
        }
        let fits = !wrap || width <= 0.0 || *used + *space_width + *word_width <= width;
        if fits || *used == 0.0 {
            if *used > 0.0 || !wrap {
                append(lines.last_mut().expect("line"), space);
                *used += *space_width;
            }
        } else {
            lines.push(Vec::new());
            *used = 0.0;
        }
        append(lines.last_mut().expect("line"), word);
        *used += *word_width;
        word.clear();
        *word_width = 0.0;
        space.clear();
        *space_width = 0.0;
    };
    for run in runs {
        for character in run.text.chars() {
            if character == '\n' {
                flush_word(
                    &mut lines,
                    &mut used,
                    &mut word,
                    &mut word_width,
                    &mut space,
                    &mut space_width,
                );
                space.clear();
                space_width = 0.0;
                lines.push(Vec::new());
                used = 0.0;
                continue;
            }
            let advance = run.font_size * text_advance_factor(character);
            if character.is_whitespace() {
                flush_word(
                    &mut lines,
                    &mut used,
                    &mut word,
                    &mut word_width,
                    &mut space,
                    &mut space_width,
                );
                space.push((character, run.clone()));
                space_width += advance;
            } else {
                word.push((character, run.clone()));
                word_width += advance;
            }
        }
    }
    flush_word(
        &mut lines,
        &mut used,
        &mut word,
        &mut word_width,
        &mut space,
        &mut space_width,
    );
    lines
}

/// Append characters to a line, merging each stretch that shares a style into
/// one run so the SVG writer emits one tspan per style change, not per glyph.
pub(super) fn append(line: &mut Vec<TextRun>, characters: &[(char, TextRun)]) {
    for (character, style) in characters {
        let same = line.last().is_some_and(|last| {
            last.font_family == style.font_family
                && last.font_size == style.font_size
                && last.bold == style.bold
                && last.italic == style.italic
                && last.fill == style.fill
        });
        if same {
            line.last_mut().expect("run").text.push(*character);
        } else {
            let mut run = style.clone();
            run.text.clear();
            run.text.push(*character);
            line.push(run);
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(super) fn draw_label(
    cell: &Cell,
    box_rect: Rect,
    rotation: f64,
    nodes: &mut Vec<Node>,
    bounds: &mut Option<Rect>,
    clips: &mut Vec<ClipPath>,
    is_edge: bool,
) {
    if cell.label.trim().is_empty() {
        return;
    }
    let style = &cell.style;
    let base = base_run(style);
    let html = cell.label_is_html || style.flag("html");
    let runs = if html {
        html_runs(&cell.label, &base)
    } else {
        vec![TextRun {
            text: cell.label.replace("\r\n", "\n").replace('\r', "\n"),
            ..base.clone()
        }]
    };
    if runs.iter().all(|run| run.text.trim().is_empty()) {
        return;
    }
    let spacing = style.number("spacing", 0.0) + LABEL_SPACING;
    let left = spacing + style.number("spacingleft", 0.0);
    let right = spacing + style.number("spacingright", 0.0);
    let top = spacing + style.number("spacingtop", 0.0);
    let bottom = spacing + style.number("spacingbottom", 0.0);
    // `overflow=hidden` keeps a label inside the shape it belongs to; `fill`
    // and `width` both bind the text to the shape's width.
    let overflow = style.text("overflow", "visible");
    let wrap = (style.get("whitespace") == Some("wrap")
        || matches!(overflow.as_str(), "fill" | "width"))
        && !is_edge;
    let available = (box_rect.width - left - right).max(1.0);
    let lines = wrap_runs(&runs, available, wrap);
    let lines = lines
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return;
    }
    let total = lines
        .iter()
        .map(|line| line_height(line, base.font_size) * LINE_HEIGHT)
        .sum::<f64>();
    let vertical = style.text("verticalalign", "middle");
    let block_top = match vertical.as_str() {
        "top" => box_rect.y + top,
        "bottom" => box_rect.bottom() - bottom - total,
        _ => box_rect.center().1 - total / 2.0,
    };
    let align = style.text("align", "center");
    let (anchor, anchor_x) = match align.as_str() {
        "left" => (TextAnchor::Start, box_rect.x + left),
        "right" => (TextAnchor::End, box_rect.right() - right),
        _ => (TextAnchor::Middle, box_rect.center().0),
    };
    let widest = lines
        .iter()
        .map(|line| line_width(line))
        .fold(0.0, f64::max);
    let block = Rect {
        x: match anchor {
            TextAnchor::Start => anchor_x,
            TextAnchor::End => anchor_x - widest,
            TextAnchor::Middle => anchor_x - widest / 2.0,
        },
        y: block_top,
        width: widest,
        height: total,
    };
    // A label rotated with its shape, or turned on its side inside a swimlane
    // title, has to carry the same transform as the text it belongs to.
    let vertical_label = style.get("horizontal") == Some("0") && !is_edge;
    let transform = shape_transform(
        block,
        rotation + if vertical_label { -90.0 } else { 0.0 },
        false,
        false,
    );
    if let Some(color) = style.color("labelbackgroundcolor") {
        nodes.push(Node::Path {
            id: format!("drawio-{}-label-background", cell.id),
            d: rectangle_path(block.grow(2.0)),
            fill_rule: "nonzero".into(),
            fill: Paint::solid(color),
            stroke: match style.color("labelbordercolor") {
                Some(border) => Stroke {
                    paint: Paint::solid(border),
                    width: 1.0,
                    ..Stroke::default()
                },
                None => Stroke::default(),
            },
            transform,
            clip_id: None,
            meta: SourceMeta {
                kind: "drawio-label-background".into(),
                source_id: cell.id.clone(),
                ..SourceMeta::default()
            },
        });
    }
    let clip = (overflow == "hidden" && !is_edge && box_rect.width > 0.0).then(|| {
        let id = format!("drawio-{}-label-clip", cell.id);
        clips.push(ClipPath {
            id: id.clone(),
            d: rectangle_path(box_rect),
            transform,
            fill_rule: "nonzero".into(),
            parent_id: None,
            additional_paths: Vec::new(),
        });
        id
    });
    let mut offset = 0.0;
    for (index, line) in lines.into_iter().enumerate() {
        let size = line_height(&line, base.font_size);
        let baseline = block_top + offset + size * (LINE_HEIGHT - 1.0) / 2.0 + size * 0.8;
        offset += size * LINE_HEIGHT;
        nodes.push(Node::Text {
            id: format!("drawio-{}-label-{index}", cell.id),
            x: anchor_x,
            y: baseline,
            runs: line,
            anchor,
            transform,
            opacity: 1.0,
            stroke: Stroke::default(),
            clip_id: clip.clone(),
            meta: SourceMeta {
                kind: "drawio-label".into(),
                source_id: cell.id.clone(),
                ..SourceMeta::default()
            },
        });
    }
    // A clipped label cannot make the page bigger than the shape it sits in.
    if clip.is_none() {
        extend(bounds, block);
    }
}

/// Read the small HTML subset draw.io writes into a label into styled runs.
///
/// Anything outside `<br>`, `<div>`, `<p>`, `<b>`, `<i>` and `<font>` keeps its
/// text and loses its markup, which is what a preview needs: the words are all
/// there and no tag ever reaches the SVG as literal text.
pub(super) fn html_runs(text: &str, base: &TextRun) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut current = base.clone();
    let mut pending = String::new();
    let mut bold = 0usize;
    let mut italic = 0usize;
    let mut fonts = Vec::<TextRun>::new();
    let mut characters = text.chars().peekable();
    let flush = |pending: &mut String, run: &TextRun, runs: &mut Vec<TextRun>| {
        if pending.is_empty() {
            return;
        }
        runs.push(TextRun {
            text: std::mem::take(pending),
            ..run.clone()
        });
    };
    while let Some(character) = characters.next() {
        match character {
            '<' => {
                let mut tag = String::new();
                for next in characters.by_ref() {
                    if next == '>' {
                        break;
                    }
                    tag.push(next);
                }
                let closing = tag.starts_with('/');
                let body = tag.trim_start_matches('/').trim();
                let name = body
                    .split(|character: char| character.is_whitespace() || character == '/')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                match name.as_str() {
                    "br" => pending.push('\n'),
                    "div" | "p" | "li" | "tr" => {
                        if !closing && (!pending.is_empty() || !runs.is_empty()) {
                            pending.push('\n');
                        }
                    }
                    "b" | "strong" => {
                        flush(&mut pending, &current, &mut runs);
                        if closing {
                            bold = bold.saturating_sub(1);
                        } else {
                            bold += 1;
                        }
                        current.bold = base.bold || bold > 0;
                    }
                    "i" | "em" => {
                        flush(&mut pending, &current, &mut runs);
                        if closing {
                            italic = italic.saturating_sub(1);
                        } else {
                            italic += 1;
                        }
                        current.italic = base.italic || italic > 0;
                    }
                    "font" | "span" => {
                        flush(&mut pending, &current, &mut runs);
                        if closing {
                            if let Some(previous) = fonts.pop() {
                                current = previous;
                            }
                        } else {
                            fonts.push(current.clone());
                            apply_inline_style(body, &mut current);
                        }
                    }
                    _ => {}
                }
            }
            '&' => {
                let mut entity = String::new();
                let mut terminated = false;
                while let Some(&next) = characters.peek() {
                    if next == ';' {
                        characters.next();
                        terminated = true;
                        break;
                    }
                    if entity.len() >= 12 || next == '<' || next == '&' || next.is_whitespace() {
                        break;
                    }
                    entity.push(next);
                    characters.next();
                }
                // A label that already went through XML unescaping can hold a
                // bare ampersand, which is text and not the start of a
                // reference.
                if terminated && !entity.is_empty() {
                    pending.push_str(&decode_entity(&entity));
                } else {
                    pending.push('&');
                    pending.push_str(&entity);
                }
            }
            _ => pending.push(character),
        }
    }
    flush(&mut pending, &current, &mut runs);
    if runs.is_empty() {
        runs.push(base.clone());
    }
    runs
}

/// Pull colour, face and size out of a `font` or `span` tag's attributes.
pub(super) fn apply_inline_style(tag: &str, run: &mut TextRun) {
    for (name, value) in html_attributes(tag) {
        match name.as_str() {
            "color" => {
                if let Some(color) = parse_color(&value) {
                    run.fill = Paint::solid(color);
                }
            }
            "face" => run.font_family = font_family(&Style::parse(&format!("fontFamily={value}"))),
            "size" => {
                // HTML font sizes are 1-7 steps, not points.
                if let Ok(step) = value.trim().parse::<f64>()
                    && (1.0..=7.0).contains(&step)
                {
                    run.font_size = DEFAULT_FONT_SIZE * (0.75 + step * 0.125);
                }
            }
            "style" => {
                for declaration in value.split(';') {
                    let Some((property, setting)) = declaration.split_once(':') else {
                        continue;
                    };
                    let setting = setting.trim();
                    match property.trim().to_ascii_lowercase().as_str() {
                        "color" => {
                            if let Some(color) = parse_color(setting) {
                                run.fill = Paint::solid(color);
                            }
                        }
                        "font-size" => {
                            if let Ok(size) = setting.trim_end_matches("px").trim().parse::<f64>()
                                && size > 0.0
                            {
                                run.font_size = size;
                            }
                        }
                        "font-weight" => {
                            run.bold = setting == "bold"
                                || setting.parse::<f64>().is_ok_and(|weight| weight >= 600.0);
                        }
                        "font-style" => run.italic = setting == "italic",
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

pub(super) fn html_attributes(tag: &str) -> Vec<(String, String)> {
    let mut attributes = Vec::new();
    let mut characters = tag.chars().peekable();
    // Skip the element name.
    while characters
        .peek()
        .is_some_and(|character| !character.is_whitespace())
    {
        characters.next();
    }
    loop {
        while characters
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            characters.next();
        }
        let mut name = String::new();
        while let Some(&character) = characters.peek() {
            if character == '=' || character.is_whitespace() {
                break;
            }
            name.push(character);
            characters.next();
        }
        if name.is_empty() {
            break;
        }
        if characters.peek() != Some(&'=') {
            continue;
        }
        characters.next();
        let mut value = String::new();
        match characters.peek().copied() {
            Some(quote @ ('"' | '\'')) => {
                characters.next();
                for character in characters.by_ref() {
                    if character == quote {
                        break;
                    }
                    value.push(character);
                }
            }
            _ => {
                while let Some(&character) = characters.peek() {
                    if character.is_whitespace() {
                        break;
                    }
                    value.push(character);
                    characters.next();
                }
            }
        }
        attributes.push((name.trim_matches('/').to_ascii_lowercase(), value));
    }
    attributes
}

pub(super) fn decode_entity(entity: &str) -> String {
    match entity.to_ascii_lowercase().as_str() {
        "amp" => return "&".into(),
        "lt" => return "<".into(),
        "gt" => return ">".into(),
        "quot" => return "\"".into(),
        "apos" | "#39" => return "'".into(),
        "nbsp" | "#160" => return "\u{a0}".into(),
        _ => {}
    }
    if let Some(digits) = entity.strip_prefix('#') {
        let value = if let Some(hex) = digits
            .strip_prefix('x')
            .or_else(|| digits.strip_prefix('X'))
        {
            u32::from_str_radix(hex, 16).ok()
        } else {
            digits.parse::<u32>().ok()
        };
        if let Some(character) = value.and_then(char::from_u32) {
            return character.to_string();
        }
    }
    format!("&{entity};")
}
