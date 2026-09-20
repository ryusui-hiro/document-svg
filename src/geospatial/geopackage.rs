//! Bounded, read-only OGC GeoPackage vector and raster-tile previews.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::limits::Limit;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::Page;

const MAX_GPKG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GPKG_WAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GPKG_SHM_BYTES: u64 = 16 * 1024 * 1024;
const MAX_GPKG_LAYERS: usize = 256;
const MAX_GPKG_FEATURES_PER_LAYER: usize = 100_000;
const MAX_GPKG_TOTAL_FEATURES: usize = 200_000;
const MAX_GPKG_POSITIONS: usize = 500_000;
const MAX_GPKG_GEOMETRIES: usize = 200_000;
const MAX_GPKG_NESTING: usize = 16;
const MAX_GPKG_GEOMETRY_BYTES: usize = 16 * 1024 * 1024;
const MAX_GPKG_PATH_IDENTIFIER_BYTES: usize = 256;
const GPKG_APPLICATION_ID: i64 = 0x4750_4B47;
const WEB_MERCATOR_LIMIT: f64 = 20_037_508.342_789_244;
const WEB_MERCATOR_RADIUS: f64 = 6_378_137.0;

#[derive(Clone, Copy, Debug)]
enum CoordinateSystem {
    Geographic,
    WebMercator,
}

#[derive(Debug)]
struct Layer {
    table_name: String,
    geometry_column: String,
    geometry_type: String,
    srs_id: i32,
    coordinates: CoordinateSystem,
}

#[derive(Default)]
struct ParseBudget {
    positions: usize,
    geometries: usize,
    has_z_or_m: bool,
    has_empty_geometry: bool,
    has_null_geometry: bool,
}

struct WkbReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    budget: &'a mut ParseBudget,
    coordinates: CoordinateSystem,
}

struct ParsedGeometry {
    kind: &'static str,
    value: Option<Value>,
}

struct SequentialPageConsumer<'a> {
    output: &'a mut dyn PageConsumer,
    next_number: usize,
}

impl PageConsumer for SequentialPageConsumer<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.number = self.next_number;
        self.next_number += 1;
        self.output.consume(page)
    }
}

