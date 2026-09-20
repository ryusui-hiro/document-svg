//! Bounded previews of GeoPackage raster tile pyramids.

use std::collections::HashSet;
use std::io::Cursor;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::{ImageFormat, ImageReader, Limits};
use rusqlite::{Connection, OptionalExtension};

use crate::convert::PageConsumer;
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, SourceMeta};

const MAX_TILE_LAYERS: usize = 256;
const MAX_TILE_ZOOM_LEVELS: usize = 64;
const MAX_TILE_ROWS: usize = 512;
const MAX_TILE_MATRIX_DIMENSION: i64 = 1_000_000;
const MAX_TILE_DIMENSION: u32 = 4096;
const MAX_TILE_BLOB_BYTES: usize = 4 * 1024 * 1024;
const MAX_TILE_TOTAL_INPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_TILE_TOTAL_PNG_BYTES: usize = 64 * 1024 * 1024;
const MAX_TILE_TOTAL_PIXELS: u64 = 20_000_000;
const MAX_TILE_IMAGE_ALLOCATION_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SVG_DATA_URI_BYTES: usize = 96 * 1024 * 1024;
const EARTH_RADIUS_METERS: f64 = 6_378_137.0;
const WEB_MERCATOR_WORLD_LIMIT: f64 = 20_037_508.342_789_244;
const PAGE_WIDTH: f64 = 1200.0;
const PAGE_HEIGHT: f64 = 800.0;
const PAGE_MARGIN: f64 = 40.0;

#[derive(Clone, Debug)]
pub(crate) struct TileLayer {
    table_name: String,
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
    matrices: Vec<TileMatrix>,
}

#[derive(Clone, Copy, Debug)]
struct TileMatrix {
    zoom_level: i64,
    matrix_width: u32,
    matrix_height: u32,
    tile_width: u32,
    tile_height: u32,
    pixel_x_size: f64,
    pixel_y_size: f64,
}

struct EncodedTile {
    column: u32,
    row: u32,
    href: String,
}

struct SelectedLevel {
    matrix: TileMatrix,
    tiles: Vec<EncodedTile>,
    warnings: Vec<String>,
}

#[derive(Default)]
struct TileEncodingBudget {
    png_bytes: usize,
    data_uri_bytes: usize,
    omitted_corrupt_tiles: usize,
}

