//! Bounded Org-mode (`.org`) document preview.
//!
//! Renders outline headings, paragraphs, lists, tables, literal source blocks,
//! quote blocks, and safe local PNG/JPEG file links. Babel code, `#+INCLUDE`,
//! raw export blocks, links, and table formulas are never evaluated or fetched.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{
    HtmlBlock, InlineHtmlImage, load_local_image_sources, render_blocks_to_pages,
};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_ORG_LINES: usize = 200_000;
const MAX_ORG_LINE_BYTES: usize = 1024 * 1024;
const MAX_ORG_IMAGE_REFERENCES: usize = 10_000;
const MAX_ORG_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ORG_IMAGE_WIDTH: u32 = 4096;
const MAX_ORG_TABLE_COLUMNS: usize = 64;
const MAX_ORG_TABLE_ROWS: usize = 10_000;
const MAX_ORG_TABLE_CELLS: usize = 500_000;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_ORG_INPUT_BYTES),
        "Org-mode input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("Org-mode file is not valid UTF-8: {error}"))
    })?;
    validate_org_lines(&text)?;
    let (sources, too_many) = collect_image_sources(&text);
    let mut warnings = Vec::new();
    let images = if sources.is_empty() && !too_many {
        HashMap::new()
    } else {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let base_dir = fs::canonicalize(parent)?;
        let (images, image_warnings) = load_local_image_sources(&base_dir, sources, too_many)?;
        warnings.extend(image_warnings);
        if !images.is_empty() {
            push_warning(
                &mut warnings,
                "Org-mode file links to images are rendered as centered flow blocks; inline positioning is approximated",
            );
        }
        images
    };
    let (blocks, parser_warnings) = parse_org_blocks_with_images(&text, &images)?;
    for warning in parser_warnings {
        push_warning(&mut warnings, &warning);
    }
    render_blocks_to_pages(&blocks, sink, options)?;
    Ok(warnings)
}

/// Parses common Org-mode syntax into renderable blocks without evaluating blocks.
pub fn parse_org_blocks(text: &str) -> Result<Vec<HtmlBlock>> {
    parse_org_blocks_with_images(text, &HashMap::new()).map(|(blocks, _)| blocks)
}

pub(crate) fn looks_like_org_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    text.lines()
        .take(12)
        .map(str::trim)
        .any(|line| line.to_ascii_uppercase().starts_with("#+TITLE:"))
}

fn validate_org_lines(text: &str) -> Result<()> {
    if text.len() as u64 > MAX_ORG_INPUT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Org-mode input exceeds {MAX_ORG_INPUT_BYTES} bytes"
        )));
    }
    if text.lines().count() > MAX_ORG_LINES {
        return Err(Error::LimitExceeded(format!(
            "Org-mode input exceeds {MAX_ORG_LINES} lines"
        )));
    }
    if text.lines().any(|line| line.len() > MAX_ORG_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "Org-mode line exceeds {MAX_ORG_LINE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn collect_image_sources(text: &str) -> (Vec<String>, bool) {
    let mut sources = Vec::new();
    let mut too_many = false;
    for line in text.lines().map(str::trim) {
        let Some(target) = standalone_file_link(line) else {
            continue;
        };
        if !is_supported_image_path(target) {
            continue;
        }
        if sources.len() >= MAX_ORG_IMAGE_REFERENCES {
            too_many = true;
        } else {
            sources.push(target.to_owned());
        }
    }
    (sources, too_many)
}

