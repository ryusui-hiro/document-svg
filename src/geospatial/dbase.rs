//! Bounded dBASE III/III+ attribute-table previews.

use std::fs;
use std::path::Path;

use encoding_rs::{Encoding, WINDOWS_1252};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, layout_and_render_table};

const MAX_DBF_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DBF_SIDECAR_BYTES: u64 = 1024;
const MAX_DBF_FIELDS: usize = 255;
const MAX_DBF_RECORD_BYTES: usize = 4_000;
const MAX_DBF_ROWS: usize = 100_000;
const MAX_DBF_CELLS: usize = 2_000_000;
const MAX_DBF_FIELD_BYTES: usize = 254;
const ROWS_PER_PAGE: usize = 100;
const COLUMNS_PER_PAGE: usize = 32;

#[derive(Clone, Debug)]
struct Field {
    name: String,
    kind: u8,
    width: usize,
}

#[derive(Debug)]
struct Header {
    fields: Vec<Field>,
    record_count: usize,
    header_bytes: usize,
    record_bytes: usize,
    language_driver: u8,
    has_memo_file: bool,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    if bytes.len() < 32 || !matches!(bytes[0], 0x03 | 0x83) {
        return false;
    }
    let header_bytes = usize::from(u16::from_le_bytes([bytes[8], bytes[9]]));
    let record_bytes = usize::from(u16::from_le_bytes([bytes[10], bytes[11]]));
    header_bytes >= 65
        && (header_bytes - 33).is_multiple_of(32)
        && header_bytes <= 32 + MAX_DBF_FIELDS * 32 + 1
        && (1..=MAX_DBF_RECORD_BYTES).contains(&record_bytes)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_DBF_BYTES),
        "dBASE table input",
    )?;
    let header = parse_header(&bytes)?;
    let cpg = read_code_page_sidecar(path)?;
    let (encoding, code_page_warning) = select_encoding(cpg.as_deref(), header.language_driver)?;
    let active_records = count_active_records(&bytes, &header)?;
    let field_groups = header.fields.len().div_ceil(COLUMNS_PER_PAGE);
    let row_groups = active_records.max(1).div_ceil(ROWS_PER_PAGE);
    let page_count = field_groups
        .checked_mul(row_groups)
        .ok_or_else(|| Error::LimitExceeded("dBASE output page count overflowed".into()))?;
    if options.max_pages == 0 || page_count > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "dBASE table requires {page_count} output pages; maximum is {}",
            options.max_pages
        )));
    }

    let mut warnings = Vec::new();
    if let Some(warning) = code_page_warning {
        warnings.push(warning);
    }
    if header.has_memo_file || header.fields.iter().any(|field| field.kind == b'M') {
        warnings.push("dBASE memo values in companion DBT files are not loaded; non-empty memo cells are labeled".into());
    }
    let unsupported_types = header
        .fields
        .iter()
        .filter(|field| !matches!(field.kind, b'C' | b'D' | b'F' | b'L' | b'M' | b'N'))
        .map(|field| field.kind)
        .collect::<std::collections::BTreeSet<_>>();
    if !unsupported_types.is_empty() {
        let types = unsupported_types
            .iter()
            .map(|kind| char::from(*kind))
            .collect::<String>();
        warnings.push(format!(
            "dBASE field type(s) {types} are shown as omitted placeholders"
        ));
    }

    let mut page_number = 1usize;
    let mut batch = Vec::with_capacity(ROWS_PER_PAGE);
    let mut active_seen = 0usize;
    let mut deleted_records = 0usize;
    let mut text_decode_errors = false;
    let mut record_offset = header.header_bytes;
    for record_index in 0..header.record_count {
        let end = record_offset + header.record_bytes;
        let record = &bytes[record_offset..end];
        record_offset = end;
        match record[0] {
            b'*' => {
                deleted_records += 1;
                continue;
            }
            b' ' => active_seen += 1,
            marker => {
                return Err(invalid(format!(
                    "record {} has an invalid deletion marker 0x{marker:02X}",
                    record_index + 1
                )));
            }
        }

        let mut row = Vec::with_capacity(header.fields.len());
        let mut field_offset = 1usize;
        for field in &header.fields {
            let field_end = field_offset + field.width;
            let value = decode_value(
                field,
                &record[field_offset..field_end],
                encoding,
                &mut text_decode_errors,
            )?;
            row.push(value);
            field_offset = field_end;
        }
        batch.push(row);
        if batch.len() == ROWS_PER_PAGE {
            page_number = emit_row_batch(
                &header.fields,
                &batch,
                active_records,
                active_seen + 1 - batch.len(),
                options,
                sink,
                page_number,
            )?;
            batch.clear();
        }
    }
    if !batch.is_empty() || active_records == 0 {
        let _ = emit_row_batch(
            &header.fields,
            &batch,
            active_records,
            active_seen + 1 - batch.len(),
            options,
            sink,
            page_number,
        )?;
    }
    if deleted_records > 0 {
        warnings.push(format!(
            "{deleted_records} deleted dBASE record(s) were omitted"
        ));
    }
    if text_decode_errors {
        warnings
            .push("invalid byte sequences in dBASE text fields were replaced with U+FFFD".into());
    }
    Ok(warnings)
}

