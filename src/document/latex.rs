//! Bounded structural previews for complete LaTeX documents.
//!
//! This is deliberately a source preview, not a TeX engine.  It recognizes a
//! small, deterministic document subset (metadata, sections, paragraphs,
//! lists, verbatim/code blocks, tabular rows, captions, simple inline math,
//! and local `\\includegraphics`) and never expands macros, reads included
//! files, executes shell escape, evaluates math, or fetches URLs.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{
    HtmlBlock, InlineHtmlImage, load_local_image_sources, render_blocks_to_pages_with_warnings,
};
use crate::error::{Error, Result};
use crate::math::{MathNode, parse_math_expression};
use crate::table::{TableAlign, TableData};

const MAX_LATEX_LINES: usize = 1_000_000;
const MAX_LATEX_LINE_BYTES: usize = 1024 * 1024;
const MAX_LATEX_IMAGE_REFERENCES: usize = 10_000;
const MAX_LATEX_COMMAND_BYTES: usize = 32 * 1024;
const MAX_LATEX_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_LATEX_ENV_DEPTH: usize = 64;

#[derive(Clone, Debug)]
struct LatexImageMacro {
    target: String,
    options: String,
    alt: String,
}

#[derive(Default)]
struct LatexTableBuilder {
    rows: Vec<Vec<String>>,
}

/// Returns true when the source looks like a complete LaTeX document rather
/// than a standalone formula.  The math converter keeps its historical
/// formula behavior for all other `.tex` inputs.
pub(crate) fn looks_like_document(source: &str) -> bool {
    source.contains("\\documentclass")
        || source.contains("\\begin{document}")
        || source.contains("\\end{document}")
}

pub(crate) fn convert_source(
    path: &Path,
    source: &str,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    validate_lines(source)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_dir = fs::canonicalize(parent)?;
    let graphic_paths = parse_graphicspath(source);
    let (sources, source_limit_exceeded) = collect_image_sources(source, &graphic_paths);
    let (images, mut warnings) =
        load_local_image_sources(&base_dir, sources, source_limit_exceeded)?;
    let (blocks, parser_warnings) = parse_blocks(source, &images, &graphic_paths)?;
    for warning in parser_warnings {
        push_warning_once(&mut warnings, &warning);
    }
    if !images.is_empty() {
        push_warning_once(
            &mut warnings,
            "LaTeX includegraphics images are rendered as centered flow blocks; figure floats, exact sizing, rotation, and wrapping are approximated",
        );
    }
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "LaTeX document contains no renderable content".into(),
        ));
    }
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

