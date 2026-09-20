//! Markdown document parser, typography typesetter, and paginated vector SVG renderer.
//!
//! Parses CommonMark / GFM markdown constructs (ATX headings, bullet/numbered lists,
//! fenced code blocks, blockquotes, horizontal rules, tables, and paragraphs),
//! layouting content into flowing multi-page vector SVG documents.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{
    HtmlBlock, InlineHtmlImage, load_local_image_sources, render_blocks_to_pages,
};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_MARKDOWN_IMAGE_REFERENCES: usize = 10_000;
const MAX_MARKDOWN_LINES: usize = 1_000_000;
const MAX_MARKDOWN_LINE_BYTES: usize = 1024 * 1024;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "Markdown input")?;
    let text = String::from_utf8(bytes)
        .map_err(|e| Error::InvalidInput(format!("Markdown file is not valid UTF-8: {e}")))?;
    validate_markdown_lines(&text)?;
    let text = expand_markdown_reference_images(&text, options.max_input_bytes)?;

    // If the file is exclusively a single standalone markdown table,
    // preserve dedicated single-table layout and embedded source for exact roundtripping.
    let non_empty: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let is_pure_table = !non_empty.is_empty()
        && non_empty
            .iter()
            .all(|l| l.starts_with('|') && l.contains('|'));

    if is_pure_table && let Ok(table) = crate::table::parse_markdown_table(&text) {
        let page = crate::table::layout_and_render_table(&table, options)?;
        sink.consume(page)?;
        return Ok(Vec::new());
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_dir = fs::canonicalize(parent)?;
    let (image_sources, too_many_images) = collect_markdown_image_sources(&text);
    let (inline_images, mut warnings) =
        load_local_image_sources(&base_dir, image_sources, too_many_images)?;
    if !inline_images.is_empty() {
        push_markdown_warning_once(
            &mut warnings,
            "Markdown images are rendered as centered flow blocks; inline styling and wrapping are approximated",
        );
    }
    let (blocks, parser_warnings) = parse_markdown_blocks_inner(&text, &inline_images)?;
    for warning in parser_warnings {
        push_markdown_warning_once(&mut warnings, &warning);
    }
    render_blocks_to_pages(&blocks, sink, options)?;
    Ok(warnings)
}

/// Parses Markdown text into a sequence of renderable [`HtmlBlock`] elements.
pub fn parse_markdown_blocks(text: &str) -> Result<Vec<HtmlBlock>> {
    let expanded = expand_markdown_reference_images(text, u64::MAX)?;
    parse_markdown_blocks_with_images(&expanded, &HashMap::new()).map(|(blocks, _)| blocks)
}

pub(crate) fn parse_markdown_blocks_with_images(
    text: &str,
    inline_images: &HashMap<String, InlineHtmlImage>,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    validate_markdown_lines(text)?;
    parse_markdown_blocks_inner(text, inline_images)
}

