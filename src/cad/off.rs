use std::fmt::Write as FmtWrite;
use std::io::{Cursor, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::Page;

const MAX_OFF_BYTES: u64 = 256 * 1024 * 1024;
const MAX_OFF_LINES: usize = 5_000_000;
const MAX_OFF_LINE_BYTES: usize = 1024 * 1024;
const MAX_OFF_VERTICES: usize = 1_000_000;
const MAX_OFF_FACES: usize = 200_000;
const MAX_OFF_FACE_VERTICES: usize = 100_000;
const MAX_OFF_INDICES: usize = 4_000_000;
const MAX_OBJ_TEXT_BYTES: usize = 256 * 1024 * 1024;

struct OffPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
}

impl PageConsumer for OffPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "off".into();
        page.title = "OFF polygon mesh".into();
        self.inner.consume(page)
    }
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_OFF_BYTES);
    let mut bytes = Vec::new();
    Read::take(input, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "OFF input exceeds maximum limit of {max_bytes} bytes"
        )));
    }
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::Unsupported(format!("binary OFF files are unsupported: {error}"))
    })?;
    let (obj_text, mut warnings) = parse_off(&text)?;
    if obj_text.len() as u64 > options.max_input_bytes || obj_text.len() > MAX_OBJ_TEXT_BYTES {
        return Err(Error::LimitExceeded(
            "OFF-to-OBJ intermediate mesh exceeds its bounded text limit".into(),
        ));
    }
    let mut page_sink = OffPageSink { inner: sink };
    warnings.extend(crate::cad::obj::convert(
        Cursor::new(obj_text),
        options,
        &mut page_sink,
    )?);
    Ok(warnings)
}

