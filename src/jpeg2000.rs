//! Bounded header inspection for JPEG 2000 codestreams and JP2 containers.
//!
//! This parses only the marker/box headers needed to validate the image shape
//! before passing pixel data to the decoder.

use crate::error::{Error, Result};

const JP2_SIGNATURE: &[u8] = b"\x00\x00\x00\x0cjP  \r\n\x87\n";
const JP2_SIGNATURE_PAYLOAD: &[u8] = b"\r\n\x87\n";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Jpeg2000ComponentHeader {
    pub(crate) precision: u16,
    pub(crate) signed: bool,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Jpeg2000ImageHeader {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) components: Vec<Jpeg2000ComponentHeader>,
}

pub(crate) fn parse_raw_codestream(bytes: &[u8]) -> Result<Jpeg2000ImageHeader> {
    if !bytes.starts_with(&[0xff, 0x4f]) {
        return Err(Error::InvalidInput(
            "JPEG 2000 frame does not begin with a raw codestream SOC marker".into(),
        ));
    }
    let mut offset = 2usize;
    while offset < bytes.len() {
        if bytes.get(offset) != Some(&0xff) {
            return Err(Error::InvalidInput(
                "malformed JPEG 2000 main-header marker sequence".into(),
            ));
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes
            .get(offset)
            .ok_or_else(|| Error::InvalidInput("truncated JPEG 2000 marker".into()))?;
        offset += 1;
        if marker == 0x51 {
            return parse_siz(bytes, offset);
        }
        if marker == 0x93 || marker == 0xd9 || marker == 0x90 {
            break;
        }
        if marker == 0x01 {
            continue;
        }
        let segment_length = usize::from(read_be_u16(bytes, offset).ok_or_else(|| {
            Error::InvalidInput("truncated JPEG 2000 marker segment length".into())
        })?);
        if segment_length < 2 {
            return Err(Error::InvalidInput(
                "invalid JPEG 2000 marker segment length".into(),
            ));
        }
        offset = offset
            .checked_add(segment_length)
            .ok_or_else(|| Error::InvalidInput("JPEG 2000 marker offset overflowed".into()))?;
        if offset > bytes.len() {
            return Err(Error::InvalidInput(
                "truncated JPEG 2000 marker segment".into(),
            ));
        }
    }
    Err(Error::InvalidInput(
        "JPEG 2000 frame has no SIZ image and component header".into(),
    ))
}

/// Validate a JP2 file/container or raw JPEG 2000 codestream before decoding.
pub(crate) fn parse_jpx_header(bytes: &[u8]) -> Result<Jpeg2000ImageHeader> {
    if bytes.starts_with(&[0xff, 0x4f]) {
        return parse_raw_codestream(bytes);
    }
    if !bytes.starts_with(JP2_SIGNATURE) {
        return Err(Error::InvalidInput(
            "JPXDecode data is neither a raw JPEG 2000 codestream nor a JP2 file".into(),
        ));
    }
    let (signature_type, signature_payload, signature_end) = parse_box(bytes, 0)?;
    if signature_type != *b"jP  "
        || bytes.get(signature_payload..signature_end) != Some(JP2_SIGNATURE_PAYLOAD)
    {
        return Err(Error::InvalidInput("invalid JP2 signature box".into()));
    }

    let mut offset = signature_end;
    let mut image_header = None::<(u32, u32, usize)>;
    let mut codestream = None::<&[u8]>;
    while offset < bytes.len() {
        let (box_type, payload_start, end) = parse_box(bytes, offset)?;
        if box_type == *b"jp2h" {
            image_header = Some(parse_jp2_header_boxes(bytes, payload_start, end)?);
        } else if box_type == *b"jp2c" {
            if codestream.is_some() {
                return Err(Error::Unsupported(
                    "multi-codestream JP2 images are not supported".into(),
                ));
            }
            codestream = Some(&bytes[payload_start..end]);
        }
        if end == bytes.len() {
            break;
        }
        offset = end;
    }

    let (container_width, container_height, container_components) = image_header
        .ok_or_else(|| Error::InvalidInput("JP2 file has no jp2h/ihdr image header".into()))?;
    let codestream =
        codestream.ok_or_else(|| Error::InvalidInput("JP2 file has no jp2c codestream".into()))?;
    let header = parse_raw_codestream(codestream)?;
    if header.width != container_width
        || header.height != container_height
        || header.components.len() != container_components
    {
        return Err(Error::InvalidInput(
            "JP2 ihdr dimensions/components do not match its JPEG 2000 codestream".into(),
        ));
    }
    Ok(header)
}

pub(crate) fn jpx_media_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(JP2_SIGNATURE) {
        "image/jp2"
    } else {
        // IANA registers image/j2c specifically for one raw JPEG 2000
        // codestream; image/jp2 denotes the boxed file format.
        "image/j2c"
    }
}