fn parse_markdown_blocks_inner(
    text: &str,
    inline_images: &HashMap<String, InlineHtmlImage>,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut image_count = 0usize;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        if let Some((alt, source)) = parse_markdown_image(trimmed) {
            image_count = image_count.saturating_add(1);
            if image_count > MAX_MARKDOWN_IMAGE_REFERENCES {
                push_markdown_warning_once(
                    &mut warnings,
                    "Markdown image blocks exceeded the supported limit; remaining images were omitted",
                );
            } else if let Some(image) = inline_images.get(&source) {
                blocks.push(HtmlBlock::Image {
                    href: image.href.clone(),
                    pixel_width: image.pixel_width,
                    pixel_height: image.pixel_height,
                    alt: clean_markdown_inline(&alt),
                });
            } else {
                push_markdown_warning_once(
                    &mut warnings,
                    "Markdown image source was omitted because it was not a validated local PNG/JPEG resource",
                );
                if !alt.is_empty() {
                    blocks.push(HtmlBlock::Paragraph {
                        text: format!("[Image omitted: {}]", clean_markdown_inline(&alt)),
                    });
                }
            }
            i += 1;
            continue;
        }

        // 1. Fenced code block (``` or ~~~)
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let fence = if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            };
            i += 1;
            let mut code_lines = Vec::new();
            while i < lines.len() {
                let code_line = lines[i];
                if code_line.trim().starts_with(fence) {
                    i += 1;
                    break;
                }
                code_lines.push(code_line);
                i += 1;
            }
            blocks.push(HtmlBlock::CodeBlock {
                text: code_lines.join("\n"),
            });
            continue;
        }

        // 2. ATX Headings (# Heading)
        if trimmed.starts_with('#') {
            let hash_count = trimmed.chars().take_while(|&c| c == '#').count();
            if hash_count <= 6 {
                let rest = trimmed[hash_count..].trim();
                if !rest.is_empty() || trimmed.chars().nth(hash_count) == Some(' ') {
                    blocks.push(HtmlBlock::Heading {
                        level: hash_count as u8,
                        text: clean_markdown_inline(rest),
                    });
                    i += 1;
                    continue;
                }
            }
        }

        // 3. Horizontal Rule (---, ***, ___)
        if (trimmed.starts_with("---") && trimmed.chars().all(|c| c == '-' || c == ' '))
            || (trimmed.starts_with("***") && trimmed.chars().all(|c| c == '*' || c == ' '))
            || (trimmed.starts_with("___") && trimmed.chars().all(|c| c == '_' || c == ' '))
        {
            blocks.push(HtmlBlock::HorizontalRule);
            i += 1;
            continue;
        }

        // 4. Markdown Table (| Col 1 | Col 2 |)
        if trimmed.starts_with('|') && trimmed.contains('|') {
            let mut table_lines = Vec::new();
            while i < lines.len() {
                let tl = lines[i].trim();
                if tl.starts_with('|') && tl.contains('|') {
                    table_lines.push(tl);
                    i += 1;
                } else {
                    break;
                }
            }
            if let Ok(table_data) = parse_embedded_table(&table_lines) {
                blocks.push(HtmlBlock::Table(table_data));
            }
            continue;
        }

        // 5. Blockquote (> quote)
        if trimmed.starts_with('>') {
            let mut quote_lines = Vec::new();
            while i < lines.len() {
                let ql = lines[i].trim();
                if let Some(stripped) = ql.strip_prefix('>') {
                    quote_lines.push(stripped.trim());
                    i += 1;
                } else if !ql.is_empty() && !quote_lines.is_empty() {
                    // Lazy continuation line
                    quote_lines.push(ql);
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(HtmlBlock::Paragraph {
                text: clean_markdown_inline(&quote_lines.join(" ")),
            });
            continue;
        }

        // 6. Unordered, Task, or Ordered List Item (- item, * item, - [x] task)
        let indent_spaces = line.chars().take_while(|&c| c == ' ' || c == '\t').count();
        let depth = (indent_spaces / 2).min(4);
        let indent_prefix = "  ".repeat(depth);

        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            let (bullet, text) = if let Some(t) = rest
                .strip_prefix("[x] ")
                .or_else(|| rest.strip_prefix("[X] "))
            {
                (format!("{indent_prefix}☑ "), t)
            } else if let Some(t) = rest.strip_prefix("[ ] ") {
                (format!("{indent_prefix}☐ "), t)
            } else {
                (format!("{indent_prefix}• "), rest)
            };
            blocks.push(HtmlBlock::ListItem {
                bullet,
                text: clean_markdown_inline(text),
            });
            i += 1;
            continue;
        }

        if let Some(dot_pos) = trimmed.find(". ") {
            let prefix = &trimmed[..dot_pos];
            if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() {
                let num = prefix;
                let text = trimmed[dot_pos + 2..].trim();
                blocks.push(HtmlBlock::ListItem {
                    bullet: format!("{indent_prefix}{num}. "),
                    text: clean_markdown_inline(text),
                });
                i += 1;
                continue;
            }
        }

        // 7. Definition list definition line (: Definition description)
        if let Some(def_text) = trimmed.strip_prefix(": ") {
            blocks.push(HtmlBlock::ListItem {
                bullet: format!("{indent_prefix}  • "),
                text: clean_markdown_inline(def_text),
            });
            i += 1;
            continue;
        }

        // 8. Footnote definition ([^label]: Footnote text)
        if trimmed.starts_with("[^")
            && let Some(colon_pos) = trimmed.find("]:")
        {
            let label = &trimmed[2..colon_pos];
            let note_body = trimmed[colon_pos + 2..].trim();
            blocks.push(HtmlBlock::ListItem {
                bullet: format!("[{label}] "),
                text: clean_markdown_inline(note_body),
            });
            i += 1;
            continue;
        }

        // 9. Regular Paragraph or Setext Heading
        let mut para_lines = Vec::new();
        let mut is_setext = false;
        let mut setext_level = 0u8;

        while i < lines.len() {
            let pl = lines[i].trim();

            // Check if current line is a Setext underline for preceding paragraph lines
            if !para_lines.is_empty() {
                if pl.chars().all(|c| c == '=') && pl.len() >= 2 {
                    is_setext = true;
                    setext_level = 1;
                    i += 1;
                    break;
                } else if pl.chars().all(|c| c == '-') && pl.len() >= 2 {
                    is_setext = true;
                    setext_level = 2;
                    i += 1;
                    break;
                }
            }

            if pl.is_empty()
                || pl.starts_with('#')
                || pl.starts_with("```")
                || pl.starts_with("~~~")
                || pl.starts_with('>')
                || pl.starts_with("- ")
                || pl.starts_with("* ")
                || pl.starts_with("+ ")
                || pl.starts_with(": ")
                || (pl.starts_with("[^") && pl.contains("]:"))
                || (pl.starts_with('|') && pl.contains('|'))
            {
                break;
            }
            para_lines.push(pl);
            i += 1;
        }

        if is_setext {
            blocks.push(HtmlBlock::Heading {
                level: setext_level,
                text: clean_markdown_inline(&para_lines.join(" ")),
            });
        } else if !para_lines.is_empty() {
            push_markdown_paragraph_with_images(
                &para_lines.join(" "),
                inline_images,
                &mut warnings,
                &mut blocks,
                &mut image_count,
            );
        }
    }

    if blocks.is_empty() && !text.trim().is_empty() {
        blocks.push(HtmlBlock::Paragraph {
            text: clean_markdown_inline(text.trim()),
        });
    }

    Ok((blocks, warnings))
}

