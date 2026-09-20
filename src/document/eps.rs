//! Bounded ASCII PostScript/EPS vector preview.
//!
//! This reader supports a safe subset of path construction, colors, line
//! widths, fill/stroke, and showpage. It never executes PostScript procedures,
//! file operators, external resources, or embedded code.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke};

const MAX_EPS_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EPS_LINES: usize = 1_000_000;
const MAX_EPS_TOKENS: usize = 5_000_000;
const MAX_EPS_PATHS: usize = 200_000;
const MAX_EPS_COORDINATE: f64 = 1_000_000.0;
const MAX_EPS_PAGE_WIDTH: f64 = 20_000.0;
const MAX_EPS_PAGE_HEIGHT: f64 = 20_000.0;

#[derive(Clone)]
struct PathState {
    d: String,
    has_path: bool,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .take(32)
        .any(|line| line.trim_start().starts_with("%!PS") || line.contains("%%BoundingBox:"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_EPS_INPUT_BYTES),
        "PostScript input",
    )?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        Error::InvalidInput(format!("PostScript input is not ASCII/UTF-8: {error}"))
    })?;
    let (pages, warnings) = parse_document(text, options.max_pages)?;
    for page in pages {
        sink.consume(page)?;
    }
    Ok(warnings)
}

fn parse_document(text: &str, max_pages: usize) -> Result<(Vec<Page>, Vec<String>)> {
    let (bbox, mut warnings) = parse_bbox(text)?;
    let width = (bbox[2] - bbox[0]).clamp(1.0, MAX_EPS_PAGE_WIDTH);
    let height = (bbox[3] - bbox[1]).clamp(1.0, MAX_EPS_PAGE_HEIGHT);
    let mut pages = Vec::new();
    let mut page = Page::new(1, width, height, "eps");
    page.title = "PostScript page 1".into();
    let mut stack = Vec::<f64>::new();
    let mut path = PathState {
        d: String::new(),
        has_path: false,
    };
    let mut fill = Paint::None;
    let mut stroke = Paint::solid("#000000");
    let mut line_width = 1.0;
    let mut paths = 0usize;
    let mut tokens = 0usize;
    let mut line_count = 0usize;
    for raw_line in text.lines() {
        line_count = line_count.saturating_add(1);
        if line_count > MAX_EPS_LINES {
            return Err(Error::LimitExceeded(format!(
                "PostScript exceeds {MAX_EPS_LINES} lines"
            )));
        }
        let line = raw_line.trim();
        if line.starts_with('%') {
            continue;
        }
        for token in line.split_whitespace() {
            tokens = tokens.saturating_add(1);
            if tokens > MAX_EPS_TOKENS {
                return Err(Error::LimitExceeded(format!(
                    "PostScript exceeds {MAX_EPS_TOKENS} tokens"
                )));
            }
            if let Ok(value) = token.parse::<f64>() {
                if !value.is_finite() || value.abs() > MAX_EPS_COORDINATE {
                    return Err(Error::InvalidInput(
                        "PostScript coordinate/value is out of range".into(),
                    ));
                }
                stack.push(value);
                continue;
            }
            match token {
                "newpath" => {
                    path = PathState {
                        d: String::new(),
                        has_path: false,
                    }
                }
                "moveto" => {
                    let (x, y) = pop_pair(&mut stack)?;
                    path.d.push_str(&format!(
                        "M {:.2},{:.2} ",
                        x - bbox[0],
                        height - (y - bbox[1])
                    ));
                    path.has_path = true;
                }
                "lineto" => {
                    let (x, y) = pop_pair(&mut stack)?;
                    path.d.push_str(&format!(
                        "L {:.2},{:.2} ",
                        x - bbox[0],
                        height - (y - bbox[1])
                    ));
                    path.has_path = true;
                }
                "curveto" => {
                    let (x3, y3) = pop_pair(&mut stack)?;
                    let (x2, y2) = pop_pair(&mut stack)?;
                    let (x1, y1) = pop_pair(&mut stack)?;
                    path.d.push_str(&format!(
                        "C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} ",
                        x1 - bbox[0],
                        height - (y1 - bbox[1]),
                        x2 - bbox[0],
                        height - (y2 - bbox[1]),
                        x3 - bbox[0],
                        height - (y3 - bbox[1])
                    ));
                    path.has_path = true;
                }
                "closepath" => path.d.push_str("Z "),
                "setrgbcolor" => {
                    let b = stack.pop().ok_or_else(|| {
                        Error::InvalidInput("PostScript setrgbcolor is missing blue".into())
                    })?;
                    let g = stack.pop().ok_or_else(|| {
                        Error::InvalidInput("PostScript setrgbcolor is missing green".into())
                    })?;
                    let r = stack.pop().ok_or_else(|| {
                        Error::InvalidInput("PostScript setrgbcolor is missing red".into())
                    })?;
                    let color = format!(
                        "#{:02X}{:02X}{:02X}",
                        (r.clamp(0.0, 1.0) * 255.0) as u8,
                        (g.clamp(0.0, 1.0) * 255.0) as u8,
                        (b.clamp(0.0, 1.0) * 255.0) as u8
                    );
                    fill = Paint::solid(&color);
                    stroke = Paint::solid(color);
                }
                "setgray" => {
                    let gray = stack.pop().ok_or_else(|| {
                        Error::InvalidInput("PostScript setgray is missing value".into())
                    })?;
                    let channel = (gray.clamp(0.0, 1.0) * 255.0) as u8;
                    let color = format!("#{channel:02X}{channel:02X}{channel:02X}");
                    fill = Paint::solid(&color);
                    stroke = Paint::solid(color);
                }
                "setlinewidth" => line_width = stack.pop().unwrap_or(1.0).clamp(0.1, 100.0),
                "fill" | "eofill" => emit_path(
                    &mut page,
                    &mut path,
                    fill.clone(),
                    Paint::None,
                    line_width,
                    &mut paths,
                    &mut warnings,
                )?,
                "stroke" => emit_path(
                    &mut page,
                    &mut path,
                    Paint::None,
                    stroke.clone(),
                    line_width,
                    &mut paths,
                    &mut warnings,
                )?,
                "showpage" => {
                    if !page.nodes.is_empty() {
                        if pages.len() + 1 >= max_pages {
                            return Err(Error::LimitExceeded(
                                "PostScript exceeded max_pages".into(),
                            ));
                        }
                        pages.push(page);
                        let number = pages.len() + 1;
                        page = Page::new(number, width, height, "eps");
                        page.title = format!("PostScript page {number}");
                    }
                }
                "gsave" | "grestore" => push_warning_once(
                    &mut warnings,
                    "PostScript graphics state save/restore is approximated",
                ),
                "show" | "ashow" | "stringwidth" => {
                    push_warning_once(&mut warnings, "PostScript text operators are omitted")
                }
                "run" | "exec" | "file" | "system" | "deletefile" => push_warning_once(
                    &mut warnings,
                    "PostScript execution/file operators were blocked",
                ),
                _ if token
                    .chars()
                    .any(|character| character.is_ascii_alphabetic()) => {}
                _ => {}
            }
        }
    }
    if !page.nodes.is_empty() {
        pages.push(page);
    }
    if pages.is_empty() {
        return Err(Error::InvalidInput(
            "PostScript contains no renderable paths".into(),
        ));
    }
    Ok((pages, warnings))
}

