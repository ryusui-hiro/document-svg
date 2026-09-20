//! Bounded BibTeX `.bib` bibliography preview.
//!
//! Parses entries and nested/quoted field values as text. BibTeX macros,
//! preambles, citation styles, crossrefs, and TeX commands are never executed.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};

const MAX_BIB_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BIB_LINES: usize = 1_000_000;
const MAX_BIB_LINE_BYTES: usize = 1024 * 1024;
const MAX_BIB_ENTRIES: usize = 100_000;
const MAX_BIB_RECORDS: usize = 100_000;
const MAX_BIB_FIELDS_PER_ENTRY: usize = 128;
const MAX_BIB_KEY_BYTES: usize = 1024;
const MAX_BIB_FIELD_NAME_BYTES: usize = 128;
const MAX_BIB_FIELD_VALUE_BYTES: usize = 2 * 1024 * 1024;
const MAX_BIB_ENTRY_BYTES: usize = 4 * 1024 * 1024;
const MAX_BIB_NESTING: usize = 64;
const MAX_BIB_RENDERED_TEXT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
struct BibParser<'a> {
    text: &'a str,
    index: usize,
    entry_count: usize,
    records_processed: usize,
    rendered_bytes: usize,
    warnings: Vec<String>,
    blocks: Vec<HtmlBlock>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_BIB_BYTES),
        "BibTeX input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("BibTeX input is not valid UTF-8: {error}"))
    })?;
    let (blocks, warnings) = parse_bibtex_blocks_with_warnings(&text)?;
    let mut sink = BibPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut sink, options)?;
    Ok(warnings)
}

struct BibPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for BibPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "bib".into();
        if page.title.is_empty() {
            page.title = "BibTeX bibliography".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub fn parse_bibtex_blocks(text: &str) -> Result<Vec<HtmlBlock>> {
    parse_bibtex_blocks_with_warnings(text).map(|(blocks, _)| blocks)
}

pub(crate) fn looks_like_bibtex_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    text.lines().take(100).any(|raw_line| {
        let line = raw_line.trim_start();
        let Some(rest) = line.strip_prefix('@') else {
            return false;
        };
        let type_end = rest
            .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
            .unwrap_or(rest.len());
        if type_end == 0 {
            return false;
        }
        let after_type = rest[type_end..].trim_start();
        let Some(open) = after_type
            .chars()
            .next()
            .filter(|character| matches!(character, '{' | '('))
        else {
            return false;
        };
        let body = after_type.get(open.len_utf8()..).unwrap_or_default();
        let closing = if open == '{' { '}' } else { ')' };
        let key = body
            .split_once(',')
            .map(|(key, _)| key.trim())
            .unwrap_or_default();
        !key.is_empty() && !key.starts_with(closing)
    })
}

fn parse_bibtex_blocks_with_warnings(text: &str) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    validate_bibtex(text)?;
    let mut parser = BibParser {
        text,
        ..BibParser::default()
    };
    parser.blocks.push(HtmlBlock::Heading {
        level: 1,
        text: "BibTeX bibliography".into(),
    });
    while parser.skip_space_and_comments() {
        if parser.peek_char() != Some('@') {
            parser.bump_char();
            continue;
        }
        parser.parse_record()?;
    }
    if parser.entry_count == 0 {
        push_bib_warning(
            &mut parser.warnings,
            "BibTeX input contains no supported entries",
        );
    }
    Ok((parser.blocks, parser.warnings))
}

