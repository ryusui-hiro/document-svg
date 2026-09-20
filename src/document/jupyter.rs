use std::collections::HashMap;
use std::io::Read;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Map, Value};
use std::io::Cursor;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{
    HtmlBlock, InlineHtmlImage, parse_html_blocks_with_limit, render_blocks_to_pages,
};
use crate::document::markdown::parse_markdown_blocks_with_images;
use crate::error::{Error, Result};

const MAX_JUPYTER_JSON_BYTES: u64 = 256 * 1024 * 1024;
const MAX_JUPYTER_CELLS: usize = 100_000;
const MAX_JUPYTER_OUTPUTS: usize = 100_000;
const MAX_JUPYTER_BLOCKS: usize = 500_000;
const MAX_MARKDOWN_LINES: usize = 500_000;
const MAX_RENDERED_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENCODED_IMAGE_BYTES: usize = MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4;
const MAX_TOTAL_IMAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_IMAGE_DECODE_BYTES: usize = 160 * 1024 * 1024;
const MAX_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_TOTAL_IMAGE_PIXELS: u64 = 100_000_000;
const SUPPORTED_NBFORMAT_MAJOR: u64 = 4;
const LATEST_NBFORMAT_MINOR: u64 = 5;

#[derive(Default)]
struct RenderBudget {
    text_bytes: usize,
    image_bytes: usize,
    image_pixels: u64,
    outputs: usize,
    blocks: usize,
    markdown_lines: usize,
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_JUPYTER_JSON_BYTES);
    let mut bytes = Vec::new();
    Read::take(input, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "Jupyter notebook exceeds maximum input limit of {max_bytes} bytes"
        )));
    }
    let root: Value = serde_json::from_slice(&bytes).map_err(|error| {
        Error::InvalidInput(format!("Jupyter notebook is not valid JSON: {error}"))
    })?;
    let (blocks, warnings) = parse_notebook(&root, options)?;
    render_blocks_to_pages(&blocks, sink, options)?;
    Ok(warnings)
}