fn parse_header(bytes: &[u8]) -> Result<Header> {
    if bytes.len() < 33 {
        return Err(invalid("file is shorter than the fixed table header"));
    }
    if !matches!(bytes[0], 0x03 | 0x83) {
        return Err(Error::Unsupported(format!(
            "dBASE version 0x{:02X} is unsupported; dBASE III and III+ tables are supported",
            bytes[0]
        )));
    }
    if bytes[14] != 0 || bytes[15] != 0 {
        return Err(Error::Unsupported(
            "dBASE transactions or encrypted tables are unsupported".into(),
        ));
    }
    let record_count = usize::try_from(u32::from_le_bytes(bytes[4..8].try_into().unwrap()))
        .map_err(|_| {
            Error::LimitExceeded("dBASE record count does not fit this platform".into())
        })?;
    let header_bytes = usize::from(u16::from_le_bytes([bytes[8], bytes[9]]));
    let record_bytes = usize::from(u16::from_le_bytes([bytes[10], bytes[11]]));
    if !(65..=32 + MAX_DBF_FIELDS * 32 + 1).contains(&header_bytes)
        || !(header_bytes - 33).is_multiple_of(32)
    {
        return Err(invalid(
            "header length does not match 32-byte field descriptors",
        ));
    }
    if header_bytes > bytes.len() || bytes[header_bytes - 1] != 0x0D {
        return Err(invalid("field descriptor terminator is missing"));
    }
    if record_count > MAX_DBF_ROWS {
        return Err(Error::LimitExceeded(format!(
            "dBASE table declares {record_count} records; maximum is {MAX_DBF_ROWS}"
        )));
    }

    let field_count = (header_bytes - 33) / 32;
    if field_count == 0 || field_count > MAX_DBF_FIELDS {
        return Err(invalid(format!(
            "field count {field_count} is outside 1..={MAX_DBF_FIELDS}"
        )));
    }
    let cell_count = record_count
        .checked_mul(field_count)
        .ok_or_else(|| Error::LimitExceeded("dBASE cell count overflowed".into()))?;
    if cell_count > MAX_DBF_CELLS {
        return Err(Error::LimitExceeded(format!(
            "dBASE table declares {cell_count} cells; maximum is {MAX_DBF_CELLS}"
        )));
    }

    let mut fields = Vec::with_capacity(field_count);
    let mut total_width = 1usize;
    for index in 0..field_count {
        let offset = 32 + index * 32;
        let descriptor = &bytes[offset..offset + 32];
        let name_bytes = &descriptor[..11];
        let name_end = name_bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(name_bytes.len());
        let name = std::str::from_utf8(&name_bytes[..name_end])
            .map_err(|_| invalid(format!("field {index} name is not ASCII/UTF-8")))?
            .trim()
            .to_owned();
        if name.is_empty() {
            return Err(invalid(format!("field {index} has an empty name")));
        }
        let kind = descriptor[11];
        let width = usize::from(descriptor[16]);
        if width == 0 || width > MAX_DBF_FIELD_BYTES {
            return Err(Error::LimitExceeded(format!(
                "field '{name}' width {width} is outside 1..={MAX_DBF_FIELD_BYTES} bytes"
            )));
        }
        validate_field_shape(&name, kind, width)?;
        total_width = total_width
            .checked_add(width)
            .ok_or_else(|| Error::LimitExceeded("dBASE record width overflowed".into()))?;
        fields.push(Field { name, kind, width });
    }
    if total_width > MAX_DBF_RECORD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "dBASE record width {total_width} exceeds {MAX_DBF_RECORD_BYTES} bytes"
        )));
    }
    if record_bytes != total_width {
        return Err(invalid(format!(
            "record length is {record_bytes} bytes but descriptors require {total_width}"
        )));
    }
    let data_bytes = record_count
        .checked_mul(record_bytes)
        .and_then(|size| size.checked_add(header_bytes))
        .ok_or_else(|| Error::LimitExceeded("dBASE physical size overflowed".into()))?;
    let valid_length =
        bytes.len() == data_bytes || (bytes.len() == data_bytes + 1 && bytes[data_bytes] == 0x1A);
    if !valid_length {
        return Err(invalid(format!(
            "table declares {data_bytes} bytes of header and records but input has {} bytes",
            bytes.len()
        )));
    }
    Ok(Header {
        fields,
        record_count,
        header_bytes,
        record_bytes,
        language_driver: bytes[29],
        has_memo_file: bytes[0] == 0x83,
    })
}

