//! AsciiDoc (.adoc, .asciidoc) technical documentation parser and vector SVG typesetter.
//!
//! Parses section titles, listing/code blocks, bullet/numbered lists, paragraphs,
//! tables, and bounded local block-image macros, rendering flowing multi-page
//! vector SVG documents.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{
    HtmlBlock, InlineHtmlImage, load_local_image_sources, render_blocks_to_pages_with_warnings,
};
use crate::error::{Error, Result};

const MAX_ASCIIDOC_IMAGE_REFERENCES: usize = 10_000;
const MAX_ASCIIDOC_ATTRIBUTE_BYTES: usize = 4_096;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "AsciiDoc input")?;
    let text = String::from_utf8(bytes)
        .map_err(|e| Error::InvalidInput(format!("AsciiDoc file is not valid UTF-8: {e}")))?;

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_dir = fs::canonicalize(parent)?;
    let imagesdir = parse_imagesdir(&text);
    let (sources, source_limit_exceeded) =
        collect_asciidoc_image_sources(&text, imagesdir.as_deref());
    let (inline_images, mut warnings) =
        load_local_image_sources(&base_dir, sources, source_limit_exceeded)?;
    if !inline_images.is_empty() {
        push_asciidoc_warning_once(
            &mut warnings,
            "AsciiDoc block images are rendered as centered flow blocks; dimensions, alignment, captions, and inline wrapping are approximated",
        );
    }
    let (blocks, parser_warnings) =
        parse_asciidoc_blocks_inner(&text, &inline_images, imagesdir.as_deref())?;
    for warning in parser_warnings {
        push_asciidoc_warning_once(&mut warnings, &warning);
    }
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

/// Parses AsciiDoc text into a sequence of renderable [`HtmlBlock`] elements.
pub fn parse_asciidoc_blocks(text: &str) -> Result<Vec<HtmlBlock>> {
    parse_asciidoc_blocks_inner(text, &HashMap::new(), None).map(|(blocks, _)| blocks)
}