fn parse_off(text: &str) -> Result<(String, Vec<String>)> {
    if text.lines().count() > MAX_OFF_LINES {
        return Err(Error::LimitExceeded(format!(
            "OFF input exceeds {MAX_OFF_LINES} lines"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    let mut line_index = 0usize;
    let (header_line, header) = next_data_line(&lines, &mut line_index)?;
    let mut header_parts = header.split_whitespace();
    let first = header_parts.next().unwrap_or_default();
    let variant = first.to_ascii_uppercase();
    if !["OFF", "COFF", "NOFF", "CNOFF"].contains(&variant.as_str()) && variant.ends_with("OFF") {
        return Err(Error::Unsupported(format!(
            "OFF header variant '{first}' is unsupported; only 3D OFF/COFF/NOFF/CNOFF is rendered"
        )));
    }
    let (has_colors, has_normals, header_present) = match variant.as_str() {
        "OFF" => (false, false, true),
        "COFF" => (true, false, true),
        "NOFF" => (false, true, true),
        "CNOFF" => (true, true, true),
        _ => (false, false, false),
    };

    let mut count_tokens = Vec::<&str>::new();
    if header_present {
        count_tokens.extend(header_parts);
    } else {
        count_tokens.extend(header.split_whitespace());
    }
    while count_tokens.len() < 3 {
        let (_, line) = next_data_line(&lines, &mut line_index)?;
        count_tokens.extend(line.split_whitespace());
    }
    if count_tokens.len() != 3 {
        return Err(Error::InvalidInput(format!(
            "OFF vertex/face/edge counts at line {} must contain exactly three integers",
            header_line
        )));
    }
    let vertex_count = parse_count(count_tokens[0], "vertex", header_line, MAX_OFF_VERTICES)?;
    let face_count = parse_count(count_tokens[1], "face", header_line, MAX_OFF_FACES)?;
    let _declared_edge_count = parse_count(count_tokens[2], "edge", header_line, MAX_OFF_INDICES)?;
    if vertex_count == 0 {
        return Err(Error::Unsupported("OFF mesh contains no vertices".into()));
    }
    if face_count == 0 {
        return Err(Error::Unsupported(
            "OFF preview requires at least one polygon face".into(),
        ));
    }

    let mut obj = String::new();
    let mut warnings = Vec::new();
    let mut ignored_attributes = has_colors || has_normals;
    for vertex_id in 0..vertex_count {
        let (line_number, line) = next_data_line(&lines, &mut line_index)?;
        if line_index > MAX_OFF_LINES {
            return Err(Error::LimitExceeded(format!(
                "OFF input exceeds {MAX_OFF_LINES} lines"
            )));
        }
        let values = line.split_whitespace().collect::<Vec<_>>();
        if values.len() < 3 {
            return Err(Error::InvalidInput(format!(
                "OFF vertex {} at line {line_number} needs three coordinates",
                vertex_id + 1
            )));
        }
        let mut coordinates = [0.0; 3];
        for index in 0..3 {
            coordinates[index] = parse_coordinate(values[index], line_number)?;
        }
        ignored_attributes |= values.len() > 3;
        let old_len = obj.len();
        writeln!(
            &mut obj,
            "v {} {} {}",
            coordinates[0], coordinates[1], coordinates[2]
        )
        .map_err(|_| Error::InvalidInput("could not write OFF intermediate vertex".into()))?;
        enforce_obj_text_limit(&obj, old_len)?;
    }

    let mut total_indices = 0usize;
    for face_id in 0..face_count {
        let (line_number, line) = next_data_line(&lines, &mut line_index)?;
        let values = line.split_whitespace().collect::<Vec<_>>();
        let Some(count_text) = values.first() else {
            return Err(Error::InvalidInput(format!(
                "OFF face {} at line {line_number} is empty",
                face_id + 1
            )));
        };
        let face_vertices = parse_count(
            count_text,
            "face vertex",
            line_number,
            MAX_OFF_FACE_VERTICES,
        )?;
        if face_vertices < 3 {
            return Err(Error::InvalidInput(format!(
                "OFF face {} at line {line_number} has fewer than three vertices",
                face_id + 1
            )));
        }
        let required = face_vertices
            .checked_add(1)
            .ok_or_else(|| Error::LimitExceeded("OFF face field count overflowed".into()))?;
        if values.len() < required {
            return Err(Error::InvalidInput(format!(
                "OFF face {} at line {line_number} has an incomplete vertex list",
                face_id + 1
            )));
        }
        total_indices = total_indices
            .checked_add(face_vertices)
            .ok_or_else(|| Error::LimitExceeded("OFF face index count overflowed".into()))?;
        if total_indices > MAX_OFF_INDICES {
            return Err(Error::LimitExceeded(format!(
                "OFF total face indices exceed {MAX_OFF_INDICES}"
            )));
        }
        let old_len = obj.len();
        obj.push('f');
        for index in &values[1..required] {
            let vertex = index.parse::<usize>().map_err(|_| {
                Error::InvalidInput(format!(
                    "invalid OFF vertex index '{index}' at line {line_number}"
                ))
            })?;
            if vertex >= vertex_count {
                return Err(Error::InvalidInput(format!(
                    "OFF face {} references vertex {vertex}, outside 0..{}",
                    face_id + 1,
                    vertex_count
                )));
            }
            write!(&mut obj, " {}", vertex + 1)
                .map_err(|_| Error::InvalidInput("could not write OFF intermediate face".into()))?;
        }
        obj.push('\n');
        enforce_obj_text_limit(&obj, old_len)?;
        ignored_attributes |= values.len() > required;
    }

    while let Some((line_number, line)) = next_optional_data_line(&lines, &mut line_index)? {
        if line.eq_ignore_ascii_case("End") {
            continue;
        }
        return Err(Error::Unsupported(format!(
            "OFF contains extra data after its declared faces at line {line_number}"
        )));
    }
    if ignored_attributes {
        warnings.push(
            "OFF vertex normals/colors and face colors were ignored; geometry is rendered with the shared shaded-mesh renderer".into(),
        );
    }
    Ok((obj, warnings))
}

fn next_data_line<'a>(lines: &[&'a str], index: &mut usize) -> Result<(usize, &'a str)> {
    next_optional_data_line(lines, index)?.ok_or_else(|| {
        Error::InvalidInput("OFF input ended before all declared mesh records were read".into())
    })
}

fn next_optional_data_line<'a>(
    lines: &[&'a str],
    index: &mut usize,
) -> Result<Option<(usize, &'a str)>> {
    while *index < lines.len() {
        let line_number = *index + 1;
        let line = lines[*index].trim_end_matches('\r');
        *index += 1;
        if line.len() > MAX_OFF_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "OFF line {line_number} exceeds {MAX_OFF_LINE_BYTES} bytes"
            )));
        }
        let line = line.split('#').next().unwrap_or_default().trim();
        if !line.is_empty() {
            return Ok(Some((line_number, line)));
        }
    }
    Ok(None)
}