fn parse_org_blocks_with_images(
    text: &str,
    images: &HashMap<String, InlineHtmlImage>,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    validate_org_lines(text)?;
    let lines = text.lines().collect::<Vec<_>>();
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut pending_caption = None::<String>;
    let mut pending_image_width = None::<u32>;
    let mut table_cells = 0usize;
    let mut i = 0usize;

    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        if let Some(title) = keyword_value(trimmed, "#+TITLE:") {
            pending_caption = None;
            pending_image_width = None;
            blocks.push(HtmlBlock::Heading {
                level: 1,
                text: clean_org_inline(title),
            });
            i += 1;
            continue;
        }
        if let Some(caption) = keyword_value(trimmed, "#+CAPTION:") {
            pending_caption = Some(clean_org_inline(caption));
            i += 1;
            continue;
        }
        if let Some(width) = org_image_width_attribute(trimmed) {
            match width {
                Ok(Some(width)) => pending_image_width = Some(width),
                Ok(None) => {}
                Err(()) => push_warning(
                    &mut warnings,
                    "Org-mode image width must be an integer in pixels from 1 to 4096; the attribute was ignored",
                ),
            }
            i += 1;
            continue;
        }
        if let Some((kind, parameters)) = begin_block(trimmed) {
            pending_caption = None;
            pending_image_width = None;
            let kind = kind.to_ascii_uppercase();
            let (body, next) = collect_org_block(&lines, i + 1, &kind);
            i = next;
            match kind.as_str() {
                "SRC" | "EXAMPLE" => blocks.push(HtmlBlock::CodeBlock {
                    text: body.join("\n"),
                }),
                "QUOTE" | "QUOTATION" => {
                    if !body.is_empty() {
                        blocks.push(HtmlBlock::Paragraph {
                            text: format!("> {}", clean_org_inline(&body.join(" "))),
                        });
                    }
                }
                "VERSE" | "CENTER" => {
                    if !body.is_empty() {
                        blocks.push(HtmlBlock::Paragraph {
                            text: clean_org_inline(&body.join(" ")),
                        });
                    }
                }
                "EXPORT" => push_warning(
                    &mut warnings,
                    "Org-mode raw/export blocks were omitted and not interpreted",
                ),
                _ => {
                    push_warning(
                        &mut warnings,
                        "unsupported Org-mode block type was shown literally",
                    );
                    blocks.push(HtmlBlock::CodeBlock {
                        text: std::iter::once(format!("#+BEGIN_{kind}{parameters}"))
                            .chain(body)
                            .chain(std::iter::once(format!("#+END_{kind}")))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    });
                }
            }
            continue;
        }

        if trimmed.to_ascii_uppercase().starts_with("#+INCLUDE:") {
            pending_caption = None;
            pending_image_width = None;
            push_warning(&mut warnings, "Org-mode #+INCLUDE was not evaluated");
            i += 1;
            continue;
        }
        if trimmed.to_ascii_uppercase().starts_with("#+CALL:") {
            pending_caption = None;
            pending_image_width = None;
            push_warning(&mut warnings, "Org-mode Babel calls were not executed");
            i += 1;
            continue;
        }
        if trimmed.to_ascii_uppercase().starts_with("#+TBLFM:") {
            pending_caption = None;
            pending_image_width = None;
            push_warning(&mut warnings, "Org-mode table formulas were not evaluated");
            i += 1;
            continue;
        }
        if trimmed.starts_with("#+") {
            pending_caption = None;
            pending_image_width = None;
            // Metadata and export options change Org presentation, not the source
            // text preview; never pass raw HTML/CSS or setup files through.
            if !is_known_metadata_keyword(trimmed) {
                push_warning(&mut warnings, "unknown Org-mode keyword was omitted");
            }
            i += 1;
            continue;
        }

        if let Some(path) = standalone_file_link(trimmed)
            && is_supported_image_path(path)
        {
            if let Some(image) = images.get(path) {
                let caption = pending_caption.take();
                let (pixel_width, pixel_height) =
                    scaled_image_dimensions(image, pending_image_width.take());
                blocks.push(HtmlBlock::Image {
                    href: image.href.clone(),
                    pixel_width,
                    pixel_height,
                    alt: caption.clone().unwrap_or_default(),
                });
                if let Some(caption) = caption {
                    blocks.push(HtmlBlock::Paragraph { text: caption });
                }
            } else {
                pending_image_width = None;
                push_warning(
                    &mut warnings,
                    "Org-mode local image link was omitted because it was not a validated local PNG/JPEG file",
                );
                if let Some(caption) = pending_caption.take() {
                    blocks.push(HtmlBlock::Paragraph {
                        text: format!("[Image omitted: {caption}]"),
                    });
                }
            }
            i += 1;
            continue;
        }

        // Caption/image attributes apply only to the immediately following
        // renderable element; do not carry them across unrelated blocks.
        pending_caption = None;
        pending_image_width = None;

        if let Some((table, next)) = parse_org_table(&lines, i, &mut table_cells, &mut warnings) {
            blocks.push(HtmlBlock::Table(table));
            i = next;
            continue;
        }
        if let Some((level, title)) = headline(trimmed) {
            blocks.push(HtmlBlock::Heading {
                level,
                text: clean_org_inline(title),
            });
            i += 1;
            continue;
        }
        if let Some((bullet, text)) = list_item(trimmed) {
            blocks.push(HtmlBlock::ListItem {
                bullet,
                text: clean_org_inline(text),
            });
            i += 1;
            continue;
        }
        if is_horizontal_rule(trimmed) {
            blocks.push(HtmlBlock::HorizontalRule);
            i += 1;
            continue;
        }

        let mut paragraph = Vec::new();
        while i < lines.len() {
            let line = lines[i].trim();
            if line.is_empty()
                || line.starts_with("#+")
                || begin_block(line).is_some()
                || headline(line).is_some()
                || list_item(line).is_some()
                || is_org_table_row(line)
                || is_horizontal_rule(line)
                || standalone_file_link(line).is_some()
            {
                break;
            }
            paragraph.push(line);
            i += 1;
        }
        if paragraph.is_empty() {
            i += 1;
            continue;
        }
        blocks.push(HtmlBlock::Paragraph {
            text: clean_org_inline(&paragraph.join(" ")),
        });
    }
    Ok((blocks, warnings))
}