#[derive(Clone, Debug)]
struct MarkdownImageToken {
    start: usize,
    end: usize,
    alt: String,
    source: String,
}

pub(crate) fn collect_markdown_image_sources(text: &str) -> (Vec<String>, bool) {
    let mut sources = Vec::new();
    let mut exceeded = false;
    let lines = text.lines().collect::<Vec<_>>();
    let mut fenced: Option<u8> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let marker = trimmed.as_bytes().first().copied();
        if let Some(fence_char) = fenced {
            if marker == Some(fence_char)
                && trimmed
                    .as_bytes()
                    .iter()
                    .take_while(|byte| **byte == fence_char)
                    .count()
                    >= 3
            {
                fenced = None;
            }
            continue;
        }
        if let Some(fence_char @ (b'`' | b'~')) = marker
            && trimmed
                .as_bytes()
                .iter()
                .take_while(|byte| **byte == fence_char)
                .count()
                >= 3
        {
            fenced = Some(fence_char);
            continue;
        }
        if markdown_images_are_text_only(line)
            || lines
                .get(index + 1)
                .is_some_and(|next| is_setext_underline(next.trim()))
        {
            continue;
        }
        for token in markdown_image_tokens(line) {
            if sources.len() < MAX_MARKDOWN_IMAGE_REFERENCES {
                sources.push(token.source);
            } else {
                exceeded = true;
            }
        }
    }
    (sources, exceeded)
}