fn parse_count(value: &str, context: &str, line_number: usize, limit: usize) -> Result<usize> {
    let count = value.parse::<usize>().map_err(|_| {
        Error::InvalidInput(format!(
            "invalid OFF {context} count '{value}' at line {line_number}"
        ))
    })?;
    if count > limit {
        return Err(Error::LimitExceeded(format!(
            "OFF {context} count exceeds {limit}"
        )));
    }
    Ok(count)
}

fn parse_coordinate(value: &str, line_number: usize) -> Result<f64> {
    let coordinate = value.parse::<f64>().map_err(|_| {
        Error::InvalidInput(format!(
            "invalid OFF vertex coordinate '{value}' at line {line_number}"
        ))
    })?;
    if !coordinate.is_finite() || coordinate.abs() > 1.0e12 {
        return Err(Error::InvalidInput(format!(
            "OFF vertex coordinate is non-finite or outside ±1e12 at line {line_number}"
        )));
    }
    Ok(coordinate)
}

fn enforce_obj_text_limit(output: &str, old_len: usize) -> Result<()> {
    if output.len() > MAX_OBJ_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OFF-to-OBJ output exceeded {MAX_OBJ_TEXT_BYTES} bytes (record started at byte {old_len})"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_off;
    use crate::error::Error;

    #[test]
    fn converts_standard_and_count_inline_off_faces_to_obj() {
        let off = "# square\nOFF 4 1 4\n0 0 0\n1 0 0\n1 1 0\n0 1 0\n4 0 1 2 3\n";
        let (obj, warnings) = parse_off(off).unwrap();
        assert!(obj.contains("v 1 1 0"));
        assert!(obj.contains("f 1 2 3 4"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn supports_coff_and_noff_geometry_but_reports_ignored_attributes() {
        let coff =
            "COFF\n3 1 3\n0 0 0 255 0 0\n1 0 0 0 255 0\n0 1 0 0 0 255\n3 0 1 2 255 255 255\n";
        let (coff_obj, warnings) = parse_off(coff).unwrap();
        assert!(coff_obj.contains("f 1 2 3"));
        assert!(warnings.iter().any(|warning| warning.contains("colors")));

        let noff = "NOFF\n3 1 3\n0 0 0 0 0 1\n1 0 0 0 0 1\n0 1 0 0 0 1\n3 0 1 2\n";
        let (noff_obj, warnings) = parse_off(noff).unwrap();
        assert!(noff_obj.contains("f 1 2 3"));
        assert!(warnings.iter().any(|warning| warning.contains("normals")));
    }

    #[test]
    fn rejects_invalid_indices_and_unsupported_header_variants() {
        let invalid_index = "OFF\n3 1 3\n0 0 0\n1 0 0\n0 1 0\n3 0 1 9\n";
        assert!(matches!(
            parse_off(invalid_index),
            Err(Error::InvalidInput(_))
        ));
        let four_d = "4OFF\n4 1 4\n0 0 0 0\n1 0 0 0\n0 1 0 0\n0 0 1 0\n3 0 1 2\n";
        assert!(matches!(parse_off(four_d), Err(Error::Unsupported(_))));
    }
}