fn parse_notebook(root: &Value, options: &ConvertOptions) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let nbformat = root
        .get("nbformat")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            Error::InvalidInput("Jupyter notebook is missing integer nbformat".into())
        })?;
    if nbformat != SUPPORTED_NBFORMAT_MAJOR {
        return Err(Error::Unsupported(format!(
            "Jupyter notebook format version {nbformat} is unsupported; nbformat 4 is required"
        )));
    }
    let minor = root
        .get("nbformat_minor")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let cells = root
        .get("cells")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidInput("Jupyter notebook is missing its cells array".into()))?;
    if cells.len() > MAX_JUPYTER_CELLS {
        return Err(Error::LimitExceeded(format!(
            "Jupyter notebook exceeds {MAX_JUPYTER_CELLS} cells"
        )));
    }

    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut budget = RenderBudget::default();
    if minor > LATEST_NBFORMAT_MINOR {
        warnings.push(format!(
            "Jupyter notebook nbformat minor version {minor} is newer than the renderer's known schema; unknown fields were ignored"
        ));
    }

    for (cell_index, cell) in cells.iter().enumerate() {
        let cell_type = cell
            .get("cell_type")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "Jupyter cell {} is missing its cell_type",
                    cell_index + 1
                ))
            })?;
        let source_hidden = cell
            .pointer("/metadata/jupyter/source_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let outputs_hidden = cell
            .pointer("/metadata/jupyter/outputs_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        match cell_type {
            "markdown" => {
                if !source_hidden {
                    let attachments = read_cell_attachments(
                        cell.get("attachments"),
                        &mut budget,
                        &mut warnings,
                        cell_index,
                    )?;
                    let source = read_multiline(cell.get("source"), "Markdown source", cell_index)?;
                    budget.add_text(&source)?;
                    let parsed = parse_markdown_content(&source, &mut budget, &attachments)?;
                    extend_blocks(&mut blocks, &mut budget, parsed)?;
                }
            }
            "code" => {
                if !source_hidden {
                    let execution_count =
                        match cell.get("execution_count") {
                            Some(Value::Null) => String::new(),
                            Some(value) => value
                                .as_u64()
                                .map(|count| count.to_string())
                                .ok_or_else(|| {
                                    Error::InvalidInput(format!(
                                        "Jupyter code cell {} has an invalid execution_count",
                                        cell_index + 1
                                    ))
                                })?,
                            None => {
                                return Err(Error::InvalidInput(format!(
                                    "Jupyter code cell {} is missing execution_count",
                                    cell_index + 1
                                )));
                            }
                        };
                    push_block(
                        &mut blocks,
                        &mut budget,
                        HtmlBlock::Heading {
                            level: 5,
                            text: format!("In [{execution_count}]:"),
                        },
                    )?;
                    let source = read_multiline(cell.get("source"), "code source", cell_index)?;
                    budget.add_text(&source)?;
                    if !source.is_empty() {
                        push_block(
                            &mut blocks,
                            &mut budget,
                            HtmlBlock::CodeBlock { text: source },
                        )?;
                    }
                }
                if !outputs_hidden {
                    let outputs =
                        cell.get("outputs")
                            .and_then(Value::as_array)
                            .ok_or_else(|| {
                                Error::InvalidInput(format!(
                                    "Jupyter code cell {} is missing its outputs array",
                                    cell_index + 1
                                ))
                            })?;
                    for output in outputs {
                        budget.outputs = budget.outputs.saturating_add(1);
                        if budget.outputs > MAX_JUPYTER_OUTPUTS {
                            return Err(Error::LimitExceeded(format!(
                                "Jupyter notebook exceeds {MAX_JUPYTER_OUTPUTS} outputs"
                            )));
                        }
                        render_output(
                            output,
                            &mut blocks,
                            &mut warnings,
                            &mut budget,
                            options.max_xml_events,
                            cell_index,
                        )?;
                    }
                }
            }
            "raw" => {
                if !source_hidden {
                    let source = read_multiline(cell.get("source"), "raw source", cell_index)?;
                    budget.add_text(&source)?;
                    if !source.is_empty() {
                        push_block(
                            &mut blocks,
                            &mut budget,
                            HtmlBlock::CodeBlock { text: source },
                        )?;
                    }
                }
            }
            _ => {
                warnings.push(format!(
                    "Jupyter cell {} has unsupported type '{cell_type}' and was omitted",
                    cell_index + 1
                ));
            }
        }
        push_block(&mut blocks, &mut budget, HtmlBlock::HorizontalRule)?;
    }

    Ok((blocks, warnings))
}

fn render_output(
    output: &Value,
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut Vec<String>,
    budget: &mut RenderBudget,
    max_xml_events: usize,
    cell_index: usize,
) -> Result<()> {
    let output_type = output
        .get("output_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match output_type {
        "stream" => {
            let text = read_multiline(output.get("text"), "stream output", cell_index)?;
            budget.add_text(&text)?;
            if !text.is_empty() {
                let name = output
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("output");
                push_block(
                    blocks,
                    budget,
                    HtmlBlock::Heading {
                        level: 6,
                        text: safe_text(name),
                    },
                )?;
                push_block(blocks, budget, HtmlBlock::CodeBlock { text })?;
            }
        }
        "error" => {
            let name = output
                .get("ename")
                .and_then(Value::as_str)
                .unwrap_or("Error");
            let message = output
                .get("evalue")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let traceback = output
                .get("traceback")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let text = safe_text(&format!("{name}: {message}\n{traceback}"));
            budget.add_text(&text)?;
            push_block(
                blocks,
                budget,
                HtmlBlock::Heading {
                    level: 6,
                    text: "Error".into(),
                },
            )?;
            push_block(blocks, budget, HtmlBlock::CodeBlock { text })?;
        }
        "display_data" | "execute_result" => {
            let Some(data) = output.get("data").and_then(Value::as_object) else {
                warnings.push(format!(
                    "Jupyter cell {} has an output without a MIME data bundle",
                    cell_index + 1
                ));
                return Ok(());
            };
            if !render_mime_bundle(data, blocks, warnings, budget, max_xml_events, cell_index)? {
                warnings.push(format!(
                    "Jupyter cell {} has an output with no supported MIME representation",
                    cell_index + 1
                ));
            }
        }
        _ => warnings.push(format!(
            "Jupyter cell {} has an unsupported output type '{output_type}'",
            cell_index + 1
        )),
    }
    Ok(())
}

