//! Bounded Adobe Forms Data Format (FDF) previews.
//!
//! FDF carries PDF form field values without the original page content. This
//! adapter lists field hierarchy, field types, safe values and option counts;
//! actions, JavaScript, submit targets and embedded files remain inert.

use std::path::Path;

use lopdf::{Dictionary, Document, Object};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_FDF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FDF_OBJECTS: usize = 500_000;
const MAX_FDF_FIELDS: usize = 100_000;
const MAX_FDF_DEPTH: usize = 64;
const MAX_FDF_VALUE_BYTES: usize = 2 * 1024 * 1024;
const MAX_FDF_TOTAL_VALUE_BYTES: usize = 32 * 1024 * 1024;
const MAX_FDF_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    bytes.windows(5).any(|window| window == b"%FDF-")
}

struct FdfPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for FdfPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "fdf".into();
        if page.title.is_empty() {
            page.title = "FDF form data".into();
        }
        page.description =
            "FDF form fields are rendered as a bounded inert summary; actions, scripts, submit targets and embedded files are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    fields: usize,
    password_fields: usize,
    options: usize,
    actions: usize,
    attachments: usize,
    total_value_bytes: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_FDF_BYTES),
        "FDF input",
    )?;
    if !looks_like_prefix(&bytes) {
        return Err(Error::InvalidInput("FDF header must contain %FDF-".into()));
    }
    // FDF uses the PDF object/xref grammar but has a different magic header.
    // lopdf intentionally accepts only PDF headers, so normalize the fixed
    // eight-byte signature in the bounded in-memory buffer before parsing.
    if bytes.len() < 8 {
        return Err(Error::InvalidInput("FDF header is truncated".into()));
    }
    bytes[1..4].copy_from_slice(b"PDF");
    bytes[5..8].copy_from_slice(b"1.7");
    let document = Document::load_mem(&bytes)?;
    if document.objects.len() > MAX_FDF_OBJECTS {
        return Err(Error::LimitExceeded(format!(
            "FDF indirect objects exceed {MAX_FDF_OBJECTS}"
        )));
    }
    let root_id = match document.trailer.get(b"Root") {
        Ok(Object::Reference(id)) => *id,
        Ok(_) => {
            return Err(Error::InvalidInput(
                "FDF trailer Root is not a reference".into(),
            ));
        }
        Err(_) => {
            return Err(Error::InvalidInput(
                "FDF trailer has no Root reference".into(),
            ));
        }
    };
    let root = document.get_object(root_id)?.as_dict()?;
    let fdf_object = root
        .get(b"FDF")
        .map_err(|_| Error::InvalidInput("FDF catalog has no FDF dictionary".into()))?;
    let fdf = resolve_dict(fdf_object, &document)?;
    let fields_object = fdf
        .get(b"Fields")
        .map_err(|_| Error::InvalidInput("FDF dictionary has no Fields array".into()))?;
    let fields = resolve_array(fields_object, &document)?;

    let mut summary = Summary::default();
    if fields.len() > MAX_FDF_FIELDS {
        return Err(Error::LimitExceeded(format!(
            "FDF fields exceed {MAX_FDF_FIELDS}"
        )));
    }
    for field in fields {
        walk_field(field, "", 1, &document, &mut summary)?;
    }
    summary.actions = count_action_objects(&document)
        + usize::from(fdf.has(b"A") || fdf.has(b"AA") || fdf.has(b"JavaScript") || fdf.has(b"JS"));
    summary.attachments = count_named_objects(&document, b"EmbeddedFile");
    if summary.fields == 0 {
        return Err(Error::InvalidInput(
            "FDF Fields array contains no field dictionaries".into(),
        ));
    }

    let metadata = format!(
        "Fields: {}\nPassword fields: {}\nChoice options: {}\nAction dictionaries: {}\nEmbedded files: {}",
        summary.fields,
        summary.password_fields,
        summary.options,
        summary.actions,
        summary.attachments
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "FDF form data".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Field".into(),
                "Type".into(),
                "Value".into(),
                "Detail".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "FDF field hierarchy, types, safe values and option counts are shown; password values, actions, submit targets, JavaScript, file specifications, embedded files and external URLs are omitted or redacted".into(),
        "FDF parsing and rendered rows are bounded; no form submission, URI dereference, JavaScript, launch action, attachment extraction or PDF page rendering runs".into(),
    ];
    let mut page_sink = FdfPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn walk_field(
    object: &Object,
    parent_path: &str,
    depth: usize,
    document: &Document,
    summary: &mut Summary,
) -> Result<()> {
    if depth > MAX_FDF_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "FDF field depth exceeds {MAX_FDF_DEPTH}"
        )));
    }
    let dict = resolve_dict(object, document)?;
    if summary.fields >= MAX_FDF_FIELDS {
        return Err(Error::LimitExceeded(format!(
            "FDF fields exceed {MAX_FDF_FIELDS}"
        )));
    }
    summary.fields = summary.fields.saturating_add(1);
    let name = object_text(dict.get(b"T").ok(), document).unwrap_or_default();
    let path = if name.is_empty() {
        parent_path.to_owned()
    } else if parent_path.is_empty() {
        name.clone()
    } else {
        format!("{parent_path}.{name}")
    };
    let field_type =
        object_name(dict.get(b"FT").ok(), document).unwrap_or_else(|| "inherited/unknown".into());
    let flags = object_integer(dict.get(b"Ff").ok(), document).unwrap_or(0);
    let password = field_type.eq_ignore_ascii_case("PS") || (flags & (1 << 13)) != 0;
    if password {
        summary.password_fields = summary.password_fields.saturating_add(1);
    }
    let value = if password {
        "[password omitted]".into()
    } else {
        object_text(dict.get(b"V").ok(), document).unwrap_or_else(|| "-".into())
    };
    summary.total_value_bytes = summary.total_value_bytes.saturating_add(value.len());
    if summary.total_value_bytes > MAX_FDF_TOTAL_VALUE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "FDF field values exceed {MAX_FDF_TOTAL_VALUE_BYTES} bytes"
        )));
    }
    let option_count = dict
        .get(b"Opt")
        .ok()
        .and_then(|object| resolve_array(object, document).ok())
        .map(|values| values.len())
        .unwrap_or(0);
    summary.options = summary.options.saturating_add(option_count);
    let detail = if option_count > 0 {
        format!("depth={depth} options={option_count}")
    } else {
        format!("depth={depth}")
    };
    push_row(&mut summary.rows, &path, &field_type, &value, &detail)?;

    if let Some(kids) = dict
        .get(b"Kids")
        .ok()
        .and_then(|object| resolve_array(object, document).ok())
    {
        for child in kids {
            walk_field(child, &path, depth.saturating_add(1), document, summary)?;
        }
    }
    Ok(())
}

