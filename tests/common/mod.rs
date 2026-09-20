use std::io::{Cursor, Write};
use std::path::Path;

#[allow(dead_code)]
pub fn write_e57_rgb_and_intensity_fixture(path: &Path) {
    use e57::{
        E57Writer, Quaternion, Record, RecordDataType, RecordName, RecordValue, Transform,
        Translation,
    };

    let mut writer = E57Writer::from_file(path, "00112233-4455-6677-8899-aabbccddeeff").unwrap();
    let coordinate_type = RecordDataType::Double {
        min: Some(-100.0),
        max: Some(100.0),
    };
    let unit_float = RecordDataType::Single {
        min: Some(0.0),
        max: Some(1.0),
    };
    {
        let prototype = vec![
            Record {
                name: RecordName::CartesianX,
                data_type: coordinate_type.clone(),
            },
            Record {
                name: RecordName::CartesianY,
                data_type: coordinate_type.clone(),
            },
            Record {
                name: RecordName::CartesianZ,
                data_type: coordinate_type.clone(),
            },
            Record {
                name: RecordName::CartesianInvalidState,
                data_type: RecordDataType::Integer { min: 0, max: 2 },
            },
            Record {
                name: RecordName::Intensity,
                data_type: unit_float.clone(),
            },
            Record {
                name: RecordName::ColorRed,
                data_type: unit_float.clone(),
            },
            Record {
                name: RecordName::ColorGreen,
                data_type: unit_float.clone(),
            },
            Record {
                name: RecordName::ColorBlue,
                data_type: unit_float.clone(),
            },
        ];
        let mut cloud = writer
            .add_pointcloud("11112233-4455-6677-8899-aabbccddeeff", prototype)
            .unwrap();
        cloud.set_name(Some("Colored scan".into()));
        cloud.set_transform(Some(Transform {
            rotation: Quaternion {
                w: std::f64::consts::FRAC_1_SQRT_2,
                x: 0.0,
                y: 0.0,
                z: std::f64::consts::FRAC_1_SQRT_2,
            },
            translation: Translation {
                x: 10.0,
                y: 20.0,
                z: 0.0,
            },
        }));
        for (xyz, invalid, intensity, rgb) in [
            ([1.0, 0.0, 0.0], 0, 0.5, [1.0, 0.0, 0.0]),
            ([0.0, 1.0, 0.0], 0, 0.5, [0.0, 1.0, 0.0]),
            ([0.0, 0.0, 0.0], 0, 0.5, [0.0, 0.0, 1.0]),
            ([9.0, 9.0, 9.0], 2, 0.5, [1.0, 1.0, 1.0]),
        ] {
            cloud
                .add_point(vec![
                    RecordValue::Double(xyz[0]),
                    RecordValue::Double(xyz[1]),
                    RecordValue::Double(xyz[2]),
                    RecordValue::Integer(invalid),
                    RecordValue::Single(intensity),
                    RecordValue::Single(rgb[0]),
                    RecordValue::Single(rgb[1]),
                    RecordValue::Single(rgb[2]),
                ])
                .unwrap();
        }
        cloud.finalize().unwrap();
    }
    {
        let prototype = vec![
            Record {
                name: RecordName::CartesianX,
                data_type: coordinate_type.clone(),
            },
            Record {
                name: RecordName::CartesianY,
                data_type: coordinate_type.clone(),
            },
            Record {
                name: RecordName::CartesianZ,
                data_type: coordinate_type,
            },
            Record {
                name: RecordName::Intensity,
                data_type: unit_float,
            },
        ];
        let mut cloud = writer
            .add_pointcloud("22223344-5566-7788-99aa-bbccddeeff00", prototype)
            .unwrap();
        cloud.set_name(Some("Intensity scan".into()));
        cloud.set_transform(Some(Transform {
            translation: Translation {
                x: 0.0,
                y: 0.0,
                z: 10.0,
            },
            ..Transform::default()
        }));
        for (xyz, intensity) in [
            ([-1.0, 0.0, 0.0], 0.0),
            ([1.0, 0.0, 0.0], 0.5),
            ([0.0, 1.0, 0.0], 1.0),
        ] {
            cloud
                .add_point(vec![
                    RecordValue::Double(xyz[0]),
                    RecordValue::Double(xyz[1]),
                    RecordValue::Double(xyz[2]),
                    RecordValue::Single(intensity),
                ])
                .unwrap();
        }
        cloud.finalize().unwrap();
    }
    writer.finalize().unwrap();
}