fn render_mime_bundle(
    data: &Map<String, Value>,
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut Vec<String>,
    budget: &mut RenderBudget,
    max_xml_events: usize,
    cell_index: usize,
) -> Result<bool> {
    if data.contains_key("image/svg+xml") {
        push_warning_once(
            warnings,
            "Jupyter SVG MIME outputs are not embedded; use a PNG/JPEG or text fallback",
        );
    }
    for mime in [
        "image/png",
        "image/jpeg",
        "text/html",
        "text/markdown",
        "text/plain",
        "application/json",
    ] {
        let Some(value) = data.get(mime) else {
            continue;
        };
        match mime {
            "image/png" | "image/jpeg" => {
                let image = read_image(value, mime, budget, cell_index)?;
                push_block(blocks, budget, image)?;
            }
            "text/html" => {
                let html = read_multiline(Some(value), "HTML output", cell_index)?;
                budget.add_text(&html)?;
                if html.to_ascii_lowercase().contains("<img") {
                    push_warning_once(
                        warnings,
                        "Jupyter HTML output images are omitted unless supplied as a PNG/JPEG MIME representation",
                    );
                }
                match parse_html_blocks_with_limit(&html, max_xml_events.min(MAX_JUPYTER_BLOCKS)) {
                    Ok(parsed) if !parsed.is_empty() => extend_blocks(blocks, budget, parsed)?,
                    Ok(_) => push_block(blocks, budget, HtmlBlock::CodeBlock { text: html })?,
                    Err(_) => {
                        push_warning_once(
                            warnings,
                            "Jupyter HTML outputs that exceed the supported safe subset are shown as source text",
                        );
                        push_block(blocks, budget, HtmlBlock::CodeBlock { text: html })?;
                    }
                }
            }
            "text/markdown" => {
                let markdown = read_multiline(Some(value), "Markdown output", cell_index)?;
                budget.add_text(&markdown)?;
                let parsed = parse_markdown_content(&markdown, budget, &HashMap::new())?;
                extend_blocks(blocks, budget, parsed)?;
            }
            "text/plain" => {
                let text = read_multiline(Some(value), "plain-text output", cell_index)?;
                budget.add_text(&text)?;
                push_block(blocks, budget, HtmlBlock::CodeBlock { text })?;
            }
            "application/json" => {
                let text = serde_json::to_string_pretty(value).map_err(|error| {
                    Error::InvalidInput(format!("cannot render Jupyter JSON output: {error}"))
                })?;
                budget.add_text(&text)?;
                push_block(blocks, budget, HtmlBlock::CodeBlock { text })?;
            }
            _ => unreachable!(),
        }
        return Ok(true);
    }
    Ok(false)
}

fn read_image(
    value: &Value,
    mime: &str,
    budget: &mut RenderBudget,
    cell_index: usize,
) -> Result<HtmlBlock> {
    let encoded = read_multiline(Some(value), "base64 image output", cell_index)?;
    let encoded = encoded
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    if encoded.len() > MAX_ENCODED_IMAGE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Jupyter cell {} encoded image exceeds its size limit",
            cell_index + 1
        )));
    }
    let bytes = BASE64_STANDARD.decode(encoded).map_err(|error| {
        Error::InvalidInput(format!(
            "Jupyter cell {} has invalid {mime} base64 data: {error}",
            cell_index + 1
        ))
    })?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Jupyter cell {} image output exceeds {MAX_IMAGE_BYTES} bytes",
            cell_index + 1
        )));
    }
    let next_total_bytes = budget
        .image_bytes
        .checked_add(bytes.len())
        .ok_or_else(|| Error::LimitExceeded("Jupyter image output byte count overflowed".into()))?;
    if next_total_bytes > MAX_TOTAL_IMAGE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Jupyter image outputs exceed {MAX_TOTAL_IMAGE_BYTES} bytes"
        )));
    }
    let (width, height) = validate_image(&bytes, mime, cell_index)?;
    let pixels = check_image_pixels(width, height, cell_index)?;
    let next_total_pixels = budget.image_pixels.saturating_add(pixels);
    if next_total_pixels > MAX_TOTAL_IMAGE_PIXELS {
        return Err(Error::LimitExceeded(format!(
            "Jupyter image outputs exceed {MAX_TOTAL_IMAGE_PIXELS} pixels"
        )));
    }
    budget.image_bytes = next_total_bytes;
    budget.image_pixels = next_total_pixels;
    let alt = format!("{mime} output ({width} × {height})");
    Ok(HtmlBlock::Image {
        href: format!("data:{mime};base64,{}", BASE64_STANDARD.encode(bytes)),
        pixel_width: width,
        pixel_height: height,
        alt,
    })
}