pub(crate) fn read_layers(connection: &Connection) -> Result<(Vec<TileLayer>, Vec<String>)> {
    let mut names = Vec::<(String, i32)>::new();
    {
        let mut statement = connection
            .prepare(
                "SELECT table_name, srs_id FROM gpkg_contents WHERE data_type = 'tiles' \
                 ORDER BY table_name COLLATE NOCASE LIMIT ?1",
            )
            .map_err(sql_error)?;
        let rows = statement
            .query_map([i64::try_from(MAX_TILE_LAYERS + 1).unwrap()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(sql_error)?;
        for row in rows {
            names.push(row.map_err(sql_error)?);
        }
    }
    if names.len() > MAX_TILE_LAYERS {
        return Err(Error::LimitExceeded(format!(
            "GeoPackage has more than {MAX_TILE_LAYERS} raster tile layers"
        )));
    }

    let mut layers = Vec::with_capacity(names.len());
    let mut warnings = Vec::new();
    for (table_name, contents_srs_id) in names {
        validate_identifier(&table_name, "tile table")?;
        let schema: Option<(String, Option<String>)> = connection
            .query_row(
                "SELECT type, sql FROM sqlite_schema WHERE name = ?1 COLLATE NOCASE LIMIT 1",
                [&table_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        let Some((schema_type, schema_sql)) = schema else {
            return Err(invalid(format!(
                "GeoPackage raster tile table '{table_name}' does not exist"
            )));
        };
        if schema_type == "view" {
            warnings.push(
                "GeoPackage raster tile views were omitted; only regular tile tables are read"
                    .into(),
            );
            continue;
        }
        if schema_type != "table"
            || schema_sql.as_deref().is_some_and(|sql| {
                sql.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("create virtual table")
            })
        {
            warnings.push(
                "GeoPackage virtual-table raster layers were omitted; only regular tile tables are read".into(),
            );
            continue;
        }

        let tile_matrix_set: Option<(i32, f64, f64, f64, f64)> = connection
            .query_row(
                "SELECT srs_id, min_x, min_y, max_x, max_y FROM gpkg_tile_matrix_set \
                 WHERE table_name = ?1 LIMIT 1",
                [&table_name],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(sql_error)?;
        let Some((matrix_srs_id, min_x, min_y, max_x, max_y)) = tile_matrix_set else {
            return Err(invalid(format!(
                "GeoPackage tile layer '{table_name}' has no tile matrix set"
            )));
        };
        if matrix_srs_id != contents_srs_id {
            return Err(invalid(format!(
                "GeoPackage tile matrix set SRS {matrix_srs_id} does not match contents SRS {contents_srs_id}"
            )));
        }
        if ![min_x, min_y, max_x, max_y]
            .iter()
            .all(|value| value.is_finite())
            || min_x >= max_x
            || min_y >= max_y
        {
            return Err(invalid(format!(
                "GeoPackage tile layer '{table_name}' has invalid bounds"
            )));
        }

        let (organization, organization_coordsys_id): (String, i32) = connection
            .query_row(
                "SELECT organization, organization_coordsys_id FROM gpkg_spatial_ref_sys \
                 WHERE srs_id = ?1 LIMIT 1",
                [matrix_srs_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| {
                invalid(format!(
                    "GeoPackage tile SRS {matrix_srs_id} is not defined"
                ))
            })?;
        if !organization.eq_ignore_ascii_case("epsg") || organization_coordsys_id != 3857 {
            warnings
                .push("GeoPackage raster tile layers with a non-EPSG:3857 CRS were omitted".into());
            continue;
        }

        validate_tile_table_columns(connection, &table_name)?;
        let matrices = read_tile_matrices(connection, &table_name, [min_x, min_y, max_x, max_y])?;
        if matrices.is_empty() {
            warnings
                .push("GeoPackage raster tile layers with no tile zoom levels were omitted".into());
            continue;
        }
        layers.push(TileLayer {
            table_name,
            min_x,
            min_y,
            max_x,
            max_y,
            matrices,
        });
    }
    Ok((layers, warnings))
}

fn read_tile_matrices(
    connection: &Connection,
    table_name: &str,
    bounds: [f64; 4],
) -> Result<Vec<TileMatrix>> {
    let mut matrices = Vec::new();
    let mut statement = connection
        .prepare(
            "SELECT zoom_level, matrix_width, matrix_height, tile_width, tile_height, \
             pixel_x_size, pixel_y_size FROM gpkg_tile_matrix WHERE table_name = ?1 \
             ORDER BY zoom_level LIMIT ?2",
        )
        .map_err(sql_error)?;
    let mut rows = statement
        .query(rusqlite::params![
            table_name,
            i64::try_from(MAX_TILE_ZOOM_LEVELS + 1).unwrap()
        ])
        .map_err(sql_error)?;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let zoom_level: i64 = row.get(0).map_err(sql_error)?;
        let matrix_width: i64 = row.get(1).map_err(sql_error)?;
        let matrix_height: i64 = row.get(2).map_err(sql_error)?;
        let tile_width: i64 = row.get(3).map_err(sql_error)?;
        let tile_height: i64 = row.get(4).map_err(sql_error)?;
        let pixel_x_size: f64 = row.get(5).map_err(sql_error)?;
        let pixel_y_size: f64 = row.get(6).map_err(sql_error)?;
        if zoom_level < 0
            || matrix_width <= 0
            || matrix_height <= 0
            || matrix_width > MAX_TILE_MATRIX_DIMENSION
            || matrix_height > MAX_TILE_MATRIX_DIMENSION
            || tile_width <= 0
            || tile_height <= 0
            || tile_width > i64::from(MAX_TILE_DIMENSION)
            || tile_height > i64::from(MAX_TILE_DIMENSION)
            || !pixel_x_size.is_finite()
            || !pixel_y_size.is_finite()
            || pixel_x_size <= 0.0
            || pixel_y_size <= 0.0
        {
            return Err(invalid(format!(
                "GeoPackage tile matrix at zoom {zoom_level} has invalid dimensions or pixel sizes"
            )));
        }
        let matrix = TileMatrix {
            zoom_level,
            matrix_width: u32::try_from(matrix_width).unwrap(),
            matrix_height: u32::try_from(matrix_height).unwrap(),
            tile_width: u32::try_from(tile_width).unwrap(),
            tile_height: u32::try_from(tile_height).unwrap(),
            pixel_x_size,
            pixel_y_size,
        };
        validate_matrix_bounds(&matrix, bounds)?;
        matrices.push(matrix);
    }
    if matrices.len() > MAX_TILE_ZOOM_LEVELS {
        return Err(Error::LimitExceeded(format!(
            "GeoPackage tile layer '{table_name}' exceeds {MAX_TILE_ZOOM_LEVELS} zoom levels"
        )));
    }
    Ok(matrices)
}

fn validate_matrix_bounds(matrix: &TileMatrix, bounds: [f64; 4]) -> Result<()> {
    let (min_x, min_y, max_x, max_y) = (bounds[0], bounds[1], bounds[2], bounds[3]);
    let expected_width =
        f64::from(matrix.matrix_width) * f64::from(matrix.tile_width) * matrix.pixel_x_size;
    let expected_height =
        f64::from(matrix.matrix_height) * f64::from(matrix.tile_height) * matrix.pixel_y_size;
    let width = max_x - min_x;
    let height = max_y - min_y;
    if !expected_width.is_finite()
        || !expected_height.is_finite()
        || (width - expected_width).abs() > width.abs().max(1.0) * 1e-6
        || (height - expected_height).abs() > height.abs().max(1.0) * 1e-6
    {
        return Err(invalid(format!(
            "GeoPackage tile matrix at zoom {} does not match its declared extent",
            matrix.zoom_level
        )));
    }
    Ok(())
}

fn validate_tile_table_columns(connection: &Connection, table_name: &str) -> Result<()> {
    let required = [
        ("zoom_level", "INTEGER"),
        ("tile_column", "INTEGER"),
        ("tile_row", "INTEGER"),
        ("tile_data", "BLOB"),
    ];
    let mut statement = connection
        .prepare("SELECT name, type FROM pragma_table_info(?1)")
        .map_err(sql_error)?;
    let rows = statement
        .query_map([table_name], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql_error)?;
    let mut actual = Vec::new();
    for row in rows {
        actual.push(row.map_err(sql_error)?);
    }
    for (name, kind) in required {
        if !actual.iter().any(|(actual_name, actual_kind)| {
            actual_name.eq_ignore_ascii_case(name) && actual_kind.trim().eq_ignore_ascii_case(kind)
        }) {
            return Err(invalid(format!(
                "GeoPackage raster tile table '{table_name}' is missing a {kind} {name} column"
            )));
        }
    }
    Ok(())
}

pub(crate) fn render_layer(
    connection: &Connection,
    layer: &TileLayer,
    raster_layer_number: usize,
    page_number: usize,
    shared_warnings: &[String],
    sink: &mut dyn PageConsumer,
) -> Result<(bool, Vec<String>)> {
    let (selected, mut omitted_warnings) = select_level(connection, layer)?;
    let Some(selected) = selected else {
        omitted_warnings.push(format!(
            "GeoPackage raster tile layer {} has no supported tiles at a bounded zoom level and was omitted",
            raster_layer_number
        ));
        return Ok((false, omitted_warnings));
    };
    let matrix = selected.matrix;
    let tile_span_x = f64::from(matrix.tile_width) * matrix.pixel_x_size;
    let tile_span_y = f64::from(matrix.tile_height) * matrix.pixel_y_size;
    let min_column = selected.tiles.iter().map(|tile| tile.column).min().unwrap();
    let max_column = selected.tiles.iter().map(|tile| tile.column).max().unwrap();
    let min_row = selected.tiles.iter().map(|tile| tile.row).min().unwrap();
    let max_row = selected.tiles.iter().map(|tile| tile.row).max().unwrap();
    let min_x = layer.min_x + f64::from(min_column) * tile_span_x;
    let max_x = layer.min_x + f64::from(max_column + 1) * tile_span_x;
    let max_y = layer.max_y - f64::from(min_row) * tile_span_y;
    let min_y = layer.max_y - f64::from(max_row + 1) * tile_span_y;
    if ![min_x, min_y, max_x, max_y]
        .iter()
        .all(|value| value.is_finite())
        || min_x >= max_x
        || min_y >= max_y
        || min_x < layer.min_x
        || min_y < layer.min_y
        || max_x > layer.max_x
        || max_y > layer.max_y
        || min_x < -WEB_MERCATOR_WORLD_LIMIT
        || max_x > WEB_MERCATOR_WORLD_LIMIT
        || min_y < -WEB_MERCATOR_WORLD_LIMIT
        || max_y > WEB_MERCATOR_WORLD_LIMIT
    {
        return Err(invalid(format!(
            "GeoPackage raster tile layer '{}' has tile bounds outside the EPSG:3857 world",
            layer.table_name
        )));
    }

    let world_min_x = min_x / EARTH_RADIUS_METERS;
    let world_max_x = max_x / EARTH_RADIUS_METERS;
    let world_min_y = min_y / EARTH_RADIUS_METERS;
    let world_max_y = max_y / EARTH_RADIUS_METERS;
    let span_x = world_max_x - world_min_x;
    let span_y = world_max_y - world_min_y;
    let scale =
        ((PAGE_WIDTH - PAGE_MARGIN * 2.0) / span_x).min((PAGE_HEIGHT - PAGE_MARGIN * 2.0) / span_y);
    let drawing_width = span_x * scale;
    let drawing_height = span_y * scale;
    let offset_x = PAGE_MARGIN + (PAGE_WIDTH - PAGE_MARGIN * 2.0 - drawing_width) / 2.0;
    let offset_y = PAGE_MARGIN + (PAGE_HEIGHT - PAGE_MARGIN * 2.0 - drawing_height) / 2.0;

    let mut page = Page::new(page_number, PAGE_WIDTH, PAGE_HEIGHT, "geopackage");
    page.title = format!("GeoPackage Raster Tile Layer {raster_layer_number}");
    page.description = format!(
        "EPSG:3857 tile pyramid zoom {}, {} tiles at {}x{} pixels",
        matrix.zoom_level,
        selected.tiles.len(),
        matrix.tile_width,
        matrix.tile_height
    );
    for warning in shared_warnings {
        page.warn(warning.clone());
    }
    for warning in &selected.warnings {
        page.warn(warning.clone());
    }
    let reencode_warning = "GeoPackage raster tiles are decoded and re-encoded as PNG; ICC color management is not applied";
    page.warn(reencode_warning);

    for tile in &selected.tiles {
        let tile_min_x = layer.min_x + f64::from(tile.column) * tile_span_x;
        let tile_max_y = layer.max_y - f64::from(tile.row) * tile_span_y;
        let x = offset_x + (tile_min_x / EARTH_RADIUS_METERS - world_min_x) * scale;
        let y = PAGE_HEIGHT - offset_y - (tile_max_y / EARTH_RADIUS_METERS - world_min_y) * scale;
        page.nodes.push(Node::Image {
            id: format!("geopackage-tile-{}-{}", tile.column, tile.row),
            href: tile.href.clone(),
            x,
            y,
            width: tile_span_x / EARTH_RADIUS_METERS * scale,
            height: tile_span_y / EARTH_RADIUS_METERS * scale,
            transform: IDENTITY,
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "geopackage:tile".into(),
                ..Default::default()
            },
        });
    }
    sink.consume(page)?;
    let mut warnings = selected.warnings;
    warnings.push(reencode_warning.into());
    Ok((true, warnings))
}

fn select_level(
    connection: &Connection,
    layer: &TileLayer,
) -> Result<(Option<SelectedLevel>, Vec<String>)> {
    let quoted_table = quote_identifier(&layer.table_name);
    let sql = format!(
        "SELECT tile_column, tile_row, tile_data FROM {quoted_table} WHERE zoom_level = ?1 LIMIT {}",
        MAX_TILE_ROWS + 1
    );
    let mut warnings = Vec::new();
    for matrix in layer.matrices.iter().rev() {
        let mut statement = connection.prepare(&sql).map_err(sql_error)?;
        let mut rows = statement.query([matrix.zoom_level]).map_err(sql_error)?;
        let mut raw_tiles = Vec::new();
        let mut total_input_bytes = 0usize;
        let mut exceeded_rows = false;
        while let Some(row) = rows.next().map_err(sql_error)? {
            if raw_tiles.len() >= MAX_TILE_ROWS {
                exceeded_rows = true;
                break;
            }
            let column: i64 = row.get(0).map_err(sql_error)?;
            let row_index: i64 = row.get(1).map_err(sql_error)?;
            if column < 0
                || row_index < 0
                || column >= i64::from(matrix.matrix_width)
                || row_index >= i64::from(matrix.matrix_height)
            {
                return Err(invalid(format!(
                    "GeoPackage raster tile row in '{}' is outside its declared matrix",
                    layer.table_name
                )));
            }
            let (column, row_index) = (column as u32, row_index as u32);
            let blob = match row.get_ref(2).map_err(sql_error)? {
                rusqlite::types::ValueRef::Blob(bytes) => bytes,
                _ => {
                    return Err(invalid(format!(
                        "GeoPackage raster tile in '{}' is not a BLOB",
                        layer.table_name
                    )));
                }
            };
            if blob.len() > MAX_TILE_BLOB_BYTES {
                exceeded_rows = true;
                break;
            }
            total_input_bytes = total_input_bytes.saturating_add(blob.len());
            if total_input_bytes > MAX_TILE_TOTAL_INPUT_BYTES {
                exceeded_rows = true;
                break;
            }
            raw_tiles.push((column, row_index, blob.to_vec()));
        }
        if exceeded_rows {
            warnings.push(format!(
                "GeoPackage tile zoom level {} and higher were omitted because they exceed the row or input-byte preview limit",
                matrix.zoom_level
            ));
            continue;
        }
        if raw_tiles.is_empty() {
            continue;
        }
        let pixels_per_tile = u64::from(matrix.tile_width)
            .checked_mul(u64::from(matrix.tile_height))
            .ok_or_else(|| Error::LimitExceeded("GeoPackage tile pixel count overflowed".into()))?;
        let pixel_count = pixels_per_tile
            .checked_mul(raw_tiles.len() as u64)
            .ok_or_else(|| Error::LimitExceeded("GeoPackage tile pixel count overflowed".into()))?;
        if pixel_count > MAX_TILE_TOTAL_PIXELS {
            warnings.push(format!(
                "GeoPackage tile zoom level {} and higher were omitted because they exceed {MAX_TILE_TOTAL_PIXELS} decoded pixels",
                matrix.zoom_level
            ));
            continue;
        }

        raw_tiles.sort_by_key(|(column, row, _)| (*row, *column));
        let mut positions = HashSet::with_capacity(raw_tiles.len());
        let mut budget = TileEncodingBudget {
            ..Default::default()
        };
        let mut tiles = Vec::with_capacity(raw_tiles.len());
        for (column, row, blob) in raw_tiles {
            if !positions.insert((column, row)) {
                return Err(invalid(format!(
                    "GeoPackage tile layer '{}' contains duplicate tile coordinates",
                    layer.table_name
                )));
            }
            let Some(encoded) = encode_tile_png(&blob, matrix.tile_width, matrix.tile_height)?
            else {
                budget.omitted_corrupt_tiles += 1;
                continue;
            };
            budget.png_bytes = budget.png_bytes.saturating_add(encoded.len());
            if budget.png_bytes > MAX_TILE_TOTAL_PNG_BYTES {
                warnings.push(format!(
                    "GeoPackage tile zoom level {} and higher were omitted because decoded PNG data exceeds {MAX_TILE_TOTAL_PNG_BYTES} bytes",
                    matrix.zoom_level
                ));
                tiles.clear();
                break;
            }
            let href = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(encoded));
            budget.data_uri_bytes = budget.data_uri_bytes.saturating_add(href.len());
            if budget.data_uri_bytes > MAX_SVG_DATA_URI_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "GeoPackage tile data URIs exceed {MAX_SVG_DATA_URI_BYTES} bytes"
                )));
            }
            tiles.push(EncodedTile { column, row, href });
        }
        if budget.omitted_corrupt_tiles > 0 {
            warnings.push(format!(
                "{} corrupt or unsupported GeoPackage tile image(s) at zoom {} were omitted",
                budget.omitted_corrupt_tiles, matrix.zoom_level
            ));
        }
        if !tiles.is_empty() {
            return Ok((
                Some(SelectedLevel {
                    matrix: *matrix,
                    tiles,
                    warnings,
                }),
                Vec::new(),
            ));
        }
    }
    Ok((None, warnings))
}