fn write_meshb_i32(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn begin_meshb_v2_keyword(bytes: &mut Vec<u8>, code: i32, count: Option<i32>) -> usize {
    write_meshb_i32(bytes, code);
    let offset_position = bytes.len();
    bytes.extend_from_slice(&[0; 4]);
    if let Some(count) = count {
        write_meshb_i32(bytes, count);
    }
    offset_position
}

fn finish_meshb_v2_keyword(bytes: &mut [u8], offset_position: usize) {
    let next_position = u32::try_from(bytes.len()).unwrap().to_le_bytes();
    bytes[offset_position..offset_position + 4].copy_from_slice(&next_position);
}

pub fn medit_binary_tetrahedron() -> Vec<u8> {
    let mut bytes = Vec::new();
    write_meshb_i32(&mut bytes, 1); // little-endian marker
    write_meshb_i32(&mut bytes, 2); // MEDIT binary version 2: float64 coordinates, 32-bit indices

    let dimension = begin_meshb_v2_keyword(&mut bytes, 3, None);
    write_meshb_i32(&mut bytes, 3);
    finish_meshb_v2_keyword(&mut bytes, dimension);

    let vertices = begin_meshb_v2_keyword(&mut bytes, 4, Some(4));
    for [x, y, z] in [
        [0.0f64, 0.0, 0.0],
        [100.0, 0.0, 0.0],
        [0.0, 100.0, 0.0],
        [0.0, 0.0, 100.0],
    ] {
        for coordinate in [x, y, z] {
            bytes.extend_from_slice(&coordinate.to_le_bytes());
        }
        write_meshb_i32(&mut bytes, 1);
    }
    finish_meshb_v2_keyword(&mut bytes, vertices);

    let tetrahedra = begin_meshb_v2_keyword(&mut bytes, 8, Some(1));
    for index in [1, 2, 3, 4, 7] {
        write_meshb_i32(&mut bytes, index);
    }
    finish_meshb_v2_keyword(&mut bytes, tetrahedra);

    write_meshb_i32(&mut bytes, 54); // End
    write_meshb_i32(&mut bytes, 0);
    bytes
}

pub fn tiff_palette_1bit() -> Vec<u8> {
    tiff_packed_fixture(1, true)
}

pub fn tiff_white_is_zero_gray(bits: u16) -> Vec<u8> {
    tiff_packed_fixture(bits, false)
}

fn tiff_packed_fixture(bits: u16, palette: bool) -> Vec<u8> {
    const WIDTH: u32 = 20;
    const HEIGHT: u32 = 10;
    assert!(matches!(bits, 1 | 2 | 4));
    let color_map_entries = if palette {
        3usize * (1usize << bits)
    } else {
        0
    };
    let entry_count = if palette { 10u16 } else { 9u16 };
    let ifd_offset = 8usize;
    let ifd_end = ifd_offset + 2 + usize::from(entry_count) * 12 + 4;
    let color_map_offset = u32::try_from(ifd_end).unwrap();
    let color_map_bytes = color_map_entries * 2;
    let strip_offset = u32::try_from(ifd_end + color_map_bytes).unwrap();
    let row_bytes = usize::try_from((WIDTH * u32::from(bits)).div_ceil(8)).unwrap();
    let strip_bytes = row_bytes * usize::try_from(HEIGHT).unwrap();
    let mut entries = vec![
        (256u16, 4u16, 1u32, WIDTH),
        (257, 4, 1, HEIGHT),
        (258, 3, 1, u32::from(bits)),
        (259, 3, 1, 1),
        (262, 3, 1, if palette { 3 } else { 0 }),
        (273, 4, 1, strip_offset),
        (277, 3, 1, 1),
        (278, 4, 1, HEIGHT),
        (279, 4, 1, u32::try_from(strip_bytes).unwrap()),
    ];
    if palette {
        entries.push((
            320,
            3,
            u32::try_from(color_map_entries).unwrap(),
            color_map_offset,
        ));
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"II");
    bytes.extend_from_slice(&42u16.to_le_bytes());
    bytes.extend_from_slice(&u32::try_from(ifd_offset).unwrap().to_le_bytes());
    bytes.extend_from_slice(&entry_count.to_le_bytes());
    for (tag, kind, count, value) in entries {
        bytes.extend_from_slice(&tag.to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&0u32.to_le_bytes());
    if palette {
        let count = 1usize << bits;
        for channel in 0..3 {
            for index in 0..count {
                let value = if channel == 0 && index == 1 {
                    u16::MAX
                } else {
                    0
                };
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    for _ in 0..HEIGHT {
        let mut row = vec![0u8; row_bytes];
        for x in (WIDTH / 2)..WIDTH {
            let bit_offset = usize::try_from(x * u32::from(bits)).unwrap();
            let byte_index = bit_offset / 8;
            let shift = 8 - bits - (bit_offset % 8) as u16;
            row[byte_index] |= (((1u16 << bits) - 1) as u8) << shift;
        }
        bytes.extend_from_slice(&row);
    }
    bytes
}

pub fn outlook_msg_bytes() -> Vec<u8> {
    let mut compound = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    let mut properties = vec![0; 48];
    properties[32..34].copy_from_slice(&0x0003u16.to_le_bytes());
    properties[34..36].copy_from_slice(&0x3FDEu16.to_le_bytes());
    properties[40..44].copy_from_slice(&932u32.to_le_bytes());
    write_stream(&mut compound, "/__properties_version1.0", &properties);
    write_unicode(&mut compound, "0037", "Outlook preview");
    write_unicode(&mut compound, "0C1A", "Rina Example");
    write_unicode(&mut compound, "0C1F", "rina@example.test");
    write_unicode(&mut compound, "0E04", "Kai Example <kai@example.test>");
    write_unicode(&mut compound, "0E03", "Mina Example <mina@example.test>");
    write_unicode(&mut compound, "1000", "Plain body fallback.");
    let (html, _, had_errors) = encoding_rs::SHIFT_JIS.encode(
        "<html><body><p>Rendered <strong>HTML</strong> body.</p><p>日本語表示</p><img src=\"https://example.invalid/logo.png\" alt=\"Company mark\"><script>window.alert('unsafe')</script></body></html>",
    );
    assert!(!had_errors);
    write_stream(&mut compound, "/__substg1.0_10130102", &html);
    let attachment = "/__attach_version1.0_#00000000";
    compound.create_storage(attachment).unwrap();
    write_stream(
        &mut compound,
        &format!("{attachment}/__properties_version1.0"),
        &[0; 8],
    );
    compound.flush().unwrap();
    compound.into_inner().into_inner()
}

pub fn webp_sample() -> Vec<u8> {
    let pixels = [0, 0, 0, 255, 255, 255, 255, 255];
    let mut bytes = Vec::new();
    image_webp::WebPEncoder::new(&mut bytes)
        .encode(&pixels, 2, 1, image_webp::ColorType::Rgba8)
        .unwrap();
    bytes
}

pub fn animated_gif_sample() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = gif::Encoder::new(&mut bytes, 32, 16, &[]).unwrap();
    encoder.set_repeat(gif::Repeat::Infinite).unwrap();

    let mut first_pixels = [0, 0, 0, 255].repeat(16 * 16);
    let mut first = gif::Frame::from_rgba_speed(16, 16, &mut first_pixels, 10);
    first.left = 0;
    first.delay = 5;
    encoder.write_frame(&first).unwrap();

    let mut second_pixels = [255, 255, 255, 255].repeat(16 * 16);
    let mut second = gif::Frame::from_rgba_speed(16, 16, &mut second_pixels, 10);
    second.left = 16;
    second.delay = 5;
    encoder.write_frame(&second).unwrap();
    drop(encoder);
    bytes
}

pub fn cmyk_jpeg_sample() -> Vec<u8> {
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 100)
        .encode(&[0, 255, 255, 0], 1, 1, jpeg_encoder::ColorType::Cmyk)
        .unwrap();
    bytes
}

pub fn odp_presentation_with_embedded_image() -> Vec<u8> {
    let source = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink">
 <office:body><office:presentation><draw:page draw:name="Image slide">
  <draw:frame draw:name="embedded-red-image" svg:x="1cm" svg:y="1cm" svg:width="4cm" svg:height="2cm"><draw:image xlink:href="Pictures/red.png"/></draw:frame>
 </draw:page></office:presentation></office:body>
</office:document-content>"#;
    let png = red_blue_png();
    let mut package = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    package.start_file("mimetype", stored).unwrap();
    package
        .write_all(b"application/vnd.oasis.opendocument.presentation")
        .unwrap();
    package
        .start_file("content.xml", zip::write::SimpleFileOptions::default())
        .unwrap();
    package.write_all(source).unwrap();
    package
        .start_file("Pictures/red.png", zip::write::SimpleFileOptions::default())
        .unwrap();
    package.write_all(&png).unwrap();
    package.finish().unwrap().into_inner()
}

fn red_blue_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0, 0, 0, 255]).unwrap();
    }
    bytes
}

fn write_unicode(
    compound: &mut cfb::CompoundFile<Cursor<Vec<u8>>>,
    property_id: &str,
    value: &str,
) {
    let mut bytes = value
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    bytes.extend_from_slice(&[0, 0]);
    write_stream(compound, &format!("/__substg1.0_{property_id}001F"), &bytes);
}

fn write_stream(compound: &mut cfb::CompoundFile<Cursor<Vec<u8>>>, path: &str, bytes: &[u8]) {
    compound
        .create_stream(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}
