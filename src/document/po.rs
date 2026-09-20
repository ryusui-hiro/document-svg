//! Bounded GNU gettext PO/POT translation catalog preview.
//!
//! Reads message identifiers, contexts, plural translations, and translator
//! notes as data. Format directives in translated strings are never executed.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};

const MAX_PO_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PO_LINES: usize = 1_000_000;
const MAX_PO_LINE_BYTES: usize = 1024 * 1024;
const MAX_PO_ENTRIES: usize = 100_000;
const MAX_PO_TRANSLATIONS_PER_ENTRY: usize = 32;
const MAX_PO_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PO_COMMENT_BYTES: usize = 1024;

#[derive(Clone, Copy)]
enum Field {
    Context,
    Id,
    PluralId,
    Translation(usize),
}

#[derive(Default)]
struct Entry {
    context: Option<String>,
    id: Option<String>,
    plural_id: Option<String>,
    translations: BTreeMap<usize, String>,
    comments: Vec<String>,
    fuzzy: bool,
    current_field: Option<Field>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_PO_BYTES),
        "GNU gettext PO input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("gettext PO file is not valid UTF-8: {error}"))
    })?;
    let (blocks, warnings) = parse_po_blocks_with_warnings(&text)?;
    let mut page_sink = PoPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

struct PoPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for PoPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "po".into();
        if page.title.is_empty() {
            page.title = "gettext catalog".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub fn parse_po_blocks(text: &str) -> Result<Vec<HtmlBlock>> {
    parse_po_blocks_with_warnings(text).map(|(blocks, _)| blocks)
}

pub(crate) fn looks_like_po_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let mut has_id = false;
    let mut has_translation = false;
    for line in text.lines().take(128).map(str::trim_start) {
        if line.starts_with("msgid ") {
            has_id = true;
        } else if line.starts_with("msgstr ") || line.starts_with("msgstr[") {
            has_translation = true;
        }
    }
    has_id && has_translation
}