fn parse_bbox(text: &str) -> Result<([f64; 4], Vec<String>)> {
    for line in text.lines() {
        if let Some(rest) = line
            .strip_prefix("%%BoundingBox:")
            .or_else(|| line.strip_prefix("%%HiResBoundingBox:"))
        {
            let values = rest
                .split_whitespace()
                .filter_map(|value| value.parse::<f64>().ok())
                .collect::<Vec<_>>();
            if values.len() == 4
                && values.iter().all(|value| value.is_finite())
                && values[2] > values[0]
                && values[3] > values[1]
            {
                return Ok(([values[0], values[1], values[2], values[3]], Vec::new()));
            }
        }
    }
    Err(Error::InvalidInput(
        "PostScript input has no valid %%BoundingBox".into(),
    ))
}

fn pop_pair(stack: &mut Vec<f64>) -> Result<(f64, f64)> {
    let y = stack
        .pop()
        .ok_or_else(|| Error::InvalidInput("PostScript path operator is missing y".into()))?;
    let x = stack
        .pop()
        .ok_or_else(|| Error::InvalidInput("PostScript path operator is missing x".into()))?;
    Ok((x, y))
}

fn emit_path(
    page: &mut Page,
    path: &mut PathState,
    fill: Paint,
    stroke: Paint,
    width: f64,
    paths: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if !path.has_path || path.d.trim().is_empty() {
        return Ok(());
    }
    *paths = paths.saturating_add(1);
    if *paths > MAX_EPS_PATHS {
        return Err(Error::LimitExceeded(format!(
            "PostScript exceeds {MAX_EPS_PATHS} paths"
        )));
    }
    page.nodes.push(Node::Path {
        id: format!("eps-path-{}", *paths),
        d: path.d.clone(),
        fill_rule: "evenodd".into(),
        fill,
        stroke: Stroke {
            paint: stroke,
            width,
            ..Default::default()
        },
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "eps:path".into(),
            ..Default::default()
        },
    });
    path.d.clear();
    path.has_path = false;
    let _ = warnings;
    Ok(())
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_bbox, parse_document};

    #[test]
    fn accepts_hires_bounding_box() {
        let (bbox, _) =
            parse_bbox("%!PS-Adobe-3.0\n%%HiResBoundingBox: 0.5 1.0 20.5 30.0\n").unwrap();
        assert_eq!(bbox, [0.5, 1.0, 20.5, 30.0]);
    }

    #[test]
    fn blocks_postscript_execution_operators() {
        let source = "%!PS-Adobe-3.0\n%%BoundingBox: 0 0 10 10\n(ignored) run\nnewpath 0 0 moveto 10 10 lineto stroke\n";
        let (pages, warnings) = parse_document(source, 2).unwrap();
        assert_eq!(pages.len(), 1);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("execution/file"))
        );
    }
}
