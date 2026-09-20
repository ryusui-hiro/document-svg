//! Bounded reStructuredText (`.rst`, `.rest`) document preview.
//!
//! Supports section adornments, paragraphs, lists, simple/grid tables, literal
//! and code blocks, common admonitions, and safe local PNG/JPEG image directives.
//! Directives that insert files, emit raw markup, or otherwise require execution
//! are never evaluated.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{
    HtmlBlock, InlineHtmlImage, load_local_image_sources, render_blocks_to_pages,
};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_RST_LINES: usize = 200_000;
const MAX_RST_LINE_BYTES: usize = 1024 * 1024;
const MAX_RST_IMAGE_REFERENCES: usize = 10_000;

pub(crate) fn looks_like_rst_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let lines = text
        .lines()
        .take(8)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.first().is_some_and(|line| directive(line).is_some()) {
        return true;
    }
    lines.windows(2).any(|pair| {
        is_adornment(pair[1])
            && pair[1].len() >= pair[0].len()
            && !pair[0].starts_with('#')
            && !pair[0].starts_with('=')
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "reStructuredText input")?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("reStructuredText is not valid UTF-8: {error}"))
    })?;
    validate_rst_lines(&text)?;
    let (image_sources, too_many_images) = collect_image_sources(&text);
    let mut warnings = Vec::new();
    let images = if image_sources.is_empty() && !too_many_images {
        HashMap::new()
    } else {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let base_dir = fs::canonicalize(parent)?;
        let (images, image_warnings) =
            load_local_image_sources(&base_dir, image_sources, too_many_images)?;
        warnings.extend(image_warnings);
        if !images.is_empty() {
            push_warning(
                &mut warnings,
                "reStructuredText images are embedded as centered flow blocks; figure alignment and scaling are approximated",
            );
        }
        images
    };
    let (blocks, parser_warnings) = parse_rst_blocks_with_images(&text, &images)?;
    for warning in parser_warnings {
        push_warning(&mut warnings, &warning);
    }
    render_blocks_to_pages(&blocks, sink, options)?;
    Ok(warnings)
}

/// Parses a bounded subset of reStructuredText into renderable blocks.
pub fn parse_rst_blocks(text: &str) -> Result<Vec<HtmlBlock>> {
    parse_rst_blocks_with_images(text, &HashMap::new()).map(|(blocks, _)| blocks)
}