fn validate_bibtex(text: &str) -> Result<()> {
    if text.len() as u64 > MAX_BIB_BYTES {
        return Err(Error::LimitExceeded(format!(
            "BibTeX input exceeds {MAX_BIB_BYTES} bytes"
        )));
    }
    let mut line_count = 0usize;
    for line in text.lines() {
        line_count += 1;
        if line_count > MAX_BIB_LINES {
            return Err(Error::LimitExceeded(format!(
                "BibTeX input exceeds {MAX_BIB_LINES} lines"
            )));
        }
        if line.len() > MAX_BIB_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "BibTeX line exceeds {MAX_BIB_LINE_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

impl BibParser<'_> {
    fn parse_record(&mut self) -> Result<()> {
        self.bump_char(); // @
        let kind_start = self.index;
        while self
            .peek_char()
            .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            self.bump_char();
        }
        if self.index - kind_start > MAX_BIB_FIELD_NAME_BYTES {
            return Err(Error::LimitExceeded(format!(
                "BibTeX entry type exceeds {MAX_BIB_FIELD_NAME_BYTES} bytes"
            )));
        }
        let kind = self.text[kind_start..self.index].to_ascii_lowercase();
        self.records_processed = self.records_processed.saturating_add(1);
        if self.records_processed > MAX_BIB_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "BibTeX input exceeds {MAX_BIB_RECORDS} records"
            )));
        }
        self.skip_whitespace();
        let Some(open) = self.bump_char() else {
            return Err(Error::InvalidInput(
                "BibTeX record has no opening delimiter".into(),
            ));
        };
        let close = match open {
            '{' => '}',
            '(' => ')',
            _ => {
                return Err(Error::InvalidInput(format!(
                    "BibTeX record @{kind} must use braces or parentheses"
                )));
            }
        };
        if kind == "comment" {
            self.skip_balanced(close)?;
            return Ok(());
        }
        if kind == "preamble" {
            self.skip_balanced(close)?;
            push_bib_warning(&mut self.warnings, "BibTeX @preamble content was omitted");
            return Ok(());
        }
        if kind == "string" {
            self.skip_balanced(close)?;
            push_bib_warning(&mut self.warnings, "BibTeX @string macros are not expanded");
            return Ok(());
        }

        self.skip_whitespace();
        let key_start = self.index;
        while let Some(character) = self.peek_char() {
            if character == ',' || character == close {
                break;
            }
            self.bump_char();
            if self.index - key_start > MAX_BIB_KEY_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "BibTeX entry key exceeds {MAX_BIB_KEY_BYTES} bytes"
                )));
            }
        }
        let key = self.text[key_start..self.index].trim().to_owned();
        if self.bump_if(',') {
            // Key separator consumed.
        } else if self.peek_char() == Some(close) {
            self.bump_char();
            self.emit_record(&kind, &key, &[])?;
            return Ok(());
        } else {
            return Err(Error::InvalidInput(format!(
                "BibTeX entry @{kind} is missing its key separator"
            )));
        }

        let mut fields = Vec::<(String, String)>::new();
        let mut entry_bytes = key.len();
        let mut saw_macro = false;
        loop {
            self.skip_whitespace();
            while self.bump_if(',') {
                self.skip_whitespace();
            }
            if self.peek_char() == Some(close) {
                self.bump_char();
                break;
            }
            if self.peek_char().is_none() {
                return Err(Error::InvalidInput(format!(
                    "BibTeX entry @{kind} has no closing delimiter"
                )));
            }
            let field_start = self.index;
            while self.peek_char().is_some_and(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            }) {
                self.bump_char();
                if self.index - field_start > MAX_BIB_FIELD_NAME_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "BibTeX field name exceeds {MAX_BIB_FIELD_NAME_BYTES} bytes"
                    )));
                }
            }
            if field_start == self.index {
                return Err(Error::InvalidInput(format!(
                    "BibTeX entry @{kind} contains an invalid field name"
                )));
            }
            let field_name = self.text[field_start..self.index].to_ascii_lowercase();
            self.skip_whitespace();
            if !self.bump_if('=') {
                return Err(Error::InvalidInput(format!(
                    "BibTeX field {field_name} has no equals sign"
                )));
            }
            let (value, uses_macro) = self.parse_value(close)?;
            entry_bytes = entry_bytes
                .saturating_add(field_name.len())
                .saturating_add(value.len());
            if entry_bytes > MAX_BIB_ENTRY_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "BibTeX entry exceeds {MAX_BIB_ENTRY_BYTES} decoded bytes"
                )));
            }
            saw_macro |= uses_macro;
            if fields.len() >= MAX_BIB_FIELDS_PER_ENTRY {
                return Err(Error::LimitExceeded(format!(
                    "BibTeX entry @{kind} exceeds {MAX_BIB_FIELDS_PER_ENTRY} fields"
                )));
            }
            fields.push((field_name, clean_bib_value(&value)));
        }
        if saw_macro {
            push_bib_warning(
                &mut self.warnings,
                "BibTeX string macros or concatenations were kept literal",
            );
        }
        self.emit_record(&kind, &key, &fields)
    }

    fn parse_value(&mut self, close: char) -> Result<(String, bool)> {
        let mut output = String::new();
        let mut brace_depth = 0usize;
        let mut in_quotes = false;
        let mut escaped = false;
        let mut uses_macro = false;
        let mut token = String::new();
        loop {
            let Some(character) = self.peek_char() else {
                return Err(Error::InvalidInput(
                    "BibTeX field value is not terminated".into(),
                ));
            };
            if in_quotes {
                self.bump_char();
                if escaped {
                    output.push('\\');
                    output.push(character);
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_quotes = false;
                } else {
                    output.push(character);
                }
                if output.len() > MAX_BIB_FIELD_VALUE_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "BibTeX field value exceeds {MAX_BIB_FIELD_VALUE_BYTES} bytes"
                    )));
                }
                continue;
            }
            if character == '\\' {
                self.bump_char();
                output.push('\\');
                if let Some(escaped) = self.bump_char() {
                    output.push(escaped);
                }
                if output.len() > MAX_BIB_FIELD_VALUE_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "BibTeX field value exceeds {MAX_BIB_FIELD_VALUE_BYTES} bytes"
                    )));
                }
                token.clear();
                continue;
            }
            if brace_depth == 0 && (character == ',' || character == close) {
                if looks_like_bib_macro(&token) {
                    uses_macro = true;
                }
                break;
            }
            self.bump_char();
            match character {
                '"' if brace_depth == 0 => {
                    if looks_like_bib_macro(&token) {
                        uses_macro = true;
                    }
                    token.clear();
                    in_quotes = true;
                }
                '"' => output.push('"'),
                '{' => {
                    if brace_depth >= MAX_BIB_NESTING {
                        return Err(Error::LimitExceeded(format!(
                            "BibTeX value exceeds nesting depth {MAX_BIB_NESTING}"
                        )));
                    }
                    brace_depth += 1;
                    output.push(character);
                    token.clear();
                }
                '}' if brace_depth > 0 => {
                    brace_depth -= 1;
                    output.push(character);
                    token.clear();
                }
                '#' if brace_depth == 0 => {
                    if looks_like_bib_macro(&token) {
                        uses_macro = true;
                    }
                    token.clear();
                    output.push(' ');
                }
                character => {
                    if character.is_ascii_alphanumeric() || character == '_' {
                        token.push(character);
                    } else if !character.is_whitespace() {
                        if looks_like_bib_macro(&token) {
                            uses_macro = true;
                        }
                        token.clear();
                    }
                    output.push(character);
                }
            }
            if output.len() > MAX_BIB_FIELD_VALUE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "BibTeX field value exceeds {MAX_BIB_FIELD_VALUE_BYTES} bytes"
                )));
            }
        }
        if brace_depth != 0 || in_quotes || escaped {
            return Err(Error::InvalidInput(
                "BibTeX field contains an unbalanced value".into(),
            ));
        }
        if looks_like_bib_macro(&token) {
            uses_macro = true;
        }
        Ok((output, uses_macro))
    }

    fn emit_record(&mut self, kind: &str, key: &str, fields: &[(String, String)]) -> Result<()> {
        self.entry_count = self.entry_count.saturating_add(1);
        if self.entry_count > MAX_BIB_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "BibTeX input exceeds {MAX_BIB_ENTRIES} entries"
            )));
        }
        let mut text = format!("@{kind}{{{key}}}");
        for (field, value) in fields {
            text.push('\n');
            text.push_str(field);
            text.push_str(": ");
            text.push_str(value);
        }
        self.rendered_bytes = self.rendered_bytes.saturating_add(text.len());
        if self.rendered_bytes > MAX_BIB_RENDERED_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "BibTeX rendered text exceeds {MAX_BIB_RENDERED_TEXT_BYTES} bytes"
            )));
        }
        self.blocks.push(HtmlBlock::Paragraph { text });
        Ok(())
    }

    fn skip_balanced(&mut self, close: char) -> Result<()> {
        let mut depth = 1usize;
        let mut in_quotes = false;
        let mut escaped = false;
        while let Some(character) = self.bump_char() {
            if in_quotes {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    in_quotes = false;
                }
                continue;
            }
            match character {
                '"' => in_quotes = true,
                '{' => depth = depth.saturating_add(1),
                '}' if close == '}' && depth == 1 => return Ok(()),
                '}' if close == '}' => depth -= 1,
                '(' if close == ')' => depth = depth.saturating_add(1),
                ')' if close == ')' && depth == 1 => return Ok(()),
                ')' if close == ')' => depth -= 1,
                _ => {}
            }
            if depth > MAX_BIB_NESTING {
                return Err(Error::LimitExceeded(format!(
                    "BibTeX skipped record exceeds nesting depth {MAX_BIB_NESTING}"
                )));
            }
        }
        Err(Error::InvalidInput(
            "BibTeX ignored record is not terminated".into(),
        ))
    }

    fn skip_space_and_comments(&mut self) -> bool {
        loop {
            while self.peek_char().is_some_and(char::is_whitespace) {
                self.bump_char();
            }
            if self.peek_char() == Some('%') {
                while let Some(character) = self.bump_char() {
                    if character == '\n' {
                        break;
                    }
                }
                continue;
            }
            return self.peek_char().is_some();
        }
    }

    fn skip_whitespace(&mut self) {
        while self.peek_char().is_some_and(char::is_whitespace) {
            self.bump_char();
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.text.get(self.index..)?.chars().next()
    }

    fn bump_char(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.index += character.len_utf8();
        Some(character)
    }

    fn bump_if(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.bump_char();
            true
        } else {
            false
        }
    }
}

