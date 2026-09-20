//! Bounded Microsoft Office 2003 XML Spreadsheet (SpreadsheetML/XMLSS) previews.
//!
//! XMLSS workbooks contain Workbook/Worksheet/Table/Row/Cell/Data elements.
//! This adapter renders cached cell values and formula presence without
//! evaluating formulas, macros, external links or workbook actions.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XMLSS_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XMLSS_XML_EVENTS: usize = 1_000_000;
const MAX_XMLSS_XML_NODES: usize = 500_000;
const MAX_XMLSS_XML_DEPTH: usize = 96;
const MAX_XMLSS_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_XMLSS_ROWS: usize = 200_000;
const MAX_XMLSS_DISPLAY_BYTES: usize = 512;
const XMLSS_NAMESPACE: &str = "urn:schemas-microsoft-com:office:spreadsheet";

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"Workbook", None)
        && String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains(XMLSS_NAMESPACE)
}

struct XmlssPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for XmlssPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "spreadsheetml".into();
        if page.title.is_empty() {
            page.title = "SpreadsheetML 2003 workbook".into();
        }
        page.description = "Microsoft Office 2003 XML Spreadsheet values are rendered as a bounded inert table; formulas, macros and external links are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    sheets: usize,
    rows: usize,
    cells: usize,
    formulas: usize,
    rows_out: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XMLSS_BYTES),
        "SpreadsheetML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XMLSS_XML_EVENTS),
            max_nodes: MAX_XMLSS_XML_NODES,
            max_depth: MAX_XMLSS_XML_DEPTH,
            max_text_bytes: MAX_XMLSS_TEXT_BYTES,
        },
        "SpreadsheetML",
    )?;
    if !root.name.eq_ignore_ascii_case("Workbook") {
        return Err(Error::InvalidInput(
            "SpreadsheetML root must Workbook".into(),
        ));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| namespace != XMLSS_NAMESPACE)
    {
        return Err(Error::InvalidInput(
            "SpreadsheetML namespace is missing or unsupported".into(),
        ));
    }
    let worksheets: Vec<&XmlElement> = root
        .children
        .iter()
        .filter(|child| child.name.eq_ignore_ascii_case("Worksheet"))
        .collect();
    let mut summary = Summary {
        sheets: worksheets.len(),
        ..Summary::default()
    };
    for worksheet in worksheets {
        let sheet_name = attr_local(worksheet, "Name")
            .unwrap_or("[unnamed sheet]")
            .to_owned();
        for (row_index, row) in worksheet
            .children
            .iter()
            .filter(|child| child.name.eq_ignore_ascii_case("Table"))
            .flat_map(|table| {
                table
                    .children
                    .iter()
                    .filter(|child| child.name.eq_ignore_ascii_case("Row"))
            })
            .enumerate()
        {
            summary.rows = summary.rows.saturating_add(1);
            for (cell_index, cell) in row
                .children
                .iter()
                .filter(|child| child.name.eq_ignore_ascii_case("Cell"))
                .enumerate()
            {
                if summary.rows_out.len() >= MAX_XMLSS_ROWS {
                    return Err(Error::LimitExceeded(format!(
                        "SpreadsheetML rows exceed {MAX_XMLSS_ROWS}"
                    )));
                }
                summary.cells = summary.cells.saturating_add(1);
                let data = cell
                    .children
                    .iter()
                    .find(|child| child.name.eq_ignore_ascii_case("Data"));
                let value = data
                    .map(|node| safe_text(&text_content(node)))
                    .unwrap_or_else(|| "-".into());
                let data_type = data
                    .and_then(|node| attr_local(node, "Type"))
                    .unwrap_or("unknown");
                let formula = attr_local(cell, "Formula")
                    .map(|_| "[formula omitted]")
                    .unwrap_or("-");
                if formula != "-" {
                    summary.formulas = summary.formulas.saturating_add(1);
                }
                summary.rows_out.push(vec![
                    sheet_name.clone(),
                    format!("R{}C{}", row_index + 1, cell_index + 1),
                    data_type.to_owned(),
                    value,
                    formula.into(),
                ]);
            }
        }
    }
    if summary.sheets == 0 || summary.cells == 0 {
        return Err(Error::InvalidInput(
            "SpreadsheetML workbook contains no Worksheet cells".into(),
        ));
    }
    let metadata = format!(
        "Worksheets: {}\nRows: {}\nCells: {}\nFormula cells: {}",
        summary.sheets, summary.rows, summary.cells, summary.formulas
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "SpreadsheetML 2003 workbook".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Sheet".into(),
                "Cell".into(),
                "Type".into(),
                "Value".into(),
                "Formula".into(),
            ],
            rows: summary.rows_out,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "SpreadsheetML Worksheet/Table/Row/Cell/Data values and formula presence are shown; styles, macros, named ranges, external links, drawings and workbook metadata are omitted or redacted".into(),
        "SpreadsheetML XML traversal and rows are bounded; DTD/entities, formula evaluation, external-resource access, scripts and Office application operations never run".into(),
    ];
    let mut page_sink = XmlssPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
}
fn text_content(element: &XmlElement) -> String {
    let mut parts = Vec::new();
    if !element.text.trim().is_empty() {
        parts.push(element.text.trim().to_owned());
    }
    for child in &element.children {
        let value = text_content(child);
        if !value.is_empty() {
            parts.push(value);
        }
    }
    parts.join(" ")
}
fn safe_text(value: &str) -> String {
    if value.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(value.trim())
    }
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_XMLSS_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_XMLSS_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;
    #[test]
    fn recognizes_xmlss_namespace() {
        assert!(looks_like_prefix(br#"<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet/></Workbook>"#));
    }
    #[test]
    fn rejects_generic_workbook() {
        assert!(!looks_like_prefix(br#"<Workbook><Worksheet/></Workbook>"#));
    }
}
