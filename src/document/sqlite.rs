//! Bounded read-only SQLite table preview.
//!
//! This adapter is intentionally a data preview rather than a SQL client. It
//! opens the database read-only, enumerates ordinary user tables, and renders
//! a bounded sample of each table as SVG tables. Triggers, views, virtual
//! tables, blobs, extensions, and arbitrary SQL are never executed.

use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::limits::Limit;
use rusqlite::{Connection, OpenFlags, types::ValueRef};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages_with_warnings};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_SQLITE_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SQLITE_TABLES: usize = 100;
const MAX_SQLITE_ROWS_PER_TABLE: usize = 2_000;
const MAX_SQLITE_COLUMNS: usize = 128;
const MAX_SQLITE_CELL_CHARS: usize = 512;
const MAX_SQLITE_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_SQLITE_QUERY_SECONDS: u64 = 5;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"SQLite format 3\0")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let metadata = std::fs::metadata(path)?;
    let limit = options.max_input_bytes.min(MAX_SQLITE_INPUT_BYTES);
    if metadata.len() > limit {
        return Err(Error::LimitExceeded(format!(
            "SQLite input exceeds maximum bytes ({limit})"
        )));
    }
    check_sidecar(path, "-wal", limit)?;
    check_sidecar(path, "-shm", limit.min(16 * 1024 * 1024))?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(path, flags).map_err(|error| {
        Error::InvalidInput(format!("cannot open SQLite database read-only: {error}"))
    })?;
    connection
        .busy_timeout(Duration::from_secs(1))
        .map_err(sql_error)?;
    let started = Instant::now();
    connection
        .progress_handler(
            1000,
            Some(move || started.elapsed() >= Duration::from_secs(MAX_SQLITE_QUERY_SECONDS)),
        )
        .map_err(sql_error)?;
    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, 8 * 1024 * 1024)
        .map_err(sql_error)?;
    connection
        .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 1024 * 1024)
        .map_err(sql_error)?;
    connection
        .set_limit(Limit::SQLITE_LIMIT_COLUMN, MAX_SQLITE_COLUMNS as i32)
        .map_err(sql_error)?;
    let mut warnings = Vec::new();
    let mut blocks = Vec::new();
    let tables = list_tables(&connection)?;
    if tables.is_empty() {
        return Err(Error::InvalidInput(
            "SQLite database contains no ordinary user tables".into(),
        ));
    }
    if tables.len() > MAX_SQLITE_TABLES {
        warnings.push(format!(
            "SQLite contains more than {MAX_SQLITE_TABLES} user tables; remaining tables were omitted"
        ));
    }
    let mut text_bytes = 0usize;
    for table_name in tables.into_iter().take(MAX_SQLITE_TABLES) {
        let (table, table_warnings) = read_table(&connection, &table_name, &mut text_bytes)?;
        warnings.extend(table_warnings);
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: table_name,
        });
        blocks.push(HtmlBlock::Table(table));
    }
    if text_bytes > MAX_SQLITE_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "SQLite rendered text exceeds {MAX_SQLITE_TEXT_BYTES} bytes"
        )));
    }
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(dedup_warnings(warnings))
}

fn list_tables(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1")
        .map_err(sql_error)?;
    let names = statement
        .query_map([MAX_SQLITE_TABLES.saturating_add(1) as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(sql_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    Ok(names)
}

fn read_table(
    connection: &Connection,
    table_name: &str,
    text_bytes: &mut usize,
) -> Result<(TableData, Vec<String>)> {
    let quoted = quote_identifier(table_name);
    let sql = format!(
        "SELECT * FROM {quoted} LIMIT {}",
        MAX_SQLITE_ROWS_PER_TABLE + 1
    );
    let mut statement = connection.prepare(&sql).map_err(sql_error)?;
    let column_count = statement.column_count().min(MAX_SQLITE_COLUMNS);
    let headers = statement
        .column_names()
        .into_iter()
        .take(column_count)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut query = statement.query([]).map_err(sql_error)?;
    while let Some(row) = query.next().map_err(sql_error)? {
        if rows.len() >= MAX_SQLITE_ROWS_PER_TABLE {
            warnings.push(format!(
                "SQLite table '{table_name}' exceeded {MAX_SQLITE_ROWS_PER_TABLE} sampled rows; remaining rows were omitted"
            ));
            break;
        }
        let mut values = Vec::with_capacity(column_count);
        for index in 0..column_count {
            let value = match row.get_ref(index).map_err(sql_error)? {
                ValueRef::Null => String::new(),
                ValueRef::Integer(value) => value.to_string(),
                ValueRef::Real(value) => value.to_string(),
                ValueRef::Text(value) => String::from_utf8_lossy(value).into_owned(),
                ValueRef::Blob(value) => format!("<BLOB {} bytes>", value.len()),
            };
            let value = truncate_cell(&value);
            *text_bytes = text_bytes.saturating_add(value.len());
            values.push(value);
        }
        rows.push(values);
    }
    Ok((
        TableData {
            headers,
            rows,
            alignments: vec![TableAlign::Left; column_count],
            raw_source: String::new(),
        },
        warnings,
    ))
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn truncate_cell(value: &str) -> String {
    value.chars().take(MAX_SQLITE_CELL_CHARS).collect()
}

fn sql_error(error: rusqlite::Error) -> Error {
    Error::InvalidInput(format!("SQLite preview query failed: {error}"))
}

fn check_sidecar(path: &Path, suffix: &str, limit: u64) -> Result<()> {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let sidecar = path.with_file_name(format!("{file_name}{suffix}"));
    let Ok(metadata) = std::fs::metadata(&sidecar) else {
        return Ok(());
    };
    if !metadata.is_file() {
        return Err(Error::InvalidInput(format!(
            "SQLite sidecar '{suffix}' is not a regular file"
        )));
    }
    if metadata.len() > limit {
        return Err(Error::LimitExceeded(format!(
            "SQLite sidecar '{suffix}' exceeds maximum bytes ({limit})"
        )));
    }
    Ok(())
}

fn dedup_warnings(mut warnings: Vec<String>) -> Vec<String> {
    warnings.sort();
    warnings.dedup();
    warnings
}