fn parse_asciidoc_blocks_inner(
    text: &str,
    inline_images: &HashMap<String, InlineHtmlImage>,
    imagesdir: Option<&str>,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut ordered_counter = 1usize;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            ordered_counter = 1;
            i += 1;
            continue;
        }

        // Document attributes configure subsequent macros and are not content
        // paragraphs. The image loader currently honors the bounded
        // `:imagesdir:` attribute below.
        if trimmed.starts_with(":imagesdir:") {
            i += 1;
            continue;
        }

        // Block image macro (`image::target[Alt]`). Asciidoctor requires the
        // macro to occupy its own line; local PNG/JPEG targets are embedded by
        // the file-based converter, while URLs and unsupported resources stay
        // inert and become an explicit placeholder.
        if let Some(image_macro) = parse_asciidoc_image_macro(trimmed) {
            let source = asciidoc_image_source(imagesdir, &image_macro.target);
            if let Some(image) = inline_images.get(&source) {
                let (pixel_width, pixel_height) = asciidoc_display_dimensions(image, &image_macro);
                blocks.push(HtmlBlock::Image {
                    href: image.href.clone(),
                    pixel_width,
                    pixel_height,
                    alt: image_macro.alt,
                });
            } else {
                push_asciidoc_warning_once(
                    &mut warnings,
                    "AsciiDoc image target was omitted because it was not a validated local PNG/JPEG resource",
                );
                if !image_macro.alt.is_empty() {
                    blocks.push(HtmlBlock::Paragraph {
                        text: format!(
                            "[Image omitted: {}]",
                            clean_asciidoc_inline(&image_macro.alt)
                        ),
                    });
                }
            }
            if image_macro.has_unsupported_attributes {
                push_asciidoc_warning_once(
                    &mut warnings,
                    "AsciiDoc image width, height, alignment, links, and captions are approximated or ignored",
                );
            }
            i += 1;
            continue;
        }

        // 1. Delimited table (|=== ... |===)
        if trimmed.starts_with("|===") {
            i += 1;
            let mut table_lines = Vec::new();
            while i < lines.len() {
                let tl = lines[i];
                if tl.trim().starts_with("|===") {
                    i += 1;
                    break;
                }
                table_lines.push(tl);
                i += 1;
            }
            if let Some(table_data) = parse_asciidoc_table(&table_lines) {
                blocks.push(HtmlBlock::Table(table_data));
            }
            continue;
        }

        // 2. Delimited listing / source code block (---- or ....)
        if trimmed.starts_with("----") || trimmed.starts_with("....") {
            let delim = if trimmed.starts_with("----") {
                "----"
            } else {
                "...."
            };
            i += 1;
            let mut code_lines = Vec::new();
            while i < lines.len() {
                let cl = lines[i];
                if cl.trim().starts_with(delim) {
                    i += 1;
                    break;
                }
                code_lines.push(cl);
                i += 1;
            }
            blocks.push(HtmlBlock::CodeBlock {
                text: code_lines.join("\n"),
            });
            continue;
        }

        // 2b. Delimited example/admonition/sidebar/quote block (====, ****, ____),
        // commonly preceded by a block attribute line such as [NOTE] or [CAUTION].
        if is_delimiter_run(trimmed, '=')
            || is_delimiter_run(trimmed, '*')
            || is_delimiter_run(trimmed, '_')
        {
            let marker = trimmed.chars().next().unwrap_or('=');
            i += 1;
            let mut block_lines = Vec::new();
            while i < lines.len() {
                let bl = lines[i];
                if is_delimiter_run(bl.trim(), marker) {
                    i += 1;
                    break;
                }
                block_lines.push(bl);
                i += 1;
            }
            let mut prefix = String::new();
            if let Some(HtmlBlock::Paragraph { text }) = blocks.last()
                && let Some(tag) = admonition_block_tag(text)
            {
                prefix = format!("[{tag}] ");
                blocks.pop();
            }
            let mut first = true;
            for group in block_lines.split(|l| l.trim().is_empty()) {
                let joined: Vec<&str> = group
                    .iter()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .collect();
                if joined.is_empty() {
                    continue;
                }
                let text = clean_asciidoc_inline(&joined.join(" "));
                let text = if first && !prefix.is_empty() {
                    format!("{prefix}{text}")
                } else {
                    text
                };
                blocks.push(HtmlBlock::Paragraph { text });
                first = false;
            }
            if first && !prefix.is_empty() {
                // The block had no renderable content; restore the attribute line
                // rather than silently dropping it.
                blocks.push(HtmlBlock::Paragraph {
                    text: prefix.trim_end().to_string(),
                });
            }
            continue;
        }

        // 3. Section Headings (= Title, == Section, === Sub-section, etc.)
        if let Some((level, title)) = asciidoc_heading(trimmed) {
            blocks.push(HtmlBlock::Heading {
                level,
                text: clean_asciidoc_inline(title),
            });
            i += 1;
            continue;
        }

        // 4. Horizontal Rule (''')
        if trimmed == "'''" {
            blocks.push(HtmlBlock::HorizontalRule);
            i += 1;
            continue;
        }

        // 5. Admonitions (NOTE:, TIP:, IMPORTANT:, WARNING:, CAUTION:)
        let admonition = trimmed
            .strip_prefix("NOTE: ")
            .map(|r| ("[NOTE] ", r))
            .or_else(|| trimmed.strip_prefix("TIP: ").map(|r| ("[TIP] ", r)))
            .or_else(|| {
                trimmed
                    .strip_prefix("IMPORTANT: ")
                    .map(|r| ("[IMPORTANT] ", r))
            })
            .or_else(|| trimmed.strip_prefix("WARNING: ").map(|r| ("[WARNING] ", r)))
            .or_else(|| trimmed.strip_prefix("CAUTION: ").map(|r| ("[CAUTION] ", r)));
        if let Some((prefix, rest)) = admonition {
            blocks.push(HtmlBlock::Paragraph {
                text: format!("{prefix}{}", clean_asciidoc_inline(rest)),
            });
            i += 1;
            continue;
        }

        // 6. Unordered List (* item, ** item, - item)
        if let Some(rest) = trimmed
            .strip_prefix("* ")
            .or_else(|| trimmed.strip_prefix("** "))
            .or_else(|| trimmed.strip_prefix("- "))
        {
            blocks.push(HtmlBlock::ListItem {
                bullet: "• ".to_string(),
                text: clean_asciidoc_inline(rest),
            });
            i += 1;
            continue;
        }

        // 7. Ordered List (. item, .. item)
        if let Some(rest) = trimmed.strip_prefix(". ") {
            blocks.push(HtmlBlock::ListItem {
                bullet: format!("{ordered_counter}. "),
                text: clean_asciidoc_inline(rest),
            });
            ordered_counter += 1;
            i += 1;
            continue;
        }

        // 8. Regular Paragraph
        let mut para_lines = Vec::new();
        while i < lines.len() {
            let pl = lines[i].trim();
            if pl.is_empty()
                || asciidoc_heading(pl).is_some()
                || is_delimiter_run(pl, '=')
                || is_delimiter_run(pl, '*')
                || is_delimiter_run(pl, '_')
                || pl.starts_with("----")
                || pl.starts_with("....")
                || pl.starts_with("|===")
                || pl == "'''"
                || pl.starts_with("* ")
                || pl.starts_with("- ")
                || pl.starts_with(". ")
                || pl.starts_with("NOTE: ")
                || pl.starts_with("TIP: ")
                || pl.starts_with("IMPORTANT: ")
                || pl.starts_with("WARNING: ")
                || pl.starts_with("CAUTION: ")
            {
                break;
            }
            para_lines.push(pl);
            i += 1;
        }

        if !para_lines.is_empty() {
            blocks.push(HtmlBlock::Paragraph {
                text: clean_asciidoc_inline(&para_lines.join(" ")),
            });
        }
    }

    if blocks.is_empty() && !text.trim().is_empty() {
        blocks.push(HtmlBlock::Paragraph {
            text: clean_asciidoc_inline(text.trim()),
        });
    }

    Ok((blocks, warnings))
}

