use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{
    HtmlBlock, load_local_image_sources, render_blocks_to_pages_with_warnings,
};
use crate::document::markdown::{
    collect_markdown_image_sources, expand_markdown_reference_images,
    parse_markdown_blocks_with_images,
};
use crate::error::{Error, Result};

const MAX_QUARTO_DOCUMENT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_QUARTO_LINES: usize = 500_000;
const MAX_QUARTO_LINE_BYTES: usize = 1024 * 1024;
const MAX_QUARTO_FRONTMATTER_BYTES: usize = 1024 * 1024;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_QUARTO_DOCUMENT_BYTES);
    let mut bytes = Vec::new();
    Read::take(File::open(path)?, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "Quarto/R Markdown input exceeds maximum bytes ({max_bytes})"
        )));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!(
            "Quarto/R Markdown file is not valid UTF-8: {error}"
        ))
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let expanded_text = expand_markdown_reference_images(text, max_bytes)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_dir = fs::canonicalize(parent)?;
    let (sources, source_limit_exceeded) = collect_markdown_image_sources(&expanded_text);
    let (inline_images, mut warnings) =
        load_local_image_sources(&base_dir, sources, source_limit_exceeded)?;
    let (blocks, parser_warnings) = parse_document_with_images(&expanded_text, &inline_images)?;
    warnings.extend(parser_warnings);
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

fn parse_document_with_images(
    text: &str,
    inline_images: &std::collections::HashMap<String, crate::document::html::InlineHtmlImage>,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    if text.lines().count() > MAX_QUARTO_LINES {
        return Err(Error::LimitExceeded(format!(
            "Quarto/R Markdown input exceeds {MAX_QUARTO_LINES} lines"
        )));
    }
    if text.lines().any(|line| line.len() > MAX_QUARTO_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "Quarto/R Markdown line exceeds {MAX_QUARTO_LINE_BYTES} bytes"
        )));
    }

    let mut blocks = Vec::new();
    let mut warnings = vec![
        "Quarto/R Markdown code chunks and inline expressions are shown as source and are not executed; generated results, filters, citations, cross-references, and includes are not evaluated".into(),
    ];
    let body = if let Some((frontmatter, body)) = split_frontmatter(text) {
        if frontmatter.len() > MAX_QUARTO_FRONTMATTER_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Quarto/R Markdown YAML front matter exceeds {MAX_QUARTO_FRONTMATTER_BYTES} bytes"
            )));
        }
        let metadata = read_simple_frontmatter(frontmatter);
        if let Some(title) = metadata.title {
            blocks.push(HtmlBlock::Heading {
                level: 1,
                text: title,
            });
        }
        let byline = [metadata.author, metadata.date]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if !byline.is_empty() {
            blocks.push(HtmlBlock::Paragraph {
                text: byline.join(" · "),
            });
        }
        body
    } else {
        warnings.push(
            "Quarto/R Markdown file has no closed YAML front-matter block; content was parsed as Markdown".into(),
        );
        text
    };

    let (markdown_blocks, markdown_warnings) =
        parse_markdown_blocks_with_images(&safe_text(body), inline_images)?;
    blocks.extend(markdown_blocks);
    warnings.extend(markdown_warnings);
    Ok((blocks, warnings))
}

struct FrontmatterSummary {
    title: Option<String>,
    author: Option<String>,
    date: Option<String>,
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let first_end = text.find('\n').map_or(text.len(), |index| index + 1);
    let first_line = text[..first_end].trim_end_matches(['\r', '\n']).trim();
    if first_line != "---" {
        return None;
    }

    let mut offset = first_end;
    for line in text[first_end..].split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']).trim();
        let next_offset = offset.checked_add(line.len())?;
        if content == "---" || content == "..." {
            return Some((&text[first_end..offset], &text[next_offset..]));
        }
        offset = next_offset;
    }
    None
}

fn read_simple_frontmatter(frontmatter: &str) -> FrontmatterSummary {
    let mut summary = FrontmatterSummary {
        title: None,
        author: None,
        date: None,
    };
    for line in frontmatter.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Some(value) = parse_simple_yaml_scalar(value.trim()) else {
            continue;
        };
        match key.trim() {
            "title" => summary.title = Some(value),
            "author" => summary.author = Some(value),
            "date" => summary.date = Some(value),
            _ => {}
        }
    }
    summary
}

fn parse_simple_yaml_scalar(value: &str) -> Option<String> {
    if value.is_empty() || matches!(value, "|" | ">" | "[]" | "{}") {
        return None;
    }
    if value.starts_with('[') || value.starts_with('{') {
        return None;
    }
    let value = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value.split(" #").next().unwrap_or(value).trim()
    };
    let value = safe_text(value.trim());
    (!value.is_empty()).then_some(value)
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

#[cfg(test)]
mod tests {
    use super::parse_document_with_images;
    use crate::document::html::HtmlBlock;
    use std::collections::HashMap;

    #[test]
    fn parses_simple_yaml_frontmatter_and_code_chunks_as_source() {
        let source = "---\ntitle: \"Analysis Report\"\nauthor: Ada Lovelace\ndate: 2026-09-13\nexecute:\n  echo: false\n---\n\n# Findings\nThe result is inline `r value`.\n\n```{r, echo=FALSE}\n1 + 1\n```\n";
        let (blocks, warnings) = parse_document_with_images(source, &HashMap::new()).unwrap();
        assert!(matches!(
            &blocks[0],
            HtmlBlock::Heading { level: 1, text } if text == "Analysis Report"
        ));
        assert!(matches!(
            &blocks[1],
            HtmlBlock::Paragraph { text } if text == "Ada Lovelace · 2026-09-13"
        ));
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::CodeBlock { text } if text.contains("1 + 1")
        )));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("are not executed"))
        );
    }

    #[test]
    fn leaves_unclosed_frontmatter_as_markdown_and_enforces_line_limits() {
        let (blocks, warnings) =
            parse_document_with_images("---\ntitle: No closing delimiter\n", &HashMap::new())
                .unwrap();
        assert!(!blocks.is_empty());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("no closed YAML"))
        );
        let too_long = format!("{}\n", "x".repeat(super::MAX_QUARTO_LINE_BYTES + 1));
        assert!(matches!(
            parse_document_with_images(&too_long, &HashMap::new()),
            Err(crate::error::Error::LimitExceeded(_))
        ));
    }
}