fn validate_image(bytes: &[u8], mime: &str, cell_index: usize) -> Result<(u32, u32)> {
    match mime {
        "image/png" => {
            let mut decoder = png::Decoder::new(Cursor::new(bytes));
            decoder.set_limits(png::Limits {
                bytes: MAX_IMAGE_DECODE_BYTES,
            });
            let mut reader = decoder.read_info().map_err(|error| {
                Error::InvalidInput(format!(
                    "Jupyter cell {} PNG output is invalid: {error}",
                    cell_index + 1
                ))
            })?;
            let (width, height) = reader.info().size();
            check_image_pixels(width, height, cell_index)?;
            let output_size = reader.output_buffer_size().ok_or_else(|| {
                Error::LimitExceeded("Jupyter PNG output dimensions overflowed".into())
            })?;
            if output_size > MAX_IMAGE_DECODE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "Jupyter cell {} PNG output exceeds the decode buffer limit",
                    cell_index + 1
                )));
            }
            let mut decoded = vec![0; output_size];
            reader.next_frame(&mut decoded).map_err(|error| {
                Error::InvalidInput(format!(
                    "Jupyter cell {} PNG output is invalid: {error}",
                    cell_index + 1
                ))
            })?;
            Ok((width, height))
        }
        "image/jpeg" => {
            let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
            decoder.set_max_decoding_buffer_size(MAX_IMAGE_DECODE_BYTES);
            decoder.read_info().map_err(|error| {
                Error::InvalidInput(format!(
                    "Jupyter cell {} JPEG output is invalid: {error}",
                    cell_index + 1
                ))
            })?;
            let info = decoder.info().ok_or_else(|| {
                Error::InvalidInput(format!(
                    "Jupyter cell {} JPEG output has no image header",
                    cell_index + 1
                ))
            })?;
            let (width, height) = (u32::from(info.width), u32::from(info.height));
            check_image_pixels(width, height, cell_index)?;
            let decoded = decoder.decode().map_err(|error| {
                Error::InvalidInput(format!(
                    "Jupyter cell {} JPEG output is invalid: {error}",
                    cell_index + 1
                ))
            })?;
            if decoded.len() > MAX_IMAGE_DECODE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "Jupyter cell {} JPEG output exceeds the decode buffer limit",
                    cell_index + 1
                )));
            }
            Ok((width, height))
        }
        _ => Err(Error::Unsupported(format!(
            "Jupyter image MIME type '{mime}' is not supported"
        ))),
    }
}

fn check_image_pixels(width: u32, height: u32, cell_index: usize) -> Result<u64> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| Error::LimitExceeded("Jupyter image pixel count overflowed".into()))?;
    if pixels == 0 || pixels > MAX_IMAGE_PIXELS {
        return Err(Error::LimitExceeded(format!(
            "Jupyter cell {} image exceeds {MAX_IMAGE_PIXELS} pixels",
            cell_index + 1
        )));
    }
    Ok(pixels)
}