fn begin_block(line: &str) -> Option<(&str, &str)> {
    if !line
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("#+BEGIN_"))
    {
        return None;
    }
    let rest = line.get(8..)?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let kind = &rest[..end];
    if kind.is_empty()
        || !kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    Some((kind, &rest[end..]))
}

fn collect_org_block(lines: &[&str], mut index: usize, kind: &str) -> (Vec<String>, usize) {
    let end_marker = format!("#+END_{}", kind.to_ascii_uppercase());
    let mut body = Vec::new();
    while index < lines.len() {
        if lines[index].trim().eq_ignore_ascii_case(&end_marker) {
            return (body, index + 1);
        }
        body.push(lines[index].to_owned());
        index += 1;
    }
    (body, index)
}

fn keyword_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.get(..key.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(key))
        .map(|_| line[key.len()..].trim())
}

fn org_image_width_attribute(line: &str) -> Option<std::result::Result<Option<u32>, ()>> {
    let attributes = keyword_value(line, "#+ATTR_ORG:")?;
    let mut tokens = attributes.split_whitespace();
    while let Some(attribute) = tokens.next() {
        if !attribute.eq_ignore_ascii_case(":width") {
            continue;
        }
        let Some(value) = tokens.next() else {
            return Some(Err(()));
        };
        let value = if value.len() >= 2
            && value.as_bytes()[value.len() - 2].eq_ignore_ascii_case(&b'p')
            && value.as_bytes()[value.len() - 1].eq_ignore_ascii_case(&b'x')
        {
            &value[..value.len() - 2]
        } else {
            value
        };
        return Some(
            value
                .parse::<u32>()
                .ok()
                .filter(|width| (1..=MAX_ORG_IMAGE_WIDTH).contains(width))
                .map(Some)
                .ok_or(()),
        );
    }
    Some(Ok(None))
}

fn scaled_image_dimensions(image: &InlineHtmlImage, requested_width: Option<u32>) -> (u32, u32) {
    let Some(width) = requested_width else {
        return (image.pixel_width, image.pixel_height);
    };
    if image.pixel_width == 0 {
        return (image.pixel_width, image.pixel_height);
    }
    let height = (u64::from(image.pixel_height) * u64::from(width)
        + u64::from(image.pixel_width) / 2)
        / u64::from(image.pixel_width);
    (width, height.clamp(1, u64::from(u32::MAX)) as u32)
}