#[derive(Debug)]
struct AsciiDocImageMacro {
    target: String,
    alt: String,
    width: Option<u32>,
    height: Option<u32>,
    has_unsupported_attributes: bool,
}

fn parse_asciidoc_image_macro(line: &str) -> Option<AsciiDocImageMacro> {
    let rest = line.strip_prefix("image::")?;
    let open = rest.find('[')?;
    if !rest.ends_with(']') || open == 0 {
        return None;
    }
    let target = rest[..open].trim();
    if target.is_empty() || target.chars().any(char::is_control) {
        return None;
    }
    let attributes = &rest[open + 1..rest.len() - 1];
    let mut positional = Vec::new();
    let mut named = HashMap::new();
    for value in split_asciidoc_attributes(attributes) {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if let Some((name, value)) = value.split_once('=') {
            named.insert(
                name.trim().to_ascii_lowercase(),
                unquote_asciidoc_attribute(value.trim()),
            );
        } else {
            positional.push(unquote_asciidoc_attribute(value));
        }
    }
    let alt = named
        .get("alt")
        .cloned()
        .or_else(|| positional.first().cloned())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            Path::new(target)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Embedded image")
                .replace(['_', '-'], " ")
        });
    let width = named
        .get("width")
        .and_then(|value| parse_asciidoc_dimension(value))
        .or_else(|| {
            positional
                .get(1)
                .and_then(|value| parse_asciidoc_dimension(value))
        });
    let height = named
        .get("height")
        .and_then(|value| parse_asciidoc_dimension(value))
        .or_else(|| {
            positional
                .get(2)
                .and_then(|value| parse_asciidoc_dimension(value))
        });
    let has_unsupported_attributes = positional.len() > 1
        || named.keys().any(|name| {
            !matches!(
                name.as_str(),
                "alt" | "width" | "height" | "align" | "float" | "title"
            )
        });
    Some(AsciiDocImageMacro {
        target: target.to_owned(),
        alt,
        width,
        height,
        has_unsupported_attributes,
    })
}