fn validate_field_shape(name: &str, kind: u8, width: usize) -> Result<()> {
    let expected = match kind {
        b'D' => Some(8),
        b'L' => Some(1),
        b'M' => Some(10),
        b'C' | b'F' | b'N' => None,
        _ => None,
    };
    if expected.is_some_and(|expected| expected != width) {
        return Err(invalid(format!(
            "field '{name}' type '{}' has width {width}, expected {}",
            char::from(kind),
            expected.unwrap()
        )));
    }
    Ok(())
}

fn count_active_records(bytes: &[u8], header: &Header) -> Result<usize> {
    let mut active = 0usize;
    let mut offset = header.header_bytes;
    for index in 0..header.record_count {
        match bytes[offset] {
            b' ' => active += 1,
            b'*' => {}
            marker => {
                return Err(invalid(format!(
                    "record {} has an invalid deletion marker 0x{marker:02X}",
                    index + 1
                )));
            }
        }
        offset += header.record_bytes;
    }
    Ok(active)
}

fn decode_value(
    field: &Field,
    bytes: &[u8],
    encoding: &'static Encoding,
    had_errors: &mut bool,
) -> Result<String> {
    let trimmed = trim_ascii_field(bytes);
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let value = match field.kind {
        b'C' => decode_text(trimmed, encoding, had_errors),
        b'D' => {
            if trimmed == b"00000000" {
                String::new()
            } else if trimmed.len() == 8 && trimmed.iter().all(u8::is_ascii_digit) {
                let value = std::str::from_utf8(trimmed).expect("date digits are ASCII");
                format!("{}-{}-{}", &value[..4], &value[4..6], &value[6..8])
            } else {
                return Err(invalid(format!(
                    "date field '{}' is not YYYYMMDD",
                    field.name
                )));
            }
        }
        b'L' => match trimmed[0] {
            b'T' | b't' | b'Y' | b'y' => "Yes".into(),
            b'F' | b'f' | b'N' | b'n' => "No".into(),
            b'?' | b' ' => String::new(),
            value => {
                return Err(invalid(format!(
                    "logical field '{}' contains invalid byte 0x{value:02X}",
                    field.name
                )));
            }
        },
        b'N' | b'F' => std::str::from_utf8(trimmed)
            .map_err(|_| invalid(format!("numeric field '{}' is not ASCII", field.name)))?
            .to_owned(),
        b'M' => "[memo omitted]".into(),
        _ => format!("[type {} omitted]", char::from(field.kind)),
    };
    Ok(value)
}