fn validate_rst_lines(text: &str) -> Result<()> {
    if text.lines().count() > MAX_RST_LINES {
        return Err(Error::LimitExceeded(format!(
            "reStructuredText exceeds {MAX_RST_LINES} lines"
        )));
    }
    if text.lines().any(|line| line.len() > MAX_RST_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "reStructuredText line exceeds {MAX_RST_LINE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn collect_image_sources(text: &str) -> (Vec<String>, bool) {
    let mut sources = Vec::new();
    let mut too_many = false;
    for line in text.lines() {
        let trimmed = line.trim();
        let Some((kind, argument)) = directive(trimmed) else {
            continue;
        };
        if matches!(kind, "image" | "figure") && !argument.is_empty() {
            if sources.len() >= MAX_RST_IMAGE_REFERENCES {
                too_many = true;
            } else {
                sources.push(argument.to_owned());
            }
        }
    }
    (sources, too_many)
}

fn parse_rst_blocks_with_images(
    text: &str,
    images: &HashMap<String, InlineHtmlImage>,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    validate_rst_lines(text)?;
    let lines = text.lines().collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut heading_styles = Vec::<char>::new();
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        if let Some((table, next)) = parse_grid_table(&lines, i) {
            blocks.push(HtmlBlock::Table(table));
            i = next;
            continue;
        }
        if let Some((table, next)) = parse_simple_table(&lines, i) {
            blocks.push(HtmlBlock::Table(table));
            i = next;
            continue;
        }

        // Overline-title-underline sections.
        if is_adornment(trimmed)
            && let Some(title) = lines.get(i + 1).map(|value| value.trim())
            && let Some(underline) = lines.get(i + 2).map(|value| value.trim())
            && same_adornment(trimmed, underline)
            && title.len() <= trimmed.len()
        {
            let mark = trimmed.chars().next().unwrap_or('=');
            let level = heading_level(mark, &mut heading_styles);
            blocks.push(HtmlBlock::Heading {
                level,
                text: clean_rst_inline(title),
            });
            i += 3;
            continue;
        }
        // Underline-only section titles.
        if let Some(underline) = lines.get(i + 1).map(|value| value.trim())
            && is_adornment(underline)
            && same_adornment(trimmed, underline)
        {
            let mark = underline.chars().next().unwrap_or('=');
            let level = heading_level(mark, &mut heading_styles);
            blocks.push(HtmlBlock::Heading {
                level,
                text: clean_rst_inline(trimmed),
            });
            i += 2;
            continue;
        }

        if let Some((kind, argument)) = directive(trimmed) {
            i += 1;
            let (body, next) = collect_indented_block(&lines, i);
            i = next;
            match kind {
                "image" | "figure" => {
                    let alt = directive_option(&body, "alt").unwrap_or_default();
                    if let Some(image) = images.get(argument) {
                        blocks.push(HtmlBlock::Image {
                            href: image.href.clone(),
                            pixel_width: image.pixel_width,
                            pixel_height: image.pixel_height,
                            alt: clean_rst_inline(alt),
                        });
                    } else {
                        push_warning(
                            &mut warnings,
                            "reStructuredText image directive was omitted because it was not a validated local PNG/JPEG",
                        );
                        if !alt.is_empty() {
                            blocks.push(HtmlBlock::Paragraph {
                                text: format!("[Image omitted: {}]", clean_rst_inline(alt)),
                            });
                        }
                    }
                    if kind == "figure" {
                        let caption = strip_directive_options(&body).join(" ");
                        if !caption.trim().is_empty() {
                            blocks.push(HtmlBlock::Paragraph {
                                text: clean_rst_inline(caption.trim()),
                            });
                        }
                    }
                }
                "code" | "code-block" | "sourcecode" => {
                    blocks.push(HtmlBlock::CodeBlock {
                        text: strip_directive_options(&body).join("\n"),
                    });
                }
                "note" | "tip" | "hint" | "important" | "warning" | "caution" | "attention"
                | "danger" | "error" => {
                    let label = kind.to_ascii_uppercase();
                    let content = if argument.is_empty() {
                        strip_directive_options(&body).join(" ")
                    } else {
                        format!("{argument} {}", strip_directive_options(&body).join(" "))
                    };
                    blocks.push(HtmlBlock::Paragraph {
                        text: format!("[{label}] {}", clean_rst_inline(content.trim())),
                    });
                }
                "include" | "raw" => {
                    push_warning(
                        &mut warnings,
                        &format!("reStructuredText {kind} directive was not evaluated"),
                    );
                }
                _ => {
                    push_warning(
                        &mut warnings,
                        "unsupported reStructuredText directive was kept as literal text",
                    );
                    let mut literal = vec![trimmed.to_owned()];
                    literal.extend(body);
                    blocks.push(HtmlBlock::CodeBlock {
                        text: literal.join("\n"),
                    });
                }
            }
            continue;
        }

        if is_rst_comment(trimmed) {
            i += 1;
            let (_, next) = collect_indented_block(&lines, i);
            i = next;
            continue;
        }

        if let Some((bullet, content)) = list_marker(trimmed) {
            blocks.push(HtmlBlock::ListItem {
                bullet,
                text: clean_rst_inline(content),
            });
            i += 1;
            continue;
        }

        if is_adornment(trimmed) && trimmed.len() >= 4 {
            blocks.push(HtmlBlock::HorizontalRule);
            i += 1;
            continue;
        }

        let start = i;
        let mut paragraph = Vec::<&str>::new();
        while i < lines.len() {
            let current = lines[i].trim();
            if current.is_empty()
                || directive(current).is_some()
                || list_marker(current).is_some()
                || (i + 1 < lines.len()
                    && is_adornment(lines[i + 1].trim())
                    && same_adornment(current, lines[i + 1].trim()))
                || (i + 2 < lines.len()
                    && is_adornment(current)
                    && same_adornment(current, lines[i + 2].trim()))
                || is_rst_comment(current)
                || parse_grid_table(&lines, i).is_some()
                || parse_simple_table(&lines, i).is_some()
            {
                break;
            }
            paragraph.push(current);
            i += 1;
        }
        if paragraph.is_empty() {
            // Defensive progress for malformed constructs the recognizers declined.
            i = start + 1;
            continue;
        }
        let joined = paragraph.join(" ");
        if joined.ends_with("::") {
            let preceding = joined.trim_end_matches(':').trim_end();
            if !preceding.is_empty() {
                blocks.push(HtmlBlock::Paragraph {
                    text: format!("{}:", clean_rst_inline(preceding)),
                });
            }
            while i < lines.len() && lines[i].trim().is_empty() {
                i += 1;
            }
            let (literal, next) = collect_indented_block(&lines, i);
            if !literal.is_empty() {
                blocks.push(HtmlBlock::CodeBlock {
                    text: literal.join("\n"),
                });
            }
            i = next;
        } else {
            blocks.push(HtmlBlock::Paragraph {
                text: clean_rst_inline(&joined),
            });
        }
    }
    Ok((blocks, warnings))
}