fn is_known_metadata_keyword(line: &str) -> bool {
    [
        "#+TITLE:",
        "#+SUBTITLE:",
        "#+AUTHOR:",
        "#+DATE:",
        "#+EMAIL:",
        "#+OPTIONS:",
        "#+STARTUP:",
        "#+LANGUAGE:",
        "#+FILETAGS:",
        "#+PROPERTY:",
        "#+TODO:",
        "#+SEQ_TODO:",
        "#+PRIORITIES:",
        "#+BIBLIOGRAPHY:",
        "#+LATEX_CLASS:",
        "#+RESULTS:",
        "#+CAPTION:",
        "#+NAME:",
        "#+ATTR_ORG:",
        "#+ATTR_HTML:",
    ]
    .iter()
    .any(|prefix| {
        line.get(..prefix.len())
            .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
    })
}

fn headline(line: &str) -> Option<(u8, &str)> {
    let level = line.bytes().take_while(|byte| *byte == b'*').count();
    if level == 0 || level > 6 || line.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    // Keep workflow states visible; they are document content, not export
    // controls or execution hints.
    Some((level as u8, line[level..].trim()))
}

fn list_item(line: &str) -> Option<(String, &str)> {
    for marker in ["- ", "+ "] {
        if let Some(text) = line.strip_prefix(marker) {
            return Some(("• ".into(), text));
        }
    }
    let split = line.find(char::is_whitespace)?;
    let token = &line[..split];
    let number = token
        .strip_suffix('.')
        .or_else(|| token.strip_suffix(')'))?;
    number
        .parse::<usize>()
        .ok()
        .map(|_| (format!("{token} "), line[split..].trim_start()))
}

fn is_horizontal_rule(line: &str) -> bool {
    line.len() >= 5 && line.bytes().all(|byte| byte == b'-')
}

fn standalone_file_link(line: &str) -> Option<&str> {
    let target = line.strip_prefix("[[file:")?.strip_suffix("]]")?;
    (!target.is_empty() && !target.contains(']')).then_some(target)
}

fn is_supported_image_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn parse_org_table(
    lines: &[&str],
    start: usize,
    document_cell_count: &mut usize,
    warnings: &mut Vec<String>,
) -> Option<(TableData, usize)> {
    if !is_org_table_row(lines.get(start)?.trim()) {
        return None;
    }
    let mut raw_rows = Vec::<Vec<String>>::new();
    let mut columns = None::<usize>;
    let mut index = start;
    while index < lines.len() {
        let line = lines[index].trim();
        if is_org_table_separator(line) {
            index += 1;
            continue;
        }
        if !is_org_table_row(line) {
            break;
        }
        if raw_rows.len() >= MAX_ORG_TABLE_ROWS {
            push_warning(
                warnings,
                "Org-mode table exceeded the supported row limit; remaining rows were omitted",
            );
            while index < lines.len()
                && (is_org_table_row(lines[index].trim())
                    || is_org_table_separator(lines[index].trim()))
            {
                index += 1;
            }
            break;
        }
        let mut cells = line
            .trim_matches('|')
            .split('|')
            .take(MAX_ORG_TABLE_COLUMNS + 1)
            .map(|cell| clean_org_inline(cell.trim()))
            .collect::<Vec<_>>();
        if cells.len() > MAX_ORG_TABLE_COLUMNS {
            push_warning(
                warnings,
                "Org-mode table exceeded the supported column limit; extra cells were omitted",
            );
            cells.truncate(MAX_ORG_TABLE_COLUMNS);
        }
        let width = *columns.get_or_insert(cells.len());
        cells.resize(width, String::new());
        cells.truncate(width);
        if document_cell_count.saturating_add(width) > MAX_ORG_TABLE_CELLS {
            push_warning(
                warnings,
                "Org-mode tables exceeded the document cell budget; remaining table rows were omitted",
            );
            while index < lines.len()
                && (is_org_table_row(lines[index].trim())
                    || is_org_table_separator(lines[index].trim()))
            {
                index += 1;
            }
            break;
        }
        *document_cell_count += width;
        raw_rows.push(cells);
        index += 1;
    }
    if raw_rows.is_empty() {
        return None;
    }
    let headers = raw_rows.remove(0);
    let columns = headers.len();
    let mut rows = raw_rows;
    for row in &mut rows {
        row.resize(columns, String::new());
        row.truncate(columns);
    }
    Some((
        TableData {
            headers,
            rows,
            alignments: vec![TableAlign::Left; columns],
            raw_source: String::new(),
        },
        index,
    ))
}