fn emit_row_batch(
    fields: &[Field],
    rows: &[Vec<String>],
    active_records: usize,
    first_active_record: usize,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    mut page_number: usize,
) -> Result<usize> {
    let group_count = fields.len().div_ceil(COLUMNS_PER_PAGE);
    for group in 0..group_count {
        let start = group * COLUMNS_PER_PAGE;
        let end = (start + COLUMNS_PER_PAGE).min(fields.len());
        let headers = fields[start..end]
            .iter()
            .map(|field| field.name.clone())
            .collect::<Vec<_>>();
        let page_rows = rows
            .iter()
            .map(|row| row[start..end].to_vec())
            .collect::<Vec<_>>();
        let alignments = (0..headers.len())
            .map(|column| {
                if !page_rows.is_empty()
                    && page_rows
                        .iter()
                        .all(|row| crate::table::is_numeric_cell(&row[column]))
                {
                    TableAlign::Right
                } else {
                    TableAlign::Left
                }
            })
            .collect();
        let table = TableData {
            headers,
            rows: page_rows,
            alignments,
            raw_source: String::new(),
        };
        let mut page = layout_and_render_table(&table, options)?;
        page.number = page_number;
        page.source_format = "dbf".into();
        page.title = if group_count == 1 {
            "dBASE Table".into()
        } else {
            format!("dBASE Table — columns {}–{}", start + 1, end)
        };
        let first_record = first_active_record;
        let last_record = first_record
            .saturating_add(rows.len().saturating_sub(1))
            .min(active_records);
        page.description = if rows.is_empty() {
            format!(
                "dBASE table with {} fields and no active records",
                fields.len()
            )
        } else {
            format!(
                "dBASE table rows {first_record}–{last_record} of {active_records} active records"
            )
        };
        sink.consume(page)?;
        page_number += 1;
    }
    Ok(page_number)
}

fn select_encoding(
    cpg: Option<&[u8]>,
    language_driver: u8,
) -> Result<(&'static Encoding, Option<String>)> {
    if let Some(bytes) = cpg {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| invalid(".cpg code-page sidecar is not ASCII/UTF-8"))?;
        let label = text.strip_prefix('\u{feff}').unwrap_or(text).trim();
        let label = code_page_label(label).ok_or_else(|| {
            Error::Unsupported(format!(
                "dBASE code page '{label}' is not supported; convert the table to UTF-8 first"
            ))
        })?;
        let encoding = Encoding::for_label(label.as_bytes()).ok_or_else(|| {
            Error::Unsupported(format!("dBASE code page '{label}' is unsupported"))
        })?;
        return Ok((encoding, None));
    }
    if language_driver == 0x57 {
        return Ok((WINDOWS_1252, None));
    }
    if language_driver == 0 {
        return Ok((WINDOWS_1252, Some(
            "dBASE table has no .cpg sidecar or recognized language-driver ID; text is decoded as Windows-1252".into(),
        )));
    }
    Err(Error::Unsupported(format!(
        "dBASE language-driver ID 0x{language_driver:02X} is not mapped; add a supported .cpg sidecar"
    )))
}

fn code_page_label(label: &str) -> Option<&'static str> {
    let normalized = label.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    Some(match normalized.as_str() {
        "65001" | "utf8" | "utf-8" => "utf-8",
        "1250" | "windows-1250" => "windows-1250",
        "1251" | "windows-1251" => "windows-1251",
        "1252" | "ansi-1252" | "windows-1252" => "windows-1252",
        "1253" | "windows-1253" => "windows-1253",
        "1254" | "windows-1254" => "windows-1254",
        "1255" | "windows-1255" => "windows-1255",
        "1256" | "windows-1256" => "windows-1256",
        "1257" | "windows-1257" => "windows-1257",
        "1258" | "windows-1258" => "windows-1258",
        "932" | "sjis" | "shiftjis" | "shift-jis" => "shift_jis",
        "936" | "gbk" => "gbk",
        "949" | "euc-kr" => "euc-kr",
        "950" | "big5" => "big5",
        "88591" | "iso-8859-1" => "iso-8859-1",
        "88592" | "iso-8859-2" => "iso-8859-2",
        _ => return None,
    })
}