pub(crate) fn expand_markdown_reference_images(text: &str, max_bytes: u64) -> Result<String> {
    let definitions = markdown_reference_definitions(text);
    if definitions.is_empty() {
        return Ok(text.to_owned());
    }
    let mut output = String::with_capacity(text.len());
    let mut fenced = None::<u8>;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let marker = trimmed.as_bytes().first().copied();
        if let Some(fence) = fenced {
            if marker == Some(fence)
                && trimmed
                    .as_bytes()
                    .iter()
                    .take_while(|byte| **byte == fence)
                    .count()
                    >= 3
            {
                fenced = None;
            }
            output.push_str(line);
        } else if let Some(fence @ (b'`' | b'~')) = marker
            && trimmed
                .as_bytes()
                .iter()
                .take_while(|byte| **byte == fence)
                .count()
                >= 3
        {
            fenced = Some(fence);
            output.push_str(line);
        } else if parse_markdown_reference_definition(trimmed).is_some() {
            // Definitions are metadata, not visible paragraph text.
        } else {
            output.push_str(&replace_markdown_reference_images(line, &definitions));
        }
        output.push('\n');
        if output.len() as u64 > max_bytes {
            return Err(Error::LimitExceeded(format!(
                "expanded Markdown reference images exceed maximum bytes ({max_bytes})"
            )));
        }
    }
    if !text.ends_with(['\n', '\r']) {
        output.pop();
    }
    validate_markdown_lines(&output)?;
    Ok(output)
}

fn markdown_reference_definitions(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(parse_markdown_reference_definition)
        .collect()
}

fn parse_markdown_reference_definition(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let close = line.find("]:")?;
    if !line.starts_with("[") || close <= 1 || line.as_bytes().get(close + 2) == Some(&b'[') {
        return None;
    }
    let label = normalize_markdown_reference_label(&line[1..close]);
    if label.starts_with('^') {
        return None;
    }
    let mut destination = line[close + 2..].trim();
    if destination.is_empty() {
        return None;
    }
    if let Some(value) = destination.strip_prefix('<') {
        let end = value.find('>')?;
        destination = &value[..end];
    } else {
        destination = destination.split_whitespace().next()?;
    }
    (!label.is_empty() && !destination.is_empty()).then(|| (label, destination.to_owned()))
}

fn normalize_markdown_reference_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn replace_markdown_reference_images(line: &str, definitions: &HashMap<String, String>) -> String {
    let mut output = String::with_capacity(line.len());
    let mut cursor = 0usize;
    while let Some(relative_start) = line.get(cursor..).and_then(|rest| rest.find("![")) {
        let start = cursor + relative_start;
        output.push_str(&line[cursor..start]);
        if is_escaped_markdown_marker(line, start) || is_inside_markdown_code_span(line, start) {
            output.push_str("![");
            cursor = start + 2;
            continue;
        }
        let Some(alt_end_relative) = line.get(start + 2..).and_then(|rest| rest.find(']')) else {
            output.push_str(&line[start..]);
            break;
        };
        let alt_end = start + 2 + alt_end_relative;
        let alt = &line[start + 2..alt_end];
        let mut reference_end = alt_end + 1;
        if line.as_bytes().get(reference_end) == Some(&b'(') {
            output.push_str(&line[start..reference_end]);
            cursor = reference_end;
            continue;
        }
        let label = if line.as_bytes().get(reference_end) == Some(&b'[') {
            let Some(close_relative) = line
                .get(reference_end + 1..)
                .and_then(|rest| rest.find(']'))
            else {
                output.push_str(&line[start..]);
                break;
            };
            let close = reference_end + 1 + close_relative;
            let label = &line[reference_end + 1..close];
            reference_end = close + 1;
            if label.is_empty() { alt } else { label }
        } else {
            alt
        };
        let key = normalize_markdown_reference_label(label);
        if let Some(destination) = definitions.get(&key) {
            output.push_str("![");
            output.push_str(alt);
            output.push_str("](<");
            output.push_str(destination);
            output.push_str(">)");
            cursor = reference_end;
        } else {
            output.push_str(&line[start..reference_end]);
            cursor = reference_end;
        }
    }
    if cursor < line.len() {
        output.push_str(&line[cursor..]);
    }
    output
}

fn validate_markdown_lines(text: &str) -> Result<()> {
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_MARKDOWN_LINES {
            return Err(Error::LimitExceeded(format!(
                "Markdown input exceeds {MAX_MARKDOWN_LINES} lines"
            )));
        }
        if line.len() > MAX_MARKDOWN_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Markdown line {} exceeds {MAX_MARKDOWN_LINE_BYTES} bytes",
                index + 1
            )));
        }
    }
    Ok(())
}