fn directive(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix(".. ")?;
    let (head, argument) = rest.split_once("::")?;
    let kind = head.trim();
    if kind.is_empty()
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return None;
    }
    Some((kind, argument.trim()))
}

fn is_rst_comment(line: &str) -> bool {
    line.starts_with(".. ") && directive(line).is_none()
}

fn collect_indented_block(lines: &[&str], mut index: usize) -> (Vec<String>, usize) {
    while index < lines.len() && lines[index].trim().is_empty() {
        index += 1;
    }
    let mut body = Vec::new();
    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            body.push(String::new());
            index += 1;
        } else if line.starts_with([' ', '\t']) {
            body.push(line.trim_start().to_owned());
            index += 1;
        } else {
            break;
        }
    }
    while body.last().is_some_and(String::is_empty) {
        body.pop();
    }
    (body, index)
}

fn directive_option<'a>(body: &'a [String], name: &str) -> Option<&'a str> {
    body.iter().find_map(|line| {
        let value = line.strip_prefix(':')?;
        let (key, value) = value.split_once(':')?;
        key.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

fn strip_directive_options(body: &[String]) -> Vec<String> {
    body.iter()
        .filter(|line| !line.trim_start().starts_with(':'))
        .cloned()
        .collect()
}

fn is_adornment(line: &str) -> bool {
    if line.len() < 3 || !line.is_ascii() {
        return false;
    }
    let mut chars = line.chars();
    let Some(mark) = chars.next() else {
        return false;
    };
    !mark.is_ascii_alphanumeric() && !mark.is_ascii_whitespace() && chars.all(|ch| ch == mark)
}

fn same_adornment(title: &str, underline: &str) -> bool {
    is_adornment(underline) && underline.len() >= title.len()
}

fn heading_level(mark: char, styles: &mut Vec<char>) -> u8 {
    let position = styles
        .iter()
        .position(|existing| *existing == mark)
        .unwrap_or_else(|| {
            styles.push(mark);
            styles.len() - 1
        });
    (position + 1).min(6) as u8
}

fn list_marker(line: &str) -> Option<(String, &str)> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(text) = line.strip_prefix(marker) {
            return Some(("• ".into(), text));
        }
    }
    let split = line.find(char::is_whitespace)?;
    let token = &line[..split];
    let content = line[split..].trim_start();
    let ordinal = token
        .strip_suffix('.')
        .or_else(|| token.strip_suffix(')'))?;
    let valid = ordinal == "#"
        || ordinal.parse::<usize>().is_ok()
        || ordinal.len() == 1 && ordinal.as_bytes()[0].is_ascii_alphabetic();
    valid.then(|| (format!("{token} "), content))
}