/// Recognize a JPEG 2000 raw codestream or JP2 signature from a bounded prefix.
pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0x4f]) || bytes.starts_with(JP2_SIGNATURE)
}

fn parse_jp2_header_boxes(
    bytes: &[u8],
    mut offset: usize,
    end: usize,
) -> Result<(u32, u32, usize)> {
    let mut header = None;
    while offset < end {
        let (box_type, payload_start, box_end) = parse_box_with_end(bytes, offset, end)?;
        if box_type == *b"ihdr" {
            if box_end.saturating_sub(payload_start) < 14 {
                return Err(Error::InvalidInput("truncated JP2 ihdr box".into()));
            }
            let height = read_be_u32(bytes, payload_start)
                .ok_or_else(|| Error::InvalidInput("truncated JP2 ihdr height".into()))?;
            let width = read_be_u32(bytes, payload_start + 4)
                .ok_or_else(|| Error::InvalidInput("truncated JP2 ihdr width".into()))?;
            let components = usize::from(
                read_be_u16(bytes, payload_start + 8)
                    .ok_or_else(|| Error::InvalidInput("truncated JP2 ihdr components".into()))?,
            );
            if width == 0 || height == 0 || components == 0 {
                return Err(Error::InvalidInput(
                    "invalid JP2 ihdr dimensions/components".into(),
                ));
            }
            if header.replace((width, height, components)).is_some() {
                return Err(Error::InvalidInput(
                    "JP2 jp2h box has duplicate ihdr boxes".into(),
                ));
            }
        }
        offset = box_end;
    }
    header.ok_or_else(|| Error::InvalidInput("JP2 jp2h box contains no ihdr box".into()))
}

fn parse_box(bytes: &[u8], offset: usize) -> Result<([u8; 4], usize, usize)> {
    parse_box_with_end(bytes, offset, bytes.len())
}

fn parse_box_with_end(
    bytes: &[u8],
    offset: usize,
    limit: usize,
) -> Result<([u8; 4], usize, usize)> {
    if offset.checked_add(8).is_none_or(|end| end > limit) {
        return Err(Error::InvalidInput("truncated JP2 box header".into()));
    }
    let length = read_be_u32(bytes, offset)
        .ok_or_else(|| Error::InvalidInput("truncated JP2 box length".into()))?;
    let box_type: [u8; 4] = bytes[offset + 4..offset + 8]
        .try_into()
        .map_err(|_| Error::InvalidInput("truncated JP2 box type".into()))?;
    let (header_size, box_size) = match length {
        0 => (8usize, limit.saturating_sub(offset)),
        1 => {
            let extended = read_be_u64(bytes, offset + 8)
                .ok_or_else(|| Error::InvalidInput("truncated extended JP2 box length".into()))?;
            let extended = usize::try_from(extended).map_err(|_| {
                Error::LimitExceeded("JP2 box length does not fit this platform".into())
            })?;
            (16usize, extended)
        }
        length => (8usize, length as usize),
    };
    if box_size < header_size {
        return Err(Error::InvalidInput(
            "JP2 box length is smaller than its header".into(),
        ));
    }
    let end = offset
        .checked_add(box_size)
        .ok_or_else(|| Error::LimitExceeded("JP2 box end offset overflowed".into()))?;
    if end > limit {
        return Err(Error::InvalidInput(
            "JP2 box extends past its container".into(),
        ));
    }
    Ok((box_type, offset + header_size, end))
}