fn parse_markdown_image(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let tokens = markdown_image_tokens(line);
    let token = tokens.first()?;
    (tokens.len() == 1 && token.start == 0 && token.end == line.len())
        .then(|| (token.alt.clone(), token.source.clone()))
}

fn markdown_image_tokens(text: &str) -> Vec<MarkdownImageToken> {
    let mut tokens = Vec::new();
    let mut cursor = 0usize;
    while let Some(relative_start) = text.get(cursor..).and_then(|rest| rest.find("![")) {
        let start = cursor + relative_start;
        if is_escaped_markdown_marker(text, start) || is_inside_markdown_code_span(text, start) {
            cursor = start + 2;
            continue;
        }
        let Some(relative_alt_end) = text.get(start + 2..).and_then(|rest| rest.find("](")) else {
            break;
        };
        let alt_end = start + 2 + relative_alt_end;
        let destination_start = alt_end + 2;
        let Some(relative_end) = text
            .get(destination_start..)
            .and_then(|rest| rest.find(')'))
        else {
            break;
        };
        let close = destination_start + relative_end;
        let raw_destination = text[destination_start..close].trim();
        let destination = if let Some(bracketed) = raw_destination.strip_prefix('<') {
            let Some(close_angle) = bracketed.find('>') else {
                cursor = start + 2;
                continue;
            };
            bracketed[..close_angle].trim()
        } else {
            raw_destination
                .split_whitespace()
                .next()
                .unwrap_or_default()
        };
        if destination.is_empty() || destination.contains(['(', ')']) {
            cursor = start + 2;
            continue;
        }
        tokens.push(MarkdownImageToken {
            start,
            end: close + 1,
            alt: text[start + 2..alt_end].to_owned(),
            source: destination.to_owned(),
        });
        if tokens.len() > MAX_MARKDOWN_IMAGE_REFERENCES {
            break;
        }
        cursor = close + 1;
    }
    tokens
}

fn is_escaped_markdown_marker(text: &str, index: usize) -> bool {
    let slashes = text[..index]
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count();
    slashes % 2 == 1
}