fn read_multiline(value: Option<&Value>, context: &str, cell_index: usize) -> Result<String> {
    let text = match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(lines)) => {
            let mut text = String::new();
            for line in lines {
                let line = line.as_str().ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "Jupyter cell {} {context} array contains a non-string value",
                        cell_index + 1
                    ))
                })?;
                text.push_str(line);
            }
            text
        }
        _ => {
            return Err(Error::InvalidInput(format!(
                "Jupyter cell {} has an invalid {context}",
                cell_index + 1
            )));
        }
    };
    Ok(safe_text(&text))
}

fn safe_text(text: &str) -> String {
    text.chars()
        .filter(|character| {
            *character == '\n'
                || *character == '\r'
                || *character == '\t'
                || !character.is_control()
        })
        .collect()
}

fn parse_markdown_content(
    text: &str,
    budget: &mut RenderBudget,
    inline_images: &HashMap<String, InlineHtmlImage>,
) -> Result<Vec<HtmlBlock>> {
    budget.add_markdown_lines(text.lines().count())?;
    parse_markdown_blocks_with_images(text, inline_images).map(|(blocks, _)| blocks)
}

fn read_cell_attachments(
    value: Option<&Value>,
    budget: &mut RenderBudget,
    warnings: &mut Vec<String>,
    cell_index: usize,
) -> Result<HashMap<String, InlineHtmlImage>> {
    let Some(attachments) = value.and_then(Value::as_object) else {
        return Ok(HashMap::new());
    };
    let mut images = HashMap::new();
    for (name, mime_bundle) in attachments {
        let Some(bundle) = mime_bundle.as_object() else {
            continue;
        };
        let mut added = false;
        for mime in ["image/png", "image/jpeg"] {
            let Some(encoded) = bundle.get(mime) else {
                continue;
            };
            match read_image(encoded, mime, budget, cell_index) {
                Ok(HtmlBlock::Image {
                    href,
                    pixel_width,
                    pixel_height,
                    ..
                }) => {
                    let image = InlineHtmlImage {
                        href,
                        pixel_width,
                        pixel_height,
                    };
                    images.insert(format!("attachment:{name}"), image.clone());
                    images.insert(name.clone(), image);
                    added = true;
                }
                Ok(_) => {}
                Err(_) => push_warning_once(
                    warnings,
                    "malformed Jupyter Markdown attachment images were omitted",
                ),
            }
            break;
        }
        if !added && !bundle.is_empty() {
            push_warning_once(
                warnings,
                "unsupported Jupyter Markdown attachment types were omitted; only PNG and JPEG are embedded",
            );
        }
    }
    if !images.is_empty() {
        push_warning_once(
            warnings,
            "Jupyter Markdown attachment images are rendered as centered flow blocks; inline placement is approximated",
        );
    }
    Ok(images)
}

fn push_block(
    blocks: &mut Vec<HtmlBlock>,
    budget: &mut RenderBudget,
    block: HtmlBlock,
) -> Result<()> {
    budget.add_blocks(1)?;
    blocks.push(block);
    Ok(())
}

fn extend_blocks(
    blocks: &mut Vec<HtmlBlock>,
    budget: &mut RenderBudget,
    added: Vec<HtmlBlock>,
) -> Result<()> {
    budget.add_blocks(added.len())?;
    blocks.extend(added);
    Ok(())
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_string());
    }
}