pub(crate) fn looks_like_geopackage_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 72
        && bytes.starts_with(b"SQLite format 3\0")
        && matches!(&bytes[68..72], b"GPKG" | b"GP10" | b"GP11")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    check_database_file(path, options.max_input_bytes.min(MAX_GPKG_BYTES))?;
    check_sidecar(path, "-wal", MAX_GPKG_WAL_BYTES)?;
    check_sidecar(path, "-shm", MAX_GPKG_SHM_BYTES)?;

    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| invalid(format!("cannot open read-only SQLite database: {error}")))?;
    connection
        .busy_timeout(Duration::from_secs(1))
        .map_err(sql_error)?;
    let sqlite_budget_started = Instant::now();
    connection
        .progress_handler(
            1000,
            Some(move || sqlite_budget_started.elapsed() >= Duration::from_secs(5)),
        )
        .map_err(sql_error)?;
    for (limit, maximum) in [
        (Limit::SQLITE_LIMIT_LENGTH, MAX_GPKG_GEOMETRY_BYTES),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, 16 * 1024),
        (Limit::SQLITE_LIMIT_COLUMN, 512),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 64),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 32),
        (Limit::SQLITE_LIMIT_VDBE_OP, 100_000),
        (Limit::SQLITE_LIMIT_FUNCTION_ARG, 64),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 256),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 32),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        connection
            .set_limit(limit, i32::try_from(maximum).unwrap())
            .map_err(sql_error)?;
    }
    connection
        .execute_batch(
            "PRAGMA query_only = ON;
             PRAGMA trusted_schema = OFF;
             PRAGMA cell_size_check = ON;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(sql_error)?;

    validate_container(&connection)?;
    let (layers, mut warnings, has_non_feature_contents) = read_layers(&connection)?;
    let (tile_layers, tile_warnings) =
        crate::geospatial::geopackage_tiles::read_layers(&connection)?;
    warnings.extend(tile_warnings);
    if layers.is_empty() && tile_layers.is_empty() {
        return Err(Error::Unsupported(
            "GeoPackage contains no supported vector feature tables or EPSG:3857 PNG/JPEG tile layers".into(),
        ));
    }
    let layer_count = layers.len().saturating_add(tile_layers.len());
    if layer_count > MAX_GPKG_LAYERS {
        return Err(Error::LimitExceeded(format!(
            "GeoPackage exceeds {MAX_GPKG_LAYERS} supported vector and raster layers"
        )));
    }
    if layer_count > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "GeoPackage contains {layer_count} supported feature and tile layers; requested page limit is {}",
            options.max_pages
        )));
    }
    if has_non_feature_contents {
        warnings.push(
            "GeoPackage non-spatial attribute and extended-content tables are not rendered".into(),
        );
    }
    if !layers.is_empty() {
        warnings.push("GeoPackage feature attributes and identifiers are not shown".into());
    }
    let shared_page_warnings = warnings.clone();

    let mut budget = ParseBudget::default();
    let mut total_features = 0usize;
    let mut empty_layers = 0usize;
    let mut next_page_number;
    {
        let mut numbered_sink = SequentialPageConsumer {
            output: sink,
            next_number: 1,
        };
        for (index, layer) in layers.iter().enumerate() {
            let root = read_layer(&connection, layer, &mut budget, &mut total_features)?;
            let Some(root) = root else {
                empty_layers += 1;
                continue;
            };
            let mut layer_warnings = shared_page_warnings.clone();
            if matches!(layer.coordinates, CoordinateSystem::WebMercator) {
                layer_warnings.push(
                    "EPSG:3857 coordinates were inverse-projected to WGS 84 before the Web Mercator map preview".into(),
                );
            }
            if budget.has_z_or_m {
                layer_warnings.push(
                    "GeoPackage Z and M ordinates were read and validated where applicable, then omitted from the 2D preview".into(),
                );
            }
            if budget.has_empty_geometry {
                layer_warnings.push("null and empty GeoPackage geometries were omitted".into());
            } else if budget.has_null_geometry {
                layer_warnings.push("null GeoPackage geometries were omitted".into());
            }
            let title = format!("GeoPackage Feature Layer {}", index + 1);
            warnings.extend(crate::geospatial::geojson::convert_value(
                &root,
                "geopackage",
                &title,
                "GeoPackage",
                layer_warnings,
                &mut numbered_sink,
            )?);
        }
        next_page_number = numbered_sink.next_number;
    }
    if empty_layers > 0 {
        warnings.push(format!(
            "{empty_layers} empty GeoPackage feature layer(s) were omitted"
        ));
    }
    for (index, layer) in tile_layers.iter().enumerate() {
        let (rendered, tile_warnings) = crate::geospatial::geopackage_tiles::render_layer(
            &connection,
            layer,
            index + 1,
            next_page_number,
            &shared_page_warnings,
            sink,
        )?;
        warnings.extend(tile_warnings);
        if rendered {
            next_page_number += 1;
        }
    }
    if next_page_number == 1 {
        return Err(Error::InvalidInput(
            "GeoPackage contains no non-empty renderable vector or tile layers".into(),
        ));
    }
    Ok(deduplicate_warnings(warnings))
}

fn check_database_file(path: &Path, max_bytes: u64) -> Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(Error::InvalidInput(
            "GeoPackage input must be a regular file".into(),
        ));
    }
    if metadata.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "GeoPackage database is {} bytes; maximum is {max_bytes}",
            metadata.len()
        )));
    }
    if metadata.len() < 100 {
        return Err(invalid("SQLite database is shorter than its header"));
    }
    let mut prefix = [0; 72];
    fs::File::open(path)?.read_exact(&mut prefix)?;
    if !looks_like_geopackage_prefix(&prefix) {
        return Err(invalid(
            "SQLite header or GeoPackage application ID is missing",
        ));
    }
    Ok(())
}