fn parse_siz(bytes: &[u8], length_offset: usize) -> Result<Jpeg2000ImageHeader> {
    let segment_length = usize::from(
        read_be_u16(bytes, length_offset)
            .ok_or_else(|| Error::InvalidInput("truncated JPEG 2000 SIZ segment length".into()))?,
    );
    if segment_length < 38 {
        return Err(Error::InvalidInput(
            "JPEG 2000 SIZ segment is shorter than its fixed fields".into(),
        ));
    }
    let segment_start = length_offset + 2;
    let segment_end = length_offset
        .checked_add(segment_length)
        .ok_or_else(|| Error::InvalidInput("JPEG 2000 SIZ offset overflowed".into()))?;
    if segment_end > bytes.len() {
        return Err(Error::InvalidInput(
            "truncated JPEG 2000 SIZ segment".into(),
        ));
    }
    let image_width_end = read_be_u32(bytes, segment_start + 2).unwrap_or(0);
    let image_height_end = read_be_u32(bytes, segment_start + 6).unwrap_or(0);
    let image_x_origin = read_be_u32(bytes, segment_start + 10).unwrap_or(u32::MAX);
    let image_y_origin = read_be_u32(bytes, segment_start + 14).unwrap_or(u32::MAX);
    let tile_width = read_be_u32(bytes, segment_start + 18).unwrap_or(0);
    let tile_height = read_be_u32(bytes, segment_start + 22).unwrap_or(0);
    let tile_x_origin = read_be_u32(bytes, segment_start + 26).unwrap_or(u32::MAX);
    let tile_y_origin = read_be_u32(bytes, segment_start + 30).unwrap_or(u32::MAX);
    let component_count = usize::from(read_be_u16(bytes, segment_start + 34).unwrap_or(0));
    let expected_length =
        38usize
            .checked_add(component_count.checked_mul(3).ok_or_else(|| {
                Error::InvalidInput("JPEG 2000 component count overflowed".into())
            })?)
            .ok_or_else(|| Error::InvalidInput("JPEG 2000 SIZ length overflowed".into()))?;
    if component_count == 0 || expected_length != segment_length {
        return Err(Error::InvalidInput(
            "JPEG 2000 SIZ component table has an invalid length".into(),
        ));
    }
    if image_width_end <= image_x_origin
        || image_height_end <= image_y_origin
        || tile_width == 0
        || tile_height == 0
        || tile_x_origin > image_x_origin
        || tile_y_origin > image_y_origin
    {
        return Err(Error::InvalidInput(
            "JPEG 2000 SIZ image or tile dimensions are invalid".into(),
        ));
    }
    let width = image_width_end - image_x_origin;
    let height = image_height_end - image_y_origin;
    let component_data_start = segment_start + 36;
    let mut components = Vec::with_capacity(component_count);
    for component_index in 0..component_count {
        let start = component_data_start + component_index * 3;
        let sample_format = bytes[start];
        let horizontal_subsampling = u64::from(bytes[start + 1]);
        let vertical_subsampling = u64::from(bytes[start + 2]);
        if horizontal_subsampling == 0 || vertical_subsampling == 0 {
            return Err(Error::InvalidInput(
                "JPEG 2000 component has a zero subsampling factor".into(),
            ));
        }
        let component_width = u64::from(image_width_end).div_ceil(horizontal_subsampling)
            - u64::from(image_x_origin).div_ceil(horizontal_subsampling);
        let component_height = u64::from(image_height_end).div_ceil(vertical_subsampling)
            - u64::from(image_y_origin).div_ceil(vertical_subsampling);
        components.push(Jpeg2000ComponentHeader {
            precision: u16::from(sample_format & 0x7f) + 1,
            signed: sample_format & 0x80 != 0,
            width: u32::try_from(component_width)
                .map_err(|_| Error::InvalidInput("JPEG 2000 component width overflowed".into()))?,
            height: u32::try_from(component_height)
                .map_err(|_| Error::InvalidInput("JPEG 2000 component height overflowed".into()))?,
        });
    }
    Ok(Jpeg2000ImageHeader {
        width,
        height,
        components,
    })
}