fn is_inside_markdown_code_span(text: &str, index: usize) -> bool {
    let bytes = text.as_bytes();
    let mut open_length = None;
    let mut cursor = 0usize;
    while cursor < index {
        if bytes[cursor] != b'`' {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < index && bytes[cursor] == b'`' {
            cursor += 1;
        }
        let length = cursor - start;
        if open_length == Some(length) {
            open_length = None;
        } else if open_length.is_none() {
            open_length = Some(length);
        }
    }
    open_length.is_some()
}

fn markdown_images_are_text_only(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#')
        || trimmed.starts_with('>')
        || trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("+ ")
        || trimmed.starts_with(": ")
        || trimmed.starts_with('|')
        || (trimmed.starts_with("[^") && trimmed.contains("]:"))
        || trimmed
            .find(". ")
            .is_some_and(|dot| dot > 0 && trimmed[..dot].bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_setext_underline(line: &str) -> bool {
    line.len() >= 2
        && (line.chars().all(|character| character == '=')
            || line.chars().all(|character| character == '-'))
}

fn push_markdown_paragraph_with_images(
    text: &str,
    inline_images: &HashMap<String, InlineHtmlImage>,
    warnings: &mut Vec<String>,
    blocks: &mut Vec<HtmlBlock>,
    image_count: &mut usize,
) {
    let mut cursor = 0usize;
    for token in markdown_image_tokens(text) {
        if token.start > cursor {
            push_markdown_paragraph_fragment(&text[cursor..token.start], blocks);
        }
        *image_count = image_count.saturating_add(1);
        if *image_count > MAX_MARKDOWN_IMAGE_REFERENCES {
            push_markdown_warning_once(
                warnings,
                "Markdown image references exceeded the supported limit; remaining images were omitted",
            );
        } else if let Some(image) = inline_images.get(&token.source) {
            blocks.push(HtmlBlock::Image {
                href: image.href.clone(),
                pixel_width: image.pixel_width,
                pixel_height: image.pixel_height,
                alt: clean_markdown_inline(&token.alt),
            });
        } else {
            push_markdown_warning_once(
                warnings,
                "Markdown image source was omitted because it was not a validated local PNG/JPEG resource",
            );
            if !token.alt.is_empty() {
                blocks.push(HtmlBlock::Paragraph {
                    text: format!("[Image omitted: {}]", clean_markdown_inline(&token.alt)),
                });
            }
        }
        cursor = token.end;
    }
    if cursor < text.len() {
        push_markdown_paragraph_fragment(&text[cursor..], blocks);
    }
}

fn push_markdown_paragraph_fragment(text: &str, blocks: &mut Vec<HtmlBlock>) {
    let text = clean_markdown_inline(text);
    if !text.trim().is_empty() {
        blocks.push(HtmlBlock::Paragraph { text });
    }
}

fn push_markdown_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn parse_embedded_table(lines: &[&str]) -> Result<TableData> {
    if lines.is_empty() {
        return Err(Error::InvalidInput("empty table lines".into()));
    }

    let headers = parse_pipe_row(lines[0]);
    let mut alignments = vec![TableAlign::Left; headers.len()];
    let mut rows = Vec::new();
    let mut start_row = 1;

    if lines.len() > 1 {
        let sep_cells = parse_pipe_row(lines[1]);
        let is_sep = !sep_cells.is_empty()
            && sep_cells.iter().all(|c| {
                let t = c.trim();
                !t.is_empty() && t.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')
            });

        if is_sep {
            alignments.clear();
            for cell in &sep_cells {
                let t = cell.trim();
                let align = if t.starts_with(':') && t.ends_with(':') {
                    TableAlign::Center
                } else if t.ends_with(':') {
                    TableAlign::Right
                } else {
                    TableAlign::Left
                };
                alignments.push(align);
            }
            while alignments.len() < headers.len() {
                alignments.push(TableAlign::Left);
            }
            start_row = 2;
        }
    }

    for line in &lines[start_row..] {
        let row = parse_pipe_row(line);
        if !row.is_empty() {
            rows.push(row);
        }
    }

    Ok(TableData {
        headers,
        rows,
        alignments,
        raw_source: lines.join("\n"),
    })
}

fn parse_pipe_row(line: &str) -> Vec<String> {
    crate::table::split_markdown_row(line)
        .into_iter()
        .map(|cell| clean_markdown_inline(cell.trim()))
        .collect()
}

pub fn clean_markdown_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Escaped characters: \* \_ \[ \] \( \) \# \| \~ \` \\ -> literal character
        if chars[i] == '\\' && i + 1 < chars.len() && "*_[]()#|~`\\".contains(chars[i + 1]) {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }

        // Image ![alt](url) -> alt
        if chars[i] == '!'
            && i + 1 < chars.len()
            && chars[i + 1] == '['
            && let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']')
        {
            let bracket_end = i + 1 + close_bracket;
            if bracket_end + 1 < chars.len()
                && chars[bracket_end + 1] == '('
                && let Some(close_paren) = chars[bracket_end + 1..].iter().position(|&c| c == ')')
            {
                let paren_end = bracket_end + 1 + close_paren;
                let label: String = chars[i + 2..bracket_end].iter().collect();
                out.push_str(&clean_markdown_inline(&label));
                i = paren_end + 1;
                continue;
            }
        }

        // Footnote reference [^1] or [^label] -> [1] or [label]
        if chars[i] == '['
            && i + 1 < chars.len()
            && chars[i + 1] == '^'
            && let Some(close_bracket) = chars[i..].iter().position(|&c| c == ']')
        {
            let bracket_end = i + close_bracket;
            let note_label: String = chars[i + 2..bracket_end].iter().collect();
            out.push_str(&format!("[{note_label}]"));
            i = bracket_end + 1;
            continue;
        }

        // Link [label](url) -> label
        if chars[i] == '['
            && let Some(close_bracket) = chars[i..].iter().position(|&c| c == ']')
        {
            let bracket_end = i + close_bracket;
            if bracket_end + 1 < chars.len()
                && chars[bracket_end + 1] == '('
                && let Some(close_paren) = chars[bracket_end + 1..].iter().position(|&c| c == ')')
            {
                let paren_end = bracket_end + 1 + close_paren;
                let label: String = chars[i + 1..bracket_end].iter().collect();
                out.push_str(&clean_markdown_inline(&label));
                i = paren_end + 1;
                continue;
            }
        }

        // HTML tags or autolinks <...>
        if chars[i] == '<'
            && let Some(close_gt) = chars[i..].iter().position(|&c| c == '>')
        {
            let tag_content: String = chars[i + 1..i + close_gt].iter().collect();
            let tag_lower = tag_content.to_ascii_lowercase();
            let tag_trimmed = tag_lower.trim();
            if tag_trimmed == "br" || tag_trimmed == "br/" || tag_trimmed == "br /" {
                out.push('\n');
                i += close_gt + 1;
                continue;
            } else if tag_trimmed.starts_with("http://")
                || tag_trimmed.starts_with("https://")
                || tag_trimmed.starts_with("mailto:")
            {
                out.push_str(&tag_content);
                i += close_gt + 1;
                continue;
            } else if tag_trimmed.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || c == '/'
                    || c == ' '
                    || c == '-'
                    || c == '_'
                    || c == '"'
                    || c == '='
                    || c == ':'
            }) {
                // Strip HTML tag
                i += close_gt + 1;
                continue;
            }
        }

        // Common HTML entities: &amp;, &lt;, &gt;, &quot;, &#39;, &nbsp;, &copy;, &mdash;, &ndash;
        if chars[i] == '&'
            && let Some(semi) = chars[i..].iter().position(|&c| c == ';')
            && semi <= 8
        {
            let entity: String = chars[i + 1..i + semi].iter().collect();
            let decoded = match entity.as_str() {
                "amp" => Some("&"),
                "lt" => Some("<"),
                "gt" => Some(">"),
                "quot" => Some("\""),
                "apos" | "#39" => Some("'"),
                "nbsp" => Some(" "),
                "copy" => Some("©"),
                "mdash" => Some("—"),
                "ndash" => Some("–"),
                _ => None,
            };
            if let Some(dec) = decoded {
                out.push_str(dec);
                i += semi + 1;
                continue;
            }
        }

        // Bold: ** or __
        if (chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '*')
            || (chars[i] == '_' && i + 1 < chars.len() && chars[i + 1] == '_')
        {
            i += 2;
            continue;
        }

        // Strikethrough: ~~
        if chars[i] == '~' && i + 1 < chars.len() && chars[i + 1] == '~' {
            i += 2;
            continue;
        }

        // Inline code: `
        if chars[i] == '`' {
            i += 1;
            continue;
        }

        // Single italic delimiter: * or _ (when flanking words)
        if (chars[i] == '*' || chars[i] == '_')
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

    #[test]
    fn image_tokens_skip_escaped_markers_and_code_spans() {
        let tokens = markdown_image_tokens(
            r#"Escaped \![no](ignored.png), code `![no](ignored.png)`, real ![chart](assets/chart.png)."#,
        );
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].alt, "chart");
        assert_eq!(tokens[0].source, "assets/chart.png");

        let titled = parse_markdown_image("![photo](<assets/photo.png> \"title\")").unwrap();
        assert_eq!(titled.1, "assets/photo.png");
    }

    #[test]
    fn expands_reference_style_images_without_touching_inline_or_fenced_code() {
        let markdown = "![Chart][diagram]\n\n[diagram]: assets/chart.png\n\n```\n![code][diagram]\n```\n\n![direct](assets/direct.png)";
        let expanded = expand_markdown_reference_images(markdown, u64::MAX).unwrap();
        assert!(expanded.contains("![Chart](<assets/chart.png>)"));
        assert!(expanded.contains("![direct](assets/direct.png)"));
        assert!(expanded.contains("![code][diagram]"));
        assert!(!expanded.contains("[diagram]:"));
    }

    #[test]
    fn markdown_line_byte_and_count_limits_are_enforced() {
        assert!(matches!(
            validate_markdown_lines(&"x".repeat(MAX_MARKDOWN_LINE_BYTES + 1)),
            Err(Error::LimitExceeded(_))
        ));
        let many_lines = "\n".repeat(MAX_MARKDOWN_LINES + 1);
        assert!(matches!(
            validate_markdown_lines(&many_lines),
            Err(Error::LimitExceeded(_))
        ));
    }
}