fn check_sidecar(path: &Path, suffix: &str, maximum: u64) -> Result<()> {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    let sidecar = PathBuf::from(name);
    match fs::metadata(&sidecar) {
        Ok(metadata) if !metadata.is_file() => Err(invalid(format!(
            "SQLite sidecar {suffix} is not a regular file"
        ))),
        Ok(metadata) if metadata.len() > maximum => Err(Error::LimitExceeded(format!(
            "GeoPackage SQLite sidecar {suffix} is {} bytes; maximum is {maximum}",
            metadata.len()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::from(error)),
    }
}

fn validate_container(connection: &Connection) -> Result<()> {
    let application_id: i64 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(sql_error)?;
    if application_id != GPKG_APPLICATION_ID {
        return Err(Error::Unsupported(format!(
            "SQLite application ID {application_id:#010x} is not a GeoPackage 1.2+ container"
        )));
    }
    let user_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sql_error)?;
    if !(10_200..20_000).contains(&user_version) {
        return Err(Error::Unsupported(format!(
            "GeoPackage user_version {user_version} is not a supported 1.x version (expected 10200–19999)"
        )));
    }
    Ok(())
}

fn read_layers(connection: &Connection) -> Result<(Vec<Layer>, Vec<String>, bool)> {
    let mut contents = Vec::<String>::new();
    {
        let mut statement = connection
            .prepare("SELECT table_name FROM gpkg_contents WHERE data_type = 'features' ORDER BY table_name COLLATE NOCASE LIMIT ?1")
            .map_err(sql_error)?;
        let mut rows = statement
            .query([i64::try_from(MAX_GPKG_LAYERS + 1).unwrap()])
            .map_err(sql_error)?;
        while let Some(row) = rows.next().map_err(sql_error)? {
            contents.push(row.get(0).map_err(sql_error)?);
        }
    }
    if contents.len() > MAX_GPKG_LAYERS {
        return Err(Error::LimitExceeded(format!(
            "GeoPackage has more than {MAX_GPKG_LAYERS} feature layers"
        )));
    }

    let mut layers = Vec::with_capacity(contents.len());
    let mut warnings = Vec::new();
    let mut prior_names = Vec::new();
    let mut skipped_views = 0usize;
    for table_name in contents {
        validate_identifier(&table_name, "feature table")?;
        if prior_names
            .iter()
            .any(|prior: &String| prior.eq_ignore_ascii_case(&table_name))
        {
            return Err(invalid("GeoPackage contains duplicate feature table names"));
        }
        prior_names.push(table_name.clone());

        let schema_type: Option<(String, Option<String>)> = connection
            .query_row(
                "SELECT type, sql FROM sqlite_schema WHERE name = ?1 COLLATE NOCASE LIMIT 1",
                [&table_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?;
        let Some((schema_type, schema_sql)) = schema_type else {
            return Err(invalid(format!(
                "GeoPackage feature table '{table_name}' does not exist"
            )));
        };
        if schema_type == "view" {
            skipped_views += 1;
            continue;
        }
        if schema_type != "table"
            || schema_sql.as_deref().is_some_and(|sql| {
                sql.trim_start()
                    .to_ascii_lowercase()
                    .starts_with("create virtual table")
            })
        {
            return Err(Error::Unsupported(format!(
                "GeoPackage feature entry '{table_name}' is not a regular feature table"
            )));
        }

        let geometry: Option<(String, String, i32)> = connection
            .query_row(
                "SELECT column_name, geometry_type_name, srs_id FROM gpkg_geometry_columns WHERE table_name = ?1 LIMIT 1",
                [&table_name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sql_error)?;
        let Some((geometry_column, geometry_type, srs_id)) = geometry else {
            return Err(invalid(format!(
                "GeoPackage feature table '{table_name}' has no geometry column metadata"
            )));
        };
        validate_identifier(&geometry_column, "geometry column")?;
        if !is_supported_geometry_name(&geometry_type) {
            return Err(Error::Unsupported(format!(
                "GeoPackage geometry type '{geometry_type}' is not a supported core geometry type"
            )));
        }
        let declaration: Option<String> = {
            let mut statement = connection
                .prepare("SELECT name, type FROM pragma_table_info(?1)")
                .map_err(sql_error)?;
            let mut rows = statement.query([&table_name]).map_err(sql_error)?;
            let mut found = None;
            while let Some(row) = rows.next().map_err(sql_error)? {
                let name: String = row.get(0).map_err(sql_error)?;
                if name.eq_ignore_ascii_case(&geometry_column) {
                    found = row.get(1).map_err(sql_error)?;
                    break;
                }
            }
            found
        };
        if !declaration
            .as_deref()
            .is_some_and(|declared| declared.trim().eq_ignore_ascii_case(&geometry_type))
        {
            return Err(invalid(format!(
                "GeoPackage geometry column metadata does not match the declared column type for '{table_name}'"
            )));
        }

        let (organization, organization_coordsys_id): (String, i32) = connection
            .query_row(
                "SELECT organization, organization_coordsys_id FROM gpkg_spatial_ref_sys WHERE srs_id = ?1 LIMIT 1",
                [srs_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(sql_error)?
            .ok_or_else(|| invalid(format!("GeoPackage SRS {srs_id} is not defined")))?;
        let coordinates = if organization.eq_ignore_ascii_case("epsg") {
            match organization_coordsys_id {
                4326 | 4979 => CoordinateSystem::Geographic,
                3857 => CoordinateSystem::WebMercator,
                other => {
                    return Err(Error::Unsupported(format!(
                        "GeoPackage EPSG:{other} cannot be transformed for map preview; supported CRSs are EPSG:4326, EPSG:4979 and EPSG:3857"
                    )));
                }
            }
        } else {
            return Err(Error::Unsupported(format!(
                "GeoPackage SRS {srs_id} is not a recognized EPSG:4326/4979/3857 coordinate reference system"
            )));
        };
        layers.push(Layer {
            table_name,
            geometry_column,
            geometry_type,
            srs_id,
            coordinates,
        });
    }

    if skipped_views > 0 {
        warnings.push(format!(
            "{skipped_views} GeoPackage feature view(s) were omitted; only regular feature tables are read"
        ));
    }
    let has_non_feature_contents: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM gpkg_contents WHERE data_type NOT IN ('features', 'tiles') LIMIT 1)",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    Ok((layers, warnings, has_non_feature_contents))
}

fn read_layer(
    connection: &Connection,
    layer: &Layer,
    budget: &mut ParseBudget,
    total_features: &mut usize,
) -> Result<Option<Value>> {
    let sql = format!(
        "SELECT {} FROM {} LIMIT {}",
        quote_identifier(&layer.geometry_column),
        quote_identifier(&layer.table_name),
        MAX_GPKG_FEATURES_PER_LAYER + 1
    );
    let mut statement = connection.prepare(&sql).map_err(sql_error)?;
    let mut rows = statement.query([]).map_err(sql_error)?;
    let mut features = Vec::new();
    let mut row_count = 0usize;
    while let Some(row) = rows.next().map_err(sql_error)? {
        row_count += 1;
        if row_count > MAX_GPKG_FEATURES_PER_LAYER {
            return Err(Error::LimitExceeded(format!(
                "GeoPackage layer exceeds {MAX_GPKG_FEATURES_PER_LAYER} rows"
            )));
        }
        *total_features = total_features.saturating_add(1);
        if *total_features > MAX_GPKG_TOTAL_FEATURES {
            return Err(Error::LimitExceeded(format!(
                "GeoPackage exceeds {MAX_GPKG_TOTAL_FEATURES} feature rows"
            )));
        }
        let blob = match row.get_ref(0).map_err(sql_error)? {
            rusqlite::types::ValueRef::Null => {
                budget.has_null_geometry = true;
                continue;
            }
            rusqlite::types::ValueRef::Blob(bytes) => bytes,
            _ => return Err(invalid("GeoPackage geometry value is not a BLOB or NULL")),
        };
        if blob.len() > MAX_GPKG_GEOMETRY_BYTES {
            return Err(Error::LimitExceeded(format!(
                "GeoPackage geometry BLOB exceeds {MAX_GPKG_GEOMETRY_BYTES} bytes"
            )));
        }
        let geometry = parse_gpkg_geometry(blob, layer.srs_id, layer.coordinates, budget)?;
        if let Some(geometry) = geometry {
            if !geometry_matches_metadata(&geometry, &layer.geometry_type) {
                return Err(invalid(format!(
                    "GeoPackage geometry does not match the declared {} type in table '{}'",
                    layer.geometry_type, layer.table_name
                )));
            }
            features.push(json!({
                "type": "Feature",
                "geometry": geometry,
                "properties": null
            }));
        }
    }
    if features.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        json!({ "type": "FeatureCollection", "features": features }),
    ))
}

fn parse_gpkg_geometry(
    blob: &[u8],
    expected_srs: i32,
    coordinates: CoordinateSystem,
    budget: &mut ParseBudget,
) -> Result<Option<Value>> {
    if blob.len() < 8 || blob.get(..2) != Some(b"GP") {
        return Err(invalid("geometry BLOB has no GeoPackageBinary header"));
    }
    if blob[2] != 0 {
        return Err(Error::Unsupported(format!(
            "GeoPackageBinary version {} is unsupported",
            blob[2]
        )));
    }
    let flags = blob[3];
    if flags & 0xc0 != 0 {
        return Err(invalid("GeoPackageBinary reserved flag bits are set"));
    }
    if flags & 0x20 != 0 {
        return Err(Error::Unsupported(
            "ExtendedGeoPackageBinary user-defined geometry types are unsupported".into(),
        ));
    }
    let little_endian = flags & 1 != 0;
    let envelope_code = (flags >> 1) & 0x07;
    let empty_flag = flags & 0x10 != 0;
    let envelope_values = match envelope_code {
        0 => 0,
        1 => 4,
        2 | 3 => 6,
        4 => 8,
        _ => return Err(invalid("GeoPackageBinary envelope code is invalid")),
    };
    let mut cursor = 4usize;
    let srs_id = read_i32(blob, &mut cursor, little_endian)?;
    if srs_id != expected_srs {
        return Err(invalid(format!(
            "GeoPackageBinary SRS {srs_id} does not match geometry column SRS {expected_srs}"
        )));
    }
    let mut envelope = Vec::with_capacity(envelope_values);
    for _ in 0..envelope_values {
        let value = read_f64(blob, &mut cursor, little_endian)?;
        if !value.is_finite() {
            return Err(invalid(
                "GeoPackageBinary envelope contains a non-finite value",
            ));
        }
        envelope.push(value);
    }
    if envelope.len() >= 4 && (envelope[0] > envelope[1] || envelope[2] > envelope[3]) {
        return Err(invalid("GeoPackageBinary XY envelope bounds are inverted"));
    }
    if empty_flag && envelope_code != 0 {
        return Err(invalid(
            "empty GeoPackage geometry has a non-empty envelope",
        ));
    }
    if cursor >= blob.len() {
        return Err(invalid("GeoPackageBinary geometry payload is missing"));
    }
    let mut reader = WkbReader {
        bytes: blob,
        cursor,
        budget,
        coordinates,
    };
    let parsed = reader.parse_geometry(0)?;
    if reader.cursor != blob.len() {
        return Err(invalid("GeoPackageBinary geometry has trailing bytes"));
    }
    if empty_flag != parsed.value.is_none() {
        return Err(invalid(
            "GeoPackageBinary empty flag does not match the WKB geometry",
        ));
    }
    if empty_flag {
        reader.budget.has_empty_geometry = true;
    }
    Ok(parsed.value)
}

impl WkbReader<'_> {
    fn parse_geometry(&mut self, depth: usize) -> Result<ParsedGeometry> {
        if depth > MAX_GPKG_NESTING {
            return Err(Error::LimitExceeded(format!(
                "GeoPackage WKB geometry nesting exceeds {MAX_GPKG_NESTING}"
            )));
        }
        self.budget.geometries = self.budget.geometries.saturating_add(1);
        if self.budget.geometries > MAX_GPKG_GEOMETRIES {
            return Err(Error::LimitExceeded(format!(
                "GeoPackage exceeds {MAX_GPKG_GEOMETRIES} WKB geometries"
            )));
        }
        let endian = match self.read_u8()? {
            0 => false,
            1 => true,
            _ => return Err(invalid("WKB byte-order marker must be 0 or 1")),
        };
        let encoded_type = self.read_u32(endian)?;
        let (base_type, dimensions) = decode_wkb_type(encoded_type)?;
        self.budget.has_z_or_m |= dimensions > 2;
        let (kind, value) = match base_type {
            1 => {
                let point = self.read_position(endian, dimensions)?;
                (
                    "Point",
                    point.map(|position| json!({ "type": "Point", "coordinates": position })),
                )
            }
            2 => {
                let count = self.read_count(endian, dimensions * 8)?;
                if count == 1 {
                    return Err(invalid(
                        "WKB LineString must contain zero or at least two positions",
                    ));
                }
                let positions = self.read_positions(endian, count, dimensions)?;
                let value = (!positions.is_empty())
                    .then(|| json!({ "type": "LineString", "coordinates": positions }));
                ("LineString", value)
            }
            3 => {
                let ring_count = self.read_count(endian, 0)?;
                let mut rings = Vec::with_capacity(ring_count.min(4096));
                for _ in 0..ring_count {
                    let point_count = self.read_count(endian, dimensions * 8)?;
                    if point_count == 0 {
                        return Err(invalid("WKB polygon contains an empty linear ring"));
                    }
                    rings.push(self.read_positions(endian, point_count, dimensions)?);
                }
                let value =
                    (!rings.is_empty()).then(|| json!({ "type": "Polygon", "coordinates": rings }));
                ("Polygon", value)
            }
            4..=6 => {
                let count = self.read_count(endian, 5)?;
                let expected = match base_type {
                    4 => "Point",
                    5 => "LineString",
                    _ => "Polygon",
                };
                let mut members = Vec::with_capacity(count.min(4096));
                for _ in 0..count {
                    let member = self.parse_geometry(depth + 1)?;
                    if member.kind != expected {
                        return Err(invalid(format!(
                            "WKB Multi{} contains a {} member",
                            match base_type {
                                4 => "Point",
                                5 => "LineString",
                                _ => "Polygon",
                            },
                            member.kind
                        )));
                    }
                    if let Some(value) = member.value {
                        let coordinates = value.get("coordinates").cloned().ok_or_else(|| {
                            invalid("WKB multi-geometry member has no coordinates")
                        })?;
                        members.push(coordinates);
                    }
                }
                let kind = match base_type {
                    4 => "MultiPoint",
                    5 => "MultiLineString",
                    _ => "MultiPolygon",
                };
                let value =
                    (!members.is_empty()).then(|| json!({ "type": kind, "coordinates": members }));
                (kind, value)
            }
            7 => {
                let count = self.read_count(endian, 5)?;
                let mut members = Vec::with_capacity(count.min(4096));
                for _ in 0..count {
                    if let Some(value) = self.parse_geometry(depth + 1)?.value {
                        members.push(value);
                    }
                }
                let value = (!members.is_empty())
                    .then(|| json!({ "type": "GeometryCollection", "geometries": members }));
                ("GeometryCollection", value)
            }
            _ => unreachable!("decode_wkb_type rejects unsupported values"),
        };
        if value.is_none() {
            self.budget.has_empty_geometry = true;
        }
        Ok(ParsedGeometry { kind, value })
    }

    fn read_position(&mut self, endian: bool, dimensions: usize) -> Result<Option<Vec<f64>>> {
        let x = self.read_f64(endian)?;
        let y = self.read_f64(endian)?;
        for _ in 2..dimensions {
            let ordinate = self.read_f64(endian)?;
            if !(ordinate.is_finite() || x.is_nan() && y.is_nan()) {
                return Err(invalid("WKB Z/M ordinates must be finite"));
            }
        }
        if x.is_nan() && y.is_nan() {
            self.budget.has_empty_geometry = true;
            return Ok(None);
        }
        if !x.is_finite() || !y.is_finite() {
            return Err(invalid("WKB XY coordinates must be finite"));
        }
        self.budget.positions = self.budget.positions.saturating_add(1);
        if self.budget.positions > MAX_GPKG_POSITIONS {
            return Err(Error::LimitExceeded(format!(
                "GeoPackage exceeds {MAX_GPKG_POSITIONS} coordinate positions"
            )));
        }
        let [x, y] = project_coordinate(x, y, self.coordinates)?;
        Ok(Some(vec![x, y]))
    }

    fn read_positions(
        &mut self,
        endian: bool,
        count: usize,
        dimensions: usize,
    ) -> Result<Vec<Vec<f64>>> {
        let mut positions = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            let Some(position) = self.read_position(endian, dimensions)? else {
                return Err(invalid(
                    "empty-point NaN coordinates are not valid inside a line or ring",
                ));
            };
            positions.push(position);
        }
        Ok(positions)
    }

    fn read_count(&mut self, endian: bool, minimum_child_bytes: usize) -> Result<usize> {
        let count = usize::try_from(self.read_u32(endian)?)
            .map_err(|_| invalid("WKB element count overflows this platform"))?;
        if count > MAX_GPKG_POSITIONS {
            return Err(Error::LimitExceeded(format!(
                "GeoPackage WKB element count exceeds {MAX_GPKG_POSITIONS}"
            )));
        }
        if minimum_child_bytes > 0
            && count
                .checked_mul(minimum_child_bytes)
                .is_none_or(|minimum| minimum > self.bytes.len().saturating_sub(self.cursor))
        {
            return Err(invalid("WKB element count exceeds the remaining payload"));
        }
        Ok(count)
    }

    fn read_u8(&mut self) -> Result<u8> {
        let value = self
            .bytes
            .get(self.cursor)
            .copied()
            .ok_or_else(|| invalid("truncated WKB geometry"))?;
        self.cursor += 1;
        Ok(value)
    }

    fn read_u32(&mut self, endian: bool) -> Result<u32> {
        let bytes = self.take::<4>()?;
        Ok(if endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    }

    fn read_f64(&mut self, endian: bool) -> Result<f64> {
        let bytes = self.take::<8>()?;
        Ok(if endian {
            f64::from_le_bytes(bytes)
        } else {
            f64::from_be_bytes(bytes)
        })
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self
            .cursor
            .checked_add(N)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| invalid("truncated WKB geometry"))?;
        let value = self.bytes[self.cursor..end].try_into().unwrap();
        self.cursor = end;
        Ok(value)
    }
}

fn decode_wkb_type(encoded: u32) -> Result<(u32, usize)> {
    let (base_type, dimensions) = match encoded {
        1..=7 => (encoded, 2),
        1001..=1007 => (encoded - 1000, 3),
        2001..=2007 => (encoded - 2000, 3),
        3001..=3007 => (encoded - 3000, 4),
        _ => {
            return Err(Error::Unsupported(format!(
                "WKB geometry type code {encoded} is unsupported"
            )));
        }
    };
    Ok((base_type, dimensions))
}

fn project_coordinate(x: f64, y: f64, coordinates: CoordinateSystem) -> Result<[f64; 2]> {
    let (longitude, latitude) = match coordinates {
        CoordinateSystem::Geographic => (x, y),
        CoordinateSystem::WebMercator => {
            if x.abs() > WEB_MERCATOR_LIMIT || y.abs() > WEB_MERCATOR_LIMIT {
                return Err(invalid(
                    "EPSG:3857 coordinate is outside the Web Mercator world bounds",
                ));
            }
            (
                x / WEB_MERCATOR_RADIUS * (180.0 / std::f64::consts::PI),
                (2.0 * (y / WEB_MERCATOR_RADIUS).exp().atan() - std::f64::consts::FRAC_PI_2)
                    * (180.0 / std::f64::consts::PI),
            )
        }
    };
    if !longitude.is_finite()
        || !latitude.is_finite()
        || !(-180.0..=180.0).contains(&longitude)
        || !(-90.0..=90.0).contains(&latitude)
    {
        return Err(invalid(
            "GeoPackage geometry is outside the supported WGS 84 longitude/latitude range",
        ));
    }
    Ok([longitude, latitude])
}

fn read_i32(bytes: &[u8], cursor: &mut usize, little_endian: bool) -> Result<i32> {
    let raw = read_u32(bytes, cursor, little_endian)?;
    Ok(raw as i32)
}

fn read_u32(bytes: &[u8], cursor: &mut usize, little_endian: bool) -> Result<u32> {
    let end = cursor
        .checked_add(4)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| invalid("truncated GeoPackageBinary header"))?;
    let array: [u8; 4] = bytes[*cursor..end].try_into().unwrap();
    *cursor = end;
    Ok(if little_endian {
        u32::from_le_bytes(array)
    } else {
        u32::from_be_bytes(array)
    })
}