fn clean_rst_inline(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut i = 0usize;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b':'
            && let Some(role_marker) = text[i + 1..].find(":`")
        {
            let content_start = i + 1 + role_marker + 2;
            if let Some(end_offset) = text[content_start..].find('`') {
                let end = content_start + end_offset;
                let content = &text[content_start..end];
                let visible = content
                    .rsplit_once(" <")
                    .map_or(content, |(label, _target)| label);
                output.push_str(visible);
                i = end + 1;
                if text[i..].starts_with('_') {
                    i += 1;
                }
                continue;
            }
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 1;
            let ch = text[i..].chars().next().unwrap_or_default();
            output.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let delimiter = if text[i..].starts_with("**") {
            Some("**")
        } else if text[i..].starts_with("``") {
            Some("``")
        } else if bytes[i] == b'*' || bytes[i] == b'`' {
            Some(&text[i..i + 1])
        } else {
            None
        };
        if let Some(delimiter) = delimiter
            && let Some(end) = text[i + delimiter.len()..].find(delimiter)
        {
            let start = i + delimiter.len();
            let end = start + end;
            let content = &text[start..end];
            if delimiter == "`"
                && let Some((label, _target)) = content.rsplit_once(" <")
            {
                output.push_str(label);
            } else {
                output.push_str(content);
            }
            i = end + delimiter.len();
            if delimiter == "`" && text[i..].starts_with('_') {
                i += 1;
            }
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or_default();
        output.push(ch);
        i += ch.len_utf8();
    }
    output
}

fn parse_grid_table(lines: &[&str], start: usize) -> Option<(TableData, usize)> {
    let border = lines.get(start)?.trim();
    if !is_grid_border(border) {
        return None;
    }
    let positions = border
        .char_indices()
        .filter_map(|(index, ch)| (ch == '+').then_some(index))
        .collect::<Vec<_>>();
    let columns = positions.len().checked_sub(1)?;
    if columns == 0 {
        return None;
    }
    let mut rows = Vec::<Vec<String>>::new();
    let mut current = vec![String::new(); columns];
    let mut index = start + 1;
    while index < lines.len() {
        let line = lines[index].trim_end();
        if is_grid_border(line) {
            if current.iter().any(|cell| !cell.is_empty()) {
                rows.push(std::mem::take(&mut current));
                current = vec![String::new(); columns];
            }
            index += 1;
            if index >= lines.len() || !lines[index].trim_start().starts_with('|') {
                break;
            }
            continue;
        }
        if !line.starts_with('|') || line.len() <= *positions.last()? {
            break;
        }
        for column in 0..columns {
            let left = positions[column] + 1;
            let right = positions[column + 1];
            let cell = line.get(left..right).unwrap_or_default().trim();
            if !cell.is_empty() {
                if !current[column].is_empty() {
                    current[column].push(' ');
                }
                current[column].push_str(cell);
            }
        }
        index += 1;
    }
    table_from_rows(rows).map(|table| (table, index))
}

fn is_grid_border(line: &str) -> bool {
    line.starts_with('+')
        && line.ends_with('+')
        && line.matches('+').count() >= 3
        && line
            .bytes()
            .all(|byte| matches!(byte, b'+' | b'-' | b'=' | b' '))
}

fn parse_simple_table(lines: &[&str], start: usize) -> Option<(TableData, usize)> {
    let border = lines.get(start)?.trim();
    let bytes = border.as_bytes();
    let mut runs = Vec::<(usize, usize)>::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'=' {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index] == b'=' {
            index += 1;
        }
        if index - start < 3 {
            return None;
        }
        runs.push((start, index));
    }
    if runs.len() < 2 || !bytes.iter().all(|byte| *byte == b'=' || *byte == b' ') {
        return None;
    }
    let columns = runs
        .iter()
        .enumerate()
        .map(|(column, (left, _))| {
            let right = runs.get(column + 1).map_or(border.len(), |run| run.0);
            (*left, right)
        })
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut index = start + 1;
    let mut repeated_border = false;
    while index < lines.len() {
        let line = lines[index];
        if line.trim() == border {
            index += 1;
            if repeated_border || rows.is_empty() {
                break;
            }
            repeated_border = true;
            continue;
        }
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        let row = columns
            .iter()
            .map(|(left, right)| {
                line.get(*left..*right)
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        rows.push(row);
        index += 1;
    }
    table_from_rows(rows).map(|table| (table, index))
}

fn table_from_rows(mut rows: Vec<Vec<String>>) -> Option<TableData> {
    if rows.is_empty() {
        return None;
    }
    let headers = rows.remove(0);
    let columns = headers.len();
    if columns == 0 {
        return None;
    }
    for row in &mut rows {
        row.resize(columns, String::new());
        row.truncate(columns);
    }
    Some(TableData {
        headers,
        rows,
        alignments: vec![TableAlign::Left; columns],
        raw_source: String::new(),
    })
}

fn push_warning(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_lists_code_tables_and_safe_directives() {
        let rst = "Title\n=====\n\nSection\n-------\n\nA **bold** and `literal` paragraph with :ref:`named target` and :py:class:`str`.\n\n* first\n* second\n\nExample::\n\n    print('safe')\n\n.. warning:: use care\n\n   Review the input.\n\n+------+-------+\n| Name | Value |\n+======+=======+\n| A    | 1     |\n+------+-------+\n\n.. include:: /etc/passwd\n";
        let (blocks, warnings) = parse_rst_blocks_with_images(rst, &HashMap::new()).unwrap();
        assert!(matches!(&blocks[0], HtmlBlock::Heading { level: 1, text } if text == "Title"));
        assert!(matches!(&blocks[1], HtmlBlock::Heading { level: 2, text } if text == "Section"));
        assert!(
            matches!(&blocks[2], HtmlBlock::Paragraph { text } if text.contains("bold") && text.contains("literal"))
        );
        assert!(
            matches!(&blocks[2], HtmlBlock::Paragraph { text } if text.contains("named target") && text.contains("str") && !text.contains(":ref:"))
        );
        assert!(blocks.iter().any(
            |block| matches!(block, HtmlBlock::CodeBlock { text } if text.contains("print('safe')"))
        ));
        assert!(blocks.iter().any(|block| matches!(block, HtmlBlock::Table(table) if table.headers == ["Name", "Value"] && table.rows.len() == 1)));
        assert!(blocks.iter().any(
            |block| matches!(block, HtmlBlock::Paragraph { text } if text.starts_with("[WARNING]"))
        ));
        assert_eq!(
            warnings,
            ["reStructuredText include directive was not evaluated"]
        );
    }

    #[test]
    fn rejects_line_and_row_expansion_limits() {
        let long = format!("{}\n", "x".repeat(MAX_RST_LINE_BYTES + 1));
        assert!(
            validate_rst_lines(&long)
                .unwrap_err()
                .to_string()
                .contains("line exceeds")
        );
        let huge = format!(
            ".. code-block:: text\n\n    {}\n",
            "y".repeat(MAX_RST_LINE_BYTES)
        );
        assert!(parse_rst_blocks(&huge).is_err());
    }

    #[test]
    fn recognizes_restructuredtext_simple_tables() {
        let input = "=====  =====\nName   State\n=====  =====\nParser Ready\n=====  =====\n";
        let (blocks, warnings) = parse_rst_blocks_with_images(input, &HashMap::new()).unwrap();
        assert!(warnings.is_empty());
        assert!(blocks.iter().any(|block| matches!(block, HtmlBlock::Table(table) if table.headers == ["Name", "State"] && table.rows == [vec!["Parser".to_owned(), "Ready".to_owned()]])));
    }
}