fn parse_asciidoc_dimension(value: &str) -> Option<u32> {
    value
        .trim()
        .strip_suffix("px")
        .unwrap_or(value.trim())
        .parse::<u32>()
        .ok()
        .filter(|value| (1..=4096).contains(value))
}

fn asciidoc_display_dimensions(
    image: &InlineHtmlImage,
    image_macro: &AsciiDocImageMacro,
) -> (u32, u32) {
    let intrinsic_width = image.pixel_width.max(1);
    let intrinsic_height = image.pixel_height.max(1);
    match (image_macro.width, image_macro.height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (
            width,
            ((u64::from(intrinsic_height) * u64::from(width)) / u64::from(intrinsic_width))
                .clamp(1, 4096) as u32,
        ),
        (None, Some(height)) => (
            ((u64::from(intrinsic_width) * u64::from(height)) / u64::from(intrinsic_height))
                .clamp(1, 4096) as u32,
            height,
        ),
        (None, None) => (intrinsic_width, intrinsic_height),
    }
}

fn split_asciidoc_attributes(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    for (index, character) in value.char_indices() {
        match character {
            '"' | '\'' if quote == Some(character) => quote = None,
            '"' | '\'' if quote.is_none() => quote = Some(character),
            ',' if quote.is_none() => {
                values.push(value[start..index].to_owned());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    values.push(value[start..].to_owned());
    values
}

fn unquote_asciidoc_attribute(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim()
        .to_owned()
}

fn parse_imagesdir(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.trim().strip_prefix(":imagesdir:")?.trim();
        if value.is_empty() || value.len() > MAX_ASCIIDOC_ATTRIBUTE_BYTES {
            return None;
        }
        (!value.chars().any(char::is_control)).then(|| value.to_owned())
    })
}

fn asciidoc_image_source(imagesdir: Option<&str>, target: &str) -> String {
    let Some(imagesdir) = imagesdir.filter(|value| !value.is_empty()) else {
        return target.to_owned();
    };
    if target.starts_with('/') || target.contains("://") || imagesdir.contains("://") {
        return target.to_owned();
    }
    format!("{imagesdir}/{target}")
}

fn collect_asciidoc_image_sources(text: &str, imagesdir: Option<&str>) -> (Vec<String>, bool) {
    let mut sources = Vec::new();
    let mut exceeded = false;
    let mut listing_delimiter = None::<&str>;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(delimiter) = listing_delimiter {
            if trimmed.starts_with(delimiter) {
                listing_delimiter = None;
            }
            continue;
        }
        if trimmed.starts_with("----") {
            listing_delimiter = Some("----");
            continue;
        }
        if trimmed.starts_with("....") {
            listing_delimiter = Some("....");
            continue;
        }
        let Some(image_macro) = parse_asciidoc_image_macro(trimmed) else {
            continue;
        };
        if sources.len() >= MAX_ASCIIDOC_IMAGE_REFERENCES {
            exceeded = true;
            continue;
        }
        sources.push(asciidoc_image_source(imagesdir, &image_macro.target));
    }
    (sources, exceeded)
}

fn push_asciidoc_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

/// True when `trimmed` is a bare delimiter line for an example/sidebar/quote
/// block: four or more of the same character (====, ****, ____) and nothing else.
/// A shorter or mixed-content run (e.g. a "* item" bullet, or a heading like
/// "== Title") does not match, which matters: this predicate is also used to
/// decide when a plain-paragraph line has been fully consumed, so it must
/// agree with [`asciidoc_heading`] on every input or a line could be claimed
/// by neither the block dispatch nor the paragraph collector, stalling `i`.
fn is_delimiter_run(trimmed: &str, marker: char) -> bool {
    trimmed.len() >= 4 && trimmed.chars().all(|c| c == marker)
}