fn resolve_dict<'a>(object: &'a Object, document: &'a Document) -> Result<&'a Dictionary> {
    match object {
        Object::Dictionary(dict) => Ok(dict),
        Object::Reference(id) => Ok(document.get_object(*id)?.as_dict()?),
        _ => Err(Error::InvalidInput("FDF value is not a dictionary".into())),
    }
}

fn resolve_array<'a>(object: &'a Object, document: &'a Document) -> Result<Vec<&'a Object>> {
    let object = match object {
        Object::Array(array) => array,
        Object::Reference(id) => document.get_object(*id)?.as_array()?,
        _ => return Err(Error::InvalidInput("FDF value is not an array".into())),
    };
    Ok(object.iter().collect())
}

fn object_text(object: Option<&Object>, document: &Document) -> Option<String> {
    let object = object?;
    let object = match object {
        Object::Reference(id) => document.get_object(*id).ok()?,
        object => object,
    };
    let value = match object {
        Object::String(bytes, _) => String::from_utf8_lossy(bytes).into_owned(),
        Object::Name(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Object::Integer(value) => value.to_string(),
        Object::Real(value) => value.to_string(),
        Object::Boolean(value) => value.to_string(),
        Object::Array(values) => format!("[{} values]", values.len()),
        _ => return None,
    };
    if value.len() > MAX_FDF_VALUE_BYTES {
        Some("[value omitted: exceeds FDF value limit]".into())
    } else if value.contains("://") {
        Some("[URL omitted]".into())
    } else {
        Some(truncate(&value))
    }
}

fn object_name(object: Option<&Object>, document: &Document) -> Option<String> {
    object_text(object, document).map(|value| value.trim_start_matches('/').to_owned())
}

fn object_integer(object: Option<&Object>, document: &Document) -> Option<i64> {
    let object = object?;
    let object = match object {
        Object::Reference(id) => document.get_object(*id).ok()?,
        object => object,
    };
    match object {
        Object::Integer(value) => Some(*value),
        _ => None,
    }
}

fn count_action_objects(document: &Document) -> usize {
    document
        .objects
        .values()
        .filter(|object| match object {
            Object::Dictionary(dict) => {
                dict.has(b"A") || dict.has(b"AA") || dict.has(b"JavaScript") || dict.has(b"JS")
            }
            Object::Stream(stream) => {
                stream.dict.has(b"A")
                    || stream.dict.has(b"AA")
                    || stream.dict.has(b"JavaScript")
                    || stream.dict.has(b"JS")
            }
            _ => false,
        })
        .count()
}

fn count_named_objects(document: &Document, name: &[u8]) -> usize {
    document
        .objects
        .values()
        .filter(|object| match object {
            Object::Dictionary(dict) => {
                matches!(dict.get(b"Type"), Ok(Object::Name(value)) if value == name)
            }
            Object::Stream(stream) => {
                matches!(stream.dict.get(b"Type"), Ok(Object::Name(value)) if value == name)
            }
            _ => false,
        })
        .count()
}

fn push_row(
    rows: &mut Vec<Vec<String>>,
    field: &str,
    field_type: &str,
    value: &str,
    detail: &str,
) -> Result<()> {
    if rows.len() >= MAX_FDF_FIELDS {
        return Err(Error::LimitExceeded(format!(
            "FDF rendered rows exceed {MAX_FDF_FIELDS}"
        )));
    }
    rows.push(vec![
        truncate(field),
        truncate(field_type),
        truncate(value),
        truncate(detail),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_FDF_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_FDF_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;
    #[test]
    fn recognizes_fdf_header() {
        assert!(looks_like_prefix(b"%FDF-1.2\n"));
    }
    #[test]
    fn rejects_pdf_header() {
        assert!(!looks_like_prefix(b"%PDF-1.7\n"));
    }
}
