//! Bounded OpenSCAD source previews.
//!
//! OpenSCAD is an executable CAD language. This adapter deliberately treats
//! OpenSCAD files as inert source: it counts declarations, primitives,
//! transforms and file-reference statements without evaluating expressions or
//! opening files.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_OPENSCAD_BYTES: u64 = 128 * 1024 * 1024;
const MAX_OPENSCAD_LINES: usize = 2_000_000;
const MAX_OPENSCAD_LINE_BYTES: usize = 1024 * 1024;
const MAX_OPENSCAD_ROWS: usize = 100_000;
const MAX_OPENSCAD_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    lines: usize,
    modules: usize,
    functions: usize,
    primitives: usize,
    transforms: usize,
    booleans: usize,
    imports: usize,
    uses: usize,
    includes: usize,
    assignments: usize,
    definitions: Vec<String>,
    rows: Vec<Vec<String>>,
}

struct OpenScadPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OpenScadPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "openscad".into();
        if page.title.is_empty() {
            page.title = "OpenSCAD source".into();
        }
        page.description = "OpenSCAD source was scanned as inert CAD code; geometry, expressions, imports and external files were not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let call = [
        "cube(",
        "sphere(",
        "cylinder(",
        "translate(",
        "rotate(",
        "linear_extrude(",
        "import(",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    let declaration = text.lines().any(|line| {
        let line = line.trim_start();
        ["module ", "function ", "use <", "include <"]
            .iter()
            .any(|needle| line.starts_with(needle))
    });
    (call || declaration) && (text.contains('{') || text.contains(';'))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_OPENSCAD_BYTES),
        "OpenSCAD input",
    )?;
    let source = std::str::from_utf8(&bytes)
        .map_err(|error| Error::InvalidInput(format!("OpenSCAD input must be UTF-8: {error}")))?;
    let mut summary = parse_source(source)?;
    push_row(
        &mut summary.rows,
        "Source",
        "OpenSCAD",
        &format!(
            "lines={} assignments={}",
            summary.lines, summary.assignments
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Declarations",
        &summary.modules.to_string(),
        &format!(
            "functions={} definitions={}",
            summary.functions,
            summary.definitions.len()
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Geometry",
        &summary.primitives.to_string(),
        &format!(
            "transforms={} booleans={}",
            summary.transforms, summary.booleans
        ),
    )?;
    push_row(
        &mut summary.rows,
        "References",
        &summary.imports.to_string(),
        &format!("use={} include={}", summary.uses, summary.includes),
    )?;
    for definition in summary
        .definitions
        .iter()
        .take(MAX_OPENSCAD_ROWS.saturating_sub(summary.rows.len()))
    {
        push_row(
            &mut summary.rows,
            "Definition",
            definition,
            "name only; body not executed",
        )?;
    }
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "OpenSCAD source".into(),
        },
        HtmlBlock::Paragraph {
            text: "OpenSCAD is a programmable CAD language. This preview reports inert source structure and never evaluates it.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "OpenSCAD modules, functions, expressions, loops and geometry were not executed; the preview is not a rendered solid model".into(),
        "import(), use(), include(), surface() and file-like references were counted only; no referenced file, URL or library was opened".into(),
    ];
    let mut page_sink = OpenScadPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_source(source: &str) -> Result<Summary> {
    let mut summary = Summary::default();
    let mut block_comment = false;
    for (line_index, raw_line) in source.lines().enumerate() {
        if line_index >= MAX_OPENSCAD_LINES {
            return Err(Error::LimitExceeded(format!(
                "OpenSCAD lines exceed {MAX_OPENSCAD_LINES}"
            )));
        }
        if raw_line.len() > MAX_OPENSCAD_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "OpenSCAD line exceeds {MAX_OPENSCAD_LINE_BYTES} bytes"
            )));
        }
        summary.lines += 1;
        let line = strip_comments(raw_line, &mut block_comment);
        scan_line(&line, &mut summary);
    }
    if block_comment {
        return Err(Error::InvalidInput(
            "OpenSCAD source has an unterminated block comment".into(),
        ));
    }
    if summary.modules == 0
        && summary.functions == 0
        && summary.primitives == 0
        && summary.transforms == 0
        && summary.imports == 0
        && summary.uses == 0
        && summary.includes == 0
    {
        return Err(Error::InvalidInput(
            "OpenSCAD source contains no recognized language constructs".into(),
        ));
    }
    Ok(summary)
}

fn strip_comments(line: &str, block_comment: &mut bool) -> String {
    let mut output = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if *block_comment {
            if index + 1 < bytes.len() && bytes[index] == b'*' && bytes[index + 1] == b'/' {
                *block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
        } else if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'/' {
            break;
        } else if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'*' {
            *block_comment = true;
            index += 2;
        } else {
            output.push(bytes[index] as char);
            index += 1;
        }
    }
    output
}

fn scan_line(line: &str, summary: &mut Summary) {
    for keyword in [
        "cube",
        "sphere",
        "cylinder",
        "polyhedron",
        "polygon",
        "square",
        "circle",
        "text",
        "surface",
    ] {
        summary.primitives += occurrences_as_call(line, keyword);
    }
    for keyword in [
        "linear_extrude",
        "rotate_extrude",
        "translate",
        "rotate",
        "scale",
        "mirror",
        "multmatrix",
        "projection",
    ] {
        summary.transforms += occurrences_as_call(line, keyword);
    }
    for keyword in ["union", "difference", "intersection", "hull", "minkowski"] {
        summary.booleans += occurrences_as_call(line, keyword);
    }
    summary.modules += occurrences_as_call(line, "module");
    summary.functions += occurrences_as_call(line, "function");
    summary.imports += occurrences_as_call(line, "import");
    summary.uses += occurrences_as_call(line, "use");
    summary.includes += occurrences_as_call(line, "include");
    summary.assignments += line.matches('=').count();
    for keyword in ["module", "function"] {
        if occurrences_as_call(line, keyword) > 0
            && let Some(name) = definition_name(line, keyword)
            && summary.definitions.len() < MAX_OPENSCAD_ROWS
        {
            summary.definitions.push(name);
        }
    }
}

fn occurrences_as_call(line: &str, keyword: &str) -> usize {
    let mut count = 0;
    let mut offset = 0;
    while let Some(found) = line[offset..].find(keyword) {
        let start = offset + found;
        let before = line[..start].chars().next_back();
        let after = line[start + keyword.len()..].chars().next();
        if before.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_')
            && after.is_some_and(|ch| ch == '(' || ch.is_ascii_whitespace())
        {
            count += 1;
        }
        offset = start + keyword.len();
    }
    count
}

fn definition_name(line: &str, keyword: &str) -> Option<String> {
    let position = line.find(keyword)? + keyword.len();
    let rest = line[position..].trim_start();
    let end = rest.find(['(', '{', '='])?;
    let name = rest[..end].trim();
    (!name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
    .then(|| truncate(name))
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OPENSCAD_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_OPENSCAD_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_OPENSCAD_ROWS {
        return Err(Error::LimitExceeded(format!(
            "OpenSCAD rows exceed {MAX_OPENSCAD_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
