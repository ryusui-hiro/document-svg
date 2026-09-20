//! Bounded decoder for Autodesk's binary DXF pair representation.

use std::io::{BufReader, Cursor, Read};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

use super::{parse_dxf, render_dxf_to_page};

pub(crate) const BINARY_DXF_SENTINEL: &[u8; 22] = b"AutoCAD Binary DXF\r\n\x1a\0";
const MAX_BINARY_DXF_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
const MAX_BINARY_DXF_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BINARY_DXF_STRING_BYTES: usize = 1024 * 1024;
const MAX_BINARY_DXF_PAIRS: usize = 2_500_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueKind {
    String,
    Chunk,
    Bool,
    Int16,
    Int32,
    Int64,
    Double,
}

pub(crate) fn convert<R: Read>(
    input: R,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_BINARY_DXF_INPUT_BYTES);
    let mut bytes = Vec::new();
    Read::take(input, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "binary DXF input exceeds the configured {max_bytes}-byte limit"
        )));
    }
    let ascii = decode(&bytes)?;
    let document = parse_dxf(BufReader::new(Cursor::new(ascii)))?;
    let mut warnings = Vec::new();
    let page = render_dxf_to_page(&document, &mut warnings)?;
    sink.consume(page)?;
    Ok(warnings)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<u8>> {
    if !bytes.starts_with(BINARY_DXF_SENTINEL) {
        return Err(Error::InvalidInput(
            "binary DXF sentinel is missing or truncated".into(),
        ));
    }
    let mut offset = BINARY_DXF_SENTINEL.len();
    let two_byte_group_codes = match bytes.get(offset..offset + 2) {
        Some([0, 0]) => true,
        Some([0, _]) => false,
        _ => {
            return Err(Error::InvalidInput(
                "binary DXF does not begin with group code 0".into(),
            ));
        }
    };
    let mut output = Vec::with_capacity(bytes.len().min(MAX_BINARY_DXF_OUTPUT_BYTES));
    let mut pair_count = 0usize;
    let mut saw_eof = false;
    while offset < bytes.len() {
        pair_count = pair_count.saturating_add(1);
        if pair_count > MAX_BINARY_DXF_PAIRS {
            return Err(Error::LimitExceeded(format!(
                "binary DXF contains more than {MAX_BINARY_DXF_PAIRS} group/value pairs"
            )));
        }
        let first = read_byte(bytes, &mut offset)?;
        let group = if two_byte_group_codes {
            if first == 0xff {
                read_u16(bytes, &mut offset)? as i32
            } else {
                let second = read_byte(bytes, &mut offset)?;
                i32::from(u16::from_le_bytes([first, second]))
            }
        } else {
            i32::from(first)
        };
        let kind = value_kind(group).ok_or_else(|| {
            Error::Unsupported(format!(
                "binary DXF group code {group} has an unsupported value type"
            ))
        })?;
        let value = match kind {
            ValueKind::String => read_string(bytes, &mut offset)?,
            ValueKind::Chunk => read_chunk(bytes, &mut offset)?,
            ValueKind::Bool => {
                let value = read_byte(bytes, &mut offset)?;
                if value > 1 {
                    return Err(Error::InvalidInput(format!(
                        "binary DXF boolean group {group} is not 0 or 1"
                    )));
                }
                value.to_string().into_bytes()
            }
            ValueKind::Int16 => i16::from_le_bytes(read_array::<2>(bytes, &mut offset)?)
                .to_string()
                .into_bytes(),
            ValueKind::Int32 => i32::from_le_bytes(read_array::<4>(bytes, &mut offset)?)
                .to_string()
                .into_bytes(),
            ValueKind::Int64 => i64::from_le_bytes(read_array::<8>(bytes, &mut offset)?)
                .to_string()
                .into_bytes(),
            ValueKind::Double => {
                let value = f64::from_le_bytes(read_array::<8>(bytes, &mut offset)?);
                if !value.is_finite() {
                    return Err(Error::InvalidInput(format!(
                        "binary DXF group {group} contains a non-finite number"
                    )));
                }
                value.to_string().into_bytes()
            }
        };
        if value.contains(&b'\n') || value.contains(&b'\r') {
            return Err(Error::InvalidInput(
                "binary DXF string contains a line break".into(),
            ));
        }
        append_pair(&mut output, group, &value)?;
        if group == 0 && value.eq_ignore_ascii_case(b"EOF") {
            saw_eof = true;
            break;
        }
    }
    if !saw_eof {
        return Err(Error::InvalidInput(
            "binary DXF ended before the EOF marker".into(),
        ));
    }
    Ok(output)
}

