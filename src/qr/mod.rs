//! QR code and barcode vector SVG generation.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke};

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "QR input")?;
    let text = String::from_utf8(bytes)
        .map_err(|e| Error::InvalidInput(format!("QR file is not valid UTF-8: {e}")))?;

    let clean_text = text.trim();
    let page = generate_qr_svg(clean_text)?;
    sink.consume(page)?;
    Ok(Vec::new())
}

/// Generates a crisp vector QR-like matrix SVG for arbitrary payload text.
pub fn generate_qr_svg(payload: &str) -> Result<Page> {
    let matrix_size = 25; // 25x25 grid (similar to QR Version 2)
    let module_size = 12.0;
    let quiet_zone = 4.0 * module_size;
    let total_size = (matrix_size as f64 * module_size) + (quiet_zone * 2.0);

    let mut grid = vec![vec![false; matrix_size]; matrix_size];

    // 1. Finder patterns at (0,0), (matrix_size-7, 0), (0, matrix_size-7)
    apply_finder_pattern(&mut grid, 0, 0);
    apply_finder_pattern(&mut grid, matrix_size - 7, 0);
    apply_finder_pattern(&mut grid, 0, matrix_size - 7);

    // 2. Timing patterns
    #[allow(clippy::needless_range_loop)]
    for i in 8..matrix_size - 8 {
        grid[6][i] = i % 2 == 0;
        grid[i][6] = i % 2 == 0;
    }

    // 3. Simple pseudo-deterministic data fill based on payload bytes hash
    let hash_bytes = payload.as_bytes();
    let mut bit_idx = 0;
    #[allow(clippy::needless_range_loop)]
    for y in 0..matrix_size {
        for x in 0..matrix_size {
            // Avoid finder zones
            if ((x < 8 || x >= matrix_size - 8) && y < 8) || (x < 8 && y >= matrix_size - 8) {
                continue;
            }
            if x == 6 || y == 6 {
                continue;
            }

            let byte = hash_bytes
                .get(bit_idx % hash_bytes.len().max(1))
                .copied()
                .unwrap_or(0);
            let bit = ((byte >> (bit_idx % 8)) & 1) == 1;
            grid[y][x] = bit ^ ((x + y) % 2 == 0); // XOR mask
            bit_idx += 1;
        }
    }

    // 4. Construct unified SVG path string with horizontal run merging
    let mut path_d = String::new();
    for (y, row) in grid.iter().enumerate() {
        let mut in_run = false;
        let mut run_start = 0;
        let py = quiet_zone + (y as f64 * module_size);

        for (x, &is_black) in row.iter().enumerate() {
            if is_black && !in_run {
                in_run = true;
                run_start = x;
            } else if !is_black && in_run {
                in_run = false;
                let run_len = (x - run_start) as f64 * module_size;
                let px = quiet_zone + (run_start as f64 * module_size);
                path_d.push_str(&format!(
                    "M {:.1},{:.1} h {:.1} v {:.1} h -{:.1} Z ",
                    px, py, run_len, module_size, run_len
                ));
            }
        }
        if in_run {
            let run_len = (row.len() - run_start) as f64 * module_size;
            let px = quiet_zone + (run_start as f64 * module_size);
            path_d.push_str(&format!(
                "M {:.1},{:.1} h {:.1} v {:.1} h -{:.1} Z ",
                px, py, run_len, module_size, run_len
            ));
        }
    }

    let mut page = Page::new(1, total_size, total_size, "qr");
    page.embedded_source = Some(payload.to_string());

    // Background white quiet zone
    let bg_d = format!("M 0,0 H {:.1} V {:.1} H 0 Z", total_size, total_size);
    page.nodes.push(Node::Path {
        id: "background".to_string(),
        d: bg_d,
        fill_rule: String::new(),
        fill: Paint::solid("#ffffff"),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta::default(),
    });

    // Modules
    page.nodes.push(Node::Path {
        id: "modules".to_string(),
        d: path_d,
        fill_rule: String::new(),
        fill: Paint::solid("#000000"),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta::default(),
    });

    Ok(page)
}

fn apply_finder_pattern(grid: &mut [Vec<bool>], start_x: usize, start_y: usize) {
    for dy in 0..7 {
        for dx in 0..7 {
            let is_outer_border = dx == 0 || dx == 6 || dy == 0 || dy == 6;
            let is_inner_center = (2..=4).contains(&dx) && (2..=4).contains(&dy);
            grid[start_y + dy][start_x + dx] = is_outer_border || is_inner_center;
        }
    }
}
