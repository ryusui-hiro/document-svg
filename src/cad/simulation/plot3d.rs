//! Bounded formatted PLOT3D structured-grid reader.
//!
//! PLOT3D ASCII grid files contain one or more structured blocks followed by
//! X/Y(/Z) coordinate arrays.  This reader intentionally handles the formatted
//! grid subset and emits boundary quadrilaterals for the shared CAE renderer.
//! Solution (`.q`) fields, IBLANK masks, unformatted Fortran records, and
//! solver metadata remain inert or are reported as warnings.

use std::collections::HashMap;
use std::io::Read;

use super::{
    MAX_SIMULATION_CELLS, MAX_SIMULATION_POINTS, MeshCell, MeshNode, SimulationParseOutput,
    render_simulation,
};
use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

const MAX_PLOT3D_LINES: usize = 5_000_000;
const MAX_PLOT3D_LINE_BYTES: usize = 1 << 20;
const MAX_PLOT3D_BLOCKS: usize = 1_024;
const MAX_PLOT3D_VALUES: usize = 16_000_000;
const MAX_PLOT3D_DIMENSION: usize = 100_000;

pub(super) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let mut lines = text.lines().filter_map(|line| {
        let line = line.split('#').next()?.trim();
        (!line.is_empty()).then_some(line)
    });
    let first = lines.next().unwrap_or_default();
    let dims = first.split_whitespace().collect::<Vec<_>>();
    if !(dims.len() == 2 || dims.len() == 3) {
        return false;
    }
    if dims.iter().any(|token| parse_dimension(token).is_none()) {
        return false;
    }
    lines.next().is_some_and(|line| {
        line.split_whitespace()
            .all(|token| token.parse::<f64>().is_ok())
    })
}

pub(super) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut bytes = Vec::new();
    Read::take(input, options.max_input_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "PLOT3D input exceeds maximum limit of {}",
            options.max_input_bytes
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("PLOT3D ASCII is not UTF-8: {error}")))?;
    let (nodes, cells, title, warnings) = parse(&text)?;
    render_simulation((nodes, cells, title, warnings), sink)
}

fn parse(text: &str) -> Result<SimulationParseOutput> {
    if text.lines().count() > MAX_PLOT3D_LINES {
        return Err(Error::LimitExceeded(format!(
            "PLOT3D input exceeds {MAX_PLOT3D_LINES} lines"
        )));
    }
    if text.lines().any(|line| line.len() > MAX_PLOT3D_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "PLOT3D line exceeds {MAX_PLOT3D_LINE_BYTES} bytes"
        )));
    }
    let values = tokenize(text)?;
    let layout = detect_layout(&values)?;
    let mut cursor = layout.data_start;
    let mut nodes = HashMap::new();
    let mut cells = Vec::new();
    let mut warnings = Vec::new();
    let mut global_offset = 0usize;
    for block in 0..layout.dimensions.len() {
        let (ni, nj, nk) = layout.dimensions[block];
        let point_count = ni
            .checked_mul(nj)
            .and_then(|value| value.checked_mul(nk))
            .ok_or_else(|| Error::LimitExceeded("PLOT3D point count overflowed".into()))?;
        let coordinate_count = if nk == 1 {
            point_count.checked_mul(2)
        } else {
            point_count.checked_mul(3)
        }
        .ok_or_else(|| Error::LimitExceeded("PLOT3D coordinate count overflowed".into()))?;
        let end = cursor
            .checked_add(coordinate_count)
            .ok_or_else(|| Error::LimitExceeded("PLOT3D coordinate offset overflowed".into()))?;
        let coordinate_values = values.get(cursor..end).ok_or_else(|| {
            Error::InvalidInput(format!(
                "PLOT3D block {} is truncated: need {} coordinate values",
                block + 1,
                coordinate_count
            ))
        })?;
        cursor = end;
        let x_values = &coordinate_values[..point_count];
        let y_values = &coordinate_values[point_count..point_count * 2];
        let z_values = if nk == 1 {
            None
        } else {
            Some(&coordinate_values[point_count * 2..])
        };
        for local in 0..point_count {
            let x = x_values[local];
            let y = y_values[local];
            let z = z_values.map_or(0.0, |values| values[local]);
            if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                return Err(Error::InvalidInput(format!(
                    "PLOT3D block {} contains non-finite coordinates",
                    block + 1
                )));
            }
            nodes.insert(global_offset + local + 1, MeshNode { x, y, scalar: None });
        }
        let block_cells = boundary_cells(ni, nj, nk, global_offset);
        if cells.len().saturating_add(block_cells.len()) > MAX_SIMULATION_CELLS {
            return Err(Error::LimitExceeded(format!(
                "PLOT3D input exceeds {MAX_SIMULATION_CELLS} cells"
            )));
        }
        cells.extend(block_cells);
        global_offset = global_offset
            .checked_add(point_count)
            .ok_or_else(|| Error::LimitExceeded("PLOT3D node offset overflowed".into()))?;
        if nk > 1 {
            warnings.push(format!(
                "PLOT3D block {} is 3D; only its six boundary surfaces are rendered",
                block + 1
            ));
        }
    }
    if cursor < values.len() {
        warnings.push(
            "PLOT3D trailing values were not rendered (for example IBLANK or solution data)".into(),
        );
    }
    if nodes.is_empty() || cells.is_empty() {
        return Err(Error::InvalidInput(
            "PLOT3D contains no renderable structured cells".into(),
        ));
    }
    let title = format!(
        "PLOT3D structured grid ({} block(s))",
        layout.dimensions.len()
    );
    Ok((nodes, cells, title, warnings))
}