fn value_kind(group: i32) -> Option<ValueKind> {
    if (0..=9).contains(&group)
        || matches!(group, 100 | 102 | 105)
        || (300..=309).contains(&group)
        || (320..=369).contains(&group)
        || (390..=399).contains(&group)
        || (410..=419).contains(&group)
        || (430..=439).contains(&group)
        || (470..=481).contains(&group)
        || (1000..=1003).contains(&group)
        || (1005..=1009).contains(&group)
    {
        Some(ValueKind::String)
    } else if (310..=319).contains(&group) || group == 1004 {
        Some(ValueKind::Chunk)
    } else if (290..=299).contains(&group) {
        Some(ValueKind::Bool)
    } else if (60..=79).contains(&group)
        || (170..=179).contains(&group)
        || (270..=289).contains(&group)
        || (370..=389).contains(&group)
        || (400..=409).contains(&group)
        || (1060..=1070).contains(&group)
    {
        Some(ValueKind::Int16)
    } else if (90..=99).contains(&group)
        || (420..=429).contains(&group)
        || (440..=459).contains(&group)
        || group == 1071
    {
        Some(ValueKind::Int32)
    } else if (160..=169).contains(&group) {
        Some(ValueKind::Int64)
    } else if (10..=59).contains(&group)
        || (110..=149).contains(&group)
        || (210..=239).contains(&group)
        || (460..=469).contains(&group)
        || (1010..=1059).contains(&group)
    {
        Some(ValueKind::Double)
    } else {
        None
    }
}

fn append_pair(output: &mut Vec<u8>, group: i32, value: &[u8]) -> Result<()> {
    let code = format!("{group}\n");
    let new_size = output
        .len()
        .checked_add(code.len())
        .and_then(|size| size.checked_add(value.len()))
        .and_then(|size| size.checked_add(1))
        .ok_or_else(|| Error::LimitExceeded("binary DXF expanded size overflowed".into()))?;
    if new_size > MAX_BINARY_DXF_OUTPUT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "binary DXF expands beyond {MAX_BINARY_DXF_OUTPUT_BYTES} bytes"
        )));
    }
    output.extend_from_slice(code.as_bytes());
    output.extend_from_slice(value);
    output.push(b'\n');
    Ok(())
}

fn read_string(bytes: &[u8], offset: &mut usize) -> Result<Vec<u8>> {
    let start = *offset;
    let remaining = bytes.get(start..).ok_or_else(|| {
        Error::InvalidInput("binary DXF string offset is outside the input".into())
    })?;
    let length = remaining
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| {
            Error::InvalidInput("binary DXF string is missing its NUL terminator".into())
        })?;
    if length > MAX_BINARY_DXF_STRING_BYTES {
        return Err(Error::LimitExceeded(format!(
            "binary DXF string exceeds {MAX_BINARY_DXF_STRING_BYTES} bytes"
        )));
    }
    if remaining[..length].contains(&b'\n') || remaining[..length].contains(&b'\r') {
        return Err(Error::InvalidInput(
            "binary DXF string contains a line break".into(),
        ));
    }
    *offset = start + length + 1;
    Ok(remaining[..length].to_vec())
}