/// Parses a `= Title` / `== Section` / ... heading line, returning its level
/// (1-6) and title text. Returns `None` for a line that merely starts with
/// '=' but isn't a well-formed heading (no space after the marker run, or
/// more than 6 '=' characters) — such a line falls through to plain
/// paragraph text instead, matching Asciidoctor.
///
/// This is shared between the heading dispatch and the paragraph collector's
/// stop condition so the two never disagree about whether a line counts as a
/// heading; disagreement previously let a malformed heading-like line (e.g. a
/// bare `====` block delimiter) match the paragraph loop's old looser
/// `starts_with('=')` check without matching the dispatch above it, so
/// neither side consumed the line and the outer parse loop spun on it forever.
fn asciidoc_heading(trimmed: &str) -> Option<(u8, &str)> {
    if !trimmed.starts_with('=') {
        return None;
    }
    let eq_count = trimmed.chars().take_while(|&c| c == '=').count();
    if eq_count <= 6 && trimmed.chars().nth(eq_count) == Some(' ') {
        Some((eq_count as u8, trimmed[eq_count..].trim()))
    } else {
        None
    }
}

/// Maps a paragraph consisting solely of a known block-attribute tag (e.g.
/// `[CAUTION]`) to its admonition label, so a block-style admonition
/// (`[CAUTION]` followed by a `====` example block) renders with the same
/// `[CAUTION] ...` prefix as the inline `CAUTION: ...` form.
fn admonition_block_tag(text: &str) -> Option<&'static str> {
    match text.strip_prefix('[')?.strip_suffix(']')? {
        "NOTE" => Some("NOTE"),
        "TIP" => Some("TIP"),
        "IMPORTANT" => Some("IMPORTANT"),
        "WARNING" => Some("WARNING"),
        "CAUTION" => Some("CAUTION"),
        _ => None,
    }
}

fn parse_asciidoc_table(lines: &[&str]) -> Option<crate::table::TableData> {
    if lines.is_empty() {
        return None;
    }

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current_cells: Vec<String> = Vec::new();

    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            if !current_cells.is_empty() {
                rows.push(std::mem::take(&mut current_cells));
            }
            continue;
        }

        if t.starts_with('|') {
            let pipe_count = t.matches('|').count();
            if pipe_count > 1 {
                let row: Vec<String> = t
                    .split('|')
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(|c| c.to_string())
                    .collect();
                if !row.is_empty() {
                    rows.push(row);
                }
            } else {
                let cell = t.strip_prefix('|').unwrap_or(t).trim().to_string();
                current_cells.push(cell);
            }
        }
    }

    if !current_cells.is_empty() {
        rows.push(current_cells);
    }

    if rows.is_empty() {
        return None;
    }

    let headers = rows.remove(0);
    let col_count = headers.len().max(1);
    let mut alignments = vec![crate::table::TableAlign::Left; col_count];
    for (c, align) in alignments.iter_mut().enumerate().take(col_count) {
        let is_numeric = !rows.is_empty()
            && rows.iter().all(|r| {
                if let Some(val) = r.get(c) {
                    crate::table::is_numeric_cell(val)
                } else {
                    true
                }
            });
        if is_numeric {
            *align = crate::table::TableAlign::Right;
        }
    }

    // Normalize row lengths
    for r in &mut rows {
        while r.len() < col_count {
            r.push(String::new());
        }
    }

    Some(crate::table::TableData {
        headers,
        rows,
        alignments,
        raw_source: lines.join("\n"),
    })
}