fn read_f64(bytes: &[u8], cursor: &mut usize, little_endian: bool) -> Result<f64> {
    let end = cursor
        .checked_add(8)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| invalid("truncated GeoPackageBinary envelope"))?;
    let array: [u8; 8] = bytes[*cursor..end].try_into().unwrap();
    *cursor = end;
    Ok(if little_endian {
        f64::from_le_bytes(array)
    } else {
        f64::from_be_bytes(array)
    })
}

fn validate_identifier(identifier: &str, label: &str) -> Result<()> {
    if identifier.is_empty()
        || identifier.len() > MAX_GPKG_PATH_IDENTIFIER_BYTES
        || identifier.contains('\0')
    {
        return Err(invalid(format!(
            "GeoPackage {label} name is empty or too long"
        )));
    }
    Ok(())
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn is_supported_geometry_name(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "GEOMETRY"
            | "POINT"
            | "LINESTRING"
            | "POLYGON"
            | "MULTIPOINT"
            | "MULTILINESTRING"
            | "MULTIPOLYGON"
            | "GEOMETRYCOLLECTION"
    )
}

fn geometry_matches_metadata(geometry: &Value, declared_type: &str) -> bool {
    let Some(kind) = geometry.get("type").and_then(Value::as_str) else {
        return false;
    };
    declared_type.eq_ignore_ascii_case("GEOMETRY") || declared_type.eq_ignore_ascii_case(kind)
}