fn read_be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_be_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_raw_codestream_size_header() {
        let codestream = include_bytes!("../tests/fixtures/sample_jpeg2000.j2k");
        let header = parse_raw_codestream(codestream).unwrap();
        assert_eq!(header.width, 4);
        assert_eq!(header.height, 4);
        assert_eq!(header.components.len(), 1);
        assert_eq!(header.components[0].precision, 8);
        assert_eq!(header.components[0].width, 4);
        assert_eq!(header.components[0].height, 4);
        assert!(parse_raw_codestream(&codestream[..20]).is_err());
    }

    #[test]
    fn parses_full_jp2_boxed_image_and_checks_ihdr_against_codestream() {
        let codestream = include_bytes!("../tests/fixtures/sample_jpeg2000.j2k");
        let mut jp2 = b"\x00\x00\x00\x0cjP  \r\n\x87\n".to_vec();
        jp2.extend_from_slice(&[0, 0, 0, 20, b'f', b't', b'y', b'p']);
        jp2.extend_from_slice(b"jp2 \x00\x00\x00\x00jp2 ");
        let ihdr = [
            0, 0, 0, 22, b'i', b'h', b'd', b'r', // box header
            0, 0, 0, 4, // height
            0, 0, 0, 4, // width
            0, 1, // components
            7, // bits per component minus one
            7, // compression type
            0, // unknown color space flag
            0, // intellectual property flag
        ];
        let mut jp2_header = ihdr.to_vec();
        // Required JP2 color specification box: enumerated grayscale.
        jp2_header.extend_from_slice(&[0, 0, 0, 15, b'c', b'o', b'l', b'r', 1, 0, 0, 0, 0, 0, 17]);
        let mut jp2h = Vec::new();
        jp2h.extend_from_slice(&u32::try_from(jp2_header.len() + 8).unwrap().to_be_bytes());
        jp2h.extend_from_slice(b"jp2h");
        jp2h.extend_from_slice(&jp2_header);
        jp2.extend_from_slice(&jp2h);
        let mut jp2c = Vec::new();
        jp2c.extend_from_slice(&u32::try_from(codestream.len() + 8).unwrap().to_be_bytes());
        jp2c.extend_from_slice(b"jp2c");
        jp2c.extend_from_slice(codestream);
        jp2.extend_from_slice(&jp2c);

        let header = parse_jpx_header(&jp2).unwrap();
        assert_eq!(header.width, 4);
        assert_eq!(header.height, 4);
        assert_eq!(header.components.len(), 1);
        assert!(parse_jpx_header(&jp2[..14]).is_err());

        let encoded_jp2 = include_bytes!("../tests/fixtures/sample_jpeg2000.jp2");
        let encoded_header = parse_jpx_header(encoded_jp2).unwrap();
        assert_eq!(encoded_header.width, 4);
        assert_eq!(encoded_header.height, 4);
        assert_eq!(encoded_header.components.len(), 1);
    }

    #[test]
    fn identifies_boxed_and_raw_jpeg2000_media_types() {
        assert_eq!(
            jpx_media_type(include_bytes!("../tests/fixtures/sample_jpeg2000.j2k")),
            "image/j2c"
        );
        assert_eq!(
            jpx_media_type(include_bytes!("../tests/fixtures/sample_jpeg2000.jp2")),
            "image/jp2"
        );
    }
}