#[derive(Debug)]
struct Layout {
    dimensions: Vec<(usize, usize, usize)>,
    data_start: usize,
}

fn detect_layout(values: &[f64]) -> Result<Layout> {
    if values.is_empty() {
        return Err(Error::InvalidInput("PLOT3D input is empty".into()));
    }
    // A single-grid file with no trailing IBLANK values has an unambiguous
    // exact length. Prefer it before trying the multi-grid interpretation,
    // whose first dimension is itself an integer.
    for dimensions_count in [3usize, 2usize] {
        if values.len() < dimensions_count {
            continue;
        }
        let Some(dimensions) = parse_single_dimensions(values.get(..dimensions_count)) else {
            continue;
        };
        let (i, j, k) = dimensions[0];
        let points = i.checked_mul(j).and_then(|value| value.checked_mul(k));
        let Some(points) = points else { continue };
        let Some(required) = points.checked_mul(if k == 1 { 2 } else { 3 }) else {
            continue;
        };
        if values.len() == dimensions_count + required {
            return Ok(Layout {
                dimensions,
                data_start: dimensions_count,
            });
        }
    }

    if let Some(blocks) = integer_at(values[0])
        && (1..=MAX_PLOT3D_BLOCKS).contains(&blocks)
    {
        let dims_end = 1usize
            .checked_add(blocks.checked_mul(3).ok_or_else(|| {
                Error::LimitExceeded("PLOT3D block dimension count overflowed".into())
            })?)
            .ok_or_else(|| Error::LimitExceeded("PLOT3D dimension offset overflowed".into()))?;
        if let Some(dimensions) = parse_dimensions(values.get(1..dims_end), blocks) {
            let required = dimensions.iter().try_fold(0usize, |total, &(i, j, k)| {
                let points = i.checked_mul(j)?.checked_mul(k)?;
                total.checked_add(points.checked_mul(if k == 1 { 2 } else { 3 })?)
            });
            if let Some(required) = required
                && values.len() >= dims_end.saturating_add(required)
            {
                return Ok(Layout {
                    dimensions,
                    data_start: dims_end,
                });
            }
        }
    }
    for dimensions_count in [3usize, 2usize] {
        if values.len() < dimensions_count {
            continue;
        }
        let Some(dimensions) = parse_single_dimensions(values.get(..dimensions_count)) else {
            continue;
        };
        let (i, j, k) = dimensions[0];
        let points = i
            .checked_mul(j)
            .and_then(|value| value.checked_mul(k))
            .ok_or_else(|| Error::LimitExceeded("PLOT3D point count overflowed".into()))?;
        let required = points
            .checked_mul(if k == 1 { 2 } else { 3 })
            .ok_or_else(|| Error::LimitExceeded("PLOT3D coordinate count overflowed".into()))?;
        if values.len() >= dimensions_count.saturating_add(required) {
            return Ok(Layout {
                dimensions,
                data_start: dimensions_count,
            });
        }
    }
    Err(Error::InvalidInput(
        "PLOT3D dimensions or coordinate arrays are invalid".into(),
    ))
}