fn read_chunk(bytes: &[u8], offset: &mut usize) -> Result<Vec<u8>> {
    let length = read_byte(bytes, offset)? as usize;
    let chunk = read_slice(bytes, offset, length)?;
    let mut output = Vec::with_capacity(length.saturating_mul(2));
    for byte in chunk {
        output.extend_from_slice(format!("{byte:02X}").as_bytes());
    }
    Ok(output)
}

fn read_byte(bytes: &[u8], offset: &mut usize) -> Result<u8> {
    let byte = *bytes
        .get(*offset)
        .ok_or_else(|| Error::InvalidInput("binary DXF ended inside a value".into()))?;
    *offset += 1;
    Ok(byte)
}

fn read_u16(bytes: &[u8], offset: &mut usize) -> Result<u16> {
    Ok(u16::from_le_bytes(read_array::<2>(bytes, offset)?))
}

fn read_array<const N: usize>(bytes: &[u8], offset: &mut usize) -> Result<[u8; N]> {
    let slice = read_slice(bytes, offset, N)?;
    Ok(slice.try_into().unwrap())
}

fn read_slice<'a>(bytes: &'a [u8], offset: &mut usize, count: usize) -> Result<&'a [u8]> {
    let end = offset
        .checked_add(count)
        .ok_or_else(|| Error::LimitExceeded("binary DXF value length overflowed".into()))?;
    let slice = bytes
        .get(*offset..end)
        .ok_or_else(|| Error::InvalidInput("binary DXF ended inside a value".into()))?;
    *offset = end;
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_modern_extended_groups_and_binary_chunks() {
        let mut bytes = BINARY_DXF_SENTINEL.to_vec();
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(b"SECTION\0");
        // 0xFF introduces the true 16-bit extended group code 1071.
        bytes.extend_from_slice(&[0xff, 0x2f, 0x04]);
        bytes.extend_from_slice(&999_999i32.to_le_bytes());
        // Binary chunk groups carry an 8-bit length followed by bytes.
        bytes.extend_from_slice(&310u16.to_le_bytes());
        bytes.extend_from_slice(&[2, 0xaa, 0xbb]);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(b"EOF\0");
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded, b"0\nSECTION\n1071\n999999\n310\nAABB\n0\nEOF\n");
    }

    #[test]
    fn decodes_release_13_one_byte_group_codes() {
        let mut bytes = BINARY_DXF_SENTINEL.to_vec();
        bytes.extend_from_slice(&[0]);
        bytes.extend_from_slice(b"EOF\0");
        assert_eq!(decode(&bytes).unwrap(), b"0\nEOF\n");
    }

    #[test]
    fn decodes_boolean_and_signed_integer_group_types() {
        let mut bytes = BINARY_DXF_SENTINEL.to_vec();
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(b"SECTION\0");
        bytes.extend_from_slice(&70u16.to_le_bytes());
        bytes.extend_from_slice(&(-12i16).to_le_bytes());
        bytes.extend_from_slice(&90u16.to_le_bytes());
        bytes.extend_from_slice(&(-123_456i32).to_le_bytes());
        bytes.extend_from_slice(&160u16.to_le_bytes());
        bytes.extend_from_slice(&(-9_000_000_000i64).to_le_bytes());
        bytes.extend_from_slice(&290u16.to_le_bytes());
        bytes.push(1);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(b"EOF\0");
        let decoded = decode(&bytes).unwrap();
        assert_eq!(
            decoded,
            b"0\nSECTION\n70\n-12\n90\n-123456\n160\n-9000000000\n290\n1\n0\nEOF\n"
        );
    }

    #[test]
    fn rejects_truncated_binary_values() {
        let mut bytes = BINARY_DXF_SENTINEL.to_vec();
        bytes.extend_from_slice(&10u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        assert!(decode(&bytes).is_err());
    }
}