fn deduplicate_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut unique = Vec::new();
    for warning in warnings {
        if !unique.iter().any(|item| item == &warning) {
            unique.push(warning);
        }
    }
    unique
}

fn sql_error(error: rusqlite::Error) -> Error {
    invalid(format!("SQLite read failed: {error}"))
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(format!("invalid GeoPackage: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point_blob(srs_id: i32, x: f64, y: f64) -> Vec<u8> {
        let mut blob = b"GP\0\x01".to_vec();
        blob.extend(srs_id.to_le_bytes());
        blob.push(1);
        blob.extend(1u32.to_le_bytes());
        blob.extend(x.to_le_bytes());
        blob.extend(y.to_le_bytes());
        blob
    }

    fn wrap_wkb(wkb: Vec<u8>) -> Vec<u8> {
        let mut blob = b"GP\0\x01".to_vec();
        blob.extend(4326i32.to_le_bytes());
        blob.extend(wkb);
        blob
    }

    fn wkb_point(x: f64, y: f64) -> Vec<u8> {
        let mut wkb = vec![1];
        wkb.extend(1u32.to_le_bytes());
        wkb.extend(x.to_le_bytes());
        wkb.extend(y.to_le_bytes());
        wkb
    }

    fn wkb_line(points: &[[f64; 2]]) -> Vec<u8> {
        let mut wkb = vec![1];
        wkb.extend(2u32.to_le_bytes());
        wkb.extend((points.len() as u32).to_le_bytes());
        for [x, y] in points {
            wkb.extend(x.to_le_bytes());
            wkb.extend(y.to_le_bytes());
        }
        wkb
    }

    #[test]
    fn detects_geo_package_sqlite_header_without_filename_extension() {
        let mut prefix = vec![0; 72];
        prefix[..16].copy_from_slice(b"SQLite format 3\0");
        prefix[68..72].copy_from_slice(b"GPKG");
        assert!(looks_like_geopackage_prefix(&prefix));
        prefix[68..72].copy_from_slice(b"SQLT");
        assert!(!looks_like_geopackage_prefix(&prefix));
    }

    #[test]
    fn parses_little_and_big_endian_geopackage_points() {
        for little_endian in [true, false] {
            let mut blob = b"GP\0".to_vec();
            blob.push(u8::from(little_endian));
            blob.extend(if little_endian {
                4326i32.to_le_bytes()
            } else {
                4326i32.to_be_bytes()
            });
            blob.push(u8::from(little_endian));
            blob.extend(if little_endian {
                1u32.to_le_bytes()
            } else {
                1u32.to_be_bytes()
            });
            for coordinate in [139.7f64, 35.6f64] {
                blob.extend(if little_endian {
                    coordinate.to_le_bytes()
                } else {
                    coordinate.to_be_bytes()
                });
            }
            let parsed = parse_gpkg_geometry(
                &blob,
                4326,
                CoordinateSystem::Geographic,
                &mut ParseBudget::default(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(parsed["coordinates"][0], 139.7);
            assert_eq!(parsed["coordinates"][1], 35.6);
        }
    }

    #[test]
    fn inverse_projects_epsg_3857_coordinates() {
        let coordinate = project_coordinate(
            15_551_332.863_820_316,
            4_245_720.660_441_585,
            CoordinateSystem::WebMercator,
        )
        .unwrap();
        assert!((coordinate[0] - 139.7).abs() < 0.01);
        assert!((coordinate[1] - 35.6).abs() < 0.01);
        assert!(project_coordinate(20_100_000.0, 0.0, CoordinateSystem::WebMercator).is_err());
    }

    #[test]
    fn rejects_invalid_envelopes_and_header_srs_mismatches() {
        let mut blob = point_blob(4326, 1.0, 2.0);
        blob[3] = 0x05; // little endian plus XY envelope
        let error = parse_gpkg_geometry(
            &blob,
            4326,
            CoordinateSystem::Geographic,
            &mut ParseBudget::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("envelope"));

        assert!(
            parse_gpkg_geometry(
                &point_blob(3857, 1.0, 2.0),
                4326,
                CoordinateSystem::Geographic,
                &mut ParseBudget::default(),
            )
            .is_err()
        );
    }

    #[test]
    fn validates_iso_wkb_dimension_type_codes() {
        assert_eq!(decode_wkb_type(1001).unwrap(), (1, 3));
        assert_eq!(decode_wkb_type(2002).unwrap(), (2, 3));
        assert_eq!(decode_wkb_type(3003).unwrap(), (3, 4));
        assert!(decode_wkb_type(0x8000_0001).is_err());
    }

    #[test]
    fn parses_lines_multi_geometries_and_geometry_collections() {
        let line = wkb_line(&[[139.0, 35.0], [140.0, 36.0]]);
        let parsed = parse_gpkg_geometry(
            &wrap_wkb(line.clone()),
            4326,
            CoordinateSystem::Geographic,
            &mut ParseBudget::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(parsed["type"], "LineString");
        assert_eq!(parsed["coordinates"].as_array().unwrap().len(), 2);

        let point_a = wkb_point(139.0, 35.0);
        let point_b = wkb_point(140.0, 36.0);
        let mut multi_point = vec![1];
        multi_point.extend(4u32.to_le_bytes());
        multi_point.extend(2u32.to_le_bytes());
        multi_point.extend(point_a.clone());
        multi_point.extend(point_b);
        let parsed = parse_gpkg_geometry(
            &wrap_wkb(multi_point),
            4326,
            CoordinateSystem::Geographic,
            &mut ParseBudget::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(parsed["type"], "MultiPoint");
        assert_eq!(parsed["coordinates"].as_array().unwrap().len(), 2);

        let mut collection = vec![1];
        collection.extend(7u32.to_le_bytes());
        collection.extend(2u32.to_le_bytes());
        collection.extend(point_a);
        collection.extend(line);
        let parsed = parse_gpkg_geometry(
            &wrap_wkb(collection),
            4326,
            CoordinateSystem::Geographic,
            &mut ParseBudget::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(parsed["type"], "GeometryCollection");
        assert_eq!(parsed["geometries"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn quotes_sql_identifiers_with_embedded_quotes() {
        assert_eq!(
            quote_identifier("roads\"; DROP TABLE roads;--"),
            "\"roads\"\"; DROP TABLE roads;--\""
        );
    }
}