fn is_org_table_row(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|') && line.len() >= 2
}

fn is_org_table_separator(line: &str) -> bool {
    is_org_table_row(line)
        && line
            .bytes()
            .all(|byte| matches!(byte, b'|' | b'-' | b'+' | b' '))
        && line.contains('-')
}

fn clean_org_inline(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut index = 0usize;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        if text[index..].starts_with("[[")
            && let Some(end) = text[index + 2..].find("]]")
        {
            let end = index + 2 + end;
            let link = &text[index + 2..end];
            let visible = link
                .split_once("][")
                .map(|(_, description)| description)
                .unwrap_or_else(|| link.strip_prefix("file:").unwrap_or(link));
            output.push_str(visible);
            index = end + 2;
            continue;
        }
        let marker = match bytes[index] {
            b'*' | b'/' | b'_' | b'+' | b'=' | b'~' => Some(bytes[index] as char),
            _ => None,
        };
        if let Some(marker) = marker {
            let ch = marker.to_string();
            if let Some(end) = text[index + 1..].find(&ch) {
                let end = index + 1 + end;
                if end > index + 1 {
                    output.push_str(&text[index + 1..end]);
                    index = end + 1;
                    continue;
                }
            }
        }
        let character = text[index..].chars().next().unwrap_or_default();
        output.push(character);
        index += character.len_utf8();
    }
    output
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
    fn renders_outline_blocks_and_keeps_code_literal() {
        let org = "#+TITLE: Org Example\n\n* TODO First section\nText with *bold* and [[https://example.invalid][a link]].\n\n| Name | State |\n|------+-------|\n| Parser | Ready |\n\n#+BEGIN_SRC emacs-lisp\n(delete-file \"important\")\n#+END_SRC\n\n#+BEGIN_EXPORT html\n<script>must-not-render()</script>\n#+END_EXPORT\n\n#+INCLUDE: \"/etc/passwd\"\n#+CALL: dangerous-block()\n";
        let (blocks, warnings) = parse_org_blocks_with_images(org, &HashMap::new()).unwrap();
        assert!(
            matches!(&blocks[0], HtmlBlock::Heading { level: 1, text } if text == "Org Example")
        );
        assert!(
            matches!(&blocks[1], HtmlBlock::Heading { level: 1, text } if text == "TODO First section")
        );
        assert!(blocks.iter().any(|block| matches!(block, HtmlBlock::Table(table) if table.headers == ["Name", "State"] && table.rows.len() == 1)));
        assert!(blocks.iter().any(
            |block| matches!(block, HtmlBlock::CodeBlock { text } if text.contains("delete-file"))
        ));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("raw/export blocks were omitted"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("#+INCLUDE was not evaluated"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("Babel calls were not executed"))
        );
        let serialized = format!("{blocks:?}");
        assert!(!serialized.contains("must-not-render"));
        assert!(!serialized.contains("/etc/passwd"));
    }

    #[test]
    fn limits_long_org_lines() {
        let text = format!("{}\n", "x".repeat(MAX_ORG_LINE_BYTES + 1));
        assert!(
            validate_org_lines(&text)
                .unwrap_err()
                .to_string()
                .contains("line exceeds")
        );
    }

    #[test]
    fn parses_bounded_org_image_width_in_pixels() {
        assert_eq!(
            org_image_width_attribute("#+ATTR_ORG: :width 96px"),
            Some(Ok(Some(96)))
        );
        assert_eq!(
            org_image_width_attribute("#+ATTR_ORG: :width 8192"),
            Some(Err(()))
        );
        let image = InlineHtmlImage {
            href: "data:image/png;base64,AA==".into(),
            pixel_width: 2,
            pixel_height: 1,
        };
        assert_eq!(scaled_image_dimensions(&image, Some(96)), (96, 48));
    }
}