impl RenderBudget {
    fn add_text(&mut self, text: &str) -> Result<()> {
        self.text_bytes = self
            .text_bytes
            .checked_add(text.len())
            .ok_or_else(|| Error::LimitExceeded("Jupyter rendered text size overflowed".into()))?;
        if self.text_bytes > MAX_RENDERED_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Jupyter rendered text exceeds {MAX_RENDERED_TEXT_BYTES} bytes"
            )));
        }
        Ok(())
    }

    fn add_blocks(&mut self, count: usize) -> Result<()> {
        self.blocks = self
            .blocks
            .checked_add(count)
            .ok_or_else(|| Error::LimitExceeded("Jupyter block count overflowed".into()))?;
        if self.blocks > MAX_JUPYTER_BLOCKS {
            return Err(Error::LimitExceeded(format!(
                "Jupyter rendered output exceeds {MAX_JUPYTER_BLOCKS} blocks"
            )));
        }
        Ok(())
    }

    fn add_markdown_lines(&mut self, count: usize) -> Result<()> {
        self.markdown_lines = self
            .markdown_lines
            .checked_add(count)
            .ok_or_else(|| Error::LimitExceeded("Jupyter Markdown line count overflowed".into()))?;
        if self.markdown_lines > MAX_MARKDOWN_LINES {
            return Err(Error::LimitExceeded(format!(
                "Jupyter Markdown content exceeds {MAX_MARKDOWN_LINES} lines"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{RenderBudget, parse_notebook};
    use crate::convert::ConvertOptions;
    use crate::document::html::HtmlBlock;
    use crate::error::Error;
    use base64::Engine;
    use serde_json::json;

    fn one_pixel_png_base64() -> String {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[128, 192, 255]).unwrap();
        }
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn renders_markdown_code_stream_errors_and_png_outputs() {
        let encoded_png = one_pixel_png_base64();
        let notebook = json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [
                {"cell_type":"markdown", "metadata":{}, "source":["# Overview\n", "A *notebook*.\n"]},
                {"cell_type":"code", "execution_count":3, "metadata":{}, "source":["print(1)"], "outputs":[
                    {"output_type":"stream", "name":"stdout", "text":["1", "\n"]},
                    {"output_type":"display_data", "data":{"image/png":encoded_png, "text/plain":"fallback"}, "metadata":{}},
                    {"output_type":"error", "ename":"ValueError", "evalue":"bad value", "traceback":["Traceback", "ValueError: bad value"]}
                ]}
            ]
        });
        let (blocks, warnings) = parse_notebook(&notebook, &ConvertOptions::default()).unwrap();
        assert!(warnings.is_empty());
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::Heading { level: 1, text } if text == "Overview"
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::CodeBlock { text } if text == "print(1)"
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::Image { href, pixel_width: 1, pixel_height: 1, .. }
                if href.starts_with("data:image/png;base64,")
        )));
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::CodeBlock { text } if text.contains("ValueError: bad value")
        )));
    }

    #[test]
    fn renders_markdown_attachment_png_as_a_bounded_image() {
        let encoded = one_pixel_png_base64();
        let notebook = json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "metadata": {},
            "cells": [{
                "cell_type": "markdown",
                "metadata": {},
                "source": ["# Attachment\n", "![attached](attachment:image.png)"],
                "attachments": {"image.png": {"image/png": encoded}}
            }]
        });
        let (blocks, warnings) = parse_notebook(&notebook, &ConvertOptions::default()).unwrap();
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::Image { href, pixel_width: 1, pixel_height: 1, .. }
                if href.starts_with("data:image/png;base64,")
        )));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("attachment images"))
        );
    }

    #[test]
    fn respects_hidden_cell_metadata_and_warns_on_unsupported_outputs() {
        let notebook = json!({
            "nbformat": 4,
            "nbformat_minor": 6,
            "metadata": {},
            "cells": [
                {"cell_type":"code", "execution_count":null, "metadata":{"jupyter":{"source_hidden":true,"outputs_hidden":true}}, "source":["secret"], "outputs":[{"output_type":"display_data","data":{"application/vnd.widget-view+json":{"version_major":2}},"metadata":{}}]},
                {"cell_type":"mystery", "metadata":{}, "source":["unknown"]}
            ]
        });
        let (blocks, warnings) = parse_notebook(&notebook, &ConvertOptions::default()).unwrap();
        assert!(!blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::CodeBlock { text } if text.contains("secret")
        )));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("minor version 6"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("unsupported type"))
        );
    }

    #[test]
    fn rejects_unsupported_notebook_versions_and_limits_rendered_text() {
        let invalid = json!({"nbformat":3,"cells":[]});
        assert!(matches!(
            parse_notebook(&invalid, &ConvertOptions::default()),
            Err(Error::Unsupported(_))
        ));
        let mut budget = RenderBudget::default();
        let too_much = "x".repeat(super::MAX_RENDERED_TEXT_BYTES + 1);
        assert!(matches!(
            budget.add_text(&too_much),
            Err(Error::LimitExceeded(_))
        ));
    }
}