fn looks_like_bib_macro(token: &str) -> bool {
    !token.is_empty() && !token.bytes().all(|byte| byte.is_ascii_digit())
}

fn clean_bib_value(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' | '}' => {}
            '\\' => {
                if let Some(next) = characters.next() {
                    if next.is_ascii_alphabetic() {
                        output.push(next);
                        while characters
                            .peek()
                            .is_some_and(|character| character.is_ascii_alphabetic())
                        {
                            output.push(characters.next().unwrap_or_default());
                        }
                    } else if matches!(next, '&' | '%' | '_' | '#' | '$' | '{' | '}' | '~') {
                        output.push(if next == '~' { ' ' } else { next });
                    } else {
                        output.push(next);
                    }
                }
            }
            character if character.is_control() && !matches!(character, '\n' | '\t') => {
                output.push('\u{fffd}')
            }
            character => output.push(character),
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_bib_warning(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_braces_quotes_and_macro_literals() {
        let input = "% bibliography\n@Article{key, author = {A. Name}, title = {{A {Nested} Title}}, year = 2024, note = \"A, quoted value\", comment = {An unpaired \" quote}, journal = jname # \" Review\"}\n@string{jname = {Journal}}\n";
        let (blocks, warnings) = parse_bibtex_blocks_with_warnings(input).unwrap();
        let output = format!("{blocks:?}");
        assert!(output.contains("@article{key}"));
        assert!(output.contains("A Nested Title"));
        assert!(output.contains("A, quoted value"));
        assert!(output.contains(r#"An unpaired \" quote"#));
        assert!(output.contains("jname Review"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("@string macros"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("macros or concatenations"))
        );
    }

    #[test]
    fn enforces_bibtex_nesting_and_text_budgets() {
        let text = format!(
            "@article{{k,title={{{}}}}}",
            "{".repeat(MAX_BIB_NESTING + 1)
        );
        assert!(parse_bibtex_blocks(&text).is_err());

        let field_value = "a".repeat(900 * 1024);
        let oversized_field =
            format!("@article{{k,title={{{field_value}\n{field_value}\n{field_value}}}}}\n");
        assert!(parse_bibtex_blocks(&oversized_field).is_err());
    }
}