fn parse_po_blocks_with_warnings(text: &str) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    validate_po(text)?;
    let mut warnings = Vec::new();
    let mut current = Entry::default();
    let mut rows = Vec::<Vec<String>>::new();
    let mut header = None::<String>;
    let mut current_bytes = 0usize;
    let mut total_text_bytes = 0usize;
    let mut entries = 0usize;
    let mut obsolete = false;
    let mut obsolete_seen = false;
    let mut fuzzy_seen = false;
    let mut skipped_line_seen = false;

    for raw_line in text.lines() {
        let line = raw_line.trim_start().trim_end_matches('\r');
        if line.is_empty() {
            if obsolete {
                obsolete = false;
            } else if current.id.is_some() {
                finish_entry(
                    &mut current,
                    &mut rows,
                    &mut header,
                    &mut entries,
                    &mut fuzzy_seen,
                    &mut current_bytes,
                )?;
            }
            current.current_field = None;
            continue;
        }
        if line.starts_with("#~") {
            if !obsolete && current.id.is_some() {
                finish_entry(
                    &mut current,
                    &mut rows,
                    &mut header,
                    &mut entries,
                    &mut fuzzy_seen,
                    &mut current_bytes,
                )?;
            }
            obsolete = true;
            obsolete_seen = true;
            current = Entry::default();
            continue;
        }
        if obsolete {
            obsolete = false;
            current = Entry::default();
        }
        if line.starts_with('#') {
            if current.id.is_some() {
                finish_entry(
                    &mut current,
                    &mut rows,
                    &mut header,
                    &mut entries,
                    &mut fuzzy_seen,
                    &mut current_bytes,
                )?;
            }
            let comment = if let Some(flags) = line.strip_prefix("#,") {
                current.fuzzy |= flags
                    .split(',')
                    .any(|flag| flag.trim().eq_ignore_ascii_case("fuzzy"));
                Some(flags.trim())
            } else {
                line.strip_prefix("#.")
                    .or_else(|| line.strip_prefix("#:"))
                    .or_else(|| line.strip_prefix("# "))
                    .map(str::trim)
            };
            if let Some(comment) = comment {
                let comment = limit_comment(comment);
                current_bytes = current_bytes.saturating_add(comment.len());
                total_text_bytes = total_text_bytes.saturating_add(comment.len());
                current.comments.push(comment);
                enforce_po_text_limit(total_text_bytes)?;
            }
            if current_bytes > MAX_PO_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "gettext PO entry text exceeds {MAX_PO_TEXT_BYTES} bytes"
                )));
            }
            continue;
        }

        if let Some((field, value)) = parse_po_field(line)? {
            if matches!(field, Field::Id | Field::Context) && current.id.is_some() {
                finish_entry(
                    &mut current,
                    &mut rows,
                    &mut header,
                    &mut entries,
                    &mut fuzzy_seen,
                    &mut current_bytes,
                )?;
            }
            if matches!(field, Field::PluralId | Field::Translation(_)) && current.id.is_none() {
                skipped_line_seen = true;
                continue;
            }
            total_text_bytes = total_text_bytes.saturating_add(value.len());
            current_bytes = current_bytes.saturating_add(value.len());
            enforce_po_text_limit(total_text_bytes)?;
            if current_bytes > MAX_PO_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "gettext PO entry text exceeds {MAX_PO_TEXT_BYTES} bytes"
                )));
            }
            set_field(&mut current, field, value);
            current.current_field = Some(field);
            continue;
        }

        if line.starts_with('"') {
            let value = parse_po_string(line)?;
            total_text_bytes = total_text_bytes.saturating_add(value.len());
            current_bytes = current_bytes.saturating_add(value.len());
            enforce_po_text_limit(total_text_bytes)?;
            if current_bytes > MAX_PO_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "gettext PO entry text exceeds {MAX_PO_TEXT_BYTES} bytes"
                )));
            }
            if let Some(field) = current.current_field {
                append_field(&mut current, field, value);
            } else {
                skipped_line_seen = true;
            }
            continue;
        }
        skipped_line_seen = true;
    }
    if current.id.is_some() {
        finish_entry(
            &mut current,
            &mut rows,
            &mut header,
            &mut entries,
            &mut fuzzy_seen,
            &mut current_bytes,
        )?;
    }

    if obsolete_seen {
        push_warning(&mut warnings, "obsolete gettext entries were omitted");
    }
    if fuzzy_seen {
        push_warning(
            &mut warnings,
            "fuzzy gettext translations were marked for review",
        );
    }
    if skipped_line_seen {
        push_warning(
            &mut warnings,
            "unrecognized or orphaned gettext PO lines were omitted",
        );
    }

    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: "Gettext catalog".into(),
    }];
    if let Some(header) = header {
        for key in ["Project-Id-Version:", "Language:", "PO-Revision-Date:"] {
            if let Some(value) = header
                .lines()
                .find_map(|line| line.strip_prefix(key).map(str::trim))
                .filter(|value| !value.is_empty())
            {
                blocks.push(HtmlBlock::Paragraph {
                    text: format!("{} {}", key.trim_end_matches(':'), value),
                });
            }
        }
    }
    for row in rows {
        let Some(source) = row.first() else { continue };
        let translation = row.get(1).map(String::as_str).unwrap_or_default();
        let notes = row.get(2).map(String::as_str).unwrap_or_default();
        let mut text = format!("Source: {source}\nTranslation: {translation}");
        if !notes.is_empty() {
            text.push_str("\nNotes: ");
            text.push_str(notes);
        }
        blocks.push(HtmlBlock::Paragraph { text });
    }
    Ok((blocks, warnings))
}