fn encode_tile_png(
    bytes: &[u8],
    expected_width: u32,
    expected_height: u32,
) -> Result<Option<Vec<u8>>> {
    let format = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        ImageFormat::Png
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        ImageFormat::Jpeg
    } else {
        return Ok(None);
    };
    let mut limits = Limits::default();
    limits.max_image_width = Some(expected_width.min(MAX_TILE_DIMENSION));
    limits.max_image_height = Some(expected_height.min(MAX_TILE_DIMENSION));
    limits.max_alloc = Some(MAX_TILE_IMAGE_ALLOCATION_BYTES);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(_) => return Ok(None),
    };
    if image.width() != expected_width || image.height() != expected_height {
        return Ok(None);
    }
    let mut encoded = Cursor::new(Vec::new());
    image
        .write_to(&mut encoded, ImageFormat::Png)
        .map_err(|error| invalid(format!("cannot re-encode GeoPackage raster tile: {error}")))?;
    Ok(Some(encoded.into_inner()))
}

fn validate_identifier(identifier: &str, label: &str) -> Result<()> {
    if identifier.is_empty() || identifier.len() > 256 || identifier.contains('\0') {
        return Err(invalid(format!(
            "GeoPackage {label} name is empty or too long"
        )));
    }
    Ok(())
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn sql_error(error: rusqlite::Error) -> Error {
    invalid(format!("SQLite tile query failed: {error}"))
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(format!(
        "invalid GeoPackage raster tiles: {}",
        message.into()
    ))
}