fn validate_lines(source: &str) -> Result<()> {
    if source.len() > MAX_LATEX_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "LaTeX source exceeds {MAX_LATEX_TEXT_BYTES} bytes"
        )));
    }
    let mut lines = 0usize;
    for line in source.lines() {
        lines = lines.saturating_add(1);
        if lines > MAX_LATEX_LINES {
            return Err(Error::LimitExceeded(format!(
                "LaTeX source exceeds {MAX_LATEX_LINES} lines"
            )));
        }
        if line.len() > MAX_LATEX_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "LaTeX line exceeds {MAX_LATEX_LINE_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

fn parse_graphicspath(source: &str) -> Vec<String> {
    let Some((contents, _)) = command_argument(source, "\\graphicspath") else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let mut cursor = 0usize;
    while cursor < contents.len() {
        let Some(open) = contents[cursor..].find('{') else {
            break;
        };
        let open = cursor + open;
        let Some((value, next)) = braced_value(contents, open) else {
            break;
        };
        if !value.is_empty()
            && value.len() <= MAX_LATEX_COMMAND_BYTES
            && !value.chars().any(char::is_control)
        {
            paths.push(value.to_owned());
        }
        cursor = next;
    }
    paths
}

fn collect_image_sources(source: &str, graphic_paths: &[String]) -> (Vec<String>, bool) {
    let mut sources = Vec::new();
    let mut exceeded = false;
    let mut in_verbatim = false;
    let mut verbatim_env = "";
    for line in source.lines() {
        let clean = strip_latex_comment(line).trim().to_owned();
        if in_verbatim {
            if clean.contains(&format!("\\end{{{verbatim_env}}}")) {
                in_verbatim = false;
            }
            continue;
        }
        for env in ["verbatim", "lstlisting", "minted"] {
            if clean.contains(&format!("\\begin{{{env}}}")) {
                in_verbatim = true;
                verbatim_env = env;
                break;
            }
        }
        if in_verbatim {
            continue;
        }
        let Some(image) = parse_includegraphics(&clean) else {
            continue;
        };
        let source = latex_image_source(graphic_paths, &image.target);
        if sources.len() >= MAX_LATEX_IMAGE_REFERENCES {
            exceeded = true;
        } else {
            sources.push(source);
        }
    }
    (sources, exceeded)
}

fn parse_blocks(
    source: &str,
    images: &HashMap<String, InlineHtmlImage>,
    graphic_paths: &[String],
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let title = command_argument(source, "\\title").map(|(value, _)| clean_latex_text(value));
    let author = command_argument(source, "\\author").map(|(value, _)| clean_latex_text(value));
    let date = command_argument(source, "\\date").map(|(value, _)| clean_latex_text(value));
    let has_document_begin = source.contains("\\begin{document}");
    let mut in_document = !has_document_begin;
    let mut paragraph = Vec::<String>::new();
    let mut list_env = None::<String>;
    let mut list_counter = 1usize;
    let mut code_env = None::<String>;
    let mut code_lines = Vec::<String>::new();
    let mut table = None::<LatexTableBuilder>;
    let mut figure_depth = 0usize;
    let mut env_depth = 0usize;
    let mut text_bytes = 0usize;
    let mut title_inserted = false;

    let flush_paragraph = |blocks: &mut Vec<HtmlBlock>, paragraph: &mut Vec<String>| {
        if paragraph.is_empty() {
            return;
        }
        let text = clean_latex_text(&paragraph.join(" "));
        if !text.trim().is_empty() {
            blocks.push(HtmlBlock::Paragraph { text });
        }
        paragraph.clear();
    };

    for raw_line in source.lines() {
        let line = strip_latex_comment(raw_line);
        let trimmed = line.trim();
        if trimmed.len() > MAX_LATEX_COMMAND_BYTES {
            return Err(Error::LimitExceeded(format!(
                "LaTeX command line exceeds {MAX_LATEX_COMMAND_BYTES} bytes"
            )));
        }
        if !in_document {
            if trimmed.contains("\\begin{document}") {
                in_document = true;
                if !title_inserted {
                    append_document_metadata(
                        &mut blocks,
                        title.as_deref(),
                        author.as_deref(),
                        date.as_deref(),
                    );
                    title_inserted = true;
                }
            }
            continue;
        }
        if trimmed.contains("\\end{document}") {
            break;
        }
        if !title_inserted {
            append_document_metadata(
                &mut blocks,
                title.as_deref(),
                author.as_deref(),
                date.as_deref(),
            );
            title_inserted = true;
        }

        if let Some(environment) = code_env.as_deref() {
            if trimmed.contains(&format!("\\end{{{environment}}}")) {
                blocks.push(HtmlBlock::CodeBlock {
                    text: code_lines.join("\n"),
                });
                code_lines.clear();
                code_env = None;
            } else {
                code_lines.push(raw_line.to_owned());
            }
            continue;
        }
        if let Some(table_builder) = table.as_mut() {
            if trimmed.contains("\\end{tabular}") {
                let table_data = finish_latex_table(std::mem::take(table_builder));
                if !table_data.headers.is_empty() {
                    blocks.push(HtmlBlock::Table(table_data));
                }
                table = None;
            } else if !trimmed.is_empty() && !trimmed.starts_with('%') {
                let row = parse_tabular_row(trimmed);
                if !row.is_empty() {
                    table_builder.rows.push(row);
                }
            }
            continue;
        }

        if let Some(environment) = list_env.as_deref() {
            if trimmed.contains(&format!("\\end{{{environment}}}")) {
                list_env = None;
                list_counter = 1;
                continue;
            }
            if let Some(item) = trimmed.strip_prefix("\\item") {
                let text = clean_latex_text(item.trim());
                if !text.is_empty() {
                    let bullet = if environment == "enumerate" {
                        let bullet = format!("{list_counter}. ");
                        list_counter = list_counter.saturating_add(1);
                        bullet
                    } else {
                        "• ".into()
                    };
                    blocks.push(HtmlBlock::ListItem { bullet, text });
                }
            } else if !trimmed.is_empty()
                && let Some(HtmlBlock::ListItem { text, .. }) = blocks.last_mut()
            {
                let continuation = clean_latex_text(trimmed);
                if !continuation.is_empty() {
                    text.push(' ');
                    text.push_str(&continuation);
                }
            }
            continue;
        }

        if let Some(environment) = begin_environment(trimmed) {
            env_depth = env_depth.saturating_add(1);
            if env_depth > MAX_LATEX_ENV_DEPTH {
                return Err(Error::LimitExceeded(format!(
                    "LaTeX environment nesting exceeds {MAX_LATEX_ENV_DEPTH}"
                )));
            }
            match environment.as_str() {
                "verbatim" | "lstlisting" | "minted" => {
                    flush_paragraph(&mut blocks, &mut paragraph);
                    code_env = Some(environment);
                }
                "itemize" | "enumerate" | "description" => {
                    flush_paragraph(&mut blocks, &mut paragraph);
                    list_env = Some(environment);
                    list_counter = 1;
                }
                "tabular" | "tabular*" => {
                    flush_paragraph(&mut blocks, &mut paragraph);
                    table = Some(LatexTableBuilder::default());
                }
                "figure" | "table" => {
                    flush_paragraph(&mut blocks, &mut paragraph);
                    figure_depth = figure_depth.saturating_add(1);
                }
                "equation" | "equation*" | "align" | "align*" => {
                    flush_paragraph(&mut blocks, &mut paragraph);
                    push_warning_once(
                        &mut warnings,
                        "LaTeX display equations are shown as inert text; no TeX math evaluation is performed",
                    );
                }
                _ => {
                    push_warning_once(
                        &mut warnings,
                        "unsupported LaTeX environments are shown as text without execution",
                    );
                }
            }
            continue;
        }
        if let Some(environment) = end_environment(trimmed) {
            env_depth = env_depth.saturating_sub(1);
            if environment == "figure" || environment == "table" {
                figure_depth = figure_depth.saturating_sub(1);
            }
            continue;
        }

        if trimmed.is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
            continue;
        }
        if trimmed == "\\maketitle" {
            continue;
        }
        if let Some((level, command)) = section_command(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(HtmlBlock::Heading {
                level,
                text: clean_latex_text(command),
            });
            continue;
        }
        if let Some(caption) = command_argument(trimmed, "\\caption") {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(HtmlBlock::Paragraph {
                text: format!("Figure: {}", clean_latex_text(caption.0)),
            });
            continue;
        }
        if let Some(image) = parse_includegraphics(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            append_image_block(&mut blocks, &mut warnings, images, graphic_paths, &image);
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("\\item") {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(HtmlBlock::ListItem {
                bullet: "• ".into(),
                text: clean_latex_text(item.trim()),
            });
            continue;
        }
        if contains_unsafe_or_external_command(trimmed) {
            push_warning_once(
                &mut warnings,
                "LaTeX input/include/shell commands were not executed or read",
            );
            continue;
        }
        let clean = clean_latex_text(trimmed);
        if !clean.is_empty() {
            text_bytes = text_bytes.saturating_add(clean.len());
            if text_bytes > MAX_LATEX_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "LaTeX rendered text exceeds {MAX_LATEX_TEXT_BYTES} bytes"
                )));
            }
            paragraph.push(clean);
        }
    }

    if let Some(code_env) = code_env {
        push_warning_once(
            &mut warnings,
            &format!("LaTeX environment '{code_env}' ended at EOF; code was still shown"),
        );
        blocks.push(HtmlBlock::CodeBlock {
            text: code_lines.join("\n"),
        });
    }
    if let Some(table_builder) = table {
        let table_data = finish_latex_table(table_builder);
        if !table_data.headers.is_empty() {
            blocks.push(HtmlBlock::Table(table_data));
        }
    }
    flush_paragraph(&mut blocks, &mut paragraph);
    if figure_depth > 0 {
        push_warning_once(
            &mut warnings,
            "LaTeX figure/table float environment was not closed; layout was approximated",
        );
    }
    Ok((blocks, warnings))
}