fn validate_po(text: &str) -> Result<()> {
    if text.len() as u64 > MAX_PO_BYTES {
        return Err(Error::LimitExceeded(format!(
            "gettext PO input exceeds {MAX_PO_BYTES} bytes"
        )));
    }
    let mut lines = 0usize;
    for line in text.lines() {
        lines += 1;
        if lines > MAX_PO_LINES {
            return Err(Error::LimitExceeded(format!(
                "gettext PO input exceeds {MAX_PO_LINES} lines"
            )));
        }
        if line.len() > MAX_PO_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "gettext PO line exceeds {MAX_PO_LINE_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

fn parse_po_field(line: &str) -> Result<Option<(Field, String)>> {
    let (field, rest) = if let Some(rest) = line.strip_prefix("msgctxt ") {
        (Field::Context, rest)
    } else if let Some(rest) = line.strip_prefix("msgid_plural ") {
        (Field::PluralId, rest)
    } else if let Some(rest) = line.strip_prefix("msgid ") {
        (Field::Id, rest)
    } else if let Some(rest) = line.strip_prefix("msgstr ") {
        (Field::Translation(0), rest)
    } else if let Some(rest) = line.strip_prefix("msgstr[") {
        let closing = rest.find(']').ok_or_else(|| {
            Error::InvalidInput("gettext PO plural translation is missing a closing bracket".into())
        })?;
        let index = rest[..closing].parse::<usize>().map_err(|_| {
            Error::InvalidInput("gettext PO plural translation index is invalid".into())
        })?;
        if index >= MAX_PO_TRANSLATIONS_PER_ENTRY {
            return Err(Error::LimitExceeded(format!(
                "gettext PO plural index exceeds {MAX_PO_TRANSLATIONS_PER_ENTRY} forms"
            )));
        }
        let after = rest[closing + 1..].trim_start();
        let after = after.strip_prefix(' ').unwrap_or(after);
        (Field::Translation(index), after)
    } else {
        return Ok(None);
    };
    Ok(Some((field, parse_po_string(rest)?)))
}

fn parse_po_string(value: &str) -> Result<String> {
    let value = value.trim_start();
    let mut chars = value
        .strip_prefix('"')
        .ok_or_else(|| {
            Error::InvalidInput("gettext PO string value must start with a quote".into())
        })?
        .chars()
        .peekable();
    let mut output = String::new();
    let mut closed = false;
    while let Some(character) = chars.next() {
        if character == '"' {
            closed = true;
            if chars.any(|remaining| !remaining.is_whitespace()) {
                return Err(Error::InvalidInput(
                    "unexpected text follows a gettext PO string value".into(),
                ));
            }
            break;
        }
        if character != '\\' {
            push_safe_po_char(&mut output, character);
            continue;
        }
        let escaped = chars.next().ok_or_else(|| {
            Error::InvalidInput("gettext PO string ends with an incomplete escape".into())
        })?;
        match escaped {
            'a' | 'b' | 'f' | 'v' => output.push('\u{fffd}'),
            'n' => output.push('\n'),
            'r' => output.push('\n'),
            't' => output.push('\t'),
            '\\' => output.push('\\'),
            '"' => output.push('"'),
            '\'' | '?' => output.push(escaped),
            'x' => {
                let mut value = 0u32;
                let mut digits = 0usize;
                while let Some(next) = chars.peek().copied().and_then(|next| next.to_digit(16)) {
                    chars.next();
                    value = value
                        .checked_mul(16)
                        .and_then(|value| value.checked_add(next))
                        .ok_or_else(|| {
                            Error::InvalidInput("gettext PO hexadecimal escape overflowed".into())
                        })?;
                    digits += 1;
                }
                if digits == 0 {
                    return Err(Error::InvalidInput(
                        "gettext PO hexadecimal escape has no digits".into(),
                    ));
                }
                let character = char::from_u32(value).ok_or_else(|| {
                    Error::InvalidInput(
                        "gettext PO hexadecimal escape is not a Unicode scalar".into(),
                    )
                })?;
                push_safe_po_char(&mut output, character);
            }
            '0'..='7' => {
                let mut value = u32::from(escaped as u8 - b'0');
                for _ in 0..2 {
                    let Some(next) = chars
                        .peek()
                        .copied()
                        .filter(|next| matches!(next, '0'..='7'))
                    else {
                        break;
                    };
                    chars.next();
                    value = value * 8 + u32::from(next as u8 - b'0');
                }
                let character = char::from_u32(value).ok_or_else(|| {
                    Error::InvalidInput("gettext PO octal escape is not a Unicode scalar".into())
                })?;
                push_safe_po_char(&mut output, character);
            }
            'u' | 'U' => {
                return Err(Error::InvalidInput(
                    "GNU gettext PO strings do not allow universal character escapes".into(),
                ));
            }
            _ => {
                return Err(Error::InvalidInput(format!(
                    "unsupported gettext PO escape \\{escaped}"
                )));
            }
        }
    }
    if !closed {
        return Err(Error::InvalidInput(
            "gettext PO string has no closing quote".into(),
        ));
    }
    Ok(output)
}

fn push_safe_po_char(output: &mut String, character: char) {
    if !character.is_control() || matches!(character, '\n' | '\r' | '\t') {
        output.push(character);
    } else {
        output.push('\u{fffd}');
    }
}

fn set_field(entry: &mut Entry, field: Field, value: String) {
    match field {
        Field::Context => entry.context = Some(value),
        Field::Id => entry.id = Some(value),
        Field::PluralId => entry.plural_id = Some(value),
        Field::Translation(index) => {
            entry.translations.insert(index, value);
        }
    }
}

fn append_field(entry: &mut Entry, field: Field, value: String) {
    match field {
        Field::Context => entry.context.get_or_insert_default().push_str(&value),
        Field::Id => entry.id.get_or_insert_default().push_str(&value),
        Field::PluralId => entry.plural_id.get_or_insert_default().push_str(&value),
        Field::Translation(index) => entry
            .translations
            .entry(index)
            .or_default()
            .push_str(&value),
    }
}

fn finish_entry(
    entry: &mut Entry,
    rows: &mut Vec<Vec<String>>,
    header: &mut Option<String>,
    entry_count: &mut usize,
    fuzzy_seen: &mut bool,
    current_bytes: &mut usize,
) -> Result<()> {
    let Some(id) = entry.id.take() else {
        *entry = Entry::default();
        *current_bytes = 0;
        return Ok(());
    };
    if id.is_empty() {
        *header = entry.translations.get(&0).cloned();
        *entry = Entry::default();
        *current_bytes = 0;
        return Ok(());
    }
    *entry_count = entry_count.saturating_add(1);
    if *entry_count > MAX_PO_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "gettext PO catalog exceeds {MAX_PO_ENTRIES} entries"
        )));
    }
    *fuzzy_seen |= entry.fuzzy;
    let plural = entry.plural_id.take();
    let is_plural = plural.is_some();
    let source = plural.map_or(id.clone(), |plural| format!("{id}\n{plural}"));
    let translation = if entry.translations.is_empty() {
        "[untranslated]".into()
    } else {
        let forms = entry
            .translations
            .iter()
            .map(|(index, value)| {
                let value = if value.is_empty() {
                    "[untranslated]"
                } else {
                    value
                };
                if entry.translations.len() > 1 || is_plural {
                    format!("[{index}] {value}")
                } else {
                    value.to_owned()
                }
            })
            .collect::<Vec<_>>();
        forms.join("\n")
    };
    let translation = if entry.fuzzy {
        format!("[fuzzy] {translation}")
    } else {
        translation
    };
    let source = match entry.context.take().filter(|context| !context.is_empty()) {
        Some(context) => format!("[{context}] {source}"),
        None => source,
    };
    rows.push(vec![source, translation, entry.comments.join("\n")]);
    *entry = Entry::default();
    *current_bytes = 0;
    Ok(())
}