fn read_code_page_sidecar(path: &Path) -> Result<Option<Vec<u8>>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let Some(stem) = path.file_stem() else {
        return Ok(None);
    };
    let stem = stem.to_string_lossy();
    for extension in ["cpg", "Cpg", "cPg", "CPg", "cpG", "CpG", "cPG", "CPG"] {
        let candidate = parent.join(format!("{stem}.{extension}"));
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_file() => {
                return Ok(Some(read_limited_file(
                    &candidate,
                    MAX_DBF_SIDECAR_BYTES,
                    "dBASE code-page sidecar",
                )?));
            }
            Ok(_) => {
                return Err(invalid(format!(
                    "code-page sidecar '{}' is not a regular file",
                    candidate.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

fn trim_ascii_field(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !matches!(byte, b' ' | 0))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | 0))
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

fn decode_text(bytes: &[u8], encoding: &'static Encoding, had_errors: &mut bool) -> String {
    let (text, contains_errors) = encoding.decode_without_bom_handling(bytes);
    *had_errors |= contains_errors;
    text.into_owned()
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(format!("invalid dBASE table: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dbf_fixture() -> Vec<u8> {
        let fields: [(&[u8], u8, u8, u8); 4] = [
            (b"PARCEL_ID", b'N', 5, 0),
            (b"NAME", b'C', 12, 0),
            (b"ACTIVE", b'L', 1, 0),
            (b"BUILT", b'D', 8, 0),
        ];
        let record_len = 1 + fields
            .iter()
            .map(|(_, _, width, _)| usize::from(*width))
            .sum::<usize>();
        let header_len = 32 + fields.len() * 32 + 1;
        let rows: [[&[u8]; 4]; 2] = [
            [b"  101", b"Cafe\xE9       ", b"T", b"20200115"],
            [b"  102", b"Park        ", b"F", b"20191231"],
        ];
        let mut bytes = vec![0; header_len];
        bytes[0] = 0x03;
        bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
        bytes[8..10].copy_from_slice(&(header_len as u16).to_le_bytes());
        bytes[10..12].copy_from_slice(&(record_len as u16).to_le_bytes());
        bytes[29] = 0x57;
        for (index, (name, kind, width, decimals)) in fields.iter().enumerate() {
            let offset = 32 + index * 32;
            bytes[offset..offset + name.len()].copy_from_slice(name);
            bytes[offset + 11] = *kind;
            bytes[offset + 16] = *width;
            bytes[offset + 17] = *decimals;
        }
        bytes[header_len - 1] = 0x0D;
        for row in rows {
            bytes.push(b' ');
            for (value, (_, _, width, _)) in row.iter().zip(fields.iter()) {
                assert_eq!(value.len(), usize::from(*width));
                bytes.extend_from_slice(value);
            }
        }
        bytes.push(0x1A);
        bytes
    }

    #[test]
    fn parses_fixed_width_fields_and_decodes_the_language_driver() {
        let bytes = dbf_fixture();
        assert!(looks_like_prefix(&bytes[..32]));
        let header = parse_header(&bytes).unwrap();
        assert_eq!(header.record_count, 2);
        assert_eq!(header.fields.len(), 4);
        let (encoding, warning) = select_encoding(None, header.language_driver).unwrap();
        assert!(warning.is_none());
        let mut had_errors = false;
        let name = decode_value(
            &header.fields[1],
            b"Cafe\xE9       ",
            encoding,
            &mut had_errors,
        )
        .unwrap();
        assert_eq!(name, "Cafeé");
        assert!(!had_errors);
    }

    #[test]
    fn rejects_descriptor_length_record_width_and_unknown_language_driver() {
        let mut bytes = dbf_fixture();
        bytes[8..10].copy_from_slice(&65u16.to_le_bytes());
        assert!(parse_header(&bytes).is_err());

        let mut bytes = dbf_fixture();
        bytes[10..12].copy_from_slice(&999u16.to_le_bytes());
        assert!(parse_header(&bytes).is_err());
        assert!(select_encoding(None, 0xC8).is_err());
    }

    #[test]
    fn maps_supported_cpg_code_page_aliases() {
        assert_eq!(code_page_label("1252"), Some("windows-1252"));
        assert_eq!(code_page_label("SJIS"), Some("shift_jis"));
        assert_eq!(code_page_label("936"), Some("gbk"));
        assert_eq!(code_page_label("437"), None);
        let (encoding, warning) = select_encoding(Some(b"932\n"), 0).unwrap();
        assert!(warning.is_none());
        let (decoded, had_errors) =
            encoding.decode_without_bom_handling(&[0x83, 0x65, 0x83, 0x58, 0x83, 0x67]);
        assert_eq!(decoded, "テスト");
        assert!(!had_errors);
    }

    #[test]
    fn converts_text_using_a_sibling_cpg_sidecar() {
        #[derive(Default)]
        struct TestSink(Vec<crate::ir::Page>);

        impl PageConsumer for TestSink {
            fn consume(&mut self, page: crate::ir::Page) -> Result<()> {
                self.0.push(page);
                Ok(())
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("codepage.dbf");
        let mut bytes = dbf_fixture();
        let header_length = usize::from(u16::from_le_bytes([bytes[8], bytes[9]]));
        let owner_offset = header_length + 1 + 5;
        let (encoded, _, had_errors) = encoding_rs::SHIFT_JIS.encode("テスト");
        assert!(!had_errors);
        bytes[owner_offset..owner_offset + 12].fill(b' ');
        bytes[owner_offset..owner_offset + encoded.len()].copy_from_slice(&encoded);
        std::fs::write(&path, bytes).unwrap();
        std::fs::write(directory.path().join("codepage.CPG"), "\u{feff}932\n").unwrap();

        let mut sink = TestSink::default();
        let warnings = convert(&path, &ConvertOptions::default(), &mut sink).unwrap();
        assert!(warnings.is_empty());
        let text = sink
            .0
            .iter()
            .flat_map(|page| &page.nodes)
            .filter_map(|node| match node {
                crate::ir::Node::Text { runs, .. } => {
                    Some(runs.iter().map(|run| run.text.as_str()).collect::<String>())
                }
                _ => None,
            })
            .collect::<String>();
        assert!(text.contains("テスト"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_code_page_sidecar() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let path = directory.path().join("linked.dbf");
        std::fs::write(&path, dbf_fixture()).unwrap();
        std::fs::write(external.path().join("encoding.txt"), "1252").unwrap();
        symlink(
            external.path().join("encoding.txt"),
            directory.path().join("linked.CpG"),
        )
        .unwrap();
        assert!(read_code_page_sidecar(&path).is_err());
    }

    #[test]
    fn paginates_wide_tables_into_bounded_column_groups() {
        #[derive(Default)]
        struct TestSink(Vec<crate::ir::Page>);

        impl PageConsumer for TestSink {
            fn consume(&mut self, page: crate::ir::Page) -> Result<()> {
                self.0.push(page);
                Ok(())
            }
        }

        let fields = (0..33)
            .map(|index| Field {
                name: format!("FIELD_{index:02}"),
                kind: b'C',
                width: 1,
            })
            .collect::<Vec<_>>();
        let rows = (0..100)
            .map(|row| (0..33).map(|_| row.to_string()).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let options = ConvertOptions::default();
        let mut sink = TestSink::default();
        let next_page = emit_row_batch(&fields, &rows, 100, 1, &options, &mut sink, 1).unwrap();
        assert_eq!(next_page, 3);
        assert_eq!(sink.0.len(), 2);
        assert_eq!(sink.0[0].number, 1);
        assert_eq!(sink.0[1].number, 2);
        assert!(sink.0[0].title.contains("columns 1–32"));
        assert!(sink.0[1].title.contains("columns 33–33"));
    }
}
