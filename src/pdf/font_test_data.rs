//! Synthetic outlines authored for regression tests; no external font assets.

fn set_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn set_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

pub(super) fn font(cff: Option<Vec<u8>>, broken: bool, quadratic: bool) -> Vec<u8> {
    let mut head = vec![0; 54];
    set_u32(&mut head, 0, 0x0001_0000);
    set_u32(&mut head, 12, 0x5f0f_3cf5);
    set_u16(&mut head, 18, 1000);
    let mut hhea = vec![0; 36];
    set_u32(&mut hhea, 0, 0x0001_0000);
    set_u16(&mut hhea, 4, 800);
    set_u16(&mut hhea, 6, (-200i16) as u16);
    set_u16(&mut hhea, 10, 600);
    set_u16(&mut hhea, 34, 4);
    let mut maxp = vec![0; 32];
    set_u32(&mut maxp, 0, 0x0001_0000);
    for (offset, value) in [
        (4, 4),
        (6, 3),
        (8, 1),
        (10, 3),
        (12, 1),
        (14, 2),
        (28, 1),
        (30, 1),
    ] {
        set_u16(&mut maxp, offset, value);
    }
    let hmtx = [(500u16, 0u16), (600, 0), (250, 0), (600, 100)]
        .into_iter()
        .flat_map(|(advance, bearing)| [advance.to_be_bytes(), bearing.to_be_bytes()].concat())
        .collect();

    // Format 4: space -> empty glyph 2; A -> glyph 1; B -> composite glyph 3.
    let codes = [32u16, 65, 66, 65535];
    let glyphs = [2u16, 1, 3, 0];
    let mut cmap = vec![0; 12 + 48];
    set_u16(&mut cmap, 2, 1);
    set_u16(&mut cmap, 4, 3);
    set_u16(&mut cmap, 6, 1);
    set_u32(&mut cmap, 8, 12);
    set_u16(&mut cmap, 12, 4);
    set_u16(&mut cmap, 14, 48);
    set_u16(&mut cmap, 18, 8);
    set_u16(&mut cmap, 20, 8);
    set_u16(&mut cmap, 22, 2);
    for (index, (code, glyph)) in codes.into_iter().zip(glyphs).enumerate() {
        set_u16(&mut cmap, 26 + index * 2, code);
        set_u16(&mut cmap, 36 + index * 2, code);
        set_u16(&mut cmap, 44 + index * 2, glyph.wrapping_sub(code));
    }
    let is_cff = cff.is_some();
    let mut tables = vec![
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"maxp", maxp),
        (*b"hmtx", hmtx),
        (*b"cmap", cmap),
    ];
    if let Some(cff) = cff {
        tables.push((*b"CFF ", cff));
    } else {
        let mut glyf = Vec::new();
        // A triangular contour, optionally using an off-curve control point.
        for value in [1i16, 0, 0, 500, 700, 2, 0] {
            glyf.extend(value.to_be_bytes());
        }
        glyf.extend([1, u8::from(!quadratic), 1]);
        for value in [0i16, 500, -500, 0, 0, 700] {
            glyf.extend(value.to_be_bytes());
        }
        glyf.push(0);
        if broken {
            // First contour refers to far more points than are present.
            set_u16(&mut glyf, 10, 60000);
        }
        // Composite B: A translated by (100, 200).
        for value in [-1i16, 100, 200, 600, 900, 3, 1, 100, 200] {
            glyf.extend(value.to_be_bytes());
        }
        let loca = [0u16, 0, 15, 15, 24]
            .into_iter()
            .flat_map(u16::to_be_bytes)
            .collect();
        tables.extend([(*b"glyf", glyf), (*b"loca", loca)]);
    }
    tables.sort_by_key(|(tag, _)| *tag);
    let mut result = vec![0; 12 + 16 * tables.len()];
    result[..4].copy_from_slice(if is_cff { b"OTTO" } else { &[0, 1, 0, 0] });
    set_u16(&mut result, 4, tables.len() as u16);
    let power = 1u16 << (tables.len() as u16).ilog2();
    set_u16(&mut result, 6, power * 16);
    set_u16(&mut result, 8, power.ilog2() as u16);
    set_u16(&mut result, 10, tables.len() as u16 * 16 - power * 16);
    let mut head_offset = 0;
    for (index, (tag, bytes)) in tables.into_iter().enumerate() {
        let offset = result.len();
        let record = 12 + index * 16;
        result[record..record + 4].copy_from_slice(&tag);
        set_u32(&mut result, record + 4, checksum(&bytes));
        set_u32(&mut result, record + 8, offset as u32);
        set_u32(&mut result, record + 12, bytes.len() as u32);
        if tag == *b"head" {
            head_offset = offset;
        }
        result.extend(bytes);
        while !result.len().is_multiple_of(4) {
            result.push(0);
        }
    }
    let adjustment = 0xb1b0_afbau32.wrapping_sub(checksum(&result));
    set_u32(&mut result, head_offset + 8, adjustment);
    result
}