fn limit_comment(comment: &str) -> String {
    let mut output = String::new();
    for character in comment.chars() {
        if output.len().saturating_add(character.len_utf8()) > MAX_PO_COMMENT_BYTES {
            break;
        }
        output.push(character);
    }
    output
}

fn enforce_po_text_limit(total: usize) -> Result<()> {
    if total > MAX_PO_TEXT_BYTES {
        Err(Error::LimitExceeded(format!(
            "gettext PO text exceeds {MAX_PO_TEXT_BYTES} bytes"
        )))
    } else {
        Ok(())
    }
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
    fn parses_context_plural_multiline_and_comment_fields() {
        let po = "msgid \"\"\nmsgstr \"\"\n\"Language: fr\\n\"\n\n#. Greeting\n#: ui.rs:3\nmsgctxt \"menu\"\nmsgid \"Welcome\"\nmsgstr \"Bienvenue\"\n\nmsgid \"One file\"\nmsgid_plural \"%d files\"\nmsgstr[0] \"Un fichier\"\nmsgstr[1] \"%d fichiers\"\n\nmsgid \"multi\"\nmsgstr \"first \"\n\"second\"\n";
        let (blocks, warnings) = parse_po_blocks_with_warnings(po).unwrap();
        assert!(warnings.is_empty());
        let serialized = format!("{blocks:?}");
        assert!(serialized.contains("Language fr"));
        assert!(serialized.contains("Welcome"));
        assert!(serialized.contains("Bienvenue"));
        assert!(serialized.contains("One file") && serialized.contains("%d files"));
        assert!(serialized.contains("[0] Un fichier"));
        assert!(serialized.contains("[1] %d fichiers"));
        assert!(serialized.contains("first second"));
        assert!(serialized.contains("Greeting"));
    }

    #[test]
    fn decodes_c_escapes_and_rejects_universal_escapes() {
        assert_eq!(
            parse_po_string("\"line\\nquote\\\"\\\\\"").unwrap(),
            "line\nquote\"\\"
        );
        assert!(parse_po_string("\"\\u1234\"").is_err());
    }

    #[test]
    fn ignores_orphan_translation_fields_instead_of_attaching_them_to_the_next_entry() {
        let po = "msgstr \"dangling\"\n\nmsgid \"active\"\nmsgstr \"ok\"\n";
        let (blocks, warnings) = parse_po_blocks_with_warnings(po).unwrap();
        assert!(warnings.iter().any(|warning| warning.contains("orphaned")));
        let serialized = format!("{blocks:?}");
        assert!(serialized.contains("active"));
        assert!(serialized.contains("ok"));
        assert!(!serialized.contains("dangling"));
    }
}