fn append_document_metadata(
    blocks: &mut Vec<HtmlBlock>,
    title: Option<&str>,
    author: Option<&str>,
    date: Option<&str>,
) {
    if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
        blocks.push(HtmlBlock::Heading {
            level: 1,
            text: title.to_owned(),
        });
    }
    if let Some(author) = author.filter(|author| !author.trim().is_empty()) {
        blocks.push(HtmlBlock::Paragraph {
            text: format!("Author: {author}"),
        });
    }
    if let Some(date) = date.filter(|date| !date.trim().is_empty()) {
        blocks.push(HtmlBlock::Paragraph {
            text: format!("Date: {date}"),
        });
    }
}

fn append_image_block(
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut Vec<String>,
    images: &HashMap<String, InlineHtmlImage>,
    graphic_paths: &[String],
    image: &LatexImageMacro,
) {
    let source = latex_image_source(graphic_paths, &image.target);
    if let Some(inline) = images.get(&source) {
        let (pixel_width, pixel_height) = latex_display_dimensions(inline, &image.options);
        blocks.push(HtmlBlock::Image {
            href: inline.href.clone(),
            pixel_width,
            pixel_height,
            alt: image.alt.clone(),
        });
    } else {
        push_warning_once(
            warnings,
            "LaTeX image target was omitted because it was not a validated local PNG/JPEG resource",
        );
        if !image.alt.is_empty() {
            blocks.push(HtmlBlock::Paragraph {
                text: format!("[Image omitted: {}]", image.alt),
            });
        }
    }
    if !image.options.trim().is_empty() {
        push_warning_once(
            warnings,
            "LaTeX includegraphics options such as trim, clip, angle, and exact float placement are approximated",
        );
    }
}