fn parse_dimensions(values: Option<&[f64]>, blocks: usize) -> Option<Vec<(usize, usize, usize)>> {
    let values = values?;
    if values.len() != blocks.checked_mul(3)? {
        return None;
    }
    let mut dimensions = Vec::with_capacity(blocks);
    for chunk in values.chunks_exact(3) {
        let i = integer_at(chunk[0])?;
        let j = integer_at(chunk[1])?;
        let k = integer_at(chunk[2])?;
        if i < 2
            || j < 2
            || k == 0
            || i > MAX_PLOT3D_DIMENSION
            || j > MAX_PLOT3D_DIMENSION
            || k > MAX_PLOT3D_DIMENSION
        {
            return None;
        }
        let points = i.checked_mul(j)?.checked_mul(k)?;
        if points > MAX_SIMULATION_POINTS {
            return None;
        }
        dimensions.push((i, j, k));
    }
    Some(dimensions)
}

fn parse_single_dimensions(values: Option<&[f64]>) -> Option<Vec<(usize, usize, usize)>> {
    let values = values?;
    let i = parse_dimension_value(*values.first()?)?;
    let j = parse_dimension_value(*values.get(1)?)?;
    let k = if values.len() == 2 {
        1
    } else if values.len() == 3 {
        parse_dimension_value(*values.get(2)?)?
    } else {
        return None;
    };
    if i < 2
        || j < 2
        || k == 0
        || i > MAX_PLOT3D_DIMENSION
        || j > MAX_PLOT3D_DIMENSION
        || k > MAX_PLOT3D_DIMENSION
    {
        return None;
    }
    let points = i.checked_mul(j)?.checked_mul(k)?;
    (points <= MAX_SIMULATION_POINTS).then_some(vec![(i, j, k)])
}

fn parse_dimension_value(value: f64) -> Option<usize> {
    integer_at(value)
}

fn boundary_cells(ni: usize, nj: usize, nk: usize, offset: usize) -> Vec<MeshCell> {
    let mut cells = Vec::new();
    let id = |i: usize, j: usize, k: usize| offset + i + ni * (j + nj * k) + 1;
    let mut add_surface = |fixed_axis: usize, fixed: usize| match fixed_axis {
        2 => {
            for j in 0..nj - 1 {
                for i in 0..ni - 1 {
                    cells.push(MeshCell {
                        node_ids: vec![
                            id(i, j, fixed),
                            id(i + 1, j, fixed),
                            id(i + 1, j + 1, fixed),
                            id(i, j + 1, fixed),
                        ],
                        scalar: None,
                    });
                }
            }
        }
        1 => {
            for k in 0..nk - 1 {
                for i in 0..ni - 1 {
                    cells.push(MeshCell {
                        node_ids: vec![
                            id(i, fixed, k),
                            id(i + 1, fixed, k),
                            id(i + 1, fixed, k + 1),
                            id(i, fixed, k + 1),
                        ],
                        scalar: None,
                    });
                }
            }
        }
        _ => {
            for k in 0..nk - 1 {
                for j in 0..nj - 1 {
                    cells.push(MeshCell {
                        node_ids: vec![
                            id(fixed, j, k),
                            id(fixed, j + 1, k),
                            id(fixed, j + 1, k + 1),
                            id(fixed, j, k + 1),
                        ],
                        scalar: None,
                    });
                }
            }
        }
    };
    if nk == 1 {
        add_surface(2, 0);
    } else {
        add_surface(2, 0);
        add_surface(2, nk - 1);
        add_surface(1, 0);
        add_surface(1, nj - 1);
        add_surface(0, 0);
        add_surface(0, ni - 1);
    }
    cells
}

fn tokenize(text: &str) -> Result<Vec<f64>> {
    let mut values = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default();
        for token in line.split_whitespace() {
            let value = token.parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!("PLOT3D token {token:?} is not numeric"))
            })?;
            if !value.is_finite() {
                return Err(Error::InvalidInput(
                    "PLOT3D contains a non-finite value".into(),
                ));
            }
            values.push(value);
            if values.len() > MAX_PLOT3D_VALUES {
                return Err(Error::LimitExceeded(format!(
                    "PLOT3D input exceeds {MAX_PLOT3D_VALUES} numeric values"
                )));
            }
        }
    }
    Ok(values)
}

fn integer_at(value: f64) -> Option<usize> {
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 {
        let integer = value as usize;
        (integer as f64 == value).then_some(integer)
    } else {
        None
    }
}

fn parse_dimension(token: &str) -> Option<usize> {
    let value = token.parse::<f64>().ok()?;
    let value = integer_at(value)?;
    (2..=MAX_PLOT3D_DIMENSION).contains(&value).then_some(value)
}