/// Cleans inline formatting from AsciiDoc text (bold, italic, monospace, links).
pub fn clean_asciidoc_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Link: url[label] or link:url[label] -> label
        if chars[i] == '['
            && let Some(close_bracket) = chars[i..].iter().position(|&c| c == ']')
        {
            let bracket_end = i + close_bracket;
            let label: String = chars[i + 1..bracket_end].iter().collect();
            if !label.is_empty() {
                if let Some(last_space) = out.rfind(' ') {
                    let prev_word = &out[last_space + 1..];
                    if prev_word.starts_with("http://")
                        || prev_word.starts_with("https://")
                        || prev_word.starts_with("link:")
                    {
                        out.truncate(last_space + 1);
                        out.push_str(&label);
                        i = bracket_end + 1;
                        continue;
                    }
                } else if out.starts_with("http://")
                    || out.starts_with("https://")
                    || out.starts_with("link:")
                {
                    out.clear();
                    out.push_str(&label);
                    i = bracket_end + 1;
                    continue;
                }
            }
        }

        // Bold: *word*
        // Italic: _word_
        // Monospace: `code`
        if (chars[i] == '*' || chars[i] == '_' || chars[i] == '`')
            && (i == 0
                || chars[i - 1].is_whitespace()
                || i + 1 == chars.len()
                || chars[i + 1].is_whitespace()
                || ",.!?;:)\"".contains(chars[i + 1]))
        {
            i += 1;
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare block delimiter (====, ****, ____) used to make the outer parse
    /// loop spin forever: the heading dispatch rejected it (no space follows
    /// the marker run) while the paragraph collector's looser check consumed
    /// zero lines before bailing, so `i` never advanced. Each variant here is
    /// a real-world admonition/sidebar/quote block opener.
    #[test]
    fn bare_delimiter_lines_do_not_stall_the_parser() {
        for marker in ["====", "****", "____", "=====", "----!", "== "] {
            let blocks = parse_asciidoc_blocks(marker).expect("must not hang or error");
            assert!(
                !blocks.is_empty(),
                "{marker:?} should still render as content"
            );
        }
    }

    #[test]
    fn malformed_heading_like_lines_fall_back_to_paragraph_text() {
        // No space after the '=' run, or more than 6 '=': not a valid heading.
        let blocks = parse_asciidoc_blocks("=====no-space-heading").unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], HtmlBlock::Paragraph { .. }));
    }

    #[test]
    fn block_style_admonition_gets_the_same_prefix_as_inline_form() {
        let doc = "[CAUTION]\n====\nMind the gap.\n\nSeriously.\n====\n";
        let blocks = parse_asciidoc_blocks(doc).unwrap();
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                HtmlBlock::Paragraph { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["[CAUTION] Mind the gap.", "Seriously."]);
    }

    #[test]
    fn plain_example_block_without_attribute_line_renders_as_paragraph() {
        let blocks = parse_asciidoc_blocks("====\nJust an example.\n====\n").unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], HtmlBlock::Paragraph { text } if text == "Just an example."));
    }

    #[test]
    fn valid_headings_still_parse_at_every_level() {
        let blocks = parse_asciidoc_blocks("= Title\n== Section\n====== Deep\n").unwrap();
        let levels: Vec<u8> = blocks
            .iter()
            .filter_map(|b| match b {
                HtmlBlock::Heading { level, .. } => Some(*level),
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec![1, 2, 6]);
    }

    #[test]
    fn block_image_macro_resolves_imagesdir_and_attributes() {
        let mut images = HashMap::new();
        images.insert(
            "assets/diagram.png".to_owned(),
            InlineHtmlImage {
                href: "data:image/png;base64,AAAA".into(),
                pixel_width: 2,
                pixel_height: 1,
            },
        );
        let (blocks, warnings) = parse_asciidoc_blocks_inner(
            ":imagesdir: assets\n\nimage::diagram.png[System diagram,200,100]\n",
            &images,
            Some("assets"),
        )
        .unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("width, height"))
        );
        assert!(matches!(
            blocks.as_slice(),
            [HtmlBlock::Image {
                href,
                pixel_width: 200,
                pixel_height: 100,
                alt
            }] if href.starts_with("data:image/png") && alt == "System diagram"
        ));
    }

    #[test]
    fn block_image_macro_parser_uses_filename_alt_and_rejects_malformed_lines() {
        let macro_line = parse_asciidoc_image_macro("image::build/api-diagram.png[]").unwrap();
        assert_eq!(macro_line.target, "build/api-diagram.png");
        assert_eq!(macro_line.alt, "api diagram");
        assert!(parse_asciidoc_image_macro("image::[missing-target]").is_none());
        assert!(parse_asciidoc_image_macro("image::remote.png[alt").is_none());
    }
}