fn latex_display_dimensions(image: &InlineHtmlImage, options: &str) -> (u32, u32) {
    let mut width = None;
    let mut height = None;
    for item in options.split(',') {
        let Some((name, value)) = item.split_once('=') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "width" => width = parse_latex_dimension(value),
            "height" => height = parse_latex_dimension(value),
            _ => {}
        }
    }
    let intrinsic_width = image.pixel_width.max(1);
    let intrinsic_height = image.pixel_height.max(1);
    match (width, height) {
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

fn parse_latex_dimension(value: &str) -> Option<u32> {
    let value = value.trim().replace(' ', "");
    if value.contains("\\textwidth") || value.contains("\\linewidth") {
        let factor = value
            .split('\\')
            .next()
            .filter(|prefix| !prefix.is_empty())
            .and_then(|prefix| prefix.parse::<f64>().ok())
            .unwrap_or(1.0);
        return finite_dimension(factor * 360.0);
    }
    let (number, scale) = if let Some(number) = value.strip_suffix("pt") {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix("in") {
        (number, 72.0)
    } else if let Some(number) = value.strip_suffix("cm") {
        (number, 72.0 / 2.54)
    } else if let Some(number) = value.strip_suffix("mm") {
        (number, 72.0 / 25.4)
    } else if let Some(number) = value.strip_suffix("px") {
        (number, 0.75)
    } else {
        (value.as_str(), 1.0)
    };
    finite_dimension(number.parse::<f64>().ok()? * scale)
}

fn finite_dimension(value: f64) -> Option<u32> {
    value
        .is_finite()
        .then_some(value.round() as u32)
        .filter(|value| (1..=4096).contains(value))
}

fn parse_includegraphics(line: &str) -> Option<LatexImageMacro> {
    let start = line.find("\\includegraphics")?;
    let mut cursor = start + "\\includegraphics".len();
    let bytes = line.as_bytes();
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    let mut options = String::new();
    if bytes.get(cursor) == Some(&b'[') {
        let end = find_balanced(line, cursor, b'[', b']')?;
        options = line[cursor + 1..end].to_owned();
        cursor = end + 1;
    }
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'{') {
        return None;
    }
    let (target, _) = braced_value(line, cursor)?;
    if target.is_empty() || target.len() > MAX_LATEX_COMMAND_BYTES {
        return None;
    }
    let alt = Path::new(&target)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Embedded LaTeX image")
        .replace(['_', '-'], " ");
    Some(LatexImageMacro {
        target: target.to_owned(),
        options,
        alt,
    })
}

fn latex_image_source(graphic_paths: &[String], target: &str) -> String {
    if target.starts_with('/') || target.contains("://") || graphic_paths.is_empty() {
        return target.to_owned();
    }
    let prefix = graphic_paths
        .first()
        .map(String::as_str)
        .unwrap_or_default();
    if prefix.is_empty() {
        target.to_owned()
    } else if prefix.ends_with('/') || prefix.ends_with('\\') {
        format!("{prefix}{target}")
    } else {
        format!("{prefix}/{target}")
    }
}

fn section_command(line: &str) -> Option<(u8, &str)> {
    for (level, command) in [
        (1, "\\part"),
        (1, "\\chapter"),
        (2, "\\section"),
        (3, "\\subsection"),
        (4, "\\subsubsection"),
        (5, "\\paragraph"),
        (6, "\\subparagraph"),
    ] {
        if let Some((value, _)) = command_argument(line, command) {
            return Some((level, value));
        }
    }
    None
}

fn begin_environment(line: &str) -> Option<String> {
    let start = line.find("\\begin{")? + "\\begin{".len();
    let end = line[start..].find('}')? + start;
    let name = &line[start..end];
    (!name.is_empty() && name.len() <= 128).then(|| name.to_owned())
}

fn end_environment(line: &str) -> Option<String> {
    let start = line.find("\\end{")? + "\\end{".len();
    let end = line[start..].find('}')? + start;
    let name = &line[start..end];
    (!name.is_empty() && name.len() <= 128).then(|| name.to_owned())
}

fn parse_tabular_row(line: &str) -> Vec<String> {
    let row = line.trim_end_matches("\\\\").trim_end_matches('&').trim();
    row.split('&')
        .map(clean_latex_text)
        .map(|value| value.trim().to_owned())
        .collect()
}

fn finish_latex_table(table: LatexTableBuilder) -> TableData {
    let mut rows = table.rows;
    let headers = rows.first().cloned().unwrap_or_default();
    if !rows.is_empty() {
        rows.remove(0);
    }
    let columns = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .max(1);
    let mut headers = headers;
    headers.resize(columns, String::new());
    for row in &mut rows {
        row.resize(columns, String::new());
    }
    TableData {
        headers,
        rows,
        alignments: vec![TableAlign::Left; columns],
        raw_source: String::new(),
    }
}

fn command_argument<'a>(source: &'a str, command: &str) -> Option<(&'a str, usize)> {
    let start = source.find(command)? + command.len();
    let mut cursor = start;
    let bytes = source.as_bytes();
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'{') {
        return None;
    }
    let (value, next) = braced_value(source, cursor)?;
    Some((value, next))
}

fn braced_value(source: &str, open: usize) -> Option<(&str, usize)> {
    if source.as_bytes().get(open) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut escaped = false;
    for (offset, byte) in source.as_bytes()[open..].iter().copied().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            continue;
        }
        match byte {
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let end = open + offset;
                    return Some((&source[open + 1..end], end + 1));
                }
            }
            _ => {}
        }
    }
    None
}

fn find_balanced(source: &str, open: usize, open_byte: u8, close_byte: u8) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in source.as_bytes()[open..].iter().copied().enumerate() {
        match byte {
            byte if byte == open_byte => depth = depth.saturating_add(1),
            byte if byte == close_byte => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn strip_latex_comment(line: &str) -> String {
    let mut escaped = false;
    let mut result = String::with_capacity(line.len());
    for character in line.chars() {
        if character == '%' && !escaped {
            break;
        }
        result.push(character);
        if character == '\\' {
            escaped = !escaped;
        } else {
            escaped = false;
        }
    }
    result
}

fn clean_latex_text(text: &str) -> String {
    clean_latex_text_depth(text, 0)
}

fn clean_latex_text_depth(text: &str, depth: usize) -> String {
    if depth > 32 {
        return String::new();
    }
    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = 0usize;
    while cursor < chars.len() {
        let character = chars[cursor];
        if character == '$'
            && let Some(end) = chars[cursor + 1..].iter().position(|value| *value == '$')
        {
            let end = cursor + 1 + end;
            let formula: String = chars[cursor + 1..end].iter().collect();
            result.push_str(&render_inline_math(&formula));
            cursor = end + 1;
            continue;
        }
        if character == '\\'
            && let Some(next) = chars.get(cursor + 1).copied()
        {
            if !next.is_alphabetic() {
                result.push(match next {
                    '~' => '\u{00a0}',
                    '%' => '%',
                    '&' => '&',
                    '#' => '#',
                    '$' => '$',
                    '_' => '_',
                    '{' => '{',
                    '}' => '}',
                    '\\' => ' ',
                    other => other,
                });
                cursor += 2;
                continue;
            }
            let command_start = cursor + 1;
            let mut command_end = command_start;
            while command_end < chars.len() && chars[command_end].is_alphabetic() {
                command_end += 1;
            }
            let command: String = chars[command_start..command_end].iter().collect();
            if command_end < chars.len() && chars[command_end] == '{' {
                let remainder: String = chars[command_end..].iter().collect();
                if let Some((value, next)) = braced_value(&remainder, 0) {
                    result.push_str(&clean_latex_text_depth(value, depth + 1));
                    cursor = command_end + next;
                    continue;
                }
            }
            if matches!(
                command.as_str(),
                "LaTeX" | "TeX" | "ldots" | "dots" | "newline" | "linebreak"
            ) {
                if matches!(command.as_str(), "ldots" | "dots") {
                    result.push('…');
                } else if matches!(command.as_str(), "newline" | "linebreak") {
                    result.push('\n');
                }
            }
            cursor = command_end;
            continue;
        }
        if character != '{' && character != '}' {
            result.push(character);
        }
        cursor += 1;
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn render_inline_math(formula: &str) -> String {
    match parse_math_expression(formula, 0) {
        Ok(node) => math_node_to_text(&node),
        Err(_) => formula.to_owned(),
    }
}

fn math_node_to_text(node: &MathNode) -> String {
    match node {
        MathNode::Text(text) => text.clone(),
        MathNode::Row(nodes) => nodes.iter().map(math_node_to_text).collect(),
        MathNode::Superscript(inner) => format!("^{{{}}}", math_node_to_text(inner)),
        MathNode::Subscript(inner) => format!("_{{{}}}", math_node_to_text(inner)),
        MathNode::Fraction {
            numerator,
            denominator,
        } => format!(
            "({})/({})",
            math_node_to_text(numerator),
            math_node_to_text(denominator)
        ),
        MathNode::Sqrt(inner) => format!("√({})", math_node_to_text(inner)),
        MathNode::Matrix { rows, delimiters } => {
            let body = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(math_node_to_text)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>()
                .join("; ");
            if let Some((left, right)) = delimiters {
                format!("{left}{body}{right}")
            } else {
                body
            }
        }
    }
}

fn contains_unsafe_or_external_command(line: &str) -> bool {
    [
        "\\input",
        "\\include",
        "\\subfile",
        "\\write18",
        "\\immediate\\write",
        "\\bibliography",
        "\\addbibresource",
        "\\href",
        "\\url",
    ]
    .iter()
    .any(|command| line.contains(command))
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_complete_documents_but_not_plain_formulas() {
        assert!(looks_like_document(
            "\\documentclass{article}\n\\begin{document}"
        ));
        assert!(looks_like_document("\\begin{document}Text"));
        assert!(!looks_like_document("x^2 + y^2"));
    }

    #[test]
    fn parses_graphics_options_and_nested_arguments() {
        let image = parse_includegraphics(
            "\\includegraphics[width=.8\\textwidth,height=20pt]{figures/a_b.png}",
        )
        .unwrap();
        assert_eq!(image.target, "figures/a_b.png");
        assert_eq!(parse_latex_dimension(".8\\textwidth"), Some(288));
        assert_eq!(braced_value("{a{b}c}", 0).unwrap().0, "a{b}c");
    }

    #[test]
    fn renders_safe_structure_without_executing_commands() {
        let source = r#"\documentclass{article}
\title{Preview}
\author{Writer}
\begin{document}
\maketitle
\section{Intro}
Text with $x^2$.
\begin{itemize}
\item Safe item
\end{itemize}
\begin{verbatim}
\write18{rm -rf /}
\end{verbatim}
\end{document}"#;
        let (blocks, warnings) = parse_blocks(source, &HashMap::new(), &[]).unwrap();
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::Heading { text, .. } if text == "Preview"
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::ListItem { text, .. } if text == "Safe item"
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::CodeBlock { text } if text.contains("write18")
        )));
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}
