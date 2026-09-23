use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use base64::Engine;
use document_svg::{ConvertOptions, ReverseOptions, SourceFormat, convert_path};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

#[path = "common/mod.rs"]
mod common;

#[path = "../src/pdf/font_test_data.rs"]
mod font_test_data;

#[test]
fn outlines_an_embedded_symbol_font_instead_of_trusting_a_system_symbol_font() {
    // A word processor's own subsetted font named "Symbol" is common for
    // dingbat-style bullets, and its ToUnicode map commonly points at
    // Private Use Area code points that mean nothing outside that specific
    // embedded font. Treating the family name as a sign that any system's
    // real Symbol font would render it left such text as a missing-glyph
    // box wherever the literal PUA text was not backed by that exact font.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("embedded-symbol-font.pdf");
    let mut document = Document::with_version("1.7");
    let bytes = font_test_data::font(None, false, false);
    let font_file = document.add_object(Stream::new(
        dictionary! { "Length1" => bytes.len() as i64 },
        bytes,
    ));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "Symbol", "Flags" => 4,
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 900.into()],
        "Ascent" => 800, "Descent" => -200, "CapHeight" => 700, "StemV" => 80,
        "FontFile2" => font_file,
    });
    let to_unicode = document.add_object(Stream::new(
        dictionary! {},
        b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n1 beginbfchar\n<41> <F0B7>\nendbfchar\nendcmap\nend\nend".to_vec(),
    ));
    let font = document.add_object(dictionary! {
        // No /Encoding: this is a symbolic simple font per the PDF spec's
        // own default, matching a real embedded "Symbol" subset.
        "Type" => "Font", "Subtype" => "TrueType", "BaseFont" => "Symbol",
        "FirstChar" => 32, "LastChar" => 66,
        "Widths" => (32..=66).map(|code| Object::Integer(if code == 32 { 250 } else { 500 })).collect::<Vec<_>>(),
        "FontDescriptor" => descriptor, "ToUnicode" => to_unicode,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 20 Tf 20 80 Td (A) Tj ET".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"text-outline\""), "{svg}");
    assert!(svg.contains("L 0.5 0"), "{svg}");
}

#[test]
fn resolves_a_cid_keyed_opentype_cff_glyph_through_its_own_cff_charset() {
    // Real-world regression (pdf.js's own Embedded_font.pdf): a CIDFontType0
    // (CFF-keyed CID font) can legally be embedded as a full OpenType
    // wrapper (`/FontFile3 /Subtype /OpenType`) rather than a bare
    // CIDFontType0C table. Since that sfnt container parses successfully as
    // a font, glyph lookup took the ordinary Identity-H path that treats the
    // raw CID as a glyph index directly -- correct for CIDFontType2
    // (TrueType-based) CID fonts, but never for a CFF-keyed one, whose own
    // `charset` table maps GID -> CID through a font-specific, non-identity
    // table. This fixture's single glyph is CID 0xA911 (43281) but lives at
    // GID 1 -- looking it up as GID 43281 in a 2-glyph font failed outright,
    // falling back to incorrect editable placeholder text instead of the
    // real "除" glyph outline.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("cid-cff-opentype.pdf");
    let mut document = Document::with_version("1.7");
    let bytes = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/notosans_tc_cid_cff_opentype.otf"),
    )
    .unwrap();
    let font_file =
        document.add_object(Stream::new(dictionary! { "Subtype" => "OpenType" }, bytes));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "AAAAAA+NotoSansTC-DemiLight", "Flags" => 32,
        "FontBBox" => vec![(-1000).into(), (-1048).into(), 2928.into(), 1808.into()],
        "Ascent" => 1808, "Descent" => (-1048), "StemV" => 0,
        "FontFile3" => font_file,
    });
    let cid_system_info = document.add_object(dictionary! {
        "Registry" => Object::string_literal("Adobe"),
        "Ordering" => Object::string_literal("Identity"),
        "Supplement" => 0,
    });
    let descendant = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType0", "BaseFont" => "AAAAAA+NotoSansTC-DemiLight",
        "CIDSystemInfo" => cid_system_info, "FontDescriptor" => descriptor,
        "W" => vec![43281.into(), Object::Array(vec![1000.into()])],
    });
    let to_unicode = document.add_object(Stream::new(
        dictionary! {},
        b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n1 beginbfchar\n<A911> <9664>\nendbfchar\nendcmap\nend\nend".to_vec(),
    ));
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "AAAAAA+NotoSansTC-DemiLight",
        "Encoding" => "Identity-H", "DescendantFonts" => vec![Object::Reference(descendant)],
        "ToUnicode" => to_unicode,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 100 Tf 20 80 Td <A911> Tj ET".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"text-outline\""), "{svg}");
    assert!(svg.contains("aria-label=\"除\""), "{svg}");
}

#[test]
fn strips_pfb_segment_headers_from_an_embedded_type1_font() {
    // Real-world regression (pdf.js's own issue14462_reduced.pdf): PDF32000
    // 9.9's own /FontFile format is a plain concatenation of a Type1 font's
    // cleartext header, its eexec-encrypted binary portion, and a
    // zero-padding trailer -- no segment framing at all -- but this font is
    // instead embedded as a raw PFB (Printer Font Binary) container
    // verbatim, complete with its 6-byte `0x80 <type> <length>` segment
    // headers. One such header lands squarely inside the encrypted portion,
    // right after the `eexec` keyword, corrupting eexec decryption's
    // running cipher state (which carries across the whole ciphertext) from
    // that point on -- every glyph's CharStrings entry decrypts to garbage,
    // so parsing found none of them at all, falling back to incorrect
    // editable placeholder text instead of the real glyph outlines.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("pfb-wrapped-type1.pdf");
    let mut document = Document::with_version("1.7");
    let bytes = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pfb_wrapped_type1_dejavusansmono.pfb"),
    )
    .unwrap();
    let font_file = document.add_object(Stream::new(
        dictionary! { "Length1" => 2608, "Length2" => 10684, "Length3" => 0 },
        bytes,
    ));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "MPDFAA+DejaVuSansMono", "Flags" => 33,
        "FontBBox" => vec![(-558).into(), (-375).into(), 718.into(), 1042.into()],
        "Ascent" => 928, "Descent" => (-236), "CapHeight" => 928, "StemV" => 70,
        "FontFile" => font_file,
    });
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "MPDFAA+DejaVuSansMono",
        "FirstChar" => 32, "LastChar" => 117,
        "Widths" => (32..=117).map(|_| Object::Integer(500)).collect::<Vec<_>>(),
        "FontDescriptor" => descriptor, "Encoding" => "WinAnsiEncoding",
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT 10 20 TD /F1 20 Tf (Issue) Tj ET".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 50.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"text-outline\""), "{svg}");
    assert!(svg.contains("aria-label=\"Issue\""), "{svg}");
}

#[test]
fn chunks_identity_h_codes_by_two_bytes_for_a_type1_program_on_a_cid_font() {
    // Real-world regression (pdf.js's own issue11740_reduced.pdf): a
    // CIDFontType0 descendant is only ever supposed to carry a bare CFF
    // (CIDFontType0C) or OpenType program (PDF32000 Table 126), but this
    // producer instead embeds a plain Type1 program through the
    // simple-font-only `/FontFile` key. Identity-H still means each
    // character code is two bytes regardless of what program type backs
    // it (PDF32000 9.7.5.2), but outline_type1 iterated `bytes` one byte
    // at a time unconditionally -- the same class of bug already fixed
    // for the bare-CFF case in outline_cff (see
    // outlines_a_cid_keyed_cff_glyph_under_identity_h), just never
    // applied to this sibling code path. Every high zero byte of a CID
    // under 256 showed up as its own spurious one-byte lookup, and the
    // real low byte could never resolve either once its position shifted
    // by one. This font's own built-in Encoding array names CID 1 as
    // `afii10032`, CID 2 as `afii10068`, CID 3 as `afii10077` -- ordinary
    // Cyrillic letters -- directly by that same numeric code, which is
    // what the fix now looks up for a Type1 program on an Identity-H CID
    // font.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("cid-keyed-type1.pdf");
    let mut document = Document::with_version("1.7");
    let bytes = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cid_keyed_type1_program.pfa"),
    )
    .unwrap();
    let font_file = document.add_object(Stream::new(dictionary! {}, bytes));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "acAura-Bold+FPEF", "Flags" => 32,
        "FontBBox" => vec![(-487).into(), (-229).into(), 1098.into(), 920.into()],
        "Ascent" => 770, "Descent" => (-229), "StemV" => 140,
        "FontFile" => font_file,
    });
    let cid_system_info = document.add_object(dictionary! {
        "Registry" => Object::string_literal("Adobe"),
        "Ordering" => Object::string_literal("Identity"),
        "Supplement" => 0,
    });
    let descendant = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType0", "BaseFont" => "acAura-Bold+FPEF",
        "CIDSystemInfo" => cid_system_info, "FontDescriptor" => descriptor,
        "CIDToGIDMap" => "Identity",
        "W" => vec![0.into(), Object::Array(vec![500.into(), 500.into(), 500.into()])],
    });
    let to_unicode = document.add_object(Stream::new(
        dictionary! {},
        b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n3 beginbfchar\n<0001> <041E>\n<0002> <0433>\n<0003> <043B>\nendbfchar\nendcmap\nend\nend".to_vec(),
    ));
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "acAura-Bold+FPEF",
        "Encoding" => "Identity-H", "DescendantFonts" => vec![Object::Reference(descendant)],
        "ToUnicode" => to_unicode,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 20 Tf 10 20 TD <000100020003> Tj ET".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 50.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"text-outline\""), "{svg}");
}

#[test]
fn resolves_a_cid_keyed_cff_glyph_whose_reverse_map_lands_on_gid_zero() {
    // Real-world regression (pdf.js's own issue11718_reduced.pdf): a
    // CID-keyed CFF's charset conventionally names GID 0 ".notdef", and
    // outline_cff treated any CID that resolved to GID 0 as "not found" on
    // that assumption -- but nothing in the CFF format actually requires
    // GID 0's charstring to be empty, and this real-world subsetted TeX
    // Computer Modern font (CMR9) stores a genuine, fully-drawn "ff"
    // ligature there while its charset still labels it ".notdef" as a
    // formality. Treating a resolved GID of 0 as "not found" discarded
    // that glyph outright, even though its own charstring draws real
    // content -- confirmed directly by decompiling this exact font's CFF
    // charstring for GID 0 with fontTools: real hstem/vstem/curve
    // operators, not an empty program. The font parser's own sentinel for
    // "this CID has no reverse-map entry at all" is 0xFFFF, not 0, so this
    // is what outline_cff must check against instead.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("cid-cff-gid-zero.pdf");
    let mut document = Document::with_version("1.7");
    let bytes = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cid_cff_with_glyph_at_gid_zero.cff"),
    )
    .unwrap();
    let font_file = document.add_object(Stream::new(
        dictionary! { "Subtype" => "CIDFontType0C" },
        bytes,
    ));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "PTKAHE+CMR9", "Flags" => 4,
        "FontBBox" => vec![(-39).into(), (-250).into(), 1036.into(), 750.into()],
        "Ascent" => 750, "Descent" => (-250), "StemV" => 74,
        "FontFile3" => font_file,
    });
    let cid_system_info = document.add_object(dictionary! {
        "Registry" => Object::string_literal("Adobe"),
        "Ordering" => Object::string_literal("Identity"),
        "Supplement" => 0,
    });
    let descendant = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType0", "BaseFont" => "PTKAHE+CMR9",
        "CIDSystemInfo" => cid_system_info, "FontDescriptor" => descriptor,
        "W" => vec![0.into(), Object::Array(vec![600.into()])],
    });
    let to_unicode = document.add_object(Stream::new(
        dictionary! {},
        b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n1 beginbfchar\n<0000> <FB00>\nendbfchar\nendcmap\nend\nend".to_vec(),
    ));
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "PTKAHE+CMR9",
        "Encoding" => "Identity-H", "DescendantFonts" => vec![Object::Reference(descendant)],
        "ToUnicode" => to_unicode,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 20 Tf 10 20 TD <0000> Tj ET".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 50.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"text-outline\""), "{svg}");
}

fn build_type0_pdf(input: &Path, encoding: &str) {
    let mut document = Document::with_version("1.7");
    let bytes = font_test_data::font(None, false, false);
    let font_file = document.add_object(Stream::new(
        dictionary! { "Length1" => bytes.len() as i64 },
        bytes,
    ));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "TestCID", "Flags" => 4,
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 900.into()],
        "Ascent" => 800, "Descent" => -200, "CapHeight" => 700, "StemV" => 80,
        "FontFile2" => font_file,
    });
    let descendant = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "TestCID",
        "CIDSystemInfo" => dictionary! { "Registry" => Object::string_literal("Adobe"), "Ordering" => Object::string_literal("Identity"), "Supplement" => 0 },
        "FontDescriptor" => descriptor, "DW" => 1000,
    });
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "TestCID",
        "Encoding" => Object::Name(encoding.as_bytes().to_vec()),
        "DescendantFonts" => vec![descendant.into()],
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 20 Tf 20 80 Td <0041> Tj ET".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(input).unwrap();
}

#[test]
fn warns_about_a_type0_font_that_uses_a_non_identity_encoding() {
    // Character code only equals CID under Identity-H/V. A predefined
    // CMap such as UniJIS-UCS2-H (common for non-embedded standard Asian
    // fonts) maps code to CID through a table this converter does not
    // resolve, silently giving every /W width lookup and glyph selection
    // the wrong key -- which showed up as visibly wrong letter spacing in
    // Latin runs mixed into CJK text. Say so in a warning instead of
    // shipping mispositioned text with no diagnostic.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("non-identity.pdf");
    build_type0_pdf(&input, "UniJIS-UCS2-H");
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let warnings = &report.pages[0].warnings;
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("non-Identity encoding UniJIS-UCS2-H")),
        "{warnings:?}"
    );
}

#[test]
fn does_not_warn_about_a_type0_font_that_uses_identity_h_encoding() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("identity.pdf");
    build_type0_pdf(&input, "Identity-H");
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let warnings = &report.pages[0].warnings;
    assert!(
        !warnings
            .iter()
            .any(|warning| warning.contains("non-Identity")),
        "{warnings:?}"
    );
}

#[test]
fn outlines_an_identity_h_cid_glyph_the_embedded_cmap_cannot_resolve() {
    // Real-world regression (pdf.js's own basicapi.pdf, an ordinary
    // DejaVuSans-subsetted document): under Identity-H the raw character
    // code already *is* the CID (PDF32000 9.7.5.2), and with no
    // /CIDToGIDMap the CID *is* the GID directly -- no cmap lookup
    // involved at all. A subsetted CID font's embedded cmap commonly
    // lacks full Unicode coverage (or any useful coverage), and trying to
    // resolve the glyph through it anyway, as if this were a simple font,
    // failed for perfectly ordinary letters ('C', 'P') and fell back to
    // incorrect placeholder text.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("identity-cid-no-cmap-entry.pdf");
    let mut document = Document::with_version("1.7");
    let bytes = font_test_data::font(None, false, false);
    let font_file = document.add_object(Stream::new(
        dictionary! { "Length1" => bytes.len() as i64 },
        bytes,
    ));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "TestCID", "Flags" => 4,
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 900.into()],
        "Ascent" => 800, "Descent" => -200, "CapHeight" => 700, "StemV" => 80,
        "FontFile2" => font_file,
    });
    let descendant = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "TestCID",
        "CIDSystemInfo" => dictionary! { "Registry" => Object::string_literal("Adobe"), "Ordering" => Object::string_literal("Identity"), "Supplement" => 0 },
        "FontDescriptor" => descriptor, "DW" => 1000,
    });
    // Maps CID 1 to 'X' (U+0058) -- a character the test font's own cmap
    // does not carry (it covers only space, 'A', 'B' and 0xFFFF) -- so any
    // lookup through the font's cmap, by code or by this Unicode value,
    // is guaranteed to fail. CID 1 is real: it is GID 1, the same
    // triangular outline used elsewhere in this font's own tests.
    let to_unicode = document.add_object(Stream::new(
        dictionary! {},
        b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n1 beginbfchar\n<0001> <0058>\nendbfchar\nendcmap\nend\nend"
            .to_vec(),
    ));
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "TestCID",
        "Encoding" => "Identity-H",
        "ToUnicode" => to_unicode,
        "DescendantFonts" => vec![descendant.into()],
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 20 Tf 20 80 Td <0001> Tj ET".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(
        report.pages[0].warnings.is_empty(),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("text-outline"), "{svg}");
}

fn add_appearance_annotation(
    document: &mut Document,
    subtype: &str,
    rect: [i64; 4],
    fill_rgb: [f64; 3],
    flags: Option<i64>,
) -> Object {
    let appearance_content = format!(
        "{} {} {} rg 0 0 50 20 re f",
        fill_rgb[0], fill_rgb[1], fill_rgb[2]
    );
    let appearance = document.add_object(Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 50.into(), 20.into()] },
        appearance_content.into_bytes(),
    ));
    let mut annotation = dictionary! {
        "Type" => "Annot", "Subtype" => subtype,
        "Rect" => rect.into_iter().map(Object::Integer).collect::<Vec<_>>(),
        "AP" => dictionary! { "N" => appearance },
    };
    if let Some(flags) = flags {
        annotation.set("F", flags);
    }
    document.add_object(annotation).into()
}

#[test]
fn renders_annotation_appearance_streams_but_skips_link_and_hidden() {
    // A filled-in form field's value, and a sticky note's icon, live only
    // in an annotation's /AP appearance stream -- not in the page content
    // stream -- so a converter that reads only page content silently drops
    // them. Link annotations are conventionally just an invisible active
    // area (never rendered even when they happen to carry an /AP), and
    // anything flagged Hidden must stay invisible either way.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("annotations.pdf");
    let mut document = Document::with_version("1.7");
    let visible_widget = add_appearance_annotation(
        &mut document,
        "Widget",
        [100, 100, 150, 120],
        [0.0, 0.0, 1.0],
        None,
    );
    let visible_text = add_appearance_annotation(
        &mut document,
        "Text",
        [100, 200, 150, 220],
        [0.0, 1.0, 0.0],
        None,
    );
    let hidden_widget = add_appearance_annotation(
        &mut document,
        "Widget",
        [100, 300, 150, 320],
        [1.0, 0.0, 0.0],
        Some(2),
    );
    let link = add_appearance_annotation(
        &mut document,
        "Link",
        [100, 400, 150, 420],
        [1.0, 1.0, 0.0],
        None,
    );
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 400.into(), 500.into()],
        "Resources" => dictionary! {},
        "Contents" => content,
        "Annots" => vec![visible_widget, visible_text, hidden_widget, link],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#0000FF"), "{svg}");
    assert!(svg.contains("#00FF00"), "{svg}");
    assert!(!svg.contains("#FF0000"), "{svg}");
    assert!(!svg.contains("#FFFF00"), "{svg}");
}

#[test]
fn renders_freetext_annotation_from_contents_when_no_appearance_exists() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("free-text-no-appearance.pdf");
    let mut document = Document::with_version("1.7");
    let annotation = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "FreeText",
        "Rect" => vec![10.into(), 30.into(), 190.into(), 90.into()],
        "Contents" => Object::string_literal("Fallback free-text comment"),
        "DA" => Object::string_literal("/Helv 12 Tf 1 0 0 rg"),
        "Q" => 1,
    });
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 120.into()],
        "Resources" => dictionary! {}, "Contents" => content,
        "Annots" => vec![Object::Reference(annotation)],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();

    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("FreeText annotation without a normal appearance"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Fallback free-text comment"), "{svg}");
    assert!(svg.contains("#FF0000"), "{svg}");
}

#[test]
fn renders_acroform_text_widget_without_appearance_and_masks_passwords() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("widgets-no-appearance.pdf");
    let mut document = Document::with_version("1.7");
    let text_field = document.add_object(dictionary! {
        "FT" => "Tx", "T" => Object::string_literal("review"),
        "V" => Object::string_literal("Review approved"),
        "DA" => Object::string_literal("/Helv 12 Tf 0 0 1 rg"),
    });
    let password_field = document.add_object(dictionary! {
        "FT" => "Tx", "T" => Object::string_literal("password"),
        "V" => Object::string_literal("top-secret"), "Ff" => 1 << 13,
        "DA" => Object::string_literal("/Helv 12 Tf 0 g"),
    });
    let stream_value = document.add_object(Stream::new(
        dictionary! {},
        b"Stream-backed field value".to_vec(),
    ));
    let stream_field = document.add_object(dictionary! {
        "FT" => "Tx", "T" => Object::string_literal("stream-value"),
        "V" => Object::Reference(stream_value),
    });
    let text_widget = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![20.into(), 20.into(), 180.into(), 50.into()],
        "Parent" => Object::Reference(text_field),
    });
    let password_widget = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![20.into(), 60.into(), 180.into(), 90.into()],
        "Parent" => Object::Reference(password_field),
    });
    let stream_widget = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![20.into(), 95.into(), 180.into(), 115.into()],
        "Parent" => Object::Reference(stream_field),
    });
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 120.into()],
        "Resources" => dictionary! {}, "Contents" => content,
        "Annots" => vec![
            Object::Reference(text_widget),
            Object::Reference(password_widget),
            Object::Reference(stream_widget),
        ],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages,
        "AcroForm" => dictionary! {
            "Fields" => vec![
                Object::Reference(text_field),
                Object::Reference(password_field),
                Object::Reference(stream_field),
            ],
            "DA" => Object::string_literal("/Helv 10 Tf 0 g"),
        },
    });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();

    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Review approved"), "{svg}");
    assert!(svg.contains("Stream-backed field value"), "{svg}");
    assert!(svg.contains("annotation-widget-text"), "{svg}");
    assert!(
        !svg.contains("top-secret"),
        "password value leaked into SVG"
    );
    assert!(svg.contains("••••••••••"), "{svg}");
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("password Widget value was masked"))
    );
}

#[test]
fn renders_checkbox_and_radio_widgets_without_appearance_streams() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("button-widgets-no-appearance.pdf");
    let mut document = Document::with_version("1.7");
    let checkbox_field = document.add_object(dictionary! {
        "FT" => "Btn", "T" => Object::string_literal("accepted"),
        "V" => Object::Name(b"Yes".to_vec()),
    });
    let radio_field = document.add_object(dictionary! {
        "FT" => "Btn", "T" => Object::string_literal("choice"),
        "Ff" => 1 << 15, "V" => Object::Name(b"OptionA".to_vec()),
    });
    let checkbox = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![20.into(), 20.into(), 40.into(), 40.into()],
        "Parent" => Object::Reference(checkbox_field),
    });
    let radio = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![60.into(), 20.into(), 80.into(), 40.into()],
        "Parent" => Object::Reference(radio_field), "AS" => Object::Name(b"OptionA".to_vec()),
    });
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 60.into()],
        "Resources" => dictionary! {}, "Contents" => content,
        "Annots" => vec![Object::Reference(checkbox), Object::Reference(radio)],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();

    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(
        svg.matches("data-content-kind=\"annotation-widget-button\"")
            .count(),
        2
    );
    assert_eq!(
        svg.matches("data-content-kind=\"annotation-widget-button-mark\"")
            .count(),
        2
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("simple checkbox/radio marker"))
    );
}

#[test]
fn renders_choice_widget_export_values_as_display_text_without_appearance() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("choice-widget-no-appearance.pdf");
    let mut document = Document::with_version("1.7");
    let choice_field = document.add_object(dictionary! {
        "FT" => "Ch", "T" => Object::string_literal("category"),
        "Ff" => 1 << 21,
        "V" => Object::Array(vec![
            Object::string_literal("42"),
            Object::string_literal("7"),
        ]),
        "Opt" => Object::Array(vec![
            Object::Array(vec![Object::string_literal("42"), Object::string_literal("Forty-two")]),
            Object::Array(vec![Object::string_literal("7"), Object::string_literal("Seven")]),
        ]),
    });
    let choice = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![20.into(), 20.into(), 180.into(), 80.into()],
        "Parent" => Object::Reference(choice_field),
    });
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! {}, "Contents" => content,
        "Annots" => vec![Object::Reference(choice)],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages,
        "AcroForm" => dictionary! {
            "Fields" => vec![Object::Reference(choice_field)],
            "DA" => Object::string_literal("/Helv 12 Tf 0 g"),
        },
    });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();

    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Forty-two"), "{svg}");
    assert!(svg.contains("Seven"), "{svg}");
    assert!(
        !svg.contains(">42</tspan>"),
        "export value should map to display text"
    );
    assert!(
        !svg.contains(">7</tspan>"),
        "export value should map to display text"
    );
    assert!(svg.contains("annotation-widget-choice"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("choice Widget without a normal appearance"))
    );
}

#[test]
fn bounds_pdf_form_value_and_default_appearance_fallback_text() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("oversized-form-value.pdf");
    let mut document = Document::with_version("1.7");
    let oversized_value = "x".repeat(2 * 1024 * 1024 + 1);
    let oversized_field = document.add_object(dictionary! {
        "FT" => "Tx", "T" => Object::string_literal("oversized-value"),
        "V" => Object::string_literal(oversized_value.as_str()),
    });
    let appearance_field = document.add_object(dictionary! {
        "FT" => "Tx", "T" => Object::string_literal("oversized-da"),
        "V" => Object::string_literal("Short value"),
        "DA" => Object::string_literal(format!("/Helv 10 Tf {}", "0 ".repeat(40 * 1024))),
    });
    let oversized_widget = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![10.into(), 20.into(), 100.into(), 40.into()],
        "Parent" => Object::Reference(oversized_field),
    });
    let appearance_widget = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![20.into(), 20.into(), 195.into(), 40.into()],
        "Parent" => Object::Reference(appearance_field),
    });
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 80.into()],
        "Resources" => dictionary! {}, "Contents" => content,
        "Annots" => vec![Object::Reference(oversized_widget), Object::Reference(appearance_widget)],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();

    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let warnings = &report.pages[0].warnings;
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("form field value or selection list exceeds"))
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("DA exceeds 65536 bytes"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(!svg.contains(&oversized_value));
    assert!(svg.contains("Short value"), "{svg}");
}

#[test]
fn caps_pdf_annotations_per_page() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("many-annotations.pdf");
    let mut document = Document::with_version("1.7");
    let link = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Link",
        "Rect" => vec![0.into(), 0.into(), 0.into(), 0.into()],
    });
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! {}, "Contents" => content,
        "Annots" => vec![Object::Reference(link); 10_001],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();

    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("only the first 10000 were considered"))
    );
}

#[test]
fn draws_a_link_annotations_fallback_border_when_it_has_no_appearance() {
    // Real-world regression (pdf.js's own file_url_link.pdf): PDF32000
    // 12.5.4 leaves how to render a Link annotation with no appearance
    // stream of its own entirely up to the reader; poppler draws a border
    // rectangle from the annotation's own /Border (or /BS) width and its
    // /C colour whenever both are given, which this now mirrors. A Link
    // with neither -- the overwhelmingly common case -- still draws
    // nothing, matching the established, deliberate choice (see
    // `renders_annotation_appearance_streams_but_skips_link_and_hidden`)
    // that a Link is conventionally just an invisible active area; this
    // narrow fallback only ever applies when a document goes out of its
    // way to declare a real border and colour with no appearance to match.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("link-border.pdf");
    let mut document = Document::with_version("1.7");
    let link = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Link",
        "Rect" => vec![10.into(), 20.into(), 190.into(), 60.into()],
        "Border" => vec![0.into(), 0.into(), 2.into()],
        "C" => vec![0.into(), 1.into(), 0.into()],
        "A" => dictionary! { "Type" => "Action", "S" => "URI", "URI" => Object::string_literal("https://example.com") },
    });
    let plain_link = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Link",
        "Rect" => vec![10.into(), 80.into(), 190.into(), 120.into()],
        "A" => dictionary! { "Type" => "Action", "S" => "URI", "URI" => Object::string_literal("https://example.com") },
    });
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 130.into()],
        "Resources" => dictionary! {},
        "Contents" => content,
        "Annots" => vec![Object::Reference(link), Object::Reference(plain_link)],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("stroke=\"#00FF00\""), "{svg}");
    assert!(svg.contains("fill=\"none\""), "{svg}");
    assert_eq!(svg.matches("data-role=\"page-background\"").count() + 1, {
        let paths = svg.matches("<path").count();
        paths + 1
    });
}

#[test]
fn decodes_a_page_content_stream_filtered_with_asciihexdecode() {
    // Real-world regression (pdf.js's own asciihexdecode.pdf): lopdf's own
    // stream decoder only implements FlateDecode/LZWDecode/ASCII85Decode for
    // page content, and on any other filter it silently substitutes the RAW,
    // still-encoded stream bytes rather than reporting an error. Fed straight
    // into the content-operator tokenizer, this file's literal hex-digit text
    // produced a spurious bare "A" operator and lost the entire page instead
    // of drawing its actual "Hello world" text.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("asciihexdecode.pdf");
    let mut document = Document::with_version("1.7");
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let hex_content =
        b"42540A2F46312033302054660A32302038302054640A2848656C6C6F20776F726C642920546A0A4554>"
            .to_vec();
    let content = document.add_object(Stream::new(
        dictionary! { "Filter" => "ASCIIHexDecode" },
        hex_content,
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hello world"), "{svg}");
}

#[test]
fn maps_an_annotation_appearance_through_its_own_matrix_before_fitting_to_rect() {
    // Real-world regression (pdf.js's own file_pdfjs_test.pdf): PDF32000
    // 12.5.5's algorithm maps BBox to Rect only *after* first applying the
    // appearance's own Matrix to BBox's four corners -- so a Matrix with a
    // real translation (common: it is exactly what lets a PDF producer
    // normalize content near the origin while BBox and Rect are declared
    // equal) must be cancelled out by that mapping, not left in place.
    // Mapping straight from the untransformed BBox values instead left
    // that translation doubled into the final position.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("annotation-matrix.pdf");
    let mut document = Document::with_version("1.7");
    let appearance = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![100.into(), 100.into(), 200.into(), 150.into()],
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), (-100).into(), (-100).into()],
        },
        b"0 0 1 rg 100 100 100 50 re f".to_vec(),
    ));
    let annotation = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![100.into(), 100.into(), 200.into(), 150.into()],
        "AP" => dictionary! { "N" => appearance },
    });
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
        "Resources" => dictionary! {},
        "Contents" => content,
        "Annots" => vec![Object::Reference(annotation)],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The path's `d` holds the appearance's own local content coordinates
    // (100,100)-(200,150); the SVG `transform` carries the CTM. Correctly
    // mapped, that CTM composes to the identity (the appearance's own
    // Matrix translation cancelled by the BBox-to-Rect mapping) with only
    // the page's Y-flip left: matrix(1 0 0 -1 0 300). Left doubled, as the
    // bug did, the appearance's own Matrix translation survives into the
    // CTM instead of cancelling: matrix(1 0 0 -1 -100 400).
    assert!(svg.contains("matrix(1 0 0 -1 0 300)"), "{svg}");
    assert!(!svg.contains("matrix(1 0 0 -1 -100 400)"), "{svg}");
}

#[test]
fn draws_a_zero_height_filled_rectangle_as_a_hairline() {
    // Real-world regression (pdf.js's own issue4260_reduced.pdf): a filled
    // rectangle with zero width or height has no area, literally nothing
    // to fill in a correct vector renderer -- but CAD exports and ruled
    // grids commonly draw a hairline this way instead of stroking it,
    // relying on every mainstream PDF viewer's rasterizer to give a
    // degenerate fill a sliver of coverage. A whole page of these (a grid
    // of horizontal and vertical rule lines) rendered as a totally blank
    // box.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("zero-height-rect.pdf");
    let mut document = Document::with_version("1.7");
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"0 0 0 RG 0 0 0 rg 20 50 100 0 re f".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! {},
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("M 20 50 L 120 50"), "{svg}");
    assert!(svg.contains("stroke=\"#000000\""), "{svg}");
}

#[test]
fn hides_content_in_an_optional_content_group_that_is_off_by_default() {
    // A PDF layer (Optional Content Group) that the document's own default
    // configuration turns off must not appear, the same way no viewer would
    // show it -- CAD- and GIS-exported PDFs commonly ship several such
    // layers, on by default or not.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ocg.pdf");
    let mut document = Document::with_version("1.7");
    let visible_ocg = document
        .add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Visible") });
    let hidden_ocg = document
        .add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Hidden") });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"q /OC /VisG BDC 0 1 0 rg 20 20 100 100 re f EMC Q\nq /OC /HidG BDC 1 0 0 rg 150 20 100 100 re f EMC Q\n".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
        "Resources" => dictionary! {
            "Properties" => dictionary! { "VisG" => visible_ocg, "HidG" => hidden_ocg },
        },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages,
        "OCProperties" => dictionary! {
            "OCGs" => vec![Object::Reference(visible_ocg), Object::Reference(hidden_ocg)],
            "D" => dictionary! { "OFF" => vec![Object::Reference(hidden_ocg)] },
        },
    });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#00FF00"), "{svg}");
    assert!(!svg.contains("#FF0000"), "{svg}");
}

#[test]
fn hides_an_optional_content_group_whose_off_list_is_an_indirect_reference() {
    // Real-world regression (pdf.js's own issue269_2.pdf): /OCProperties
    // /D /OFF (and /ON, and the top-level /OCGs array) can each just as
    // legally be an indirect reference to an array as an inline one --
    // this file's own /OFF is its own separate object. Reading it with a
    // non-dereferencing `dictionary.get` before checking `.as_array()`
    // silently treated the reference as absent, hiding nothing at all: a
    // document meant to show one of 49 layers instead showed all of them
    // stacked on top of each other.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ocg-indirect-off.pdf");
    let mut document = Document::with_version("1.7");
    let visible_ocg = document
        .add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Visible") });
    let hidden_ocg = document
        .add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Hidden") });
    let off_array = document.add_object(vec![Object::Reference(hidden_ocg)]);
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"q /OC /VisG BDC 0 1 0 rg 20 20 100 100 re f EMC Q\nq /OC /HidG BDC 1 0 0 rg 150 20 100 100 re f EMC Q\n".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
        "Resources" => dictionary! {
            "Properties" => dictionary! { "VisG" => visible_ocg, "HidG" => hidden_ocg },
        },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages,
        "OCProperties" => dictionary! {
            "OCGs" => vec![Object::Reference(visible_ocg), Object::Reference(hidden_ocg)],
            "D" => dictionary! { "OFF" => Object::Reference(off_array) },
        },
    });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#00FF00"), "{svg}");
    assert!(!svg.contains("#FF0000"), "{svg}");
}

#[test]
fn hides_a_form_xobject_whose_oc_entry_is_an_ocmd_naming_a_single_ocg() {
    // Real-world regression (pdf.js's own issue12007_reduced.pdf): an
    // OCMD's /OCGs can legally name a single OCG directly -- a bare
    // indirect reference, not wrapped in an array -- rather than an array
    // of them (PDF32000 8.11.2.3). The fix for a *different*
    // /OCProperties dereferencing gap earlier this session dereferenced
    // /OCGs before checking whether the result was an array, which for
    // this single-reference form discarded the object ID the hidden-group
    // lookup needs: a Form XObject's own /OC entry (as opposed to a
    // content-stream BDC wrapper) pointing at an OCMD with a bare single
    // OCG reference always rendered as visible regardless of that OCG's
    // real default state.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ocg-single-ocmd-reference.pdf");
    let mut document = Document::with_version("1.7");
    let hidden_ocg = document
        .add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Hidden") });
    let ocmd = document.add_object(dictionary! {
        "Type" => "OCMD", "OCGs" => Object::Reference(hidden_ocg),
    });
    let form = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
            "OC" => Object::Reference(ocmd),
        },
        b"1 0 0 rg 20 20 100 100 re f".to_vec(),
    ));
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"0 1 0 rg 150 20 100 100 re f\n/Fm0 Do".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
        "Resources" => dictionary! {
            "XObject" => dictionary! { "Fm0" => Object::Reference(form) },
        },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages,
        "OCProperties" => dictionary! {
            "OCGs" => vec![Object::Reference(hidden_ocg)],
            "D" => dictionary! { "OFF" => vec![Object::Reference(hidden_ocg)] },
        },
    });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(
        report.pages[0].warnings.is_empty(),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#00FF00"), "{svg}");
    assert!(!svg.contains("#FF0000"), "{svg}");
}

#[test]
fn outlines_a_type3_glyph_whose_char_proc_contains_an_inline_image() {
    // Real-world regression (pdf.js's own bug1245391_reduced.pdf): a Type3
    // glyph's own CharProc content stream can itself contain a `BI...ID...EI`
    // inline image, same as any other content stream. Every content stream
    // except the page's top-level one was being handed to lopdf's own
    // `Content::decode` directly, which has no notion of inline-image syntax
    // at all and gets thrown off by the raw binary sample data between `ID`
    // and `EI` -- unlike the page's own decoder, which pre-scans for
    // `BI`/`ID`/`EI` before ever reaching that generic parser. The
    // resulting parse failure discarded not just the inline image but the
    // glyph's entire remaining paint program, so the whole page (just this
    // one Type3 glyph) rendered as nothing.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("type3-inline-image.pdf");
    let mut document = Document::with_version("1.7");
    let glyph_id = document.add_object(Stream::new(
        dictionary! {},
        b"600 0 0 0 600 700 d1\nq 1 0 0 1 0 0 cm\nBI /W 1 /H 1 /BPC 8 /CS /G ID \x01 EI\nQ\n0 0 0 rg\n0 0 600 700 re f\n".to_vec(),
    ));
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "Name" => "F3",
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 700.into()],
        "FontMatrix" => vec![Object::Real(0.001), 0.into(), 0.into(), Object::Real(0.001), 0.into(), 0.into()],
        "CharProcs" => dictionary! { "A" => Object::Reference(glyph_id) },
        "Encoding" => dictionary! { "Type" => "Encoding", "Differences" => vec![65.into(), Object::Name(b"A".to_vec())] },
        "FirstChar" => 65,
        "LastChar" => 65,
        "Widths" => vec![600.into()],
        "Resources" => dictionary! {},
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F3" => Object::Reference(font_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F3".to_vec()), 100.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 20.into()],
            ),
            Operation::new("Tj", vec![Object::string_literal("A")]),
            Operation::new("ET", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(
        report.pages[0]
            .warnings
            .iter()
            .all(|warning| !warning.contains("syntax decoder")
                && !warning.contains("drew nothing")
                && !warning.contains("content is invalid")),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("M 0 0 L 600 0 L 600 700 L 0 700 Z"), "{svg}");
}

#[test]
fn recovers_an_unfiltered_inline_image_whose_last_data_byte_is_not_a_pdf_delimiter() {
    // Real-world regression (pdf.js's own bug1513120_reduced.pdf and
    // issue10388_reduced.pdf): the PDF spec recommends, but does not
    // require, a white-space byte between an inline image's last data byte
    // and its closing `EI` operator. Both files ship unfiltered 1-bit
    // images whose raw data happens to end on a byte that is not itself a
    // PDF delimiter (whitespace or one of `[]<>()/`), with `EI` following
    // immediately -- which defeated the token search's own delimiter check
    // (it requires the byte *before* a candidate `EI` match to itself be a
    // delimiter, to avoid matching stray "EI" bytes inside binary noise),
    // so no `EI` was ever found and the whole content stream failed to
    // decode instead of just this one image.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline-image-no-separator.pdf");
    let mut document = Document::with_version("1.7");
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"q 20 0 0 20 0 0 cm\nBI /W 8 /H 1 /BPC 1 /CS /G ID \xffEI\nQ\n".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        "Resources" => dictionary! {},
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/png;base64,"), "{svg}");
}

#[test]
fn paints_an_inline_image_mask_with_the_current_fill_color() {
    // Real-world regression (pdf.js's own images_1bit_grayscale.pdf): an
    // inline image's `/IM true` (`/ImageMask`) flag was never recognized at
    // all. Every inline `BI...ID...EI` block is fully decoded up front by
    // `InlineImageSpec`, which builds a plain `Stream` carrying only
    // `Width`/`Height`/`BitsPerComponent`/`ColorSpace`/`Interpolate` -- with
    // no `ImageMask` field to parse it into, an inline stencil mask always
    // looked like an ordinary grayscale image instead, painted in its own
    // black/white sample values rather than stenciled through the current
    // fill color. The file's own "Inline Image Mask" test box rendered in
    // plain black instead of the magenta the page sets immediately before
    // it, the one inline-image box out of eight that differed from
    // poppler's own rendering.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline-image-mask.pdf");
    let mut document = Document::with_version("1.7");
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"q\n1 0 1 rg\n20 0 0 20 0 0 cm\nBI /W 8 /H 1 /BPC 1 /IM true ID \x00 EI\nQ\n".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        "Resources" => dictionary! {},
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("data:image/png;base64,")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .unwrap();
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(&pixels[0..4], &[255, 0, 255, 255], "{pixels:?}");
}

#[test]
fn sizes_a_page_whose_media_box_is_an_indirect_reference() {
    // Real-world regression (pdf.js's own bug852992_reduced.pdf): a page's
    // own /MediaBox entry can legally be an indirect reference to an array,
    // rather than the array inline -- this file's own producer shares one
    // indirect array object as the /BBox of several Form XObjects and
    // shadings too. Reading it with a non-dereferencing dictionary lookup
    // before checking whether it was an array silently treated the whole
    // /MediaBox as absent, falling back to a default US Letter page --
    // 612x792 -- instead of this file's real, much smaller and
    // negative-origin 540x190 page, squeezing all of its actual content
    // into a small corner of a mostly blank canvas.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("indirect-mediabox.pdf");
    let mut document = Document::with_version("1.7");
    let media_box = document.add_object(vec![
        Object::Real(-270.0),
        Object::Real(-95.0),
        Object::Real(270.0),
        Object::Real(95.0),
    ]);
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"0 1 0 rg -270 -95 540 190 re f".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => Object::Reference(media_box),
        "Resources" => dictionary! {},
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("width=\"540pt\" height=\"190pt\""), "{svg}");
}

#[test]
fn resolves_an_indexed_image_palette_through_its_own_base_color_space() {
    // Real-world regression (pdf.js's own issue10339_reduced.pdf): an
    // Indexed color space's base need not be DeviceRGB/Gray/CMYK -- Lab is
    // legal too (as are CalGray, CalRGB, Separation, DeviceN and
    // ICCBased). Each palette entry was instead assumed to already be raw
    // sRGB bytes whenever it happened to have 3 components, which is true
    // for an RGB base but not for a 3-component Lab one -- so an
    // indexed-Lab image rendered with the palette's raw L*a*b* byte values
    // reinterpreted directly as red/green/blue, producing a completely
    // wrong (though structurally identical, since only the color channel
    // is wrong) muddy brown/pink image instead of the intended blue tones.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("indexed-lab.pdf");
    let mut document = Document::with_version("1.7");
    // A single palette entry: L*=80, a*=0, b*=-88 (a strong blue), stored
    // as the raw bytes a Lab-based Indexed image palette actually uses --
    // L in [0, 100] mapped to [0, 255], a/b in this Lab space's own
    // [-128, 127] range mapped to [0, 255].
    let lookup = document.add_object(Stream::new(dictionary! {}, vec![204, 128, 40]));
    let color_space = document.add_object(vec![
        Object::Name(b"Indexed".to_vec()),
        vec![
            Object::Name(b"Lab".to_vec()),
            Object::Dictionary(dictionary! {
                "WhitePoint" => vec![0.9505.into(), 1.into(), 1.089.into()],
                "Range" => vec![(-128).into(), 127.into(), (-128).into(), 127.into()],
            }),
        ]
        .into(),
        0.into(),
        Object::Reference(lookup),
    ]);
    let image = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => 1, "Height" => 1,
            "ColorSpace" => Object::Reference(color_space),
            "BitsPerComponent" => 8,
        },
        vec![0],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Im0" => Object::Reference(image) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    100.into(),
                    0.into(),
                    0.into(),
                    100.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("data:image/png;base64,")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .unwrap();
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixels).unwrap();
    // Blue, not the muddy brown the raw Lab bytes would give read as RGB.
    assert!(
        pixels[2] > pixels[0] && pixels[2] > pixels[1],
        "expected blue to dominate, got {pixels:?}"
    );
}

#[test]
fn fills_a_path_with_a_function_based_shading_pattern() {
    // A shading pattern (PatternType 2) has no SVG gradient equivalent once
    // its ShadingType is 1 (function-based) rather than 2/3 (axial/radial):
    // the fill silently disappeared. Tessellate it into an SVG <pattern>
    // instead, the same way the sh operator already tessellates one.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("function-pattern.pdf");
    let mut document = Document::with_version("1.7");
    let function = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 200.into(), 0.into(), 200.into()],
            "Range" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        b"{ 200 div exch 200 div exch 0.5 }".to_vec(),
    ));
    let shading = dictionary! {
        "ShadingType" => 1, "ColorSpace" => "DeviceRGB",
        "Domain" => vec![0.into(), 200.into(), 0.into(), 200.into()],
        "Function" => function,
    };
    let pattern = document.add_object(dictionary! {
        "Type" => "Pattern", "PatternType" => 2, "Shading" => shading,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"/Pattern cs /P0 scn 20 20 160 160 re f\n".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Resources" => dictionary! { "Pattern" => dictionary! { "P0" => pattern } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(
        report.pages[0].warnings.is_empty(),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<pattern"), "{svg}");
    assert!(
        svg.contains("data-content-kind=\"function-shading-cell\""),
        "{svg}"
    );
    assert!(svg.contains("fill=\"url(#"), "{svg}");
}

#[test]
fn fills_a_path_with_a_mesh_shading_pattern() {
    // A mesh shading pattern (PatternType 2, ShadingType 4-7) has no SVG
    // gradient equivalent either: the fill silently disappeared. Tessellate
    // it into an SVG <pattern> the same way the sh operator already does,
    // clipped to the mesh's own vertex bounds so the pattern's necessary
    // tiling does not repeat visibly beyond the shape the mesh actually
    // covers.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mesh-pattern.pdf");
    let mut document = Document::with_version("1.7");
    // ShadingType 4: free-form Gouraud-shaded triangle mesh. Each vertex is
    // flag(8 bits) + x,y(16 bits each) + r,g,b(8 bits each), decoded through
    // /Decode into a 0..200 coordinate range and 0..1 color range.
    fn vertex(flag: u8, x: u16, y: u16, r: u8, g: u8, b: u8) -> [u8; 8] {
        let xb = x.to_be_bytes();
        let yb = y.to_be_bytes();
        [flag, xb[0], xb[1], yb[0], yb[1], r, g, b]
    }
    let scale = |value: f64| -> u16 { (value / 200.0 * 65535.0) as u16 };
    let mut data = Vec::new();
    data.extend(vertex(0, scale(20.0), scale(20.0), 255, 0, 0));
    data.extend(vertex(0, scale(180.0), scale(20.0), 0, 255, 0));
    data.extend(vertex(0, scale(100.0), scale(180.0), 0, 0, 255));
    let shading = document.add_object(Stream::new(
        dictionary! {
            "ShadingType" => 4, "ColorSpace" => "DeviceRGB",
            "BitsPerCoordinate" => 16, "BitsPerComponent" => 8, "BitsPerFlag" => 8,
            "Decode" => vec![0.into(), 200.into(), 0.into(), 200.into(), 0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        data,
    ));
    let pattern = document.add_object(dictionary! {
        "Type" => "Pattern", "PatternType" => 2, "Shading" => shading,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"/Pattern cs /P0 scn 10 10 180 180 re f\n".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Resources" => dictionary! { "Pattern" => dictionary! { "P0" => pattern } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(
        report.pages[0].warnings.is_empty(),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<pattern"), "{svg}");
    assert!(svg.contains("data-content-kind=\"mesh-triangle\""), "{svg}");
    assert!(svg.contains("fill=\"url(#"), "{svg}");
}

#[test]
fn keeps_invisible_text_selectable_instead_of_dropping_it() {
    // Rendering mode 3 (invisible) is how a searchable scanned PDF hides its
    // OCR text layer behind the scanned image: nothing should paint, but
    // the text itself is the entire point of that layer, and dropping it
    // turned a searchable document into one that is not.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("invisible-text.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => Object::Reference(font_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new("Tr", vec![3.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 60.into()],
            ),
            Operation::new(
                "Tj",
                vec![Object::String(
                    b"Hidden OCR text".to_vec(),
                    lopdf::StringFormat::Literal,
                )],
            ),
            Operation::new("ET", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("aria-label=\"Hidden OCR text\""), "{svg}");
    assert!(svg.contains("Hidden"), "{svg}");
    let hidden = svg.split("aria-label=\"Hidden OCR text\"").next().unwrap();
    let text_tag = hidden.rsplit("<text").next().unwrap();
    assert!(text_tag.contains("stroke=\"none\""), "{svg}");
    let tspan = svg.split_once("aria-label=\"Hidden OCR text\"").unwrap().1;
    assert!(tspan.contains("fill=\"none\""), "{svg}");
}

#[test]
fn converts_embedded_true_type_to_outlines_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("embedded-font.pdf");
    let mut document = Document::with_version("1.7");
    let bytes = font_test_data::font(None, false, false);
    let font_file = document.add_object(Stream::new(
        dictionary! { "Length1" => bytes.len() as i64 },
        bytes,
    ));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "Arial", "Flags" => 32,
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 900.into()],
        "Ascent" => 800, "Descent" => -200, "CapHeight" => 700, "StemV" => 80,
        "FontFile2" => font_file,
    });
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "TrueType", "BaseFont" => "Arial",
        "FirstChar" => 32, "LastChar" => 66,
        "Widths" => (32..=66).map(|code| Object::Integer(if code == 32 { 250 } else { 500 })).collect::<Vec<_>>(),
        "FontDescriptor" => descriptor, "Encoding" => "WinAnsiEncoding",
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 20 Tf 20 80 Td (A B) Tj ET".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let options = ConvertOptions {
        outline_embedded_pdf_text: true,
        ..Default::default()
    };
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    let report = convert_path(&input, &first, &options).unwrap();
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(first.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"text-outline\""), "{svg}");
    assert!(svg.contains("L 0.5 0"), "{svg}");
    convert_path(&input, &second, &options).unwrap();
    assert_eq!(
        svg,
        fs::read_to_string(second.join("page-0001.svg")).unwrap()
    );
}

#[test]
fn refuses_nonempty_output_without_modifying_existing_files() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.pdf");
    let output = temporary.path().join("out");
    make_pdf(&input);
    fs::create_dir(&output).unwrap();
    let existing = output.join("page-0001.svg");
    fs::write(&existing, "keep me").unwrap();
    assert!(convert_path(&input, &output, &ConvertOptions::default()).is_err());
    assert_eq!(fs::read_to_string(existing).unwrap(), "keep me");
}

#[test]
fn converts_minimal_pdf() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.pdf");
    let output = temporary.path().join("out");
    make_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Pdf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hello PDF"));
    assert!(svg.contains("data-source-format=\"pdf\""));
    assert!(svg.contains("<tspan x=\"0\" y=\"0\">H</tspan>"));
    assert!(svg.contains("xml:space=\"preserve\""));
}

#[test]
fn applies_pdf_page_user_unit_to_dimensions_and_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("user-unit.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let resources_id = document.add_object(dictionary! {});
    let content = Content {
        operations: vec![
            Operation::new("re", vec![10.into(), 10.into(), 20.into(), 10.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    document.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "Contents" => Object::Reference(content_id),
            "Resources" => Object::Reference(resources_id),
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 50.into()],
            "UserUnit" => 2,
        }),
    );
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    document.trailer.set("Root", Object::Reference(catalog_id));
    document.save(&input).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.pages[0].width_points, 200.0);
    assert_eq!(report.pages[0].height_points, 100.0);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("width=\"200pt\" height=\"100pt\""));
    assert!(svg.contains("transform=\"matrix(2 0 0 -2 0 100)\""));
}

#[test]
fn applies_pdf_ext_gstate_line_style_parameters() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ext-gstate-line-style.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let state_id = document.add_object(dictionary! {
        "Type" => "ExtGState",
        "LW" => 14,
        "LC" => 2,
        "LJ" => 1,
        "ML" => 4,
        "D" => vec![Object::Array(vec![8.into(), 3.into()]), 2.into()],
    });
    let resources_id = document.add_object(dictionary! {
        "ExtGState" => dictionary! { "GS1" => Object::Reference(state_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("gs", vec![Object::Name(b"GS1".to_vec())]),
            Operation::new("m", vec![20.into(), 20.into()]),
            Operation::new("l", vec![200.into(), 20.into()]),
            Operation::new("S", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("stroke-width=\"14\""));
    assert!(svg.contains("stroke-linecap=\"square\""));
    assert!(svg.contains("stroke-linejoin=\"round\""));
    assert!(svg.contains("stroke-dasharray=\"8 3\""));
    assert!(svg.contains("stroke-dashoffset=\"2\""));
}

#[test]
fn restores_pdf_text_state_parameters_across_q_and_q_restore() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("text-state-restore.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => Object::Reference(font_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new("Tr", vec![2.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 60.into()],
            ),
            Operation::new(
                "Tj",
                vec![Object::String(
                    b"Outlined".to_vec(),
                    lopdf::StringFormat::Literal,
                )],
            ),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 30.into()],
            ),
            Operation::new(
                "Tj",
                vec![Object::String(
                    b"Body".to_vec(),
                    lopdf::StringFormat::Literal,
                )],
            ),
            Operation::new("ET", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let outlined = svg.split("aria-label=\"Outlined\"").next().unwrap();
    assert!(
        outlined
            .rsplit("<text")
            .next()
            .unwrap()
            .contains("stroke=\"#000000\"")
    );
    let body = svg.split("aria-label=\"Body\"").next().unwrap();
    assert!(
        body.rsplit("<text")
            .next()
            .unwrap()
            .contains("stroke=\"none\"")
    );
}

#[test]
fn ignores_inline_image_tokens_inside_pdf_strings() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bi-string.pdf");
    let output = temporary.path().join("out");
    make_pdf_with_text(&input, "BI without inline ID");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("BI without inline ID"));
}

#[test]
fn ignores_unknown_pdf_operators_inside_compatibility_sections() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("compatibility.pdf");
    let output = temporary.path().join("out");
    make_pdf_compatibility_section(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"vector\""));
    assert!(svg.contains("fill=\"#FF0000\""));
}

#[test]
fn converts_pdf_type3_charproc_glyphs() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("type3.pdf");
    let output = temporary.path().join("out");
    make_type3_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"type3-text\""));
    assert!(svg.contains("aria-label=\"A\""));
    assert!(svg.contains("M 1 0 L 300 700 L 600 0 Z"));
    assert!(svg.contains("fill=\"#FF0000\""));
}

#[test]
fn converts_pdf_packed_samples_and_stencil_images() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("packed-images.pdf");
    let output = temporary.path().join("out");
    make_packed_images_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // One data URI per image: `href` only, no duplicate `xlink:href`.
    assert_eq!(svg.matches("data:image/png;base64,").count(), 2);
    assert_eq!(svg.matches("data-content-kind=\"image\"").count(), 2);
    assert_eq!(svg.matches("image-rendering=\"pixelated\"").count(), 2);
}

#[test]
fn converts_pdf_jpeg_soft_mask_to_png_alpha() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("jpeg-soft-mask.pdf");
    let output = temporary.path().join("out");
    make_jpeg_soft_mask_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("data:image/png;base64,")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .unwrap();
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!((info.width, info.height), (2, 1));
    assert!(pixels[3] < 16, "alpha={}", pixels[3]);
    assert!(pixels[7] > 239, "alpha={}", pixels[7]);
}

#[test]
fn resamples_a_soft_mask_whose_dimensions_do_not_match_its_base_image() {
    // Real-world regression (pdf.js's own
    // chrome-text-selection-markedContent.pdf): a soft mask's own pixel
    // dimensions need not match the base image it applies to -- each is
    // independently mapped onto the same unit image square, so a real
    // renderer resamples both to whatever resolution it actually draws at.
    // Chrome's own "print to PDF" renders a text-selection highlight
    // exactly this way: a tiny, uniformly-coloured base image (here 1x1)
    // stretched over the highlighted run, with the actual highlight shape
    // -- fine per-glyph detail -- carried entirely by a full-resolution
    // soft mask (here 2x1). Dropping the mask outright whenever its
    // dimensions differed from the base image's, as a same-size check did,
    // discarded that detail and left the highlight either solid or
    // invisible instead of glyph-shaped; downsampling the mask down to the
    // base image's own tiny resolution would have been just as wrong, so
    // the two are resampled to their larger common resolution instead.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mismatched-soft-mask.pdf");
    let output = temporary.path().join("out");
    make_mismatched_soft_mask_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("data:image/png;base64,")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .unwrap();
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!((info.width, info.height), (2, 1));
    assert_eq!(&pixels[0..3], &[255, 0, 0], "left pixel colour");
    assert_eq!(&pixels[4..7], &[255, 0, 0], "right pixel colour");
    assert!(pixels[3] < 16, "left alpha={}", pixels[3]);
    assert!(pixels[7] > 239, "right alpha={}", pixels[7]);
}

#[test]
fn normalizes_inverted_pdf_cmyk_jpeg_to_rgb() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("cmyk-jpeg.pdf");
    let output = temporary.path().join("out");
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 100)
        .encode(
            &[255, 255, 255, 255],
            1,
            1,
            jpeg_encoder::ColorType::CmykAsYcck,
        )
        .unwrap();
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ColorSpace" => "DeviceCMYK",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg,
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Cmyk" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"Cmyk".to_vec())])],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("data:image/jpeg;base64,")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .unwrap();
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(jpeg));
    let pixels = decoder.decode().unwrap();
    assert!(pixels.iter().all(|value| *value > 240), "{pixels:?}");
}

#[test]
fn draws_a_jpeg_image_whose_filter_chain_wraps_it_in_an_outer_flate_layer() {
    // Real-world regression (pdf.js's own comments.pdf): a stream's
    // /Filter can be an array that wraps an image codec filter in an
    // outer, ordinary compression filter -- /Filter [/FlateDecode
    // /DCTDecode] is legal and appears in real files, using Flate for a
    // little extra compression on top of the JPEG. The still-Flate-
    // compressed bytes are not a JPEG stream themselves, so handing them
    // directly to the JPEG decoder failed immediately with "first two
    // bytes are not an SOI marker" and the image (here, with an SMask)
    // was skipped entirely.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("flate-wrapped-jpeg.pdf");
    let output = temporary.path().join("out");
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 100)
        .encode(&[200, 40, 40], 1, 1, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    let mut flate_encoder =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    flate_encoder.write_all(&jpeg).unwrap();
    let flate_wrapped_jpeg = flate_encoder.finish().unwrap();
    let mut document = Document::with_version("1.7");
    let mask_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => 1, "Height" => 1,
            "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8,
        },
        vec![255],
    ));
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image",
            "Width" => 1, "Height" => 1,
            "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
            "Filter" => vec![Object::Name(b"FlateDecode".to_vec()), Object::Name(b"DCTDecode".to_vec())],
            "SMask" => Object::Reference(mask_id),
        },
        flate_wrapped_jpeg,
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Im0" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"Im0".to_vec())])],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(
        report.pages[0].warnings.is_empty(),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("data:image/png;base64,")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .unwrap();
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert!(
        pixels[0] > 150 && pixels[1] < 100 && pixels[2] < 100,
        "{pixels:?}"
    );
    assert!(pixels[3] > 240, "alpha={}", pixels[3]);
}

#[test]
fn rejects_pdf_image_dimension_bombs_before_allocation() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("image-bomb.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 100_000,
            "Height" => 100_000,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "SMaskInData" => 1,
            "Filter" => "JPXDecode",
        },
        Vec::new(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Bomb" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"Bomb".to_vec())])],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    // The guard charges the embedded alpha plane too, so the 40 GB decoded
    // image is never allocated. One unusable image is a warning, not a reason
    // to discard every page in the document.
    assert_eq!(report.page_count, 1);
    assert!(
        report.warnings.iter().any(
            |warning| warning.contains("expands to 100000x100000 pixels")
                && warning.contains("was skipped")
        ),
        "{:?}",
        report.warnings
    );
}

#[test]
fn skips_pdf_images_with_invalid_packed_samples_and_reports_warning() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("invalid-packed-image.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 2,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
        },
        vec![0],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Broken" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"Broken".to_vec())])],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("invalid packed samples and was skipped")),
        "{:?}",
        report.warnings
    );
    assert!(output.join("page-0001.svg").exists());
}

#[test]
fn converts_pdf_axial_shading_to_svg_gradient() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shading.pdf");
    let output = temporary.path().join("out");
    make_shading_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<linearGradient"));
    assert!(svg.contains("#FF0000"));
    assert!(svg.contains("#0000FF"));
    assert!(svg.contains("data-content-kind=\"gradient\""));
}

#[test]
fn converts_pdf_function_shading_with_calculator_function() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("function-shading.pdf");
    let output = temporary.path().join("out");
    make_function_shading_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"function-shading-cell\""));
    assert!(svg.contains("#01FE40"));
    assert!(
        svg.matches("data-content-kind=\"function-shading-cell\"")
            .count()
            >= 100
    );
}

#[test]
fn converts_pdf_luminosity_soft_mask_to_svg_mask() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("soft-mask.pdf");
    let output = temporary.path().join("out");
    make_soft_mask_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<mask id=\"pdf-mask-1-1\""));
    assert!(svg.contains("mask-type:luminance"));
    assert!(svg.contains("mask=\"url(#pdf-mask-1-1)\""));
}

#[test]
fn converts_pdf_soft_mask_transfer_function_to_svg_filter() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("soft-mask-transfer.pdf");
    let output = temporary.path().join("out");
    make_soft_mask_transfer_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"pdf-mask-1-1-transfer\""));
    assert!(svg.contains("<feColorMatrix"));
    assert!(svg.contains("<feFuncR type=\"table\""));
    assert!(svg.contains(" 0.25 "));
}

#[test]
fn preserves_nonisolated_pdf_transparency_group() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("nonisolated.pdf");
    let output = temporary.path().join("out");
    make_nonisolated_group_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"transparency-group\""));
    assert!(!svg.contains("isolation:isolate"));
}

#[test]
fn converts_pdf_special_color_spaces_and_tint_functions() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("special-colors.pdf");
    let output = temporary.path().join("out");
    make_special_color_space_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("fill=\"#33CC4D\""));
    assert!(svg.contains("fill=\"#33CC80\""));
    assert!(svg.contains("fill=\"#777777\""));
    assert!(svg.contains("fill=\"#FF0000\""));
}

#[test]
fn converts_pdf_shading_pattern_with_background_bbox_and_alpha() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shading-pattern.pdf");
    let output = temporary.path().join("out");
    make_shading_pattern_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<linearGradient"));
    assert!(svg.contains("id=\"pdf-pattern-clip-1-1\""));
    assert!(svg.contains("stop-color=\"#CC3300\" stop-opacity=\"0.9375\""));
    assert!(svg.contains("stop-color=\"#00FF00\" stop-opacity=\"0.75\""));
}

#[test]
fn converts_pdf_colored_and_uncolored_tiling_patterns() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("tiling-patterns.pdf");
    let output = temporary.path().join("out");
    make_tiling_pattern_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.matches("<pattern id=\"pdf-pattern-").count() >= 4);
    assert!(svg.contains("fill=\"#FF0000\""));
    assert!(svg.contains("fill=\"#0000FF\""));
    assert!(svg.contains("fill=\"#00FF00\""));
    assert!(svg.matches("fill=\"url(#pdf-pattern-").count() >= 4);
}

#[test]
fn positions_a_tiling_pattern_at_its_own_nonzero_bbox_origin() {
    // Real-world regression (pdf.js's own 22060_A1_01_Plans.pdf): a tiling
    // pattern's own content stream draws in absolute BBox-space coordinates
    // (PDF32000 8.7.3.1), which need not start at the origin at all -- a
    // producer is free to declare e.g. `/BBox [20 30 30 40]`, matching
    // wherever on the page the pattern's own designer happened to lay it
    // out (this real file's patterns all have BBoxes with origins in the
    // thousands, not zero). The generated SVG `<pattern>` element's own x/y
    // set where that content lands within the tile it repeats, but was
    // hardcoded to (0, 0) regardless of the real BBox origin, so content
    // drawn at its own genuine BBox-space coordinates no longer overlapped
    // the pattern's assumed [0, width] x [0, height] viewport at all --
    // this file's evacuation-route highlight bands, filled with exactly
    // this kind of pattern, rendered as wrong-sized fragments scattered
    // near the top of the page instead of matching poppler's own,
    // correctly-sized and positioned bands.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("tiling-pattern-bbox-origin.pdf");
    let mut document = Document::with_version("1.7");
    let pattern_content = Content {
        operations: vec![
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![20.into(), 30.into(), 10.into(), 10.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let pattern_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "Pattern",
            "PatternType" => 1,
            "PaintType" => 1,
            "TilingType" => 1,
            "BBox" => vec![20.into(), 30.into(), 30.into(), 40.into()],
            "XStep" => 10,
            "YStep" => 10,
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()],
            "Resources" => dictionary! {},
        },
        pattern_content.encode().unwrap(),
    ));
    let resources_id = document.add_object(dictionary! {
        "Pattern" => dictionary! { "P1" => Object::Reference(pattern_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("cs", vec![Object::Name(b"Pattern".to_vec())]),
            Operation::new("scn", vec![Object::Name(b"P1".to_vec())]),
            Operation::new("re", vec![0.into(), 0.into(), 100.into(), 100.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let output = temporary.path().join("out");
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("<pattern id=\"pdf-pattern-") && svg.contains("x=\"20\" y=\"30\""),
        "{svg}"
    );
}

#[test]
fn converts_minimal_pptx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Title"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill></p:spPr><p:txBody><a:p><a:r><a:rPr sz="2400" b="1"/><a:t>Hello PPTX</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Pptx);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hello PPTX"));
    assert!(svg.contains("#DDEEFF"));
}

#[test]
fn converts_pptx_outer_shadow_to_bounded_svg_filter() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("outer-shadow.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Shadow card"/></p:nvSpPr><p:spPr><a:xfrm rot="1800000"><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="1270000"/></a:xfrm><a:prstGeom prst="roundRect"/><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill><a:effectLst><a:outerShdw blurRad="25400" dist="25400" dir="5400000" rotWithShape="0"><a:srgbClr val="123456"><a:alpha val="50000"/></a:srgbClr></a:outerShdw></a:effectLst></p:spPr><p:txBody><a:p><a:r><a:rPr sz="1200"/><a:t>Shadow text</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"outer-shadow-pptx-slide-1-1\""));
    assert!(svg.contains("stdDeviation=\"1\""));
    assert!(svg.contains("dx=\"0\" dy=\"2\""));
    assert!(svg.contains("flood-color=\"#123456\" flood-opacity=\"0.5\""));
    assert!(svg.contains("fill=\"#DDEEFF\""));
    assert_eq!(svg.matches("filter=\"url(#outer-shadow-").count(), 1);
    assert!(svg.contains("Shadow text"));
}

#[test]
fn converts_pptx_connector_glow_to_bounded_svg_filter() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("connector-glow.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:a14="a14"><p:cSld><p:spTree><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="1" name="Glowing connector"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="1270000"/></a:xfrm><a:prstGeom prst="line"/><a:ln w="25400"><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:ln><a:effectLst><a:glow rad="63500"><a:schemeClr val="accent2"><a:satMod val="175000"/><a:alpha val="70000"/></a:schemeClr></a:glow></a:effectLst><a14:hiddenEffects><a:effectLst><a:glow rad="127000"><a:srgbClr val="FF00FF"/></a:glow></a:effectLst></a14:hiddenEffects></p:spPr></p:cxnSp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("filter=\"url(#glow-").count(), 1);
    assert!(svg.contains("id=\"glow-pptx-slide-1-1\""));
    assert!(svg.contains("stdDeviation=\"2.5\" result=\"glow-blur\""));
    assert!(svg.contains("flood-color=\"#FF7A1F\""));
    assert!(svg.contains("flood-opacity=\"0.7\" result=\"glow-color\""));
    assert!(svg.contains("<feMergeNode in=\"glow\"/>"));
}

#[test]
fn converts_pptx_text_effects_but_ignores_hidden_compatibility_effects() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("text-shadow.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:a14="a14"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Text effects"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="1270000"/></a:xfrm><a:prstGeom prst="rect"/><a:noFill/><a14:hiddenFill><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a14:hiddenFill><a14:hiddenLine><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a14:hiddenLine><a14:hiddenEffects><a:effectLst><a:outerShdw dist="127000" dir="0"><a:srgbClr val="FF00FF"/></a:outerShdw><a:glow rad="127000"><a:srgbClr val="FF00FF"/></a:glow></a:effectLst></a14:hiddenEffects></p:spPr><p:txBody><a:p><a:r><a:rPr sz="1800"><a:effectLst><a:outerShdw blurRad="38100" dist="38100" dir="2700000"><a:srgbClr val="000000"><a:alpha val="43137"/></a:srgbClr></a:outerShdw><a:glow rad="25400"><a:srgbClr val="00AAFF"><a:alpha val="50000"/></a:srgbClr></a:glow></a:effectLst></a:rPr><a:t>Visible text effects</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("filter=\"url(#drawingml-effects-").count(), 1);
    assert!(svg.contains("id=\"drawingml-effects-pptx-slide-1-1-text-1-1\""));
    assert!(svg.contains("stdDeviation=\"1.5\""));
    assert!(svg.contains("dx=\"2.12132\" dy=\"2.12132\""));
    assert!(svg.contains("flood-opacity=\"0.43137\""));
    assert!(svg.contains("flood-color=\"#00AAFF\" flood-opacity=\"0.5\""));
    assert!(svg.contains("<feMergeNode in=\"shadow\"/><feMergeNode in=\"glow\"/>"));
    assert!(!svg.contains("<path id=\"pptx-slide-1-1\""));
}

#[test]
fn inherits_pptx_master_layout_and_placeholder_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inherited.pptx");
    let output = temporary.path().join("out");
    make_master_layout_pptx(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Pptx);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#112233"));
    assert!(svg.contains("#445566"));
    assert!(svg.contains("Inherited title"));
    assert!(svg.contains("x=\"149\""));
    assert!(!svg.contains("MASTER SAMPLE"));
    assert!(!svg.contains("LAYOUT SAMPLE"));
}

#[test]
fn inherits_pptx_layout_background_image() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("layout-background.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                br#"<p:sldLayout xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:bg><p:bgPr><a:blipFill><a:blip r:embed="rId2"/><a:stretch><a:fillRect/></a:stretch></a:blipFill></p:bgPr></p:bg><p:spTree/></p:cSld></p:sldLayout>"#.to_vec(),
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/background.png"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                br#"<p:sldMaster xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldMaster>"#.to_vec(),
            ),
            ("ppt/media/background.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"background\""));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("width=\"720\" height=\"540\""));
}

#[test]
fn resolves_pptx_effect_refs_from_each_slide_master_theme() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("multi-theme-effects.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Theme shadow"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></p:spPr><p:style><a:effectRef idx="1"><a:schemeClr val="accent1"/></a:effectRef></p:style></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="No theme effect"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="1905000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></p:spPr><p:style><a:effectRef idx="0"><a:schemeClr val="accent1"/></a:effectRef></p:style></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Explicitly cleared effect"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="3683000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst/></p:spPr><p:style><a:effectRef idx="1"><a:schemeClr val="accent1"/></a:effectRef></p:style></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/slide2.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="1" name="Theme glow"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="635000"/></a:xfrm><a:prstGeom prst="line"/><a:ln w="25400"><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:ln></p:spPr><p:style><a:effectRef idx="2"><a:schemeClr val="accent2"/></a:effectRef></p:style></p:cxnSp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/_rels/slide2.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout2.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                r#"<p:sldLayout xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout2.xml",
                r#"<p:sldLayout xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout2.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster2.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                r#"<p:sldMaster xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldMaster>"#,
            ),
            (
                "ppt/slideMasters/slideMaster2.xml",
                r#"<p:sldMaster xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldMaster>"#,
            ),
            (
                "ppt/slideMasters/_rels/slideMaster1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/_rels/slideMaster2.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme2.xml"/></Relationships>"#,
            ),
            (
                "ppt/theme/theme1.xml",
                r#"<a:theme xmlns:a="a"><a:themeElements><a:fmtScheme><a:effectStyleLst><a:effectStyle><a:effectLst><a:outerShdw blurRad="25400" dist="12700" dir="0" rotWithShape="0"><a:srgbClr val="AA0000"><a:alpha val="50000"/></a:srgbClr></a:outerShdw></a:effectLst></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst></a:fmtScheme></a:themeElements></a:theme>"#,
            ),
            (
                "ppt/theme/theme2.xml",
                r#"<a:theme xmlns:a="a"><a:themeElements><a:fmtScheme><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst><a:glow rad="38100"><a:srgbClr val="0000AA"><a:alpha val="60000"/></a:srgbClr></a:glow></a:effectLst></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst></a:fmtScheme></a:themeElements></a:theme>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(first.matches("filter=\"url(#outer-shadow-").count(), 1);
    assert!(first.contains("id=\"outer-shadow-pptx-slide-1-1\""));
    assert!(first.contains("flood-color=\"#AA0000\" flood-opacity=\"0.5\""));
    assert!(!first.contains("outer-shadow-pptx-slide-1-2"));
    assert!(!first.contains("outer-shadow-pptx-slide-1-3"));
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert_eq!(second.matches("filter=\"url(#glow-").count(), 1);
    assert!(second.contains("id=\"glow-pptx-slide-2-1\""));
    assert!(second.contains("flood-color=\"#0000AA\" flood-opacity=\"0.6\""));
}

#[test]
fn recovers_pptx_placeholder_geometry_and_ignores_empty_zero_size_artifacts() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("placeholder-fallbacks.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Inherited title"/><p:cNvSpPr/><p:nvPr><p:ph type="title" idx="99"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>Inherited title text</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Footer fallback"/><p:cNvSpPr/><p:nvPr><p:ph type="ftr" idx="77"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>Footer text</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Slide number fallback"/><p:cNvSpPr/><p:nvPr><p:ph type="sldNum" idx="78"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:fld id="1" type="slidenum"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="4" name="Empty compatibility line"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="10" y="20"/><a:ext cx="0" cy="100"/></a:xfrm><a:prstGeom prst="line"/><a:noFill/><a:ln w="0"><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="5" name="Malformed body"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="500"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>Missing geometry</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                r#"<p:sldLayout xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Title layout"/><p:cNvSpPr/><p:nvPr><p:ph type="title" idx="5"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp></p:spTree></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                r#"<p:sldMaster xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldMaster>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("Malformed body"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Inherited title\""));
    assert!(svg.contains("Inherited title text"));
    assert!(svg.contains("data-source-id=\"Footer fallback\""));
    assert!(svg.contains("Footer text"));
    assert!(svg.contains("data-source-id=\"Slide number fallback\""));
    assert!(svg.contains(">1</tspan>"));
    assert!(!svg.contains("Empty compatibility line"));
    assert!(!svg.contains("Missing geometry"));
}

#[test]
fn converts_pptx_gradient_and_arrow_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("gradient-arrow.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Gradient triangle"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="1270000"/></a:xfrm><a:prstGeom prst="triangle"/><a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="FF0000"/></a:gs><a:gs pos="100000"><a:srgbClr val="0000FF"/></a:gs></a:gsLst><a:lin ang="0"/></a:gradFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Arrow"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="1905000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="rightArrow"/><a:solidFill><a:srgbClr val="00AA44"/></a:solidFill></p:spPr></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<linearGradient"));
    assert!(svg.contains("#FF0000"));
    assert!(svg.contains("#0000FF"));
    assert!(svg.contains("data-source-id=\"Arrow\""));
    assert!(svg.contains("#00AA44"));
}

#[test]
fn converts_pptx_svg_blip_and_common_presets() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("svg-blip-presets.pptx");
    let output = temporary.path().join("out");
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r" xmlns:asvg="asvg"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="1" name="SVG logo"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip><a:extLst><a:ext><asvg:svgBlip r:embed="rId2"/></a:ext></a:extLst></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="2" name="Straight connector"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="127000" y="1016000"/><a:ext cx="1270000" cy="254000"/></a:xfrm><a:prstGeom prst="straightConnector1"/><a:ln><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:ln></p:spPr></p:cxnSp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Input output"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="1905000" y="1016000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="flowChartInputOutput"/><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill></p:spPr></p:sp></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/logo.svg"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/media/logo.svg",
                br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="#336699"/></svg>"##.to_vec(),
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/svg+xml;base64,"));
    assert!(svg.contains("data-source-id=\"SVG logo\""));
    assert!(svg.contains("data-source-id=\"Straight connector\""));
    assert!(svg.contains("d=\"M 10 80 L 110 100\""));
    assert!(svg.contains("data-source-id=\"Input output\""));
    assert!(svg.contains("d=\"M 170 80 L 250 80 L 230 130 L 150 130 Z\""));
}

#[test]
fn selects_svg_choice_without_rendering_alternate_content_fallback() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("hybrid-svg.pptx");
    let output = temporary.path().join("out");
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r" xmlns:mc="mc" xmlns:asvg="asvg"><p:cSld><p:spTree><mc:AlternateContent><mc:Choice Requires="asvg"><p:pic><p:nvPicPr><p:cNvPr id="2" name="Hybrid SVG"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"><a:extLst><a:ext><asvg:svgBlip r:embed="rId1"/></a:ext></a:extLst></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></mc:Choice><mc:Fallback><p:pic><p:nvPicPr><p:cNvPr id="2" name="Hybrid SVG"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></mc:Fallback></mc:AlternateContent></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.svg"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/media/image1.svg",
                br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="#336699"/></svg>"##.to_vec(),
            ),
            (
                "ppt/media/image1.png",
                base64::engine::general_purpose::STANDARD
                    .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
                    .unwrap(),
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"image\"").count(), 1);
    assert_eq!(svg.matches("data:image/svg+xml;base64,").count(), 1);
    assert!(!svg.contains("data:image/png;base64,"));
}

#[test]
fn warns_and_renders_placeholder_for_unsupported_emf_picture() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("emf-picture.pptx");
    let output = temporary.path().join("out");
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="2" name="Legacy diagram"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId1"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.emf"/></Relationships>"#.to_vec(),
            ),
            ("ppt/media/image1.emf", b"not-a-renderable-emf".to_vec()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("uses EMF"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("EMF preview unavailable"));
    assert!(svg.contains("unsupported-vector-image-placeholder"));
    assert!(!svg.contains("data:image/x-emf"));
}

#[test]
fn converts_pptx_src_rect_crop_with_rotation_group_and_negative_edges() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("picture-crops.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="1" name="Rotated crop"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId1"/><a:srcRect l="25000" t="10000" r="25000" b="10000"/></p:blipFill><p:spPr><a:xfrm rot="1800000"><a:off x="127000" y="254000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:effectLst><a:outerShdw blurRad="12700" dist="12700" dir="0"><a:srgbClr val="000000"><a:alpha val="40000"/></a:srgbClr></a:outerShdw></a:effectLst></p:spPr></p:pic><p:grpSp><p:nvGrpSpPr/><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2540000" cy="1270000"/><a:chOff x="0" y="0"/><a:chExt cx="1270000" cy="1270000"/></a:xfrm></p:grpSpPr><p:pic><p:nvPicPr><p:cNvPr id="2" name="Grouped negative crop"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/><a:srcRect l="-25000" r="25000"/></p:blipFill><p:spPr><a:xfrm><a:off x="254000" y="381000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:grpSp><p:pic><p:nvPicPr><p:cNvPr id="3" name="Invalid crop"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId3"/><a:srcRect l="90000" r="20000"/></p:blipFill><p:spPr><a:xfrm><a:off x="3810000" y="254000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm></p:spPr></p:pic></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("ppt/media/image1.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("Invalid crop crop was ignored"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("x=\"-40\" y=\"10\" width=\"200\" height=\"100\""));
    assert!(svg.contains("<g id=\"pptx-slide-1-1\""));
    assert!(svg.contains("id=\"pptx-slide-1-1-source\""));
    assert!(svg.contains("filter=\"url(#outer-shadow-pptx-slide-1-1)\""));
    assert!(svg.contains("d=\"M 10 20 H 110 V 100 H 10 Z\""));
    assert!(svg.contains("x=\"45\" y=\"30\" width=\"100\" height=\"50\""));
    assert!(svg.contains("d=\"M 20 30 H 120 V 80 H 20 Z\""));
    assert!(svg.contains("x=\"300\" y=\"20\" width=\"100\" height=\"80\""));
    assert_eq!(svg.matches("<clipPath id=\"pptx-picture-crop-").count(), 2);
    assert!(svg.contains("transform=\"matrix(2 0 0 1 0 0)\""));
}

#[test]
fn converts_pptx_image_color_effects_and_composes_crop_shadow() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("image-effects.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="1" name="Duotone crop shadow"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId1"><a:duotone><a:schemeClr val="accent1"><a:shade val="50000"/></a:schemeClr><a:prstClr val="white"/></a:duotone></a:blip><a:srcRect l="10000" r="10000"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:effectLst><a:outerShdw blurRad="12700" dist="12700" dir="0"><a:srgbClr val="000000"><a:alpha val="40000"/></a:srgbClr></a:outerShdw></a:effectLst></p:spPr></p:pic><p:pic><p:nvPicPr><p:cNvPr id="2" name="Grayscale"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"><a:grayscl/></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="1905000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic><p:pic><p:nvPicPr><p:cNvPr id="3" name="Luminance"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId3"><a:lum bright="12000" contrast="20000"/></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="3683000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic><p:pic><p:nvPicPr><p:cNvPr id="4" name="Transparent white"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId4"><a:clrChange><a:clrFrom><a:srgbClr val="FFFFFF"/></a:clrFrom><a:clrTo><a:srgbClr val="FFFFFF"><a:alpha val="0"/></a:srgbClr></a:clrTo></a:clrChange></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="5461000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("ppt/media/image1.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"drawingml-effects-pptx-slide-1-1\""));
    assert!(svg.contains("filter=\"url(#drawingml-effects-pptx-slide-1-1)\""));
    assert!(svg.contains("feGaussianBlur in=\"image-effect-1\""));
    assert!(svg.contains("id=\"pptx-slide-1-1-source\""));
    assert!(svg.contains("id=\"image-effects-pptx-slide-1-2\""));
    assert!(svg.contains("id=\"image-effects-pptx-slide-1-3\""));
    assert!(svg.contains("slope=\"1.1\" intercept=\"0.01\""));
    assert!(svg.contains("id=\"image-effects-pptx-slide-1-4\""));
    assert!(svg.contains("mode=\"difference\""));
    assert!(svg.contains("flood-color=\"#FFFFFF\" flood-opacity=\"0\""));
    assert!(svg.contains("values=\"0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 255 255 255 0 0\""));
}

#[test]
fn converts_pptx_density_hatch_and_diamond_pattern_fills() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("patterns.pptx");
    let output = temporary.path().join("out");
    let presets = [
        "pct5", "pct90", "narHorz", "narVert", "wdUpDiag", "wdDnDiag", "dkUpDiag", "openDmnd",
    ];
    let mut shapes = String::new();
    for (index, preset) in presets.iter().enumerate() {
        let x = 127_000 + index as i64 * 762_000;
        let foreground = if index == 0 {
            r#"<a:srgbClr val="FF0000"><a:tint val="50000"/><a:alpha val="50000"/></a:srgbClr>"#
        } else if index == 1 {
            r#"<a:sysClr val="windowText" lastClr="00AA00"/>"#
        } else {
            r#"<a:schemeClr val="accent1"/>"#
        };
        let shape = format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="{}" name="Pattern {}"/></p:nvSpPr><p:spPr><a:xfrm{}><a:off x="{}" y="254000"/><a:ext cx="635000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:pattFill prst="{}"><a:fgClr>{}</a:fgClr><a:bgClr><a:schemeClr val="bg1"/></a:bgClr></a:pattFill></p:spPr></p:sp>"#,
            index + 1,
            preset,
            if index == 3 { " rot=\"1800000\"" } else { "" },
            x,
            preset,
            foreground,
        );
        if index == 2 {
            shapes.push_str(
                r#"<p:grpSp><p:nvGrpSpPr/><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="12700000" cy="1270000"/><a:chOff x="0" y="0"/><a:chExt cx="6350000" cy="1270000"/></a:xfrm></p:grpSpPr>"#,
            );
            shapes.push_str(&shape);
            shapes.push_str("</p:grpSp>");
        } else {
            shapes.push_str(&shape);
        }
    }
    let slide = format!(
        r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:sld>"#
    );
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            ("ppt/slides/slide1.xml", &slide),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(
        svg.matches("<pattern id=\"pptx-pattern-slide-1-").count(),
        8
    );
    assert_eq!(svg.matches("fill=\"url(#pptx-pattern-slide-1-").count(), 8);
    assert!(svg.contains("fill=\"#FF8080\" fill-opacity=\"0.5\""));
    assert!(svg.contains("fill=\"#00AA00\" fill-opacity=\"1\""));
    assert!(svg.contains("data-content-kind=\"drawingml-pattern-cell\""));
    assert!(svg.contains("data-content-kind=\"drawingml-pattern-line\""));
    assert!(svg.contains("stroke-width=\"3\""));
    assert!(svg.contains("M 0 6 L 6 0 L 12 6 L 6 12 Z"));
    assert!(svg.contains("transform=\"matrix(2 0 0 1 0 0)\""));
}

#[test]
fn converts_high_frequency_pptx_preset_geometries() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("preset-geometries.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:grpSp><p:nvGrpSpPr/><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="18288000" cy="6858000"/><a:chOff x="0" y="0"/><a:chExt cx="9144000" cy="6858000"/></a:xfrm></p:grpSpPr><p:sp><p:nvSpPr><p:cNvPr id="1" name="Right brace"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="381000" cy="1270000"/></a:xfrm><a:prstGeom prst="rightBrace"><a:avLst><a:gd name="adj1" fmla="val 25000"/><a:gd name="adj2" fmla="val 40000"/></a:avLst></a:prstGeom><a:noFill/><a:ln><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></p:spPr></p:sp></p:grpSp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Left brace"/></p:nvSpPr><p:spPr><a:xfrm rot="1800000"><a:off x="762000" y="127000"/><a:ext cx="381000" cy="1270000"/></a:xfrm><a:prstGeom prst="leftBrace"><a:avLst><a:gd name="adj1" fmla="val 8333"/><a:gd name="adj2" fmla="val 50000"/></a:avLst></a:prstGeom><a:noFill/><a:ln><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Arc"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="1397000" y="127000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:prstGeom prst="arc"><a:avLst><a:gd name="adj1" fmla="val 16200000"/><a:gd name="adj2" fmla="val 0"/></a:avLst></a:prstGeom><a:noFill/><a:ln><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></a:ln></p:spPr></p:sp><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="4" name="Bent connector"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="2921000" y="127000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:prstGeom prst="bentConnector3"><a:avLst><a:gd name="adj1" fmla="val 25000"/></a:avLst></a:prstGeom></p:spPr></p:cxnSp><p:sp><p:nvSpPr><p:cNvPr id="5" name="Manual input"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="4445000" y="127000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:prstGeom prst="flowChartManualInput"/><a:solidFill><a:srgbClr val="FFFF00"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="6" name="Cylinder"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="5969000" y="127000"/><a:ext cx="1016000" cy="1016000"/></a:xfrm><a:prstGeom prst="can"><a:avLst><a:gd name="adj" fmla="val 25000"/></a:avLst></a:prstGeom><a:solidFill><a:srgbClr val="00AAFF"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="7" name="Decision"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="7366000" y="127000"/><a:ext cx="1016000" cy="1016000"/></a:xfrm><a:prstGeom prst="flowChartDecision"/><a:solidFill><a:srgbClr val="00AA00"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="8" name="Extract"/></p:nvSpPr><p:spPr><a:xfrm rot="10800000"><a:off x="127000" y="1905000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:prstGeom prst="flowChartExtract"/><a:solidFill><a:srgbClr val="FFAA00"/></a:solidFill></p:spPr></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"vector\"").count(), 8);
    assert!(svg.contains("data-source-id=\"Right brace\""));
    assert!(svg.contains("data-source-id=\"Left brace\""));
    assert!(svg.contains("M 160 10 A 50 40 0 0 1 210 50"));
    assert!(svg.contains("M 230 10 L 255 10 L 255 90 L 330 90"));
    assert!(svg.contains("M 372 10 L 450 10 L 450 90 L 350 90 Z"));
    assert!(svg.contains("data-source-id=\"Cylinder\""));
    assert!(svg.contains("M 620 10 L 660 50 L 620 90 L 580 50 Z"));
    assert!(svg.contains("M 60 150 L 110 230 L 10 230 Z"));
    assert!(svg.contains("transform=\"matrix(2 0 0 1 0 0)\""));
}

#[test]
fn converts_second_batch_pptx_preset_geometries() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("preset-geometries-2.pptx");
    let output = temporary.path().join("out");
    let presets = [
        "moon",
        "bentArrow",
        "flowChartDelay",
        "flowChartConnector",
        "flowChartCollate",
        "flowChartProcess",
        "cube",
        "leftBracket",
        "uturnArrow",
        "donut",
        "curvedLeftArrow",
        "curvedUpArrow",
        "curvedDownArrow",
        "circularArrow",
        "upDownArrow",
        "bracketPair",
        "snip2SameRect",
        "homePlate",
        "bentUpArrow",
        "corner",
        "rightBracket",
        "leftRightArrow",
        "wave",
        "actionButtonForwardNext",
        "mathPlus",
        "irregularSeal1",
        "irregularSeal2",
        "wedgeRoundRectCallout",
        "wedgeEllipseCallout",
        "borderCallout1",
        "borderCallout2",
    ];
    let mut shapes = String::new();
    for (index, preset) in presets.iter().enumerate() {
        let column = index % 10;
        let row = index / 10;
        let x = 127_000 + column as i64 * 889_000;
        let y = 127_000 + row as i64 * 1_270_000;
        let adjustments = match *preset {
            "moon" => r#"<a:gd name="adj" fmla="val 37454"/>"#,
            "cube" => r#"<a:gd name="adj" fmla="val 81676"/>"#,
            "snip2SameRect" => {
                r#"<a:gd name="adj1" fmla="val 22750"/><a:gd name="adj2" fmla="val 29251"/>"#
            }
            "corner" => {
                r#"<a:gd name="adj1" fmla="val 30000"/><a:gd name="adj2" fmla="val 40000"/>"#
            }
            "wave" => r#"<a:gd name="adj1" fmla="val 5332"/><a:gd name="adj2" fmla="val 0"/>"#,
            _ => "",
        };
        let open_shape = matches!(
            *preset,
            "moon" | "leftBracket" | "rightBracket" | "bracketPair"
        );
        shapes.push_str(&format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="{}" name="Preset {}"/></p:nvSpPr><p:spPr><a:xfrm{}><a:off x="{}" y="{}"/><a:ext cx="762000" cy="762000"/></a:xfrm><a:prstGeom prst="{}"><a:avLst>{}</a:avLst></a:prstGeom>{}<a:ln><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></p:spPr></p:sp>"#,
            index + 1,
            preset,
            if index % 3 == 0 { " rot=\"1800000\"" } else { "" },
            x,
            y,
            preset,
            adjustments,
            if open_shape {
                "<a:noFill/>"
            } else {
                "<a:solidFill><a:srgbClr val=\"66CCFF\"/></a:solidFill>"
            },
        ));
    }
    let slide = format!(
        r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:sld>"#
    );
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            ("ppt/slides/slide1.xml", &slide),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(
        svg.matches("data-content-kind=\"vector\"").count(),
        presets.len()
    );
    for preset in presets {
        assert!(
            svg.contains(&format!("data-source-id=\"Preset {preset}\"")),
            "missing {preset}"
        );
    }
    assert!(svg.contains("A 30 30 0 1 0"));
    assert!(svg.contains("data-source-id=\"Preset cube\""));
    assert!(svg.contains("data-source-id=\"Preset donut\""));
    assert!(svg.contains("data-source-id=\"Preset wave\""));
}

#[test]
fn converts_pptx_adjustable_wedge_rect_callout() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("wedge-callout.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Adjusted callout"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="1524000" cy="762000"/></a:xfrm><a:prstGeom prst="wedgeRectCallout"><a:avLst><a:gd name="adj1" fmla="val 0"/><a:gd name="adj2" fmla="val 100000"/></a:avLst></a:prstGeom><a:solidFill><a:srgbClr val="FFFF00"/></a:solidFill></p:spPr><p:style><a:lnRef idx="2"><a:schemeClr val="accent1"><a:shade val="50000"/></a:schemeClr></a:lnRef><a:fillRef idx="1"><a:schemeClr val="accent1"/></a:fillRef></p:style></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Adjusted callout\""));
    assert!(svg.contains("data-content-kind=\"vector\""));
    assert!(svg.contains("L 70 110"));
    assert!(svg.contains("fill=\"#FFFF00\""));
}

#[test]
fn converts_pptx_custom_geometry_to_editable_svg_path() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("custom-geometry.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Custom wave"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="1270000"/></a:xfrm><a:custGeom><a:avLst/><a:gdLst><a:gd name="xMid" fmla="*/ w 1 2"/></a:gdLst><a:ahLst/><a:cxnLst/><a:rect l="l" t="t" r="r" b="b"/><a:pathLst><a:path w="21600" h="21600"><a:moveTo><a:pt x="0" y="21600"/></a:moveTo><a:lnTo><a:pt x="xMid" y="0"/></a:lnTo><a:cubicBezTo><a:pt x="16200" y="0"/><a:pt x="21600" y="5400"/><a:pt x="21600" y="10800"/></a:cubicBezTo><a:quadBezTo><a:pt x="10800" y="21600"/><a:pt x="0" y="21600"/></a:quadBezTo><a:close/></a:path><a:path w="21600" h="21600"><a:moveTo><a:pt x="10800" y="0"/></a:moveTo><a:arcTo wR="10800" hR="10800" stAng="16200000" swAng="21600000"/><a:close/></a:path></a:pathLst></a:custGeom><a:solidFill><a:srgbClr val="336699"/></a:solidFill></p:spPr></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Custom wave\""));
    assert!(svg.contains("M 10 110 L 60 10 C 85 10 110 35 110 60 Q 60 110 10 110 Z"));
    assert!(svg.matches(" A 50 50").count() >= 2);
}

#[test]
fn converts_pptx_cached_smartart_drawing() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("smartart.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="2540000" cy="1524000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:dgm="dgm" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Process diagram"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="1270000" cy="635000"/></p:xfrm><a:graphic><a:graphicData><dgm:relIds r:dm="rId2"/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData" Target="../diagrams/data1.xml"/><Relationship Id="rId6" Type="http://schemas.microsoft.com/office/2007/relationships/diagramDrawing" Target="../diagrams/drawing1.xml"/></Relationships>"#,
            ),
            (
                "ppt/diagrams/data1.xml",
                r#"<dgm:dataModel xmlns:dgm="dgm" xmlns:dsp="dsp"><dgm:extLst><dgm:ext><dsp:dataModelExt relId="rId6"/></dgm:ext></dgm:extLst></dgm:dataModel>"#,
            ),
            (
                "ppt/diagrams/drawing1.xml",
                r#"<dsp:drawing xmlns:dsp="dsp" xmlns:a="a"><dsp:spTree><dsp:sp><dsp:nvSpPr><dsp:cNvPr id="1" name="Node"/></dsp:nvSpPr><dsp:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="508000" cy="508000"/></a:xfrm><a:prstGeom prst="ellipse"/><a:solidFill><a:schemeClr val="accent4"><a:alpha val="50000"/></a:schemeClr></a:solidFill><a:ln><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></dsp:spPr><dsp:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="ctr"/><a:r><a:rPr sz="1100"/><a:t>Smart node</a:t></a:r></a:p></dsp:txBody><dsp:txXfrm><a:off x="190500" y="279400"/><a:ext cx="381000" cy="127000"/></dsp:txXfrm></dsp:sp></dsp:spTree></dsp:drawing>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"smartart\""));
    assert!(svg.contains("data-semantic-role=\"diagram\""));
    assert!(svg.contains("fill=\"#FFC000\" fill-opacity=\"0.5\""));
    assert!(svg.contains("stroke=\"#FFFFFF\""));
    assert!(svg.contains(">Smart</tspan>"));
    assert!(svg.contains(">node</tspan>"));
}

#[test]
fn converts_pptx_native_table_cells_text_and_merges() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("table.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Native table"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr firstRow="1" bandRow="1"/><a:tblGrid><a:gridCol w="1016000"/><a:gridCol w="1524000"/></a:tblGrid><a:tr h="508000"><a:tc><a:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="ctr"/><a:r><a:rPr b="1" sz="1200"><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst><a:outerShdw blurRad="12700" dist="12700" dir="0"><a:srgbClr val="000000"><a:alpha val="40000"/></a:srgbClr></a:outerShdw><a:glow rad="12700"><a:srgbClr val="00AAFF"><a:alpha val="30000"/></a:srgbClr></a:glow></a:effectLst></a:rPr><a:t>Alpha Beta Gamma</a:t></a:r></a:p></a:txBody><a:tcPr><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:tcPr></a:tc><a:tc><a:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="ctr"/><a:r><a:rPr b="1" sz="1200"><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a:rPr><a:t>Head B</a:t></a:r></a:p></a:txBody><a:tcPr><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:tcPr></a:tc></a:tr><a:tr h="762000"><a:tc gridSpan="2"><a:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="l"/><a:r><a:rPr sz="1100"><a:solidFill><a:srgbClr val="223344"/></a:solidFill></a:rPr><a:t>Merged body</a:t></a:r></a:p></a:txBody><a:tcPr><a:gridSpan val="2"/><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill><a:lnL><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:lnL></a:tcPr></a:tc><a:tc hMerge="1"><a:txBody><a:bodyPr/><a:p/></a:txBody><a:tcPr><a:hMerge/></a:tcPr></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"table-cell\"").count(), 3);
    assert_eq!(
        svg.matches("data-content-kind=\"table-cell-text\"").count(),
        4
    );
    assert!(svg.contains("data-source-id=\"Native table!R1C1\""));
    assert!(svg.contains("fill=\"#112233\""));
    assert!(svg.contains("fill=\"#DDEEFF\""));
    assert!(svg.contains("font-weight=\"700\""));
    assert!(svg.contains("Alpha Beta"));
    assert!(svg.contains("Gamma"));
    assert!(svg.contains("Merged body"));
    assert!(svg.contains("d=\"M 10 60 H 210 V 120 H 10 Z\""));
    assert!(svg.contains("clip-path=\"url(#pptx-table-1-1-clip-2-1)\""));
    assert_eq!(svg.matches("filter=\"url(#drawingml-effects-").count(), 2);
    assert!(svg.contains("stdDeviation=\"0.5\""));
    assert!(svg.contains("flood-opacity=\"0.4\""));
    assert!(svg.contains("flood-color=\"#00AAFF\" flood-opacity=\"0.3\""));
}

#[test]
fn recovers_pptx_zero_height_table_rows_from_frame_extent() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("zero-row-table.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Zero row table"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></p:xfrm><a:graphic><a:graphicData><a:tbl><a:tblPr/><a:tblGrid><a:gridCol w="1270000"/><a:gridCol w="1270000"/></a:tblGrid><a:tr h="0"><a:tc><a:txBody><a:p><a:r><a:t>A</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc><a:tc><a:txBody><a:p><a:r><a:t>B</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr><a:tr h="0"><a:tc><a:txBody><a:p><a:r><a:t>C</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc><a:tc><a:txBody><a:p><a:r><a:t>D</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"table-cell\"").count(), 4);
    assert!(svg.contains("d=\"M 10 20 H 110 V 70 H 10 Z\""));
    assert!(svg.contains("d=\"M 10 70 H 110 V 120 H 10 Z\""));
    assert!(svg.contains(">A</tspan>"));
    assert!(svg.contains(">D</tspan>"));
}

#[test]
fn applies_pptx_group_coordinate_transform() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("group-transform.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:grpSp><p:nvGrpSpPr/><p:grpSpPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/><a:chOff x="0" y="0"/><a:chExt cx="1270000" cy="1270000"/></a:xfrm></p:grpSpPr><p:sp><p:nvSpPr><p:cNvPr id="1" name="Grouped rectangle"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="254000" cy="254000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="123456"/></a:solidFill></p:spPr></p:sp></p:grpSp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Grouped rectangle\""));
    assert!(svg.contains("d=\"M 10 10 H 30 V 30 H 10 Z\""));
    assert!(svg.contains("transform=\"matrix(2 0 0 1 10 20)\""));
}

#[test]
fn renders_pptx_embedded_video_as_static_poster() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("video.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r" xmlns:p14="p14"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="1" name="Demo video" descr="Video preview"/><p:cNvPicPr/><p:nvPr><a:videoFile r:link="rId2"/><p14:media r:embed="rId2"/></p:nvPr></p:nvPicPr><p:blipFill><a:blip r:embed="rId1"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/poster.png"/><Relationship Id="rId2" Type="http://schemas.microsoft.com/office/2007/relationships/media" Target="../media/video1.mp4"/></Relationships>"#.to_vec(),
            ),
            ("ppt/media/poster.png", png),
            ("ppt/media/video1.mp4", b"not-decoded-by-static-svg".to_vec()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("rendered as a static poster"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"video-poster\""));
    assert!(svg.contains("data-semantic-role=\"video\""));
    assert!(svg.contains("data-source-id=\"Demo video\""));
    assert!(svg.contains("data:image/png;base64,"));
}

#[test]
fn preserves_pptx_ole_frame_as_static_placeholder_without_preview() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ole.pptx");
    let output = temporary.path().join("out");
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Budget workbook"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></p:xfrm><a:graphic><a:graphicData><p:oleObj r:id="rId2" name="Budget workbook" progId="Excel.Sheet.12"><p:embed/></p:oleObj></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject" Target="../embeddings/workbook1.xlsx"/></Relationships>"#.to_vec(),
            ),
            ("ppt/embeddings/workbook1.xlsx", b"opaque-ole-payload".to_vec()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("rendered as a static placeholder"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"embedded-object-placeholder\""));
    assert!(svg.contains("data-semantic-role=\"embedded-object\""));
    assert!(svg.contains("data-source-id=\"Budget workbook\""));
    assert!(svg.contains("d=\"M 10 20 H 210 V 120 H 10 Z\""));
    assert!(svg.contains(">Budget workbook</tspan>"));
}

#[test]
fn preserves_pptx_ole_static_preview_without_duplicate_placeholder() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ole-preview.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Workbook frame"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></p:xfrm><a:graphic><a:graphicData><p:oleObj r:id="rId2" name="Workbook preview" progId="Excel.Sheet.12"><p:embed/><p:pic><p:nvPicPr><p:cNvPr id="2" name="Workbook poster"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId3"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></a:xfrm></p:spPr></p:pic></p:oleObj></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject" Target="../embeddings/workbook1.xlsx"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/ole-preview.png"/></Relationships>"#.to_vec(),
            ),
            ("ppt/embeddings/workbook1.xlsx", b"opaque-ole-payload".to_vec()),
            ("ppt/media/ole-preview.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("rendered as a static preview"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"embedded-object-preview\""));
    assert!(svg.contains("data-semantic-role=\"embedded-object\""));
    assert!(svg.contains("data-source-id=\"Workbook poster\""));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(!svg.contains("embedded-object-placeholder"));
}

#[test]
fn converts_pptx_cached_line_chart() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("chart.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:c="c" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Trend chart"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="127000"/><a:ext cx="3810000" cy="2540000"/></p:xfrm><a:graphic><a:graphicData><c:chart r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "ppt/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Trend</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:lineChart><c:ser><c:tx><c:v>Series 1</c:v></c:tx><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>3</c:v></c:pt><c:pt idx="2"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">Trend</tspan>"));
    assert!(svg.contains("data-content-kind=\"chart-line\""));
}

#[test]
fn converts_pptx_scatter_as_xy_and_area_as_filled_chart() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("charts.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:c="c" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="XY chart"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="127000"/><a:ext cx="3810000" cy="2540000"/></p:xfrm><a:graphic><a:graphicData><c:chart r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="2" name="Area chart"/></p:nvGraphicFramePr><p:xfrm><a:off x="4200000" y="127000"/><a:ext cx="3810000" cy="2540000"/></p:xfrm><a:graphic><a:graphicData><c:chart r:id="rId3"/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/scatter.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/area.xml"/></Relationships>"#,
            ),
            (
                "ppt/charts/scatter.xml",
                r#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:scatterChart><c:scatterStyle val="marker"/><c:ser><c:tx><c:v>Measurements</c:v></c:tx><c:xVal><c:numRef><c:numCache><c:pt idx="0"><c:v>10</c:v></c:pt><c:pt idx="1"><c:v>20</c:v></c:pt></c:numCache></c:numRef></c:xVal><c:yVal><c:numRef><c:numCache><c:pt idx="0"><c:v>20</c:v></c:pt><c:pt idx="1"><c:v>10</c:v></c:pt></c:numCache></c:numRef></c:yVal></c:ser></c:scatterChart></c:plotArea><c:legend><c:legendPos val="r"/></c:legend></c:chart></c:chartSpace>"#,
            ),
            (
                "ppt/charts/area.xml",
                r#"<c:chartSpace xmlns:c="c"><c:chart><c:plotArea><c:areaChart><c:ser><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Jan</c:v></c:pt><c:pt idx="1"><c:v>Feb</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>5</c:v></c:pt><c:pt idx="1"><c:v>15</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:areaChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(
        svg.matches("data-content-kind=\"chart-scatter-point\"")
            .count(),
        2
    );
    assert!(svg.contains("data-content-kind=\"chart-area\""));
    assert!(svg.contains("fill-opacity=\"0.28\""));
    assert!(svg.contains("data-content-kind=\"chart-legend-label\""));
    assert!(svg.contains(">Measurements</tspan>"));
}

#[test]
fn converts_minimal_xlsx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Hello XLSX</t></is></c><c r="B1"><v>42</v></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Xlsx);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hello XLSX"));
    assert!(svg.contains(">42</tspan>"));
}

#[test]
fn converts_binary_dxf_and_sniffs_its_sentinel() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/binary_line.dxf");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::Dxf);
    let output = temporary.path().join("binary-dxf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dxf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"dxf\""));
    assert!(svg.contains("<path"));

    let extensionless = temporary.path().join("binary-cad-drawing");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Dxf
    );
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("binary-dxf-sniff-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Dxf);
}

#[test]
fn converts_geojson_features_and_sniffs_the_json_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.geojson");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::GeoJson);
    let output = temporary.path().join("geojson-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::GeoJson);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("GeoJSON Map Preview"));
    assert!(svg.contains("geojson:polygon"));
    assert!(svg.contains("geojson:line"));
    assert!(svg.contains("geojson:point"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("altitude"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("closed"))
    );

    let extensionless = temporary.path().join("map-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::GeoJson
    );
    let generic_json = temporary.path().join("map.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::GeoJson
    );
}

#[test]
fn converts_esri_shapefile_polygons_and_validates_sidecars() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_polygon.shp");
    assert_eq!(
        SourceFormat::detect(&input).unwrap(),
        SourceFormat::Shapefile
    );
    let output = temporary.path().join("shapefile-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Shapefile);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Shapefile Map Preview"));
    assert!(svg.contains("data-source-format=\"shapefile\""));
    assert!(svg.contains("data-semantic-role=\"shapefile:polygon\""));
    assert!(svg.contains("1 features, 1 geometries, and 15 positions"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("Web Mercator"))
    );
    assert!(
        !report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains(".prj"))
    );

    let extensionless = temporary.path().join("spatial-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Shapefile
    );
    let extensionless_output = temporary.path().join("extensionless-out");
    let extensionless_report = convert_path(
        &extensionless,
        &extensionless_output,
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        extensionless_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("coordinates are assumed"))
    );

    let bad_crs = temporary.path().join("unsupported.shp");
    fs::copy(&input, &bad_crs).unwrap();
    fs::write(
        temporary.path().join("unsupported.prj"),
        "PROJCS[\"WGS_1984_Web_Mercator_Auxiliary_Sphere\",AUTHORITY[\"EPSG\",\"3857\"]]",
    )
    .unwrap();
    let unsupported_crs_error = convert_path(
        &bad_crs,
        temporary.path().join("unsupported-crs-out"),
        &ConvertOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        unsupported_crs_error.contains("not a recognized degree-based WGS 84 geographic CRS"),
        "{unsupported_crs_error}"
    );

    let bad_index = temporary.path().join("bad-index.shp");
    fs::copy(&input, &bad_index).unwrap();
    let index_path = temporary.path().join("bad-index.shx");
    let source_index =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_polygon.shx");
    let mut shx = fs::read(source_index).unwrap();
    shx[100..104].copy_from_slice(&51u32.to_be_bytes());
    fs::write(index_path, shx).unwrap();
    assert!(
        convert_path(
            &bad_index,
            temporary.path().join("bad-index-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("SHX entry does not match")
    );

    let dbf_shp = temporary.path().join("sample_polygon.shp");
    fs::copy(&input, &dbf_shp).unwrap();
    fs::write(
        temporary.path().join("sample_polygon.dbf"),
        b"dbf placeholder",
    )
    .unwrap();
    let dbf_report = convert_path(
        &dbf_shp,
        temporary.path().join("dbf-warning-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        dbf_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("DBF attribute fields"))
    );
}

#[test]
fn converts_read_only_geopackage_vector_layers_with_supported_crs() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_features.gpkg");
    let before = fs::read(&input).unwrap();
    assert_eq!(
        SourceFormat::detect(&input).unwrap(),
        SourceFormat::Geopackage
    );
    let output = temporary.path().join("geopackage-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Geopackage);
    assert_eq!(report.page_count, 3);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("EPSG:3857"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("Z and M"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("attributes"))
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("Private layer title"))
    );
    assert_eq!(
        fs::read(&input).unwrap(),
        before,
        "conversion modified the source database"
    );

    let area = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(area.contains("GeoPackage Feature Layer 1"));
    assert!(area.contains("data-source-format=\"geopackage\""));
    assert!(area.contains("data-semantic-role=\"geopackage:polygon\""));
    assert!(!area.contains("private record"));
    assert!(!area.contains("Private layer title"));

    let elevated = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(elevated.contains("geopackage:point"));
    let projected = fs::read_to_string(output.join("page-0003.svg")).unwrap();
    assert!(projected.contains("geopackage:point"));

    let extensionless = temporary.path().join("spatial-database");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Geopackage
    );

    let limited = ConvertOptions {
        max_pages: 2,
        ..ConvertOptions::default()
    };
    let page_limit = convert_path(
        &input,
        temporary.path().join("geopackage-page-limit"),
        &limited,
    )
    .unwrap_err()
    .to_string();
    assert!(page_limit.contains("page limit"), "{page_limit}");

    let unsupported_crs = temporary.path().join("unsupported-crs.gpkg");
    fs::copy(&input, &unsupported_crs).unwrap();
    let database = rusqlite::Connection::open(&unsupported_crs).unwrap();
    database
        .execute(
            "UPDATE gpkg_spatial_ref_sys SET organization_coordsys_id=32654 WHERE srs_id=4326",
            [],
        )
        .unwrap();
    drop(database);
    let unsupported = convert_path(
        &unsupported_crs,
        temporary.path().join("unsupported-crs-out"),
        &ConvertOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(unsupported.contains("EPSG:32654"), "{unsupported}");
}

#[test]
fn converts_geopackage_raster_tiles_and_tile_only_packages() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_tile_mixed.gpkg");
    assert_eq!(
        SourceFormat::detect(&input).unwrap(),
        SourceFormat::Geopackage
    );
    let output = temporary.path().join("geopackage-mixed-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Geopackage);
    assert_eq!(report.page_count, 2);
    assert!(
        report.pages[1]
            .warnings
            .iter()
            .any(|warning| warning.contains("re-encoded as PNG"))
    );
    let tile_svg = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(tile_svg.contains("GeoPackage Raster Tile Layer 1"));
    assert!(tile_svg.contains("data-semantic-role=\"geopackage:tile\""));
    assert!(tile_svg.contains("data:image/png;base64,"));

    let tile_only = temporary.path().join("tiles-only.gpkg");
    fs::copy(&input, &tile_only).unwrap();
    let database = rusqlite::Connection::open(&tile_only).unwrap();
    database
        .execute("DELETE FROM gpkg_contents WHERE data_type='features'", [])
        .unwrap();
    database
        .execute("DELETE FROM gpkg_geometry_columns", [])
        .unwrap();
    database.execute("DROP TABLE districts", []).unwrap();
    drop(database);
    let tile_only_report = convert_path(
        &tile_only,
        temporary.path().join("tiles-only-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(tile_only_report.page_count, 1);
    assert!(tile_only_report.pages[0].svg.ends_with("page-0001.svg"));
    let tile_only_svg =
        fs::read_to_string(temporary.path().join("tiles-only-out/page-0001.svg")).unwrap();
    assert!(tile_only_svg.contains("data-semantic-role=\"geopackage:tile\""));

    let unsupported_tiles = temporary.path().join("unsupported-tile-crs.gpkg");
    fs::copy(&input, &unsupported_tiles).unwrap();
    let database = rusqlite::Connection::open(&unsupported_tiles).unwrap();
    database
        .execute(
            "UPDATE gpkg_contents SET srs_id=4326 WHERE data_type='tiles'",
            [],
        )
        .unwrap();
    database
        .execute("UPDATE gpkg_tile_matrix_set SET srs_id=4326", [])
        .unwrap();
    drop(database);
    let unsupported_report = convert_path(
        &unsupported_tiles,
        temporary.path().join("unsupported-tile-crs-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(unsupported_report.page_count, 1);
    assert!(
        unsupported_report
            .warnings
            .iter()
            .any(|warning| warning.contains("non-EPSG:3857"))
    );

    let corrupt_tiles = temporary.path().join("corrupt-tiles.gpkg");
    fs::copy(&input, &corrupt_tiles).unwrap();
    let database = rusqlite::Connection::open(&corrupt_tiles).unwrap();
    database
        .execute(
            "UPDATE basemap SET tile_data=?1",
            [b"not an image".as_slice()],
        )
        .unwrap();
    drop(database);
    let corrupt_report = convert_path(
        &corrupt_tiles,
        temporary.path().join("corrupt-tiles-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(corrupt_report.page_count, 1);
    assert!(
        corrupt_report
            .warnings
            .iter()
            .any(|warning| warning.contains("corrupt or unsupported GeoPackage tile image"))
    );
}

#[test]
fn converts_rfc7464_generic_json_text_sequences() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsons");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::JsonSeq);
    let output = temporary.path().join("json-sequence-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::JsonSeq);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("4 JSON sequence record(s)"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"jsonseq\""));
    assert!(svg.contains("JSON sequence record 2"));
    assert!(svg.contains("ingest"));
    assert!(svg.contains("validated"));
    assert!(svg.contains("42"));

    let extensionless = temporary.path().join("json-stream");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::JsonSeq
    );
    let generic_json = temporary.path().join("json-stream.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::JsonSeq
    );

    let invalid_record = temporary.path().join("recoverable.jsons");
    fs::write(&invalid_record, b"\x1e{\"good\":true}\n\x1e{invalid}\n").unwrap();
    let recovered = convert_path(
        &invalid_record,
        temporary.path().join("json-sequence-recovered"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        recovered
            .warnings
            .iter()
            .any(|warning| warning.contains("invalid JSON sequence record(s) were skipped"))
    );

    let newline = temporary.path().join("compat.jsonl");
    fs::write(&newline, b"{\"kind\":\"alpha\"}\n{\"kind\":\"beta\"}\n").unwrap();
    let newline_report = convert_path(
        &newline,
        temporary.path().join("json-lines-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        newline_report
            .warnings
            .iter()
            .any(|warning| warning.contains("compatibility mode"))
    );
}

#[test]
fn converts_quantized_topojson_with_shared_and_reversed_arcs() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.topojson");
    assert_eq!(
        SourceFormat::detect(&input).unwrap(),
        SourceFormat::TopoJson
    );
    let output = temporary.path().join("topojson-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::TopoJson);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("TopoJSON Map Preview"));
    assert!(svg.contains("data-source-format=\"topojson\""));
    assert!(svg.contains("data-semantic-role=\"topojson:polygon\""));
    assert!(svg.contains("data-semantic-role=\"topojson:point\""));
    assert!(!svg.contains("private west"));
    assert!(!svg.contains("private station"));

    let extensionless = temporary.path().join("topology");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::TopoJson
    );
    let generic_json = temporary.path().join("topology.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::TopoJson
    );
}

#[test]
fn converts_rfc8142_geojson_text_sequences_and_line_delimited_compatibility() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.geojsons");
    assert_eq!(
        SourceFormat::detect(&input).unwrap(),
        SourceFormat::GeoJsonSeq
    );
    let output = temporary.path().join("geojson-sequence-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::GeoJsonSeq);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("3 GeoJSON sequence record(s)"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("GeoJSON Text Sequence Map Preview"));
    assert!(svg.contains("data-source-format=\"geojsonseq\""));
    assert!(svg.contains("data-semantic-role=\"geojsonseq:point\""));
    assert!(svg.contains("data-semantic-role=\"geojsonseq:line\""));
    assert!(svg.contains("data-semantic-role=\"geojsonseq:polygon\""));
    assert!(!svg.contains("private-sample-point-id"));
    assert!(!svg.contains("Private feature attribute"));

    let extensionless = temporary.path().join("geojson-stream");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::GeoJsonSeq
    );
    let json_extension = temporary.path().join("geojson-stream.json");
    fs::copy(&input, &json_extension).unwrap();
    assert_eq!(
        SourceFormat::detect(&json_extension).unwrap(),
        SourceFormat::GeoJsonSeq
    );

    let newline_delimited = temporary.path().join("compatibility.geojsonl");
    fs::write(
        &newline_delimited,
        b"{\"type\":\"Feature\",\"geometry\":{\"type\":\"Point\",\"coordinates\":[139,35]},\"properties\":null}\n{\"type\":\"LineString\",\"coordinates\":[[139,35],[140,36]]}\n",
    )
    .unwrap();
    assert_eq!(
        SourceFormat::detect(&newline_delimited).unwrap(),
        SourceFormat::GeoJsonSeq
    );
    let newline_report = convert_path(
        &newline_delimited,
        temporary.path().join("geojsonl-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        newline_report
            .warnings
            .iter()
            .any(|warning| warning.contains("compatibility mode"))
    );
}

#[test]
fn converts_georss_simple_feed_geometries_without_fetching_feed_links() {
    let temporary = TempDir::new().unwrap();
    let feed = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.georss");
    assert_eq!(SourceFormat::detect(&feed).unwrap(), SourceFormat::GeoRss);
    let output = temporary.path().join("georss-out");
    let report = convert_path(&feed, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::GeoRss);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("GeoRSS Feed Map Preview"));
    assert!(svg.contains("georss:polygon"));
    assert!(svg.contains("georss:line"));
    assert!(svg.contains("georss:point"));
    assert!(svg.contains("6 features, 6 geometries, and 19 positions"));
    assert!(!svg.contains("example.invalid"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("not fetched"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("unsupported geometries"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("extra coordinate ordinates"))
    );

    let extensionless = temporary.path().join("geo-feed");
    fs::copy(&feed, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::GeoRss
    );
    let generic_xml = temporary.path().join("geo-feed.xml");
    fs::copy(&feed, &generic_xml).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_xml).unwrap(),
        SourceFormat::GeoRss
    );

    for (filename, source_xml) in [
        (
            "map.atom",
            "<atom:feed xmlns:atom=\"http://www.w3.org/2005/Atom\" xmlns:geo=\"http://www.georss.org/georss\"><atom:entry><geo:point>37.4 -122.1</geo:point></atom:entry></atom:feed>",
        ),
        (
            "map-rdf.xml",
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:geo=\"http://www.georss.org/georss\"><item><geo:point>37.4 -122.1</geo:point></item></rdf:RDF>",
        ),
    ] {
        let input = temporary.path().join(filename);
        fs::write(&input, source_xml).unwrap();
        assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::GeoRss);
        let output = temporary.path().join(format!("{filename}-out"));
        let report = convert_path(&input, output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, SourceFormat::GeoRss);
        assert_eq!(report.page_count, 1);
    }

    let unsupported_gml_crs = temporary.path().join("unsupported-georss-crs.rss");
    fs::write(
        &unsupported_gml_crs,
        "<rss xmlns:g=\"http://www.georss.org/georss\" xmlns:m=\"http://www.opengis.net/gml/3.2\"><channel><item><g:where><m:Point srsName=\"EPSG:3857\"><m:pos>0 0</m:pos></m:Point></g:where></item></channel></rss>",
    )
    .unwrap();
    assert!(
        convert_path(
            &unsupported_gml_crs,
            temporary.path().join("unsupported-georss-crs-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("without a coordinate transformation")
    );
}

#[test]
fn converts_gml_linear_geometries_with_crs_axis_order_and_no_fetching() {
    let temporary = TempDir::new().unwrap();
    let gml = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gml");
    assert_eq!(SourceFormat::detect(&gml).unwrap(), SourceFormat::Gml);
    let output = temporary.path().join("gml-out");
    let report = convert_path(&gml, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("GML Map Preview"));
    assert!(svg.contains("gml:polygon"));
    assert!(svg.contains("gml:line"));
    assert!(svg.contains("gml:point"));
    assert!(svg.contains("3 features, 3 geometries, and 14 positions"));
    assert!(!svg.contains("example.invalid"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("ordinates"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("not fetched"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("properties"))
    );

    let extensionless = temporary.path().join("spatial-features");
    fs::copy(&gml, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Gml
    );
    let generic_xml = temporary.path().join("spatial-features.xml");
    fs::copy(&gml, &generic_xml).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_xml).unwrap(),
        SourceFormat::Gml
    );

    let gml2 = temporary.path().join("gml2.gml");
    fs::write(
        &gml2,
        b"<gml:FeatureCollection xmlns:gml=\"http://www.opengis.net/gml\"><gml:featureMember><gml:Point srsName=\"CRS:84\"><gml:coordinates>-122.1,37.4</gml:coordinates></gml:Point></gml:featureMember></gml:FeatureCollection>",
    )
    .unwrap();
    assert_eq!(SourceFormat::detect(&gml2).unwrap(), SourceFormat::Gml);
    assert_eq!(
        convert_path(
            &gml2,
            temporary.path().join("gml2-out"),
            &ConvertOptions::default()
        )
        .unwrap()
        .source_format,
        SourceFormat::Gml
    );

    let unsupported_crs = temporary.path().join("unknown-crs.gml");
    fs::write(
        &unsupported_crs,
        b"<gml:Point xmlns:gml=\"http://www.opengis.net/gml/3.2\" srsName=\"EPSG:3857\"><gml:pos>0 0</gml:pos></gml:Point>",
    )
    .unwrap();
    assert!(
        convert_path(
            &unsupported_crs,
            temporary.path().join("unknown-crs-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("without a coordinate transformation")
    );

    let oversized_position_list = temporary.path().join("oversized-poslist.gml");
    let tuples = vec!["-122.1 37.4"; 500_001].join(" ");
    let oversized_xml = format!(
        "<gml:LineString xmlns:gml=\"http://www.opengis.net/gml/3.2\" srsName=\"CRS:84\"><gml:posList srsDimension=\"2\">{tuples}</gml:posList></gml:LineString>"
    );
    fs::write(&oversized_position_list, oversized_xml).unwrap();
    assert!(
        convert_path(
            &oversized_position_list,
            temporary.path().join("oversized-poslist-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("exceeds 500000 positions")
    );
}

#[test]
fn converts_gpx_waypoints_routes_and_separate_track_segments() {
    let temporary = TempDir::new().unwrap();
    let gpx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gpx");
    assert_eq!(SourceFormat::detect(&gpx).unwrap(), SourceFormat::Gpx);
    let output = temporary.path().join("gpx-out");
    let report = convert_path(&gpx, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gpx);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("GPX Map Preview"));
    assert!(svg.contains("data-source-format=\"gpx\""));
    assert!(svg.contains("gpx:point"));
    assert!(svg.contains("gpx:line"));
    assert!(svg.contains("3 features, 5 geometries, and 8 positions"));
    assert!(!svg.contains("example.invalid"));
    assert!(!svg.contains("Harbor loop"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("elevation"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("not fetched"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("extension data"))
    );

    let extensionless = temporary.path().join("gps-data");
    fs::copy(&gpx, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Gpx
    );
    let generic_xml = temporary.path().join("gps-data.xml");
    fs::copy(&gpx, &generic_xml).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_xml).unwrap(),
        SourceFormat::Gpx
    );

    let prefixed = temporary.path().join("prefixed-gpx.xml");
    fs::write(
        &prefixed,
        b"<gps:gpx xmlns:gps=\"http://www.topografix.com/GPX/1/1\" version=\"1.1\" creator=\"fixture\"><gps:wpt lat=\"37.4\" lon=\"-122.1\"/></gps:gpx>",
    )
    .unwrap();
    assert_eq!(SourceFormat::detect(&prefixed).unwrap(), SourceFormat::Gpx);
    let prefixed_report = convert_path(
        &prefixed,
        temporary.path().join("prefixed-gpx-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(prefixed_report.source_format, SourceFormat::Gpx);

    let wrong_version = temporary.path().join("gpx-1.0.xml");
    let xml = fs::read_to_string(&gpx)
        .unwrap()
        .replace("version=\"1.1\"", "version=\"1.0\"");
    fs::write(&wrong_version, xml).unwrap();
    assert!(
        convert_path(
            &wrong_version,
            temporary.path().join("wrong-version-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("only GPX 1.1")
    );

    let invalid_coordinate = temporary.path().join("gpx-outside-world.gpx");
    fs::write(
        &invalid_coordinate,
        b"<gpx xmlns=\"http://www.topografix.com/GPX/1/1\" version=\"1.1\"><wpt lat=\"90.1\" lon=\"0\"/></gpx>",
    )
    .unwrap();
    assert!(
        convert_path(
            &invalid_coordinate,
            temporary.path().join("invalid-coordinate-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("outside WGS 84 bounds")
    );
}

#[test]
fn converts_bounded_wkt_simple_geometries_and_enforces_srid_and_dimensions() {
    let temporary = TempDir::new().unwrap();
    let wkt = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wkt");
    assert_eq!(SourceFormat::detect(&wkt).unwrap(), SourceFormat::Wkt);
    let output = temporary.path().join("wkt-out");
    let report = convert_path(&wkt, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Wkt);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("WKT Geometry Map Preview"));
    assert!(svg.contains("data-source-format=\"wkt\""));
    assert!(svg.contains("wkt:polygon"));
    assert!(svg.contains("wkt:line"));
    assert!(svg.contains("wkt:point"));
    assert!(svg.contains("1 features, 7 geometries, and 24 positions"));
    assert!(
        !report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("no SRID"))
    );

    let extensionless = temporary.path().join("map-geometry");
    fs::copy(&wkt, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Wkt
    );

    let untagged = temporary.path().join("untagged.wkt");
    fs::write(&untagged, "POINT ZM (-122.1 37.4 30 99)").unwrap();
    let untagged_report = convert_path(
        &untagged,
        temporary.path().join("untagged-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        untagged_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("no SRID"))
    );
    assert!(
        untagged_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("Z/M"))
    );

    let inherited_dimensions = temporary.path().join("inherited-dimensions.wkt");
    fs::write(
        &inherited_dimensions,
        "GEOMETRYCOLLECTION Z (POINT (-122.1 37.4 12), LINESTRING (-122.2 37.3 4, -122.0 37.5 5))",
    )
    .unwrap();
    let inherited_report = convert_path(
        &inherited_dimensions,
        temporary.path().join("inherited-dimensions-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        inherited_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("Z/M"))
    );

    let empty_member = temporary.path().join("empty-member.wkt");
    fs::write(
        &empty_member,
        "SRID=4326; MULTIPOINT (EMPTY, (-122.1 37.4))",
    )
    .unwrap();
    let empty_report = convert_path(
        &empty_member,
        temporary.path().join("empty-member-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        empty_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("empty WKT geometry members"))
    );

    let bom = temporary.path().join("bom-geometry");
    fs::write(&bom, "\u{feff}POINT (-122.1 37.4)").unwrap();
    assert_eq!(SourceFormat::detect(&bom).unwrap(), SourceFormat::Wkt);
    assert_eq!(
        convert_path(
            &bom,
            temporary.path().join("bom-out"),
            &ConvertOptions::default()
        )
        .unwrap()
        .source_format,
        SourceFormat::Wkt
    );

    let unsupported_srid = temporary.path().join("unsupported-srid.ewkt");
    fs::write(&unsupported_srid, "SRID=3857;POINT(0 0)").unwrap();
    assert!(
        convert_path(
            &unsupported_srid,
            temporary.path().join("unsupported-srid-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("without a coordinate transformation")
    );

    let curved = temporary.path().join("curved.wkt");
    fs::write(&curved, "CIRCULARSTRING(0 0,1 1,2 0)").unwrap();
    assert!(
        convert_path(
            &curved,
            temporary.path().join("curved-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("outside the supported linear")
    );
}

#[test]
fn converts_kml_geometries_and_kmz_documents_without_fetching_links() {
    let temporary = TempDir::new().unwrap();
    let kml = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.kml");
    assert_eq!(SourceFormat::detect(&kml).unwrap(), SourceFormat::Kml);
    let output = temporary.path().join("kml-out");
    let report = convert_path(&kml, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Kml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("KML Map Preview"));
    assert!(svg.contains("data-source-format=\"kml\""));
    assert!(svg.contains("kml:polygon"));
    assert!(svg.contains("kml:line"));
    assert!(svg.contains("kml:point"));
    assert!(!svg.contains("example.invalid"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("altitude"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("not fetched"))
    );
    let limited_options = ConvertOptions {
        max_xml_events: 2,
        ..ConvertOptions::default()
    };
    let limited_output = temporary.path().join("kml-limited-out");
    assert!(
        convert_path(&kml, &limited_output, &limited_options)
            .unwrap_err()
            .to_string()
            .contains("XML events")
    );

    let extensionless = temporary.path().join("placemarks");
    fs::copy(&kml, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Kml
    );
    let generic_xml = temporary.path().join("placemarks.xml");
    fs::copy(&kml, &generic_xml).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_xml).unwrap(),
        SourceFormat::Kml
    );

    let invalid_coordinates = temporary.path().join("outside-world.kml");
    fs::write(
        &invalid_coordinates,
        b"<kml xmlns=\"http://www.opengis.net/kml/2.2\"><Placemark><Point><coordinates>181,91</coordinates></Point></Placemark></kml>",
    )
    .unwrap();
    assert!(
        convert_path(
            &invalid_coordinates,
            temporary.path().join("invalid-coordinate-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("outside WGS 84 bounds")
    );

    let kmz = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.kmz");
    assert_eq!(SourceFormat::detect(&kmz).unwrap(), SourceFormat::Kmz);
    let kmz_output = temporary.path().join("kmz-out");
    let kmz_report = convert_path(&kmz, &kmz_output, &ConvertOptions::default()).unwrap();
    assert_eq!(kmz_report.source_format, SourceFormat::Kmz);
    let kmz_svg = fs::read_to_string(kmz_output.join("page-0001.svg")).unwrap();
    assert!(kmz_svg.contains("kmz:point"));
    assert!(!kmz_svg.contains("example.invalid"));
    assert!(
        kmz_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("embedded images/models"))
    );

    let extensionless_kmz = temporary.path().join("compressed-map");
    fs::copy(&kmz, &extensionless_kmz).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless_kmz).unwrap(),
        SourceFormat::Kmz
    );

    let unsafe_kmz = temporary.path().join("unsafe.kmz");
    let mut archive = ZipWriter::new(File::create(&unsafe_kmz).unwrap());
    archive
        .start_file("../escape.kml", SimpleFileOptions::default())
        .unwrap();
    archive.write_all(&fs::read(&kml).unwrap()).unwrap();
    archive.finish().unwrap();
    assert!(
        convert_path(
            &unsafe_kmz,
            temporary.path().join("unsafe-kmz-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("unsafe part name")
    );
}

#[test]
fn converts_legacy_xls_using_bounded_worksheet_pages() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy_sample.xls");
    let output = temporary.path().join("legacy-xls-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xls);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("sample — rows 1–4, columns A–B"));
    assert!(svg.contains("Name"));
    assert!(svg.contains("Amount"));
    assert!(svg.contains("Alpha"));
    assert!(svg.contains("99.75"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("cached results"))
    );

    let template = temporary.path().join("template.xlt");
    fs::copy(input, &template).unwrap();
    assert_eq!(SourceFormat::detect(&template).unwrap(), SourceFormat::Xls);
    let template_report = convert_path(
        &template,
        temporary.path().join("legacy-xlt-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(template_report.source_format, SourceFormat::Xls);
}

#[test]
fn converts_legacy_word_doc_text_and_disambiguates_dot_templates() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_legacy.doc");
    let output = temporary.path().join("legacy-doc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Doc);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("document-svg sample"));
    assert!(svg.contains("Supported inputs"));
    assert!(svg.contains("Entry point") && svg.contains("Output"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("layout"))
    );

    let extensionless = temporary.path().join("legacy-word");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Doc
    );

    let template = temporary.path().join("template.dot");
    fs::copy(&input, &template).unwrap();
    assert_eq!(SourceFormat::detect(&template).unwrap(), SourceFormat::Doc);
    let graphviz = temporary.path().join("diagram.dot");
    fs::write(&graphviz, "digraph sample { a -> b; }").unwrap();
    assert_eq!(SourceFormat::detect(&graphviz).unwrap(), SourceFormat::Dot);
}

#[test]
fn converts_legacy_powerpoint_binary_text_in_live_slide_order() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_legacy.ppt");
    let output = temporary.path().join("legacy-ppt-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ppt);
    assert_eq!(report.page_count, 2);

    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(first.contains("Slide 1"));
    assert!(first.contains("document-svg"));
    assert!(first.contains("self-contained SVG pages"));
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(second.contains("Slide 2"));
    assert!(second.contains("One page in, one SVG out"));
    assert!(second.contains("Every page becomes page-NNNN.svg"));
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("geometry"))
    );

    let extensionless = temporary.path().join("legacy-presentation");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ppt
    );
}

#[test]
fn converts_kicad_pcb_tracks_pads_zone_and_sniffs_extensionless_boards() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.kicad_pcb");
    let output = temporary.path().join("kicad-pcb-out");

    assert_eq!(
        SourceFormat::detect(&input).unwrap(),
        SourceFormat::KicadPcb
    );
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::KicadPcb);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("PCB DEMO"));
    assert!(svg.contains("R1"));
    assert!(svg.contains("pcb:track"));
    assert!(svg.contains("pcb:pad"));
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("2D artwork"))
    );

    let extensionless = temporary.path().join("board");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::KicadPcb
    );
}

#[test]
fn converts_legacy_word_complex_piece_table_text() {
    fn write_complex_doc(path: &Path, text: &str, one_table: bool, encrypted: bool) {
        let text_offset = 1024usize;
        let text_bytes = text.as_bytes();
        let mut word_document = vec![0u8; text_offset + text_bytes.len()];
        word_document[text_offset..].copy_from_slice(text_bytes);
        word_document[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
        word_document[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
        word_document[6..8].copy_from_slice(&0x0409u16.to_le_bytes());
        let flags: u16 =
            0x0004 | if one_table { 0x0200 } else { 0 } | if encrypted { 0x0100 } else { 0 };
        word_document[10..12].copy_from_slice(&flags.to_le_bytes());
        word_document[32..34].copy_from_slice(&14u16.to_le_bytes()); // csw
        let cslw_offset = 34 + 14 * 2;
        word_document[cslw_offset..cslw_offset + 2].copy_from_slice(&22u16.to_le_bytes());
        let fib_rg_lw = cslw_offset + 2;
        let word_document_len = word_document.len() as u32;
        word_document[fib_rg_lw..fib_rg_lw + 4].copy_from_slice(&word_document_len.to_le_bytes());
        word_document[fib_rg_lw + 12..fib_rg_lw + 16]
            .copy_from_slice(&(text.len() as i32).to_le_bytes());
        let fc_lcb_count_offset = fib_rg_lw + 22 * 4;
        word_document[fc_lcb_count_offset..fc_lcb_count_offset + 2]
            .copy_from_slice(&93u16.to_le_bytes());

        let mut plc_pcd = Vec::new();
        plc_pcd.extend_from_slice(&0u32.to_le_bytes());
        plc_pcd.extend_from_slice(&(text.len() as u32).to_le_bytes());
        plc_pcd.extend_from_slice(&[0, 0]); // PCD flags
        let compressed_fc = 0x4000_0000u32 | (text_offset as u32 * 2);
        plc_pcd.extend_from_slice(&compressed_fc.to_le_bytes());
        plc_pcd.extend_from_slice(&[0, 0]); // PCD property modifier
        let mut clx = vec![0x02]; // Pcdt marker
        clx.extend_from_slice(&(plc_pcd.len() as u32).to_le_bytes());
        clx.extend_from_slice(&plc_pcd);
        let clx_table_entry = fc_lcb_count_offset + 2 + 33 * 8;
        word_document[clx_table_entry..clx_table_entry + 4].copy_from_slice(&0u32.to_le_bytes());
        word_document[clx_table_entry + 4..clx_table_entry + 8]
            .copy_from_slice(&(clx.len() as u32).to_le_bytes());

        let mut compound = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
        compound
            .create_stream("WordDocument")
            .unwrap()
            .write_all(&word_document)
            .unwrap();
        compound
            .create_stream(if one_table { "1Table" } else { "0Table" })
            .unwrap()
            .write_all(&clx)
            .unwrap();
        fs::write(path, compound.into_inner().into_inner()).unwrap();
    }

    let temporary = TempDir::new().unwrap();
    for (one_table, name) in [(false, "zero-table"), (true, "one-table")] {
        let input = temporary
            .path()
            .join(format!("complex-piece-table-{name}.doc"));
        let output = temporary.path().join(format!("out-{name}"));
        write_complex_doc(&input, "Complex piece-table text\r", one_table, false);
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, SourceFormat::Doc);
        assert_eq!(report.page_count, 1);
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        assert!(svg.contains("Complex piece-table text"));
    }

    let encrypted = temporary.path().join("encrypted-legacy-word.doc");
    write_complex_doc(&encrypted, "Secret text\r", false, true);
    let error = convert_path(
        &encrypted,
        temporary.path().join("encrypted-out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(&error, document_svg::Error::Unsupported(message) if message.contains("encrypted or obfuscated")),
        "{error}"
    );
}

#[test]
fn converts_xlsb_shared_strings_and_detects_binary_workbook_packages() {
    let temporary = TempDir::new().unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/calamine_any_sheets.xlsb");
    let output = temporary.path().join("xlsb-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xlsb);
    assert_eq!(report.page_count, 3);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("hidden"))
    );
    let first_svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(first_svg.contains("Visible — rows 1–5"));
    assert!(first_svg.contains("data-source-format=\"xlsb\""));

    let extensionless = temporary.path().join("excel-package");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xlsb
    );
}

#[test]
fn formats_xlsx_dates_currency_and_percent_values() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("formatted.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><workbookPr date1904="1"/><sheets><sheet name="Formats" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/styles.xml",
                r#"<styleSheet><numFmts count="1"><numFmt numFmtId="200" formatCode="¥#,##0.00"/></numFmts><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><borders count="1"><border/></borders><cellXfs count="3"><xf numFmtId="14"/><xf numFmtId="200"/><xf numFmtId="10"/></cellXfs></styleSheet>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" s="0"><v>0</v></c><c r="B1" s="1"><v>12345.6</v></c><c r="C1" s="2"><v>0.125</v></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">1/1/04</tspan>"));
    assert!(svg.contains(">¥12,345.60</tspan>"));
    assert!(svg.contains(">12.50%</tspan>"));
}

#[test]
fn renders_xlsx_conditional_formats() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("conditional.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Conditional" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/styles.xml",
                r#"<styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><borders count="1"><border/></borders><cellXfs count="1"><xf/></cellXfs><dxfs count="1"><dxf><font><color rgb="FFFF0000"/></font><fill><patternFill patternType="solid"><fgColor rgb="FFFFFF00"/></patternFill></fill></dxf></dxfs></styleSheet>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>1</v></c><c r="C1"><v>1</v></c></row><row r="2"><c r="A2"><v>5</v></c><c r="B2"><v>5</v></c><c r="C2"><v>5</v></c></row><row r="3"><c r="A3"><v>10</v></c><c r="B3"><v>10</v></c><c r="C3"><v>10</v></c></row></sheetData><conditionalFormatting sqref="A1:A3"><cfRule type="cellIs" dxfId="0" priority="1" stopIfTrue="1" operator="greaterThan"><formula>4</formula></cfRule></conditionalFormatting><conditionalFormatting sqref="B1:B3"><cfRule type="colorScale" priority="2"><colorScale><cfvo type="min"/><cfvo type="percent" val="50"/><cfvo type="max"/><color rgb="FFFF0000"/><color rgb="FFFFFF00"/><color rgb="FF00FF00"/></colorScale></cfRule></conditionalFormatting><conditionalFormatting sqref="C1:C3"><cfRule type="dataBar" priority="3"><dataBar><cfvo type="min"/><cfvo type="max"/><color rgb="FF638EC6"/></dataBar><extLst><ext><x14:id>duplicate</x14:id></ext></extLst></cfRule></conditionalFormatting><extLst><ext><x14:conditionalFormattings><x14:conditionalFormatting><x14:cfRule type="dataBar" priority="3"><x14:dataBar><x14:cfvo type="min"/><x14:cfvo type="max"/><x14:fillColor rgb="FF638EC6"/></x14:dataBar></x14:cfRule><xm:sqref>C1:C3</xm:sqref></x14:conditionalFormatting></x14:conditionalFormattings></ext></extLst></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"cell-fill-2-1\""));
    assert!(svg.contains("fill=\"#FFFF00\""));
    assert!(svg.contains("id=\"cell-text-2-1\""));
    assert!(svg.contains("fill=\"#FF0000\""));
    assert!(svg.contains("id=\"cell-fill-1-2\""));
    assert!(svg.contains("id=\"cell-fill-3-2\""));
    assert_eq!(
        svg.matches("data-content-kind=\"conditional-data-bar\"")
            .count(),
        3
    );
    assert!(svg.contains("fill=\"#638EC6\" fill-opacity=\"0.65\""));
}

#[test]
fn renders_xlsx_blank_and_text_conditional_formats() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("text-conditional.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Text rules" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/styles.xml",
                r#"<styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><borders count="1"><border/></borders><cellXfs count="1"><xf/></cellXfs><dxfs count="1"><dxf><font><color rgb="FFFF0000"/></font><fill><patternFill patternType="solid"><fgColor rgb="FFFFFF00"/></patternFill></fill></dxf></dxfs></styleSheet>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1"><v></v></c><c r="B1" t="inlineStr"><is><t>Needs Review</t></is></c><c r="C1" t="inlineStr"><is><t>yes</t></is></c></row><row r="2"><c r="C2" t="inlineStr"><is><t>no</t></is></c></row></sheetData><conditionalFormatting sqref="A1"><cfRule type="containsBlanks" dxfId="0" priority="1"><formula>LEN(TRIM(A1))=0</formula></cfRule></conditionalFormatting><conditionalFormatting sqref="B1"><cfRule type="containsText" operator="containsText" text="review" dxfId="0" priority="2"><formula>NOT(ISERROR(SEARCH("review",B1)))</formula></cfRule></conditionalFormatting><conditionalFormatting sqref="C1:C2"><cfRule type="expression" dxfId="0" priority="3"><formula>OR($C$1:$C$2="yes")</formula></cfRule></conditionalFormatting></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"cell-fill-1-1\""));
    assert!(svg.contains("id=\"cell-fill-1-2\""));
    assert!(svg.contains("id=\"cell-fill-1-3\""));
    assert!(svg.contains("id=\"cell-fill-2-3\""));
    assert_eq!(svg.matches("fill=\"#FFFF00\"").count(), 4);
    assert!(svg.contains("id=\"cell-text-1-2\""));
    assert!(svg.contains("fill=\"#FF0000\""));
}

#[test]
fn evaluates_xlsx_formulas_when_cached_values_are_missing() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("formulas.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Formula" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1"><v>2</v></c><c r="B1"><f>SUM(A1:A2)</f></c><c r="C1" t="inlineStr"><is><t>Visible</t></is></c><c r="E1" t="inlineStr"><is><t>Key A</t></is></c><c r="F1" t="inlineStr"><is><t>Result A</t></is></c></row><row r="2"><c r="A2"><v>3</v></c><c r="B2"><f>A1*A2+1</f></c><c r="C2"><f>IF(C1&lt;&gt;"",C1,"")</f></c><c r="E2" t="inlineStr"><is><t>Key B</t></is></c><c r="F2" t="inlineStr"><is><t>Result B</t></is></c></row><row r="3"><c r="B3"><f>IF(6&lt;B2,ROUND(9.6,0),0)</f></c><c r="C3"><f>COUNTA(C1:C2)</f></c></row><row r="4"><c r="C4"><f>IF(D1="","",D1)</f></c></row><row r="5"><c r="C5"><f>VLOOKUP("Key B",E1:F2,2,0)</f></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Formula!B1\""));
    assert!(svg.contains(">5</tspan>"));
    assert!(svg.contains("data-source-id=\"Formula!B2\""));
    assert!(svg.contains(">7</tspan>"));
    assert!(svg.contains("data-source-id=\"Formula!B3\""));
    assert!(svg.contains(">10</tspan>"));
    assert!(svg.contains("data-source-id=\"Formula!C2\""));
    assert!(svg.contains(">Visible</tspan>"));
    assert!(svg.contains("data-source-id=\"Formula!C3\""));
    assert!(svg.contains(">2</tspan>"));
    assert!(!svg.contains("data-source-id=\"Formula!C4\""));
    assert!(svg.contains("data-source-id=\"Formula!C5\""));
    assert!(svg.contains(">Result B</tspan>"));
}

#[test]
fn evaluates_xlsx_cross_sheet_and_defined_name_formulas() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("cross-sheet-formulas.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Main" sheetId="1" r:id="rId1"/><sheet name="Other Sheet" sheetId="2" r:id="rId2"/></sheets><definedNames><definedName name="LocalName">'Main'!$A$1</definedName><definedName name="ExternalName">'Other Sheet'!$A$1</definedName></definedNames></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Local</t></is></c><c r="B1"><f>IF('Other Sheet'!A1&lt;&gt;"",'Other Sheet'!A1,"")</f></c><c r="C1"><f>IF(LocalName="Local","Named","")</f></c><c r="D1"><f>IF(ExternalName="External","External named","")</f></c></row></sheetData></worksheet>"#,
            ),
            (
                "xl/worksheets/sheet2.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>External</t></is></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 2);
    let main = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(main.contains("data-source-id=\"Main!B1\""));
    assert!(main.contains(">External</tspan>"));
    assert!(main.contains("data-source-id=\"Main!C1\""));
    assert!(main.contains(">Named</tspan>"));
    assert!(main.contains("data-source-id=\"Main!D1\""));
    assert!(main.contains(">External named</tspan>"));
}

#[test]
fn evaluates_xlsx_conditional_aggregate_formulas_without_cached_values() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("conditional-formulas.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Criteria" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>East</t></is></c><c r="B1"><v>10</v></c><c r="C1"><f>COUNTIF(A1:A3,"e*")</f></c><c r="D1"><f>COUNTIFS(A1:A3,"east",B1:B3,"&gt;20")</f></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>West</t></is></c><c r="B2"><v>20</v></c><c r="C2"><f>SUMIF(A1:A3,"east",B1:B3)</f></c><c r="D2"><f>SUMIFS(B1:B3,A1:A3,"east",B1:B3,"&gt;10")</f></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>East</t></is></c><c r="B3"><v>30</v></c><c r="C3"><f>AVERAGEIF(B1:B3,"&gt;10")</f></c><c r="D3"><f>AVERAGEIFS(B1:B3,A1:A3,"east")</f></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Criteria!C1\""));
    assert!(svg.contains(">2</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!C2\""));
    assert!(svg.contains(">40</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!C3\""));
    assert!(svg.contains(">25</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!D1\""));
    assert!(svg.contains(">1</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!D2\""));
    assert!(svg.contains(">30</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!D3\""));
    assert!(svg.contains(">20</tspan>"));
}

#[test]
fn paginates_xlsx_print_area_and_manual_breaks() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("print-area.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Print" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">'Print'!$B$2:$D$6</definedName></definedNames></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="2"><c r="B2" t="inlineStr"><is><t>B2</t></is></c><c r="C2" t="inlineStr"><is><t>C2</t></is></c><c r="D2" t="inlineStr"><is><t>D2</t></is></c></row><row r="4"><c r="B4" t="inlineStr"><is><t>B4</t></is></c><c r="C4" t="inlineStr"><is><t>C4</t></is></c><c r="D4" t="inlineStr"><is><t>D4</t></is></c></row><row r="6"><c r="B6" t="inlineStr"><is><t>B6</t></is></c><c r="C6" t="inlineStr"><is><t>C6</t></is></c><c r="D6" t="inlineStr"><is><t>D6</t></is></c></row></sheetData><rowBreaks count="1" manualBreakCount="1"><brk id="3" min="1" max="3" man="1"/></rowBreaks><colBreaks count="1" manualBreakCount="1"><brk id="2" min="1" max="5" man="1"/></colBreaks></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    assert_eq!(report.page_count, 4);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let third = fs::read_to_string(output.join("page-0003.svg")).unwrap();
    assert!(first.contains("<title>Print (1/4)</title>"));
    assert!(first.contains("width=\"48pt\" height=\"30pt\""));
    assert!(first.contains("Print!B2"));
    assert!(!first.contains("Print!C2"));
    assert!(third.contains("width=\"96pt\" height=\"30pt\""));
    assert!(third.contains("Print!C2"));
    assert!(third.contains("Print!D2"));
}

#[test]
fn repeats_xlsx_print_title_rows_and_columns_on_later_pages() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("print-titles.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Titles" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">'Titles'!$A$1:$D$6</definedName><definedName name="_xlnm.Print_Titles" localSheetId="0">'Titles'!$1:$1,'Titles'!$A:$A</definedName></definedNames></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>A1 title</t></is></c><c r="B1" t="inlineStr"><is><t>B1 title</t></is></c><c r="C1" t="inlineStr"><is><t>C1 title</t></is></c><c r="D1" t="inlineStr"><is><t>D1 title</t></is></c></row><row r="2"><c r="B2" t="inlineStr"><is><t>B2 omitted</t></is></c></row><row r="4"><c r="A4" t="inlineStr"><is><t>A4 title</t></is></c><c r="C4" t="inlineStr"><is><t>C4 body</t></is></c><c r="D4" t="inlineStr"><is><t>D4 body</t></is></c></row><row r="6"><c r="A6" t="inlineStr"><is><t>A6 title</t></is></c><c r="C6" t="inlineStr"><is><t>C6 body</t></is></c><c r="D6" t="inlineStr"><is><t>D6 body</t></is></c></row></sheetData><rowBreaks count="1" manualBreakCount="1"><brk id="3" min="0" max="3" man="1"/></rowBreaks><colBreaks count="1" manualBreakCount="1"><brk id="2" min="0" max="5" man="1"/></colBreaks></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 4);
    let fourth = fs::read_to_string(output.join("page-0004.svg")).unwrap();
    assert!(fourth.contains("<title>Titles (4/4)</title>"));
    assert!(fourth.contains("width=\"144pt\" height=\"60pt\""));
    assert!(fourth.contains("Titles!A1"));
    assert!(fourth.contains("Titles!C1"));
    assert!(fourth.contains("Titles!D1"));
    assert!(fourth.contains("Titles!A4"));
    assert!(fourth.contains("Titles!A6"));
    assert!(fourth.contains("Titles!C4"));
    assert!(fourth.contains("Titles!D6"));
    assert!(!fourth.contains("Titles!B1"));
    assert!(!fourth.contains("Titles!B2"));
    assert!(fourth.contains("transform=\"matrix(1 0 0 1 -48 0)\""));
    assert!(fourth.contains("transform=\"matrix(1 0 0 1 0 -30)\""));
    assert!(fourth.contains("transform=\"matrix(1 0 0 1 -48 -30)\""));
}

#[test]
fn auto_tiles_large_xlsx_without_a_print_area() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("large-sheet.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Large" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>First tile</t></is></c></row><row r="1100"><c r="A1100" t="inlineStr"><is><t>Second tile</t></is></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("<title>Large (1/2)</title>"));
    assert!(first.contains("width=\"48pt\" height=\"16380pt\""));
    assert!(first.contains("data-source-id=\"Large!A1\""));
    assert!(!first.contains("Large!A1100"));
    assert!(second.contains("<title>Large (2/2)</title>"));
    assert!(second.contains("width=\"48pt\" height=\"120pt\""));
    assert!(!second.contains("data-source-id=\"Large!A1\""));
    assert!(second.contains("Large!A1100"));
}

#[test]
fn auto_tiles_xlsx_by_the_grid_cell_budget() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("wide-grid.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Grid" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>First grid tile</t></is></c></row><row r="50"><c r="AX50" t="inlineStr"><is><t>Second grid tile</t></is></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("width=\"2400pt\" height=\"600pt\""));
    assert!(first.contains("Grid!A1"));
    assert!(!first.contains("Grid!AX50"));
    assert!(second.contains("width=\"2400pt\" height=\"150pt\""));
    assert!(!second.contains("Grid!A1"));
    assert!(second.contains("Grid!AX50"));
}

#[test]
fn fits_xlsx_print_region_to_landscape_paper() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("fit-to-page.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Fit" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">Fit!$A$1:$T$10</definedName></definedNames></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetPr><pageSetUpPr fitToPage="1"/></sheetPr><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Fit content</t></is></c></row></sheetData><printOptions horizontalCentered="1" verticalCentered="1"/><pageMargins left="0.5" right="0.5" top="0.5" bottom="0.5" header="0.2" footer="0.2"/><pageSetup paperSize="9" orientation="landscape" fitToWidth="1" fitToHeight="1"/></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("width=\"841.89pt\" height=\"595.28pt\""));
    assert!(svg.contains("transform=\"matrix(0.80196875 0 0 0.80196875 36"));
    assert!(svg.contains("Fit!A1"));
}

#[test]
fn converts_xlsx_anchored_drawing_image() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("drawing.xlsx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "xl/workbook.xml",
                br#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_vec(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                br#"<worksheet xmlns:r="r"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Image</t></is></c></row></sheetData><drawing r:id="rId2"/></worksheet>"#.to_vec(),
            ),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                br#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "xl/drawings/drawing1.xml",
                br#"<xdr:wsDr xmlns:xdr="xdr" xmlns:a="a" xmlns:r="r"><xdr:oneCellAnchor><xdr:from><xdr:col>1</xdr:col><xdr:colOff>12700</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>12700</xdr:rowOff></xdr:from><xdr:ext cx="127000" cy="254000"/><xdr:pic><xdr:nvPicPr><xdr:cNvPr id="1" name="Logo" descr="One pixel logo"><a:extLst><a:ext uri="metadata"/></a:extLst></xdr:cNvPr></xdr:nvPicPr><xdr:blipFill><a:blip r:embed="rId1"/></xdr:blipFill></xdr:pic></xdr:oneCellAnchor><xdr:oneCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:ext cx="0" cy="0"/><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="2" name="Hidden control" hidden="1"/></xdr:nvSpPr><xdr:spPr><a:prstGeom prst="rect"/></xdr:spPr></xdr:sp></xdr:oneCellAnchor></xdr:wsDr>"#.to_vec(),
            ),
            (
                "xl/drawings/_rels/drawing1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("xl/media/image1.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("data-source-id=\"Logo\""));
    assert!(svg.contains("x=\"49\" y=\"16\" width=\"10\" height=\"20\""));
    assert!(!svg.contains("Hidden control"));
}

#[test]
fn recovers_xlsx_shape_local_geometry_when_cell_anchors_are_zero() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shape-fallback.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Shapes" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns:r="r"><sheetData><row r="5"><c r="G5"><v>1</v></c></row></sheetData><drawing r:id="rId2"/></worksheet>"#,
            ),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#,
            ),
            (
                "xl/drawings/drawing1.xml",
                r#"<xdr:wsDr xmlns:xdr="xdr" xmlns:a="a"><xdr:twoCellAnchor><xdr:from><xdr:col>6</xdr:col><xdr:colOff>1000</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>6</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="1" name="Fallback arrow"/></xdr:nvSpPr><xdr:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="254000" cy="127000"/></a:xfrm><a:prstGeom prst="rightArrow"/><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></xdr:spPr></xdr:sp></xdr:twoCellAnchor><xdr:twoCellAnchor><xdr:from><xdr:col>2</xdr:col><xdr:row>2</xdr:row></xdr:from><xdr:to><xdr:col>2</xdr:col><xdr:row>2</xdr:row></xdr:to><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="2" name="Auto text box"/></xdr:nvSpPr><xdr:spPr><a:xfrm><a:off x="508000" y="508000"/><a:ext cx="0" cy="0"/></a:xfrm><a:prstGeom prst="rect"/><a:noFill/></xdr:spPr><xdr:txBody><a:p><a:r><a:rPr sz="1000"/><a:t>Auto box</a:t></a:r></a:p></xdr:txBody></xdr:sp></xdr:twoCellAnchor></xdr:wsDr>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Fallback arrow\""));
    assert!(svg.contains("fill=\"#FF0000\""));
    assert!(svg.contains("M 10 22.5 H 22 V 20 L 30 25"));
    assert!(svg.contains("data-source-id=\"Auto text box\""));
    assert!(svg.contains(">Auto box</tspan>"));
}

#[test]
fn converts_xlsx_cached_bar_chart() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("chart.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns:r="r"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Chart</t></is></c></row></sheetData><drawing r:id="rId2"/></worksheet>"#,
            ),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#,
            ),
            (
                "xl/drawings/drawing1.xml",
                r#"<xdr:wsDr xmlns:xdr="xdr" xmlns:a="a" xmlns:c="c" xmlns:r="r"><xdr:oneCellAnchor><xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:ext cx="2540000" cy="1524000"/><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="1" name="Sales chart"/></xdr:nvGraphicFramePr><a:graphic><a:graphicData><c:chart r:id="rId1"/></a:graphicData></a:graphic></xdr:graphicFrame></xdr:oneCellAnchor></xdr:wsDr>"#,
            ),
            (
                "xl/drawings/_rels/drawing1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "xl/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Quarterly sales</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:barChart><c:ser><c:tx><c:v>Sales</c:v></c:tx><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Q1</c:v></c:pt><c:pt idx="1"><c:v>Q2</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>10</c:v></c:pt><c:pt idx="1"><c:v>20</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Quarterly sales"));
    assert!(svg.contains("data-content-kind=\"chart-bar\""));
    assert_eq!(svg.matches("data-content-kind=\"chart-bar\"").count(), 2);
}

#[test]
fn converts_minimal_docx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:rPr><w:b/><w:sz w:val="28"/></w:rPr><w:t>Hello DOCX</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Docx);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hello DOCX"));
    assert!(svg.contains("font-weight=\"700\""));
}

#[test]
fn converts_iso_strict_ooxml_word_workbook_and_presentation_packages() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (filename, expected_format, expected_pages, expected_text) in [
        (
            "sample_strict.docx",
            SourceFormat::Docx,
            2,
            "document-svg sample",
        ),
        (
            "sample_strict.xlsx",
            SourceFormat::Xlsx,
            4,
            "Quarterly sales by region",
        ),
        (
            "sample_strict.pptx",
            SourceFormat::Pptx,
            2,
            "PDF and Office documents as self-contained SVG pages",
        ),
    ] {
        let input = fixture_dir.join(filename);
        let output = temporary.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, expected_format, "{filename}");
        assert_eq!(report.page_count, expected_pages, "{filename}");
        assert!(
            report.warnings.is_empty(),
            "{filename}: {:?}",
            report.warnings
        );
        let first_page = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        assert!(first_page.contains(expected_text), "{filename}");
    }
}

#[test]
fn converts_microsoft_project_xml_to_a_bounded_gantt_preview() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.project.xml");
    assert_eq!(
        SourceFormat::detect(&fixture).unwrap(),
        SourceFormat::ProjectXml
    );
    let output = temporary.path().join("project-out");
    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::ProjectXml);
    assert_eq!(report.page_count, 1);
    assert!(report.pages[0].warnings.iter().any(|warning| {
        warning.contains("predecessor relationship") && warning.contains("not recalculated")
    }));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("without a schedule bar"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"msproject\""));
    assert!(svg.contains("Website Refresh Plan"));
    assert!(svg.contains("User &amp; stakeholder discovery"));
    assert!(svg.contains("office:project-task-bar"));
    assert!(svg.contains("office:project-dependency-arrow"));
    assert!(svg.contains("office:project-milestone"));
    assert!(!svg.contains("Private project resource"));
    assert!(!svg.contains("Notes are data"));

    let extensionless = temporary.path().join("project-plan");
    fs::copy(&fixture, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::ProjectXml
    );
    assert_eq!(
        convert_path(
            &extensionless,
            temporary.path().join("project-sniffed-out"),
            &ConvertOptions::default()
        )
        .unwrap()
        .source_format,
        SourceFormat::ProjectXml
    );

    let mspdi_alias = temporary.path().join("schedule.mspdi");
    fs::copy(&fixture, &mspdi_alias).unwrap();
    assert_eq!(
        SourceFormat::detect(&mspdi_alias).unwrap(),
        SourceFormat::ProjectXml
    );
    assert_eq!(
        convert_path(
            &mspdi_alias,
            temporary.path().join("project-mspdi-out"),
            &ConvertOptions::default()
        )
        .unwrap()
        .page_count,
        1
    );

    let limited = ConvertOptions {
        max_pages: 0,
        ..Default::default()
    };
    assert!(matches!(
        convert_path(
            &fixture,
            temporary.path().join("project-limited-out"),
            &limited
        ),
        Err(document_svg::Error::LimitExceeded(_))
    ));

    let many_tasks = temporary.path().join("many-tasks.xml");
    let mut xml = String::from(
        "<Project xmlns=\"http://schemas.microsoft.com/project\"><Name>Task pagination</Name><Tasks>",
    );
    for uid in 1..=26 {
        xml.push_str(&format!(
            "<Task><UID>{uid}</UID><Name>Task {uid}</Name><Start>2026-09-01T08:00:00</Start><Finish>2026-09-02T17:00:00</Finish></Task>"
        ));
    }
    xml.push_str("</Tasks></Project>");
    fs::write(&many_tasks, xml).unwrap();
    let pages = temporary.path().join("project-pages-out");
    let page_report = convert_path(&many_tasks, &pages, &ConvertOptions::default()).unwrap();
    assert_eq!(page_report.page_count, 2);
    let last_page = fs::read_to_string(pages.join("page-0002.svg")).unwrap();
    assert!(last_page.contains("Task 26"));
    let one_page_limit = ConvertOptions {
        max_pages: 1,
        ..Default::default()
    };
    assert!(matches!(
        convert_path(
            &many_tasks,
            temporary.path().join("project-pages-limited-out"),
            &one_page_limit
        ),
        Err(document_svg::Error::LimitExceeded(_))
    ));

    let inverted_dates = temporary.path().join("inverted-project-dates.xml");
    fs::write(
        &inverted_dates,
        r#"<Project xmlns="http://schemas.microsoft.com/project"><Tasks><Task><UID>1</UID><Name>Invalid date order</Name><Start>2026-09-05T08:00:00</Start><Finish>2026-09-01T17:00:00</Finish></Task></Tasks></Project>"#,
    )
    .unwrap();
    let inverted_output = temporary.path().join("inverted-project-out");
    let inverted_report = convert_path(
        &inverted_dates,
        &inverted_output,
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        inverted_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("Finish earlier than Start"))
    );
    let inverted_svg = fs::read_to_string(inverted_output.join("page-0001.svg")).unwrap();
    assert!(!inverted_svg.contains("office:project-task-bar"));
}

#[test]
fn preserves_docx_words_and_script_font_fallbacks_when_wrapping() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("word-wrap.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="SimSun"/><w:sz w:val="40"/></w:rPr><w:t>Alpha Beta Gamma</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="4000" w:h="6000"/><w:pgMar w:top="400" w:right="400" w:bottom="400" w:left="400"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("font-family=\"Times New Roman, SimSun\""));
    assert!(svg.contains(">Alpha Beta</tspan>"));
    assert!(svg.contains(">Gamma</tspan>"));
    assert!(!svg.contains(">Alpha Beta G"));
}

#[test]
fn resolves_docx_based_on_style_chains() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("style-chain.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/styles.xml",
                r#"<w:styles xmlns:w="w"><w:style w:type="paragraph" w:styleId="Base"><w:rPr><w:rFonts w:ascii="Arial"/><w:b/><w:sz w:val="28"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Derived"><w:basedOn w:val="Base"/><w:pPr><w:jc w:val="center"/></w:pPr><w:rPr><w:i/></w:rPr></w:style></w:styles>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body><w:p><w:pPr><w:pStyle w:val="Derived"/></w:pPr><w:r><w:t>Inherited style</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="4000" w:h="6000"/><w:pgMar w:top="400" w:right="400" w:bottom="400" w:left="400"/></w:sectPr></w:body></w:document>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("x=\"100\""));
    assert!(svg.contains("font-family=\"Arial\""));
    assert!(svg.contains("font-size=\"14\""));
    assert!(svg.contains("font-weight=\"700\""));
    assert!(svg.contains("font-style=\"italic\""));
}

#[test]
fn renders_docx_superscript_and_subscript_runs() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("vertical-runs.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:rPr><w:sz w:val="20"/></w:rPr><w:t>x</w:t></w:r><w:r><w:rPr><w:sz w:val="20"/><w:vertAlign w:val="superscript"/></w:rPr><w:t>2</w:t></w:r><w:r><w:rPr><w:sz w:val="20"/><w:vertAlign w:val="subscript"/></w:rPr><w:t>3</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="4000" w:h="6000"/><w:pgMar w:top="400" w:right="400" w:bottom="400" w:left="400"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">x</tspan>"));
    assert!(svg.contains("font-size=\"7.5\""));
    assert!(svg.contains("baseline-shift=\"3.5\""));
    assert!(svg.contains("baseline-shift=\"-2\""));
}

#[test]
fn renders_docx_omml_fraction_scripts_and_radical() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("math.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w" xmlns:m="m"><w:body><w:p><m:oMath><m:f><m:num><m:r><m:t>a</m:t></m:r></m:num><m:den><m:r><m:t>b</m:t></m:r></m:den></m:f><m:r><m:t>+</m:t></m:r><m:sSub><m:e><m:r><m:t>x</m:t></m:r></m:e><m:sub><m:r><m:t>i</m:t></m:r></m:sub></m:sSub><m:sSup><m:e><m:r><m:t>y</m:t></m:r></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup><m:rad><m:radPr><m:degHide m:val="1"/></m:radPr><m:deg/><m:e><m:r><m:t>z</m:t></m:r></m:e></m:rad></m:oMath></w:p><w:sectPr><w:pgSz w:w="4000" w:h="6000"/><w:pgMar w:top="400" w:right="400" w:bottom="400" w:left="400"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("(a)/(b)+x"));
    assert!(svg.contains("baseline-shift=\"3.85\""));
    assert!(svg.contains("baseline-shift=\"-2.2\""));
    assert!(svg.contains("√(z)"));
}

#[test]
fn splits_docx_inline_page_breaks_inside_a_paragraph() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline-page-break.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Before break</w:t><w:br w:type="page"/><w:t>After break, page </w:t><w:fldSimple w:instr="PAGE"/></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("Before break"));
    assert!(!first.contains("After break"));
    assert!(!second.contains("Before break"));
    assert!(second.contains("After break, page 2"));
}

#[test]
fn repeats_docx_default_header_and_footer() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("header-footer.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w" xmlns:r="r"><w:body><w:p><w:r><w:t>Page one</w:t></w:r></w:p><w:p><w:r><w:br w:type="page"/><w:t>Page two</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rId1"/><w:footerReference w:type="default" r:id="rId2"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#,
            ),
            (
                "word/header1.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER TEXT</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="3600"/><w:gridCol w:w="3600"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>H1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>H2</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:hdr>"#,
            ),
            (
                "word/footer1.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>FOOTER TEXT</w:t></w:r></w:p></w:ftr>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 2);
    assert!(report.warnings.is_empty());
    for page in ["page-0001.svg", "page-0002.svg"] {
        let svg = fs::read_to_string(output.join(page)).unwrap();
        assert!(svg.contains("HEADER TEXT"));
        assert!(svg.contains("FOOTER TEXT"));
        assert!(svg.contains("data-content-kind=\"header\""));
        assert!(svg.contains("data-content-kind=\"footer\""));
        assert_eq!(
            svg.matches("data-content-kind=\"header-table-cell\"")
                .count(),
            2
        );
        assert!(svg.contains("H1"));
        assert!(svg.contains("H2"));
    }
}

#[test]
fn selects_docx_first_even_and_section_specific_stories() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("section-stories.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w" xmlns:r="r"><w:body><w:p><w:r><w:t>Section one first</w:t></w:r></w:p><w:p><w:r><w:br w:type="page"/><w:t>Section one even</w:t></w:r></w:p><w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rH1D"/><w:headerReference w:type="first" r:id="rH1F"/><w:headerReference w:type="even" r:id="rH1E"/><w:footerReference w:type="default" r:id="rF1D"/><w:footerReference w:type="first" r:id="rF1F"/><w:footerReference w:type="even" r:id="rF1E"/><w:titlePg/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:pPr></w:p><w:p><w:r><w:t>Section two</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rH2"/><w:footerReference w:type="default" r:id="rF2"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="rH1D" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header-default.xml"/><Relationship Id="rH1F" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header-first.xml"/><Relationship Id="rH1E" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header-even.xml"/><Relationship Id="rF1D" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer-default.xml"/><Relationship Id="rF1F" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer-first.xml"/><Relationship Id="rF1E" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer-even.xml"/><Relationship Id="rH2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header-section2.xml"/><Relationship Id="rF2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer-section2.xml"/></Relationships>"#,
            ),
            (
                "word/header-default.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER DEFAULT ONE</w:t></w:r></w:p></w:hdr>"#,
            ),
            (
                "word/header-first.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER FIRST ONE</w:t></w:r></w:p></w:hdr>"#,
            ),
            (
                "word/header-even.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER EVEN ONE</w:t></w:r></w:p></w:hdr>"#,
            ),
            (
                "word/footer-default.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t>FOOTER DEFAULT ONE</w:t></w:r></w:p></w:ftr>"#,
            ),
            (
                "word/footer-first.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t>FOOTER FIRST ONE</w:t></w:r></w:p></w:ftr>"#,
            ),
            (
                "word/footer-even.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t>FOOTER EVEN ONE</w:t></w:r></w:p></w:ftr>"#,
            ),
            (
                "word/header-section2.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER SECTION TWO</w:t></w:r></w:p></w:hdr>"#,
            ),
            (
                "word/footer-section2.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t>FOOTER SECTION TWO</w:t></w:r></w:p></w:ftr>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 3);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    let third = fs::read_to_string(output.join("page-0003.svg")).unwrap();
    assert!(first.contains("HEADER FIRST ONE"));
    assert!(first.contains("FOOTER FIRST ONE"));
    assert!(!first.contains("HEADER DEFAULT ONE"));
    assert!(second.contains("HEADER EVEN ONE"));
    assert!(second.contains("FOOTER EVEN ONE"));
    assert!(!second.contains("HEADER FIRST ONE"));
    assert!(third.contains("HEADER SECTION TWO"));
    assert!(third.contains("FOOTER SECTION TWO"));
    assert!(!third.contains("HEADER EVEN ONE"));
}

#[test]
fn renders_docx_numbered_and_bulleted_lists() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("lists.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="10"/></w:numPr></w:pPr><w:r><w:t>First</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="10"/></w:numPr></w:pPr><w:r><w:t>Nested</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="10"/></w:numPr></w:pPr><w:r><w:t>Second</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="11"/></w:numPr></w:pPr><w:r><w:t>Bullet</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/numbering.xml",
                r#"<w:numbering xmlns:w="w"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1.%2)"/><w:pPr><w:ind w:left="1440" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/></w:lvl></w:abstractNum><w:num w:numId="10"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="11"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">1. First</tspan>"));
    assert!(svg.contains(">1.a) Nested</tspan>"));
    assert!(svg.contains(">2. Second</tspan>"));
    assert!(svg.contains(">• Bullet</tspan>"));
}

#[test]
fn renders_docx_footnotes_and_endnotes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("notes.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Body</w:t><w:footnoteReference w:id="2"/><w:t> and end</w:t><w:endnoteReference w:id="3"/><w:t> comment</w:t><w:commentReference w:id="4"/></w:r><w:del><w:r><w:delText>Deleted revision</w:delText></w:r></w:del><w:ins><w:r><w:t> Inserted revision</w:t></w:r></w:ins></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/footnotes.xml",
                r#"<w:footnotes xmlns:w="w"><w:footnote w:id="-1"><w:p/></w:footnote><w:footnote w:id="2"><w:p><w:r><w:t>Footnote detail</w:t></w:r></w:p></w:footnote></w:footnotes>"#,
            ),
            (
                "word/endnotes.xml",
                r#"<w:endnotes xmlns:w="w"><w:endnote w:id="3"><w:p><w:r><w:t>Endnote detail</w:t></w:r></w:p></w:endnote></w:endnotes>"#,
            ),
            (
                "word/comments.xml",
                r#"<w:comments xmlns:w="w"><w:comment w:id="4" w:author="Reviewer"><w:p><w:r><w:t>Comment detail</w:t></w:r></w:p></w:comment></w:comments>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("baseline-shift=\"3.85\""));
    assert!(svg.contains("data-content-kind=\"note-separator\""));
    assert!(svg.contains("data-content-kind=\"footnote\""));
    assert!(svg.contains("Footnote detail"));
    assert!(svg.contains("data-content-kind=\"endnote\""));
    assert!(svg.contains("Endnote detail"));
    assert!(svg.contains("data-content-kind=\"comment\""));
    assert!(svg.contains("Comment detail"));
    assert!(svg.contains("Inserted revision"));
    assert!(!svg.contains("Deleted revision"));
}

#[test]
fn renders_docx_drawingml_and_vml_text_boxes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("text-boxes.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r##"<w:document xmlns:w="w" xmlns:wp="wp" xmlns:a="a" xmlns:wps="wps" xmlns:v="v"><w:body><w:p><w:r><w:t>Anchor</w:t><w:drawing><wp:anchor><wp:positionH relativeFrom="page"><wp:posOffset>127000</wp:posOffset></wp:positionH><wp:positionV relativeFrom="page"><wp:posOffset>254000</wp:posOffset></wp:positionV><wp:extent cx="1270000" cy="635000"/><wp:docPr id="1" name="Modern box" descr="Modern text box"/><a:graphic><a:graphicData><wps:wsp><wps:spPr><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill><a:ln w="12700"><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:ln></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>Modern textbox</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p><w:p><w:r><w:t>VML anchor</w:t><w:pict><v:shape style="position:absolute;margin-left:30pt;margin-top:40pt;width:120pt;height:55pt" fillcolor="#FFF2CC" strokecolor="#806000" strokeweight="1pt"><v:textbox><w:txbxContent><w:p><w:r><w:t>VML textbox</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"##,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(
        svg.matches("data-content-kind=\"floating-text-box\"")
            .count(),
        2
    );
    assert!(svg.contains("fill=\"#DDEEFF\""));
    assert!(svg.contains("stroke=\"#112233\""));
    assert!(svg.contains("Modern textbox"));
    assert!(svg.contains("fill=\"#FFF2CC\""));
    assert!(svg.contains("stroke=\"#806000\""));
    assert!(svg.contains("VML textbox"));
}

#[test]
fn renders_docx_multi_section_page_setups() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sections.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Portrait section</w:t></w:r></w:p><w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:pPr></w:p><w:p><w:r><w:t>Landscape section</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    assert_eq!(report.page_count, 2);
    let portrait = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let landscape = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(portrait.contains("width=\"612pt\" height=\"792pt\""));
    assert!(portrait.contains("Portrait section"));
    assert!(landscape.contains("width=\"792pt\" height=\"612pt\""));
    assert!(landscape.contains("Landscape section"));
}

#[test]
fn positions_docx_floating_image() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("floating.docx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "word/document.xml",
                br#"<w:document xmlns:w="w" xmlns:wp="wp" xmlns:a="a" xmlns:r="r"><w:body><w:p><w:r><w:t>Body text</w:t></w:r><w:r><w:drawing><wp:anchor><wp:positionH relativeFrom="page"><wp:posOffset>127000</wp:posOffset></wp:positionH><wp:positionV relativeFrom="page"><wp:posOffset>254000</wp:posOffset></wp:positionV><wp:extent cx="254000" cy="127000"/><wp:docPr id="1" name="Floating logo" descr="Floating image"/><a:graphic><a:graphicData><a:blip r:embed="rId1"/></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#.to_vec(),
            ),
            (
                "word/_rels/document.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("word/media/image1.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"floating-image\""));
    assert!(svg.contains("x=\"10\" y=\"20\" width=\"20\" height=\"10\""));
}

#[test]
fn does_not_expose_internal_page_break_markers_from_docx_tables() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("table-break.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Before</w:t><w:br w:type="page"/><w:t>After</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("table pagination may differ"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Before After"));
    assert!(!svg.contains("DOCSVG_BREAK"));
}

#[test]
fn renders_docx_grid_span_as_one_merged_cell() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("grid-span.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:shd w:fill="CCDDEE"/></w:tcPr><w:p><w:r><w:t>Merged heading</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"table-cell\"").count(), 1);
    assert!(svg.contains("data-source-id=\"R1C1\""));
    assert!(svg.contains("Merged heading"));
}

#[test]
fn tiles_xlsx_sheets_that_outgrow_the_configured_paper() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("long.xlsx");
    let output = temporary.path().join("out");
    // 200 default-height rows are far taller than one sheet of letter paper.
    let rows = (1..=200)
        .map(|row| {
            format!(
                r#"<row r="{row}"><c r="A{row}" t="inlineStr"><is><t>row {row}</t></is></c></row>"#
            )
        })
        .collect::<String>();
    let sheet = format!(
        r#"<worksheet><sheetData>{rows}</sheetData><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75"/><pageSetup orientation="portrait"/></worksheet>"#
    );
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            ("xl/worksheets/sheet1.xml", sheet.as_str()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    // A sheet with a print setup must be split across pages rather than drawn
    // past the edge of the first one.
    assert!(
        report.page_count > 1,
        "expected the sheet to be paginated, got {} page(s)",
        report.page_count
    );
    let last_row = ">row 200</tspan>";
    let last_page = output.join(format!("page-{:04}.svg", report.page_count));
    assert!(fs::read_to_string(last_page).unwrap().contains(last_row));
    for page in 1..=report.page_count {
        let svg = fs::read_to_string(output.join(format!("page-{page:04}.svg"))).unwrap();
        assert!(svg.contains(r#"viewBox="0 0 612 792""#));
    }
}

#[test]
fn keeps_fit_to_page_worksheets_on_a_single_page() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("fitted.xlsx");
    let output = temporary.path().join("out");
    let rows = (1..=200)
        .map(|row| {
            format!(
                r#"<row r="{row}"><c r="A{row}" t="inlineStr"><is><t>row {row}</t></is></c></row>"#
            )
        })
        .collect::<String>();
    let sheet = format!(
        r#"<worksheet><sheetData>{rows}</sheetData><pageSetup orientation="portrait" fitToWidth="1" fitToHeight="1"/></worksheet>"#
    );
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            ("xl/worksheets/sheet1.xml", sheet.as_str()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    // Fit-to-page asks for one sheet of paper, so scaling replaces tiling.
    assert_eq!(report.page_count, 1);
}

#[test]
fn keeps_bullet_and_paragraph_end_colors_out_of_the_shape_fill() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bullet-color.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<presentation xmlns:r="r"><sldSz cx="9144000" cy="6858000"/><sldIdLst><sldId id="256" r:id="rId1"/></sldIdLst></presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Box"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="4000000" cy="1000000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="304FFE"/></a:solidFill></p:spPr><p:txBody><a:bodyPr/><a:p><a:pPr><a:buClr><a:srgbClr val="000000"/></a:buClr><a:buNone/></a:pPr><a:r><a:rPr lang="en"/><a:t>Legible</a:t></a:r><a:endParaRPr><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:endParaRPr></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The bullet color and the end-of-paragraph run color describe text, so
    // the shape keeps the fill its own spPr asked for.
    assert!(svg.contains("#304FFE"), "shape fill was overwritten: {svg}");
    assert!(svg.contains("Legible"));
}

#[test]
fn rejects_encrypted_office_documents_as_unsupported() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("locked.xlsx");
    let output = temporary.path().join("out");
    // Office stores a password-protected document in a compound file, not a ZIP.
    let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    bytes.extend_from_slice(&[0u8; 512]);
    fs::write(&input, bytes).unwrap();

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(
        error.to_string().contains("encrypted"),
        "unhelpful error: {error}"
    );
}

#[test]
fn rebuilds_a_cross_reference_table_with_short_entries() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("short-xref.pdf");
    let output = temporary.path().join("out");
    make_pdf(&input);
    // Some producers end each cross-reference entry after 19 bytes instead of
    // the 20 the specification requires; mainstream viewers still open those.
    let bytes = fs::read(&input).unwrap();
    let damaged = String::from_utf8_lossy(&bytes)
        .replace(" n \r\n", " n\n")
        .replace(" f \r\n", " f\n")
        .replace(" n \n", " n\n")
        .replace(" f \n", " f\n");
    fs::write(&input, damaged.as_bytes()).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("cross-reference")),
        "recovery was not reported: {:?}",
        report.warnings
    );
    assert!(
        fs::read_to_string(output.join("page-0001.svg"))
            .unwrap()
            .contains("Hello PDF")
    );
}

#[test]
fn advances_standard_font_text_without_a_widths_array() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("standard-font.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    // Helvetica without /Widths: every reader is expected to know its metrics.
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![20.into(), 100.into()]),
            Operation::new("Tj", vec![Object::string_literal("iiii")]),
            Operation::new("ET", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![20.into(), 60.into()]),
            Operation::new("Tj", vec![Object::string_literal("WWWW")]),
            Operation::new("ET", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A narrow letter must not advance as far as a wide one.
    let xs = |needle: &str| -> Vec<f64> {
        let start = svg.find(needle).expect("run not rendered");
        let tail = &svg[start..];
        let end = tail.find("</text>").unwrap_or(tail.len());
        tail[..end]
            .match_indices("x=\"")
            .filter_map(|(index, _)| {
                let rest = &tail[index + 3..];
                let stop = rest.find('"')?;
                rest[..stop].parse::<f64>().ok()
            })
            .collect()
    };
    let narrow = xs("iiii");
    let wide = xs("WWWW");
    assert!(
        narrow.len() > 1 && wide.len() > 1,
        "expected per-glyph positions, got {narrow:?} and {wide:?}"
    );
    let narrow_step = narrow[1] - narrow[0];
    let wide_step = wide[1] - wide[0];
    assert!(
        wide_step > narrow_step * 2.0,
        "standard font metrics were not applied: 'i' advanced {narrow_step}, 'W' advanced {wide_step}"
    );
}

fn make_zip(path: &Path, entries: &[(&str, &str)]) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn make_zip_bytes(path: &Path, entries: Vec<(&str, Vec<u8>)>) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        zip.start_file(name, options).unwrap();
        zip.write_all(&content).unwrap();
    }
    zip.finish().unwrap();
}

fn make_master_layout_pptx(path: &Path) {
    make_zip(
        path,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                r#"<p:sldMaster xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Master decoration"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="635000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="112233"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Master title"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="914400" y="457200"/><a:ext cx="4572000" cy="914400"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>MASTER SAMPLE</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sldMaster>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                r#"<p:sldLayout xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Layout decoration"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="889000"/><a:ext cx="635000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="445566"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Layout title"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="1828800" y="457200"/><a:ext cx="4572000" cy="914400"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>LAYOUT SAMPLE</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Slide title"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:p><a:r><a:rPr sz="2400"/><a:t>Inherited title</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );
}

fn make_pdf(path: &Path) {
    make_pdf_with_text(path, "Hello PDF");
}

fn make_pdf_with_text(path: &Path, text: &str) {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => Object::Reference(font_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    720.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    document.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "Contents" => Object::Reference(content_id),
            "Resources" => Object::Reference(resources_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    document.trailer.set("Root", Object::Reference(catalog_id));
    document.compress();
    document.save(path).unwrap();
}

fn make_pdf_compatibility_section(path: &Path) {
    let mut document = Document::with_version("1.7");
    let resources_id = document.add_object(dictionary! {});
    let content = Content {
        operations: vec![
            Operation::new("BX", vec![]),
            Operation::new("FutureOperator", vec![1.into()]),
            Operation::new("EX", vec![]),
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![10.into(), 10.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_type3_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let glyph = Content {
        operations: vec![
            Operation::new(
                "d1",
                vec![
                    600.into(),
                    0.into(),
                    0.into(),
                    0.into(),
                    600.into(),
                    700.into(),
                ],
            ),
            Operation::new("m", vec![0.into(), 0.into()]),
            Operation::new("l", vec![300.into(), 700.into()]),
            Operation::new("l", vec![600.into(), 0.into()]),
            Operation::new("h", vec![]),
            Operation::new("f", vec![]),
        ],
    };
    let glyph_id = document.add_object(Stream::new(dictionary! {}, glyph.encode().unwrap()));
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "Name" => "F3",
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 700.into()],
        "FontMatrix" => vec![Object::Real(0.001), 0.into(), 0.into(), Object::Real(0.001), 0.into(), 0.into()],
        "CharProcs" => dictionary! { "A" => Object::Reference(glyph_id) },
        "Encoding" => dictionary! { "Type" => "Encoding", "Differences" => vec![65.into(), Object::Name(b"A".to_vec())] },
        "FirstChar" => 65,
        "LastChar" => 65,
        "Widths" => vec![600.into()],
        "Resources" => dictionary! {},
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F3" => Object::Reference(font_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F3".to_vec()), 100.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 20.into()],
            ),
            Operation::new("Tj", vec![Object::string_literal("A")]),
            Operation::new("ET", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_packed_images_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let grayscale_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 3,
            "Height" => 2,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 4,
            "Decode" => vec![0.into(), 1.into()],
        },
        vec![0x08, 0xF0, 0xF8, 0x00],
    ));
    let stencil_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 8,
            "Height" => 1,
            "ImageMask" => true,
            "BitsPerComponent" => 1,
            "Decode" => vec![0.into(), 1.into()],
        },
        vec![0b1010_1010],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! {
            "Gray4" => Object::Reference(grayscale_id),
            "Stencil" => Object::Reference(stencil_id),
        },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    60.into(),
                    0.into(),
                    0.into(),
                    40.into(),
                    20.into(),
                    20.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Gray4".to_vec())]),
            Operation::new("Q", vec![]),
            Operation::new("q", vec![]),
            Operation::new("rg", vec![0.into(), 1.into(), 0.into()]),
            Operation::new(
                "cm",
                vec![
                    80.into(),
                    0.into(),
                    0.into(),
                    20.into(),
                    100.into(),
                    20.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Stencil".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_jpeg_soft_mask_pdf(path: &Path) {
    let mut mask_jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut mask_jpeg, 100)
        .encode(&[0, 255], 2, 1, jpeg_encoder::ColorType::Luma)
        .unwrap();
    let mut document = Document::with_version("1.7");
    let mask_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 1,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        mask_jpeg,
    ));
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 1,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "SMask" => Object::Reference(mask_id),
        },
        vec![255, 0, 0, 0, 0, 255],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Masked" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    100.into(),
                    0.into(),
                    0.into(),
                    50.into(),
                    20.into(),
                    20.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Masked".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_mismatched_soft_mask_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let mask_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 1,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0, 255],
    ));
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "SMask" => Object::Reference(mask_id),
        },
        vec![255, 0, 0],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Masked" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    100.into(),
                    0.into(),
                    0.into(),
                    50.into(),
                    20.into(),
                    20.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Masked".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_shading_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let shading_id = document.add_object(dictionary! {
        "ShadingType" => 2,
        "ColorSpace" => "DeviceRGB",
        "Coords" => vec![0.into(), 0.into(), 200.into(), 0.into()],
        "Domain" => vec![0.into(), 1.into()],
        "Function" => dictionary! {
            "FunctionType" => 2,
            "Domain" => vec![0.into(), 1.into()],
            "C0" => vec![1.into(), 0.into(), 0.into()],
            "C1" => vec![0.into(), 0.into(), 1.into()],
            "N" => 1,
        },
        "Extend" => vec![true.into(), true.into()],
    });
    let resources_id = document.add_object(dictionary! {
        "Shading" => dictionary! { "Sh1" => Object::Reference(shading_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    500.into(),
                ],
            ),
            Operation::new("sh", vec![Object::Name(b"Sh1".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_function_shading_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let function_id = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        b"{ pop dup 1 exch sub 0.25 }".to_vec(),
    ));
    let shading_id = document.add_object(dictionary! {
        "ShadingType" => 1,
        "ColorSpace" => "DeviceRGB",
        "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
        "Matrix" => vec![180.into(), 0.into(), 0.into(), 80.into(), 10.into(), 10.into()],
        "Function" => Object::Reference(function_id),
    });
    let resources_id = document.add_object(dictionary! {
        "Shading" => dictionary! { "Sh1" => Object::Reference(shading_id) },
    });
    let content = Content {
        operations: vec![Operation::new("sh", vec![Object::Name(b"Sh1".to_vec())])],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_soft_mask_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let mask_content = Content {
        operations: vec![
            Operation::new("g", vec![0.5.into()]),
            Operation::new("re", vec![72.into(), 600.into(), 200.into(), 100.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let mask_form_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {},
            "Group" => dictionary! {
                "S" => "Transparency",
                "CS" => "DeviceGray",
                "I" => true,
            },
        },
        mask_content.encode().unwrap(),
    ));
    let graphics_state_id = document.add_object(dictionary! {
        "Type" => "ExtGState",
        "SMask" => dictionary! {
            "S" => "Luminosity",
            "G" => Object::Reference(mask_form_id),
        },
    });
    let resources_id = document.add_object(dictionary! {
        "ExtGState" => dictionary! { "GS1" => Object::Reference(graphics_state_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new("gs", vec![Object::Name(b"GS1".to_vec())]),
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![72.into(), 600.into(), 200.into(), 100.into()]),
            Operation::new("f", vec![]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_soft_mask_transfer_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let mask_content = Content {
        operations: vec![
            Operation::new("g", vec![Object::Real(0.5)]),
            Operation::new("re", vec![20.into(), 20.into(), 100.into(), 100.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let mask_form_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 144.into(), 144.into()],
            "Resources" => dictionary! {},
            "Group" => dictionary! { "S" => "Transparency", "CS" => "DeviceGray", "I" => true },
        },
        mask_content.encode().unwrap(),
    ));
    let graphics_state_id = document.add_object(dictionary! {
        "Type" => "ExtGState",
        "SMask" => dictionary! {
            "S" => "Luminosity",
            "G" => Object::Reference(mask_form_id),
            "TR" => dictionary! {
                "FunctionType" => 2,
                "Domain" => vec![0.into(), 1.into()],
                "C0" => vec![0.into()],
                "C1" => vec![1.into()],
                "N" => 2,
                "Range" => vec![0.into(), 1.into()],
            },
        },
    });
    let resources_id = document.add_object(dictionary! {
        "ExtGState" => dictionary! { "GS1" => Object::Reference(graphics_state_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("gs", vec![Object::Name(b"GS1".to_vec())]),
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![20.into(), 20.into(), 100.into(), 100.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_nonisolated_group_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let form_content = Content {
        operations: vec![
            Operation::new("rg", vec![0.into(), 0.into(), 1.into()]),
            Operation::new("re", vec![20.into(), 20.into(), 80.into(), 80.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let form_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 144.into(), 144.into()],
            "Resources" => dictionary! {},
            "Group" => dictionary! {
                "Type" => "Group",
                "S" => "Transparency",
                "CS" => "DeviceRGB",
                "I" => false,
                "K" => false,
            },
        },
        form_content.encode().unwrap(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Fm1" => Object::Reference(form_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"Fm1".to_vec())])],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_special_color_space_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let separation_id = document.add_object(Object::Array(vec![
        Object::Name(b"Separation".to_vec()),
        Object::Name(b"LogoGreen".to_vec()),
        Object::Name(b"DeviceRGB".to_vec()),
        Object::Dictionary(dictionary! {
            "FunctionType" => 2,
            "Domain" => vec![0.into(), 1.into()],
            "C0" => vec![1.into(), 1.into(), 1.into()],
            "C1" => vec![Object::Real(0.2), Object::Real(0.8), Object::Real(0.3)],
            "N" => 1,
        }),
    ]));
    let device_n_function_id = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        b"{ 0.5 }".to_vec(),
    ));
    let device_n_id = document.add_object(Object::Array(vec![
        Object::Name(b"DeviceN".to_vec()),
        Object::Array(vec![
            Object::Name(b"Spot1".to_vec()),
            Object::Name(b"Spot2".to_vec()),
        ]),
        Object::Name(b"DeviceRGB".to_vec()),
        Object::Reference(device_n_function_id),
    ]));
    let lab_id = document.add_object(Object::Array(vec![
        Object::Name(b"Lab".to_vec()),
        Object::Dictionary(dictionary! {
            "WhitePoint" => vec![Object::Real(0.95047), 1.into(), Object::Real(1.08883)],
            "Range" => vec![(-128).into(), 127.into(), (-128).into(), 127.into()],
        }),
    ]));
    let profile_id = document.add_object(Stream::new(dictionary! { "N" => 3 }, Vec::new()));
    let icc_id = document.add_object(Object::Array(vec![
        Object::Name(b"ICCBased".to_vec()),
        Object::Reference(profile_id),
    ]));
    let resources_id = document.add_object(dictionary! {
        "ColorSpace" => dictionary! {
            "CSsep" => Object::Reference(separation_id),
            "CSdevn" => Object::Reference(device_n_id),
            "CSlab" => Object::Reference(lab_id),
            "CSicc" => Object::Reference(icc_id),
        },
    });
    let content = Content {
        operations: vec![
            Operation::new("cs", vec![Object::Name(b"CSsep".to_vec())]),
            Operation::new("scn", vec![1.into()]),
            Operation::new("re", vec![0.into(), 0.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
            Operation::new("cs", vec![Object::Name(b"CSdevn".to_vec())]),
            Operation::new("scn", vec![Object::Real(0.2), Object::Real(0.8)]),
            Operation::new("re", vec![40.into(), 0.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
            Operation::new("cs", vec![Object::Name(b"CSlab".to_vec())]),
            Operation::new("scn", vec![50.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![80.into(), 0.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
            Operation::new("cs", vec![Object::Name(b"CSicc".to_vec())]),
            Operation::new("scn", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![120.into(), 0.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_shading_pattern_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let shading_id = document.add_object(dictionary! {
        "ShadingType" => 2,
        "ColorSpace" => "DeviceRGB",
        "Coords" => vec![50.into(), 0.into(), 150.into(), 0.into()],
        "Domain" => vec![0.into(), 1.into()],
        "Function" => dictionary! {
            "FunctionType" => 2,
            "Domain" => vec![0.into(), 1.into()],
            "C0" => vec![1.into(), 0.into(), 0.into()],
            "C1" => vec![0.into(), 0.into(), 1.into()],
            "N" => 1,
        },
        "Extend" => vec![false.into(), false.into()],
        "Background" => vec![0.into(), 1.into(), 0.into()],
        "BBox" => vec![25.into(), 20.into(), 175.into(), 80.into()],
    });
    let pattern_id = document.add_object(dictionary! {
        "Type" => "Pattern",
        "PatternType" => 2,
        "Shading" => Object::Reference(shading_id),
        "Matrix" => vec![1.into(), Object::Real(0.1), Object::Real(0.2), Object::Real(0.8), 0.into(), 10.into()],
    });
    let resources_id = document.add_object(dictionary! {
        "Pattern" => dictionary! { "P1" => Object::Reference(pattern_id) },
        "ExtGState" => dictionary! { "GS1" => dictionary! { "ca" => Object::Real(0.75) } },
    });
    let content = Content {
        operations: vec![
            Operation::new("gs", vec![Object::Name(b"GS1".to_vec())]),
            Operation::new("cs", vec![Object::Name(b"Pattern".to_vec())]),
            Operation::new("scn", vec![Object::Name(b"P1".to_vec())]),
            Operation::new("re", vec![10.into(), 10.into(), 180.into(), 80.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_tiling_pattern_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let colored_content = Content {
        operations: vec![
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![0.into(), 0.into(), 5.into(), 5.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let colored_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "Pattern",
            "PatternType" => 1,
            "PaintType" => 1,
            "TilingType" => 1,
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "XStep" => 10,
            "YStep" => 10,
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()],
            "Resources" => dictionary! {},
        },
        colored_content.encode().unwrap(),
    ));
    let uncolored_content = Content {
        operations: vec![
            Operation::new("re", vec![0.into(), 0.into(), 5.into(), 10.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let uncolored_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "Pattern",
            "PatternType" => 1,
            "PaintType" => 2,
            "TilingType" => 1,
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "XStep" => 10,
            "YStep" => 10,
            "Resources" => dictionary! {},
        },
        uncolored_content.encode().unwrap(),
    ));
    let resources_id = document.add_object(dictionary! {
        "Pattern" => dictionary! {
            "P1" => Object::Reference(colored_id),
            "P2" => Object::Reference(uncolored_id),
        },
        "ColorSpace" => dictionary! {
            "PCS" => Object::Array(vec![
                Object::Name(b"Pattern".to_vec()),
                Object::Name(b"DeviceRGB".to_vec()),
            ]),
        },
    });
    let content = Content {
        operations: vec![
            Operation::new("cs", vec![Object::Name(b"Pattern".to_vec())]),
            Operation::new("scn", vec![Object::Name(b"P1".to_vec())]),
            Operation::new("re", vec![10.into(), 10.into(), 80.into(), 80.into()]),
            Operation::new("f", vec![]),
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    1.into(),
                    Object::Real(0.2),
                    0.into(),
                    1.into(),
                    280.into(),
                    0.into(),
                ],
            ),
            Operation::new("re", vec![0.into(), 10.into(), 60.into(), 60.into()]),
            Operation::new("f", vec![]),
            Operation::new("Q", vec![]),
            Operation::new("cs", vec![Object::Name(b"PCS".to_vec())]),
            Operation::new(
                "scn",
                vec![0.into(), 0.into(), 1.into(), Object::Name(b"P2".to_vec())],
            ),
            Operation::new("re", vec![100.into(), 10.into(), 80.into(), 80.into()]),
            Operation::new("f", vec![]),
            Operation::new(
                "scn",
                vec![0.into(), 1.into(), 0.into(), Object::Name(b"P2".to_vec())],
            ),
            Operation::new("re", vec![190.into(), 10.into(), 80.into(), 80.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn save_single_page_pdf(
    document: &mut Document,
    path: &Path,
    resources_id: (u32, u16),
    content: Content,
) {
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    document.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "Contents" => Object::Reference(content_id),
            "Resources" => Object::Reference(resources_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    document.trailer.set("Root", Object::Reference(catalog_id));
    document.compress();
    document.save(path).unwrap();
}

#[test]
fn decodes_pdf_jpx_images_and_uses_codestream_precision() {
    fn create_pdf(input: &Path, jpx: &[u8], dictionary_bits_per_component: i64) {
        let mut document = Document::with_version("1.7");
        let image_id = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 4,
                "Height" => 4,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => dictionary_bits_per_component,
                "Filter" => "JPXDecode",
            },
            jpx.to_vec(),
        ));
        let resources_id = document.add_object(dictionary! {
            "XObject" => dictionary! { "JPX" => Object::Reference(image_id) },
        });
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        120.into(),
                        0.into(),
                        0.into(),
                        120.into(),
                        50.into(),
                        50.into(),
                    ],
                ),
                Operation::new("Do", vec![Object::Name(b"JPX".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        };
        save_single_page_pdf(&mut document, input, resources_id, content);
    }

    let temporary = TempDir::new().unwrap();
    for (name, jpx, dictionary_bits_per_component) in [
        (
            "raw",
            include_bytes!("fixtures/sample_jpeg2000.j2k").as_slice(),
            8,
        ),
        (
            "boxed",
            include_bytes!("fixtures/sample_jpeg2000.jp2").as_slice(),
            8,
        ),
        (
            "dictionary-bpc-ignored",
            include_bytes!("fixtures/sample_jpeg2000.jp2").as_slice(),
            1,
        ),
    ] {
        let input = temporary.path().join(format!("{name}-jpx.pdf"));
        let output = temporary.path().join(format!("{name}-jpx-out"));
        create_pdf(&input, jpx, dictionary_bits_per_component);
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, SourceFormat::Pdf);
        assert_eq!(report.page_count, 1);
        assert!(report.warnings.is_empty(), "{name}: {:?}", report.warnings);
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        assert!(svg.contains("data:image/png;base64,"), "{name}");
        assert!(!svg.contains("data:image/jp2"), "{name}");
        let encoded = svg
            .split_once("href=\"data:image/png;base64,")
            .unwrap()
            .1
            .split_once('"')
            .unwrap()
            .0;
        let png_bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (4, 4));
        assert_eq!(info.color_type, png::ColorType::Grayscale);
        assert_eq!(
            &pixels[..info.buffer_size()],
            &[
                0, 32, 64, 96, 32, 64, 96, 128, 64, 96, 128, 160, 96, 128, 160, 255
            ]
        );
    }

    let mut oversized_codestream = include_bytes!("fixtures/sample_jpeg2000.j2k").to_vec();
    let siz_marker = oversized_codestream
        .windows(2)
        .position(|window| window == [0xff, 0x51])
        .unwrap();
    oversized_codestream[siz_marker + 6..siz_marker + 10].copy_from_slice(&8192u32.to_be_bytes());
    oversized_codestream[siz_marker + 10..siz_marker + 14].copy_from_slice(&8192u32.to_be_bytes());
    let input = temporary.path().join("oversized-jpx.pdf");
    let output = temporary.path().join("oversized-jpx-out");
    create_pdf(&input, &oversized_codestream, 8);
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("JPX image is 8192x8192"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(!svg.contains("<image"));
}

#[test]
fn decodes_pdf_jpx_rgb_and_sixteen_bit_grayscale_to_png() {
    fn converted_pixels(jpx: &[u8], color_space: &[u8], bits: i64) -> (png::ColorType, Vec<u8>) {
        let temporary = TempDir::new().unwrap();
        let input = temporary.path().join("jpx-image.pdf");
        let output = temporary.path().join("out");
        let mut document = Document::with_version("1.7");
        let image_id = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 4,
                "Height" => 4,
                "ColorSpace" => Object::Name(color_space.to_vec()),
                "BitsPerComponent" => bits,
                "Filter" => "JPXDecode",
            },
            jpx.to_vec(),
        ));
        let resources_id = document.add_object(dictionary! {
            "XObject" => dictionary! { "JPX" => Object::Reference(image_id) },
        });
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        120.into(),
                        0.into(),
                        0.into(),
                        120.into(),
                        50.into(),
                        50.into(),
                    ],
                ),
                Operation::new("Do", vec![Object::Name(b"JPX".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        };
        save_single_page_pdf(&mut document, &input, resources_id, content);
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        let encoded = svg
            .split_once("href=\"data:image/png;base64,")
            .unwrap()
            .1
            .split_once('"')
            .unwrap()
            .0;
        let png_bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (4, 4));
        (info.color_type, pixels[..info.buffer_size()].to_vec())
    }

    let (color_type, rgb_pixels) = converted_pixels(
        include_bytes!("fixtures/sample_jpeg2000_rgb.jp2"),
        b"DeviceRGB",
        8,
    );
    assert_eq!(color_type, png::ColorType::Rgb);
    let expected_rgb = (0..4)
        .flat_map(|y| {
            let colors = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
            (0..4).flat_map(move |x| colors[(x + y) % colors.len()])
        })
        .collect::<Vec<_>>();
    assert_eq!(rgb_pixels, expected_rgb);

    let (color_type, gray_pixels) = converted_pixels(
        include_bytes!("fixtures/sample_jpeg2000_gray16.jp2"),
        b"DeviceGray",
        8,
    );
    assert_eq!(color_type, png::ColorType::Grayscale);
    assert_eq!(
        gray_pixels,
        [
            0, 16, 32, 64, 96, 128, 143, 159, 175, 191, 207, 223, 239, 247, 251, 255
        ]
    );
}

#[test]
fn honors_pdf_jpx_embedded_alpha_and_warns_when_mode_2_has_no_matte() {
    fn convert_jpx(smask_in_data: Option<i64>, encoded: &[u8]) -> (Vec<String>, String) {
        let temporary = TempDir::new().unwrap();
        let input = temporary.path().join("jpx-alpha.pdf");
        let output = temporary.path().join("out");
        let mut document = Document::with_version("1.7");
        let mut image_dictionary = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 4,
            "Height" => 4,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "JPXDecode",
        };
        if let Some(mode) = smask_in_data {
            image_dictionary.set("SMaskInData", mode);
        }
        let image_id = document.add_object(Stream::new(image_dictionary, encoded.to_vec()));
        let resources_id = document.add_object(dictionary! {
            "XObject" => dictionary! { "JPX" => Object::Reference(image_id) },
        });
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        120.into(),
                        0.into(),
                        0.into(),
                        120.into(),
                        50.into(),
                        50.into(),
                    ],
                ),
                Operation::new("Do", vec![Object::Name(b"JPX".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        };
        save_single_page_pdf(&mut document, &input, resources_id, content);
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        (
            report.pages[0].warnings.clone(),
            fs::read_to_string(output.join("page-0001.svg")).unwrap(),
        )
    }

    fn embedded_png(svg: &str) -> (png::ColorType, Vec<u8>) {
        let encoded = svg
            .split_once("href=\"data:image/png;base64,")
            .unwrap()
            .1
            .split_once('"')
            .unwrap()
            .0;
        let png_bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (4, 4));
        (info.color_type, pixels[..info.buffer_size()].to_vec())
    }

    let colors = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
    let expected_rgba = (0..4)
        .flat_map(move |y| {
            (0..4).flat_map(move |x| {
                let mut pixel = colors[(x + y) % colors.len()].to_vec();
                pixel.push([255, 128, 0, 64][(x + y) % 4]);
                pixel
            })
        })
        .collect::<Vec<_>>();
    let boxed_alpha = include_bytes!("fixtures/sample_jpeg2000_rgba.jp2");
    let raw_alpha = include_bytes!("fixtures/sample_jpeg2000_rgba.j2k");
    let (warnings, svg) = convert_jpx(Some(1), boxed_alpha);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!svg.contains("data:image/jp2"));
    assert_eq!(
        embedded_png(&svg),
        (png::ColorType::Rgba, expected_rgba.clone())
    );
    let (warnings, raw_svg) = convert_jpx(Some(1), raw_alpha);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        embedded_png(&raw_svg),
        (png::ColorType::Rgba, expected_rgba)
    );

    // Missing /SMaskInData defaults to 0: discard the JPX alpha channel.
    let (warnings, svg) = convert_jpx(None, boxed_alpha);
    assert!(warnings.is_empty(), "{warnings:?}");
    let expected_rgb = (0..4)
        .flat_map(move |y| (0..4).flat_map(move |x| colors[(x + y) % colors.len()]))
        .collect::<Vec<_>>();
    assert_eq!(embedded_png(&svg), (png::ColorType::Rgb, expected_rgb));

    let (warnings, svg) = convert_jpx(Some(2), boxed_alpha);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("JPX image") && warning.contains("alpha")),
        "{warnings:?}"
    );
    assert!(svg.contains("data:image/jp2;base64,"));
    assert!(!svg.contains("data:image/png;base64,"));

    let (warnings, svg) = convert_jpx(Some(2), raw_alpha);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("JPX image") && warning.contains("alpha")),
        "{warnings:?}"
    );
    assert!(svg.contains("data:image/j2c;base64,"));
}

#[test]
fn unblends_jpx_smask_in_data_2_when_the_matte_is_supported() {
    let temporary = TempDir::new().unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_jpx_smask_in_data_2.pdf");
    let output = temporary.path().join("out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0;
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!((info.width, info.height), (4, 4));
    let colors = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
    let alpha_values = [255, 128, 0, 64];
    let expected = (0..4)
        .flat_map(|y| {
            (0..4).flat_map(move |x| {
                let index = (x + y) % 4;
                let mut pixel = colors[index].to_vec();
                if alpha_values[index] == 0 {
                    pixel.fill(0);
                }
                pixel.push(alpha_values[index]);
                pixel
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(&pixels[..info.buffer_size()], expected);
}

#[test]
fn unblends_jpx_smask_in_data_2_in_calibrated_rgb_components() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("jpx-matte-calrgb.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let cal_rgb = Object::Array(vec![
        Object::Name(b"CalRGB".to_vec()),
        Object::Dictionary(dictionary! {
            "WhitePoint" => vec![0.9505.into(), 1.0.into(), 1.0890.into()],
            "Gamma" => vec![1.0.into(), 1.0.into(), 1.0.into()],
            "Matrix" => vec![1.0.into(), 0.0.into(), 0.0.into(), 0.0.into(), 1.0.into(), 0.0.into(), 0.0.into(), 0.0.into(), 1.0.into()],
        }),
    ]);
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 4,
            "Height" => 4,
            "ColorSpace" => cal_rgb,
            "BitsPerComponent" => 8,
            "Filter" => "JPXDecode",
            "SMaskInData" => 2,
            "Matte" => vec![0.into(), 0.into(), 1.into()],
        },
        include_bytes!("fixtures/sample_jpeg2000_rgba_preblended.jp2").to_vec(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "JPX" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"JPX".to_vec())])],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0;
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!((info.width, info.height), (4, 4));
    let alpha_values = (0..4)
        .flat_map(|y| (0..4).map(move |x| [255, 128, 0, 64][(x + y) % 4]))
        .collect::<Vec<_>>();
    assert_eq!(
        (0..16)
            .map(|pixel| pixels[pixel * 4 + 3])
            .collect::<Vec<_>>(),
        alpha_values
    );
}

#[test]
fn applies_a_bounded_external_soft_mask_to_a_jpx_image() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("jpx-soft-mask.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let mask_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 2,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0, 64, 128, 255],
    ));
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 4,
            "Height" => 4,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "JPXDecode",
            "SMask" => Object::Reference(mask_id),
        },
        include_bytes!("fixtures/sample_jpeg2000_rgb.jp2").to_vec(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "JPX" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    120.into(),
                    0.into(),
                    0.into(),
                    120.into(),
                    40.into(),
                    40.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"JPX".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0;
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    let pixels = &pixels[..info.buffer_size()];
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!((info.width, info.height), (4, 4));
    assert_eq!(&pixels[0..3], &[255, 0, 0]);
    assert_eq!(pixels[3], 0);
    assert_eq!(&pixels[8..11], &[0, 0, 255]);
    assert_eq!(pixels[11], 64);
    assert_eq!(pixels[63], 255);
}

#[test]
fn keeps_jpx_with_matte_soft_mask_as_a_warned_unsupported_image() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("jpx-matte-soft-mask.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let mask_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 2,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
            "Matte" => vec![0.0.into(), 0.0.into(), 0.0.into()],
        },
        vec![0, 64, 128, 255],
    ));
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 4,
            "Height" => 4,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "JPXDecode",
            "SMask" => Object::Reference(mask_id),
        },
        include_bytes!("fixtures/sample_jpeg2000_rgb.jp2").to_vec(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "JPX" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"JPX".to_vec())])],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("PDF-level masks may not be preserved")),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/jp2;base64,"));
    assert!(!svg.contains("data:image/png;base64,"));
}

/// A 600 DPI bilevel page scan is one byte per pixel once unpacked, so the
/// expansion budget must not charge it four. Before this was fixed an ordinary
/// scanned A4 page failed the whole document with an "RGBA limit" error.
#[test]
fn accepts_grayscale_images_that_would_exceed_the_rgba_budget() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("large-gray-scan.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    // 4910 x 6962 is a real 600 DPI A4 scan: 34.2 M pixels, which is 137 MB as
    // RGBA (over the 128 MiB default) but 34 MB as the grayscale it decodes to.
    let width = 4910usize;
    let height = 6962usize;
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width as i64,
            "Height" => height as i64,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0x80; width * height],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Scan" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    612.into(),
                    0.into(),
                    0.into(),
                    792.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Scan".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert_eq!(report.pages[0].node_count, 1, "{:?}", report.warnings);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

/// JBIG2 is common in scanned manuals and has no decoder here. Naming the
/// filter beats relaying the decoder's "missing feature" text, and skipping
/// one image beats discarding the other 35 pages.
#[test]
fn skips_jbig2_images_by_name_without_failing_the_document() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("jbig2.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 16,
            "Height" => 16,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 1,
            "Filter" => "JBIG2Decode",
        },
        vec![0u8; 32],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Scan" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new("0 0 1 rg", vec![]),
            Operation::new("re", vec![0.into(), 0.into(), 10.into(), 10.into()]),
            Operation::new("f", vec![]),
            Operation::new("Do", vec![Object::Name(b"Scan".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning
                .contains("uses the unsupported JBIG2Decode filter and was skipped")),
        "{:?}",
        report.warnings
    );
}

/// A page that consumed real content operators but drew nothing looks exactly
/// like a genuinely empty page in the SVG. Say which one it was.
#[test]
fn warns_when_a_page_consumes_content_but_draws_nothing() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("silently-blank.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let resources_id = document.add_object(dictionary! {});
    // Referring to an XObject that no resource dictionary defines leaves the
    // page with operators to run and nothing to show for them.
    let content = Content {
        operations: vec![Operation::new(
            "Do",
            vec![Object::Name(b"Missing".to_vec())],
        )],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.pages[0].node_count, 0);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("drew nothing from")),
        "{:?}",
        report.warnings
    );
}

/// A page whose content stream is only whitespace is legitimately empty and
/// must not be reported as a conversion problem.
#[test]
fn does_not_warn_about_a_genuinely_empty_page() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("empty-page.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let resources_id = document.add_object(dictionary! {});
    save_single_page_pdf(
        &mut document,
        &input,
        resources_id,
        Content { operations: vec![] },
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.pages[0].node_count, 0);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("drew nothing from")),
        "{:?}",
        report.warnings
    );
}

/// Slide-less decks are a real export failure mode. "input contains no
/// renderable pages" does not tell the user which part of their file is empty.
#[test]
fn names_the_empty_slide_list_in_a_slideless_pptx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("no-slides.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst/><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#,
            ),
            ("ppt/_rels/presentation.xml.rels", r#"<Relationships/>"#),
        ],
    );

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(
        error.to_string().contains("PPTX declares no slides"),
        "{error}"
    );
}

/// The XLSX counterpart of [`names_the_empty_slide_list_in_a_slideless_pptx`].
#[test]
fn names_the_empty_sheet_list_in_a_sheetless_xlsx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("no-sheets.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="w" xmlns:r="r"><sheets/></workbook>"#,
            ),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships/>"#),
        ],
    );

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(
        error.to_string().contains("XLSX declares no worksheets"),
        "{error}"
    );
}

/// PowerPoint stores `<#>` as the cached text of a slide-number field and
/// substitutes the real number when it renders. Emitting the cached text puts a
/// literal `<#>` on every slide of the roughly one deck in three that numbers
/// its slides.
#[test]
fn resolves_pptx_slide_number_fields_to_the_page_number() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("slide-numbers.pptx");
    let output = temporary.path().join("out");
    let slide = r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Number"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="6000000" y="6000000"/><a:ext cx="800000" cy="300000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:fld id="{X}" type="slidenum"><a:rPr sz="1200"/><a:t>&#8249;#&#8250;</a:t></a:fld></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/></Relationships>"#,
            ),
            ("ppt/slides/slide1.xml", slide),
            ("ppt/slides/slide2.xml", slide),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains(">1</tspan>"), "{first}");
    assert!(second.contains(">2</tspan>"), "{second}");
    assert!(
        !first.contains('\u{2039}') && !first.contains('\u{203a}'),
        "cached placeholder text leaked: {first}"
    );
}

/// Word keeps whatever name a picture arrived with, so `media/image1.png`
/// holding JPEG bytes is ordinary. Declaring the wrong type in the data URI
/// makes strict SVG renderers drop the image and the document silently loses
/// its figures.
#[test]
fn labels_docx_images_from_their_signature_not_their_file_name() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mislabelled-image.docx");
    let output = temporary.path().join("out");
    // A minimal but real JPEG: SOI, APP0/JFIF, EOI.
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
    jpeg.extend_from_slice(b"JFIF\0");
    jpeg.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    jpeg.extend_from_slice(&[0xff, 0xd9]);
    make_zip_bytes(
        &input,
        vec![
            (
                "word/document.xml",
                br#"<w:document xmlns:w="w" xmlns:r="r" xmlns:a="a" xmlns:wp="wp"><w:body><w:p><w:r><w:drawing><wp:inline><wp:extent cx="1270000" cy="1270000"/><a:graphic><a:graphicData><pic:pic xmlns:pic="pic"><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>"#.to_vec(),
            ),
            (
                "word/_rels/document.xml.rels",
                br#"<Relationships><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("word/media/image1.png", jpeg),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/jpeg;base64,"), "{svg}");
    assert!(!svg.contains("data:image/png;base64,"), "{svg}");
}

/// `fldCharType="separate"` is optional. A `PAGE` field Word has never
/// calculated runs begin -> instrText -> end, and skipping it leaves the footer
/// without its page number.
#[test]
fn materializes_a_docx_page_field_that_has_no_separate_marker() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("page-field.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Body</w:t></w:r></w:p><w:p><w:r><w:br w:type="page"/></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p><w:sectPr><w:footerReference w:type="default" r:id="rId5" xmlns:r="r"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#,
            ),
            (
                "word/footer1.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/><w:instrText xml:space="preserve"> PAGE </w:instrText><w:fldChar w:fldCharType="end"/></w:r></w:p></w:ftr>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.page_count >= 2, "{}", report.page_count);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("Page 1"), "{first}");
    assert!(second.contains("Page 2"), "{second}");
}

/// quick-xml reports `&amp;` as `Event::GeneralRef`, not as text. A parser that
/// only handles `Event::Text` drops every ampersand, so "R&D" reached the SVG
/// as "RD". This held for PPTX shape text, PPTX table text, DOCX body text and
/// chart labels; 12.7% of the documents in the review corpus were affected.
#[test]
fn keeps_xml_entities_in_pptx_shape_and_table_text() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("entities.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="T"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="5000000" cy="1000000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:rPr sz="2400"/><a:t>R&amp;D &#183; Q&amp;A</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The SVG re-escapes the ampersand, so the round trip is `&amp;`.
    assert!(svg.contains("R&amp;D \u{b7} Q&amp;A"), "{svg}");
}

/// The DOCX half of [`keeps_xml_entities_in_pptx_shape_and_table_text`].
#[test]
fn keeps_xml_entities_in_docx_text() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("entities.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t xml:space="preserve">Research &amp; Development &#8212; Q&amp;A</w:t></w:r></w:p></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("Research &amp; Development \u{2014} Q&amp;A"),
        "{svg}"
    );
}

/// `a14:hiddenFill` and `a14:hiddenLine` hold the colours Word would restore if
/// the shape's real `noFill` were removed. Reading them as the shape's own paint
/// filled every such text box with `hiddenLine`'s black, hiding its text.
#[test]
fn ignores_compatibility_paint_on_docx_text_boxes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("hidden-fill.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w" xmlns:a="a" xmlns:wps="wps" xmlns:wp="wp" xmlns:a14="a14"><w:body><w:p><w:r><w:drawing><wp:anchor><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="3630295" cy="361950"/><a:graphic><a:graphicData><wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="3630295" cy="361950"/></a:xfrm><a:prstGeom prst="rect"/><a:noFill/><a:ln><a:noFill/></a:ln><a:extLst><a:ext uri="{909E8E84}"><a14:hiddenFill><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a14:hiddenFill></a:ext><a:ext uri="{91240B29}"><a14:hiddenLine w="9525"><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a14:hiddenLine></a:ext></a:extLst></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>Visible caption</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Visible caption"), "{svg}");
    let box_path = svg
        .lines()
        .find(|line| line.contains("docx-floating-text-box"))
        .unwrap_or_default();
    assert!(
        box_path.contains("fill=\"none\""),
        "compatibility paint became the shape fill: {box_path}"
    );
}

/// Word's default text box is `noAutofit`: text longer than the frame spills
/// out rather than vanishing. Cutting the layout off at the declared height
/// dropped every paragraph after the first in a short box.
#[test]
fn keeps_docx_text_box_paragraphs_that_overflow_the_frame() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("overflowing-box.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w" xmlns:a="a" xmlns:wps="wps" xmlns:wp="wp"><w:body><w:p><w:r><w:drawing><wp:anchor><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="3630295" cy="180000"/><a:graphic><a:graphicData><wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="3630295" cy="180000"/></a:xfrm><a:prstGeom prst="rect"/><a:noFill/><a:noAutofit/></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>First line of the box</w:t></w:r></w:p><w:p><w:r><w:t>Second line of the box</w:t></w:r></w:p><w:p><w:r><w:t>Third line of the box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    for line in [
        "First line of the box",
        "Second line of the box",
        "Third line of the box",
    ] {
        assert!(svg.contains(line), "missing {line:?} in {svg}");
    }
}

/// A Flate image written with a PNG predictor decoded to noise: `/DecodeParms`
/// given as an indirect reference was not read at all, and the Average row
/// filter reconstructs `(left + above) / 2`, not `left + above / 2`. Both are
/// silent — the page simply comes out as static.
#[test]
fn undoes_png_predictors_including_the_average_row_filter() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("predicted-image.pdf");
    let output = temporary.path().join("out");
    let width = 4usize;
    let height = 4usize;
    let bpp = 3usize;
    let stride = width * bpp;

    // A gradient whose Average rows differ under the two reconstructions.
    let mut expected = vec![0u8; stride * height];
    for y in 0..height {
        for x in 0..width {
            for c in 0..bpp {
                expected[y * stride + x * bpp + c] = ((x * 40 + y * 30 + c * 20) % 256) as u8;
            }
        }
    }
    // Filter each row: None, Sub, Up, then Average.
    let mut filtered = Vec::new();
    for y in 0..height {
        let row = &expected[y * stride..(y + 1) * stride];
        let previous: &[u8] = if y == 0 {
            &[0u8; 12]
        } else {
            &expected[(y - 1) * stride..y * stride]
        };
        let filter = y as u8; // 0 None, 1 Sub, 2 Up, 3 Average
        filtered.push(filter);
        for i in 0..stride {
            let left = if i >= bpp { row[i - bpp] } else { 0 };
            let above = previous[i];
            let predicted = match filter {
                1 => left,
                2 => above,
                3 => ((u16::from(left) + u16::from(above)) / 2) as u8,
                _ => 0,
            };
            filtered.push(row[i].wrapping_sub(predicted));
        }
    }

    let mut document = Document::with_version("1.7");
    let parameters = document.add_object(dictionary! {
        "Predictor" => 15,
        "Colors" => 3,
        "BitsPerComponent" => 8,
        "Columns" => width as i64,
    });
    let mut image = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width as i64,
            "Height" => height as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            // Referenced, not inline: the form that was being ignored.
            "DecodeParms" => Object::Reference(parameters),
        },
        filtered,
    );
    image.compress().unwrap();
    let image_id = document.add_object(image);
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Predicted" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    100.into(),
                    0.into(),
                    0.into(),
                    100.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Predicted".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.pages[0].node_count, 1, "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("base64,")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("image data URI");
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut samples = vec![0u8; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut samples).unwrap();
    assert_eq!(&samples[..info.buffer_size()], &expected[..]);
}

/// `<p:bg>` accepts the same fill grammar as a shape, but the background was
/// tracked as a single colour, so a gradient collapsed to whichever stop the
/// parser read last. A title slide meant to fade from bright to dark came out
/// flat in the dark stop. 6% of the decks in the review corpus use one.
#[test]
fn renders_a_pptx_gradient_slide_background_as_a_gradient() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("gradient-background.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:bg><p:bgPr><a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="FFC000"/></a:gs><a:gs pos="100000"><a:srgbClr val="C00000"/></a:gs></a:gsLst><a:lin ang="2700000"/></a:gradFill></p:bgPr></p:bg><p:spTree/></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<linearGradient"), "{svg}");
    // Both stops must survive, not just the last one read.
    assert!(svg.contains("#FFC000"), "{svg}");
    assert!(svg.contains("#C00000"), "{svg}");
}

/// `showMasterSp="0"` on a layout means the master's own shapes are not drawn.
/// Ignoring it doubles anything a template repeats on both the master and the
/// layout — a footer, a logo. 27 of the 259 decks in the review corpus set it.
#[test]
fn honours_show_master_sp_when_a_layout_hides_master_shapes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("hidden-master.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Body"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="4000000" cy="900000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:t>Slide body</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                r#"<p:sldLayout xmlns:p="p" xmlns:a="a" showMasterSp="0"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="LayoutFooter"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="5000000"/><a:ext cx="4000000" cy="400000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:t>Repeated footer</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                r#"<p:sldMaster xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="3" name="MasterFooter"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="5000000"/><a:ext cx="4000000" cy="400000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:t>Repeated footer</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sldMaster>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Slide body"), "{svg}");
    assert_eq!(
        svg.matches("Repeated footer").count(),
        1,
        "the master copy should be suppressed: {svg}"
    );
}

/// `<a:tr h="...">` is a minimum row height: PowerPoint grows a row when its
/// cells wrap to more lines than fit. Treating it as final clipped the second
/// line off every wrapped cell, cutting the text mid-glyph.
#[test]
fn grows_pptx_table_rows_to_fit_wrapped_cell_text() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("wrapped-table.pptx");
    let output = temporary.path().join("out");
    // One narrow column, a short declared row height, and text that needs
    // several lines at that width.
    let slide = r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Grid"/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="2000000" cy="400000"/></p:xfrm><a:graphic><a:graphicData><a:tbl><a:tblGrid><a:gridCol w="2000000"/></a:tblGrid><a:tr h="200000"><a:tc><a:txBody><a:p><a:r><a:rPr sz="1200"/><a:t>alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#;
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            ("ppt/slides/slide1.xml", slide),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Every wrapped word survives, and the last line sits below the declared
    // 200000 EMU (about 15.7pt) row height rather than being clipped away.
    for word in ["alpha", "juliet", "kilo", "lima"] {
        assert!(svg.contains(word), "missing {word:?} in {svg}");
    }
    let last_baseline = svg
        .split("<text")
        .filter_map(|chunk| chunk.split("y=\"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .filter_map(|value| value.parse::<f64>().ok())
        .fold(0.0_f64, f64::max);
    assert!(
        last_baseline > 15.7,
        "the row did not grow past its declared height: {last_baseline}"
    );
}

/// `Attribute::value` holds raw, still-escaped bytes. Taking them verbatim and
/// then escaping again on the way out turned an alt text of `R&D` into
/// `R&amp;D` on screen. 58 of the 364 Office files in the review corpus carry
/// an entity in a `descr`, `name` or `title` attribute.
#[test]
fn resolves_xml_entities_in_pptx_attribute_values() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("entity-attributes.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Chart" descr="Revenue for R&amp;D and Q&amp;A"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="4000000" cy="1000000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:t>body</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // One level of escaping, so a reader sees "R&D", not "R&amp;D".
    assert!(
        svg.contains(r#"aria-label="Revenue for R&amp;D and Q&amp;A""#),
        "{svg}"
    );
}

/// `/Interpolate false` asks for no smoothing when an image is enlarged. CSS
/// `pixelated` also disables the area averaging every PDF viewer applies when
/// an image is reduced, which speckles a scan placed at a fraction of its pixel
/// size, so it belongs only on images actually drawn larger than their own grid.
#[test]
fn asks_for_pixelated_rendering_only_where_the_image_is_magnified() {
    fn convert_at(scale: i64, directory: &Path) -> String {
        let input = directory.join(format!("image-{scale}.pdf"));
        let output = directory.join(format!("out-{scale}"));
        let mut document = Document::with_version("1.7");
        // A 64x64 image, drawn either much smaller or much larger.
        let image_id = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 64,
                "Height" => 64,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0x40; 64 * 64],
        ));
        let resources_id = document.add_object(dictionary! {
            "XObject" => dictionary! { "Pic" => Object::Reference(image_id) },
        });
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        scale.into(),
                        0.into(),
                        0.into(),
                        scale.into(),
                        0.into(),
                        0.into(),
                    ],
                ),
                Operation::new("Do", vec![Object::Name(b"Pic".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        };
        save_single_page_pdf(&mut document, &input, resources_id, content);
        convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        fs::read_to_string(output.join("page-0001.svg")).unwrap()
    }

    let temporary = TempDir::new().unwrap();
    let reduced = convert_at(16, temporary.path());
    let enlarged = convert_at(400, temporary.path());

    assert!(
        reduced.contains("image-rendering=\"auto\""),
        "a 64px image drawn at 16pt is being reduced: {reduced}"
    );
    assert!(
        enlarged.contains("image-rendering=\"pixelated\""),
        "a 64px image drawn at 400pt is being enlarged: {enlarged}"
    );
}

/// `<c:barDir val="bar"/>` is a horizontal bar chart. It was never read, so a
/// bar chart came out as a column chart — the wrong shape for the data — and
/// the category names, which the parser already collects, were never drawn, so
/// the bars had nothing identifying them.
#[test]
fn draws_a_horizontal_bar_chart_with_its_category_names() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bar-chart.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Chart"/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="5000000" cy="3000000"/></p:xfrm><a:graphic><a:graphicData><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "ppt/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="bar"/><c:ser><c:tx><c:v>Count</c:v></c:tx><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Moulding</c:v></c:pt><c:pt idx="1"><c:v>Assembly</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>47</c:v></c:pt><c:pt idx="1"><c:v>12</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("Moulding"),
        "category names are missing: {svg}"
    );
    assert!(
        svg.contains("Assembly"),
        "category names are missing: {svg}"
    );
    // Horizontal bars share a left edge and differ in length; column bars would
    // instead share a bottom edge and differ in height.
    let bars: Vec<&str> = svg
        .split("data-content-kind=\"chart-bar\"")
        .take(3)
        .collect();
    assert!(bars.len() >= 3, "expected two bars: {svg}");
    let widths: Vec<f64> = svg
        .split("<path")
        .filter(|chunk| chunk.contains("chart-bar"))
        .filter_map(|chunk| chunk.split(" H ").nth(1))
        .filter_map(|rest| rest.split(' ').next())
        .filter_map(|value| value.parse::<f64>().ok())
        .collect();
    assert_eq!(widths.len(), 2, "expected two bar rectangles: {svg}");
    assert!(
        (widths[0] - widths[1]).abs() > 1.0,
        "horizontal bars must differ in length: {widths:?}"
    );
}

/// `<c:grouping val="stacked"/>` puts the series on top of one another, so the
/// category total is what the reader sees. Drawing them side by side instead
/// turns a column of 146 into two columns of 64 and 82 — a different claim
/// about the data.
#[test]
fn stacks_a_stacked_bar_chart_instead_of_clustering_it() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("stacked-chart.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="w" xmlns:r="r"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns="w" xmlns:r="r"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Grid</t></is></c></row></sheetData><drawing r:id="rId2"/></worksheet>"#,
            ),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#,
            ),
            (
                "xl/drawings/drawing1.xml",
                r#"<xdr:wsDr xmlns:xdr="xdr" xmlns:a="a" xmlns:r="r"><xdr:twoCellAnchor><xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>8</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>18</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="1" name="Chart"/></xdr:nvGraphicFramePr><a:graphic><a:graphicData><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId3"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>"#,
            ),
            (
                "xl/drawings/_rels/drawing1.xml.rels",
                r#"<Relationships><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "xl/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/><c:grouping val="stacked"/><c:ser><c:tx><c:v>Inside</c:v></c:tx><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>G1</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>64</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser><c:ser><c:tx><c:v>Outside</c:v></c:tx><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>82</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(report.page_count, 1);
    // Two bars sharing one category slot: the same left edge, stacked so that
    // together they fill the plot height.
    let lefts: Vec<f64> = svg
        .split("<path")
        .filter(|chunk| chunk.contains("chart-bar"))
        .filter_map(|chunk| chunk.split("M ").nth(1))
        .filter_map(|rest| rest.split(' ').next())
        .filter_map(|value| value.parse::<f64>().ok())
        .collect();
    assert_eq!(lefts.len(), 2, "expected two stacked segments: {svg}");
    assert!(
        (lefts[0] - lefts[1]).abs() < 0.01,
        "stacked segments must share a left edge, got {lefts:?}"
    );
}

/// A line chart shares the bar chart's category axis, and its points sit on the
/// plot edges rather than in slots, so its labels belong under the points.
#[test]
fn labels_line_chart_categories_under_their_points() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("line-chart.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Chart"/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="5000000" cy="3000000"/></p:xfrm><a:graphic><a:graphicData><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "ppt/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:lineChart><c:ser><c:tx><c:v>Cost</c:v></c:tx><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Jan</c:v></c:pt><c:pt idx="1"><c:v>Feb</c:v></c:pt><c:pt idx="2"><c:v>Mar</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>3</c:v></c:pt><c:pt idx="1"><c:v>6</c:v></c:pt><c:pt idx="2"><c:v>9</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    for month in ["Jan", "Feb", "Mar"] {
        assert!(svg.contains(month), "missing {month:?} in {svg}");
    }
    // The first and last labels sit on the plot edges, where the line starts
    // and ends, rather than inset by half a slot.
    let label_xs: Vec<f64> = svg
        .split("<text")
        .filter(|chunk| chunk.contains("chart-category-label"))
        .filter_map(|chunk| chunk.split("x=\"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .filter_map(|value| value.parse::<f64>().ok())
        .collect();
    assert_eq!(label_xs.len(), 3, "{svg}");
    let middle = (label_xs[0] + label_xs[2]) / 2.0;
    assert!(
        (label_xs[1] - middle).abs() < 0.5,
        "the middle label should sit midway between the outer two: {label_xs:?}"
    );
}

/// `<a:buChar>` was only read from a master's text styles, so a paragraph that
/// names its own bullet — the usual way a deck writes a bulleted list — lost
/// it. `marL` and `indent` were not read from the paragraph either, so the
/// bullet sat hard against the shape inset instead of hanging beside its text.
#[test]
fn draws_a_pptx_paragraphs_own_bullet_at_its_hanging_indent() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bulleted.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Body"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="5000000" cy="2000000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr lIns="0"/><a:p><a:pPr marL="228600" indent="-228600"><a:buChar char="&#8226;"/></a:pPr><a:r><a:rPr sz="1400"/><a:t>First item</a:t></a:r></a:p><a:p><a:pPr marL="228600" indent="-228600"><a:buNone/></a:pPr><a:r><a:rPr sz="1400"/><a:t>Unbulleted item</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Exactly one bullet: the second paragraph asked for none.
    assert_eq!(
        svg.matches(r#"data-content-kind="bullet""#).count(),
        1,
        "{svg}"
    );
    // marL is 228600 EMU (18pt) and indent cancels it, so the bullet sits at 0
    // and the text at 18pt.
    let bullet_x: f64 = svg
        .split("<text")
        .find(|chunk| chunk.contains(r#"data-content-kind="bullet""#))
        .and_then(|chunk| chunk.split("x=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse().ok())
        .expect("bullet x");
    let text_x: f64 = svg
        .split("<text")
        .find(|chunk| chunk.contains("First item"))
        .and_then(|chunk| chunk.split("x=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse().ok())
        .expect("text x");
    assert!(
        bullet_x < 0.01,
        "bullet should hang at the margin: {bullet_x}"
    );
    assert!(
        (text_x - 18.0).abs() < 0.5,
        "text should start at marL: {text_x}"
    );
}

/// Curved connectors fell through to the bounding-box fallback, drawing a
/// stroked rectangle where a line should curve between two shapes.
#[test]
fn draws_curved_connectors_as_curves_not_boxes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("curved-connector.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="1" name="Curve"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2000000" cy="1000000"/></a:xfrm><a:prstGeom prst="curvedConnector3"><a:avLst/></a:prstGeom><a:ln w="12700"><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></a:ln></p:spPr></p:cxnSp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains(" C "),
        "the connector should be a cubic curve: {svg}"
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("curvedConnector3")),
        "{:?}",
        report.warnings
    );
}

fn drawio_file(path: &Path, body: &str) {
    let mut file = File::create(path).unwrap();
    file.write_all(body.as_bytes()).unwrap();
}

/// draw.io stores a diagram as `encodeURIComponent` output, raw-deflated and
/// base64-encoded. This builds the same thing so the reader is exercised the
/// way the editor writes it.
fn packed_diagram(xml: &str) -> String {
    let mut encoded = String::new();
    for byte in xml.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(byte) {
            encoded.push(char::from(*byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    let mut deflater =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    deflater.write_all(encoded.as_bytes()).unwrap();
    base64::engine::general_purpose::STANDARD.encode(deflater.finish().unwrap())
}

const DRAWIO_FLOW: &str = r##"<mxGraphModel dx="800" dy="600" background="#FAFAFA"><root>
<mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="start" value="&lt;b&gt;Start&lt;/b&gt;&lt;br&gt;here" style="ellipse;whiteSpace=wrap;html=1;fillColor=#D5E8D4;strokeColor=#82B366;" vertex="1" parent="1"><mxGeometry x="40" y="40" width="120" height="60" as="geometry"/></mxCell>
<mxCell id="dec" value="Is it OK?" style="rhombus;whiteSpace=wrap;html=1;" vertex="1" parent="1"><mxGeometry x="280" y="140" width="140" height="90" as="geometry"/></mxCell>
<mxCell id="edge" value="yes" style="edgeStyle=orthogonalEdgeStyle;rounded=0;html=1;" edge="1" parent="1" source="start" target="dec"><mxGeometry relative="1" as="geometry"/></mxCell>
</root></mxGraphModel>"##;

#[test]
fn converts_drawio_shapes_labels_and_edges() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("flow.drawio");
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    drawio_file(
        &input,
        &format!(
            r##"<mxfile host="test"><diagram id="p1" name="Flow">{DRAWIO_FLOW}</diagram></mxfile>"##
        ),
    );

    let report = convert_path(&input, &first, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Drawio);
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(first.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<title>Flow</title>"), "{svg}");
    // Model pixels reach the page as points: the drawing spans 40..420 across
    // and 40..230 down, plus the 10 pixel crop margin on each side.
    assert!(svg.contains(r#"width="300pt" height="157.5pt""#), "{svg}");
    assert!(svg.contains("#D5E8D4"), "{svg}");
    assert!(
        svg.contains(">Start</tspan>") && svg.contains(">here</tspan>"),
        "{svg}"
    );
    assert!(svg.contains("font-weight=\"700\""), "{svg}");
    assert!(svg.contains(">Is it OK?</tspan>"), "{svg}");
    assert!(svg.contains(">yes</tspan>"), "{svg}");
    assert!(
        svg.contains("data-content-kind=\"drawio-edge-marker\""),
        "{svg}"
    );
    convert_path(&input, &second, &ConvertOptions::default()).unwrap();
    assert_eq!(
        svg,
        fs::read_to_string(second.join("page-0001.svg")).unwrap()
    );
}

#[test]
fn expands_compressed_drawio_diagrams_into_one_page_each() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("packed.drawio");
    let output = temporary.path().join("out");
    let second = r##"<mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="note" value="Second &amp; last" style="rounded=1;whiteSpace=wrap;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="200" height="60" as="geometry"/></mxCell>
</root></mxGraphModel>"##;
    drawio_file(
        &input,
        &format!(
            r##"<mxfile host="test"><diagram id="p1" name="Plain">{DRAWIO_FLOW}</diagram><diagram id="p2" name="Packed">{}</diagram></mxfile>"##,
            packed_diagram(second)
        ),
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 2);
    let svg = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(svg.contains("<title>Packed</title>"), "{svg}");
    assert!(svg.contains(">Second &amp; last</tspan>"), "{svg}");
}

#[test]
fn routes_orthogonal_drawio_edges_square_on_to_the_shapes_they_join() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("route.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        &format!(r##"<mxfile><diagram name="Flow">{DRAWIO_FLOW}</diagram></mxfile>"##),
    );

    convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let path = svg
        .lines()
        .find(|line| line.contains(r#"id="drawio-edge""#))
        .unwrap_or_default();
    // The route leaves the ellipse's right side and enters the rhombus's left
    // side, turning halfway between the two shapes rather than at either one.
    assert!(
        path.contains(r#"d="M 160 70 L 220 70 L 220 185 L "#),
        "{path}"
    );
}

#[test]
fn keeps_a_drawio_shape_it_cannot_draw_as_a_labelled_placeholder() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("library.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Icons"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="fn" value="Lambda" style="sketch=0;shape=mxgraph.aws4.lambda_function;fillColor=#F58534;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="78" height="78" as="geometry"/></mxCell>
<mxCell id="fn2" value="Also Lambda" style="shape=mxgraph.aws4.lambda_function;" vertex="1" parent="1"><mxGeometry x="140" y="20" width="78" height="78" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("mxgraph.aws4.lambda_function")
            && report.warnings[0].contains("shape library"),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">Lambda</tspan>"), "{svg}");
    assert!(svg.contains("#F58534"), "{svg}");
}

#[test]
fn previews_generic_xml_that_is_not_a_drawio_document() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("notes.xml");
    let output = temporary.path().join("out");
    drawio_file(&input, "<notes><note>Not a diagram</note></notes>");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Xml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Not a diagram"));
}

#[test]
fn embeds_drawio_pictures_and_refuses_to_fetch_remote_ones() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("pictures.drawio");
    let output = temporary.path().join("out");
    // A 2x2 PNG, stored the way draw.io writes one: no ";base64" marker.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAFElEQVR4nGP8z8Dwn4GBgYmBgYEBAB0EAwFgYUqTAAAAAElFTkSuQmCC";
    drawio_file(
        &input,
        &format!(
            r##"<mxfile><diagram name="Pictures"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="pic" value="Photo" style="shape=image;html=1;image=data:image/png,{png}" vertex="1" parent="1"><mxGeometry x="20" y="20" width="80" height="80" as="geometry"/></mxCell>
<mxCell id="remote" value="Remote" style="shape=image;html=1;image=https://example.com/logo.png" vertex="1" parent="1"><mxGeometry x="20" y="140" width="60" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##
        ),
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("not fetched"),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains(&format!("data:image/png;base64,{png}")),
        "{svg}"
    );
    assert!(!svg.contains("example.com"), "{svg}");
}

#[test]
fn round_trips_a_drawio_file_through_svg_without_losing_the_diagram() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("flow.drawio");
    let pages = temporary.path().join("pages");
    let restored = temporary.path().join("restored.drawio");
    let again = temporary.path().join("again");
    drawio_file(
        &input,
        &format!(
            r##"<mxfile host="test"><diagram id="p1" name="Flow">{DRAWIO_FLOW}</diagram></mxfile>"##
        ),
    );
    let options = ConvertOptions {
        embed_drawio_source: true,
        ..Default::default()
    };

    convert_path(&input, &pages, &options).unwrap();
    let svg = fs::read_to_string(pages.join("page-0001.svg")).unwrap();
    assert!(svg.contains(" content=\"&lt;mxfile"), "{svg}");

    let report =
        document_svg::svg_to_document(&pages, &restored, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Drawio);
    assert!(
        report.warnings[0].contains("restored"),
        "{:?}",
        report.warnings
    );
    let diagram = fs::read_to_string(&restored).unwrap();
    assert!(
        diagram.contains(r#"<diagram id="p1" name="Flow">"#),
        "{diagram}"
    );

    // The restored diagram converts to the same SVG the original did, embedded
    // source and all: a diagram body is written across several lines, and a
    // reader turns a literal newline inside an attribute into a space, so the
    // source only survives the trip if it is escaped as a character reference.
    convert_path(&restored, &again, &options).unwrap();
    assert_eq!(
        svg,
        fs::read_to_string(again.join("page-0001.svg")).unwrap()
    );
    let plain = temporary.path().join("plain");
    convert_path(&input, &plain, &ConvertOptions::default()).unwrap();
    let plain_again = temporary.path().join("plain-again");
    convert_path(&restored, &plain_again, &ConvertOptions::default()).unwrap();
    assert_eq!(
        fs::read_to_string(plain.join("page-0001.svg")).unwrap(),
        fs::read_to_string(plain_again.join("page-0001.svg")).unwrap()
    );
}

/// Inputs that are malformed, hostile or simply strange must come back as an
/// error or a page, never as a panic and never as an unbounded run. Written as
/// a test so it runs in a debug build, where arithmetic overflow aborts.
#[test]
fn survives_malformed_and_hostile_drawio_input() {
    let temporary = TempDir::new().unwrap();
    let cases: [(&str, String); 17] = [
        ("empty", String::new()),
        ("not-xml", "this is not xml at all".into()),
        ("truncated", "<mxfile><diagram><mxGraphModel><root>".into()),
        (
            "cyclic-parents",
            r##"<mxGraphModel><root><mxCell id="a" parent="b" vertex="1"><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell><mxCell id="b" parent="a" vertex="1"><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "self-parent",
            r##"<mxGraphModel><root><mxCell id="a" parent="a" vertex="1"><mxGeometry width="10" height="10" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "self-loop-edge",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1"><mxGeometry width="80" height="40" as="geometry"/></mxCell><mxCell id="e" edge="1" parent="1" source="a" target="a" style="edgeStyle=orthogonalEdgeStyle;"><mxGeometry relative="1" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "edge-to-nothing",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="e" edge="1" parent="1" source="missing" target="gone"><mxGeometry relative="1" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "not-a-number",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1" style="rounded=1;strokeWidth=NaN;opacity=inf;rotation=1e400;"><mxGeometry x="NaN" y="-inf" width="1e400" height="abc" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "enormous-geometry",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1"><mxGeometry x="-1e300" y="-1e300" width="1e308" height="1e308" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "zero-size",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" value="x" parent="1" vertex="1"><mxGeometry width="0" height="0" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "negative-size",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" value="x" parent="1" vertex="1"><mxGeometry width="-40" height="-40" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "duplicate-ids",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1"><mxGeometry width="10" height="10" as="geometry"/></mxCell><mxCell id="a" parent="1" vertex="1"><mxGeometry width="10" height="10" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "unclosed-html-label",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" value="&lt;b&gt;&lt;font color=&quot;#fff&quot;&gt;never closed" style="html=1;whiteSpace=wrap;" parent="1" vertex="1"><mxGeometry width="80" height="40" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "bare-ampersand-and-controls",
            "<mxGraphModel><root><mxCell id=\"1\"/><mxCell id=\"a\" value=\"A &amp; B &amp;#x; &amp;nope; \u{7}\" style=\"html=1;\" parent=\"1\" vertex=\"1\"><mxGeometry width=\"80\" height=\"40\" as=\"geometry\"/></mxCell></root></mxGraphModel>".into(),
        ),
        (
            "deep-groups",
            {
                let mut xml = String::from("<mxGraphModel><root><mxCell id=\"1\"/>");
                for level in 0..400 {
                    let parent = if level == 0 { "1".to_owned() } else { format!("g{}", level - 1) };
                    xml.push_str(&format!(
                        "<mxCell id=\"g{level}\" parent=\"{parent}\" vertex=\"1\" style=\"group\"><mxGeometry x=\"1\" y=\"1\" width=\"50\" height=\"50\" as=\"geometry\"/></mxCell>"
                    ));
                }
                xml.push_str("</root></mxGraphModel>");
                xml
            },
        ),
        (
            "many-waypoints",
            {
                let mut xml = String::from("<mxGraphModel><root><mxCell id=\"1\"/><mxCell id=\"e\" edge=\"1\" parent=\"1\" style=\"edgeStyle=orthogonalEdgeStyle;rounded=1;\"><mxGeometry relative=\"1\" as=\"geometry\"><mxPoint x=\"0\" y=\"0\" as=\"sourcePoint\"/><mxPoint x=\"900\" y=\"900\" as=\"targetPoint\"/><Array as=\"points\">");
                for index in 0..2_000 {
                    xml.push_str(&format!("<mxPoint x=\"{}\" y=\"{}\"/>", index % 97, index % 89));
                }
                xml.push_str("</Array></mxGeometry></mxCell></root></mxGraphModel>");
                xml
            },
        ),
        (
            "image-payload-garbage",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1" style="shape=image;image=data:image/png,!!!not base64!!!"><mxGeometry width="40" height="40" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
    ];
    for (name, body) in cases {
        let input = temporary.path().join(format!("{name}.drawio"));
        let output = temporary.path().join(name);
        fs::write(&input, &body).unwrap();

        // Either outcome is fine; hanging, panicking or overflowing is not.
        match convert_path(&input, &output, &ConvertOptions::default()) {
            Ok(report) => {
                assert!(report.page_count >= 1, "{name}");
                for page in &report.pages {
                    // A page has to stay a page: positive, finite and small
                    // enough that a renderer can open it.
                    assert!(
                        page.width_points.is_finite() && page.width_points > 0.0,
                        "{name}"
                    );
                    assert!(
                        page.height_points.is_finite() && page.height_points > 0.0,
                        "{name}"
                    );
                    assert!(
                        page.width_points <= 1_000_000.0,
                        "{name}: {}",
                        page.width_points
                    );
                    assert!(
                        page.height_points <= 1_000_000.0,
                        "{name}: {}",
                        page.height_points
                    );
                    let svg = fs::read_to_string(output.join(&page.svg)).unwrap();
                    assert!(svg.starts_with("<?xml"), "{name}");
                    assert!(!svg.contains("NaN"), "{name}: {svg:.400}");
                }
            }
            Err(error) => {
                let text = error.to_string();
                assert!(!text.is_empty(), "{name}");
            }
        }
    }
}

#[test]
fn refuses_a_drawio_diagram_that_expands_past_the_entry_limit() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bomb.drawio");
    let output = temporary.path().join("out");
    // 8 MiB of one repeated byte compresses to a few kilobytes.
    let payload = "A".repeat(8 * 1024 * 1024);
    drawio_file(
        &input,
        &format!(
            r##"<mxfile><diagram name="Bomb">{}</diagram></mxfile>"##,
            packed_diagram(&payload)
        ),
    );
    let options = ConvertOptions {
        max_zip_entry_bytes: 64 * 1024,
        ..Default::default()
    };

    let error = convert_path(&input, &output, &options).unwrap_err();

    assert!(error.to_string().contains("expands past"), "{error}");
    assert!(!output.join("page-0001.svg").exists());
}

#[test]
fn refuses_more_drawio_diagrams_than_the_page_limit_allows() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("many.drawio");
    let output = temporary.path().join("out");
    let mut body = String::from("<mxfile>");
    for index in 0..12 {
        body.push_str(&format!(
            r##"<diagram id="p{index}" name="P{index}"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/><mxCell id="a{index}" value="{index}" style="rounded=1;" vertex="1" parent="1"><mxGeometry width="40" height="20" as="geometry"/></mxCell></root></mxGraphModel></diagram>"##
        ));
    }
    body.push_str("</mxfile>");
    drawio_file(&input, &body);
    let options = ConvertOptions {
        max_pages: 4,
        ..Default::default()
    };

    let error = convert_path(&input, &output, &options).unwrap_err();

    assert!(error.to_string().contains("maximum is 4"), "{error}");
}

#[test]
fn refuses_a_drawio_model_that_exceeds_the_xml_event_budget() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("chatty.drawio");
    let output = temporary.path().join("out");
    let mut body =
        String::from(r#"<mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>"#);
    for index in 0..2_000 {
        body.push_str(&format!(
            r##"<mxCell id="c{index}" vertex="1" parent="1"><mxGeometry width="10" height="10" as="geometry"/></mxCell>"##
        ));
    }
    body.push_str("</root></mxGraphModel>");
    drawio_file(&input, &body);
    let options = ConvertOptions {
        max_xml_events: 500,
        ..Default::default()
    };

    let error = convert_path(&input, &output, &options).unwrap_err();

    assert!(error.to_string().contains("events"), "{error}");
}

/// mxGraph looks a style token without `=` up in the stylesheet's named styles
/// and ignores what it does not find. Treating any such token as a shape turned
/// the stray `9E9E9E` left in draw.io's own GCP templates into an unknown shape
/// warning on 54 of the project's example diagrams.
#[test]
fn ignores_a_style_token_that_names_no_shape() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("stray.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Stray"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="stray" value="Card" style="fillColor=#F6F6F6;fontColor=#717171;9E9E9E;verticalAlign=top;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="200" height="60" as="geometry"/></mxCell>
<mxCell id="named" value="Oval" style="ellipse;whiteSpace=wrap;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="120" width="120" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The stray token leaves a plain rectangle; the leading `ellipse` still
    // selects a shape, because that one does name a style draw.io defines.
    assert!(svg.contains("data-semantic-role=\"rectangle\""), "{svg}");
    assert!(svg.contains("data-semantic-role=\"ellipse\""), "{svg}");
}

#[test]
fn draws_the_container_shapes_a_real_diagram_uses() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("containers.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Containers"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="grp" style="group" vertex="1" connectable="0" parent="1"><mxGeometry x="20" y="20" width="200" height="100" as="geometry"/></mxCell>
<mxCell id="inner" value="In group" style="rounded=1;html=1;" vertex="1" parent="grp"><mxGeometry x="10" y="10" width="100" height="40" as="geometry"/></mxCell>
<mxCell id="cell" value="Cell" style="shape=partialRectangle;top=0;left=0;html=1;fillColor=#EEEEEE;" vertex="1" parent="1"><mxGeometry x="20" y="160" width="120" height="40" as="geometry"/></mxCell>
<mxCell id="point" style="shape=waypoint;" vertex="1" parent="1"><mxGeometry x="300" y="160" width="20" height="20" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A group and a waypoint position things without drawing anything.
    assert!(!svg.contains("id=\"drawio-grp\""), "{svg}");
    assert!(!svg.contains("id=\"drawio-point\""), "{svg}");
    // The child is placed relative to the group it sits in: 20 + 10, with the
    // rounded corner the style asks for starting the path 6 further along.
    assert!(svg.contains(r#"d="M 36 30 H 124 A 6 6"#), "{svg}");
    // A partial rectangle fills, and strokes only the sides it switches on.
    assert!(svg.contains("#EEEEEE"), "{svg}");
    assert!(svg.contains("id=\"drawio-cell-detail-0\""), "{svg}");
    assert!(!svg.contains("id=\"drawio-cell-detail-2\""), "{svg}");
}

#[test]
fn says_what_a_drawio_shape_library_is_instead_of_calling_it_empty() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shapes.xml");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r#"<mxlibrary>[{"xml":"...","w":100,"h":100}]</mxlibrary>"#,
    );

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(error.to_string().contains("shape library"), "{error}");
}

/// A shape library is a drawing program per shape, in the shape's own
/// coordinate space. Reading it is what puts the real icon on the page instead
/// of a labelled box.
#[test]
fn draws_a_shape_from_a_library_the_caller_supplies() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    fs::write(
        stencils.join("demo.xml"),
        r##"<shapes name="mxgraph.demo">
<shape name="Round Badge" h="20" w="20" aspect="fixed" strokewidth="inherit">
  <connections/>
  <background><ellipse x="0" y="0" w="20" h="20"/></background>
  <foreground>
    <fillstroke/>
    <fillcolor color="#123456"/>
    <path><move x="4" y="10"/><line x="16" y="10"/><close/></path>
    <fill/>
  </foreground>
</shape>
</shapes>"##,
    )
    .unwrap();
    let input = temporary.path().join("library.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Library"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="badge" value="Badge" style="shape=mxgraph.demo.round_badge;fillColor=#FFEEDD;strokeColor=#884400;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="40" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils],
        ..Default::default()
    };

    let report = convert_path(&input, &output, &options).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("data-content-kind=\"drawio-stencil\""),
        "{svg}"
    );
    // The shape's own 20x20 space is scaled onto the cell's 40x40 box, and the
    // cell's colours are what the stencil paints with.
    assert!(svg.contains("#FFEEDD") && svg.contains("#884400"), "{svg}");
    // The stencil's own colour overrides the cell's for what follows it.
    assert!(svg.contains("#123456"), "{svg}");
    assert!(svg.contains(">Badge</tspan>"), "{svg}");

    // Without the library the same diagram says so, and still shows the label.
    let plain = temporary.path().join("plain");
    let bare = convert_path(&input, &plain, &ConvertOptions::default()).unwrap();
    assert!(
        bare.warnings[0].contains("mxgraph.demo.round_badge"),
        "{:?}",
        bare.warnings
    );
    assert!(
        fs::read_to_string(plain.join("page-0001.svg"))
            .unwrap()
            .contains(">Badge</tspan>")
    );
}

#[test]
fn draws_a_shape_the_diagram_carries_inline() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline.drawio");
    let output = temporary.path().join("out");
    let shape = r#"<shape h="10" w="10" aspect="variable" strokewidth="inherit"><connections/><background><rect x="0" y="0" w="10" h="10"/></background><foreground><fillstroke/></foreground></shape>"#;
    drawio_file(
        &input,
        &format!(
            r##"<mxfile><diagram name="Inline"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="own" value="Mine" style="shape=stencil({});fillColor=#AABBCC;" vertex="1" parent="1"><mxGeometry x="10" y="10" width="60" height="30" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
            packed_diagram(shape)
        ),
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("data-content-kind=\"drawio-stencil\""),
        "{svg}"
    );
    assert!(svg.contains("#AABBCC"), "{svg}");
    // The shape's 10x10 space is stretched over the whole 60x30 cell.
    assert!(svg.contains("M 10 10 H 70 V 40 H 10 Z"), "{svg}");
}

#[test]
fn draws_the_glyph_a_cloud_tile_names_in_its_style() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    fs::write(
        stencils.join("cloud.xml"),
        r##"<shapes name="mxgraph.cloud">
<shape name="Bucket" h="10" w="10" aspect="fixed" strokewidth="inherit">
  <connections/><background><rect x="0" y="0" w="10" h="10"/></background>
  <foreground><fillstroke/></foreground>
</shape>
</shapes>"##,
    )
    .unwrap();
    let input = temporary.path().join("tile.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Tile"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="tile" value="Storage" style="shape=mxgraph.aws4.resourceIcon;resIcon=mxgraph.cloud.bucket;fillColor=#3334B9;strokeColor=#ffffff;" vertex="1" parent="1"><mxGeometry x="0" y="0" width="100" height="100" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils],
        ..Default::default()
    };

    let report = convert_path(&input, &output, &options).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The tile keeps the style's colour, and the glyph sits at seven tenths of
    // it in white.
    assert!(svg.contains("#3334B9"), "{svg}");
    assert!(
        svg.contains("data-content-kind=\"drawio-stencil-icon\""),
        "{svg}"
    );
    assert!(svg.contains("M 15 15 H 85 V 85 H 15 Z"), "{svg}");
}

/// Entity-relationship ends are the notation an ER diagram is read by: a tick
/// for one, a crow's foot for many, and a ring for zero.
#[test]
fn draws_entity_relationship_connector_ends() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("er.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="ER"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="a" value="Order" style="shape=table;startSize=30;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="160" height="90" as="geometry"/></mxCell>
<mxCell id="b" value="Line" style="shape=table;startSize=30;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="20" width="160" height="90" as="geometry"/></mxCell>
<mxCell id="e" style="edgeStyle=entityRelationEdgeStyle;html=1;startArrow=ERmandOne;endArrow=ERzeroToMany;" edge="1" parent="1" source="a" target="b"><mxGeometry relative="1" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Two ticks at the mandatory-one end, and a ring plus a crow's foot at the
    // other. The ring is filled white so the line does not show through it.
    assert_eq!(
        svg.matches("data-content-kind=\"drawio-edge-marker\"")
            .count(),
        3,
        "{svg}"
    );
    assert!(svg.contains("fill=\"#FFFFFF\""), "{svg}");
    // A table is a swimlane: its name sits in the band above the rows.
    assert!(svg.contains("data-semantic-role=\"swimlane\""), "{svg}");
}

#[test]
fn draws_bpmn_events_gateways_and_their_symbols() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bpmn.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="BPMN"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="start" value="Order" style="shape=mxgraph.bpmn.shape;outline=standard;symbol=message;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="gate" value="Split" style="shape=mxgraph.bpmn.shape;background=gateway;outline=none;symbol=parallelGw;html=1;" vertex="1" parent="1"><mxGeometry x="120" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="end" value="Done" style="shape=mxgraph.bpmn.shape;outline=end;symbol=terminate;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="task" value="Pack" style="shape=mxgraph.bpmn.task;rectStyle=rounded;size=10;html=1;" vertex="1" parent="1"><mxGeometry x="320" y="20" width="100" height="50" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"drawio-bpmn\""), "{svg}");
    // A gateway is a diamond with the symbol in its middle half.
    assert!(
        svg.contains("M 145 20 L 170 45 L 145 70 L 120 45 Z"),
        "{svg}"
    );
    // An end event's ring is three times the stroke width, and its terminate
    // symbol takes the line colour because the event throws.
    assert!(svg.contains("stroke-width=\"3\""), "{svg}");
    // The activity keeps the corner radius its own style asks for.
    assert!(svg.contains("A 10 10 0 0 1"), "{svg}");
}

#[test]
fn draws_uml_components_and_packages() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("uml.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="UML"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="c" value="Service" style="shape=component;align=left;spacingLeft=36;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="180" height="90" as="geometry"/></mxCell>
<mxCell id="p" value="Domain" style="shape=folder;tabWidth=80;tabHeight=20;tabPosition=left;labelInHeader=1;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="20" width="200" height="120" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The component keeps its two sockets on the left edge.
    assert_eq!(svg.matches("drawio-c-detail-").count(), 2, "{svg}");
    // The package's name sits in the tab, not in the middle of the box.
    assert!(svg.contains(">Domain</tspan>"), "{svg}");
    let label = svg
        .lines()
        .find(|line| line.contains("drawio-p-label-0"))
        .unwrap();
    let baseline = label
        .split("y=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap();
    // The tab runs from 20 to 40 in the model's own pixels.
    assert!(
        (20.0..40.0).contains(&baseline),
        "the name is in the tab: {label}"
    );
}

/// The shapes draw.io implements in code rather than as stencils still have to
/// be drawn: a ring segment keeps its angles, a mock-up grid keeps its cells,
/// and an AWS 3D service keeps the isometric body it stands on.
#[test]
fn draws_the_library_shapes_that_have_no_stencil() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("coded.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Coded"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="ring" style="shape=mxgraph.basic.partConcEllipse;startAngle=0;endAngle=0.25;arcWidth=0.5;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="80" height="80" as="geometry"/></mxCell>
<mxCell id="grid" style="shape=mxgraph.mockup.graphics.iconGrid;gridSize=2,2;html=1;" vertex="1" parent="1"><mxGeometry x="140" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="ribbon" value="Step" style="shape=mxgraph.infographic.ribbonSimple;notch1=20;notch2=20;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="20" width="160" height="40" as="geometry"/></mxCell>
<mxCell id="box" value="Server" style="shape=mxgraph.aws3d.application_server;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="140" width="100" height="100" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    // Only the AWS service says something, and it says exactly what is missing.
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("service glyph it carries is not drawn"),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A quarter ring, drawn from the top round to the right and back.
    assert!(
        svg.contains("M 60 20 A 40 40 0 0 1 100 60 L 80 60 A 20 20 0 0 0 60 40 Z"),
        "{svg}"
    );
    // Two by two cells, each two thirds of the box with a gap between them.
    assert_eq!(svg.matches("id=\"drawio-grid\"").count(), 1, "{svg}");
    assert!(svg.contains("M 140 20 H 160 V 40 H 140 Z"), "{svg}");
    // The ribbon keeps its notches.
    assert!(
        svg.contains("M 220 60 L 240 40 L 220 20 L 360 20 L 380 40 L 360 60 Z"),
        "{svg}"
    );
    // The isometric body and its two shaded faces.
    assert!(
        svg.contains("data-content-kind=\"drawio-isometric\""),
        "{svg}"
    );
    assert_eq!(
        svg.matches("data-content-kind=\"drawio-isometric-face\"")
            .count(),
        2,
        "{svg}"
    );
}

/// The stencil loader reads files the caller points it at, so it has to hold up
/// against whatever is in that directory: files that are not stencils, shapes
/// with no geometry, and numbers that are not numbers.
#[test]
fn survives_stencil_libraries_that_are_malformed_or_hostile() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    fs::write(stencils.join("not-xml.xml"), "this is not xml at all").unwrap();
    fs::write(stencils.join("empty.xml"), "").unwrap();
    fs::write(stencils.join("notes.txt"), "ignored: not an xml file").unwrap();
    fs::write(
        stencils.join("odd.xml"),
        concat!(
            r##"<shapes name="mxgraph.odd">"##,
            // No geometry at all.
            r##"<shape name="Bare" w="0" h="0"/>"##,
            // Numbers that are not numbers, and a shape that never closes its
            // path before painting.
            r##"<shape name="Broken" w="NaN" h="-1e400" aspect="fixed" strokewidth="wide">"##,
            r##"<foreground><fill/><stroke/><restore/><restore/>"##,
            r##"<path><line x="1e400" y="NaN"/><arc rx="-5" ry="0" x="10" y="10"/></path>"##,
            r##"<fillcolor color="not a colour"/><strokewidth width="1e400"/><alpha alpha="5"/>"##,
            r##"<fillstroke/></foreground></shape>"##,
            // A deeply nested body, which must not be followed into recursion.
            r##"<shape name="Nested" w="10" h="10"><foreground>"##,
            r##"<save/><save/><save/><path><move x="0" y="0"/><line x="10" y="10"/></path><fillstroke/>"##,
            r##"</foreground></shape></shapes>"##,
        ),
    )
    .unwrap();
    let input = temporary.path().join("odd.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Odd"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="a" value="Bare" style="shape=mxgraph.odd.bare;html=1;" vertex="1" parent="1"><mxGeometry x="10" y="10" width="40" height="40" as="geometry"/></mxCell>
<mxCell id="b" value="Broken" style="shape=mxgraph.odd.broken;html=1;" vertex="1" parent="1"><mxGeometry x="70" y="10" width="40" height="40" as="geometry"/></mxCell>
<mxCell id="c" value="Nested" style="shape=mxgraph.odd.nested;html=1;" vertex="1" parent="1"><mxGeometry x="130" y="10" width="40" height="40" as="geometry"/></mxCell>
<mxCell id="d" value="Absent" style="shape=mxgraph.odd.absent;html=1;" vertex="1" parent="1"><mxGeometry x="190" y="10" width="40" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils, temporary.path().join("missing-directory")],
        ..Default::default()
    };

    // A directory that is not there is an error the caller can act on; nothing
    // in the files themselves may panic or run away.
    let error = convert_path(&input, &output, &options).unwrap_err();
    assert!(error.to_string().contains("No such file"), "{error}");

    let options = ConvertOptions {
        stencil_paths: options.stencil_paths[..1].to_vec(),
        ..Default::default()
    };
    let report = convert_path(&input, &output, &options).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(!svg.contains("NaN"), "{svg}");
    // Every label survives, whatever happened to the shape around it.
    for label in ["Bare", "Broken", "Nested", "Absent"] {
        assert!(svg.contains(&format!(">{label}</tspan>")), "{label}: {svg}");
    }
    // The shape that was not in any library is the only one reported.
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("mxgraph.odd.absent"),
        "{:?}",
        report.warnings
    );
}

#[test]
fn refuses_a_stencil_library_that_exceeds_the_event_budget() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    let mut library = String::from(
        r##"<shapes name="mxgraph.big"><shape name="Long" w="10" h="10"><foreground><path>"##,
    );
    for index in 0..4_000 {
        library.push_str(&format!(
            r##"<line x="{}" y="{}"/>"##,
            index % 10,
            index % 7
        ));
    }
    library.push_str("</path><fillstroke/></foreground></shape></shapes>");
    fs::write(stencils.join("big.xml"), library).unwrap();
    let input = temporary.path().join("big.drawio");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Big"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="a" style="shape=mxgraph.big.long;html=1;" vertex="1" parent="1"><mxGeometry width="40" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils],
        max_xml_events: 500,
        ..Default::default()
    };

    let error = convert_path(&input, temporary.path().join("out"), &options).unwrap_err();

    assert!(error.to_string().contains("events"), "{error}");
}

#[test]
fn draws_the_shapes_that_need_more_than_one_colour() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shaded.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Shaded"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="pie" style="shape=mxgraph.mockup.graphics.pieChart;parts=25,25,50;partColors=#FF0000,#00FF00,#0000FF;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="80" height="80" as="geometry"/></mxCell>
<mxCell id="cube" style="shape=mxgraph.infographic.shadedCube;isoAngle=15;html=1;fillColor=#DDDDDD;" vertex="1" parent="1"><mxGeometry x="140" y="20" width="80" height="80" as="geometry"/></mxCell>
<mxCell id="brace" style="shape=mxgraph.mockup.markup.curlyBrace;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="140" width="120" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Three slices, each in the colour the style lists for it.
    for colour in ["#FF0000", "#00FF00", "#0000FF"] {
        assert!(svg.contains(colour), "{colour}: {svg}");
    }
    // The cube's body plus its two shaded faces.
    assert_eq!(
        svg.matches("data-content-kind=\"drawio-shaded\"").count(),
        7,
        "{svg}"
    );
    assert!(svg.contains("fill-opacity=\"0.2\""), "{svg}");
    // The brace is a line, so it carries no fill of its own.
    let brace = svg
        .lines()
        .find(|line| line.contains("drawio-brace-detail-0"))
        .unwrap();
    assert!(brace.contains("fill=\"none\""), "{brace}");
}

/// A connector has to meet the outline it points at, not the box around it.
#[test]
fn attaches_connectors_to_the_outline_the_perimeter_names() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("perimeter.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Perimeter"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="tri" style="triangle;perimeter=trianglePerimeter;html=1;" vertex="1" parent="1"><mxGeometry x="0" y="0" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="far" style="rounded=0;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="40" width="40" height="20" as="geometry"/></mxCell>
<mxCell id="e" style="html=1;" edge="1" parent="1" source="tri" target="far"><mxGeometry relative="1" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let edge = svg
        .lines()
        .find(|line| line.contains(r#"id="drawio-e""#))
        .unwrap();
    // The triangle points right, so the line starts at its tip, not at the
    // right edge of its bounding box at the centre height.
    assert!(edge.contains("M 100 50 L"), "{edge}");
}

#[test]
fn keeps_a_hidden_overflow_label_inside_its_shape() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("overflow.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Overflow"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="clipped" value="A label far longer than the box it belongs to" style="rounded=0;html=1;overflow=hidden;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="60" height="30" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("<clipPath id=\"drawio-clipped-label-clip\""),
        "{svg}"
    );
    assert!(
        svg.contains("clip-path=\"url(#drawio-clipped-label-clip)\""),
        "{svg}"
    );
    // The page is the shape plus the margin: the label does not stretch it.
    assert_eq!(report.pages[0].width_points, 60.0);
}

/// draw.io's stylesheet gives a bare style token its own settings, which is how
/// `plain-yellow` colours a shape, and `glass=1` lays a sheen over the top of
/// it. Ignoring either left the shape white.
#[test]
fn applies_the_named_styles_drawio_ships_with() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("named.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Named"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="a" value="Yellow" style="ellipse;whiteSpace=wrap;html=1;plain-yellow;strokeWidth=2;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="120" height="60" as="geometry"/></mxCell>
<mxCell id="b" value="Override" style="rounded=1;html=1;plain-orange;glass=1;fillColor=#123456;" vertex="1" parent="1"><mxGeometry x="180" y="20" width="120" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The named style brings a fill, a border and a gradient with it.
    assert!(svg.contains("#FFF2CC") && svg.contains("#D6B656"), "{svg}");
    assert!(svg.contains("#FFD966"), "the gradient stop: {svg}");
    // A setting after the name still wins over it.
    assert!(svg.contains("#123456"), "{svg}");
    assert!(
        !svg.contains("#FFCD28"),
        "the overridden fill is gone: {svg}"
    );
    // The sheen is a white gradient over the top of the shape.
    assert!(svg.contains("data-content-kind=\"drawio-glass\""), "{svg}");
    assert!(svg.contains("stop-opacity=\"0.9\""), "{svg}");
}

/// A value stream map is drawn almost entirely out of lean-mapping shapes: the
/// process boxes with a band for their name, the data boxes ruled underneath
/// them, and the striped push and hooked pull arrows that join them.
#[test]
fn draws_the_lean_mapping_shapes_a_value_stream_map_is_made_of() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("vsm.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="VSM"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="proc" value="Wafer" style="shape=mxgraph.lean_mapping.manufacturing_process;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="120" height="60" as="geometry"/></mxCell>
<mxCell id="data" value="C/T= 2 min" style="shape=mxgraph.lean_mapping.data_box;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="100" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="push" style="shape=mxgraph.lean_mapping.push_arrow;html=1;" vertex="1" parent="1"><mxGeometry x="160" y="20" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="pull" style="shape=mxgraph.lean_mapping.physical_pull;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="20" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="plan" value="Schedule" style="shape=mxgraph.lean_mapping.schedule;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="140" width="100" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The process box wears a band the height of one and a half lines of text.
    assert!(svg.contains("M 20 20 H 140 V 80 H 20 Z"), "{svg}");
    assert!(svg.contains("M 20 32 L 140 32"), "{svg}");
    // The data box is open at the top and ruled into five rows.
    assert!(
        svg.contains("M 20 200 L 20 100 L 120 100 L 120 200"),
        "{svg}"
    );
    for rule in ["M 20 120 L 120 120", "M 20 180 L 120 180"] {
        assert!(svg.contains(rule), "{rule} missing from {svg}");
    }
    // The push arrow keeps its three stripes.
    assert!(
        svg.contains("M 160 37 L 235 37 L 235 20 L 260 70 L 235 120 L 235 103 L 160 103 Z"),
        "{svg}"
    );
    for stripe in [
        "M 160 37 H 172 V 103 H 160 Z",
        "M 184 37 H 196 V 103 H 184 Z",
        "M 208 37 H 220 V 103 H 208 Z",
    ] {
        assert!(svg.contains(stripe), "{stripe} missing from {svg}");
    }
    // The pull arrow is a near-full circle ending in a solid head.
    assert!(
        svg.contains("M 373.2 27.36 A 48.27 49.59 0 1 0 395.53 81.91"),
        "{svg}"
    );
    assert!(
        svg.contains("M 390.71 81.91 L 397.94 69.51 L 400 84.38 Z"),
        "{svg}"
    );
    // A schedule is a plain box, and it keeps its label.
    assert!(svg.contains("M 300 140 H 400 V 180 H 300 Z"), "{svg}");
    assert!(svg.contains("Schedule"), "{svg}");
}

/// Callouts and the shaded infographic shapes. The banners and pyramids are
/// drawn as several layers — a face, a light side and a dark one — so the test
/// checks that the layers are there as well as that the outline is right.
#[test]
fn draws_the_callouts_and_shaded_shapes_an_infographic_is_built_from() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("info.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Info"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="bar" style="shape=mxgraph.infographic.barCallout;dx=60;dy=20;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="180" height="60" as="geometry"/></mxCell>
<mxCell id="rect" style="shape=mxgraph.basic.rectCallout;dx=30;dy=20;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="20" width="140" height="70" as="geometry"/></mxCell>
<mxCell id="round" style="shape=mxgraph.basic.roundRectCallout;dx=60;dy=20;size=10;html=1;" vertex="1" parent="1"><mxGeometry x="380" y="20" width="140" height="70" as="geometry"/></mxCell>
<mxCell id="tri" style="shape=mxgraph.infographic.shadedTriangle;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="120" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="pyr" style="shape=mxgraph.infographic.shadedPyramid;html=1;" vertex="1" parent="1"><mxGeometry x="140" y="120" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="step" style="shape=mxgraph.infographic.pyramidStep;html=1;" vertex="1" parent="1"><mxGeometry x="260" y="120" width="100" height="60" as="geometry"/></mxCell>
<mxCell id="banner" style="shape=mxgraph.infographic.banner;dx=25;dy=15;notch=15;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="260" width="240" height="80" as="geometry"/></mxCell>
<mxCell id="fold" style="shape=mxgraph.infographic.bannerSingleFold;dx=32;dy=15;dx2=20;notch=15;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="260" width="240" height="80" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A bar with a pointer hanging seven pixels either side of x = 60.
    assert!(
        svg.contains("M 20 20 L 200 20 L 200 60 L 87 60 L 80 80 L 73 60 L 20 60 Z"),
        "{svg}"
    );
    // A speech box, its tail below the body rather than inside it.
    assert!(
        svg.contains("M 240 70 L 220 70 L 220 20 L 360 20 L 360 70 L 260 70 L 230 90 Z"),
        "{svg}"
    );
    // The rounded one turns each corner with an arc of the requested size.
    assert!(
        svg.contains("M 430 70 L 390 70 A 10 10 0 0 1 380 60 L 380 30 A 10 10 0 0 1 390 20"),
        "{svg}"
    );
    assert!(
        svg.contains("A 38 28 0 0 1 420 90 A 18 28 0 0 0 430 70 Z"),
        "{svg}"
    );
    // The triangle is one face plus a light and a dark side, stroked over.
    assert_eq!(svg.matches("id=\"drawio-tri-part-").count(), 4, "{svg}");
    assert!(svg.contains("M 20 220 L 70 120 L 120 220 Z"), "{svg}");
    assert!(svg.contains("M 20 220 L 70 120 L 70 187 Z"), "{svg}");
    assert!(svg.contains("M 120 220 L 70 187 L 70 120 Z"), "{svg}");
    // The pyramid's ridge sits three tenths of its width above the base.
    assert!(
        svg.contains("M 140 190 L 190 120 L 240 190 L 190 220 Z"),
        "{svg}"
    );
    // The stepped block is a box with a shallow ridge on top.
    assert!(
        svg.contains("M 260 130 L 310 120 L 360 130 L 360 180 L 260 180 Z"),
        "{svg}"
    );
    // The ribbon banner: notched ends, folded tails, and shading on both ends.
    assert!(
        svg.contains(
            "M 20 275 L 45 275 L 45 260 L 235 260 L 235 275 L 260 275 L 245 307.5 \
             L 260 340 L 205 340 L 205 325 L 75 325 L 75 340 L 20 340 L 35 307.5 Z"
        ),
        "{svg}"
    );
    assert_eq!(svg.matches("id=\"drawio-banner-part-").count(), 5, "{svg}");
    // A single fold is pointed on the left and folded only on the right.
    assert!(
        svg.contains(
            "M 320 260 L 508 260 L 508 275 L 540 275 L 525 307.5 L 540 340 \
             L 478 340 L 478 325 L 320 325 L 300 292.5 Z"
        ),
        "{svg}"
    );
}

/// The controls a mock-up is assembled from, and the shapes several libraries
/// each define their own copy of: buttons rounded along one edge, rounded and
/// inset rectangles, checkboxes and radio buttons, and anchors, which draw
/// nothing at all because they are only there to attach a connector to.
#[test]
fn draws_the_controls_a_mock_up_is_assembled_from() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ui.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="UI"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="top" style="shape=mxgraph.mockup.containers.topButton;rSize=10;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="120" height="40" as="geometry"/></mxCell>
<mxCell id="bottom" style="shape=mxgraph.bootstrap.bottomButton;rSize=10;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="60" width="120" height="40" as="geometry"/></mxCell>
<mxCell id="left" style="shape=mxgraph.mockup.leftButton;rSize=10;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="20" width="60" height="40" as="geometry"/></mxCell>
<mxCell id="right" style="shape=mxgraph.bootstrap.rightButton;rSize=10;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="20" width="60" height="40" as="geometry"/></mxCell>
<mxCell id="round" style="shape=mxgraph.mockup.forms.rrect;rSize=15;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="80" width="120" height="50" as="geometry"/></mxCell>
<mxCell id="inset" style="shape=mxgraph.mockup.containers.marginRect;rectMargin=10;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="20" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="plain" style="shape=mxgraph.gmdl.marginRect;rectMargin=5;rectMarginTop=15;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="90" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="open" style="shape=mxgraph.mockup.forms.uRect;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="120" width="120" height="40" as="geometry"/></mxCell>
<mxCell id="tick" style="shape=mxgraph.bootstrap.checkbox;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="150" width="24" height="24" as="geometry"/></mxCell>
<mxCell id="radio" style="shape=mxgraph.bootstrap.radioButton;strokeColor=#0085FC;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="150" width="24" height="24" as="geometry"/></mxCell>
<mxCell id="hold" style="shape=mxgraph.ios7ui.anchor;html=1;" vertex="1" parent="1"><mxGeometry x="500" y="20" width="10" height="10" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Each button rounds the two corners on its own edge and squares the rest.
    for (id, d) in [
        (
            "top",
            "M 20 30 A 10 10 0 0 1 30 20 L 130 20 A 10 10 0 0 1 140 30 L 140 60 L 20 60 Z",
        ),
        (
            "bottom",
            "M 140 90 A 10 10 0 0 1 130 100 L 30 100 A 10 10 0 0 1 20 90 L 20 60 L 140 60 Z",
        ),
        (
            "left",
            "M 190 60 A 10 10 0 0 1 180 50 L 180 30 A 10 10 0 0 1 190 20 L 240 20 L 240 60 Z",
        ),
        (
            "right",
            "M 290 20 A 10 10 0 0 1 300 30 L 300 50 A 10 10 0 0 1 290 60 L 240 60 L 240 20 Z",
        ),
    ] {
        assert!(
            svg.contains(&format!("id=\"drawio-{id}\" d=\"{d}\"")),
            "{id} missing from {svg}"
        );
    }
    // A rounded rectangle takes its radius from `rSize`, not from `arcSize`.
    assert!(svg.contains("M 195 80 H 285 A 15 15 0 0 1 300 95"), "{svg}");
    // An inset rectangle is rounded by ten, and its plain form by nothing.
    assert!(svg.contains("M 360 30 H 460 A 10 10 0 0 1 470 40"), "{svg}");
    assert!(svg.contains("M 345 110 H 475 V 145 H 345 Z"), "{svg}");
    // Three sides of a box, open along the bottom.
    assert!(
        svg.contains("M 20 160 L 20 120 L 140 120 L 140 160"),
        "{svg}"
    );
    // A checkbox is rounded by three whatever its size, and carries a tick.
    assert!(svg.contains("M 183 150 H 201 A 3 3 0 0 1 204 153"), "{svg}");
    assert!(
        svg.contains("d=\"M 199.2 154.8 L 189.6 169.2 L 186 164.4\""),
        "{svg}"
    );
    // A radio button's dot takes the stroke colour rather than the fill.
    assert!(
        svg.contains("M 226 162 A 6 6 0 1 0 238 162 A 6 6 0 1 0 226 162 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#0085FC\""),
        "{svg}"
    );
    // An anchor has no outline of its own.
    assert!(!svg.contains("id=\"drawio-hold\""), "{svg}");
}

/// The shapes draw.io registers under a plain name, which a diagram reaches for
/// without naming a library: the isometric faces, the brace and the pie slice.
#[test]
fn draws_the_isometric_faces_the_brace_and_the_pie_slice() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("iso.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Iso"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="face" style="shape=isoRectangle;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="120" height="80" as="geometry"/></mxCell>
<mxCell id="cube" style="shape=isoCube2;isoAngle=15;html=1;" vertex="1" parent="1"><mxGeometry x="170" y="20" width="100" height="120" as="geometry"/></mxCell>
<mxCell id="brace" style="shape=curlyBracket;size=0.5;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="20" width="40" height="120" as="geometry"/></mxCell>
<mxCell id="slice" style="shape=mxgraph.basic.pie;startAngle=0.25;endAngle=0.9;html=1;" vertex="1" parent="1"><mxGeometry x="370" y="20" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="half" style="shape=mxgraph.basic.pie;startAngle=0;endAngle=0.5;html=1;" vertex="1" parent="1"><mxGeometry x="490" y="20" width="100" height="100" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A rhombus on a thirty degree grid, centred in a box wider than it is.
    assert!(
        svg.contains("M 20 60 L 80 25.359 L 140 60 L 80 94.641 Z"),
        "{svg}"
    );
    // A cube: six sides, with a Y drawn over it to divide the three faces.
    assert!(
        svg.contains("M 220 20 L 270 44.008 L 270 115.992 L 220 140 L 170 115.992 L 170 44.008 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 170 44.008 L 220 68.016 L 270 44.008"),
        "{svg}"
    );
    assert!(svg.contains("M 220 68.016 L 220 140"), "{svg}");
    // A brace is drawn open, so it has strokes but no outline to fill.
    assert!(!svg.contains("id=\"drawio-brace\" "), "{svg}");
    assert!(
        svg.contains("M 340 20 L 320 20 L 320 80 L 300 80 L 320 80 L 320 140 L 340 140"),
        "{svg}"
    );
    // Two thirds of a turn is drawn as two arcs, because one arc of half a
    // turn or more leaves the direction it sweeps in undefined.
    assert!(
        svg.contains("M 420 70 L 470 70 A 50 50 0 0 1 397.3 114.55 A 50 50 0 0 1 390.611 29.549 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 540 70 L 540 20 A 50 50 0 0 1 590 70 A 50 50 0 0 1 540 120 Z"),
        "{svg}"
    );
}

/// The SysML activity and flow nodes draw.io implements in code. Each is a
/// body with square ports let into its sides, and because every part takes the
/// same fill and stroke they are drawn as one path with several subpaths.
#[test]
fn draws_the_sysml_activity_and_flow_nodes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sysml.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="SysML"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="done" style="shape=mxgraph.sysml.actFinal;strokeColor=#FF0000;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="stop" style="shape=mxgraph.sysml.flowFinal;html=1;" vertex="1" parent="1"><mxGeometry x="90" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="ctrl" style="shape=mxgraph.sysml.isControl;html=1;" vertex="1" parent="1"><mxGeometry x="170" y="20" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="inflow" style="shape=mxgraph.sysml.objFlowL;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="20" width="120" height="50" as="geometry"/></mxCell>
<mxCell id="outflow" style="shape=mxgraph.sysml.objFlowR;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="90" width="120" height="50" as="geometry"/></mxCell>
<mxCell id="items" style="shape=mxgraph.sysml.itemFlowRight;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="100" width="140" height="120" as="geometry"/></mxCell>
<mxCell id="frame" style="shape=mxgraph.sysml.paramDgm;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="160" width="160" height="100" as="geometry"/></mxCell>
<mxCell id="short" style="shape=mxgraph.sysml.paramDgm;html=1;" vertex="1" parent="1"><mxGeometry x="520" y="160" width="160" height="50" as="geometry"/></mxCell>
<mxCell id="port" style="shape=mxgraph.sysml.port1;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="250" width="100" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // An activity end is a ring with a disc inside it, and the disc takes the
    // stroke colour rather than the fill.
    assert!(
        svg.contains("M 25 45 A 20 20 0 1 0 65 45 A 20 20 0 1 0 25 45 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#FF0000\""),
        "{svg}"
    );
    // A flow end is the same ring crossed out.
    assert!(svg.contains("M 97.25 27.25 L 132.75 62.75"), "{svg}");
    assert!(svg.contains("M 132.75 27.25 L 97.25 62.75"), "{svg}");
    // A control operator has a port at each end, an object flow one.
    assert!(
        svg.contains(
            "M 170 35 H 180 V 55 H 170 Z M 190 20 H 290 A 10 10 0 0 1 300 30 V 60 \
             A 10 10 0 0 1 290 70 H 190 A 10 10 0 0 1 180 60 V 30 A 10 10 0 0 1 190 20 Z \
             M 300 35 H 310 V 55 H 300 Z"
        ),
        "{svg}"
    );
    assert!(
        svg.contains("M 340 35 H 350 V 55 H 340 Z M 360 20 H 450"),
        "{svg}"
    );
    assert!(
        svg.contains("A 10 10 0 0 1 350 90 Z M 450 105 H 460 V 125 H 450 Z"),
        "{svg}"
    );
    // An item flow carries three ports, spaced a quarter apart down its side.
    assert!(
        svg.contains(
            "M 180 100 H 310 V 220 H 180 Z M 300 120 H 320 V 140 H 300 Z \
             M 300 150 H 320 V 170 H 300 Z M 300 180 H 320 V 200 H 300 Z"
        ),
        "{svg}"
    );
    // A parametric frame carries two ports, but only when it is tall enough.
    assert!(svg.contains("M 340 175 H 360 V 195 H 340 Z"), "{svg}");
    assert!(!svg.contains("id=\"drawio-short-detail-0\""), "{svg}");
    // A port is drawn inset by a twentieth of its box on each side.
    assert!(svg.contains("M 25 250 H 115 V 290 H 25 Z"), "{svg}");
}

/// The floor-plan shapes that are drawn from their own measurements rather than
/// from a stencil: a room's four walls, a flight of stairs with a landing, and
/// a sliding door.
#[test]
fn draws_a_room_its_stairs_and_its_sliding_door() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("plan.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Plan"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="room" style="shape=mxgraph.floorplan.room;wallThickness=10;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="200" height="140" as="geometry"/></mxCell>
<mxCell id="stairs" style="shape=mxgraph.floorplan.stairsRest;html=1;" vertex="1" parent="1"><mxGeometry x="260" y="20" width="180" height="60" as="geometry"/></mxCell>
<mxCell id="door" style="shape=mxgraph.floorplan.doorBypass;dx=0.3;html=1;" vertex="1" parent="1"><mxGeometry x="260" y="110" width="160" height="30" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The inside of the room is wound the other way round, so it reads as a
    // hole in the walls rather than as a second filled box.
    assert!(
        svg.contains("M 20 160 L 20 20 L 220 20 L 220 160 Z M 30 30 L 30 150 L 210 150 L 210 30 Z"),
        "{svg}"
    );
    // A tread every twenty five pixels, stopping short of the landing.
    assert_eq!(
        svg.matches("id=\"drawio-stairs-detail-").count(),
        8,
        "{svg}"
    );
    assert!(svg.contains("M 285 20 L 285 80"), "{svg}");
    assert!(svg.contains("M 385 20 L 385 80"), "{svg}");
    // The landing carries the arrow that says which way the stairs go up.
    assert!(svg.contains("M 440 20 L 410 50 L 440 80"), "{svg}");
    // Two panels that pass each other, between a jamb at either end.
    assert!(
        svg.contains(
            "M 260 120 H 265 V 130 H 260 Z M 415 120 H 420 V 130 H 415 Z \
             M 260 125 H 340 V 130 H 260 Z M 308 120 H 388 V 125 H 308 Z"
        ),
        "{svg}"
    );
}

/// The iOS 7 controls. Several of them carry a second colour of their own —
/// the status bar's ink, a slider's track, a switch's off state — which the
/// shape has to read from the style rather than take from the fill and stroke.
#[test]
fn draws_the_ios_controls_and_the_status_bar_above_them() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ios.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="iOS"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="phone" style="shape=mxgraph.ios7ui.phone;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="160" height="320" as="geometry"/></mxCell>
<mxCell id="bar" style="shape=mxgraph.ios7ui.appBar;fillColor2=#222222;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="20" width="220" height="24" as="geometry"/></mxCell>
<mxCell id="dots" style="shape=mxgraph.ios7ui.pageControl;fillColor=#FFFFFF;strokeColor=#999999;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="60" width="120" height="16" as="geometry"/></mxCell>
<mxCell id="load" style="shape=mxgraph.ios7ui.downloadBar;fillColor=#DDDDDD;strokeColor=#0080F0;barPos=60;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="90" width="200" height="20" as="geometry"/></mxCell>
<mxCell id="slide" style="shape=mxgraph.ios7ui.slider;barColor=#BBBBBB;barPos=40;handleSize=14;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="130" width="200" height="20" as="geometry"/></mxCell>
<mxCell id="on" style="shape=mxgraph.ios7ui.onOffButton;buttonState=on;handleColor=#FFFFFF;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="170" width="60" height="30" as="geometry"/></mxCell>
<mxCell id="off" style="shape=mxgraph.ios7ui.onOffButton;buttonState=off;fillColor2=#FFFFFF;strokeColor2=#AAAAAA;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="170" width="60" height="30" as="geometry"/></mxCell>
<mxCell id="apps" style="shape=mxgraph.ios7ui.iconGrid;gridSize=4,5;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="220" width="140" height="160" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The case is rounded by twenty five, and carries the screen, the earpiece
    // and the home button as outlines over it.
    assert!(svg.contains("M 45 20 H 155 A 25 25 0 0 1 180 45"), "{svg}");
    assert!(svg.contains("M 30 68 H 170 V 292 H 30 Z"), "{svg}");
    // The earpiece is a rounded rectangle with oval, not circular, corners.
    assert!(
        svg.contains("M 83.2 44 H 116.8 A 3.2 3 0 0 1 120 47 V 47 A 3.2 3 0 0 1 116.8 50"),
        "{svg}"
    );
    // The status bar: five signal dots, then the wifi fan over its own dot.
    assert!(
        svg.contains("M 225 32 A 1.5 1.5 0 1 0 228 32 A 1.5 1.5 0 1 0 225 32 Z"),
        "{svg}"
    );
    assert!(svg.contains("M 272 33 A 3.5 3.5 0 0 1 279 33"), "{svg}");
    assert!(svg.contains("M 270 31 A 6 6 0 0 1 282 31"), "{svg}");
    // The battery, its charge, and the glyph beside it.
    assert!(
        svg.contains("M 421 30 L 434 30 L 434 34 L 421 34 Z"),
        "{svg}"
    );
    assert!(
        svg.contains(
            "M 420 29 L 435 29 L 435 31 L 436.5 31 L 436.5 33 L 435 33 L 435 35 L 420 35 Z"
        ),
        "{svg}"
    );
    // Five page dots, the last one the odd one out in the fill colour.
    assert_eq!(svg.matches("id=\"drawio-dots-part-").count(), 5, "{svg}");
    assert!(
        svg.contains("M 328 68 A 6 6 0 1 0 340 68 A 6 6 0 1 0 328 68 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#FFFFFF\""),
        "{svg}"
    );
    // The download bar draws the whole track, then the part already done.
    assert!(svg.contains("M 220 99 H 420 V 101 H 220 Z"), "{svg}");
    assert!(svg.contains("M 220 99 H 340 V 101 H 220 Z"), "{svg}");
    // The slider's track takes `barColor`, and so does its handle's edge.
    assert!(
        svg.contains("M 220 140 L 420 140\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"none\" stroke=\"#BBBBBB\""),
        "{svg}"
    );
    assert!(svg.contains("M 293 140 A 7 7 0 1 0 307 140"), "{svg}");
    // A switch is at least twice as wide as it is tall, and its handle sits at
    // whichever end matches its state.
    assert!(svg.contains("M 251 185 A 14 14 0 1 0 279 185"), "{svg}");
    assert!(svg.contains("M 300 185 A 15 15 0 1 0 330 185"), "{svg}");
    // Twenty tiles, each a tenth of its own width clear of the next.
    assert_eq!(svg.matches("H 252.558 V").count(), 5, "{svg}");
    assert!(
        svg.contains("M 255.814 220 H 288.372 V 249.63 H 255.814 Z"),
        "{svg}"
    );
}

/// The rest of the lean mapping set: the lead time ladder, the FIFO lane and
/// the lorry, whose wheels take the stroke colour.
#[test]
fn draws_the_lead_time_ladder_the_fifo_lane_and_the_lorry() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("lean.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Lean"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="ladder" style="shape=mxgraph.lean_mapping.timeline2;dy1=0;dx2=60;dy2=1;dx3=140;dy3=0;dx4=220;dy4=1;dx5=300;dy5=0;dy6=1;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="380" height="60" as="geometry"/></mxCell>
<mxCell id="lane" style="shape=mxgraph.lean_mapping.fifo_lane;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="110" width="160" height="60" as="geometry"/></mxCell>
<mxCell id="lorry" style="shape=mxgraph.lean_mapping.truck_shipment;strokeColor=#000000;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="110" width="120" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A square wave that turns where each step says, and only where it turns.
    assert!(
        svg.contains(
            "M 20 20 L 80 20 L 80 80 L 160 80 L 160 20 L 240 20 L 240 80 \
             L 320 80 L 320 20 L 400 20 L 400 80"
        ),
        "{svg}"
    );
    // The lane's band, and the three shapes queued along it.
    assert!(svg.contains("M 20 122 L 180 122"), "{svg}");
    assert!(
        svg.contains(
            "M 23.2 126 H 64.8 V 166 H 23.2 Z M 76 146 A 20.8 20 0 1 0 117.6 146 \
             A 20.8 20 0 1 0 76 146 Z M 130.4 126 L 176.8 126 L 153.6 166 Z"
        ),
        "{svg}"
    );
    // A body and a cab, on wheels drawn in the stroke colour.
    assert!(svg.contains("M 220 110 H 292 V 158 H 220 Z"), "{svg}");
    assert!(
        svg.contains("M 238 164 A 12 6 0 1 0 262 164 A 12 6 0 1 0 238 164 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#000000\""),
        "{svg}"
    );
}

/// The odds and ends a real diagram reaches for: a mock-up rule, button and
/// checkbox, a close cross, and the basic set's drop, obtuse triangle and the
/// polygon a style spells out corner by corner.
#[test]
fn draws_a_polygon_a_style_spells_out_and_the_shapes_beside_it() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("misc.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Misc"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="rule" style="shape=mxgraph.mockup.markup.line;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="180" height="20" as="geometry"/></mxCell>
<mxCell id="go" style="shape=mxgraph.mockup.buttons.button;buttonStyle=round;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="60" width="140" height="40" as="geometry"/></mxCell>
<mxCell id="next" style="shape=mxgraph.mockup.buttons.button;buttonStyle=chevron;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="60" width="140" height="40" as="geometry"/></mxCell>
<mxCell id="tick" style="shape=mxgraph.mockup.forms.checkbox;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="60" width="30" height="30" as="geometry"/></mxCell>
<mxCell id="close" style="shape=mxgraph.bootstrap.x;html=1;" vertex="1" parent="1"><mxGeometry x="390" y="60" width="20" height="20" as="geometry"/></mxCell>
<mxCell id="drop" style="shape=mxgraph.basic.drop;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="130" width="70" height="110" as="geometry"/></mxCell>
<mxCell id="wedge" style="shape=mxgraph.basic.obtuse_triangle;dx=0.2;html=1;" vertex="1" parent="1"><mxGeometry x="120" y="130" width="120" height="90" as="geometry"/></mxCell>
<mxCell id="poly" style="shape=mxgraph.basic.polygon;polyCoords=[[0,0.5],[0.35,0],[1,0.15],[0.8,1],[0.15,0.9]];html=1;" vertex="1" parent="1"><mxGeometry x="270" y="130" width="140" height="110" as="geometry"/></mxCell>
<mxCell id="curved" style="shape=mxgraph.basic.polygon;polyCoords=[[0,1],[0.5,0],[1,1]];polyCurves=[[&quot;Q&quot;,0.1,0.2],null,[&quot;Q&quot;,0.5,0.6]];html=1;" vertex="1" parent="1"><mxGeometry x="440" y="130" width="140" height="110" as="geometry"/></mxCell>
<mxCell id="open" style="shape=mxgraph.basic.polygon;polyCoords=[[0,1],[0.5,0],[1,1]];polyline=1;html=1;" vertex="1" parent="1"><mxGeometry x="600" y="130" width="140" height="110" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("M 20 30 L 200 30"), "{svg}");
    assert!(svg.contains("M 30 60 H 150 A 10 10 0 0 1 160 70"), "{svg}");
    // A chevron button comes to a point on the right.
    assert!(
        svg.contains("L 318.6 78.34 A 12.6 4 0 0 1 318.6 81.66"),
        "{svg}"
    );
    assert!(svg.contains("M 364 66 L 352 84 L 347.5 78"), "{svg}");
    assert!(svg.contains("M 390 60 L 410 80"), "{svg}");
    // A drop: straight sides from the point, meeting the circle at a tangent.
    assert!(
        svg.contains("M 55 130 L 85.955 188.667 A 35 35 0 0 1 90 205"),
        "{svg}"
    );
    // The triangle's apex sits a fifth of the way along the top.
    assert!(svg.contains("M 144 220 L 120 130 L 240 220 Z"), "{svg}");
    // The polygon's corners are fractions of its own box.
    assert!(
        svg.contains("M 270 185 L 319 130 L 410 146.5 L 382 240 L 291 229 Z"),
        "{svg}"
    );
    // A curved side names its control point in a second list, one entry per
    // side, so a straight side has to keep its place in that list.
    assert!(
        svg.contains("M 440 240 Q 454 152 510 130 L 580 240 Q 510 196 440 240 Z"),
        "{svg}"
    );
    // A polyline is left open, so it has no outline to fill.
    assert!(!svg.contains("id=\"drawio-open\" "), "{svg}");
    assert!(svg.contains("M 600 240 L 670 130 L 740 240"), "{svg}");
}

/// A diagram can be saved with nothing on it. draw.io stands its own "click
/// here to edit" placeholder on such a page, but that placeholder is not in the
/// model, so the page is written blank at the size the model declares and the
/// report says why rather than leaving a speck of a page behind.
#[test]
fn writes_an_empty_drawio_page_at_the_size_the_model_declares() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("empty.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Page-1"><mxGraphModel pageWidth="850" pageHeight="1100">
<root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("no cells to draw"),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // 850 x 1100 pixels is 637.5 x 825 points.
    assert!(svg.contains("width=\"637.5pt\" height=\"825pt\""), "{svg}");

    // Without a declared size the page falls back to US Letter rather than to
    // a page barely larger than nothing.
    let bare = temporary.path().join("bare.drawio");
    let bare_output = temporary.path().join("bare-out");
    drawio_file(
        &bare,
        r##"<mxfile><diagram name="Page-1"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram></mxfile>"##,
    );
    convert_path(&bare, &bare_output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(bare_output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("width=\"637.5pt\" height=\"825pt\""), "{svg}");
}

/// ArchiMate 3 draws an element as a frame with a small badge in its corner
/// saying which kind of element it is. The frame comes from `archiType` and the
/// badge from `appType` or `techType`, and both take the element's own colours,
/// except for the lines across a badge, which are stroked and not filled.
#[test]
fn draws_archimate_elements_with_the_badge_that_names_their_kind() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("archi.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="ArchiMate"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="goal" style="shape=mxgraph.archimate3.application;appType=goal;archiType=oct;strokeColor=#FF0000;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="need" style="shape=mxgraph.archimate3.application;appType=requirement;archiType=oct;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="20" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="look" style="shape=mxgraph.archimate3.application;appType=assess;archiType=oct;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="20" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="node" style="shape=mxgraph.archimate3.application;appType=node;archiType=square;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="100" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="func" style="shape=mxgraph.archimate3.application;appType=func;archiType=rounded;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="100" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="both" style="shape=mxgraph.archimate3.application;appType=collab;archiType=square;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="100" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="when" style="shape=mxgraph.archimate3.application;appType=event;archiType=rounded;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="180" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="step" style="shape=mxgraph.archimate3.application;appType=proc;archiType=rounded;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="180" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="who" style="shape=mxgraph.archimate3.application;appType=actor;archiType=square;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="180" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="offer" style="shape=mxgraph.archimate3.service;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="260" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="runs" style="shape=mxgraph.archimate3.tech;techType=sysSw;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="260" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="mystery" style="shape=mxgraph.archimate3.application;appType=nosuchtype;archiType=square;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="260" width="140" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The frame takes one of three shapes, and the badge sits fifteen pixels
    // in from its top right corner whatever size the element is.
    assert!(
        svg.contains("M 20 30 L 30 20 L 150 20 L 160 30 L 160 70 L 150 80 L 30 80 L 20 70 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 190 100 H 310 A 10 10 0 0 1 320 110"),
        "{svg}"
    );
    assert!(svg.contains("M 340 100 H 480 V 160 H 340 Z"), "{svg}");
    // A goal is three rings, and only its centre takes the stroke colour.
    assert!(svg.contains("M 140 32.5 A 7.5 7.5 0 1 0 155 32.5"), "{svg}");
    assert!(
        svg.contains("M 144.5 32.5 A 3 3 0 1 0 150.5 32.5 A 3 3 0 1 0 144.5 32.5 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#FF0000\""),
        "{svg}"
    );
    // The badges that say which kind of element each one is.
    for (what, d) in [
        ("requirement", "M 303.75 25 L 315 25 L 311.25 40 L 300 40 Z"),
        ("assessment handle", "M 460 40 L 464.8 35.2"),
        (
            "node",
            "M 140 108.75 L 143.75 105 L 155 105 L 155 116.25 L 151.25 120 L 140 120 Z \
             M 140 108.75 L 151.25 108.75 L 151.25 120",
        ),
        (
            "function",
            "M 307.5 105 L 315 108 L 315 120 L 307.5 117 L 300 120 L 300 108 Z",
        ),
        ("collaboration", "M 466 112.5 A 4.5 4.5 0 1 0 475 112.5"),
        (
            "event",
            "M 150.5 188 A 4.5 4.5 0 0 1 150.5 197 L 140 197 L 144.5 192.5 L 140 188 Z",
        ),
        (
            "process",
            "M 300 189.5 L 309 189.5 L 309 185 L 315 192.5 L 309 200 L 309 195.5 L 300 195.5 Z",
        ),
        (
            "system software",
            "M 290 282.65 A 7.35 7.35 0 1 0 304.7 282.65",
        ),
    ] {
        assert!(svg.contains(d), "{what} badge missing from {svg}");
    }
    // A figure's arms and legs are stroked, not filled.
    assert!(
        svg.contains("M 460 200 L 467.5 196.25 L 475 200\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"none\""),
        "{svg}"
    );
    // A service is a rounded bar and carries no badge of its own.
    assert!(
        svg.contains("M 135 260 A 25 25 0 0 1 135 310 L 45 310 A 25 25 0 0 1 45 260 Z"),
        "{svg}"
    );
    assert!(!svg.contains("id=\"drawio-offer-part-2\""), "{svg}");
    // An element type that names no badge keeps its frame and an empty corner
    // rather than losing the element altogether.
    assert!(svg.contains("id=\"drawio-mystery-part-1\""), "{svg}");
    assert!(!svg.contains("id=\"drawio-mystery-part-2\""), "{svg}");
}

/// The rack units draw.io implements in code: a cabinet frame that snaps to a
/// whole number of rack units, a blanking plate shaded at each mounting ear, a
/// cable duct with a bay every thirty three pixels, a shelf bracket and a patch
/// panel in its own body colour.
#[test]
fn draws_the_rack_units_a_cabinet_is_filled_with() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("rack.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Rack"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="cabinet" style="shape=mxgraph.rackGeneral.rackCabinet3;fillColor=#F4F4F4;fillColor2=#FFFFFF;rackUnitSize=14.8;numDisp=descend;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="180" height="220" as="geometry"/></mxCell>
<mxCell id="plate" style="shape=mxgraph.rackGeneral.plate;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="20" width="160" height="20" as="geometry"/></mxCell>
<mxCell id="duct" style="shape=mxgraph.rackGeneral.horCableDuct;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="60" width="160" height="20" as="geometry"/></mxCell>
<mxCell id="shelf" style="shape=mxgraph.rackGeneral.shelf;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="100" width="160" height="24" as="geometry"/></mxCell>
<mxCell id="patch" style="shape=mxgraph.rackGeneral.neatPatch;bodyColor=#666666;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="140" width="160" height="24" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The numbering column is outside the frame, so the frame starts 24 in,
    // and its height snaps to twelve units of 14.8 plus its two 21-tall rails.
    assert!(svg.contains("M 44 20 H 200 V 239.6 H 44 Z"), "{svg}");
    assert!(svg.contains("M 44 20 H 200 V 41 H 44 Z"), "{svg}");
    assert!(svg.contains("M 44 41 H 53 V 218.6 H 44 Z"), "{svg}");
    // Four screws hold it in.
    assert_eq!(svg.matches("A 3 3 0 1 0").count(), 8, "{svg}");
    // The plate's mounting ears are shaded rather than a different fill.
    assert!(
        svg.contains("M 240 20 H 249 V 40 H 240 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#000000\" fill-opacity=\"0.23\""),
        "{svg}"
    );
    // A bay every thirty three pixels, each drawn as two stacked slots.
    assert!(svg.contains("M 254 60 H 257 V 67 H 254 Z"), "{svg}");
    assert!(svg.contains("M 287 60 H 290 V 67 H 287 Z"), "{svg}");
    assert_eq!(svg.matches("id=\"drawio-duct-detail-").count(), 10, "{svg}");
    // A shelf is the bracket that holds it, so it has no outline to fill.
    assert!(!svg.contains("id=\"drawio-shelf\" "), "{svg}");
    assert!(
        svg.contains("M 241 100 L 241 123 L 399 123 L 399 101"),
        "{svg}"
    );
    // The patch panel takes its body colour, not the style's fill.
    assert!(
        svg.contains("M 240 140 H 400 V 164 H 240 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#666666\""),
        "{svg}"
    );
}

/// An entity-relationship diagram's own shapes, a C4 person and a browser
/// window. Each carries colours or an order of its own: a weak entity gets a
/// second frame, a person's head is drawn over the shoulders, and a browser's
/// close button and chrome are separate colours from the frame.
#[test]
fn draws_er_entities_a_c4_person_and_a_browser_window() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mixed.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Mixed"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="entity" value="Customer" style="shape=mxgraph.er.entity;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="weak" value="Order" style="shape=mxgraph.er.entity;buttonStyle=dblFrame;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="20" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="has" value="places" style="shape=mxgraph.er.has;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="10" width="140" height="70" as="geometry"/></mxCell>
<mxCell id="owns" value="owns" style="shape=mxgraph.er.has;buttonStyle=dblFrame;html=1;" vertex="1" parent="1"><mxGeometry x="500" y="10" width="140" height="70" as="geometry"/></mxCell>
<mxCell id="user" value="User" style="shape=mxgraph.c4.person;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="110" width="100" height="120" as="geometry"/></mxCell>
<mxCell id="page" style="shape=mxgraph.mockup.containers.browserWindow;strokeColor2=#008CFF;strokeColor3=#C4C4C4;html=1;" vertex="1" parent="1"><mxGeometry x="200" y="110" width="500" height="320" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // An entity is rounded by ten; a weak one is square with a frame inside.
    assert!(svg.contains("M 30 20 H 150 A 10 10 0 0 1 160 30"), "{svg}");
    assert!(svg.contains("M 180 20 H 320 V 70 H 180 Z"), "{svg}");
    assert!(svg.contains("M 185 25 H 315 V 65 H 185 Z"), "{svg}");
    // A relationship is a diamond, and a weak one gets a second one inside.
    assert!(
        svg.contains("M 340 45 L 410 10 L 480 45 L 410 80 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 514 45 L 570 17 L 626 45 L 570 73 Z"),
        "{svg}"
    );
    // The person's head is drawn twice, so the shoulders stop at it.
    assert_eq!(
        svg.matches("M 50 130 A 20 20 0 1 0 90 130 A 20 20 0 1 0 50 130 Z")
            .count(),
        2,
        "{svg}"
    );
    assert!(
        svg.contains("M 20 162 A 20 20 0 0 1 40 142 L 100 142"),
        "{svg}"
    );
    // The browser's close button takes its own colour, and the chrome another.
    assert!(
        svg.contains("M 675 125 A 10 10 0 1 0 695 125 A 10 10 0 1 0 675 125 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"none\" stroke=\"#008CFF\""),
        "{svg}"
    );
    assert!(
        svg.contains("M 200 150 L 230 150 L 230 125 A 5 5 0 0 1 235 120 L 370 120"),
        "{svg}"
    );
    // Back and forward point opposite ways, and the page icon is drawn twice.
    assert!(
        svg.contains("M 212 184 L 222 174 L 222 180 L 232 180"),
        "{svg}"
    );
    assert!(
        svg.contains("M 262 184 L 252 174 L 252 180 L 242 180"),
        "{svg}"
    );
    assert_eq!(
        svg.matches("L 252 131 L 252 145 L 237 145 Z").count(),
        1,
        "{svg}"
    );
}

/// The last of the shapes a real diagram reaches for: the arrows2 set, a data
/// store with its identifier band, the SysML accept-event and call-behaviour
/// actions, a UML state and BPMN's data object with its transfer and collection
/// marks.
#[test]
fn draws_the_arrows_states_and_data_objects_left_in_the_tail() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("tail.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Tail"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="both" style="shape=mxgraph.arrows2.twoWayArrow;dx=30;dy=0.5;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="160" height="60" as="geometry"/></mxCell>
<mxCell id="fancy" style="shape=mxgraph.arrows2.stylisedArrow;dx=30;dy=0.5;notch=20;feather=0.6;html=1;" vertex="1" parent="1"><mxGeometry x="200" y="20" width="160" height="60" as="geometry"/></mxCell>
<mxCell id="loop" style="shape=mxgraph.arrows2.jumpInArrow;dx=32;dy=20;arrowHead=40;html=1;" vertex="1" parent="1"><mxGeometry x="380" y="20" width="120" height="100" as="geometry"/></mxCell>
<mxCell id="store" style="shape=mxgraph.dfd.dataStoreID;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="110" width="160" height="40" as="geometry"/></mxCell>
<mxCell id="wait" style="shape=mxgraph.sysml.accEvent;html=1;" vertex="1" parent="1"><mxGeometry x="200" y="110" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="call" style="shape=mxgraph.sysml.callBehAct;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="180" width="160" height="70" as="geometry"/></mxCell>
<mxCell id="small" style="shape=mxgraph.sysml.callBehAct;html=1;" vertex="1" parent="1"><mxGeometry x="560" y="180" width="30" height="20" as="geometry"/></mxCell>
<mxCell id="state" style="shape=umlState;rounded=1;html=1;" vertex="1" parent="1"><mxGeometry x="200" y="180" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="entry" style="shape=umlState;rounded=1;umlStateConnection=connPointRefEntry;html=1;" vertex="1" parent="1"><mxGeometry x="380" y="280" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="data" style="shape=mxgraph.bpmn.data;bpmnTransferType=input;isCollection=1;html=1;" vertex="1" parent="1"><mxGeometry x="380" y="150" width="80" height="100" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A head at each end, and a shaft half the height thick.
    assert!(
        svg.contains(
            "M 50 35 L 150 35 L 150 20 L 180 50 L 150 80 L 150 65 L 50 65 L 50 80 L 20 50 L 50 20 Z"
        ),
        "{svg}"
    );
    // A feathered tail with a notch cut into its back.
    assert!(
        svg.contains("M 200 38 L 330 35 L 320 20 L 360 50 L 320 80 L 330 65 L 200 62 L 220 50 Z"),
        "{svg}"
    );
    // A head, then two arcs that carry the shaft back round to it.
    assert!(
        svg.contains(
            "M 468 20 L 500 40 L 468 60 L 468 60 A 88 60 0 0 0 380 120 A 88 100 0 0 1 468 20 Z"
        ),
        "{svg}"
    );
    // The data store is open on its right, and banded thirty in from the left.
    assert!(
        svg.contains("M 180 150 L 20 150 L 20 110 L 180 110"),
        "{svg}"
    );
    assert!(svg.contains("M 50 110 L 50 150"), "{svg}");
    // An accept-event action is notched by three tenths of its own height.
    assert!(
        svg.contains("M 200 110 L 340 110 L 340 160 L 200 160 L 215 135 Z"),
        "{svg}"
    );
    // The rake that marks a call to another behaviour, which a box too small
    // to hold it leaves off.
    assert!(
        svg.contains("M 150 240 L 150 230 L 170 230 L 170 240"),
        "{svg}"
    );
    assert!(!svg.contains("id=\"drawio-small-detail-0\""), "{svg}");
    // A state carrying a connection point reference is indented ten on the
    // left to leave room for it; a plain one is not.
    assert!(svg.contains("M 209 180 H 331 A 9 9 0 0 1 340 189"), "{svg}");
    assert!(svg.contains("M 399 280 H 511 A 9 9 0 0 1 520 289"), "{svg}");
    // A data object is a note, with an arrow saying which way the data goes
    // and three bars saying it stands for a collection.
    assert!(
        svg.contains("M 380 150 L 445 150 L 460 165 L 460 250 L 380 250 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 383 156.6 L 390.7 156.6 L 390.7 153 L 397 159 L 390.7 165 L 390.7 161.4 L 383 161.4 Z"),
        "{svg}"
    );
    assert!(svg.contains("M 420 238 L 420 250"), "{svg}");
}

/// A stencil can defer to the cell it is drawn for: `fill`, `stroke` and
/// `inherit` name the cell's own colours rather than colours of their own, and
/// `none` paints nothing. Reading those as literal colours left every stencil
/// that uses them painted black, which is most of the network and rack sets.
#[test]
fn lets_a_stencil_defer_to_the_colours_of_the_cell_it_is_drawn_for() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    fs::write(
        stencils.join("demo.xml"),
        r##"<shapes name="mxgraph.demo">
<shape name="Deferred" h="20" w="20" aspect="fixed" strokewidth="inherit">
  <connections/>
  <foreground>
    <fillcolor color="fill"/>
    <strokecolor color="stroke"/>
    <rect x="0" y="0" w="20" h="10"/>
    <fillstroke/>
    <fillcolor color="stroke"/>
    <rect x="0" y="10" w="20" h="5"/>
    <fill/>
    <fillcolor color="none"/>
    <rect x="0" y="15" w="20" h="5"/>
    <fill/>
  </foreground>
</shape>
</shapes>"##,
    )
    .unwrap();
    let input = temporary.path().join("deferred.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Deferred"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="tile" style="shape=mxgraph.demo.deferred;fillColor=#EDEDED;strokeColor=#884400;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="40" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils],
        ..Default::default()
    };

    let report = convert_path(&input, &output, &options).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // `fill` and `stroke` take the cell's own colours, not black.
    assert!(svg.contains("fill=\"#EDEDED\""), "{svg}");
    assert!(svg.contains("stroke=\"#884400\""), "{svg}");
    // A part filled with `stroke` takes the stroke colour as its fill.
    assert!(svg.contains("fill=\"#884400\""), "{svg}");
    // Nothing anywhere is painted black by accident.
    assert!(!svg.contains("fill=\"#000000\""), "{svg}");
}

/// Every shape draw.io implements in code that the example diagrams reach for
/// now draws. This walks the ones added last, which between them cover the
/// gradients, the stroke widths and the second colours the painter had to grow
/// to support, and asserts that none of them falls back to a placeholder.
#[test]
fn draws_every_coded_shape_the_example_diagrams_reach_for() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("all.drawio");
    let output = temporary.path().join("out");
    let shapes = [
        ("chart", "mxgraph.mockup.graphics.columnChart", ""),
        (
            "player",
            "mxgraph.mockup.containers.videoPlayer",
            "barHeight=30;barPos=40;",
        ),
        ("channel", "mxgraph.eip.messageChannel", ""),
        ("dead", "mxgraph.eip.deadLetterChannel", ""),
        ("tile", "mxgraph.ios7ui.icon", ""),
        ("picture", "mxgraph.bootstrap.image", ""),
        ("window", "mxgraph.mockup.containers.window", ""),
        (
            "scroll",
            "mxgraph.mockup.navigation.scrollBar",
            "barPos=40;",
        ),
        ("spin", "mxgraph.mockup.forms.spinner", ""),
        (
            "checks",
            "mxgraph.mockup.forms.checkboxGroup",
            "mainText=One,+Two;",
        ),
        ("pin", "mxgraph.mockup.misc.pin", ""),
        (
            "stars",
            "mxgraph.mockup.misc.rating",
            "grade=3;ratingScale=5;",
        ),
        (
            "hearts",
            "mxgraph.bootstrap.rating",
            "ratingStyle=heart;grade=2;ratingScale=4;",
        ),
        ("striped", "mxgraph.bootstrap.leftButtonStriped", ""),
        ("bus", "mxgraph.networks.bus", ""),
        ("face", "smileyFace", "smileyType=happy;"),
        ("sad", "smileyFace", "smileyType=sad;"),
        ("flat", "smileyFace", "smileyType=neutral;"),
        ("person", "mxgraph.mockup.containers.userMale", ""),
        ("map", "mxgraph.ios.iBgMap", ""),
        ("stripes", "mxgraph.ios.iBgStriped", ""),
        ("marker", "mxgraph.ios.iPin", ""),
        ("media", "mxgraph.gmdl.player", ""),
        (
            "gate",
            "mxgraph.electrical.logic_gates.logic_gate",
            "operation=and;numInputs=2;",
        ),
        (
            "orgate",
            "mxgraph.electrical.logic_gates.logic_gate",
            "operation=or;numInputs=3;",
        ),
        (
            "xorgate",
            "mxgraph.electrical.logic_gates.logic_gate",
            "operation=xor;negating=1;",
        ),
        ("locbar", "mxgraph.ios.iLocBar", "barPos=60;"),
        ("status", "mxgraph.android.statusBar", ""),
        ("callout", "mxgraph.infographic.circularCallout2", "dy=15;"),
        ("sheet", "mxgraph.ios7ui.actionDialog", ""),
        ("column", "mxgraph.pid2misc.column", "columnType=tray;"),
        ("trayless", "mxgraph.pid2misc.column", "columnType=fixed;"),
        ("valve", "mxgraph.pid2valves.valve", "valveType=ball;"),
        ("params", "mxgraph.sysml.actParamNode", ""),
        ("item", "mxgraph.sysml.itemFlow", "flowDir=e;"),
        (
            "times",
            "mxgraph.lean_mapping.timeline",
            "mainText=20,A,50,B,30,C;",
        ),
        ("state", "mxgraph.sysml.compState", ""),
        (
            "speech",
            "mxgraph.mockup.text.callout",
            "linkText=Note;callStyle=roundRect;",
        ),
        ("composite", "ext", "rounded=1;"),
        ("grouped", "mxgraph.aws4.groupCenter", ""),
    ];
    let mut body = String::from(
        "<mxfile><diagram name=\"All\"><mxGraphModel><root><mxCell id=\"0\"/><mxCell id=\"1\" parent=\"0\"/>",
    );
    for (index, (id, shape, extra)) in shapes.iter().enumerate() {
        let column = (index % 8) as i32 * 200;
        let row = (index / 8) as i32 * 200;
        body.push_str(&format!(
            "<mxCell id=\"{id}\" value=\"x\" style=\"shape={shape};{extra}html=1;\" vertex=\"1\" parent=\"1\">\
             <mxGeometry x=\"{column}\" y=\"{row}\" width=\"160\" height=\"120\" as=\"geometry\"/></mxCell>"
        ));
    }
    body.push_str("</root></mxGraphModel></diagram></mxfile>");
    drawio_file(&input, &body);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Every one of them drew something of its own, and none of them fell back
    // to the placeholder rectangle, which carries the shape name as its role.
    for (id, shape, _) in shapes {
        assert!(
            svg.contains(&format!("id=\"drawio-{id}\""))
                || svg.contains(&format!("id=\"drawio-{id}-")),
            "{shape} drew nothing"
        );
        assert!(
            !svg.contains(&format!("data-semantic-role=\"{shape}\"")),
            "{shape} fell back to a placeholder"
        );
    }
    // The gradients the painter grew to support reach the output as gradients.
    assert!(svg.contains("linearGradient"), "{svg}");
}

#[test]
fn converts_dxf_file_with_lines_circles_and_layers_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("drawing.dxf");
    let output = temporary.path().join("output");

    let dxf_data = r#"  0
SECTION
  2
TABLES
  0
TABLE
  2
LAYER
  0
LAYER
  2
Walls
 70
0
 62
1
  0
LAYER
  2
Columns
 70
0
 62
5
  0
LAYER
  2
HiddenLayer
 70
1
 62
2
  0
ENDTAB
  0
ENDSEC
  0
SECTION
  2
ENTITIES
  0
LINE
  8
Walls
 10
0.0
 20
0.0
 11
400.0
 21
300.0
  0
CIRCLE
  8
Columns
 10
200.0
 20
150.0
 40
50.0
  0
TEXT
  8
Walls
  1
Room 101
 10
50.0
 20
50.0
 40
12.0
  0
LINE
  8
HiddenLayer
 10
10.0
 20
10.0
 11
20.0
 21
20.0
  0
ENDSEC
  0
EOF
"#;
    fs::write(&input, dxf_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dxf);
    assert_eq!(report.page_count, 1);

    let svg_path = output.join("page-0001.svg");
    assert!(svg_path.exists());
    let svg = fs::read_to_string(svg_path).unwrap();

    // Verify layer groups
    assert!(svg.contains("id=\"layer-Walls\""));
    assert!(svg.contains("id=\"layer-Columns\""));
    // Frozen layer should NOT be rendered
    assert!(!svg.contains("id=\"layer-HiddenLayer\""));

    // Verify text presence
    assert!(svg.contains("Room 101"));

    // Verify manifest
    let manifest = fs::read_to_string(output.join("conversion.json")).unwrap();
    assert!(manifest.contains("\"source_format\": \"dxf\""));
}

#[test]
fn converts_dxf_blocks_and_lwpolyline_with_bulge() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("blocks_and_polylines.dxf");
    let output = temporary.path().join("output");

    let dxf_data = r#"  0
SECTION
  2
BLOCKS
  0
BLOCK
  2
CHAIR
 10
0.0
 20
0.0
  0
LINE
  8
Furniture
 10
-10.0
 20
-10.0
 11
10.0
 21
-10.0
  0
ENDBLK
  0
ENDSEC
  0
SECTION
  2
ENTITIES
  0
LWPOLYLINE
  8
Outline
 70
1
 90
2
 10
0.0
 20
0.0
 42
1.0
 10
100.0
 20
0.0
  0
INSERT
  8
Furniture
  2
CHAIR
 10
50.0
 20
50.0
 41
2.0
 42
2.0
 50
45.0
  0
ENDSEC
  0
EOF
"#;
    fs::write(&input, dxf_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dxf);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Arc path command from bulge
    assert!(svg.contains(" A "), "SVG should contain arc command: {svg}");
    // Layer for outline and furniture
    assert!(svg.contains("id=\"layer-Outline\""));
    assert!(svg.contains("id=\"layer-Furniture\""));
}

#[test]
fn converts_gerber_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("board.gbr");
    let output = temporary.path().join("output");

    let gerber_data = r#"%FSLAX24Y24*%
%MOMM*%
%ADD10C,0.8000*%
%ADD11R,1.5000X1.5000*%
D10*
X000000Y000000D02*
X020000Y020000D01*
D11*
X030000Y030000D03*
M02*
"#;
    fs::write(&input, gerber_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gerber);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"pcb-substrate\""));
    assert!(svg.contains("id=\"pcb-copper-layer\""));
}

#[test]
fn converts_hpgl_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("plot.plt");
    let output = temporary.path().join("output");

    let hpgl_data = "IN;SP1;PU0,0;PD500,0,500,500,0,500,0,0;PU;SP2;CI250;SP0;";
    fs::write(&input, hpgl_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Hpgl);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"pen-1\""));
    assert!(svg.contains("M "));
}

#[test]
fn converts_ansi_windows1252_dxf_file_with_degree_symbol() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("drawing_ansi.dxf");
    let output = temporary.path().join("output");

    // DXF file encoded in Windows-1252 with degree symbol (0xB0)
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        b"0\nSECTION\n2\nENTITIES\n0\nTEXT\n8\n0\n10\n0.0\n20\n0.0\n40\n10.0\n1\nANGLE 45",
    );
    bytes.push(0xB0); // degree sign in Windows-1252 / ISO-8859-1
    bytes.extend_from_slice(b"\n0\nENDSEC\n0\nEOF\n");

    fs::write(&input, bytes).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dxf);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("ANGLE 45°"),
        "SVG should contain decoded degree symbol: {svg}"
    );
}

#[test]
fn converts_dxf_dimension_leader_and_negative_layer() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("dim_leader.dxf");
    let output = temporary.path().join("output");

    let dxf_data = r#"  0
SECTION
  2
TABLES
  0
TABLE
  2
LAYER
  0
LAYER
  2
VisibleLayer
 70
0
 62
3
  6
CONTINUOUS
  0
LAYER
  2
HiddenLayer
 70
0
 62
-1
  6
CONTINUOUS
  0
ENDTAB
  0
ENDSEC
  0
SECTION
  2
BLOCKS
  0
BLOCK
  8
VisibleLayer
  2
*D0
 70
1
 10
0.0
 20
0.0
  0
LINE
  8
VisibleLayer
 10
10.0
 20
10.0
 11
100.0
 21
10.0
  0
MTEXT
  8
VisibleLayer
 10
50.0
 20
15.0
 40
5.0
  1
90.00mm
  0
ENDBLK
  0
ENDSEC
  0
SECTION
  2
ENTITIES
  0
DIMENSION
  8
VisibleLayer
  2
*D0
 10
0.0
 20
0.0
 11
50.0
 21
15.0
  0
LEADER
  8
VisibleLayer
 76
3
 10
20.0
 20
20.0
 10
30.0
 20
35.0
 10
50.0
 20
35.0
  0
LINE
  8
HiddenLayer
 10
999.0
 20
999.0
 11
1999.0
 21
1999.0
  0
ENDSEC
  0
EOF
"#;
    fs::write(&input, dxf_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dxf);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Block *D0 expanded from DIMENSION
    assert!(svg.contains("90.00mm"));
    // Leader line rendered
    assert!(svg.contains("id=\"layer-VisibleLayer\""));
    // HiddenLayer must not be rendered
    assert!(!svg.contains("id=\"layer-HiddenLayer\""));
}

#[test]
fn converts_gerber_arc_interpolation_and_plated_holes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("circuit.gbr");
    let output = temporary.path().join("output");

    let gbr_data = r#"%FSLAX25Y25*%
%MOMM*%
%ADD10C,0.20000*%
%ADD12C,1.50000X0.70000*%
D10*
X0000000Y0000000D02*
G02*
X0050000Y0050000I0050000J0000000D01*
G01*
D12*
X0100000Y0100000D03*
%LPC*%
X0080000Y0080000D03*
%LPD*%
M02*
"#;
    fs::write(&input, gbr_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gerber);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Gerber arc uses SVG A command
    assert!(svg.contains(" A "));
    // Plated hole pad renders hole cutout in background substrate color #143d22
    assert!(svg.contains("#143d22"));
    // Copper color is present
    assert!(svg.contains("#e8be38"));
}

#[test]
fn converts_hpgl_labels_and_rectangles_through_cli() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("plot.plt");
    let output = temporary.path().join("output");

    let plt_data = "IN;SP1;PU100,100;PD1000,100;EA2000,1500;PU500,500;RA1200,800;LBENGINEERING DRAWING REV 2\x03;SP0;";
    fs::write(&input, plt_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Hpgl);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ENGINEERING DRAWING REV 2"));
    assert!(svg.contains("pen-1"));
}

#[test]
fn converts_gcode_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mill.nc");
    let output = temporary.path().join("output");

    let gcode_data = r#"G21 G90
G00 X10.0 Y10.0 Z5.0
M03 S1500
G01 Z-2.0 F250
G01 X50.0 Y10.0 F800
G02 X70.0 Y30.0 R20.0
G01 X70.0 Y60.0
M05
G00 Z10.0
M02
"#;
    fs::write(&input, gcode_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gcode);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"cam:workspace\""));
    assert!(svg.contains("data-semantic-role=\"gcode:rapid\""));
    assert!(svg.contains("data-semantic-role=\"gcode:cut\""));
}

#[test]
fn converts_excellon_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("board.drl");
    let output = temporary.path().join("output");

    let excellon_data = r#"M48
METRIC
T01C0.8
T02C1.2
%
T01
X10.0Y15.0
X30.0Y15.0
T02
X20.0Y35.0
M30
"#;
    fs::write(&input, excellon_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Excellon);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"pcb:substrate\""));
    assert!(svg.contains("data-semantic-role=\"pcb:drill-holes\""));
}

#[test]
fn converts_stl_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bracket.stl");
    let output = temporary.path().join("output");

    let stl_data = r#"solid bracket
  facet normal 0 0 1
    outer loop
      vertex 0 0 5
      vertex 40 0 5
      vertex 40 30 5
    endloop
  endfacet
  facet normal 0 -1 0
    outer loop
      vertex 0 0 0
      vertex 40 0 0
      vertex 40 0 10
    endloop
  endfacet
endsolid bracket
"#;
    fs::write(&input, stl_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Stl);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("STL Slicer"));
    assert!(svg.contains("data-semantic-role=\"stl:background\""));
}

#[test]
fn converts_simulation_msh_and_vtk_files_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let msh_input = temporary.path().join("fem.msh");
    let vtk_input = temporary.path().join("thermal.vtk");
    let msh_out = temporary.path().join("out_msh");
    let vtk_out = temporary.path().join("out_vtk");

    let msh_data = r#"$MeshFormat
2.2 0 8
$EndMeshFormat
$Nodes
3
1 0.0 0.0 0.0
2 100.0 0.0 0.0
3 50.0 80.0 0.0
$EndNodes
$Elements
1
1 2 2 0 1 1 2 3
$EndElements
"#;
    fs::write(&msh_input, msh_data).unwrap();

    let report = convert_path(&msh_input, &msh_out, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Simulation);
    let msh_svg = fs::read_to_string(msh_out.join("page-0001.svg")).unwrap();
    assert!(msh_svg.contains("data-semantic-role=\"simulation:mesh\""));

    let vtk_data = r#"# vtk DataFile Version 3.0
Thermal Simulation
ASCII
DATASET UNSTRUCTURED_GRID
POINTS 3 float
0.0 0.0 0.0
100.0 0.0 0.0
50.0 80.0 0.0
CELLS 1 4
3 0 1 2
CELL_TYPES 1
5
POINT_DATA 3
SCALARS temperature float 1
LOOKUP_TABLE default
20.0 85.0 150.0
"#;
    fs::write(&vtk_input, vtk_data).unwrap();

    let vtk_report = convert_path(&vtk_input, &vtk_out, &ConvertOptions::default()).unwrap();
    assert_eq!(vtk_report.source_format, SourceFormat::Simulation);
    let vtk_svg = fs::read_to_string(vtk_out.join("page-0001.svg")).unwrap();
    assert!(vtk_svg.contains("colorbar-max"));
}

#[test]
fn converts_gmsh_v41_block_mesh_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("block_mesh.msh");
    let output = temporary.path().join("gmsh41_out");
    let msh = r#"$MeshFormat
4.1 0 8
$EndMeshFormat
$Nodes
1 6 1 6
2 1 0 6
1
2
3
4
5
6
0 0 0
1 0 0
1 1 0
0 1 0
2 0 0
2 1 0
$EndNodes
$Elements
1 2 1 2
2 1 3 2
1 1 2 3 4
2 2 5 6 3
$EndElements
$NodeData
1
"Temperature"
1
0.0
3
0
1
6
1 10.0
2 20.0
3 30.0
4 40.0
5 50.0
6 60.0
$EndNodeData
$ElementData
1
"Strain"
1
0.0
3
0
1
2
1 2.0
2 8.0
$EndElementData
"#;
    fs::write(&input, msh).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Simulation);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("color map uses cell values"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Gmsh MSH 4.1 FEA Mesh"));
    assert!(svg.contains("Temperature / Strain"));
    assert!(svg.contains("colorbar-max"));
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));
}

#[test]
fn converts_gmsh_v40_ascii_mesh_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mesh_v40.msh");
    let output = temporary.path().join("gmsh40_out");
    let msh = r#"$MeshFormat
4.0 0 8
$EndMeshFormat
$Nodes
1 4
1 2 0 4
10 0 0 0
20 50 0 0
30 50 50 0
40 0 50 0
$EndNodes
$Elements
1 2
1 2 2 2
100 10 20 30
200 10 30 40
$EndElements
"#;
    fs::write(&input, msh).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Simulation);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Gmsh MSH 4.0 FEA Mesh"));
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));
}

#[test]
fn converts_ascii_vtk_xml_unstructured_grid_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("stress.vtu");
    let output = temporary.path().join("vtu_out");
    let source = r#"<?xml version="1.0"?>
<VTKFile type="UnstructuredGrid" version="1.0" byte_order="LittleEndian">
  <UnstructuredGrid><Piece NumberOfPoints="4" NumberOfCells="2">
    <PointData Scalars="temperature"><DataArray type="Float32" Name="temperature" format="ascii">10 20 30 40</DataArray></PointData>
    <CellData Scalars="stress"><DataArray type="Float32" Name="stress" format="ascii">2 8</DataArray></CellData>
    <Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0 100 0 0 100 100 0 0 100 0</DataArray></Points>
    <Cells>
      <DataArray type="Int32" Name="connectivity" format="ascii">0 1 2 0 2 3</DataArray>
      <DataArray type="Int32" Name="offsets" format="ascii">3 6</DataArray>
      <DataArray type="UInt8" Name="types" format="ascii">5 5</DataArray>
    </Cells>
  </Piece></UnstructuredGrid>
</VTKFile>"#;
    fs::write(&input, source).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Simulation);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("color map uses cell values"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(svg.contains("colorbar-max"));
}

#[test]
fn converts_zlib_appended_vtk_xml_through_public_api() {
    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("stress.vtu");
    let output = temporary.path().join("vtu_binary_out");
    let mut appended = Vec::new();
    let mut arrays_xml = String::new();
    let mut cells_xml = String::new();

    let mut add_array = |type_name: &str, name: &str, raw: &[u8]| {
        let offset = appended.len();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(raw).unwrap();
        let compressed = encoder.finish().unwrap();
        appended.extend_from_slice(&1u32.to_le_bytes());
        appended.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        appended.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        appended.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        appended.extend_from_slice(&compressed);
        let context = match name {
            "points" => format!(
                "<Points><DataArray type=\"{type_name}\" NumberOfComponents=\"3\" format=\"appended\" offset=\"{offset}\"/></Points>"
            ),
            "temperature" => format!(
                "<PointData Scalars=\"temperature\"><DataArray type=\"{type_name}\" Name=\"{name}\" format=\"appended\" offset=\"{offset}\"/></PointData>"
            ),
            "stress" => format!(
                "<CellData Scalars=\"stress\"><DataArray type=\"{type_name}\" Name=\"{name}\" format=\"appended\" offset=\"{offset}\"/></CellData>"
            ),
            _ => format!(
                "<DataArray type=\"{type_name}\" Name=\"{name}\" format=\"appended\" offset=\"{offset}\"/>"
            ),
        };
        if matches!(name, "connectivity" | "offsets" | "types") {
            cells_xml.push_str(&context);
        } else {
            arrays_xml.push_str(&context);
        }
    };

    let mut points = Vec::new();
    for value in [
        0.0f32, 0.0, 0.0, 100.0, 0.0, 0.0, 100.0, 100.0, 0.0, 0.0, 100.0, 0.0,
    ] {
        points.extend_from_slice(&value.to_le_bytes());
    }
    add_array("Float32", "points", &points);
    let mut point_scalars = Vec::new();
    for value in [10.0f32, 20.0, 30.0, 40.0] {
        point_scalars.extend_from_slice(&value.to_le_bytes());
    }
    add_array("Float32", "temperature", &point_scalars);
    let mut cell_scalars = Vec::new();
    for value in [2.0f32, 8.0] {
        cell_scalars.extend_from_slice(&value.to_le_bytes());
    }
    add_array("Float32", "stress", &cell_scalars);
    let mut connectivity = Vec::new();
    for value in [0i32, 1, 2, 0, 2, 3] {
        connectivity.extend_from_slice(&value.to_le_bytes());
    }
    add_array("Int32", "connectivity", &connectivity);
    let mut offsets = Vec::new();
    for value in [3i32, 6] {
        offsets.extend_from_slice(&value.to_le_bytes());
    }
    add_array("Int32", "offsets", &offsets);
    add_array("UInt8", "types", &[5, 5]);
    arrays_xml.push_str(&format!("<Cells>{cells_xml}</Cells>"));

    let xml = format!(
        "<VTKFile type=\"UnstructuredGrid\" version=\"1.0\" byte_order=\"LittleEndian\" header_type=\"UInt32\" compressor=\"vtkZLibDataCompressor\"><UnstructuredGrid><Piece NumberOfPoints=\"4\" NumberOfCells=\"2\">{arrays_xml}</Piece></UnstructuredGrid><AppendedData encoding=\"base64\">_{}</AppendedData></VTKFile>",
        base64::engine::general_purpose::STANDARD.encode(appended)
    );
    fs::write(&input, xml).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Simulation);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("color map uses cell values"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(svg.contains("colorbar-max"));
}

fn cbz_png(red: u8, green: u8, blue: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[red, green, blue]).unwrap();
    }
    bytes
}

fn minimal_xps_package(page_source: &str, openxps: bool, utf16_parts: bool) -> Vec<u8> {
    let mut archive = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    let parts = [
        (
            "_rels/.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/xps/2005/06/fixedrepresentation" Target="/FixedDocumentSequence.fdseq"/></Relationships>"#,
        ),
        (
            "FixedDocumentSequence.fdseq",
            r#"<FixedDocumentSequence xmlns="http://schemas.microsoft.com/xps/2005/06"><DocumentReference Source="Documents/1/FixedDocument.fdoc"/></FixedDocumentSequence>"#,
        ),
        (
            "Documents/1/FixedDocument.fdoc",
            "<FixedDocument xmlns=\"http://schemas.microsoft.com/xps/2005/06\"><PageContent Source=\"{page_source}\" Width=\"200\" Height=\"120\"/></FixedDocument>",
        ),
        (
            "Documents/1/Pages/1.fpage",
            r##"<?xml version="1.0"?><FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06" Width="200" Height="120"><Path Fill="#FFFF0000" Data="F0 M 10,10 L 190,10 190,110 10,110 Z"/><Canvas Opacity="0.5" RenderTransform="0.75,0,0,0.75,5,0"><Path><Path.Fill><SolidColorBrush Color="#FF00FF00"/></Path.Fill><Path.Data><PathGeometry Figures="F1 M 20,20 L 180,20 180,100 20,100 Z"/></Path.Data></Path></Canvas><Path><Path.Fill><ImageBrush ImageSource="/Resources/Images/image1.png"/></Path.Fill><Path.Data><PathGeometry Figures="F1 M 30,60 L 170,60 170,100 30,100 Z"/></Path.Data></Path><Glyphs FontUri="/Resources/Fonts/missing.odttf" OriginX="20" OriginY="85" UnicodeString="XPS &amp; OXPS" FontRenderingEmSize="12" Fill="#FF0000FF"/></FixedPage>"##,
        ),
    ];
    for (name, contents) in parts {
        archive.start_file(name, options).unwrap();
        let mut contents = if name == "Documents/1/FixedDocument.fdoc" {
            contents.replace("{page_source}", page_source)
        } else {
            contents.to_owned()
        };
        if openxps {
            contents = contents.replace(
                "http://schemas.microsoft.com/xps/2005/06",
                "http://schemas.openxps.org/oxps/v1.0",
            );
        }
        if utf16_parts && name != "_rels/.rels" {
            archive.write_all(&[0xff, 0xfe]).unwrap();
            for code_unit in contents.encode_utf16() {
                archive.write_all(&code_unit.to_le_bytes()).unwrap();
            }
        } else {
            archive.write_all(contents.as_bytes()).unwrap();
        }
    }
    let mut image_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut image_bytes, 2, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0])
            .unwrap();
    }
    archive
        .start_file("Resources/Images/image1.png", options)
        .unwrap();
    archive.write_all(&image_bytes).unwrap();
    archive.finish().unwrap().into_inner()
}

#[test]
fn converts_xps_and_oxps_fixed_pages_with_bounded_package_parts() {
    let temporary = TempDir::new().unwrap();
    let xps_bytes = minimal_xps_package("Pages/1.fpage", false, false);
    let oxps_bytes = minimal_xps_package("Pages/1.fpage", true, true);
    for (filename, output_name, bytes) in [
        ("sample.xps", "xps_out", xps_bytes.clone()),
        ("sample.oxps", "oxps_out", oxps_bytes),
        ("extensionless", "xps_sniffed_out", xps_bytes),
    ] {
        let input = temporary.path().join(filename);
        let output = temporary.path().join(output_name);
        fs::write(&input, bytes).unwrap();
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, SourceFormat::Xps);
        assert_eq!(report.page_count, 1);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("embedded fonts"))
        );
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        assert!(svg.contains("width=\"150pt\" height=\"90pt\""));
        assert!(svg.contains("data-semantic-role=\"xps:path\""));
        assert!(svg.contains("data-semantic-role=\"xps:glyphs\""));
        assert!(svg.contains("XPS &amp; OXPS"));
        assert!(svg.contains("data:image/png;base64,"));
        assert!(svg.contains("clip-path=\"url(#xps-image-clip-"));
        assert!(svg.contains("matrix(0.75 0 0 0.75 5 0)"));
        assert!(svg.contains("opacity=\"0.5\""));
    }

    let traversal = minimal_xps_package("../../../escape.fpage", false, false);
    let traversal_path = temporary.path().join("traversal.xps");
    fs::write(&traversal_path, traversal).unwrap();
    assert!(matches!(
        convert_path(
            &traversal_path,
            temporary.path().join("traversal_out"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::InvalidInput(_))
    ));
}

#[test]
fn converts_dwfx_fixed_page_alias_through_xps_reader() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.dwfx");
    let output = temporary.path().join("dwfx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xps);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("DWFx preview"));
    assert!(svg.contains("data-semantic-role=\"xps:path\""));
}

#[test]
fn converts_nastran_op2_record_preflight_without_decoding_results() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.op2");
    let output = temporary.path().join("op2-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Op2);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Nastran OP2 preflight"));
    assert!(svg.contains("GEOM1"));
    assert!(svg.contains("OUGV1"));
    let extensionless = temporary.path().join("nastran-results");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Op2
    );
}

#[test]
fn converts_ipc2581_pcb_exchange_structure_without_rendering_manufacturing_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ipc2581");
    let output = temporary.path().join("ipc2581-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ipc2581);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("IPC-2581 PCB exchange"));
    assert!(svg.contains("Layers"));
    assert!(svg.contains("Components"));
    let extensionless = temporary.path().join("pcb-exchange");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ipc2581
    );
}

#[test]
fn converts_jt_fixed_header_without_loading_model_segments() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jt");
    let output = temporary.path().join("jt-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Jt);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Siemens JT header"));
    assert!(svg.contains("Version 10.6"));
    let extensionless = temporary.path().join("jt-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Jt
    );
}

#[test]
fn converts_cbz_images_in_natural_order_and_warns_on_unsupported_types() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("comic.cbz");
    let mut archive = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    for (name, bytes) in [
        ("ComicInfo.xml", b"<ComicInfo/>".to_vec()),
        ("Page 10.png", cbz_png(0, 0, 255)),
        ("Page 2.png", cbz_png(255, 0, 0)),
        ("Page 3.gif", b"GIF89a\x01\x00\x01\x00".to_vec()),
    ] {
        archive.start_file(name, options).unwrap();
        archive.write_all(&bytes).unwrap();
    }
    fs::write(&input, archive.finish().unwrap().into_inner()).unwrap();

    let output = temporary.path().join("cbz_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Cbz);
    assert_eq!(report.page_count, 2);
    assert!(report.warnings.iter().any(
        |warning| warning.contains("omitted 1 image member(s)") && warning.contains("PNG/JPEG")
    ));

    let first_svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = first_svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split_once('\"')
        .unwrap()
        .0;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut pixel = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixel).unwrap();
    assert_eq!(&pixel[..info.buffer_size()], &[255, 0, 0]);

    let limits = ConvertOptions {
        max_pages: 1,
        ..Default::default()
    };
    assert!(matches!(
        convert_path(&input, temporary.path().join("cbz_limited_out"), &limits),
        Err(document_svg::Error::LimitExceeded(_))
    ));
}

#[test]
fn converts_abaqus_inp_parts_instances_and_extensionless_meshes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("assembly.inp");
    let deck = "*Heading\nTwo translated triangles\n*Part, name=Triangle\n*Node\n1, 0, 0, 0\n2, 1, 0, 0\n3, 0, 1, 0\n*Element, type=CPS3\n1, 1, 2, 3\n*End Part\n*Assembly, name=Model\n*Instance, name=Left, part=Triangle\n*End Instance\n*Instance, name=Right, part=Triangle\n2, 0, 0\n*End Instance\n*End Assembly\n";
    fs::write(&input, deck).unwrap();

    let output = temporary.path().join("abaqus_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Abaqus);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(svg.contains("Two translated triangles"));

    let extensionless = temporary.path().join("mesh-data");
    fs::write(&extensionless, deck).unwrap();
    let sniffed_output = temporary.path().join("abaqus_sniffed_out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Abaqus);
    assert_eq!(sniffed.page_count, 1);
}

#[test]
fn converts_nastran_bulk_data_and_sniffs_extensionless_meshes() {
    let temporary = TempDir::new().unwrap();
    let deck = "$ Nastran free-field mesh\nGRID,1,,0.,0.,0.\nGRID,2,,1.,0.,0.\nGRID,3,,0.,1.,2.\nINCLUDE 'external.bdf'\nCTRIA3,10,1,1,2,3\nCQUAD9,11,1,1,2,3,1,2,3,1,2,3\nENDDATA\n";
    let input = temporary.path().join("plate.bdf");
    fs::write(&input, deck).unwrap();
    let output = temporary.path().join("nastran_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Nastran);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("INCLUDE"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("unsupported element"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(svg.contains("Nastran Bulk Data mesh"));

    let extensionless = temporary.path().join("mesh-data");
    let whitespace_deck = "$ Nastran whitespace free field\nGRID 1 0 0 0 0\nGRID 2 0 1 0 0\nGRID 3 0 0 1 0\nCTRIA3 10 1 1 2 3\n";
    fs::write(&extensionless, whitespace_deck).unwrap();
    let sniffed_output = temporary.path().join("nastran_sniffed_out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Nastran);
    assert_eq!(sniffed.page_count, 1);

    let alias = temporary.path().join("mesh.nastran");
    fs::write(&alias, deck).unwrap();
    let alias_output = temporary.path().join("nastran_alias_out");
    let alias_report = convert_path(&alias, &alias_output, &ConvertOptions::default()).unwrap();
    assert_eq!(alias_report.source_format, SourceFormat::Nastran);
}

#[test]
fn converts_lsdyna_keyword_mesh_and_sniffs_extensionless_files() {
    let temporary = TempDir::new().unwrap();
    let deck = "*KEYWORD\n*TITLE\nLS-DYNA panel\n*ELEMENT_SHELL\n1,1,1,2,3,4\n*NODE\n1,0,0,0\n2,100,0,0\n3,100,50,2\n4,0,50,2\n*INCLUDE\nmesh-extra.k\n*END\n";
    let input = temporary.path().join("panel.k");
    fs::write(&input, deck).unwrap();
    let output = temporary.path().join("lsdyna_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::LsDyna);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("INCLUDE"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("LS-DYNA panel"));
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));

    let extensionless = temporary.path().join("keyword-mesh");
    fs::write(&extensionless, deck).unwrap();
    let sniffed_output = temporary.path().join("lsdyna_sniffed_out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::LsDyna);
    assert_eq!(sniffed.page_count, 1);

    let alias = temporary.path().join("panel.key");
    fs::write(&alias, deck).unwrap();
    let alias_output = temporary.path().join("lsdyna_alias_out");
    let alias_report = convert_path(&alias, &alias_output, &ConvertOptions::default()).unwrap();
    assert_eq!(alias_report.source_format, SourceFormat::LsDyna);
}

#[test]
fn converts_jupyter_notebooks_and_sniffs_extensionless_json() {
    let temporary = TempDir::new().unwrap();
    let notebook = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            {"cell_type":"markdown", "metadata":{}, "source":["# Notebook report\n", "Short introduction."]},
            {"cell_type":"code", "execution_count":1, "metadata":{}, "source":["print(1 + 1)"], "outputs":[
                {"output_type":"stream", "name":"stdout", "text":["2\n"]}
            ]}
        ]
    });
    let source_text = serde_json::to_string(&notebook).unwrap();
    let input = temporary.path().join("analysis.ipynb");
    fs::write(&input, &source_text).unwrap();
    let output = temporary.path().join("jupyter_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Jupyter);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Notebook report"));
    assert!(svg.contains("print(1 + 1)"));
    assert!(svg.contains("2"));

    let extensionless = temporary.path().join("notebook-data");
    fs::write(&extensionless, source_text).unwrap();
    let sniffed_output = temporary.path().join("jupyter_sniffed_out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Jupyter);
}

#[test]
fn renders_jupyter_markdown_attachment_images_with_shared_limits() {
    let temporary = TempDir::new().unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jupyter_attachment.ipynb");
    let output = temporary.path().join("jupyter-attachment-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Jupyter);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("aria-label=\"attached chart\""));
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("attachments were omitted"))
    );
}

#[test]
fn previews_quarto_and_r_markdown_source_without_executing_chunks() {
    let temporary = TempDir::new().unwrap();
    let source_text = "---\ntitle: \"Simulation Notes\"\nauthor: Ada\ndate: 2026-09-13\nformat: pdf\n---\n\n## Results\nThe stored source shows a chunk.\n\n```{r, echo=FALSE}\nmean(c(1, 2, 3))\n```\n";
    let input = temporary.path().join("analysis.qmd");
    fs::write(&input, source_text).unwrap();
    let output = temporary.path().join("quarto_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Quarto);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("are not executed"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Simulation Notes"));
    assert!(svg.contains("mean(c(1, 2, 3))"));

    let alias = temporary.path().join("analysis.Rmd");
    fs::write(&alias, source_text).unwrap();
    let alias_output = temporary.path().join("rmarkdown_out");
    let alias_report = convert_path(&alias, &alias_output, &ConvertOptions::default()).unwrap();
    assert_eq!(alias_report.source_format, SourceFormat::Quarto);
}

#[test]
fn embeds_quarto_local_and_reference_images_without_executing_chunks() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/quarto_image.qmd");
    let input = temporary.path().join("report.qmd");
    fs::copy(&fixture, &input).unwrap();
    fs::create_dir_all(input.parent().unwrap().join("assets")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        input.parent().unwrap().join("assets/red-blue.png"),
    )
    .unwrap();
    let output = temporary.path().join("quarto-image-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Quarto);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("are not executed"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("external image resources"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("aria-label=\"Local Quarto image\""));
    assert!(svg.contains("aria-label=\"Reference Quarto image\""));
    assert!(!svg.contains("example.invalid"));
}

#[test]
fn converts_jats_article_sections_figures_and_tables() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jats");
    let input = temporary.path().join("article.jats");
    fs::copy(&fixture, &input).unwrap();
    fs::create_dir_all(input.parent().unwrap().join("assets")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        input.parent().unwrap().join("assets/red-blue.png"),
    )
    .unwrap();
    let output = temporary.path().join("jats-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Jats);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("JATS Article Preview"));
    assert!(svg.contains("Introduction"));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("Figure: Local figure"));
    assert!(svg.contains("Ready"));
}

#[test]
fn converts_docbook_article_with_lists_code_tables_and_local_image() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.docbook");
    let input = temporary.path().join("article.dbk");
    fs::copy(&fixture, &input).unwrap();
    fs::create_dir_all(input.parent().unwrap().join("assets")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        input.parent().unwrap().join("assets/red-blue.png"),
    )
    .unwrap();
    let output = temporary.path().join("docbook-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Docbook);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("DocBook Preview"));
    assert!(svg.contains("First item"));
    assert!(svg.contains("1.Ordered item"));
    assert!(svg.contains("never executed"));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("Figure: Validated local image"));
    assert!(svg.contains("Ready"));

    let extensionless = temporary.path().join("article");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed_output = temporary.path().join("docbook-sniffed");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Docbook);
}

#[test]
fn converts_dita_topic_and_ditamap_with_local_topics() {
    let temporary = TempDir::new().unwrap();
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let input = temporary.path().join("guide.dita");
    fs::copy(fixture_root.join("sample.dita"), &input).unwrap();
    fs::create_dir_all(input.parent().unwrap().join("assets")).unwrap();
    fs::copy(
        fixture_root.join("assets/red-blue.png"),
        input.parent().unwrap().join("assets/red-blue.png"),
    )
    .unwrap();
    let output = temporary.path().join("dita-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dita);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("DITA Preview"));
    assert!(svg.contains("1.Ordered topic item"));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("Ready"));

    fs::copy(
        fixture_root.join("sample.ditamap"),
        temporary.path().join("guide.ditamap"),
    )
    .unwrap();
    fs::copy(
        fixture_root.join("second.dita"),
        temporary.path().join("second.dita"),
    )
    .unwrap();
    let map_output = temporary.path().join("dita-map-out");
    let map_report = convert_path(
        temporary.path().join("guide.ditamap"),
        &map_output,
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(map_report.source_format, SourceFormat::Dita);
    let map_svg = fs::read_to_string(map_output.join("page-0001.svg")).unwrap();
    assert!(map_svg.contains("DITA Guide Map"));
    assert!(map_svg.contains("Second Topic"));

    let unsafe_map = temporary.path().join("unsafe.ditamap");
    fs::write(
        &unsafe_map,
        r#"<map xmlns="http://dita.oasis-open.org/architecture/1.3/"><title>Unsafe refs</title><topicref href="../outside.dita"/><topicref href="https://example.invalid/remote.dita"/></map>"#,
    )
    .unwrap();
    let unsafe_report = convert_path(
        &unsafe_map,
        temporary.path().join("unsafe-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        unsafe_report
            .warnings
            .iter()
            .any(|warning| warning.contains("outside the map directory"))
    );

    let extensionless = temporary.path().join("topic");
    fs::copy(fixture_root.join("sample.dita"), &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("dita-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Dita);
}

#[test]
fn converts_pdb_models_and_sniffs_extensionless_coordinates() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdb");
    let input = temporary.path().join("models.pdb");
    fs::copy(&fixture, &input).unwrap();
    let output = temporary.path().join("pdb-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Pdb);
    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(first.contains("SAMPLE PDB PREVIEW"));
    assert!(first.contains("chemical:bond"));

    let extensionless = temporary.path().join("coordinates");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("pdb-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Pdb);
}

#[test]
fn converts_hwpx_sections_tables_and_package_images() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.hwpx");
    let input = temporary.path().join("report.hwpx");
    fs::copy(&fixture, &input).unwrap();
    let output = temporary.path().join("hwpx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Hwpx);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("HWPX Preview"));
    assert!(svg.contains("Ready"));
    assert!(svg.contains("data:image/png;base64,"));

    let extensionless = temporary.path().join("report");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("hwpx-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Hwpx);
}

#[test]
fn converts_collada_geometry_and_sniffs_extensionless_mesh() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.dae");
    let input = temporary.path().join("triangle.dae");
    fs::copy(&fixture, &input).unwrap();
    let output = temporary.path().join("collada-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Collada);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("obj:background"));
    assert!(svg.contains("COLLADA materials"));

    let extensionless = temporary.path().join("triangle");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("collada-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Collada);
}

#[test]
fn converts_x3d_indexed_face_set_and_sniffs_extensionless_mesh() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.x3d");
    let input = temporary.path().join("triangle.x3d");
    fs::copy(&fixture, &input).unwrap();
    let output = temporary.path().join("x3d-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::X3d);
    assert!(
        fs::read_to_string(output.join("page-0001.svg"))
            .unwrap()
            .contains("obj:background")
    );

    let extensionless = temporary.path().join("triangle");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("x3d-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::X3d);
}

#[test]
fn converts_xmind_topic_outline_and_sniffs_extensionless_package() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xmind");
    let input = temporary.path().join("map.xmind");
    fs::copy(&fixture, &input).unwrap();
    let output = temporary.path().join("xmind-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xmind);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XMind Preview"));
    assert!(svg.contains("Child topic"));

    let extensionless = temporary.path().join("map");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("xmind-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Xmind);
}

#[test]
fn converts_xmind_json_content_package() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_json.xmind");
    let output = temporary.path().join("xmind-json-out");
    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xmind);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("JSON XMind Preview"));
    assert!(svg.contains("JSON Child"));
}

#[test]
fn converts_nifti_nii_and_gzip_volumes() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nii");
    let gz_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nii.gz");
    let input = temporary.path().join("volume.nii");
    fs::copy(&fixture, &input).unwrap();
    let report = convert_path(
        &input,
        temporary.path().join("nii-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Nifti);
    assert_eq!(report.page_count, 2);
    assert!(
        fs::read_to_string(temporary.path().join("nii-out/page-0001.svg"))
            .unwrap()
            .contains("NIfTI slice 1")
    );

    let gz_input = temporary.path().join("volume.nii.gz");
    fs::copy(&gz_fixture, &gz_input).unwrap();
    let gz_report = convert_path(
        &gz_input,
        temporary.path().join("nii-gz-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(gz_report.source_format, SourceFormat::Nifti);
    assert_eq!(gz_report.page_count, 2);
}

#[test]
fn converts_big_endian_nifti_integer_volume() {
    let temporary = TempDir::new().unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_big_endian.nii");
    let report = convert_path(
        &fixture,
        temporary.path().join("nifti-be-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Nifti);
    assert_eq!(report.page_count, 1);
    assert!(
        fs::read_to_string(temporary.path().join("nifti-be-out/page-0001.svg"))
            .unwrap()
            .contains("data:image/png;base64,")
    );
}

#[test]
fn converts_fits_and_gzip_image_planes() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fits");
    let gz_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fits.gz");
    let input = temporary.path().join("image.fits");
    fs::copy(&fixture, &input).unwrap();
    let report = convert_path(
        &input,
        temporary.path().join("fits-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Fits);
    assert_eq!(report.page_count, 2);
    assert!(
        fs::read_to_string(temporary.path().join("fits-out/page-0001.svg"))
            .unwrap()
            .contains("FITS image plane 1")
    );

    let gz_input = temporary.path().join("image.fits.gz");
    fs::copy(&gz_fixture, &gz_input).unwrap();
    let gz_report = convert_path(
        &gz_input,
        temporary.path().join("fits-gz-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(gz_report.source_format, SourceFormat::Fits);
    assert_eq!(gz_report.page_count, 2);
}

#[test]
fn converts_mrc_and_gzip_density_planes() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mrc");
    let gz_fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mrc.gz");
    let input = temporary.path().join("map.mrc");
    fs::copy(&fixture, &input).unwrap();
    let report = convert_path(
        &input,
        temporary.path().join("mrc-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Mrc);
    assert_eq!(report.page_count, 2);
    let gz_input = temporary.path().join("map.mrc.gz");
    fs::copy(&gz_fixture, &gz_input).unwrap();
    let gz_report = convert_path(
        &gz_input,
        temporary.path().join("mrc-gz-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(gz_report.source_format, SourceFormat::Mrc);
    assert_eq!(gz_report.page_count, 2);
}

#[test]
fn converts_big_endian_mrc_density_plane() {
    let temporary = TempDir::new().unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_big_endian.mrc");
    let report = convert_path(
        &fixture,
        temporary.path().join("mrc-be-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Mrc);
    assert_eq!(report.page_count, 1);
    assert!(
        fs::read_to_string(temporary.path().join("mrc-be-out/page-0001.svg"))
            .unwrap()
            .contains("MRC plane 1")
    );
}

#[test]
fn converts_read_only_sqlite_user_tables() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sqlite");
    let input = temporary.path().join("data.sqlite");
    fs::copy(&fixture, &input).unwrap();
    let output = temporary.path().join("sqlite-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Sqlite);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("people"));
    assert!(svg.contains("Alice"));
    assert!(svg.contains("metrics"));
}

#[test]
fn converts_cif_atom_site_models_and_sniffs_extensionless_coordinates() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cif");
    let input = temporary.path().join("structure.cif");
    fs::copy(&fixture, &input).unwrap();
    let report = convert_path(
        &input,
        temporary.path().join("cif-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Cif);
    assert_eq!(report.page_count, 2);
    let extensionless = temporary.path().join("structure");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("cif-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Cif);
}

#[test]
fn converts_mol2_atoms_bonds_and_sniffs_extensionless_structure() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mol2");
    let input = temporary.path().join("structure.mol2");
    fs::copy(&fixture, &input).unwrap();
    let report = convert_path(
        &input,
        temporary.path().join("mol2-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Mol2);
    let svg = fs::read_to_string(temporary.path().join("mol2-out/page-0001.svg")).unwrap();
    assert!(svg.contains("chemical:bond"));
    let extensionless = temporary.path().join("structure");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("mol2-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Mol2);
}

#[test]
fn converts_nquads_graph_column() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nq");
    let report = convert_path(
        &fixture,
        temporary.path().join("nq-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Turtle);
    let svg = fs::read_to_string(temporary.path().join("nq-out/page-0001.svg")).unwrap();
    assert!(svg.contains("Graph"));
    assert!(svg.contains("Graph"));
}

#[test]
fn converts_turtle_semicolon_and_comma_continuations() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_multiline.ttl");
    let report = convert_path(
        &fixture,
        temporary.path().join("turtle-multiline-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Turtle);
    let svg =
        fs::read_to_string(temporary.path().join("turtle-multiline-out/page-0001.svg")).unwrap();
    assert!(svg.contains("score"));
    assert!(svg.contains("11"));
}

#[test]
fn converts_turtle_rdf_statements_and_sniffs_extensionless_file() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ttl");
    let input = temporary.path().join("graph.ttl");
    fs::copy(&fixture, &input).unwrap();
    let report = convert_path(
        &input,
        temporary.path().join("turtle-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Turtle);
    let svg = fs::read_to_string(temporary.path().join("turtle-out/page-0001.svg")).unwrap();
    assert!(svg.contains("RDF statements"));
    assert!(svg.contains("Alice"));
    let extensionless = temporary.path().join("graph");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("turtle-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Turtle);
}

#[test]
fn converts_bounded_eps_paths_and_sniffs_extensionless_postscript() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.eps");
    let input = temporary.path().join("triangle.eps");
    fs::copy(&fixture, &input).unwrap();
    let report = convert_path(
        &input,
        temporary.path().join("eps-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Eps);
    assert!(
        fs::read_to_string(temporary.path().join("eps-out/page-0001.svg"))
            .unwrap()
            .contains("eps:path")
    );
    let extensionless = temporary.path().join("triangle");
    fs::copy(&fixture, &extensionless).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("eps-sniffed"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Eps);
}

#[test]
fn converts_postscript_showpage_sequence_as_multiple_pages() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multi-page.ps");
    let report = convert_path(
        &fixture,
        temporary.path().join("ps-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Eps);
    assert_eq!(report.page_count, 2);
    assert!(temporary.path().join("ps-out/page-0002.svg").exists());
}

#[test]
fn rejects_postscript_showpage_sequence_over_max_pages() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/multi-page.ps");
    let options = ConvertOptions {
        max_pages: 1,
        ..ConvertOptions::default()
    };
    let error =
        convert_path(&fixture, temporary.path().join("ps-limited-out"), &options).unwrap_err();
    assert!(error.to_string().contains("PostScript exceeded max_pages"));
}

#[test]
fn converts_multiple_mol2_molecule_records_as_pages() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_multi.mol2");
    let report = convert_path(
        &fixture,
        temporary.path().join("mol2-multi-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(report.source_format, SourceFormat::Mol2);
    assert_eq!(report.page_count, 2);
    assert!(
        fs::read_to_string(temporary.path().join("mol2-multi-out/page-0002.svg"))
            .unwrap()
            .contains("Second molecule")
    );
}

#[test]
fn rejects_sqlite_sidecar_that_exceeds_the_input_budget() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sqlite");
    let input = temporary.path().join("data.sqlite");
    fs::copy(&fixture, &input).unwrap();
    fs::write(
        temporary.path().join("data.sqlite-shm"),
        vec![0u8; 16 * 1024 * 1024 + 1],
    )
    .unwrap();
    let error = convert_path(
        &input,
        temporary.path().join("sqlite-sidecar-out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("sidecar '-shm' exceeds"));
}

#[test]
fn converts_medit_ascii_mesh_and_sniffs_extensionless_files() {
    let temporary = TempDir::new().unwrap();
    let mesh = "# MEDIT sample\nMeshVersionFormatted 2\nDimension 3\nVertices\n4\n0 0 0 1\n1 0 0 1\n0 1 0 1\n0 0 1 1\nEdges 1\n1 2 0\nTriangles\n1\n1 2 3 5\nTetrahedra\n1\n1 2 3 4 7\nRequiredVertices\n1\n1\nEnd\n";
    let input = temporary.path().join("mesh.mesh");
    fs::write(&input, mesh).unwrap();
    let output = temporary.path().join("medit_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Medit);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("XY projection"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("MEDIT finite-element mesh"));
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));

    let extensionless = temporary.path().join("medit-data");
    fs::write(&extensionless, mesh).unwrap();
    let sniffed_output = temporary.path().join("medit_sniffed_out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Medit);
}

#[test]
fn converts_binary_medit_meshb_and_sniffs_extensionless_header() {
    let temporary = TempDir::new().unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/medit_binary_v2.meshb");
    let bytes = fs::read(fixture).unwrap();
    assert_eq!(bytes, common::medit_binary_tetrahedron());
    let input = temporary.path().join("volume.meshb");
    fs::write(&input, &bytes).unwrap();
    let output = temporary.path().join("meshb_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Medit);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("XY projection"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("MEDIT binary finite-element mesh"));
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));

    let extensionless = temporary.path().join("meshb-data");
    fs::write(&extensionless, bytes).unwrap();
    let sniffed_output = temporary.path().join("meshb_sniffed_out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Medit);
    assert_eq!(sniffed.page_count, 1);
}

#[test]
fn converts_off_polygon_mesh_and_sniffs_header_variants() {
    let temporary = TempDir::new().unwrap();
    let mesh = "# simple OFF quad\nOFF 4 1 4\n0 0 0\n100 0 0\n100 100 0\n0 100 0\n4 0 1 2 3\n";
    let input = temporary.path().join("surface.off");
    fs::write(&input, mesh).unwrap();
    let output = temporary.path().join("off_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Off);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OFF polygon mesh"));
    assert!(svg.contains("data-semantic-role=\"obj:mesh\""));

    let extensionless = temporary.path().join("off-data");
    fs::write(&extensionless, mesh).unwrap();
    let sniffed_output = temporary.path().join("off_sniffed_out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Off);

    let alias = temporary.path().join("surface.coff");
    fs::write(&alias, mesh.replace("OFF", "COFF")).unwrap();
    let alias_output = temporary.path().join("coff_out");
    let alias_report = convert_path(&alias, &alias_output, &ConvertOptions::default()).unwrap();
    assert_eq!(alias_report.source_format, SourceFormat::Off);
    assert!(
        alias_report
            .warnings
            .iter()
            .any(|warning| warning.contains("colors"))
    );
}

#[test]
fn converts_ifc4_tessellated_geometry_and_sniffs_extensionless_model() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ifc");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::Ifc);
    let output = temporary.path().join("ifc_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ifc);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("materials"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("IFC tessellated building model"));
    assert!(svg.contains("data-semantic-role=\"obj:mesh\""));

    let extensionless = temporary.path().join("ifc-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ifc
    );
    let sniffed_output = temporary.path().join("ifc_sniffed");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Ifc);
}

#[test]
fn converts_ifc_mapped_tessellations_with_product_and_map_transforms() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_mapped.ifc");
    let output = temporary.path().join("ifc_mapped_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ifc);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("materials"))
    );
    assert!(!report.warnings.iter().any(|warning| {
        warning.contains("mapped geometry") || warning.contains("mapped representation")
    }));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("IFC tessellated building model"));
    assert!(svg.contains("data-semantic-role=\"obj:mesh\""));

    let extensionless = temporary.path().join("mapped-building-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ifc
    );

    let nonuniform = temporary.path().join("nonuniform-mapped.ifc");
    let fixture = fs::read_to_string(&input).unwrap();
    let nonuniform_fixture = fixture.replace(
        "#11=IFCCARTESIANTRANSFORMATIONOPERATOR3D(#7,#8,#10,2.,#9);",
        "#11=IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM(#7,#8,#10,2.,#9,1.,1.);",
    );
    assert_ne!(nonuniform_fixture, fixture);
    fs::write(&nonuniform, &nonuniform_fixture).unwrap();
    let partial = convert_path(
        &nonuniform,
        temporary.path().join("nonuniform_out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(partial.page_count, 1);
    assert!(
        partial
            .warnings
            .iter()
            .any(|warning| { warning.contains("unsupported mapped/swept geometry") })
    );

    let unsupported_only = temporary.path().join("unsupported-only-mapped.ifc");
    let unsupported_fixture = nonuniform_fixture.replace(
        "#14=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#13,$,$);",
        "#14=IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM($,$,#13,$,$,$,$);",
    );
    assert_ne!(unsupported_fixture, nonuniform_fixture);
    fs::write(&unsupported_only, unsupported_fixture).unwrap();
    let error = convert_path(
        &unsupported_only,
        temporary.path().join("unsupported_only_out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("no supported"));
}

#[test]
fn converts_ifc_extruded_area_solids_from_common_profiles() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_extruded.ifc");
    let output = temporary.path().join("ifc_extruded_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ifc);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("materials"))
    );
    assert!(!report.warnings.iter().any(|warning| {
        warning.contains("unsupported mapped") || warning.contains("extruded profile")
    }));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("IFC tessellated building model"));
    assert!(svg.contains("data-semantic-role=\"obj:mesh\""));

    let extensionless = temporary.path().join("extruded-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ifc
    );
}

#[test]
fn converts_bounded_ifcxml_geometry_and_rejects_dtds() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_ifcxml.ifcxml");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::IfcXml);
    let xml_with_ifc_extension = temporary.path().join("model.ifc");
    fs::copy(&input, &xml_with_ifc_extension).unwrap();
    assert_eq!(
        SourceFormat::detect(&xml_with_ifc_extension).unwrap(),
        SourceFormat::IfcXml
    );
    let output = temporary.path().join("ifcxml_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::IfcXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"obj:mesh\""));

    let extensionless = temporary.path().join("ifcxml-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::IfcXml
    );
    let sniffed_output = temporary.path().join("ifcxml_sniffed_out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::IfcXml);

    let wrong_extension = temporary.path().join("step-data.ifcxml");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ifc"),
        &wrong_extension,
    )
    .unwrap();
    let wrong_error = convert_path(
        &wrong_extension,
        temporary.path().join("wrong_extension_out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(
        wrong_error
            .to_string()
            .contains("buildingSMART IFCXML root")
    );

    let doctype = temporary.path().join("dtd.ifcxml");
    fs::write(
        &doctype,
        "<!DOCTYPE ifcXML [<!ENTITY ext SYSTEM 'file:///etc/passwd'>]><ifc:ifcXML xmlns:ifc='http://www.buildingsmart-tech.org/ifcXML/IFC4/Add2'><IfcProject Name='&ext;'/></ifc:ifcXML>",
    )
    .unwrap();
    let error = convert_path(
        &doctype,
        temporary.path().join("dtd_out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("document type declarations"));
}

#[test]
fn converts_ifc4_indexed_concave_polygon_faces() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("polygon.ifc");
    fs::write(
        &input,
        "ISO-10303-21;HEADER;FILE_SCHEMA(('IFC4'));ENDSEC;DATA;\
         #1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(400.,0.,0.),(400.,200.,0.),(200.,200.,0.),(200.,400.,0.),(0.,400.,0.)),$);\
         #2=IFCINDEXEDPOLYGONALFACE((1,2,3,4,5,6));\
         #3=IFCPOLYGONALFACESET(#1,.F.,(#2),$);\
         #4=IFCSHAPEREPRESENTATION($,'Body','Tessellation',(#3));\
         #5=IFCPRODUCTDEFINITIONSHAPE($,$,(#4));\
         #6=IFCCARTESIANPOINT((0.,0.,0.));\
         #7=IFCAXIS2PLACEMENT3D(#6,$,$);\
         #8=IFCLOCALPLACEMENT($,#7);\
         #9=IFCBUILDINGELEMENTPROXY('2L$!Polygon',$,'Quad',$,$,#8,#5,$,$);\
         ENDSEC;END-ISO-10303-21;",
    )
    .unwrap();
    let output = temporary.path().join("polygon_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ifc);
    assert_eq!(report.page_count, 1);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("polygon"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"obj:mesh\""));
}

#[test]
fn converts_ifczip_with_one_root_model_and_ignores_sidecar_files() {
    let temporary = TempDir::new().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ifc");
    let input = temporary.path().join("building.ifczip");
    let mut archive = ZipWriter::new(File::create(&input).unwrap());
    archive
        .start_file("building.ifc", SimpleFileOptions::default())
        .unwrap();
    archive.write_all(&fs::read(&source).unwrap()).unwrap();
    archive
        .start_file("textures/material.png", SimpleFileOptions::default())
        .unwrap();
    archive.write_all(b"sidecar is not loaded").unwrap();
    archive.finish().unwrap();

    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::IfcZip);
    let output = temporary.path().join("ifczip_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::IfcZip);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("sidecar"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"ifczip\""));

    let extensionless = temporary.path().join("building-archive");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::IfcZip
    );

    let nested_input = temporary.path().join("nested.ifczip");
    let mut nested = ZipWriter::new(File::create(&nested_input).unwrap());
    nested
        .start_file("models/building.ifc", SimpleFileOptions::default())
        .unwrap();
    nested.write_all(&fs::read(&source).unwrap()).unwrap();
    nested.finish().unwrap();
    assert!(
        convert_path(
            &nested_input,
            temporary.path().join("nested_out"),
            &ConvertOptions::default()
        )
        .is_err()
    );
}

#[test]
fn converts_su2_cfd_mesh_and_sniffs_extensionless_file() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.su2");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::Su2);
    let output = temporary.path().join("su2_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Su2);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("marker name"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SU2 CFD mesh"));
    assert!(svg.contains("data-source-format=\"su2\""));
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));

    let extensionless = temporary.path().join("cfd-mesh");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Su2
    );
}

#[test]
fn converts_openfoam_ascii_poly_mesh_from_case_marker() {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openfoam_case/sample.foam");
    assert_eq!(
        SourceFormat::detect(&input).unwrap(),
        SourceFormat::OpenFoam
    );
    let temporary = TempDir::new().unwrap();
    let output = temporary.path().join("openfoam_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OpenFoam);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("patch names"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OpenFOAM polyMesh"));
    assert!(svg.contains("data-source-format=\"openfoam\""));
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(svg.contains("data-semantic-role=\"simulation:boundary-patch\""));
}

#[test]
fn converts_gzip_compressed_ascii_openfoam_poly_mesh_with_expansion_limits() {
    let temporary = TempDir::new().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openfoam_case");
    let case = temporary.path().join("compressed-case");
    let poly_mesh = case.join("constant/polyMesh");
    fs::create_dir_all(&poly_mesh).unwrap();
    fs::copy(source.join("sample.foam"), case.join("case.foam")).unwrap();
    for name in ["points", "faces", "owner", "neighbour", "boundary"] {
        let input = fs::read(source.join("constant/polyMesh").join(name)).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&input).unwrap();
        fs::write(
            poly_mesh.join(format!("{name}.gz")),
            encoder.finish().unwrap(),
        )
        .unwrap();
    }
    let output = temporary.path().join("compressed_openfoam_out");
    let report = convert_path(case.join("case.foam"), &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OpenFoam);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("simulation:boundary-patch"));

    let bomb_case = temporary.path().join("expanded-case");
    let bomb_poly_mesh = bomb_case.join("constant/polyMesh");
    fs::create_dir_all(&bomb_poly_mesh).unwrap();
    fs::copy(source.join("sample.foam"), bomb_case.join("case.foam")).unwrap();
    for name in ["points", "faces", "owner", "neighbour", "boundary"] {
        let mut input = fs::read(source.join("constant/polyMesh").join(name)).unwrap();
        if name == "points" {
            let comment = format!("/*{}*/\n", "x".repeat(8192));
            input.splice(0..0, comment.bytes());
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&input).unwrap();
        fs::write(
            bomb_poly_mesh.join(format!("{name}.gz")),
            encoder.finish().unwrap(),
        )
        .unwrap();
    }
    let limited = ConvertOptions {
        max_input_bytes: 4096,
        ..ConvertOptions::default()
    };
    let error = convert_path(
        bomb_case.join("case.foam"),
        temporary.path().join("expanded_openfoam_out"),
        &limited,
    )
    .unwrap_err();
    assert!(error.to_string().contains("decompressed OpenFOAM file"));
}

#[cfg(unix)]
#[test]
fn rejects_openfoam_poly_mesh_sidecars_that_escape_the_case_root() {
    use std::os::unix::fs::symlink;

    let temporary = TempDir::new().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openfoam_case");
    let case = temporary.path().join("case");
    let poly_mesh = case.join("constant/polyMesh");
    fs::create_dir_all(&poly_mesh).unwrap();
    fs::copy(source.join("sample.foam"), case.join("case.foam")).unwrap();
    for name in ["faces", "owner", "neighbour", "boundary"] {
        fs::copy(
            source.join("constant/polyMesh").join(name),
            poly_mesh.join(name),
        )
        .unwrap();
    }
    let outside = temporary.path().join("outside-points");
    fs::copy(source.join("constant/polyMesh/points"), &outside).unwrap();
    symlink(&outside, poly_mesh.join("points")).unwrap();

    let error = convert_path(
        case.join("case.foam"),
        temporary.path().join("out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("escapes the case directory"));

    let compressed_case = temporary.path().join("compressed-case");
    let compressed_poly_mesh = compressed_case.join("constant/polyMesh");
    fs::create_dir_all(&compressed_poly_mesh).unwrap();
    fs::copy(
        source.join("sample.foam"),
        compressed_case.join("case.foam"),
    )
    .unwrap();
    for name in ["faces", "owner", "neighbour", "boundary"] {
        fs::copy(
            source.join("constant/polyMesh").join(name),
            compressed_poly_mesh.join(name),
        )
        .unwrap();
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&fs::read(source.join("constant/polyMesh/points")).unwrap())
        .unwrap();
    let outside_gzip = temporary.path().join("outside-points.gz");
    fs::write(&outside_gzip, encoder.finish().unwrap()).unwrap();
    symlink(&outside_gzip, compressed_poly_mesh.join("points.gz")).unwrap();
    let gzip_error = convert_path(
        compressed_case.join("case.foam"),
        temporary.path().join("gzip_escape_out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(
        gzip_error
            .to_string()
            .contains("escapes the case directory")
    );
}

#[test]
fn converts_multiframe_tiff_as_color_svg_pages_and_sniffs_header() {
    use tiff::encoder::{TiffEncoder, colortype};

    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("multipage.tiff");
    {
        let file = fs::File::create(&input).unwrap();
        let mut encoder = TiffEncoder::new(file).unwrap();
        let mut image = encoder.new_image::<colortype::RGB8>(3, 2).unwrap();
        image
            .encoder()
            .write_tag(tiff::tags::Tag::Orientation, 6u16)
            .unwrap();
        image
            .write_data(&[
                255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0, 0, 255, 255, 255, 0, 255,
            ])
            .unwrap();
        encoder
            .write_image::<colortype::Gray8>(2, 1, &[0, 255])
            .unwrap();
        encoder
            .write_image::<colortype::RGB16>(1, 1, &[0, 32_768, 65_535])
            .unwrap();
        encoder
            .write_image::<colortype::CMYK8>(1, 1, &[255, 0, 0, 0])
            .unwrap();
    }

    let output = temporary.path().join("tiff_out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Tiff);
    assert_eq!(report.page_count, 4);
    assert!(
        report.pages[2]
            .warnings
            .iter()
            .any(|warning| warning.contains("reduced from 16-bit"))
    );
    assert!(
        report.pages[3]
            .warnings
            .iter()
            .any(|warning| warning.contains("CMYK samples"))
    );
    assert_eq!(report.pages[0].width_points, 2.0);
    assert_eq!(report.pages[0].height_points, 3.0);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("matrix(0 1 -1 0 2 0)"));
    let encoded = svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0;
    let png_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(
        &pixels[..info.buffer_size()],
        &[
            255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0, 0, 255, 255, 255, 0, 255
        ]
    );

    let gray_svg = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(gray_svg.contains("<image"));
    let high_depth_svg = fs::read_to_string(output.join("page-0003.svg")).unwrap();
    let encoded = high_depth_svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split_once('\"')
        .unwrap()
        .0;
    let png_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(&pixels[..info.buffer_size()], &[0, 128, 255]);

    let cmyk_svg = fs::read_to_string(output.join("page-0004.svg")).unwrap();
    let encoded = cmyk_svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split_once('\"')
        .unwrap()
        .0;
    let png_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(&pixels[..info.buffer_size()], &[0, 255, 255]);

    let extensionless = temporary.path().join("content-sniffed");
    fs::copy(&input, &extensionless).unwrap();
    let sniffed_output = temporary.path().join("tiff_sniffed_out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Tiff);
    assert_eq!(sniffed.page_count, 4);

    let limited_output = temporary.path().join("tiff_limited_out");
    let limits = ConvertOptions {
        max_pages: 1,
        ..Default::default()
    };
    assert!(matches!(
        convert_path(&input, &limited_output, &limits),
        Err(document_svg::Error::LimitExceeded(_))
    ));

    let big_tiff = temporary.path().join("bigtiff.dat");
    {
        let mut encoder = TiffEncoder::new_big(fs::File::create(&big_tiff).unwrap()).unwrap();
        encoder
            .write_image::<colortype::Gray8>(2, 1, &[32, 224])
            .unwrap();
    }
    let big_output = temporary.path().join("bigtiff_out");
    let big_report = convert_path(&big_tiff, &big_output, &ConvertOptions::default()).unwrap();
    assert_eq!(big_report.source_format, SourceFormat::Tiff);
    assert_eq!(big_report.page_count, 1);
}

#[test]
fn converts_dicom_multiframe_and_rgb_without_serializing_patient_metadata() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let multiframe = fixture_dir.join("sample_multiframe.dcm");
    assert_eq!(
        SourceFormat::detect(&multiframe).unwrap(),
        SourceFormat::Dicom
    );
    let multiframe_output = temporary.path().join("dicom-multiframe-out");
    let report = convert_path(&multiframe, &multiframe_output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dicom);
    assert_eq!(report.page_count, 2);
    for page_number in 1..=2 {
        let svg = fs::read_to_string(multiframe_output.join(format!("page-{page_number:04}.svg")))
            .unwrap();
        assert!(svg.contains("DICOM Image Frame"));
        assert!(svg.contains("data:image/png;base64,"));
        assert!(svg.contains("data-semantic-role=\"dicom:image-frame\""));
        assert!(!svg.contains("Doe^DICOM QA"));
    }
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("not de-identification"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("2 sequential SVG pages"))
    );

    let extensionless = temporary.path().join("medical-image");
    fs::copy(&multiframe, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Dicom
    );
    assert_eq!(
        convert_path(
            &extensionless,
            temporary.path().join("dicom-sniffed-out"),
            &ConvertOptions::default(),
        )
        .unwrap()
        .source_format,
        SourceFormat::Dicom
    );

    let rgb = fixture_dir.join("sample_rgb.dcm");
    let rgb_report = convert_path(
        &rgb,
        temporary.path().join("dicom-rgb-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(rgb_report.page_count, 1);
    let rgb_svg = fs::read_to_string(temporary.path().join("dicom-rgb-out/page-0001.svg")).unwrap();
    assert!(rgb_svg.contains("data:image/png;base64,"));
    assert!(!rgb_svg.contains("Private^Image"));

    let ct = fixture_dir.join("sample_ct_16bit.dcm");
    let ct_report = convert_path(
        &ct,
        temporary.path().join("dicom-ct-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(ct_report.page_count, 1);
    assert!(
        ct_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("converted to 8-bit display images"))
    );
    let ct_svg = fs::read_to_string(temporary.path().join("dicom-ct-out/page-0001.svg")).unwrap();
    assert!(!ct_svg.contains("Private^CT"));
    let encoded = ct_svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split_once('\"')
        .unwrap()
        .0;
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);

    let implicit = fixture_dir.join("sample_implicit.dcm");
    let implicit_report = convert_path(
        &implicit,
        temporary.path().join("dicom-implicit-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(implicit_report.source_format, SourceFormat::Dicom);
    assert_eq!(implicit_report.page_count, 1);

    let rle = fixture_dir.join("sample_rle.dcm");
    let rle_report = convert_path(
        &rle,
        temporary.path().join("dicom-rle-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(rle_report.source_format, SourceFormat::Dicom);
    assert_eq!(rle_report.page_count, 1);

    let jpeg2000 = fixture_dir.join("sample_jpeg2000.dcm");
    let jpeg2000_output = temporary.path().join("dicom-jpeg2000-out");
    let jpeg2000_report =
        convert_path(&jpeg2000, &jpeg2000_output, &ConvertOptions::default()).unwrap();
    assert_eq!(jpeg2000_report.source_format, SourceFormat::Dicom);
    assert_eq!(jpeg2000_report.page_count, 1);
    let jpeg2000_svg = fs::read_to_string(jpeg2000_output.join("page-0001.svg")).unwrap();
    let encoded = jpeg2000_svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split_once('"')
        .unwrap()
        .0;
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut decoded_pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut decoded_pixels).unwrap();
    assert_eq!((info.width, info.height), (4, 4));
    assert_eq!(info.color_type, png::ColorType::Grayscale);
    assert_eq!(
        &decoded_pixels[..info.buffer_size()],
        &[
            0, 32, 64, 96, 32, 64, 96, 128, 64, 96, 128, 160, 96, 128, 160, 255
        ]
    );

    let mut jpeg2000_general = fs::read(&jpeg2000).unwrap();
    let lossless_uid = b"1.2.840.10008.1.2.4.90";
    let general_uid = b"1.2.840.10008.1.2.4.91";
    let transfer_syntax_offset = jpeg2000_general
        .windows(lossless_uid.len())
        .position(|window| window == lossless_uid)
        .unwrap();
    jpeg2000_general[transfer_syntax_offset..transfer_syntax_offset + lossless_uid.len()]
        .copy_from_slice(general_uid);
    let jpeg2000_general_path = temporary.path().join("general-jpeg2000.dcm");
    fs::write(&jpeg2000_general_path, jpeg2000_general).unwrap();
    assert_eq!(
        convert_path(
            &jpeg2000_general_path,
            temporary.path().join("general-jpeg2000-out"),
            &ConvertOptions::default()
        )
        .unwrap()
        .page_count,
        1
    );

    let mut oversized_jpeg2000 = fs::read(&jpeg2000).unwrap();
    let codestream = fs::read(fixture_dir.join("sample_jpeg2000.j2k")).unwrap();
    let codestream_offset = oversized_jpeg2000
        .windows(codestream.len())
        .position(|window| window == codestream)
        .unwrap();
    let siz_marker = codestream
        .windows(2)
        .position(|window| window == [0xff, 0x51])
        .unwrap();
    let x_size_offset = codestream_offset + siz_marker + 6;
    let y_size_offset = codestream_offset + siz_marker + 10;
    oversized_jpeg2000[x_size_offset..x_size_offset + 4].copy_from_slice(&8192u32.to_be_bytes());
    oversized_jpeg2000[y_size_offset..y_size_offset + 4].copy_from_slice(&8192u32.to_be_bytes());
    let oversized_jpeg2000_path = temporary.path().join("oversized-jpeg2000.dcm");
    fs::write(&oversized_jpeg2000_path, oversized_jpeg2000).unwrap();
    assert!(matches!(
        convert_path(
            &oversized_jpeg2000_path,
            temporary.path().join("oversized-jpeg2000-out"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::LimitExceeded(_))
    ));

    let mut unsupported_jpeg2000 = fs::read(&jpeg2000).unwrap();
    let unsupported_uid = b"1.2.840.10008.1.2.4.92";
    let transfer_syntax_offset = unsupported_jpeg2000
        .windows(lossless_uid.len())
        .position(|window| window == lossless_uid)
        .unwrap();
    unsupported_jpeg2000[transfer_syntax_offset..transfer_syntax_offset + lossless_uid.len()]
        .copy_from_slice(unsupported_uid);
    let unsupported_jpeg2000_path = temporary.path().join("part2-jpeg2000.dcm");
    fs::write(&unsupported_jpeg2000_path, unsupported_jpeg2000).unwrap();
    assert!(matches!(
        convert_path(
            &unsupported_jpeg2000_path,
            temporary.path().join("part2-jpeg2000-out"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::Unsupported(message)) if message.contains("Part 2")
    ));

    let mut oversized = fs::read(&multiframe).unwrap();
    for tag in [0x0010u8, 0x0011u8] {
        let pattern = [0x28, 0x00, tag, 0x00, b'U', b'S', 0x02, 0x00, 0x04, 0x00];
        let start = oversized
            .windows(pattern.len())
            .position(|window| window == pattern)
            .unwrap();
        oversized[start + 8..start + 10].copy_from_slice(&u16::MAX.to_le_bytes());
    }
    let oversized_path = temporary.path().join("oversized.dcm");
    fs::write(&oversized_path, oversized).unwrap();
    assert!(matches!(
        convert_path(
            &oversized_path,
            temporary.path().join("oversized-dicom-out"),
            &ConvertOptions::default(),
        ),
        Err(document_svg::Error::LimitExceeded(_))
    ));
}

#[test]
fn converts_dicom_encapsulated_pdf_without_copying_dicom_metadata() {
    let temporary = TempDir::new().unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_encapsulated_pdf.dcm");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::Dicom);
    let output = temporary.path().join("dicom-encapsulated-pdf");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dicom);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"dicom\""));
    assert!(svg.contains("DICOM Encapsulated PDF"));
    assert!(svg.contains("Synthetic radiology report"));
    assert!(!svg.contains("Synthetic^Patient"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("not de-identification"))
    );

    let bytes = fs::read(&input).unwrap();
    let mime = b"application/pdf";
    let mime_offset = bytes
        .windows(mime.len())
        .position(|window| window == mime)
        .unwrap();
    let mut wrong_mime = bytes.clone();
    wrong_mime[mime_offset..mime_offset + mime.len()].copy_from_slice(b"application/xml");
    let wrong_mime_path = temporary.path().join("wrong-mime.dcm");
    fs::write(&wrong_mime_path, wrong_mime).unwrap();
    assert!(matches!(
        convert_path(
            &wrong_mime_path,
            temporary.path().join("wrong-mime-out"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::Unsupported(message)) if message.contains("MIME type")
    ));

    let mut bad_length = bytes;
    let length_tag = [0x42, 0x00, 0x15, 0x00, b'U', b'L', 0x04, 0x00];
    let length_offset = bad_length
        .windows(length_tag.len())
        .position(|window| window == length_tag)
        .unwrap()
        + length_tag.len();
    bad_length[length_offset..length_offset + 4].copy_from_slice(&1u32.to_le_bytes());
    let bad_length_path = temporary.path().join("bad-document-length.dcm");
    fs::write(&bad_length_path, bad_length).unwrap();
    assert!(matches!(
        convert_path(
            &bad_length_path,
            temporary.path().join("bad-document-length-out"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::InvalidInput(message)) if message.contains("Length does not match")
    ));

    let mut oversized_document = fs::read(&input).unwrap();
    let document_tag = [0x42, 0x00, 0x11, 0x00, b'O', b'B', 0x00, 0x00];
    let document_offset = oversized_document
        .windows(document_tag.len())
        .position(|window| window == document_tag)
        .unwrap();
    oversized_document[document_offset + 8..document_offset + 12]
        .copy_from_slice(&(64u32 * 1024 * 1024 + 1).to_le_bytes());
    let oversized_path = temporary.path().join("oversized-embedded-pdf.dcm");
    fs::write(&oversized_path, oversized_document).unwrap();
    assert!(matches!(
        convert_path(
            &oversized_path,
            temporary.path().join("oversized-embedded-pdf-out"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::LimitExceeded(message)) if message.contains("Encapsulated Document exceeds")
    ));

    let mut wrong_vr = fs::read(&input).unwrap();
    let document_vr_offset = wrong_vr
        .windows(document_tag.len())
        .position(|window| window == document_tag)
        .unwrap()
        + 4;
    wrong_vr[document_vr_offset..document_vr_offset + 2].copy_from_slice(b"UN");
    let wrong_vr_path = temporary.path().join("wrong-document-vr.dcm");
    fs::write(&wrong_vr_path, wrong_vr).unwrap();
    assert!(matches!(
        convert_path(
            &wrong_vr_path,
            temporary.path().join("wrong-document-vr-out"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::InvalidInput(message)) if message.contains("must use the OB")
    ));
}

#[test]
fn converts_v2000_v3000_mol_and_multi_record_sdf_structures() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_molecules.sdf");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::Sdf);
    let output = temporary.path().join("sdf-pages");
    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Sdf);
    assert_eq!(report.page_count, 2);
    let first_svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(first_svg.contains("Synthetic Aromatic Ion"));
    assert!(first_svg.contains("chemical:bond"));
    assert!(first_svg.contains("chemical:atom"));
    assert!(first_svg.contains("N+"));
    assert!(!first_svg.contains("SYNTHETIC-001"));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("SDF data fields"))
    );
    let second_svg = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(second_svg.contains("Synthetic Water"));
    assert!(
        report.pages[1]
            .warnings
            .iter()
            .any(|warning| warning.contains("projected onto the XY plane"))
    );

    let bytes = fs::read(&fixture).unwrap();
    let first_record = bytes
        .split(|byte| *byte == b'\n')
        .take_while(|line| *line != b"$$$$")
        .collect::<Vec<_>>()
        .join(&[b'\n'][..]);
    let mol_path = temporary.path().join("aromatic.mol");
    fs::write(&mol_path, first_record).unwrap();
    assert_eq!(SourceFormat::detect(&mol_path).unwrap(), SourceFormat::Mol);
    let mol_report = convert_path(
        &mol_path,
        temporary.path().join("mol-page"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(mol_report.source_format, SourceFormat::Mol);
    assert_eq!(mol_report.page_count, 1);

    let extensionless = temporary.path().join("chemical-records");
    fs::copy(&fixture, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Sdf
    );
    assert_eq!(
        convert_path(
            &extensionless,
            temporary.path().join("extensionless-sdf"),
            &ConvertOptions::default()
        )
        .unwrap()
        .page_count,
        2
    );

    let v3000_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_molecule_v3000.mol");
    assert_eq!(
        SourceFormat::detect(&v3000_path).unwrap(),
        SourceFormat::Mol
    );
    let v3000_report = convert_path(
        &v3000_path,
        temporary.path().join("v3000-mol"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(v3000_report.page_count, 1);
    let v3000_svg = fs::read_to_string(temporary.path().join("v3000-mol/page-0001.svg")).unwrap();
    assert!(v3000_svg.contains("Synthetic V3000 Isotope"));
    assert!(v3000_svg.contains("18O-"));
    assert!(v3000_svg.contains("chemical:stereo-bond"));
    assert!(
        v3000_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("Sgroups"))
    );
    let v3000_extensionless = temporary.path().join("advanced-structure");
    fs::copy(&v3000_path, &v3000_extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&v3000_extensionless).unwrap(),
        SourceFormat::Mol
    );

    let mut unsupported = fs::read(&mol_path).unwrap();
    let version = b"V2000";
    let version_offset = unsupported
        .windows(version.len())
        .position(|window| window == version)
        .unwrap();
    unsupported[version_offset..version_offset + version.len()].copy_from_slice(b"V4000");
    let unsupported_path = temporary.path().join("unsupported.mol");
    fs::write(&unsupported_path, unsupported).unwrap();
    assert!(matches!(
        convert_path(
            &unsupported_path,
            temporary.path().join("unsupported-mol"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::Unsupported(message)) if message.contains("version marker")
    ));
}

#[test]
fn converts_v2000_rxn_into_a_bounded_reaction_diagram() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_reaction.rxn");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::Rxn);
    let output = temporary.path().join("reaction");
    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Rxn);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Synthetic two-reactant reaction"));
    assert!(svg.contains("Reactants"));
    assert!(svg.contains("Products"));
    assert!(svg.contains("chemical:reaction-arrow"));
    assert!(svg.contains("chemical:reactant"));
    assert!(svg.contains("chemical:product"));
    assert!(svg.contains("Reactant carbonyl"));
    assert!(svg.contains("Reactant hydrogen chloride"));
    assert!(svg.contains("Product chloromethanol"));

    let extensionless = temporary.path().join("reaction-record");
    fs::copy(&fixture, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Rxn
    );
    assert_eq!(
        convert_path(
            &extensionless,
            temporary.path().join("extensionless-reaction"),
            &ConvertOptions::default()
        )
        .unwrap()
        .page_count,
        1
    );

    let mut v3000 = fs::read_to_string(&fixture).unwrap();
    v3000.replace_range(0..4, "$RXN V3000");
    let v3000_path = temporary.path().join("unsupported.rxn");
    fs::write(&v3000_path, v3000.as_bytes()).unwrap();
    assert!(matches!(
        convert_path(
            &v3000_path,
            temporary.path().join("unsupported-reaction"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::Unsupported(message)) if message.contains("V3000")
    ));
}

#[test]
fn converts_dicomdir_file_set_in_hierarchy_order_with_confined_file_ids() {
    let temporary = TempDir::new().unwrap();
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dicom_set");
    let dicomdir = fixture_root.join("DICOMDIR");
    assert_eq!(
        SourceFormat::detect(&dicomdir).unwrap(),
        SourceFormat::DicomDir
    );
    let output = temporary.path().join("dicomdir-out");
    let report = convert_path(&dicomdir, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::DicomDir);
    assert_eq!(report.page_count, 3);
    for page_number in 1..=3 {
        let svg = fs::read_to_string(output.join(format!("page-{page_number:04}.svg"))).unwrap();
        assert!(svg.contains("data-source-format=\"dicomdir\""));
        assert!(svg.contains(&format!("DICOMDIR Image {page_number}")));
        assert!(!svg.contains("Private^DirectoryPatient"));
    }
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("hierarchy is followed"))
    );
    assert!(
        report.pages[1]
            .warnings
            .iter()
            .any(|warning| warning.contains("2 sequential SVG pages"))
    );
    assert!(
        !report.pages[2]
            .warnings
            .iter()
            .any(|warning| warning.contains("2 sequential SVG pages"))
    );

    let copied_root = temporary.path().join("copied-file-set");
    let image_dir = copied_root.join("DICOM");
    fs::create_dir_all(&image_dir).unwrap();
    fs::copy(&dicomdir, copied_root.join("medical-index")).unwrap();
    fs::copy(
        fixture_root.join("DICOM/IMG0001"),
        image_dir.join("IMG0001"),
    )
    .unwrap();
    fs::copy(
        fixture_root.join("DICOM/IMG0002"),
        image_dir.join("IMG0002"),
    )
    .unwrap();
    let extensionless = copied_root.join("medical-index");
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::DicomDir
    );
    assert_eq!(
        convert_path(
            &extensionless,
            temporary.path().join("dicomdir-sniffed-out"),
            &ConvertOptions::default(),
        )
        .unwrap()
        .page_count,
        3
    );

    let mut traversal = fs::read(&dicomdir).unwrap();
    let source_id = b"DICOM\\IMG0001";
    let invalid_id = b"DICOM\\../0001";
    assert_eq!(source_id.len(), invalid_id.len());
    let start = traversal
        .windows(source_id.len())
        .position(|window| window == source_id)
        .unwrap();
    traversal[start..start + source_id.len()].copy_from_slice(invalid_id);
    let traversal_root = temporary.path().join("traversal-file-set");
    fs::create_dir_all(&traversal_root).unwrap();
    let traversal_path = traversal_root.join("DICOMDIR");
    fs::write(&traversal_path, traversal).unwrap();
    assert!(
        convert_path(
            &traversal_path,
            temporary.path().join("traversal-out"),
            &ConvertOptions::default()
        )
        .unwrap_err()
        .to_string()
        .contains("File ID component violates")
    );
}

#[test]
fn rejects_palette_tiff_and_converts_white_is_zero_low_bit_grayscale() {
    let temporary = TempDir::new().unwrap();
    let palette = temporary.path().join("palette.tif");
    fs::write(&palette, common::tiff_palette_1bit()).unwrap();

    let palette_error = convert_path(
        &palette,
        temporary.path().join("palette-out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(palette_error, document_svg::Error::Unsupported(_)));

    let decode_png = |svg: &str| {
        let encoded = svg
            .split_once("href=\"data:image/png;base64,")
            .unwrap()
            .1
            .split_once('\"')
            .unwrap()
            .0;
        let png_bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        pixels.truncate(info.buffer_size());
        (info.color_type, pixels)
    };

    for bits in [1, 2, 4] {
        let grayscale = temporary.path().join(format!("white-is-zero-{bits}.tif"));
        fs::write(&grayscale, common::tiff_white_is_zero_gray(bits)).unwrap();
        let grayscale_output = temporary.path().join(format!("gray-{bits}-out"));
        let grayscale_report =
            convert_path(&grayscale, &grayscale_output, &ConvertOptions::default()).unwrap();
        assert_eq!(grayscale_report.page_count, 1);
        let grayscale_svg = fs::read_to_string(grayscale_output.join("page-0001.svg")).unwrap();
        let (grayscale_color, grayscale_pixels) = decode_png(&grayscale_svg);
        assert_eq!(grayscale_color, png::ColorType::Grayscale);
        assert_eq!(
            grayscale_pixels[0], 255,
            "WhiteIsZero {bits}-bit sample 0 must be white"
        );
        assert_eq!(
            grayscale_pixels[10], 0,
            "WhiteIsZero {bits}-bit sample max must be black"
        );
    }
}

#[test]
fn converts_svgz_and_caps_decompressed_svg_size() {
    let temporary = TempDir::new().unwrap();
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80"><rect x="10" y="10" width="90" height="50" fill="#00aa55"/></svg>"##;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(svg.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();

    let input = temporary.path().join("shape.svgz");
    let output = temporary.path().join("svgz_out");
    fs::write(&input, &compressed).unwrap();
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Svg);
    assert_eq!(report.page_count, 1);
    let converted = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(converted.contains("#00aa55"));

    let extensionless = temporary.path().join("compressed-vector");
    fs::write(&extensionless, &compressed).unwrap();
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("sniffed_svgz_out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Svg);

    let expanded_svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">{}</svg>",
        "<rect x=\"0\" y=\"0\" width=\"1\" height=\"1\"/>".repeat(300)
    );
    let mut bomb_encoder =
        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    bomb_encoder.write_all(expanded_svg.as_bytes()).unwrap();
    let compressed_expansion = bomb_encoder.finish().unwrap();
    assert!(compressed_expansion.len() < 1024);
    let expansion_path = temporary.path().join("expanded.svgz");
    fs::write(&expansion_path, compressed_expansion).unwrap();
    let limited = ConvertOptions {
        max_input_bytes: 1024,
        ..ConvertOptions::default()
    };
    assert!(matches!(
        convert_path(
            &expansion_path,
            temporary.path().join("too_large_svgz_out"),
            &limited
        ),
        Err(document_svg::Error::LimitExceeded(_))
    ));
}

#[test]
fn converts_step_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let step_input = temporary.path().join("model.step");
    let out_dir = temporary.path().join("step_out");

    let step_data = r#"
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('STEP test'),'2;1');
FILE_NAME('test.step','2026-09-10',('Engineer'),('Testing'),'','','');
FILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));
ENDSEC;
DATA;
#1 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));
#2 = CARTESIAN_POINT('', (100.0, 0.0, 0.0));
#3 = CARTESIAN_POINT('', (50.0, 100.0, 50.0));
#10 = VERTEX_POINT('', #1);
#11 = VERTEX_POINT('', #2);
#12 = VERTEX_POINT('', #3);
#20 = EDGE_CURVE('', #10, #11, #0, .T.);
#21 = EDGE_CURVE('', #11, #12, #0, .T.);
#22 = EDGE_CURVE('', #12, #10, #0, .T.);
ENDSEC;
END-ISO-10303-21;
"#;
    fs::write(&step_input, step_data).unwrap();

    let report = convert_path(&step_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Step);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"step:wireframe\""));
    assert!(svg.contains("STEP ISO 10303-21"));
}

#[test]
fn converts_obj_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let obj_input = temporary.path().join("model.obj");
    let out_dir = temporary.path().join("obj_out");

    let obj_data = r#"
# Cube OBJ
v 0 0 0
v 10 0 0
v 10 10 0
v 0 10 0
v 0 0 10
v 10 0 10
v 10 10 10
v 0 10 10

f 1 2 3 4
f 5 6 7 8
f 1 2 6 5
f 2 3 7 6
f 3 4 8 7
f 4 1 5 8
"#;
    fs::write(&obj_input, obj_data).unwrap();

    let report = convert_path(&obj_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Obj);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"obj:mesh\""));
    assert!(svg.contains("data-semantic-role=\"obj:background\""));
}

#[test]
fn converts_ply_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let ply_input = temporary.path().join("cube.ply");
    let out_dir = temporary.path().join("ply_out");

    let ply_data = r#"ply
format ascii 1.0
element vertex 8
property float x
property float y
property float z
element face 6
property list uchar int vertex_indices
end_header
0 0 0
10 0 0
10 10 0
0 10 0
0 0 10
10 0 10
10 10 10
0 10 10
4 0 1 2 3
4 7 6 5 4
4 0 4 5 1
4 1 5 6 2
4 2 6 7 3
4 3 7 4 0
"#;
    fs::write(&ply_input, ply_data).unwrap();

    let report = convert_path(&ply_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ply);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<path id=\"face_"));
}

#[test]
fn converts_ascii_and_binary_ply_point_clouds_without_faces() {
    let temporary = TempDir::new().unwrap();
    let ascii = temporary.path().join("scan.ply");
    let ascii_output = temporary.path().join("ply-points-out");
    let point_cloud = concat!(
        "ply\nformat ascii 1.0\n",
        "element vertex 4\n",
        "property float x\nproperty float y\nproperty float z\n",
        "property uchar red\nproperty uchar green\nproperty uchar blue\n",
        "end_header\n",
        "-10 0 0 255 0 0\n10 0 0 0 255 0\n0 10 0 0 0 255\n0 0 10 255 255 255\n",
    );
    fs::write(&ascii, point_cloud).unwrap();

    let report = convert_path(&ascii, &ascii_output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ply);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(ascii_output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("PLY Point Cloud"));
    assert!(svg.contains("data-semantic-role=\"ply:point-cloud\""));
    assert!(svg.contains("a2.5 2.5"));
    assert!(svg.contains("fill=\"#FF0000\""));
    assert!(svg.contains("fill=\"#00FF00\""));

    let extensionless = temporary.path().join("scan-content-sniffed");
    fs::copy(&ascii, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ply
    );

    let binary = temporary.path().join("scan-binary.ply");
    let mut bytes = b"ply\nformat binary_little_endian 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n".to_vec();
    for (point, color) in [
        ([0.0f32, 0.0, 0.0], [255u8, 0, 0]),
        ([10.0, 0.0, 0.0], [0, 255, 0]),
        ([0.0, 10.0, 4.0], [0, 0, 255]),
    ] {
        for coordinate in point {
            bytes.extend_from_slice(&coordinate.to_le_bytes());
        }
        bytes.extend_from_slice(&color);
    }
    fs::write(&binary, bytes).unwrap();
    let binary_output = temporary.path().join("ply-binary-points-out");
    let binary_report = convert_path(&binary, &binary_output, &ConvertOptions::default()).unwrap();
    assert_eq!(binary_report.source_format, SourceFormat::Ply);
    assert_eq!(binary_report.page_count, 1);
    assert!(
        fs::read_to_string(binary_output.join("page-0001.svg"))
            .unwrap()
            .contains("ply:point-cloud")
    );
    let binary_svg = fs::read_to_string(binary_output.join("page-0001.svg")).unwrap();
    assert!(binary_svg.contains("fill=\"#00FF00\""));
}

#[test]
fn bounds_ply_point_cloud_color_groups_with_quantization() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("many-colors.ply");
    let output = temporary.path().join("bounded-color-points-out");
    let mut ply = String::from(
        "ply\nformat ascii 1.0\nelement vertex 513\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n",
    );
    for index in 0u16..513 {
        let red = (index % 256) as u8;
        let green = (index / 256) as u8;
        ply.push_str(&format!(
            "{index} {} {} {red} {green} 0\n",
            index % 13,
            index % 7
        ));
    }
    fs::write(&input, ply).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ply);
    assert_eq!(report.page_count, 1);
    assert!(report.pages[0].node_count <= 512);
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("quantized"))
    );
}

#[test]
fn converts_pcd_ascii_binary_and_lzf_compressed_point_clouds() {
    let temporary = TempDir::new().unwrap();
    let ascii = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/point_cloud_ascii.pcd");
    let ascii_output = temporary.path().join("pcd-ascii-out");
    let ascii_report = convert_path(&ascii, &ascii_output, &ConvertOptions::default()).unwrap();
    assert_eq!(ascii_report.source_format, SourceFormat::Pcd);
    assert_eq!(ascii_report.page_count, 1);
    assert!(
        ascii_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("non-coordinate fields"))
    );
    assert!(
        ascii_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("non-finite"))
    );
    let ascii_svg = fs::read_to_string(ascii_output.join("page-0001.svg")).unwrap();
    assert!(ascii_svg.contains("PCD Point Cloud"));
    assert!(ascii_svg.contains("data-semantic-role=\"pcd:point-cloud\""));
    assert!(ascii_svg.contains("fill=\"#FF0000\""));
    assert!(ascii_svg.contains("fill=\"#00FF00\""));
    assert!(ascii_svg.contains("fill=\"#FF0000\""));
    assert!(ascii_svg.contains("fill=\"#00FF00\""));

    let extensionless = temporary.path().join("pcd-header-sniff");
    fs::copy(&ascii, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Pcd
    );

    let header = concat!(
        "# .PCD v0.7 - Point Cloud Data file format\n",
        "VERSION .7\nFIELDS x y z\nSIZE 4 4 4\nTYPE F F F\nCOUNT 1 1 1\n",
        "WIDTH 3\nHEIGHT 1\nVIEWPOINT 0 0 0 1 0 0 0\nPOINTS 3\nDATA ",
    );
    let points = [[0.0f32, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 4.0]];
    let mut raw = Vec::new();
    for point in points {
        for value in point {
            raw.extend_from_slice(&value.to_le_bytes());
        }
    }
    let binary = temporary.path().join("binary.pcd");
    let mut binary_bytes = format!("{header}binary\n").into_bytes();
    binary_bytes.extend_from_slice(&raw);
    fs::write(&binary, binary_bytes).unwrap();
    let binary_output = temporary.path().join("pcd-binary-out");
    let binary_report = convert_path(&binary, &binary_output, &ConvertOptions::default()).unwrap();
    assert_eq!(binary_report.source_format, SourceFormat::Pcd);
    assert!(
        fs::read_to_string(binary_output.join("page-0001.svg"))
            .unwrap()
            .contains("pcd:point-cloud")
    );

    let mut soa = Vec::new();
    for axis in 0..3 {
        for point in points {
            soa.extend_from_slice(&point[axis].to_le_bytes());
        }
    }
    let mut compressed = Vec::new();
    for chunk in soa.chunks(32) {
        compressed.push(u8::try_from(chunk.len() - 1).unwrap());
        compressed.extend_from_slice(chunk);
    }
    let mut compressed_bytes = format!("{header}binary_compressed\n").into_bytes();
    compressed_bytes.extend_from_slice(&u32::try_from(compressed.len()).unwrap().to_le_bytes());
    compressed_bytes.extend_from_slice(&u32::try_from(soa.len()).unwrap().to_le_bytes());
    compressed_bytes.extend_from_slice(&compressed);
    let compressed_path = temporary.path().join("compressed.pcd");
    fs::write(&compressed_path, compressed_bytes).unwrap();
    let compressed_output = temporary.path().join("pcd-compressed-out");
    let compressed_report = convert_path(
        &compressed_path,
        &compressed_output,
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(compressed_report.source_format, SourceFormat::Pcd);
    assert!(
        fs::read_to_string(compressed_output.join("page-0001.svg"))
            .unwrap()
            .contains("pcd:point-cloud")
    );

    let separate_channels = temporary.path().join("separate-channels.pcd");
    fs::write(
        &separate_channels,
        concat!(
            "VERSION .7\nFIELDS x y z red green blue\n",
            "SIZE 4 4 4 2 2 2\nTYPE F F F U U U\nCOUNT 1 1 1 1 1 1\n",
            "WIDTH 1\nHEIGHT 1\nPOINTS 1\nDATA ascii\n",
            "0 0 0 65535 32768 0\n"
        ),
    )
    .unwrap();
    let separate_output = temporary.path().join("pcd-separate-colors-out");
    let separate_report = convert_path(
        &separate_channels,
        &separate_output,
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(separate_report.source_format, SourceFormat::Pcd);
    let separate_svg = fs::read_to_string(separate_output.join("page-0001.svg")).unwrap();
    assert!(separate_svg.contains("fill=\"#FF8000\""));

    let mut invalid_compressed = format!("{header}binary_compressed\n").into_bytes();
    invalid_compressed.extend_from_slice(&1u32.to_le_bytes());
    invalid_compressed.extend_from_slice(&36u32.to_le_bytes());
    invalid_compressed.push(31);
    let invalid_path = temporary.path().join("invalid-compressed.pcd");
    fs::write(&invalid_path, invalid_compressed).unwrap();
    assert!(
        convert_path(
            &invalid_path,
            temporary.path().join("pcd-invalid-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );
}

#[test]
fn converts_multiscan_ptx_with_registration_transforms_colors_and_intensity() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ptx");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::Ptx);
    let output = temporary.path().join("ptx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ptx);
    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("PTX scan 1"));
    assert!(first.contains("data-semantic-role=\"ptx:point-cloud\""));
    assert!(first.contains("fill=\"#FF009B\""));
    assert!(first.contains("fill=\"#00FF9B\""));
    assert!(first.contains("fill=\"#2563EB\""));
    assert!(report.pages[0].node_count > 5);
    assert!(second.contains("PTX scan 2"));
    assert!(second.contains("data-semantic-role=\"ptx:point-cloud\""));
    assert!(second.contains("fill=\"#808080\""));
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("no-return"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("mark no color"))
    );
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("mark no color"))
    );
    assert!(
        report.pages[1]
            .warnings
            .iter()
            .any(|warning| warning.contains("scan 2"))
    );
    assert!(
        report.pages[1]
            .warnings
            .iter()
            .all(|warning| !warning.contains("scan 1"))
    );

    let extensionless = temporary.path().join("ptx-header-sniff");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ptx
    );
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("ptx-sniffed-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Ptx);
}

#[test]
fn converts_leica_pts_rgb_and_legacy_intensity_point_clouds() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pts");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::Pts);
    let output = temporary.path().join("pts-rgb-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Pts);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"pts:point-cloud\""));
    assert!(svg.contains("fill=\"#FF0000\""));
    assert!(svg.contains("fill=\"#00FF00\""));
    assert!(svg.contains("fill=\"#0000FF\""));
    assert!(svg.contains("fill=\"#2563EB\""));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("mark no color"))
    );

    let intensity_input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_intensity.pts");
    let intensity_output = temporary.path().join("pts-intensity-out");
    let intensity_report = convert_path(
        &intensity_input,
        &intensity_output,
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(intensity_report.source_format, SourceFormat::Pts);
    let intensity_svg = fs::read_to_string(intensity_output.join("page-0001.svg")).unwrap();
    assert!(intensity_svg.contains("fill=\"#000000\""));
    assert!(intensity_svg.contains("fill=\"#808080\""));
    assert!(intensity_svg.contains("fill=\"#FFFFFF\""));

    let extensionless = temporary.path().join("pts-header-sniff");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Pts
    );
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("pts-sniffed-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Pts);
}

#[test]
fn rejects_pts_count_mismatch_and_inconsistent_records() {
    let temporary = TempDir::new().unwrap();
    let count_mismatch = temporary.path().join("count-mismatch.pts");
    fs::write(&count_mismatch, "2\n0 0 0 0.5\n").unwrap();
    assert!(
        convert_path(
            &count_mismatch,
            temporary.path().join("count-mismatch-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );

    let inconsistent = temporary.path().join("inconsistent.pts");
    fs::write(&inconsistent, "2\n0 0 0 0.5\n1 1 1 0.5 0 255 0\n").unwrap();
    assert!(
        convert_path(
            &inconsistent,
            temporary.path().join("inconsistent-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );
}

#[test]
fn converts_real_and_multiscan_e57_point_clouds_and_checks_page_crc() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_e57_bunny.e57");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::E57);
    let output = temporary.path().join("e57-bunny-out");
    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::E57);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"e57:point-cloud\""));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .all(|warning| !warning.contains("invalid E57 colors"))
    );

    let extensionless = temporary.path().join("e57-signature-sniff");
    fs::copy(&fixture, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::E57
    );
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("e57-bunny-sniffed-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::E57);
    assert_eq!(sniffed.page_count, 1);

    let generated = temporary.path().join("multiscan.e57");
    common::write_e57_rgb_and_intensity_fixture(&generated);
    let generated_output = temporary.path().join("e57-multiscan-out");
    let generated_report =
        convert_path(&generated, &generated_output, &ConvertOptions::default()).unwrap();
    assert_eq!(generated_report.source_format, SourceFormat::E57);
    assert_eq!(generated_report.page_count, 2);
    let colored = fs::read_to_string(generated_output.join("page-0001.svg")).unwrap();
    let intensity = fs::read_to_string(generated_output.join("page-0002.svg")).unwrap();
    assert!(colored.contains("data-semantic-role=\"e57:point-cloud\""));
    assert!(colored.contains("fill=\"#FF0000\""));
    assert!(colored.contains("fill=\"#00FF00\""));
    assert!(colored.contains("fill=\"#0000FF\""));
    assert!(
        generated_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("invalid or out-of-range"))
    );
    assert!(intensity.contains("fill=\"#000000\""));
    assert!(intensity.contains("fill=\"#808080\""));
    assert!(intensity.contains("fill=\"#FFFFFF\""));

    let corrupted = temporary.path().join("bad-crc.e57");
    let mut corrupted_bytes = fs::read(&fixture).unwrap();
    corrupted_bytes[1020] ^= 0xFF;
    fs::write(&corrupted, corrupted_bytes).unwrap();
    assert!(
        convert_path(
            &corrupted,
            temporary.path().join("bad-crc-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );
}

#[test]
fn converts_common_xyz_ascii_point_layouts_and_keeps_black_rgb() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let rgb_input = fixture_dir.join("sample.xyz");
    assert_eq!(SourceFormat::detect(&rgb_input).unwrap(), SourceFormat::Xyz);
    let rgb_output = temporary.path().join("xyz-rgb-out");
    let report = convert_path(&rgb_input, &rgb_output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xyz);
    assert_eq!(report.page_count, 1);
    let rgb_svg = fs::read_to_string(rgb_output.join("page-0001.svg")).unwrap();
    assert!(rgb_svg.contains("data-semantic-role=\"xyz:point-cloud\""));
    assert!(rgb_svg.contains("fill=\"#FF0000\""));
    assert!(rgb_svg.contains("fill=\"#00FF00\""));
    assert!(rgb_svg.contains("fill=\"#0000FF\""));
    assert!(rgb_svg.contains("fill=\"#000000\""));

    let intensity_input = fixture_dir.join("sample_xyz_intensity.xyz");
    let intensity_output = temporary.path().join("xyz-intensity-out");
    let intensity_report = convert_path(
        &intensity_input,
        &intensity_output,
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(intensity_report.source_format, SourceFormat::Xyz);
    let intensity_svg = fs::read_to_string(intensity_output.join("page-0001.svg")).unwrap();
    assert!(intensity_svg.contains("fill=\"#000000\""));
    assert!(intensity_svg.contains("fill=\"#808080\""));
    assert!(intensity_svg.contains("fill=\"#FFFFFF\""));

    let rgb_only_input = fixture_dir.join("sample_xyz_rgb.xyz");
    let rgb_only_output = temporary.path().join("xyz-rgb-only-out");
    convert_path(
        &rgb_only_input,
        &rgb_only_output,
        &ConvertOptions::default(),
    )
    .unwrap();
    let rgb_only_svg = fs::read_to_string(rgb_only_output.join("page-0001.svg")).unwrap();
    assert!(rgb_only_svg.contains("fill=\"#000000\""));
    assert!(rgb_only_svg.contains("fill=\"#FF4000\""));

    let normals_input = fixture_dir.join("sample_xyz_normals.xyz");
    let normals_report = convert_path(
        &normals_input,
        temporary.path().join("xyz-normals-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        normals_report
            .warnings
            .iter()
            .any(|warning| warning.contains("normal vectors"))
    );
    let normals9_input = fixture_dir.join("sample_xyz_normals9.xyz");
    let normals9_report = convert_path(
        &normals9_input,
        temporary.path().join("xyz-normals9-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert!(
        normals9_report
            .warnings
            .iter()
            .any(|warning| warning.contains("normal vectors"))
    );

    let header_input = fixture_dir.join("sample_xyz_header.xyz");
    let header_output = temporary.path().join("xyz-header-out");
    let header_report =
        convert_path(&header_input, &header_output, &ConvertOptions::default()).unwrap();
    assert_eq!(header_report.source_format, SourceFormat::Xyz);
    let header_svg = fs::read_to_string(header_output.join("page-0001.svg")).unwrap();
    assert!(header_svg.contains("fill=\"#FF0000\""));
    assert!(header_svg.contains("fill=\"#00FF00\""));
    assert!(
        header_report
            .warnings
            .iter()
            .any(|warning| warning.contains("1 unrecognized XYZ header column"))
    );

    let headerless_header = temporary.path().join("xyz-header-sniff");
    fs::copy(&header_input, &headerless_header).unwrap();
    assert_eq!(
        SourceFormat::detect(&headerless_header).unwrap(),
        SourceFormat::Xyz
    );

    let extensionless = temporary.path().join("xyz-content-sniff");
    fs::write(&extensionless, "0 0 0\n1 0 0\n").unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xyz
    );
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("xyz-sniffed-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Xyz);
}

#[test]
fn rejects_xyz_rows_with_inconsistent_field_widths() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mixed-width.xyz");
    fs::write(&input, "0 0 0\n1 1 1 0.5\n").unwrap();
    assert!(
        convert_path(
            &input,
            temporary.path().join("mixed-width-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );

    let incomplete_header = temporary.path().join("incomplete-color-header.xyz");
    fs::write(
        &incomplete_header,
        "x y z red green\n0 0 0 255 0\n1 0 0 0 255\n",
    )
    .unwrap();
    assert!(
        convert_path(
            &incomplete_header,
            temporary.path().join("incomplete-header-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );
}

#[test]
fn converts_esri_ascii_grid_and_keeps_nodata_transparent() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_elevation.asc");
    assert_eq!(
        SourceFormat::detect(&input).unwrap(),
        SourceFormat::EsriAsciiGrid
    );
    let output = temporary.path().join("ascii-grid-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::EsriAsciiGrid);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("NODATA_VALUE"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"esri:ascii-grid-raster\""));
    let data_uri = svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split('"')
        .next()
        .unwrap();
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(data_uri)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!((info.width, info.height), (4, 3));
    assert_eq!(&pixels[0..4], &[68, 1, 84, 255]);
    assert_eq!(&pixels[20..24], &[0, 0, 0, 0]);
    assert_eq!(&pixels[11 * 4..11 * 4 + 4], &[253, 231, 37, 255]);

    let extensionless = temporary.path().join("grid-signature-sniff");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::EsriAsciiGrid
    );
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("grid-sniffed-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::EsriAsciiGrid);
}

#[test]
fn converts_an_all_nodata_esri_grid_to_a_transparent_raster() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("all-nodata.asc");
    fs::write(
        &input,
        "ncols 2\nnrows 1\nxllcenter 1\nyllcenter 2\ncellsize 3\nnodata_value -9999\n-9999 -9999\n",
    )
    .unwrap();
    let output = temporary.path().join("all-nodata-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::EsriAsciiGrid);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("no valid data cells"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("2 NODATA_VALUE cells"))
    );

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("No valid data cells"));
    assert!(!svg.contains("grid-legend-band-"));
    let data_uri = svg
        .split_once("href=\"data:image/png;base64,")
        .unwrap()
        .1
        .split('\"')
        .next()
        .unwrap();
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(data_uri)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!((info.width, info.height), (2, 1));
    assert_eq!(&pixels[..8], &[0, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn rejects_esri_ascii_grid_mismatched_origin_and_cell_counts() {
    let temporary = TempDir::new().unwrap();
    let mismatched = temporary.path().join("mismatched.asc");
    fs::write(
        &mismatched,
        "ncols 2\nnrows 2\nxllcorner 0\nyllcorner 0\ncellsize 1\n1 2 3\n",
    )
    .unwrap();
    assert!(
        convert_path(
            &mismatched,
            temporary.path().join("mismatched-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );

    let mismatched_origin = temporary.path().join("mismatched-origin.asc");
    fs::write(
        &mismatched_origin,
        "ncols 1\nnrows 1\nxllcenter 0.5\nyllcorner 0\ncellsize 1\n1\n",
    )
    .unwrap();
    assert!(
        convert_path(
            &mismatched_origin,
            temporary.path().join("mismatched-origin-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );
}

#[test]
fn converts_dbase_attribute_tables_and_skips_deleted_records() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_attributes.dbf");
    assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::Dbf);
    let output = temporary.path().join("dbase-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dbf);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("deleted dBASE record"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("PARCEL_ID"));
    assert!(svg.contains("AREA_HA"));
    assert!(svg.contains("Café Moreno"));
    assert!(svg.contains("2008-09-22"));
    assert!(!svg.contains("Willow Farm"));

    let extensionless = temporary.path().join("dbase-signature-sniff");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Dbf
    );
    let sniffed = convert_path(
        &extensionless,
        temporary.path().join("dbase-sniffed-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Dbf);
}

#[test]
fn converts_toml_configuration_as_inert_nested_data() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_config.toml");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::Toml);
    let input = temporary.path().join("inert-strings.toml");
    let mut source = fs::read_to_string(fixture).unwrap();
    source.push_str("\nliteral_example = \"<script>this is text</script>\"\n");
    fs::write(&input, source).unwrap();
    let output = temporary.path().join("toml-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Toml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"toml\""));
    assert!(svg.contains("$.service.limits.requests_per_minute (integer): 1200"));
    assert!(svg.contains("$.service.limits.error_rate (float): 0.25"));
    assert!(svg.contains("$.targets[1].regions[1] (string): \"ap-northeast-1\""));
    assert!(svg.contains("(datetime): 2026-08-03T17:20:00Z"));
    assert!(svg.contains("&lt;script&gt;this is text&lt;/script&gt;"));
    assert!(!svg.contains("<script>this is text</script>"));
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("float value(s) were normalized"))
    );
}

#[test]
fn converts_yaml_documents_without_expanding_aliases_or_tags() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_config.yaml");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::Yaml);
    let yml_alias = temporary.path().join("alias.yml");
    fs::copy(&fixture, &yml_alias).unwrap();
    assert_eq!(
        SourceFormat::detect(&yml_alias).unwrap(),
        SourceFormat::Yaml
    );
    let output = temporary.path().join("yaml-out");

    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Yaml);
    assert_eq!(report.page_count, 2);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let svg_second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"yaml\""));
    assert!(svg.contains("$.service.limits.requests_per_minute (scalar): \"1200\""));
    assert!(svg.contains("$.service.limits.error_rate (scalar): \"0.25\""));
    assert!(svg.contains("$.targets[1].regions[1] (scalar): \"ap-northeast-1\""));
    assert!(svg.contains("not expanded"));
    assert!(svg.contains("./secrets.yml"));
    assert!(svg_second.contains("Document 2"));
    assert!(!svg.contains("secrets.yml contents") && !svg_second.contains("secrets.yml contents"));
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("alias reference"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("custom-tagged"))
    );
}

#[test]
fn previews_generic_xml_and_keeps_specialized_xml_routes() {
    let temporary = TempDir::new().unwrap();
    let generic = temporary.path().join("application.xml");
    fs::write(
        &generic,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<configuration xmlns="urn:example:config" version="1">
  <service id="catalog">A&amp;B</service>
  <enabled>true</enabled>
</configuration>"#,
    )
    .unwrap();
    assert_eq!(SourceFormat::detect(&generic).unwrap(), SourceFormat::Xml);
    let extensionless = temporary.path().join("xml-content-sniff");
    fs::copy(&generic, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xml
    );
    let plain_xml = temporary.path().join("not-xml.xml");
    fs::write(&plain_xml, "plain text with no markup").unwrap();
    assert!(matches!(
        SourceFormat::detect(&plain_xml),
        Err(document_svg::Error::Unsupported(_))
    ));
    let output = temporary.path().join("xml-out");
    let report = convert_path(&generic, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"xml\""));
    assert!(svg.contains("namespace ns1 = \"urn:example:config\""));
    assert!(svg.contains("/@version = \"1\""));
    assert!(svg.contains("A&amp;B"));

    let drawio = temporary.path().join("diagram.xml");
    fs::write(
        &drawio,
        "<mxfile><diagram name=\"Empty\"><mxGraphModel><root><mxCell id=\"0\"/><mxCell id=\"1\" parent=\"0\"/></root></mxGraphModel></diagram></mxfile>",
    )
    .unwrap();
    assert_eq!(SourceFormat::detect(&drawio).unwrap(), SourceFormat::Drawio);

    let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/source/sample.project.xml");
    assert_eq!(
        SourceFormat::detect(&project).unwrap(),
        SourceFormat::ProjectXml
    );
}

#[test]
fn converts_java_properties_with_last_value_override_and_inert_placeholders() {
    let temporary = TempDir::new().unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_properties.properties");
    assert_eq!(
        SourceFormat::detect(&fixture).unwrap(),
        SourceFormat::Properties
    );
    let output = temporary.path().join("properties-out");

    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Properties);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("duplicate"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"properties\""));
    assert!(svg.contains("$[\"app.name\"] = \"Catalog API\""));
    assert!(svg.contains("$[\"welcome\"] = \"Hello, Café!\""));
    assert!(svg.contains("$[\"workflow.steps\"] = \"compiletestpackage\""));
    assert!(svg.contains("$[\"release.channel\"] = \"stable\""));
    assert!(!svg.contains("$[\"release.channel\"] = \"old\""));
    assert!(svg.contains("${HOME} is displayed as text"));
    assert!(svg.contains("🚀"));
}

#[test]
fn converts_bpmn_20_diagrams_and_sniffs_the_bpmn_namespace() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_bpmn.bpmn");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::Bpmn);
    let output = temporary.path().join("bpmn-out");

    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Bpmn);
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"bpmn\""));
    assert!(svg.contains("Order fulfillment"));
    assert!(svg.contains("Validate order"));
    assert!(svg.contains("In stock?"));
    assert!(svg.contains("bpmn-flow-arrow-Flow_In_Stock"));

    let extensionless = temporary.path().join("bpmn-content-sniff");
    fs::copy(&fixture, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Bpmn
    );
    let bpmn2 = temporary.path().join("workflow.bpmn2");
    fs::copy(&fixture, &bpmn2).unwrap();
    assert_eq!(SourceFormat::detect(&bpmn2).unwrap(), SourceFormat::Bpmn);

    let without_di = temporary.path().join("model-only.bpmn");
    fs::write(
        &without_di,
        format!(
            "<definitions xmlns=\"{}\"><process id=\"p\"/></definitions>",
            "http://www.omg.org/spec/BPMN/20100524/MODEL"
        ),
    )
    .unwrap();
    let error = convert_path(
        &without_di,
        temporary.path().join("model-only-out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("no BPMN Diagram Interchange"));

    let dtd = temporary.path().join("external.bpmn");
    fs::write(
        &dtd,
        format!(
            "<!DOCTYPE definitions SYSTEM \"https://example.invalid/bpmn.dtd\"><definitions xmlns=\"{}\"/>",
            "http://www.omg.org/spec/BPMN/20100524/MODEL"
        ),
    )
    .unwrap();
    assert!(
        convert_path(
            &dtd,
            temporary.path().join("dtd-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );
}

#[test]
fn converts_dmn_15_decision_tables_as_inert_rule_rows() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_dmn.dmn");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::Dmn);
    let output = temporary.path().join("dmn-out");

    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Dmn);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"dmn\""));
    assert!(svg.contains("Loan approval"));
    assert!(svg.contains("Requires inputData: Applicant age"));
    assert!(svg.contains("Applicant age"));
    assert!(svg.contains("Credit score"));
    assert!(svg.contains("manual review"));
    assert!(svg.contains("FEEL is") && svg.contains("not evaluated."));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let extensionless = temporary.path().join("dmn-content-sniff");
    fs::copy(&fixture, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Dmn
    );
    let generic_xml = temporary.path().join("decision.xml");
    fs::copy(&fixture, &generic_xml).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_xml).unwrap(),
        SourceFormat::Dmn
    );
}

#[test]
fn converts_cmmn_11_case_plan_with_cmmndi_geometry() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_cmmn.cmmn");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::Cmmn);
    let output = temporary.path().join("cmmn-out");
    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Cmmn);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("not evaluated"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"cmmn\""));
    assert!(svg.contains("Claims file"));
    assert!(svg.contains("Review documents"));
    assert!(svg.contains("cmmn:connector"));

    let extensionless = temporary.path().join("case-content-sniff");
    fs::copy(&fixture, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Cmmn
    );
    let wrong = temporary.path().join("wrong.cmmn");
    fs::write(
        &wrong,
        "<definitions xmlns=\"http://example.invalid/not-cmmn\"/>",
    )
    .unwrap();
    assert!(
        convert_path(
            &wrong,
            temporary.path().join("wrong-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );
}

#[test]
fn converts_reqif_requirements_hierarchy_values_and_relations() {
    let temporary = TempDir::new().unwrap();
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_requirements.reqif");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::Reqif);
    let output = temporary.path().join("reqif-out");
    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Reqif);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("XHTML"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"reqif\""));
    assert!(svg.contains("Vehicle braking requirements"));
    assert!(svg.contains("Stopping distance"));
    assert!(svg.contains("Stop within 40 m at 100 km/h"));
    assert!(svg.contains("Approved"));
    assert!(svg.contains("Relations"));
    assert!(svg.contains("Derives"));

    let reqif_xml = temporary.path().join("requirements.reqif.xml");
    fs::copy(&fixture, &reqif_xml).unwrap();
    assert_eq!(
        SourceFormat::detect(&reqif_xml).unwrap(),
        SourceFormat::Reqif
    );
    let extensionless = temporary.path().join("requirements-content-sniff");
    fs::copy(&fixture, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Reqif
    );
}

#[test]
fn converts_xmi_model_elements_and_inert_references() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_model.xmi");
    assert_eq!(SourceFormat::detect(&fixture).unwrap(), SourceFormat::Xmi);
    let output = temporary.path().join("xmi-out");
    let report = convert_path(&fixture, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xmi);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"xmi\""));
    assert!(svg.contains("BrakeController"));
    assert!(svg.contains("WheelSensor"));
    assert!(svg.contains("type=double"));
    assert!(svg.contains("memberEnd=attr-pressure") && svg.contains("attr-speed"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let extensionless = temporary.path().join("model-content-sniff");
    fs::copy(&fixture, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xmi
    );
}

#[test]
fn converts_las_and_laz_point_clouds_with_sampling_and_rgb() {
    let temporary = TempDir::new().unwrap();
    let las_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_rgb.las");
    let output = temporary.path().join("las-out");
    let report = convert_path(&las_path, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Las);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("LAS/LAZ Point Cloud"));
    assert!(svg.contains("data-semantic-role=\"las:point-cloud\""));
    assert!(svg.contains("fill=\"#FF0000\""));
    assert!(svg.contains("fill=\"#00FF00\""));
    assert!(svg.contains("fill=\"#0000FF\""));
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("classification"))
    );

    let extensionless = temporary.path().join("las-header-sniff");
    fs::copy(&las_path, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Las
    );
    let sniffed_output = temporary.path().join("las-sniff-out");
    let sniffed =
        convert_path(&extensionless, &sniffed_output, &ConvertOptions::default()).unwrap();
    assert_eq!(sniffed.source_format, SourceFormat::Las);

    let mut builder = las::Builder::from((1, 2));
    builder.point_format = las::point::Format::new(2).unwrap();
    builder.point_format.is_compressed = true;
    let header = builder.into_header().unwrap();
    let laz_path = temporary.path().join("compressed.laz");
    let mut writer = las::Writer::new(File::create(&laz_path).unwrap(), header).unwrap();
    for point in [
        las::Point {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            color: Some(las::Color::new(65_535, 0, 0)),
            ..Default::default()
        },
        las::Point {
            x: 10.0,
            y: 0.0,
            z: 2.0,
            color: Some(las::Color::new(0, 65_535, 0)),
            ..Default::default()
        },
        las::Point {
            x: 0.0,
            y: 10.0,
            z: 4.0,
            color: Some(las::Color::new(0, 0, 65_535)),
            ..Default::default()
        },
    ] {
        writer.write_point(point).unwrap();
    }
    writer.close().unwrap();
    let laz_output = temporary.path().join("laz-out");
    let laz_report = convert_path(&laz_path, &laz_output, &ConvertOptions::default()).unwrap();
    assert_eq!(laz_report.source_format, SourceFormat::Las);
    let laz_svg = fs::read_to_string(laz_output.join("page-0001.svg")).unwrap();
    assert!(laz_svg.contains("data-semantic-role=\"las:point-cloud\""));
    assert!(laz_svg.contains("fill=\"#FF0000\""));
    assert!(laz_svg.contains("fill=\"#00FF00\""));

    let mut builder = las::Builder::from((1, 4));
    builder.point_format = las::point::Format::new(8).unwrap();
    let header = builder.into_header().unwrap();
    let las14_path = temporary.path().join("las14-nir.las");
    let mut writer = las::Writer::new(File::create(&las14_path).unwrap(), header).unwrap();
    writer
        .write_point(las::Point {
            x: 4.0,
            y: 5.0,
            z: 6.0,
            gps_time: Some(12.5),
            color: Some(las::Color::new(65_535, 32_768, 0)),
            nir: Some(45_000),
            ..Default::default()
        })
        .unwrap();
    writer.close().unwrap();
    let las14_output = temporary.path().join("las14-out");
    let las14_report =
        convert_path(&las14_path, &las14_output, &ConvertOptions::default()).unwrap();
    assert_eq!(las14_report.source_format, SourceFormat::Las);
    let las14_svg = fs::read_to_string(las14_output.join("page-0001.svg")).unwrap();
    assert!(las14_svg.contains("fill=\"#FF8000\""));
    assert!(
        las14_report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("NIR"))
    );
}

#[test]
fn bounds_las_point_data_and_laz_decode_before_parsing_points() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_rgb.las");
    let mut bytes = fs::read(fixture).unwrap();

    let truncated = temporary.path().join("truncated.las");
    bytes[107..111].copy_from_slice(&5u32.to_le_bytes());
    fs::write(&truncated, &bytes).unwrap();
    assert!(
        convert_path(
            &truncated,
            temporary.path().join("truncated-out"),
            &ConvertOptions::default()
        )
        .is_err()
    );

    let oversized_laz = temporary.path().join("oversized.laz");
    bytes[104] = 0x82;
    bytes[107..111].copy_from_slice(&20_000_001u32.to_le_bytes());
    fs::write(&oversized_laz, &bytes).unwrap();
    let error = convert_path(
        &oversized_laz,
        temporary.path().join("oversized-out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(format!("{error}").contains("maximum sequential decode count"));
}

#[test]
fn converts_gerber_layer_extension_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let gtl_input = temporary.path().join("board_top.gtl");
    let out_dir = temporary.path().join("gtl_out");

    let gerber_data =
        "%FSLAX24Y24*%\n%MOIN*%\n%ADD10C,0.050*%\nD10*\nX0000Y0000D03*\nX1000Y1000D03*\nM02*\n";
    fs::write(&gtl_input, gerber_data).unwrap();

    let report = convert_path(&gtl_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gerber);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"pcb-copper-layer\""));
}

#[test]
fn converts_bmp_raster_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let bmp_input = temporary.path().join("test.bmp");
    let out_dir = temporary.path().join("bmp_out");

    // 2x2 24-bit uncompressed BMP: 1 black pixel, 3 white pixels
    let mut bmp_bytes = Vec::new();
    // File header (14 bytes)
    bmp_bytes.extend_from_slice(b"BM");
    let file_size: u32 = 14 + 40 + 8; // 2 rows of 2 pixels (3 bytes each + 2 bytes padding = 4 bytes per row)
    bmp_bytes.extend_from_slice(&file_size.to_le_bytes());
    bmp_bytes.extend_from_slice(&[0, 0, 0, 0]); // reserved
    let offset: u32 = 54;
    bmp_bytes.extend_from_slice(&offset.to_le_bytes());
    // DIB header (40 bytes)
    let dib_size: u32 = 40;
    bmp_bytes.extend_from_slice(&dib_size.to_le_bytes());
    let width: i32 = 2;
    let height: i32 = 2;
    bmp_bytes.extend_from_slice(&width.to_le_bytes());
    bmp_bytes.extend_from_slice(&height.to_le_bytes());
    let planes: u16 = 1;
    let bpp: u16 = 24;
    bmp_bytes.extend_from_slice(&planes.to_le_bytes());
    bmp_bytes.extend_from_slice(&bpp.to_le_bytes());
    let comp: u32 = 0; // BI_RGB
    bmp_bytes.extend_from_slice(&comp.to_le_bytes());
    let img_size: u32 = 8;
    bmp_bytes.extend_from_slice(&img_size.to_le_bytes());
    bmp_bytes.extend_from_slice(&[0; 16]); // ppm & colors

    // Pixel data (bottom-up):
    // Row 0 (bottom): (0,0,0) black, (255,255,255) white, 2 padding bytes
    bmp_bytes.extend_from_slice(&[0, 0, 0, 255, 255, 255, 0, 0]);
    // Row 1 (top): (255,255,255) white, (255,255,255) white, 2 padding bytes
    bmp_bytes.extend_from_slice(&[255, 255, 255, 255, 255, 255, 0, 0]);

    fs::write(&bmp_input, &bmp_bytes).unwrap();

    let report = convert_path(&bmp_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Raster);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"vectorized_path\""));
}

#[test]
fn converts_iges_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let iges_input = temporary.path().join("model.iges");
    let out_dir = temporary.path().join("iges_out");

    let iges_content = format!(
        "{:72}S{:07}\n{:72}G{:07}\n{:72}D{:07}\n{:72}D{:07}\n{:72}P{:07}\n{:72}T{:07}\n",
        "Sample IGES Start Section",
        1,
        "1H,,1H;,4HSTEP,4HFILE,16,38,6,38,15,4HSTEP,1.,1,4HINCH,1,0.01;",
        1,
        "     110       1       0       0       0       0       0       000000000",
        1,
        "     110       0       0       1       0                               0",
        2,
        "110,0.,0.,0.,10.,20.,30.;",
        1,
        "S      1G      1D      2P      1",
        1
    );
    fs::write(&iges_input, iges_content).unwrap();

    let report = convert_path(&iges_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Iges);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"iges_0\""));
}

#[test]
fn converts_threemf_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let threemf_input = temporary.path().join("model.3mf");
    let out_dir = temporary.path().join("threemf_out");

    let file = File::create(&threemf_input).unwrap();
    let mut zip = ZipWriter::new(file);
    zip.start_file("3D/3dmodel.model", SimpleFileOptions::default())
        .unwrap();
    let model_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
  <resources>
    <object id="1" type="model">
      <mesh>
        <vertices>
          <vertex x="0" y="0" z="0" />
          <vertex x="10" y="0" z="0" />
          <vertex x="0" y="10" z="0" />
        </vertices>
        <triangles>
          <triangle v1="0" v2="1" v3="2" />
        </triangles>
      </mesh>
    </object>
  </resources>
</model>"#;
    zip.write_all(model_xml.as_bytes()).unwrap();
    zip.finish().unwrap();

    let report = convert_path(&threemf_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::ThreeMf);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"face_0\""));
}

#[test]
fn follows_3mf_primary_model_relationship_and_applies_build_selection_and_transforms() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("assembly.3mf");
    let output = temporary.path().join("assembly_out");
    let file = File::create(&input).unwrap();
    let mut zip = ZipWriter::new(file);
    zip.start_file("unrelated/preview.model", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"<model/>").unwrap();
    zip.start_file("_rels/.rels", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(
        br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="root" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel" Target="/Models/primary.model"/></Relationships>"#,
    )
    .unwrap();
    zip.start_file("Models/primary.model", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(
        br#"<model xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources>
<object id="1" type="model"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>
<object id="2" type="model"><mesh><vertices><vertex x="20" y="0" z="0"/><vertex x="30" y="0" z="0"/><vertex x="20" y="10" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>
</resources><build><item objectid="2" transform="2 0 0 0 1 0 0 0 1 5 0 0"/></build></model>"#,
    )
    .unwrap();
    zip.finish().unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"face_0\""));
    assert!(!svg.contains("id=\"face_1\""));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn covers_the_full_domain_for_a_radially_symmetric_function_shading() {
    // Real-world regression: a function shading computing distance from
    // its domain's center (sampled at y=0 and y=1, both equidistant from
    // the center at y=0.5) always shows zero change along y at the two
    // points the adaptive tessellator's root cell samples -- not by
    // floating-point coincidence, but because both really do evaluate to
    // the same value for every x. Once that made the tessellator commit
    // to only ever splitting x, x's own change kept "winning" forever,
    // starving y of ever being resampled at new points and leaving the
    // whole shading but a sliver near x=0 completely blank. Also exercises
    // a real PDF's non-standard placement of /Matrix directly on the
    // shading dictionary (not just on a pattern wrapping it) together with
    // /BBox, which a real producer's page-layout PDF used to lay out a
    // grid of shadings and which a previous bug in the BBox clip's own
    // transform separately hid entirely.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("symmetric-function-shading.pdf");
    let mut document = Document::with_version("1.7");
    let function = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into()],
        },
        b"{ 0.5 sub exch 0.5 sub dup mul exch dup mul add sqrt }".to_vec(),
    ));
    let shading = document.add_object(dictionary! {
        "ShadingType" => 1, "ColorSpace" => "DeviceGray",
        "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
        "Matrix" => vec![170.into(), 0.into(), 0.into(), 170.into(), 30.into(), 30.into()],
        "BBox" => vec![30.into(), 30.into(), 200.into(), 200.into()],
        "Function" => function,
    });
    let content = document.add_object(Stream::new(dictionary! {}, b"/SH1 sh\n".to_vec()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 250.into(), 250.into()],
        "Resources" => dictionary! { "Shading" => dictionary! { "SH1" => shading } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(
        report.pages[0].warnings.is_empty(),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let xs: Vec<f64> = svg
        .split("<path ")
        .skip(1)
        .filter(|tag| tag.contains("data-content-kind=\"function-shading-cell\""))
        .filter_map(|tag| {
            // Search for " d=\"" (with the leading space): "id=\"" also
            // contains the bare substring "d=\"", one character in.
            let d_start = tag.find(" d=\"")? + 4;
            let d_end = tag[d_start..].find('"')? + d_start;
            tag[d_start..d_end]
                .split_ascii_whitespace()
                .nth(1)?
                .parse::<f64>()
                .ok()
        })
        .collect();
    assert!(xs.len() > 100, "too few cells: {}", xs.len());
    let min_x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    // A starved tessellation collapses to a sliver near the domain's left
    // edge (x=30); a healthy one spans close to the full 30..200 BBox.
    assert!(
        max_x - min_x > 100.0,
        "cells span only {:.3} of the ~170-wide domain (min={min_x:.3}, max={max_x:.3}); \
         the shading was starved to one edge instead of covering it",
        max_x - min_x
    );
}

#[test]
fn degrades_gracefully_instead_of_failing_a_page_with_a_high_frequency_function_shading() {
    // Real-world regression (pdf.js's own function_based_shading.pdf test
    // file, SH8): a sin(1440 * distance-from-center) ripple oscillates
    // roughly 229 times across its unit domain, so no cell wider than
    // about 1/458th of that domain can ever land within tessellation
    // tolerance -- exhausting the adaptive tessellator's cell budget is
    // expected here, not just on a pathological input. That used to be a
    // hard error that failed the whole page; it must instead degrade to a
    // warning and a lower-resolution rendering of just that shading.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("high-frequency-function-shading.pdf");
    let mut document = Document::with_version("1.7");
    let function = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into()],
        },
        b"{ dup mul exch dup mul add sqrt 1440 mul sin 1 add 2 div }".to_vec(),
    ));
    let shading = document.add_object(dictionary! {
        "ShadingType" => 1, "ColorSpace" => "DeviceGray",
        "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
        "Matrix" => vec![170.into(), 0.into(), 0.into(), 170.into(), 30.into(), 30.into()],
        "Function" => function,
    });
    let content = document.add_object(Stream::new(dictionary! {}, b"/SH1 sh\n".to_vec()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 250.into(), 250.into()],
        "Resources" => dictionary! { "Shading" => dictionary! { "SH1" => shading } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("lower-resolution approximation")),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("data-content-kind=\"function-shading-cell\""),
        "{svg}"
    );
}

#[test]
fn draws_content_after_a_comment_trailing_an_operator_on_the_same_line() {
    // Real-world regression (pdf.js's own calgray.pdf/calrgb.pdf test
    // files): the exact bytes of that file's content stream, byte for
    // byte. Its `%` comments, used purely for column alignment, made the
    // content stream parser drop every single operation with no error and
    // no warning, turning an ordinary page into a silently blank one --
    // but only with this precise formatting: a smaller, hand-written
    // reproduction of "a comment trails an operator" was not enough to
    // reproduce it, so this test uses the real file's own bytes rather
    // than risk exercising a different, merely similar-looking case.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("trailing-comment.pdf");
    let mut document = Document::with_version("1.7");
    let color_space = document.add_object(vec![
        Object::Name(b"CalGray".to_vec()),
        dictionary! {
            "WhitePoint" => vec![Object::Integer(1), Object::Integer(1), Object::Integer(1)],
            "Gamma" => Object::Integer(1),
        }
        .into(),
    ]);
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"\n /Cs5 cs               %\n  0.00 sc              %\n   25   25  200  200 re%\n f                     %\n"
            .to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 250.into(), 250.into()],
        "Resources" => dictionary! { "ColorSpace" => dictionary! { "Cs5" => color_space } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(
        report.pages[0].warnings.is_empty(),
        "{:?}",
        report.pages[0].warnings
    );
    assert_eq!(report.pages[0].node_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#000000"), "{svg}");
}

#[test]
fn converts_visio_open_xml_pages_and_sniffs_extensionless_packages() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("flow.vsdx");
    let package_entries = [
        (
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/visio/2010/relationships/visioDocument" Target="visio/document.xml"/></Relationships>"#,
        ),
        (
            "visio/document.xml",
            r#"<VisioDocument xmlns="urn:visio"/>"#,
        ),
        (
            "visio/_rels/document.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdPages" Type="http://schemas.microsoft.com/visio/2010/relationships/pages" Target="pages/pages.xml"/></Relationships>"#,
        ),
        (
            "visio/pages/pages.xml",
            r#"<Pages xmlns="urn:visio" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><Page ID="0" NameU="Main"><PageSheet><Cell N="PageWidth" V="5"/><Cell N="PageHeight" V="4"/></PageSheet><Rel r:id="rId1"/></Page></Pages>"#,
        ),
        (
            "visio/pages/_rels/pages.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/visio/2010/relationships/page" Target="page1.xml"/></Relationships>"#,
        ),
        (
            "visio/pages/page1.xml",
            r##"<PageContents xmlns="urn:visio"><PageSheet><Cell N="PageWidth" V="5"/><Cell N="PageHeight" V="4"/></PageSheet><Shapes><Shape ID="1" NameU="Process" Type="Shape"><Cell N="PinX" V="2.5"/><Cell N="PinY" V="2"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Cell N="LocPinX" V="1"/><Cell N="LocPinY" V="0.5"/><Cell N="FillForegnd" V="#FF0000"/><Cell N="LineColor" V="#112233"/><Section N="Geometry"><Row T="RelMoveTo"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="1"/></Row><Row T="RelLineTo"><Cell N="X" V="0"/><Cell N="Y" V="1"/></Row></Section><Text>R&amp;D</Text><Section N="Character"><Row IX="0"><Cell N="Color" V="#000000"/><Cell N="Size" V="0.166667"/></Row></Section></Shape></Shapes></PageContents>"##,
        ),
    ];
    make_zip(&input, &package_entries);

    let output = temporary.path().join("vsdx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Visio);
    assert_eq!(report.page_count, 1);
    assert!(
        report.pages[0].warnings.is_empty(),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"visio\""));
    assert!(svg.contains("Main"));
    assert!(svg.contains("R&amp;D"));
    assert!(svg.contains("#FF0000"));

    let extensionless = temporary.path().join("flow.unknown");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Visio
    );
}

#[test]
fn converts_legacy_visio_xml_pages_and_sniffs_extensionless_files() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("flow.vdx");
    let xml = r##"<?xml version="1.0" encoding="UTF-8"?><VisioDocument xmlns="urn:visio" xml:space="preserve"><Pages><Page ID="2" NameU="Legacy flow"><PageSheet><Cell N="PageWidth" V="4"/><Cell N="PageHeight" V="3"/></PageSheet><Shapes><Shape ID="7" NameU="Process" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" V="1.5"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Cell N="FillForegnd" V="#22AA44"/><Section N="Geometry"><Row T="RelMoveTo"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="1"/></Row></Section><Text>Legacy &amp; editable</Text></Shape></Shapes></Page></Pages></VisioDocument>"##;
    fs::write(&input, xml).unwrap();

    let output = temporary.path().join("vdx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Visio);
    assert_eq!(report.page_count, 1);
    assert!(
        report.pages[0].warnings.is_empty(),
        "{:?}",
        report.pages[0].warnings
    );
    assert_eq!(report.pages[0].width_points, 288.0);
    assert_eq!(report.pages[0].height_points, 216.0);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Legacy &amp; editable"));
    assert!(svg.contains("#22AA44"));

    let extensionless = temporary.path().join("flow.xml");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Visio
    );
}

#[test]
fn converts_eml_html_alternative_without_fetching_remote_resources_or_attachments() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("message.eml");
    let message = concat!(
        "From: =?UTF-8?Q?Alice_Example?= <alice@example.test>\r\n",
        "To: Bob <bob@example.test>\r\n",
        "Date: Tue, 10 Sep 2024 12:30:00 +0000\r\n",
        "Subject: =?UTF-8?Q?Quarterly_update_=E2=9C=93?=\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=outer\r\n\r\n",
        "--outer\r\n",
        "Content-Type: multipart/alternative; boundary=alt\r\n\r\n",
        "--alt\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
        "Revenue increased by 12=25.\r\n\r\nNext steps follow.\r\n",
        "--alt\r\n",
        "Content-Type: text/html; charset=utf-8\r\n\r\n",
        "<html><body><p>Styled alternative.</p><img src=\"https://example.invalid/pixel.png\"></body></html>\r\n",
        "--alt--\r\n",
        "--outer\r\n",
        "Content-Type: application/octet-stream; name=secret.bin\r\n",
        "Content-Disposition: attachment; filename=secret.bin\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "U0VDUkVU\r\n",
        "--outer--\r\n",
    );
    fs::write(&input, message).unwrap();

    let output = temporary.path().join("eml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Eml);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("attachment"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("HTML image source")),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Quarterly update ✓"));
    assert!(svg.contains("Alice Example"));
    assert!(svg.contains("alice@example.test"));
    assert!(svg.contains("Styled alternative."));
    assert!(!svg.contains("Revenue increased by 12%"));
    assert!(!svg.contains("SECRET"));
    assert!(!svg.contains("example.invalid/pixel.png"));

    let extensionless = temporary.path().join("message.unknown");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Eml
    );
}

#[test]
fn unfolds_rfc3676_flowed_plain_text_and_space_stuffing() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flowed_message.eml");
    let output = temporary.path().join("flowed-eml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Eml);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("RFC 3676 flowed plain text"))
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("HTML e-mail alternative is shown"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("A flowed line with a preserved word."));
    assert!(svg.contains("From this sender."));
    assert!(svg.contains("&gt; quoted words continue."));
    assert!(svg.contains("--"));
    assert!(svg.contains("Signature line."));
}

#[test]
fn converts_eml_cid_inline_png_as_a_safe_html_image() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline-image.eml");
    let png =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"))
            .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let message = format!(
        "From: Alice <alice@example.test>\r\nSubject: Inline image\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=eml; type=\"text/html\"\r\n\r\n--eml\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>Before image.</p><img src=\"cid:eml-logo\" alt=\"brand logo\"><img src=\"https://example.invalid/eml-logo.png\" alt=\"location logo\"><img src=\"https://example.invalid/tracker.png\"><script>must not render</script></body></html>\r\n--eml\r\nContent-Type: image/png\r\nContent-ID: <eml-logo>\r\nContent-Location: https://example.invalid/eml-logo.png\r\nContent-Transfer-Encoding: base64\r\n\r\n{encoded}\r\n--eml--\r\n"
    );
    fs::write(&input, message).unwrap();

    let output = temporary.path().join("eml-inline-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Eml);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("HTML image source"))
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("1 mail attachment/resource part"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Before image."));
    assert!(svg.contains("brand logo"));
    assert!(svg.contains("location logo"));
    assert_eq!(svg.matches("data:image/png;base64,").count(), 2);
    assert!(!svg.contains("example.invalid"));
    assert!(!svg.contains("must not render"));
}

#[test]
fn converts_emlx_only_within_declared_message_bytes_and_sniffs_extensionless_input() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.emlx");
    let output = temporary.path().join("emlx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Emlx);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("property-list metadata was ignored"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Apple Mail message"));
    assert!(svg.contains("EMLX body text."));
    assert!(svg.contains("日本語の本文です。"));
    assert!(!svg.contains("plist"));
    assert!(!svg.contains("flags"));

    let extensionless = temporary.path().join("apple-mail-message");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Emlx
    );
}

#[test]
fn converts_mbox_messages_to_separate_sequential_svg_pages() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("archive.mbox");
    let mbox = concat!(
        "From alice@example.test Sat Sep 14 10:00:00 2024\r\n",
        "From: Alice <alice@example.test>\r\n",
        "Subject: First message\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n\r\n",
        "First archived body.\r\n\r\n",
        "From bob@example.test Sun Sep 15 11:30:00 2024\r\n",
        "From: Bob <bob@example.test>\r\n",
        "Subject: Second message\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n\r\n",
        "Second archived body.\r\n",
    );
    fs::write(&input, mbox).unwrap();

    let output = temporary.path().join("mbox-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Mbox);
    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("First message"));
    assert!(first.contains("First archived body."));
    assert!(second.contains("Second message"));
    assert!(second.contains("Second archived body."));

    let extensionless = temporary.path().join("archive.unknown");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Mbox
    );
}

#[test]
fn converts_mhtml_html_part_without_fetching_referenced_resources() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("saved-page.mhtml");
    let mhtml = concat!(
        "MIME-Version: 1.0\r\n",
        "Subject: Saved page\r\n",
        "Content-Type: multipart/related; boundary=archive; type=\"text/html\"\r\n\r\n",
        "--archive\r\n",
        "Content-Type: text/html; charset=utf-8\r\n",
        "Content-Location: https://example.invalid/saved/page.html\r\n\r\n",
        "<html><body><h1>Archived heading</h1><p>Archived web content.</p>",
        "<script>must not render</script><img src=\"cid:inline-image\" alt=\"inline logo\">",
        "<img src=\"cid:INLINE-image\" alt=\"case mismatch\">",
        "<img src=\"https://example.invalid/images/inline.png\" alt=\"location logo\">",
        "<img src=\"https://example.invalid/tracker.png\">",
        "</body></html>\r\n",
        "--archive\r\n",
        "Content-Type: image/png\r\nContent-ID: <inline-image>\r\n",
        "Content-Location: https://example.invalid/images/inline.png\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=\r\n",
        "--archive\r\n",
        "Content-Type: application/octet-stream; name=secret.txt\r\n",
        "Content-Disposition: attachment; filename=secret.txt\r\n",
        "Content-Transfer-Encoding: base64\r\n\r\n",
        "U0VDUkVU\r\n",
        "--archive--\r\n",
    );
    fs::write(&input, mhtml).unwrap();

    let output = temporary.path().join("mhtml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Mhtml);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("image source"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("part(s)"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Saved page"));
    assert!(svg.contains("Archived heading"));
    assert!(svg.contains("Archived web content."));
    assert_eq!(svg.matches("data:image/png;base64,").count(), 2);
    assert!(svg.contains("location logo"));
    assert!(svg.contains("case mismatch"));
    assert!(!svg.contains("must not render"));
    assert!(!svg.contains("example.invalid"));
    assert!(!svg.contains("SECRET"));

    let extensionless = temporary.path().join("saved-page.unknown");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Mhtml
    );
}

#[test]
fn resolves_mhtml_relative_content_locations_against_html_and_mime_bases() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("relative-resource.mhtml");
    let message = concat!(
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/related; boundary=archive; type=\"text/html\"\r\n",
        "Content-Base: https://example.test/archive/\r\n\r\n",
        "--archive\r\nContent-Type: text/html; charset=utf-8\r\n",
        "Content-Location: page.html\r\n\r\n",
        "<html><head><base href=\"assets/\"></head><body><h1>Relative base</h1>",
        "<img src=\"logo.png\" alt=\"relative logo\"></body></html>\r\n",
        "--archive\r\nContent-Type: image/png\r\n",
        "Content-Location: assets/logo.png\r\nContent-Transfer-Encoding: base64\r\n\r\n",
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=\r\n",
        "--archive--\r\n",
    );
    fs::write(&input, message).unwrap();

    let output = temporary.path().join("relative-resource-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Mhtml);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Relative base"));
    assert!(svg.contains("relative logo"));
    assert_eq!(svg.matches("data:image/png;base64,").count(), 1);
    assert!(!svg.contains("logo.png"));
}

#[test]
fn rejects_mhtml_start_parameter_that_does_not_name_a_direct_related_part() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("invalid-start.mhtml");
    let message = concat!(
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/related; boundary=archive; type=\"text/html\"; start=\"<missing>\"\r\n\r\n",
        "--archive\r\nContent-Type: text/html; charset=utf-8\r\nContent-ID: <actual>\r\n\r\n<html><body>Not selected</body></html>\r\n",
        "--archive--\r\n",
    );
    fs::write(&input, message).unwrap();
    let output = temporary.path().join("invalid-start-out");
    assert!(convert_path(&input, &output, &ConvertOptions::default()).is_err());
}

#[test]
fn converts_ical_events_and_tasks_without_expanding_recurrence_or_timezones() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("schedule.ics");
    let calendar = concat!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Planning\r\n",
        "BEGIN:VEVENT\r\nUID:event-1\r\nDTSTAMP:20240912T120000Z\r\n",
        "DTSTART;TZID=America/New_York:20240913T100000\r\n",
        "DTEND;TZID=America/New_York:20240913T110000\r\n",
        "SUMMARY:Quarterly planning \r\n review\r\n",
        "DESCRIPTION:Prepare the agenda\\nSend the notes\r\n",
        "LOCATION:Conference Room\r\nRRULE:FREQ=DAILY;COUNT=3\r\n",
        "BEGIN:VALARM\r\nTRIGGER:-PT15M\r\nACTION:DISPLAY\r\nEND:VALARM\r\n",
        "END:VEVENT\r\n",
        "BEGIN:VTODO\r\nUID:todo-1\r\nSUMMARY:Follow up\r\n",
        "DUE;VALUE=DATE:20240920\r\nDESCRIPTION:Send the report\r\nEND:VTODO\r\n",
        "END:VCALENDAR\r\n",
    );
    fs::write(&input, calendar).unwrap();

    let output = temporary.path().join("ical-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ical);
    assert_eq!(report.page_count, 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("recurrence"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("TZID"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("VALARM"))
    );
    let event = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let task = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(event.contains("Quarterly planning review"));
    assert!(event.contains("Prepare the agenda"));
    assert!(event.contains("Send the notes"));
    assert!(event.contains("2024-09-13 10:00:00 (TZID=America/New_York)"));
    assert!(event.contains("FREQ=DAILY;COUNT=3"));
    assert!(task.contains("Follow up"));
    assert!(task.contains("2024-09-20 (all-day)"));

    let extensionless = temporary.path().join("schedule.unknown");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ical
    );
}

#[test]
fn converts_vcalendar_10_legacy_events_as_calendar_pages() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("meeting.vcs");
    let source = fs::read_to_string("tests/fixtures/meeting.vcs").unwrap();
    fs::write(&input, source.replace('\n', "\r\n")).unwrap();
    let output = temporary.path().join("vcalendar-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Vcalendar);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("unsupported"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Legacy vCalendar meeting"));
    assert!(svg.contains("Old-format calendar export with a readable summary."));
    assert!(svg.contains("Conference room"));
    assert!(!svg.contains("DALARM"));

    let extensionless = temporary.path().join("meeting.data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Vcalendar
    );
}

#[test]
fn parses_standard_html_void_tags_and_warns_when_image_resources_are_omitted() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("void-tags.html");
    fs::write(
        &input,
        "<!doctype html><html><head><meta charset=\"utf-8\"></head><body><p>before<img src=\"https://example.invalid/remote.png\"><br>after</p></body></html>",
    )
    .unwrap();
    let output = temporary.path().join("html-void-tags-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Html);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("external image resources"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("before"));
    assert!(svg.contains("after"));
    assert!(!svg.contains("example.invalid"));
}

#[test]
fn converts_outlook_msg_and_sniffs_extensionless_compound_documents() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("message.msg");
    fs::write(&input, common::outlook_msg_bytes()).unwrap();
    let output = temporary.path().join("msg-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Msg);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("attachment"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("script"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Outlook preview"));
    assert!(svg.contains("Rina Example &lt;rina@example.test&gt;"));
    assert!(svg.contains("Kai Example &lt;kai@example.test&gt;"));
    assert!(svg.contains("Mina Example &lt;mina@example.test&gt;"));
    assert!(svg.contains("Rendered"));
    assert!(svg.contains("HTML"));
    assert!(svg.contains("日本語表示"));
    assert!(svg.contains("Company mark"));
    assert!(svg.contains("Attachments: 1 omitted"));
    assert!(!svg.contains("window.alert"));
    assert!(!svg.contains("example.invalid"));
    assert!(!svg.contains("Plain body fallback"));

    let extensionless = temporary.path().join("message.data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Msg
    );
}

#[test]
fn converts_vcard_contacts_and_sniffs_extensionless_cards() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("contacts.vcf");
    let source = fs::read_to_string("tests/fixtures/contact.vcf").unwrap();
    fs::write(&input, source.replace('\n', "\r\n")).unwrap();
    let output = temporary.path().join("vcard-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Vcard);
    assert_eq!(report.page_count, 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("photo"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("unsupported"))
    );
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("山田花子"));
    assert!(first.contains("Example, Inc. / Research"));
    assert!(first.contains("Phone (cell, voice)"));
    assert!(first.contains("tel:+81-90-1234-5678"));
    assert!(first.contains("中央1丁目"));
    assert!(first.contains("Second line"));
    assert!(first.contains("https://example.test/contact"));
    assert!(!first.contains("example.invalid/avatar.jpg"));
    assert!(second.contains("Ren Tanaka"));
    assert!(second.contains("Example Studio"));
    assert!(second.contains("ren@example.test"));
    assert!(!second.contains("ignored"));

    let extensionless = temporary.path().join("contact.data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Vcard
    );
}

#[test]
fn converts_vcard_21_quoted_printable_and_bare_type_parameters() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("legacy-contact.vcf");
    let source = fs::read_to_string("tests/fixtures/legacy_contact_21.vcf").unwrap();
    fs::write(&input, source.replace('\n', "\r\n")).unwrap();
    let output = temporary.path().join("legacy-vcard-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Vcard);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("photo"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hanako 山田花子"));
    assert!(svg.contains("Organization: Example; Corp"));
    assert!(svg.contains("Email (internet, home): hana@example.test"));
    assert!(svg.contains("Phone (cell, voice): +81-90-1234-5678"));
    assert!(svg.contains("Second line"));
    assert!(svg.contains("Legacy folded line"));
    assert!(!svg.contains("AA=="));
}

#[test]
fn decodes_declared_vcard_21_legacy_charset() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("latin-contact.vcf");
    fs::write(
        &input,
        b"BEGIN:VCARD\r\nVERSION:2.1\r\nFN;CHARSET=ISO-8859-1:Jos\xE9\r\nEND:VCARD\r\n",
    )
    .unwrap();
    let output = temporary.path().join("latin-vcard-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("José"));
}

#[test]
fn rejects_unknown_vcard_versions_instead_of_mislabeling_them() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("legacy.vcf");
    fs::write(
        &input,
        "BEGIN:VCARD\r\nVERSION:5.0\r\nFN:Unknown Version\r\nEND:VCARD\r\n",
    )
    .unwrap();
    let output = temporary.path().join("legacy-out");

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("versions 2.1, 3.0, and 4.0 are supported")
    );
}

#[test]
fn converts_webp_raster_images_and_sniffs_extensionless_input() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.webp");
    fs::write(&input, common::webp_sample()).unwrap();
    let output = temporary.path().join("webp-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Raster);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("vectorized_path"));

    let extensionless = temporary.path().join("sample.data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Raster
    );
}

#[test]
fn converts_animated_gif_first_frame_and_sniffs_extensionless_input() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("animation.gif");
    fs::write(&input, common::animated_gif_sample()).unwrap();
    let output = temporary.path().join("gif-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Raster);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("first frame"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("partial first GIF frame"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("vectorized_path"));

    let extensionless = temporary.path().join("animation.data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Raster
    );
}

#[test]
fn vectorizes_cmyk_jpeg_instead_of_returning_a_blank_fallback() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("cmyk.jpg");
    fs::write(&input, common::cmyk_jpeg_sample()).unwrap();
    let output = temporary.path().join("cmyk-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Raster);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("CMYK JPEG"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("vectorized_path"));
    assert!(svg.contains("M 0,0"));
}

#[test]
fn renders_package_linked_opendocument_presentation_images() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("embedded-image.odp");
    fs::write(&input, common::odp_presentation_with_embedded_image()).unwrap();
    let output = temporary.path().join("odp-image-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Odp);
    assert_eq!(report.page_count, 1);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("embedded images")
                || warning.contains("image parts were omitted")),
        "unexpected ODP image omission warning: {:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("odp-image-1"));
}

#[test]
fn renders_flat_fodp_inline_binary_png_in_a_frame() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline.fodp");
    let output = temporary.path().join("inline-fodp-out");
    let xml = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:presentation><draw:page draw:name="Inline"><draw:frame draw:name="image" svg:x="1cm" svg:y="1cm" svg:width="4cm" svg:height="2cm"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame></draw:page></office:presentation></office:body></office:document-content>"#;
    fs::write(&input, xml).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Odp);
    assert_eq!(report.page_count, 1);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("inline office:binary-data images are omitted"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/png;base64,"));
}

#[test]
fn renders_package_linked_opendocument_text_images_as_bounded_flow_blocks() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odt_image.odt");
    let output = temporary.path().join("odt-image-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Odt);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("centered flow blocks"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Before image"));
    assert!(svg.contains("after image."));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("aria-label=\"red-blue sample\""));
}

#[test]
fn renders_flat_fodt_inline_binary_png_with_shared_image_limits() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline-image.fodt");
    let output = temporary.path().join("inline-image-out");
    let xml = r#"<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:text><text:p>Before inline image<draw:frame draw:name="inline"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame>after inline image</text:p></office:text></office:body></office:document>"#;
    fs::write(&input, xml).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Odt);
    assert_eq!(report.page_count, 1);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("binary-data images are omitted"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Before inline image"));
    assert!(svg.contains("after inline image"));
    assert!(svg.contains("data:image/png;base64,"));
}

#[test]
fn renders_bounded_opendocument_paragraph_and_character_styles() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odt_styles.odt");
    let output = temporary.path().join("odt-style-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Odt);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Styled ODT heading"));
    assert!(svg.contains("Blue bold paragraph"));
    assert!(svg.contains("inherited bold red italic"));
    assert!(svg.contains("automatic green italic"));
    assert!(svg.contains("#1D4ED8"));
    assert!(svg.contains("#DC2626"));
    assert!(svg.contains("#16A34A"));
    assert!(svg.contains("DejaVu Serif"));
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("styles were not found")),
        "unexpected missing-style warning: {:?}",
        report.warnings
    );
}

#[test]
fn does_not_fetch_external_or_escape_package_opendocument_images() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("untrusted-images.odt");
    let output = temporary.path().join("untrusted-images-out");
    let mut package = ZipWriter::new(fs::File::create(&input).unwrap());
    package
        .start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
    package
        .write_all(b"application/vnd.oasis.opendocument.text")
        .unwrap();
    package
        .start_file("content.xml", SimpleFileOptions::default())
        .unwrap();
    package
        .write_all(br#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:text><text:p>Untrusted sources <draw:frame draw:name="external"><draw:image xlink:href="https://example.invalid/image.png"/></draw:frame> and <draw:frame draw:name="escaped"><draw:image xlink:href="../outside.png"/></draw:frame></text:p></office:text></office:body></office:document-content>"#)
        .unwrap();
    package.finish().unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("external ODT image"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("escaped the package"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(!svg.contains("data:image/"));
}

#[test]
fn embeds_package_linked_epub_images_and_omits_remote_or_escaped_sources() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/epub_image.epub");
    let output = temporary.path().join("epub-image-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Epub);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("remote EPUB image"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("EPUB image paths escaping the package"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("srcset uses its first candidate"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Before image"));
    assert!(svg.contains("after image."));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("aria-label=\"red-blue EPUB image\""));
    assert!(!svg.contains("example.invalid"));
}

#[test]
fn embeds_package_linked_ods_images_after_their_sheet_table() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ods_image.ods");
    let output = temporary.path().join("ods-image-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Ods);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("after the sheet table"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("external ODS image"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("escaped the package"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Requests"));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("aria-label=\"red-blue sheet image\""));
    assert!(svg.contains("aria-label=\"cell-anchored image\""));
    assert!(!svg.contains("example.invalid"));
}

#[test]
fn renders_flat_fods_inline_binary_png_after_the_sheet_table() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline.fods");
    let output = temporary.path().join("inline-fods-out");
    let xml = r#"<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:spreadsheet><table:table table:name="Sheet"><table:table-row><table:table-cell><text:p>Cell</text:p><draw:frame draw:name="inline"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document>"#;
    fs::write(&input, xml).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Ods);
    assert_eq!(report.page_count, 1);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("binary-data images are omitted"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("Cell"));
}

#[test]
fn embeds_local_html_images_and_omits_remote_or_escaped_sources() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local_image.html");
    let output = temporary.path().join("html-image-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Html);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("external image resources"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("outside the input directory"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("srcset uses its first candidate"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("centered flow blocks"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Before"));
    assert!(svg.contains("after."));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("aria-label=\"red-blue HTML image\""));
    assert!(!svg.contains("example.invalid"));

    let limited = ConvertOptions {
        max_input_bytes: 4,
        ..ConvertOptions::default()
    };
    assert!(matches!(
        convert_path(&input, temporary.path().join("html-limited-out"), &limited),
        Err(document_svg::Error::LimitExceeded(_))
    ));
}

#[test]
fn embeds_local_markdown_images_and_omits_remote_or_escaped_sources() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/markdown_image.md");
    let output = temporary.path().join("markdown-image-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Markdown);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("external image resources"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("Markdown images are rendered"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Text before the standalone image."));
    assert!(svg.contains("Text before an inline image"));
    assert!(svg.contains("with trailing text."));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("aria-label=\"red-blue Markdown image\""));
    assert!(svg.contains("aria-label=\"red-blue inline image\""));
    assert!(svg.contains("aria-label=\"red-blue reference image\""));
    assert!(!svg.contains("example.invalid"));
}

#[test]
fn converts_restructuredtext_and_sniffs_section_adornments() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rst");
    let output = temporary.path().join("rst-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Rst);
    assert!(report.page_count >= 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("include directive was not evaluated"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("raw directive was not evaluated"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("external image resources"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("reStructuredText images are embedded"))
    );
    let svg = (1..=report.page_count)
        .map(|page| fs::read_to_string(output.join(format!("page-{page:04}.svg"))).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(svg.contains("Document Conversion Notes"));
    assert!(svg.contains("named target"));
    assert!(!svg.contains(":ref:"));
    assert!(svg.contains("[NOTE]"));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("aria-label=\"RST red and blue image\""));
    assert!(svg.contains("Figure caption is retained"));
    assert!(svg.contains("Parser"));
    assert!(!svg.contains("example.invalid"));
    assert!(!svg.contains("must-not-be-read"));
    assert!(!svg.contains("must-not-run"));

    let extensionless = temporary.path().join("sectioned-document");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Rst
    );
}

#[test]
fn converts_org_mode_and_keeps_babel_and_file_insertion_inactive() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.org");
    let output = temporary.path().join("org-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Org);
    assert!(report.page_count >= 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("#+INCLUDE was not evaluated"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("Babel calls were not executed"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("raw/export blocks were omitted"))
    );
    let svg = (1..=report.page_count)
        .map(|page| fs::read_to_string(output.join(format!("page-{page:04}.svg"))).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(svg.contains("Org-mode Architecture Notes"));
    assert!(svg.contains("Components"));
    assert!(svg.contains("Ready"));
    assert!(svg.contains("delete-file"));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("width=\"96\" height=\"48\""));
    assert!(svg.contains("Local Org image caption"));
    assert!(!svg.contains("example.invalid"));
    assert!(!svg.contains("must-not-render"));
    assert!(!svg.contains("/etc/passwd"));

    let extensionless = temporary.path().join("org-document");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Org
    );
}

#[test]
fn converts_gettext_po_plural_context_and_fuzzy_entries() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.po");
    let output = temporary.path().join("po-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Po);
    assert!(report.page_count >= 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("obsolete gettext entries"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("fuzzy gettext translations"))
    );
    let svg = (1..=report.page_count)
        .map(|page| fs::read_to_string(output.join(format!("page-{page:04}.svg"))).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(svg.contains("Project-Id-Version Sample App 1.0"));
    assert!(svg.contains("Language fr"));
    assert!(svg.contains("main-menu"));
    assert!(svg.contains("Bienvenue"));
    assert!(svg.contains("One file"));
    assert!(svg.contains("%d files"));
    assert!(svg.contains("[0] Un fichier"));
    assert!(svg.contains("[1] %d fichiers"));
    assert!(svg.contains("[untranslated]"));
    assert!(svg.contains("[fuzzy] À vérifier"));
    assert!(svg.contains("Label on the main menu"));
    assert!(!svg.contains("Obsolete source text"));

    let extensionless = temporary.path().join("translation-catalog");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Po
    );
}

#[test]
fn converts_bibtex_entries_without_expanding_macros_or_running_tex() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.bib");
    let output = temporary.path().join("bib-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Bib);
    assert!(report.page_count >= 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("macros or concatenations"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("@string macros are not expanded"))
    );
    let svg = (1..=report.page_count)
        .map(|page| fs::read_to_string(output.join(format!("page-{page:04}.svg"))).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(svg.contains("@article{smith2024}"));
    assert!(svg.contains("Nested Unicode Study"));
    assert!(svg.contains("A value, with punctuation."));
    assert!(svg.contains("joc Review"));
    assert!(!svg.contains("Catalog preview example"));

    let extensionless = temporary.path().join("bibliography");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Bib
    );

    let bibtex_extension = temporary.path().join("bibliography.bibtex");
    fs::copy(&input, &bibtex_extension).unwrap();
    assert_eq!(
        SourceFormat::detect(&bibtex_extension).unwrap(),
        SourceFormat::Bib
    );
}

#[test]
fn converts_srt_and_webvtt_with_inert_cue_text_and_extensionless_detection() {
    let temporary = TempDir::new().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (filename, format, expected, omitted) in [
        (
            "sample.srt",
            SourceFormat::Srt,
            &[
                "00:00:01.250",
                "Hello world.",
                "字幕の2行目。",
                "not executed",
            ][..],
            "<i>",
        ),
        (
            "sample.vtt",
            SourceFormat::Vtt,
            &[
                "English captions",
                "Narrator: Welcome aboard.",
                "chapter-one",
            ][..],
            "This comment must not appear",
        ),
    ] {
        let input = fixtures.join(filename);
        let output = temporary.path().join(format!("{filename}-out"));
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, format);
        assert_eq!(report.page_count, 1);
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        for text in expected {
            assert!(svg.contains(text), "missing {text:?} in {filename}");
        }
        assert!(!svg.contains(omitted));
        if format == SourceFormat::Vtt {
            assert!(
                report
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("NOTE"))
            );
            assert!(
                report
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("STYLE/REGION"))
            );
            assert!(
                report
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("positioning/settings"))
            );
            assert!(svg.contains("[chapter-one]"));
        } else {
            assert!(svg.contains("&lt;script&gt;"));
            assert!(!svg.contains("<script>"));
        }

        let extensionless = temporary.path().join(format!("{filename}.data"));
        fs::copy(&input, &extensionless).unwrap();
        assert_eq!(SourceFormat::detect(&extensionless).unwrap(), format);
    }
}

#[test]
fn converts_ttml_text_profile_with_bounded_timing_and_xml_sniffing() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ttml");
    let output = temporary.path().join("ttml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Ttml);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("styles, regions"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("00:00:01.250–00:00:03.500 [intro]"));
    assert!(svg.contains("Welcome aboard."));
    assert!(svg.contains("安全な字幕"));
    assert!(svg.contains("00:00:03.500–00:00:04.500"));
    assert!(svg.contains("Literal &lt;script&gt; stays text."));
    assert!(!svg.contains("<script>"));

    let generic_xml = temporary.path().join("captions.xml");
    fs::copy(&input, &generic_xml).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_xml).unwrap(),
        SourceFormat::Ttml
    );
    let extensionless = temporary.path().join("captions");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ttml
    );
    let dfxp = temporary.path().join("captions.dfxp");
    fs::copy(&input, &dfxp).unwrap();
    assert_eq!(SourceFormat::detect(&dfxp).unwrap(), SourceFormat::Ttml);
}

#[test]
fn converts_xliff_source_target_and_status_without_alternate_translation_confusion() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xliff");
    let output = temporary.path().join("xliff-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Xliff);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Languages: en → ja"));
    assert!(svg.contains("Unit welcome — Welcome message"));
    assert!(svg.contains("Source (en): Hello [pc:emphasis]world[/pc][ph:count]!"));
    assert!(svg.contains("Segment 1 [translated]"));
    assert!(svg.contains("Target (ja):"));
    assert!(svg.contains("[pc:emphasis]世界[/pc][ph:count]"));
    assert!(svg.contains("！"));
    assert!(svg.contains("Segment 1 [initial]"));
    assert!(svg.contains("Target (ja): [untranslated]"));
    assert!(svg.contains("Note: location: Home screen"));
    assert!(svg.contains("&lt;script&gt; stays text"));
    assert!(!svg.contains("<script>"));

    let generic_xml = temporary.path().join("messages.xml");
    fs::copy(&input, &generic_xml).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_xml).unwrap(),
        SourceFormat::Xliff
    );
    let extensionless = temporary.path().join("messages");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xliff
    );
}

#[test]
fn converts_unv_universal_mesh_and_detects_extensionless_files() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.unv");
    let output = temporary.path().join("unv-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Unv);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("nonzero Z"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("UNV Universal FEA mesh"));
    assert!(svg.contains("data-source-format=\"unv\""));
    assert!(svg.contains("data-semantic-role=\"simulation:mesh\""));

    let extensionless = temporary.path().join("universal-mesh");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Unv
    );
    let extensionless_output = temporary.path().join("unv-extensionless-out");
    let extensionless_report = convert_path(
        &extensionless,
        &extensionless_output,
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(extensionless_report.source_format, SourceFormat::Unv);
    assert_eq!(extensionless_report.page_count, 1);
}

#[test]
fn converts_general_json_as_bounded_path_and_type_rows() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.json");
    let output = temporary.path().join("json-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Json);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("$.service.enabled (boolean): true"));
    assert!(svg.contains("$.service.ports[1] (number): 8081"));
    assert!(svg.contains("$.empty (object): {}"));
    assert!(svg.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!svg.contains("<script>"));

    let extensionless = temporary.path().join("structured-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Json
    );

    let generic_elements = temporary.path().join("elements.json");
    fs::write(&generic_elements, r#"{"elements":["unit","second"]}"#).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_elements).unwrap(),
        SourceFormat::Json
    );
}

#[test]
fn converts_glb_scene_geometry_with_node_instances_and_warns_about_appearance() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.glb");
    let output = temporary.path().join("gltf-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Gltf);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("materials, textures"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"gltf\""));
    assert!(svg.contains("data-semantic-role=\"gltf:mesh\""));
}

#[test]
fn converts_json_gltf_with_confined_external_buffer_and_sniffs_glb() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.gltf");
    let output = temporary.path().join("gltf-json-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Gltf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"gltf:mesh\""));

    let extensionless = temporary.path().join("model.data");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.glb"),
        &extensionless,
    )
    .unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Gltf
    );
}

#[test]
fn gltf_buffer_uris_cannot_escape_or_fetch_the_asset_directory() {
    let temporary = TempDir::new().unwrap();
    let asset_dir = temporary.path().join("asset");
    fs::create_dir_all(&asset_dir).unwrap();
    fs::write(temporary.path().join("outside.bin"), [0u8; 4]).unwrap();

    for (index, uri) in ["../outside.bin", "https://example.invalid/buffer.bin"]
        .into_iter()
        .enumerate()
    {
        let input = asset_dir.join(format!("unsafe-{index}.gltf"));
        let source = format!(
            r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"uri":"{uri}","byteLength":4}}]}}"#
        );
        fs::write(&input, source).unwrap();
        let result = convert_path(
            &input,
            temporary.path().join(format!("unsafe-out-{index}")),
            &ConvertOptions::default(),
        );
        assert!(
            matches!(result, Err(document_svg::Error::InvalidInput(message)) if message.contains("external or escapes"))
        );
    }
}

#[test]
fn embeds_bounded_png_pictures_from_rtf_destinations() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rtf_image.rtf");
    let output = temporary.path().join("rtf-picture-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Rtf);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("picture anchors"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Before image"));
    assert!(svg.contains("after image."));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("aria-label=\"Embedded RTF picture\""));
}

#[test]
fn embeds_bounded_asciidoc_block_images_from_imagesdir() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/asciidoc_image.adoc");
    let input = temporary.path().join("image-appendix.adoc");
    fs::copy(&fixture, &input).unwrap();
    fs::create_dir_all(input.parent().unwrap().join("assets")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        input.parent().unwrap().join("assets/red-blue.png"),
    )
    .unwrap();
    let output = temporary.path().join("asciidoc-image-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Asciidoc);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("centered flow blocks")),
        "{:?}",
        report.warnings
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("not a validated local PNG/JPEG")),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("This paragraph comes before the figure."));
    assert!(svg.contains("This paragraph comes after the figure."));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("aria-label=\"Red and blue test image\""));

    let extensionless = temporary.path().join("asciidoc-image");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Asciidoc
    );
}

#[test]
fn previews_complete_latex_document_without_executing_tex_or_external_includes() {
    let temporary = TempDir::new().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_latex.tex");
    let input = temporary.path().join("sample.tex");
    fs::copy(&fixture, &input).unwrap();
    fs::create_dir_all(input.parent().unwrap().join("assets")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        input.parent().unwrap().join("assets/red-blue.png"),
    )
    .unwrap();
    let output = temporary.path().join("latex-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Tex);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("input/include/shell")),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("LaTeX Document Preview"));
    assert!(svg.contains("Deterministic document structure"));
    assert!(svg.contains("Preview"));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("Local red and blue figure"));
    assert!(!svg.contains("not-loaded.tex"));

    let extensionless = temporary.path().join("latex-document");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Tex
    );
}

#[test]
fn converts_fictionbook_body_and_embedded_png_without_rendering_notes_body() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fb2");
    let output = temporary.path().join("fb2-out");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Fb2);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("FictionBook Preview"));
    assert!(svg.contains("Chapter One"));
    assert!(svg.contains("After the embedded figure."));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(!svg.contains("Notes are not part of the main flow"));

    let extensionless = temporary.path().join("book-content");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Fb2
    );

    let archive_path = temporary.path().join("book.fb2.zip");
    let mut archive = ZipWriter::new(fs::File::create(&archive_path).unwrap());
    archive
        .start_file("books/sample.fb2", SimpleFileOptions::default())
        .unwrap();
    archive.write_all(&fs::read(&input).unwrap()).unwrap();
    archive.finish().unwrap();
    let archive_output = temporary.path().join("fb2-zip-out");
    let archive_report =
        convert_path(&archive_path, &archive_output, &ConvertOptions::default()).unwrap();
    assert_eq!(archive_report.source_format, SourceFormat::Fb2);
    assert_eq!(archive_report.page_count, 1);
    let archive_extensionless = temporary.path().join("book-archive");
    fs::copy(&archive_path, &archive_extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&archive_extensionless).unwrap(),
        SourceFormat::Fb2
    );
}

#[test]
fn converts_mobi_palmdoc_records_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mobi");
    let output = temporary.path().join("mobi-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Mobi);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("MOBI Preview"));
    assert!(svg.contains("PalmDOC text from a bounded record"));

    let extensionless = temporary.path().join("ebook-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Mobi
    );
    let azw = temporary.path().join("book.azw");
    fs::copy(&input, &azw).unwrap();
    assert_eq!(SourceFormat::detect(&azw).unwrap(), SourceFormat::Mobi);

    let compressed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_mobi_compressed.mobi");
    let compressed_report = convert_path(
        &compressed,
        temporary.path().join("mobi-compressed-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(compressed_report.source_format, SourceFormat::Mobi);
    assert_eq!(compressed_report.page_count, 1);
    let compressed_svg =
        fs::read_to_string(temporary.path().join("mobi-compressed-out/page-0001.svg")).unwrap();
    assert!(compressed_svg.contains("Compressed MOBI"));
    assert!(compressed_svg.contains("PalmDOC LZ77 record"));
}

#[test]
fn converts_arff_dense_and_sparse_records_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let source = "% ARFF preview\n@relation weather\n@attribute outlook {sunny,overcast,rainy}\n@attribute temperature numeric\n@attribute note string\n@data\nsunny,25,'windy, warm'\n{1 18, 2 clear}\n";
    let input = temporary.path().join("weather.arff");
    fs::write(&input, source).unwrap();
    let output = temporary.path().join("arff-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Arff);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ARFF"));
    assert!(svg.contains("windy, warm"));
    assert!(svg.contains("18"));

    let extensionless = temporary.path().join("weather-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Arff
    );
}

#[test]
fn converts_jsonld_nodes_and_named_graphs_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonld");
    let output = temporary.path().join("jsonld-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::JsonLd);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("@context"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("urn:book:1"));
    assert!(svg.contains("A bounded JSON-LD title"));
    assert!(svg.contains("urn:graph:1"));

    let extensionless = temporary.path().join("linked-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::JsonLd
    );
    let generic_extension = temporary.path().join("linked-data.json");
    fs::copy(&input, &generic_extension).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_extension).unwrap(),
        SourceFormat::JsonLd
    );
    let dashed_extension = temporary.path().join("linked-data.json-ld");
    fs::copy(&input, &dashed_extension).unwrap();
    assert_eq!(
        SourceFormat::detect(&dashed_extension).unwrap(),
        SourceFormat::JsonLd
    );
}

#[test]
fn converts_graphml_nodes_and_edges_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.graphml");
    let output = temporary.path().join("graphml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Graphml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Start order"));
    assert!(svg.contains("approve"));
    assert!(svg.contains("GraphML"));

    let extensionless = temporary.path().join("workflow-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Graphml
    );
}

#[test]
fn converts_gexf_nodes_and_edges_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gexf");
    let output = temporary.path().join("gexf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gexf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Start order"));
    assert!(svg.contains("approve"));
    assert!(svg.contains("GEXF"));

    let extensionless = temporary.path().join("gephi-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Gexf
    );
}

#[test]
fn converts_netcdf_classic_and_cdf2_arrays_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_netcdf.nc");
    let output = temporary.path().join("netcdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Netcdf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("NetCDF CDF-1"));
    assert!(svg.contains("NetCDF demo"));
    assert!(svg.contains("20"));

    let cdf2 = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_netcdf_cdf2.nc");
    let cdf2_report = convert_path(
        &cdf2,
        temporary.path().join("netcdf-cdf2-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(cdf2_report.source_format, SourceFormat::Netcdf);
    let cdf2_svg =
        fs::read_to_string(temporary.path().join("netcdf-cdf2-out/page-0001.svg")).unwrap();
    assert!(cdf2_svg.contains("NetCDF CDF-2"));
    assert!(cdf2_svg.contains("CDF-2 demo"));
    let extensionless = temporary.path().join("netcdf-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Netcdf
    );
    let nc_text = temporary.path().join("toolpath.nc");
    fs::write(&nc_text, "G21\nG1 X1 Y1\n").unwrap();
    assert_eq!(SourceFormat::detect(&nc_text).unwrap(), SourceFormat::Gcode);
    let cdf5 = temporary.path().join("unsupported.nc");
    fs::write(&cdf5, b"CDF\x05").unwrap();
    assert_eq!(SourceFormat::detect(&cdf5).unwrap(), SourceFormat::Netcdf);
    let error = convert_path(
        &cdf5,
        temporary.path().join("unsupported-out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("CDF-5"));
}

#[test]
fn converts_xgmml_nodes_and_edges_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xgmml");
    let output = temporary.path().join("xgmml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xgmml);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("att"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Start order"));
    assert!(svg.contains("approve"));
    assert!(svg.contains("XGMML"));
    let extensionless = temporary.path().join("cytoscape-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xgmml
    );
}

#[test]
fn converts_graph_gml_and_keeps_geographic_gml_detection_separate() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_graph.gml");
    let output = temporary.path().join("graph-gml-out");
    assert_eq!(
        SourceFormat::detect(&input).unwrap(),
        SourceFormat::GraphGml
    );
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::GraphGml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Start order"));
    assert!(svg.contains("approve"));
    assert!(svg.contains("Graph GML"));

    let extensionless = temporary.path().join("graph-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::GraphGml
    );
    let geo = temporary.path().join("geo.gml");
    fs::write(
        &geo,
        "<gml:FeatureCollection xmlns:gml=\"http://www.opengis.net/gml\"><gml:featureMember><gml:Point><gml:coordinates>1,2</gml:coordinates></gml:Point></gml:featureMember></gml:FeatureCollection>",
    )
    .unwrap();
    assert_eq!(SourceFormat::detect(&geo).unwrap(), SourceFormat::Gml);
}

#[test]
fn converts_standalone_jpeg2000_jp2_and_raw_codestreams() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (filename, expected_text) in [
        ("sample_jpeg2000.jp2", "JPEG 2000 image"),
        ("sample_jpeg2000.j2k", "JPEG 2000 image"),
        ("sample_jpeg2000_rgba.jp2", "JPEG 2000 image"),
    ] {
        let input = fixture_dir.join(filename);
        let output = temporary.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, SourceFormat::Jpeg2000);
        assert_eq!(report.page_count, 1);
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        assert!(svg.contains(expected_text));
        assert!(svg.contains("data:image/png;base64,"));
    }
    let extensionless = temporary.path().join("jpx-data");
    fs::copy(fixture_dir.join("sample_jpeg2000.jp2"), &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Jpeg2000
    );
    let jpc = temporary.path().join("image.jpc");
    fs::copy(fixture_dir.join("sample_jpeg2000.j2k"), &jpc).unwrap();
    assert_eq!(SourceFormat::detect(&jpc).unwrap(), SourceFormat::Jpeg2000);
}

#[test]
fn converts_tecplot_ascii_finite_element_zones_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_tecplot.dat");
    let output = temporary.path().join("tecplot-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Tecplot);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Tecplot finite-element sample"));
    assert!(svg.contains("simulation:mesh"));

    let extensionless = temporary.path().join("mesh-preview");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Tecplot
    );
}

#[test]
fn converts_tecplot_block_and_ordered_zones() {
    let temporary = TempDir::new().unwrap();
    let block = temporary.path().join("block.tec");
    fs::write(
        &block,
        "TITLE=\"Block\"\nVARIABLES=\"X\" \"Y\" \"Value\"\nZONE N=4 E=2 F=BLOCK ET=TRIANGLE\n0 1 1 0  0 0 1 1  1 2 3 4  1 2 3  1 3 4\n",
    )
    .unwrap();
    let block_report = convert_path(
        &block,
        temporary.path().join("block-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(block_report.source_format, SourceFormat::Tecplot);
    assert_eq!(block_report.page_count, 1);

    let ordered = temporary.path().join("ordered.dat");
    fs::write(
        &ordered,
        "VARIABLES=\"X\" \"Y\" \"Value\"\nZONE I=3, J=2, F=POINT\n0 0 1\n1 0 2\n2 0 3\n0 1 4\n1 1 5\n2 1 6\n",
    )
    .unwrap();
    let ordered_report = convert_path(
        &ordered,
        temporary.path().join("ordered-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(ordered_report.source_format, SourceFormat::Tecplot);
    assert_eq!(ordered_report.page_count, 1);
}

#[test]
fn converts_ensight_gold_case_and_geometry_with_safe_sidecar_resolution() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let case = fixture_dir.join("sample_ensight.case");
    let output = temporary.path().join("ensight-out");
    let report = convert_path(&case, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ensight);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("variable files"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("EnSight: EnSight Gold ASCII sample"));
    assert!(svg.contains("simulation:mesh"));

    let geometry = fixture_dir.join("sample_ensight.geo");
    assert_eq!(
        SourceFormat::detect(&geometry).unwrap(),
        SourceFormat::Ensight
    );
    let extensionless = temporary.path().join("ensight-geometry");
    fs::copy(&geometry, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ensight
    );

    let unsafe_case = temporary.path().join("unsafe.case");
    fs::write(
        &unsafe_case,
        "FORMAT\ntype: ensight gold\nGEOMETRY\nmodel: ../outside.geo\n",
    )
    .unwrap();
    let error = convert_path(
        &unsafe_case,
        temporary.path().join("unsafe-out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("inside the case directory"));
}

#[test]
fn converts_plot3d_ascii_single_and_multiblock_grids() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_plot3d.p3d");
    let output = temporary.path().join("plot3d-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Plot3d);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("PLOT3D structured grid"));
    assert!(svg.contains("simulation:mesh"));

    let multi = temporary.path().join("multi.plot3d");
    fs::write(
        &multi,
        "2\n2 2 1\n2 2 1\n0 1 0 1  0 1 0 1  0 1 0 1  0 0 1 1\n",
    )
    .unwrap();
    let multi_report = convert_path(
        &multi,
        temporary.path().join("multi-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(multi_report.source_format, SourceFormat::Plot3d);
    assert_eq!(multi_report.page_count, 1);

    let extensionless = temporary.path().join("structured-grid");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Plot3d
    );
}

#[test]
fn converts_vrml97_indexed_faces_and_lines_without_executing_routes() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wrl");
    let output = temporary.path().join("vrml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Vrml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Wavefront OBJ 3D Model"));
    assert!(svg.contains("<path"));
    assert!(!svg.contains("<script"));
}

#[test]
fn converts_netpbm_pbm_pgm_ppm_and_pam_rasters_with_content_sniffing() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for filename in [
        "sample_rgb.ppm",
        "sample_gray16.pgm",
        "sample_alpha.pam",
        "sample_bitmap.pbm",
    ] {
        let input = fixture_dir.join(filename);
        let output = temporary.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, SourceFormat::Raster);
        assert_eq!(report.page_count, 1);
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        assert!(svg.contains("<path"));
        let extensionless = temporary.path().join(format!("{filename}.data"));
        fs::copy(&input, &extensionless).unwrap();
        assert_eq!(
            SourceFormat::detect(&extensionless).unwrap(),
            SourceFormat::Raster
        );
    }
}

#[test]
fn converts_sylk_cells_and_preserves_formulas_as_inert_text() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.slk");
    let output = temporary.path().join("sylk-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Sylk);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("formulas"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SYLK spreadsheet"));
    assert!(svg.contains("Widget"));
    assert!(svg.contains("[formula: 1C2+R2C2]"));
    let extensionless = temporary.path().join("sylk-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Sylk
    );
}

#[test]
fn converts_dif_spreadsheet_tuples_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.dif");
    let output = temporary.path().join("dif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dif);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("DIF spreadsheet"));
    assert!(svg.contains("Widget"));
    assert!(svg.contains("42"));
    let extensionless = temporary.path().join("dif-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Dif
    );
}

#[test]
fn converts_fasta_and_fastq_sequence_records_as_inert_tables() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let fasta = fixture_dir.join("sample.fasta");
    let fasta_report = convert_path(
        &fasta,
        temporary.path().join("fasta-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(fasta_report.source_format, SourceFormat::Fasta);
    assert_eq!(fasta_report.page_count, 1);
    let fasta_svg = fs::read_to_string(temporary.path().join("fasta-out/page-0001.svg")).unwrap();
    assert!(fasta_svg.contains("FASTA sequence records"));
    assert!(fasta_svg.contains("seq1"));
    assert!(fasta_svg.contains("GC"));

    let fastq = fixture_dir.join("sample.fastq");
    let fastq_report = convert_path(
        &fastq,
        temporary.path().join("fastq-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(fastq_report.source_format, SourceFormat::Fastq);
    assert_eq!(fastq_report.page_count, 1);
    let fastq_svg = fs::read_to_string(temporary.path().join("fastq-out/page-0001.svg")).unwrap();
    assert!(fastq_svg.contains("FASTQ sequence records"));
    assert!(fastq_svg.contains("Quality ASCII range"));

    let extensionless = temporary.path().join("sequence-data");
    fs::copy(&fastq, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Fastq
    );
}

#[test]
fn converts_gff3_and_gtf_feature_annotations_without_loading_embedded_sequence() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let gff = fixture_dir.join("sample.gff3");
    let gff_report = convert_path(
        &gff,
        temporary.path().join("gff3-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(gff_report.source_format, SourceFormat::Gff3);
    assert_eq!(gff_report.page_count, 1);
    assert!(
        gff_report
            .warnings
            .iter()
            .any(|warning| warning.contains("FASTA"))
    );
    let gff_svg = fs::read_to_string(temporary.path().join("gff3-out/page-0001.svg")).unwrap();
    assert!(gff_svg.contains("GFF3 feature annotations"));
    assert!(gff_svg.contains("gene00001"));
    assert!(gff_svg.contains("Example%20gene"));

    let gtf = fixture_dir.join("sample.gtf");
    let gtf_report = convert_path(
        &gtf,
        temporary.path().join("gtf-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(gtf_report.source_format, SourceFormat::Gtf);
    assert_eq!(gtf_report.page_count, 1);
    let gtf_svg = fs::read_to_string(temporary.path().join("gtf-out/page-0001.svg")).unwrap();
    assert!(gtf_svg.contains("GTF feature annotations"));
    let extensionless = temporary.path().join("annotation-data");
    fs::copy(&gff, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Gff3
    );
}

#[test]
fn converts_bed_and_bedgraph_intervals_with_zero_based_validation() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let bed = fixture_dir.join("sample.bed");
    let bed_report = convert_path(
        &bed,
        temporary.path().join("bed-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(bed_report.source_format, SourceFormat::Bed);
    assert_eq!(bed_report.page_count, 1);
    assert!(
        bed_report
            .warnings
            .iter()
            .any(|warning| warning.contains("directives"))
    );
    let bed_svg = fs::read_to_string(temporary.path().join("bed-out/page-0001.svg")).unwrap();
    assert!(bed_svg.contains("BED interval annotations"));
    assert!(bed_svg.contains("gene1"));

    let graph = fixture_dir.join("sample.bedgraph");
    let graph_report = convert_path(
        &graph,
        temporary.path().join("bedgraph-out"),
        &ConvertOptions::default(),
    )
    .unwrap();
    assert_eq!(graph_report.source_format, SourceFormat::BedGraph);
    assert_eq!(graph_report.page_count, 1);
    let graph_svg =
        fs::read_to_string(temporary.path().join("bedgraph-out/page-0001.svg")).unwrap();
    assert!(graph_svg.contains("BEDGRAPH interval annotations"));
    let extensionless = temporary.path().join("bed-data");
    fs::copy(&bed, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Bed
    );
}

#[test]
fn converts_vcf_variants_and_keeps_vcard_extension_disambiguated() {
    let temporary = TempDir::new().unwrap();
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let input = fixture_dir.join("sample_variants.vcf");
    let output = temporary.path().join("vcf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Vcf);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("reference URLs"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("VCF variant annotations"));
    assert!(svg.contains("rs-demo-1"));
    assert!(svg.contains("SAMPLE_A"));

    let vcard = fixture_dir.join("contact.vcf");
    assert_eq!(SourceFormat::detect(&vcard).unwrap(), SourceFormat::Vcard);
    let extensionless = temporary.path().join("variant-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Vcf
    );
}

#[test]
fn converts_sam_alignment_records_without_reference_or_tag_evaluation() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sam");
    let output = temporary.path().join("sam-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Sam);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SAM alignment records"));
    assert!(svg.contains("r001"));
    assert!(svg.contains("NM:i:1"));
    let extensionless = temporary.path().join("alignment-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Sam
    );
}

#[test]
fn converts_wig_fixed_and_variable_step_signals() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wig");
    let output = temporary.path().join("wig-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Wig);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("directives"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("WIG continuous signal"));
    assert!(svg.contains("chr2"));
    let extensionless = temporary.path().join("wiggle-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Wig
    );
}

#[test]
fn converts_maf_multiple_alignment_blocks_and_keeps_optional_rows_inert() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.maf");
    let output = temporary.path().join("maf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Maf);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("optional"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("MAF multiple alignments"));
    assert!(svg.contains("hg38.chr1"));
    let extensionless = temporary.path().join("alignment-blocks");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Maf
    );
}

#[test]
fn converts_newick_phylogenetic_tree_and_sniffs_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nwk");
    let output = temporary.path().join("newick-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Newick);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Newick phylogenetic tree"));
    assert!(svg.contains("Homo_sapiens"));
    assert!(svg.contains("Pan troglodytes"));
    assert!(svg.contains("0.1"));
    let extensionless = temporary.path().join("tree-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Newick
    );

    let unsafe_tree = temporary.path().join("unsafe.tree");
    fs::write(&unsafe_tree, "('<script>alert(1)</script>':1)root;").unwrap();
    let unsafe_output = temporary.path().join("unsafe-out");
    convert_path(&unsafe_tree, &unsafe_output, &ConvertOptions::default()).unwrap();
    let unsafe_svg = fs::read_to_string(unsafe_output.join("page-0001.svg")).unwrap();
    assert!(unsafe_svg.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!unsafe_svg.contains("<script>"));
}

#[test]
fn converts_stockholm_multiple_alignments_and_split_sequence_rows() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sto");
    let output = temporary.path().join("stockholm-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Stockholm);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("annotation"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Stockholm multiple alignments"));
    assert!(svg.contains("seq1"));
    assert!(svg.contains("AC-GTT.."));
    assert!(svg.contains("seqA"));
    let extensionless = temporary.path().join("alignment-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Stockholm
    );
}

#[test]
fn converts_clustal_block_alignment_and_ignores_consensus_rows() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.aln");
    let output = temporary.path().join("clustal-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Clustal);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("consensus"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("CLUSTAL multiple alignments"));
    assert!(svg.contains("seq1"));
    assert!(svg.contains("AC-GTT.."));
    assert!(svg.contains("seq2"));
    let extensionless = temporary.path().join("clustal-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Clustal
    );
}

#[test]
fn converts_nexus_tree_block_and_keeps_translate_map_inert() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nex");
    let output = temporary.path().join("nexus-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Nexus);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("TRANSLATE"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("NEXUS phylogenetic tree"));
    assert!(svg.contains("Mammals"));
    assert!(svg.contains("0.1"));
    let extensionless = temporary.path().join("nexus-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Nexus
    );
}

#[test]
fn converts_genbank_records_with_features_and_origin_preview() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gb");
    let output = temporary.path().join("genbank-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Genbank);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("GenBank records"));
    assert!(svg.contains("DEMO0001"));
    assert!(svg.contains("Demonstration GenBank record"));
    assert!(svg.contains("atgcgtaaccgg"));
    assert!(svg.contains("DEMO0002"));
    let extensionless = temporary.path().join("genbank-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Genbank
    );
}

#[test]
fn converts_embl_records_with_fixed_tags_and_sq_sequence() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.embl");
    let output = temporary.path().join("embl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Embl);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("EMBL-Bank records"));
    assert!(svg.contains("DEMO0001"));
    assert!(svg.contains("Description continuation"));
    assert!(svg.contains("atgcgtaaccgg"));
    assert!(svg.contains("DEMO0002"));
    let extensionless = temporary.path().join("embl-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Embl
    );
}

#[test]
fn converts_uniprot_flat_records_and_disambiguates_dat_from_tecplot() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_uniprot.dat");
    let output = temporary.path().join("uniprot-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Uniprot);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("cross-reference"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("UniProtKB protein records"));
    assert!(svg.contains("DEMO_HUMAN"));
    assert!(svg.contains("Demonstration protein"));
    assert!(svg.contains("MKTAYIAKQRQG"));
    assert!(svg.contains("DEMO_TRMBL"));
    let extensionless = temporary.path().join("uniprot-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Uniprot
    );
}

#[test]
fn converts_ris_bibliography_records_and_keeps_links_inert() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ris");
    let output = temporary.path().join("ris-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ris);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("unknown RIS"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("RIS bibliography"));
    assert!(svg.contains("Safe document conversion"));
    assert!(svg.contains("Doe, Jane"));
    assert!(svg.contains("https://example.invalid/paper/1"));
    assert!(!svg.contains("<script>"));
    let extensionless = temporary.path().join("citation-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ris
    );
}

#[test]
fn converts_spice_netlist_cards_without_executing_simulation_or_includes() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cir");
    let output = temporary.path().join("spice-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Spice);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("include"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SPICE netlist"));
    assert!(svg.contains("R1"));
    assert!(svg.contains("in out"));
    assert!(!svg.contains("external-model.lib"));
    let extensionless = temporary.path().join("circuit-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Spice
    );
}

#[test]
fn converts_legacy_kicad_schematic_components_wires_and_labels() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_legacy.sch");
    let output = temporary.path().join("kicad-sch-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::KicadSchLegacy);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("KiCad legacy schematic"));
    assert!(svg.contains("R1"));
    assert!(svg.contains("1k"));
    assert!(svg.contains("FILTERED_OUT"));
    assert!(svg.contains("Legacy Eeschema preview"));
    let extensionless = temporary.path().join("legacy-schematic");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::KicadSchLegacy
    );
}

#[test]
fn converts_modern_kicad_schematic_sexpressions() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.kicad_sch");
    let output = temporary.path().join("kicad-modern-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::KicadSch);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("embedded library"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("KiCad schematic"));
    assert!(svg.contains("R1"));
    assert!(svg.contains("1k"));
    assert!(svg.contains("FILTERED_OUT"));
    assert!(svg.contains("Modern KiCad schematic"));
    let extensionless = temporary.path().join("kicad-modern-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::KicadSch
    );
}

#[test]
fn converts_ltspice_ascii_schematic_and_disambiguates_esri_asc() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_ltspice.asc");
    let output = temporary.path().join("ltspice-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::LtspiceAsc);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("LTspice schematic"));
    assert!(svg.contains("R1"));
    assert!(svg.contains("1k"));
    assert!(svg.contains("IN"));
    assert!(svg.contains(".tran"));
    let extensionless = temporary.path().join("ltspice-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::LtspiceAsc
    );
}

#[test]
fn converts_eagle_xml_schematic_with_parts_wires_and_labels() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_eagle.sch");
    let output = temporary.path().join("eagle-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::EagleSch);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("EAGLE schematic"));
    assert!(svg.contains("R1"));
    assert!(svg.contains("1k"));
    assert!(svg.contains("EAGLE schematic preview"));
    assert!(svg.contains("OUT"));
    let extensionless = temporary.path().join("eagle-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::EagleSch
    );
}

#[test]
fn converts_openapi_json_and_yaml_without_fetching_refs_or_servers() {
    let temporary = TempDir::new().unwrap();
    let json = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.openapi.json");
    let output = temporary.path().join("openapi-json-out");
    let report = convert_path(&json, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Openapi);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never fetched"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("not resolved"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OpenAPI description"));
    assert!(svg.contains("Catalog API"));
    assert!(svg.contains("listPets"));
    assert!(svg.contains("https://api.example.invalid/v1"));
    assert!(!svg.contains("<script>"));
    let extensionless = temporary.path().join("openapi-data");
    fs::copy(&json, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Openapi
    );

    let yaml = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.openapi.yaml");
    let yaml_output = temporary.path().join("openapi-yaml-out");
    let yaml_report = convert_path(&yaml, &yaml_output, &ConvertOptions::default()).unwrap();
    assert_eq!(yaml_report.source_format, SourceFormat::Openapi);
    let yaml_svg = fs::read_to_string(yaml_output.join("page-0001.svg")).unwrap();
    assert!(yaml_svg.contains("Catalog YAML API"));
    assert!(yaml_svg.contains("deletePet"));
    let yaml_extensionless = temporary.path().join("openapi-yaml-data");
    fs::copy(&yaml, &yaml_extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&yaml_extensionless).unwrap(),
        SourceFormat::Openapi
    );
}

#[test]
fn converts_asyncapi_json_and_yaml_without_fetching_servers_or_refs() {
    let temporary = TempDir::new().unwrap();
    let json = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.asyncapi.json");
    let output = temporary.path().join("asyncapi-json-out");
    let report = convert_path(&json, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Asyncapi);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never fetched"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("not resolved"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("AsyncAPI description"));
    assert!(svg.contains("Event Catalog"));
    assert!(svg.contains("onUserSignedUp"));
    assert!(svg.contains("user/signedup"));
    assert!(!svg.contains("<script>"));
    let extensionless = temporary.path().join("asyncapi-data");
    fs::copy(&json, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Asyncapi
    );

    let yaml = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.asyncapi.yaml");
    let yaml_output = temporary.path().join("asyncapi-yaml-out");
    let yaml_report = convert_path(&yaml, &yaml_output, &ConvertOptions::default()).unwrap();
    assert_eq!(yaml_report.source_format, SourceFormat::Asyncapi);
    let yaml_svg = fs::read_to_string(yaml_output.join("page-0001.svg")).unwrap();
    assert!(yaml_svg.contains("Event Catalog YAML"));
    assert!(yaml_svg.contains("onUserSigned"));
    assert!(yaml_svg.contains("publishInvoice"));
}

#[test]
fn converts_json_schema_json_and_yaml_without_resolving_refs_or_patterns() {
    let temporary = TempDir::new().unwrap();
    let json = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.schema.json");
    let output = temporary.path().join("schema-json-out");
    let report = convert_path(&json, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::JsonSchema);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("not resolved"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("JSON Schema"));
    assert!(svg.contains("Catalog record"));
    assert!(svg.contains("$.name"));
    assert!(svg.contains("minLength"));
    assert!(!svg.contains("<script>"));
    let extensionless = temporary.path().join("schema-data");
    fs::copy(&json, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::JsonSchema
    );

    let yaml = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.schema.yaml");
    let yaml_output = temporary.path().join("schema-yaml-out");
    let yaml_report = convert_path(&yaml, &yaml_output, &ConvertOptions::default()).unwrap();
    assert_eq!(yaml_report.source_format, SourceFormat::JsonSchema);
    let yaml_svg = fs::read_to_string(yaml_output.join("page-0001.svg")).unwrap();
    assert!(yaml_svg.contains("Catalog YAML schema"));
    assert!(yaml_svg.contains("$.enabled"));
}

#[test]
fn converts_ansys_cdb_nblock_eblock_mesh_without_executing_apdl() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cdb");
    let output = temporary.path().join("cdb-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Cdb);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("NBLOCK/EBLOCK"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("no command"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ANSYS CDB mesh"));
    assert!(svg.contains("sim-background"));
    let extensionless = temporary.path().join("ansys-mesh-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Cdb
    );
}

#[test]
fn converts_har_entries_with_sensitive_query_masking_and_body_omission() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.har");
    let output = temporary.path().join("har-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Har);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never fetched"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("masked"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("body data omitted"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("HAR network archive"));
    assert!(svg.contains("200"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private"));
    let extensionless = temporary.path().join("browser-log");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Har
    );
}

#[test]
fn converts_warc_records_and_bounded_gzip_without_opening_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.warc");
    let output = temporary.path().join("warc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Warc);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("payload record"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("masked"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("WARC web archive"));
    assert!(svg.contains("200"));
    assert!(svg.contains("token=***") || svg.contains("example.invalid"));
    assert!(!svg.contains("private body"));
    let gzip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.warc.gz");
    let gzip_output = temporary.path().join("warc-gzip-out");
    let gzip_report = convert_path(&gzip, &gzip_output, &ConvertOptions::default()).unwrap();
    assert_eq!(gzip_report.source_format, SourceFormat::Warc);
    assert_eq!(gzip_report.page_count, 1);
    let extensionless = temporary.path().join("web-archive-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Warc
    );
}

#[test]
fn converts_wacz_manifest_and_pages_without_replaying_archive_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wacz");
    let output = temporary.path().join("wacz-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Wacz);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("payloads"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("masked"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("WACZ web archive"));
    assert!(svg.contains("Demo WACZ collection"));
    assert!(svg.contains("Ho"));
    assert!(!svg.contains("<script>"));
    let extensionless = temporary.path().join("archive-package");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Wacz
    );
}

#[test]
fn converts_postman_collection_without_executing_auth_scripts_or_bodies() {
    let temporary = TempDir::new().unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.postman_collection.json");
    let output = temporary.path().join("postman-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Postman);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never") && warning.contains("executed"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("masked"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("script"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("body"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Postman collection"));
    assert!(svg.contains("Catalog API collection"));
    assert!(svg.contains("GET"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private response"));
    let extensionless = temporary.path().join("postman-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Postman
    );
}

#[test]
fn converts_graphql_sdl_types_and_fields_without_executing_operations() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.graphql");
    let output = temporary.path().join("graphql-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Graphql);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("executed"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("GraphQL schema"));
    assert!(svg.contains("Query"));
    assert!(svg.contains("pets"));
    assert!(svg.contains("PetStatus"));
    assert!(!svg.contains("<script>"));
    let extensionless = temporary.path().join("graphql-schema");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Graphql
    );
}

#[test]
fn converts_protobuf_schema_without_executing_imports_or_rpc() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.proto");
    let output = temporary.path().join("protobuf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Protobuf);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("not opened"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("RPC call"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Protocol Buffers schema"));
    assert!(svg.contains("catalog.v1"));
    assert!(svg.contains("GetPet"));
    assert!(svg.contains("PET_STATUS"));
    assert!(!svg.contains("very-secret"));
    let extensionless = temporary.path().join("protobuf-schema");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Protobuf
    );
}

#[test]
fn converts_kubernetes_yaml_and_json_without_cluster_or_secret_access() {
    let temporary = TempDir::new().unwrap();
    let yaml = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.k8s.yaml");
    let output = temporary.path().join("k8s-yaml-out");
    let report = convert_path(&yaml, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Kubernetes);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("kubectl"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("Secret"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Kubernetes manifests"));
    assert!(svg.contains("Deployment/prod"));
    assert!(svg.contains("Secret/prod"));
    assert!(!svg.contains("very-secret"));
    let json = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.k8s.json");
    let json_output = temporary.path().join("k8s-json-out");
    let json_report = convert_path(&json, &json_output, &ConvertOptions::default()).unwrap();
    assert_eq!(json_report.source_format, SourceFormat::Kubernetes);
    assert_eq!(json_report.page_count, 1);
    let extensionless = temporary.path().join("k8s-manifest");
    fs::copy(&yaml, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Kubernetes
    );
}

#[test]
fn converts_compose_yaml_and_json_without_runtime_or_secret_access() {
    let temporary = TempDir::new().unwrap();
    let yaml = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.compose.yaml");
    let output = temporary.path().join("compose-yaml-out");
    let report = convert_path(&yaml, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Compose);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("daemon"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("secret"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Docker Compose"));
    assert!(svg.contains("web"));
    assert!(svg.contains("db"));
    assert!(svg.contains("nginx:1.27"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("super-secret-value"));

    let json = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.compose.json");
    let json_output = temporary.path().join("compose-json-out");
    let json_report = convert_path(&json, &json_output, &ConvertOptions::default()).unwrap();
    assert_eq!(json_report.source_format, SourceFormat::Compose);
    assert_eq!(json_report.page_count, 1);
    let extensionless = temporary.path().join("compose-data");
    fs::copy(&yaml, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Compose
    );
}

#[test]
fn converts_github_actions_workflow_without_executing_steps() {
    let temporary = TempDir::new().unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.github.workflow.yml");
    let output = temporary.path().join("workflow-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format.to_string(), "GITHUB-ACTIONS");
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("GitHub Actions workflow"));
    assert!(svg.contains("lint"));
    assert!(svg.contains("ubuntu-latest"));
    assert!(svg.contains("A:2"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("super-secret"));
    let workflow_dir = temporary.path().join(".github").join("workflows");
    fs::create_dir_all(&workflow_dir).unwrap();
    let named = workflow_dir.join("ci.yml");
    fs::copy(&input, &named).unwrap();
    assert_eq!(
        SourceFormat::detect(&named).unwrap().to_string(),
        "GITHUB-ACTIONS"
    );
    let extensionless = temporary.path().join("workflow-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap().to_string(),
        "GITHUB-ACTIONS"
    );
}

#[test]
fn converts_junit_xml_without_executing_tests_or_exposing_logs() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.junit.xml");
    let output = temporary.path().join("junit-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Junit);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("failing"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("JUnit test report"));
    assert!(svg.contains("converter"));
    assert!(svg.contains("bindings"));
    assert!(svg.contains("Failures"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private traceback"));
    let extensionless = temporary.path().join("test-results");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Junit
    );
}

#[test]
fn converts_sarif_results_without_exposing_alert_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sarif");
    let output = temporary.path().join("sarif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Sarif);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SARIF static analysis"));
    assert!(svg.contains("Demo Scanner"));
    assert!(svg.contains("SEC001"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("file:///private"));
    let extensionless = temporary.path().join("scan-results");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Sarif
    );
}

#[test]
fn converts_terraform_plan_json_without_exposing_values_or_running_providers() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.tfplan.json");
    let output = temporary.path().join("terraform-plan-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::TerraformPlan);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Terraform plan"));
    assert!(svg.contains("aws_instance"));
    assert!(svg.contains("delete/c"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private source"));
    let extensionless = temporary.path().join("tfplan-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::TerraformPlan
    );
}

#[test]
fn converts_cyclonedx_json_and_xml_without_exposing_sbom_payloads() {
    let temporary = TempDir::new().unwrap();
    let json = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cdx.json");
    let json_output = temporary.path().join("cdx-json-out");
    let report = convert_path(&json, &json_output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::CycloneDx);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("omitted"))
    );
    let svg = fs::read_to_string(json_output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("CycloneDX BOM"));
    assert!(svg.contains("serde"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private-bom"));
    let xml = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cdx.xml");
    let xml_output = temporary.path().join("cdx-xml-out");
    let xml_report = convert_path(&xml, &xml_output, &ConvertOptions::default()).unwrap();
    assert_eq!(xml_report.source_format, SourceFormat::CycloneDx);
    assert_eq!(xml_report.page_count, 1);
    let extensionless = temporary.path().join("software-bom");
    fs::copy(&json, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::CycloneDx
    );
}

#[test]
fn converts_spdx_json_without_exposing_compliance_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.spdx.json");
    let output = temporary.path().join("spdx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Spdx);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SPDX document"));
    assert!(svg.contains("serde"));
    assert!(svg.contains("src/lib.rs"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private.example"));
    let extensionless = temporary.path().join("software-bom");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Spdx
    );
    let tag = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.spdx");
    let tag_output = temporary.path().join("spdx-tag-out");
    let tag_report = convert_path(&tag, &tag_output, &ConvertOptions::default()).unwrap();
    assert_eq!(tag_report.source_format, SourceFormat::Spdx);
    assert_eq!(tag_report.page_count, 1);
    let tag_svg = fs::read_to_string(tag_output.join("page-0001.svg")).unwrap();
    assert!(tag_svg.contains("Preview tag-value SBOM"));
    assert!(tag_svg.contains("serde"));
    assert!(!tag_svg.contains("very-secret"));
}

#[test]
fn converts_jacoco_and_cobertura_coverage_without_source_payloads() {
    let temporary = TempDir::new().unwrap();
    let jacoco = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jacoco.xml");
    let jacoco_output = temporary.path().join("jacoco-out");
    let report = convert_path(&jacoco, &jacoco_output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Coverage);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never"))
    );
    let svg = fs::read_to_string(jacoco_output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Coverage report"));
    assert!(svg.contains("com/example/app"));
    assert!(svg.contains("5/2"));
    assert!(!svg.contains("private-source"));
    assert!(!svg.contains("private-session"));
    let cobertura =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cobertura.xml");
    let cobertura_output = temporary.path().join("cobertura-out");
    let cobertura_report =
        convert_path(&cobertura, &cobertura_output, &ConvertOptions::default()).unwrap();
    assert_eq!(cobertura_report.source_format, SourceFormat::Coverage);
    assert_eq!(cobertura_report.page_count, 1);
    let extensionless = temporary.path().join("coverage-data");
    fs::copy(&jacoco, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Coverage
    );
}

#[test]
fn converts_lcov_tracefile_without_exposing_source_paths_or_execution_records() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.lcov.info");
    let output = temporary.path().join("lcov-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Lcov);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("LCOV coverage"));
    assert!(svg.contains("main.rs"));
    assert!(svg.contains("lib.rs"));
    assert!(!svg.contains("/private/workspace"));
    assert!(!svg.contains("FNDA"));
    let extensionless = temporary.path().join("trace-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Lcov
    );
}

#[test]
fn converts_json_patch_without_applying_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonpatch");
    let output = temporary.path().join("jsonpatch-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::JsonPatch);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("never"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("JSON Patch"));
    assert!(svg.contains("replace"));
    assert!(svg.contains("/metadata/owner"));
    assert!(svg.contains("string"));
    assert!(!svg.contains("very-secret"));
    let extensionless = temporary.path().join("patch-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::JsonPatch
    );
}

#[test]
fn converts_json_merge_patch_without_applying_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mergepatch");
    let output = temporary.path().join("mergepatch-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::JsonMergePatch);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("JSON Merge Patch"));
    assert!(svg.contains("delete"));
    assert!(svg.contains("merge"));
    assert!(!svg.contains("very-secret"));
}

#[test]
fn converts_openfoam_scalar_and_vector_fields_without_solver_access() {
    let temporary = TempDir::new().unwrap();
    let scalar = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.foamfield");
    let output = temporary.path().join("foam-field-out");
    let report = convert_path(&scalar, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format.to_string(), "OPENFOAM-FIELD");
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("solver"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OpenFOAM field"));
    assert!(svg.contains("volScalarField"));
    assert!(svg.contains("101250"));
    assert!(!svg.contains("fixedValue"));
    let vector =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.foamvectorfield");
    let vector_output = temporary.path().join("foam-vector-out");
    let vector_report = convert_path(&vector, &vector_output, &ConvertOptions::default()).unwrap();
    assert_eq!(vector_report.source_format.to_string(), "OPENFOAM-FIELD");
    let extensionless = temporary.path().join("field-data");
    fs::copy(&scalar, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap().to_string(),
        "OPENFOAM-FIELD"
    );
}

#[test]
fn converts_csl_json_without_style_or_network_access() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.csl.json");
    let output = temporary.path().join("csl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format.to_string(), "CSL-JSON");
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("CSL-JSON bibliography"));
    assert!(svg.contains("reproducible"));
    assert!(svg.contains("Smith"));
    assert!(svg.contains("2024"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private abstract"));
    let extensionless = temporary.path().join("citations-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap().to_string(),
        "CSL-JSON"
    );
}

#[test]
fn converts_json_feed_without_fetching_content_or_links() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonfeed");
    let output = temporary.path().join("jsonfeed-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::JsonFeed);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("JSON Feed"));
    assert!(svg.contains("A safe update"));
    assert!(svg.contains("html"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private.example"));
    let extensionless = temporary.path().join("feed-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::JsonFeed
    );
}

#[test]
fn converts_cloud_events_without_displaying_payloads_or_following_uris() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cloudevent.json");
    let output = temporary.path().join("cloudevents-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::CloudEvents);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("CloudEvents JSON"));
    assert!(svg.contains("com.exa"));
    assert!(svg.contains("evt…"));
    assert!(svg.contains("events.example"));
    assert!(svg.contains("With data: 1"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private customer"));
    assert!(!svg.contains("schema.example.invalid"));
    let extensionless = temporary.path().join("event-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::CloudEvents
    );
    let generic_json = temporary.path().join("event.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::CloudEvents
    );
}

#[test]
fn converts_fhir_json_bundle_without_displaying_clinical_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fhir.json");
    let output = temporary.path().join("fhir-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::FhirJson);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("FHIR JSON"));
    assert!(svg.contains("Bundle type: collection"));
    assert!(svg.contains("Patient"));
    assert!(svg.contains("Observa"));
    assert!(!svg.contains("private patient"));
    assert!(!svg.contains("SecretFamily"));
    assert!(!svg.contains("secret-identifier"));
    assert!(!svg.contains("secret display"));
    let extensionless = temporary.path().join("patient-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::FhirJson
    );
    let generic_json = temporary.path().join("patient.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::FhirJson
    );
}

#[test]
fn converts_avro_schema_without_executing_defaults_or_docs() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.avsc");
    let output = temporary.path().join("avro-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Avro);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Apache Avro schema"));
    assert!(svg.contains("Root schema: User"));
    assert!(svg.contains("fields/email"));
    assert!(!svg.contains("private documentation"));
    assert!(!svg.contains("secret field docs"));
    let extensionless = temporary.path().join("schema-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Avro
    );
    let generic_json = temporary.path().join("schema.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::Avro
    );
}

#[test]
fn converts_otlp_json_without_displaying_telemetry_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.otlp.json");
    let output = temporary.path().join("otlp-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OtlpJson);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OpenTelemetry OTLP JSON"));
    assert!(svg.contains("checko"));
    assert!(svg.contains("Spans: 1"));
    assert!(svg.contains("Metrics: 1"));
    assert!(svg.contains("Log records: 1"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("private-span"));
    let extensionless = temporary.path().join("telemetry-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OtlpJson
    );
    let generic_json = temporary.path().join("telemetry.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::OtlpJson
    );
}

#[test]
fn converts_ocel_json_without_displaying_attribute_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonocel");
    let output = temporary.path().join("ocel-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OcelJson);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OCEL 2.0 JSON"));
    assert!(svg.contains("Events: 2"));
    assert!(svg.contains("Objects: 2"));
    assert!(svg.contains("Event→object relationships: 3"));
    assert!(!svg.contains("private carrier"));
    assert!(!svg.contains("secret state"));
    let extensionless = temporary.path().join("process-log");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OcelJson
    );
    let generic_json = temporary.path().join("process-log.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::OcelJson
    );
}

#[test]
fn converts_json_api_compound_document_without_displaying_values_or_links() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonapi");
    let output = temporary.path().join("jsonapi-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::JsonApi);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("JSON:API 1.1"));
    assert!(svg.contains("Primary resources: 1"));
    assert!(svg.contains("Included resources: 1"));
    assert!(!svg.contains("private article"));
    assert!(!svg.contains("secret"));
    assert!(!svg.contains("api.example.invalid"));
    let extensionless = temporary.path().join("api-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::JsonApi
    );
    let generic_json = temporary.path().join("api.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::JsonApi
    );
}

#[test]
fn converts_opendrive_reference_lines_without_simulation_execution() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xodr");
    let output = temporary.path().join("opendrive-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OpenDrive);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-format=\"opendrive\""));
    assert!(svg.contains("OpenDRIVE contains 1 road(s), 1 junction(s)"));
    assert!(!svg.contains("sig-secret"));
    let extensionless = temporary.path().join("road-network");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OpenDrive
    );
}

#[test]
fn converts_openscenario_structure_without_catalog_or_simulation_access() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xosc");
    let output = temporary.path().join("openscenario-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OpenScenario);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ASAM OpenSCENARIO XML"));
    assert!(svg.contains("Entities: 2"));
    assert!(svg.contains("Stories: 1"));
    assert!(svg.contains("Catalog references: 2"));
    assert!(svg.contains("Ego"));
    assert!(!svg.contains("private scenario"));
    assert!(!svg.contains("private/catalog"));
    assert!(!svg.contains("secret-car"));
    let extensionless = temporary.path().join("scenario-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OpenScenario
    );
}

#[test]
fn converts_openlabel_annotations_without_sensor_payloads_or_urls() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.openlabel.json");
    let output = temporary.path().join("openlabel-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OpenLabel);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ASAM OpenLABEL JSON"));
    assert!(svg.contains("Schema version: 1.0.0"));
    assert!(svg.contains("Object data entries: 1"));
    assert!(svg.contains("Frame object-data entries: 2"));
    assert!(!svg.contains("private annotator"));
    assert!(!svg.contains("secret bbox"));
    assert!(!svg.contains("sensors.example.invalid"));
    let extensionless = temporary.path().join("annotations-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OpenLabel
    );
    let generic_json = temporary.path().join("annotations.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::OpenLabel
    );
}

#[test]
fn converts_citygml_metadata_without_geometry_or_xlink_resolution() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.citygml");
    let output = temporary.path().join("citygml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::CityGml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OGC CityGML"));
    assert!(svg.contains("Objects: 3"));
    assert!(svg.contains("CRS metadata: present"));
    assert!(svg.contains("Build"));
    assert!(svg.contains("Road"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("city-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::CityGml
    );
    let gml = temporary.path().join("city-model.gml");
    fs::copy(&input, &gml).unwrap();
    assert_eq!(SourceFormat::detect(&gml).unwrap(), SourceFormat::CityGml);
}

#[test]
fn converts_cityjson_without_expanding_coordinates_or_external_metadata() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cityjson");
    let output = temporary.path().join("cityjson-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::CityJson);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OGC CityJSON"));
    assert!(svg.contains("Objects: 2"));
    assert!(svg.contains("Transform: present"));
    assert!(svg.contains("Build"));
    assert!(svg.contains("Road"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("cityjson-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::CityJson
    );
    let generic_json = temporary.path().join("city.json");
    fs::copy(&input, &generic_json).unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_json).unwrap(),
        SourceFormat::CityJson
    );
}

#[test]
fn converts_stix_bundle_without_displaying_patterns_or_reference_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.stix.json");
    let output = temporary.path().join("stix-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::StixJson);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("STIX 2.1 JSON"));
    assert!(svg.contains("Objects: 2"));
    assert!(svg.contains("Indicators: 1"));
    assert!(!svg.contains("very-secret"));
    assert!(!svg.contains("secret description"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("threat-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::StixJson
    );
}

#[test]
fn converts_opencrg_header_without_decoding_road_payload() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.crg");
    let output = temporary.path().join("opencrg-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OpenCrg);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ASAM OpenCRG"));
    assert!(svg.contains("ROAD_CRG"));
    assert!(svg.contains("Sections: 5"));
    assert!(!svg.contains("private_surface"));
    let extensionless = temporary.path().join("road-surface");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OpenCrg
    );
}

#[test]
fn converts_taxii_manifest_without_network_or_stix_payload_access() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.taxii.json");
    let output = temporary.path().join("taxii-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::TaxiiJson);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("TAXII 2.1 JSON"));
    assert!(svg.contains("Resource: manifest"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("taxii-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::TaxiiJson
    );
}

#[test]
fn converts_wsdl_service_description_without_import_or_soap_access() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wsdl");
    let output = temporary.path().join("wsdl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Wsdl);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("WSDL service description"));
    assert!(svg.contains("Services: 1"));
    assert!(svg.contains("Operations: 3"));
    assert!(svg.contains("CatalogService"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("service-definition");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Wsdl
    );
}

#[test]
fn converts_opml_outline_without_fetching_feed_urls() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.opml");
    let output = temporary.path().join("opml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Opml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OPML outline"));
    assert!(svg.contains("Outlines: 5"));
    assert!(svg.contains("Feed outlines: 2"));
    assert!(svg.contains("Release notes"));
    assert!(svg.contains("Standards"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private description"));
    let extensionless = temporary.path().join("feed-outline");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Opml
    );
    let xml_alias = temporary.path().join("feed-outline.opml.xml");
    fs::copy(&input, &xml_alias).unwrap();
    assert_eq!(
        SourceFormat::detect(&xml_alias).unwrap(),
        SourceFormat::Opml
    );
}

#[test]
fn converts_rss_feed_without_fetching_links_or_rendering_content() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rss");
    let output = temporary.path().join("rss-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Feed);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("RSS/Atom feed"));
    assert!(svg.contains("Items/entries: 2"));
    assert!(svg.contains("Release 1.2"));
    assert!(svg.contains("Maintenance"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private article body"));
    assert!(!svg.contains("alert("));
    assert!(!svg.contains("private HTML"));
    let extensionless = temporary.path().join("engineering-feed");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Feed
    );
}

#[test]
fn converts_atom_feed_without_following_entry_links() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.atom");
    let output = temporary.path().join("atom-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Feed);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Format: Atom"));
    assert!(svg.contains("Items/entries: 1"));
    assert!(svg.contains("Paper one"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private abstract"));
}

#[test]
fn converts_xml_plist_without_rendering_secrets_or_urls() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.plist");
    let output = temporary.path().join("plist-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Plist);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Apple Property List"));
    assert!(svg.contains("XML plist"));
    assert!(svg.contains("CFBundleName"));
    assert!(svg.contains("Document SVG"));
    assert!(svg.contains("[URL omitted]"));
    assert!(svg.contains("[redacted]"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("do-not-render"));
    assert!(!svg.contains("SENSITIVE"));
    let extensionless = temporary.path().join("app-settings");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Plist
    );
}

#[test]
fn converts_binary_plist_with_bounded_object_summary() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.bplist");
    let output = temporary.path().join("bplist-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Plist);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Format: Binary plist"));
    assert!(svg.contains("Objects: 3"));
    assert!(svg.contains("object[2]"));
}

#[test]
fn converts_tei_scholarly_text_without_external_targets() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.tei");
    let output = temporary.path().join("tei-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Tei);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("TEI scholarly text"));
    assert!(svg.contains("A Bounded Edition"));
    assert!(svg.contains("Chapter One"));
    assert!(svg.contains("First paragraph"));
    assert!(svg.contains("Divisions: 3"));
    assert!(svg.contains("Paragraphs: 2"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private source description"));
}

#[test]
fn converts_alto_ocr_layout_without_opening_source_images() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.alto");
    let output = temporary.path().join("alto-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Alto);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ALTO OCR layout"));
    assert!(svg.contains("Pages: 2"));
    assert!(svg.contains("OCR metadata"));
    assert!(svg.contains("The first page"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("ocr-layout");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Alto
    );
}

#[test]
fn converts_mets_archive_structure_without_following_locations() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mets");
    let output = temporary.path().join("mets-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Mets);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("METS archive structure"));
    assert!(svg.contains("File groups: 2"));
    assert!(svg.contains("Structural maps: 2"));
    assert!(svg.contains("Chapter One"));
    assert!(svg.contains("image/tiff"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("Private title metadata"));
    let extensionless = temporary.path().join("archive-structure");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Mets
    );
}

#[test]
fn converts_marcxml_records_without_exposing_catalog_urls() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.marcxml");
    let output = temporary.path().join("marcxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Marcxml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("MARCXML record preview"));
    assert!(svg.contains("Records: 2"));
    assert!(svg.contains("A Safe Catalog Record"));
    assert!(svg.contains("Example Author"));
    assert!(svg.contains("[URL omitted]"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("catalog-records");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Marcxml
    );
}

#[test]
fn converts_mods_collection_without_exposing_location_urls() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mods");
    let output = temporary.path().join("mods-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Mods);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("MODS bibliographic record"));
    assert!(svg.contains("Records: 2"));
    assert!(svg.contains("Digital Preservation Handbook"));
    assert!(svg.contains("Example Author"));
    assert!(svg.contains("technical report"));
    assert!(svg.contains("[URL omitted]"));
    assert!(svg.contains("external location omitted"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private catalog note"));
    let extensionless = temporary.path().join("mods-records");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Mods
    );
}

#[test]
fn converts_premis_preservation_metadata_without_exposing_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.premis");
    let output = temporary.path().join("premis-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Premis);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("PREMIS preservation metadata"));
    assert!(svg.contains("Objects: 1"));
    assert!(svg.contains("Events: 1"));
    assert!(svg.contains("Agents: 1"));
    assert!(svg.contains("Rights statements: 1"));
    assert!(svg.contains("validation"));
    assert!(svg.contains("Document Validator"));
    assert!(!svg.contains("private-checksum"));
    assert!(!svg.contains("private command"));
    assert!(!svg.contains("private rights payload"));
}

#[test]
fn converts_iiif_manifest_without_fetching_images_or_ids() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.iiif.json");
    let output = temporary.path().join("iiif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Iiif);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("IIIF Presentation manifest"));
    assert!(svg.contains("Canvases: 2"));
    assert!(svg.contains("Painted images: 2"));
    assert!(svg.contains("Page 1"));
    assert!(svg.contains("Chapter One"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("do-not-render"));
    assert!(!svg.contains("private annotation"));
    let extensionless = temporary.path().join("presentation-manifest");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Iiif
    );
}

#[test]
fn converts_iiif_v2_sequence_without_fetching_image_service() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.iiif.v2.json");
    let output = temporary.path().join("iiif-v2-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Iiif);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Presentation API: 2"));
    assert!(svg.contains("Canvases: 1"));
    assert!(svg.contains("Legacy page"));
    assert!(!svg.contains("private.example.invalid"));
}

#[test]
fn converts_ead_finding_aid_without_opening_digital_objects() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ead");
    let output = temporary.path().join("ead-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ead);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("EAD archival finding aid"));
    assert!(svg.contains("Components: 3"));
    assert!(svg.contains("Series One"));
    assert!(svg.contains("Correspondence"));
    assert!(svg.contains("Example Archive"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private scope description"));
    let extensionless = temporary.path().join("finding-aid");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ead
    );
}

#[test]
fn converts_eac_cpf_authority_without_exposing_relations() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.eac-cpf");
    let output = temporary.path().join("eac-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::EacCpf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("EAC-CPF archival authority"));
    assert!(svg.contains("Entity type: person"));
    assert!(svg.contains("Example Researcher"));
    assert!(svg.contains("CPF relations: 1"));
    assert!(svg.contains("Resource relations: 1"));
    assert!(svg.contains("Function relations: 1"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private biographical description"));
    assert!(!svg.contains("Private relation"));
}

#[test]
fn converts_dublin_core_metadata_without_exposing_urls_or_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.dc.xml");
    let output = temporary.path().join("dc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::DublinCore);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Dublin Core metadata"));
    assert!(svg.contains("Records: 1"));
    assert!(svg.contains("A Safe Dublin Core Record"));
    assert!(svg.contains("Example Author"));
    assert!(svg.contains("Document conversion"));
    assert!(svg.contains("[value omitted]"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private rights text"));
    assert!(!svg.contains("private description payload"));
    let extensionless = temporary.path().join("dc-metadata");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::DublinCore
    );
}

#[test]
fn converts_s1000d_data_module_without_following_dm_or_icn_references() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.s1000d");
    let output = temporary.path().join("s1000d-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::S1000d);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("S1000D data module"));
    assert!(svg.contains("DMC: modelIdentCode=EXAMPLE"));
    assert!(svg.contains("Hydraulic Pump"));
    assert!(svg.contains("Remove the access panel"));
    assert!(svg.contains("Security classification"));
    assert!(!svg.contains("private-image"));
    assert!(!svg.contains("private-target"));
    let extensionless = temporary.path().join("maintenance-module");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::S1000d
    );
}

#[test]
fn converts_dicom_structured_report_without_following_references() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_sr.dcm");
    let output = temporary.path().join("dicom-sr-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::DicomSr);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("DICOM Structured Report"));
    assert!(svg.contains("Basic Text SR"));
    assert!(svg.contains("Finding"));
    assert!(svg.contains("No acute abnormality"));
    assert!(svg.contains("4.2 millimeter"));
    assert!(svg.contains("Procedure"));
    assert!(svg.contains("URL omitted"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("sr-document");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::DicomSr
    );
}

#[test]
fn converts_spreadsheetml_without_evaluating_formulas_or_external_links() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.spreadsheetml");
    let output = temporary.path().join("xmlss-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Spreadsheetml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SpreadsheetML 2003"));
    assert!(svg.contains("Worksheets: 2"));
    assert!(svg.contains("Formula cells: 2"));
    assert!(svg.contains("Widget A"));
    assert!(svg.contains("formula omitted"));
    assert!(svg.contains("URL omitted"));
    assert!(!svg.contains("https://private.example.invalid"));
    let extensionless = temporary.path().join("office-xml-sheet");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Spreadsheetml
    );
}

#[test]
fn converts_rdfxml_without_resolving_iris_or_nested_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rdf");
    let output = temporary.path().join("rdfxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::RdfXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("RDF/XML graph"));
    assert!(svg.contains("Safe RDF document"));
    assert!(svg.contains("Example Author"));
    assert!(svg.contains("IRI omitted"));
    assert!(svg.contains("Blank nodes: 1"));
    assert!(svg.contains("nested resource omitted"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private nested payload"));
    let extensionless = temporary.path().join("semantic-graph");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::RdfXml
    );
}

#[test]
fn converts_bcfzip_issue_package_without_opening_models_or_urls() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.bcfzip");
    let output = temporary.path().join("bcf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Bcfzip);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("BCF issue package"));
    assert!(svg.contains("Example BIM Coordination"));
    assert!(svg.contains("Clash at stair core"));
    assert!(svg.contains("type=Issue"));
    assert!(svg.contains("status=Open"));
    assert!(svg.contains("priority=High"));
    assert!(svg.contains("Comments: 1"));
    assert!(svg.contains("Snapshots skipped: 1"));
    assert!(svg.contains("external models"));
    assert!(!svg.contains("private issue description"));
    assert!(!svg.contains("private comment payload"));
    assert!(!svg.contains("topic-001"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("coordination-package");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Bcfzip
    );
}

#[test]
fn converts_flat_opc_word_package_without_extracting_or_executing_parts() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.flatopc");
    let output = temporary.path().join("flat-opc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::FlatOpc);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Flat OPC office document"));
    assert!(svg.contains("External links remain inert"));
    assert!(svg.contains("bounded inert Open XML package"));
    assert!(!svg.contains("http://private.example.invalid"));
    let extensionless = temporary.path().join("office-flat-package");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::FlatOpc
    );
}

#[test]
fn converts_aasx_package_without_opening_supplementary_cad_or_manual_files() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.aasx");
    let output = temporary.path().join("aasx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Aasx);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("AASX asset package"));
    assert!(svg.contains("Asset Administration Shells: 1"));
    assert!(svg.contains("Submodels: 1"));
    assert!(svg.contains("Concept descriptions: 1"));
    assert!(svg.contains("Supplementary files skipped: 1"));
    assert!(svg.contains("Thumbnails skipped: 1"));
    assert!(svg.contains("Pump A"));
    assert!(svg.contains("Maintenance"));
    assert!(!svg.contains("PRIVATE-SERIAL"));
    assert!(!svg.contains("manual.pdf"));
    assert!(!svg.contains("private-thumbnail"));
    let extensionless = temporary.path().join("asset-package");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Aasx
    );
}

#[test]
fn previews_openscad_source_without_executing_imports_or_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.scad");
    let output = temporary.path().join("openscad-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OpenScad);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OpenSCAD source"));
    assert!(svg.contains("rounded_box"));
    assert!(svg.contains("imports"));
    assert!(svg.contains("not a rendered solid model"));
    assert!(!svg.contains("private-library.scad"));
    assert!(!svg.contains("private-model.stl"));
    let extensionless = temporary.path().join("cad-source");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OpenScad
    );
}

#[test]
fn converts_amf_mesh_without_opening_materials_or_external_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.amf");
    let output = temporary.path().join("amf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Amf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("AMF mesh"));
    assert!(svg.contains("shaded preview"));
    assert!(!svg.contains("Private AMF design"));
    assert!(!svg.contains("Private material"));
    let extensionless = temporary.path().join("additive-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Amf
    );
}

#[test]
fn converts_plmxml_product_structure_without_following_cad_references() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.plmxml");
    let output = temporary.path().join("plmxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::PlmXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("PLMXML product structure"));
    assert!(svg.contains("schemaVersion=7.0"));
    assert!(svg.contains("parts=1 structures=1 instances=1"));
    assert!(svg.contains("externalReferences=1"));
    assert!(!svg.contains("Private Engineer"));
    assert!(!svg.contains("SECRET-VALUE"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("product-structure");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::PlmXml
    );
}

#[test]
fn converts_step_xml_without_fetching_schemas_or_external_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.stepxml");
    let output = temporary.path().join("stepxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::StepXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("STEP-XML product data"));
    assert!(svg.contains("product_definitions=1"));
    assert!(svg.contains("items=1 points=1 directions=1"));
    assert!(svg.contains("externalReferences=1"));
    assert!(!svg.contains("private-product-id"));
    assert!(!svg.contains("Private property"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("step-xml-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::StepXml
    );
}

#[test]
fn converts_qif_inspection_data_without_exposing_measurement_values_or_files() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.qif");
    let output = temporary.path().join("qif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Qif);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("QIF inspection data"));
    assert!(svg.contains("results=1 traceability=1"));
    assert!(svg.contains("features=1 characteristics=1"));
    assert!(svg.contains("External files"));
    assert!(!svg.contains("PRIVATE-PRODUCT"));
    assert!(!svg.contains("PRIVATE-MEASURED-VALUE"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("inspection-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Qif
    );
}

#[test]
fn converts_b2mml_manufacturing_data_without_running_operations() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.b2mml");
    let output = temporary.path().join("b2mml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::B2mml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("B2MML manufacturing data"));
    assert!(svg.contains("performances=1 requests=1"));
    assert!(svg.contains("materials=2 personnel=1"));
    assert!(svg.contains("capabilities=1"));
    assert!(svg.contains("externalReferences=1"));
    assert!(!svg.contains("PRIVATE-REQUEST"));
    assert!(!svg.contains("PRIVATE-PERFORMANCE-VALUE"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("manufacturing-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::B2mml
    );
}

#[test]
fn converts_jdf_job_ticket_without_running_devices_or_urls() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jdf");
    let output = temporary.path().join("jdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Jdf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("JDF job ticket"));
    assert!(svg.contains("version=1.8 status=Waiting"));
    assert!(svg.contains("devices=1 media=1"));
    assert!(svg.contains("URL/file links counted"));
    assert!(!svg.contains("private-job"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("Private brand"));
    let extensionless = temporary.path().join("job-ticket");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Jdf
    );
}

#[test]
fn converts_xjdf_job_ticket_without_running_workflow_commands() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xjdf");
    let output = temporary.path().join("xjdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xjdf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XJDF job ticket"));
    assert!(svg.contains("version=2.2 status=Waiting"));
    assert!(!svg.contains("private-xjdf"));
    let extensionless = temporary.path().join("exchange-job-ticket");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xjdf
    );
}

#[test]
fn converts_cml_chemical_document_without_resolving_dictionaries() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cml");
    let output = temporary.path().join("cml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Cml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("CML chemical document"));
    assert!(svg.contains("Molecules: 1"));
    assert!(svg.contains("Atoms: 3"));
    assert!(svg.contains("Bonds: 2"));
    assert!(svg.contains("Water"));
    assert!(svg.contains("H=2 O=1"));
    assert!(!svg.contains("private value"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private spectrum payload"));
    let extensionless = temporary.path().join("chemical-xml");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Cml
    );
}

#[test]
fn converts_xdp_package_without_executing_xfa_logic() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xdp");
    let output = temporary.path().join("xdp-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xdp);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XDP/XFA data package"));
    assert!(svg.contains("Packets: 4"));
    assert!(svg.contains("Fields: 3"));
    assert!(svg.contains("ApplicantName"));
    assert!(svg.contains("Scripts/calculations: 2"));
    assert!(svg.contains("Submit/action nodes: 1"));
    assert!(!svg.contains("private-xdp-uuid"));
    assert!(!svg.contains("private applicant"));
    assert!(!svg.contains("secret-value"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private embedded PDF payload"));
    let extensionless = temporary.path().join("xfa-package");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xdp
    );
}

#[test]
fn converts_xmp_metadata_without_exposing_private_identifiers() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xmp");
    let output = temporary.path().join("xmp-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xmp);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XMP metadata"));
    assert!(svg.contains("A Safe XMP Title"));
    assert!(svg.contains("Example Author"));
    assert!(svg.contains("2026-09-17T10:00:00Z"));
    assert!(svg.contains("Safe Producer"));
    assert!(svg.contains("Private/identifier properties omitted: 3"));
    assert!(!svg.contains("private-document-id"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private description payload"));
    assert!(!svg.contains("private-image-bytes"));
    let extensionless = temporary.path().join("metadata-packet");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xmp
    );
}

#[test]
fn converts_mathml_without_exposing_annotations_or_scripts() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mathml");
    let output = temporary.path().join("mathml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Mathml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("MathML formula"));
    assert!(svg.contains("Tokens: 14"));
    assert!(svg.contains("Annotations skipped: 2"));
    assert!(svg.contains("x_(1)^(2)"));
    assert!(svg.contains("root(c, 3)"));
    assert!(!svg.contains("private annotation payload"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("alert('private')"));
    let extensionless = temporary.path().join("equation");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Mathml
    );
}

#[test]
fn converts_landxml_civil_model_without_exposing_coordinate_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.landxml");
    let output = temporary.path().join("landxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::LandXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("LandXML civil model"));
    assert!(svg.contains("Version: 1.2"));
    assert!(svg.contains("Units: meter"));
    assert!(svg.contains("Surfaces: 1"));
    assert!(svg.contains("points=3 faces=1"));
    assert!(svg.contains("segments=2"));
    assert!(svg.contains("Pipe networks: 1"));
    assert!(!svg.contains("100.0"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private terrain"));
    let extensionless = temporary.path().join("civil-exchange");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::LandXml
    );
}

#[test]
fn converts_xfdf_form_data_without_fetching_pdf_targets() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xfdf");
    let output = temporary.path().join("xfdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xfdf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XFDF form data"));
    assert!(svg.contains("Applicant"));
    assert!(svg.contains("Alice Example"));
    assert!(svg.contains("Sensitive fields: 1"));
    assert!(svg.contains("[sensitive value omitted]"));
    assert!(svg.contains("Rich-text fields: 1"));
    assert!(svg.contains("Annotations: 2"));
    assert!(!svg.contains("secret-xfdf"));
    assert!(!svg.contains("private rich text"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("form-xml");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xfdf
    );
}

#[test]
fn converts_fdf_form_data_without_executing_actions() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fdf");
    let output = temporary.path().join("fdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Fdf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("FDF form data"));
    assert!(svg.contains("Full Name"));
    assert!(svg.contains("Alice Example"));
    assert!(svg.contains("Password fields: 1"));
    assert!(svg.contains("[password omitted]"));
    assert!(svg.contains("Choice options: 2"));
    assert!(svg.contains("Action dictionaries: 1"));
    assert!(svg.contains("[URL omitted]"));
    assert!(!svg.contains("secret-value"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("form-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Fdf
    );
}

#[test]
fn converts_xbrl_instance_without_fetching_taxonomies_or_entity_urls() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xbrl.xml");
    let output = temporary.path().join("xbrl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xbrl);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XBRL 2.1 instance"));
    assert!(svg.contains("Contexts: 2"));
    assert!(svg.contains("Facts: 4"));
    assert!(svg.contains("Revenue"));
    assert!(svg.contains("1250000"));
    assert!(svg.contains("iso4217:USD"));
    assert!(svg.contains("2026-01-01"));
    assert!(svg.contains("Tuples: 1"));
    assert!(!svg.contains("EXAMPLE-ENTITY"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private footnote payload"));
    let extensionless = temporary.path().join("financial-instance");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xbrl
    );
}

#[test]
fn converts_ubl_invoice_without_exposing_party_or_amount_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ubl.xml");
    let output = temporary.path().join("ubl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ubl);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("UBL Invoice"));
    assert!(svg.contains("UBL version"));
    assert!(svg.contains("2026-09-16"));
    assert!(svg.contains("2026-10-16"));
    assert!(svg.contains("Line items"));
    assert!(svg.contains("supplier=1"));
    assert!(svg.contains("JPY"));
    assert!(!svg.contains("Private Supplier"));
    assert!(!svg.contains("private item description"));
    assert!(!svg.contains("private-bank-account"));
    assert!(!svg.contains("11000"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("business-invoice");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Ubl
    );
}

#[test]
fn converts_iso19115_metadata_without_exposing_private_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.iso19115.xml");
    let output = temporary.path().join("iso19115-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Iso19115);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ISO 19115 metadata"));
    assert!(svg.contains("Coastal habitat survey"));
    assert!(svg.contains("EPSG:4326"));
    assert!(svg.contains("west=-123.5"));
    assert!(svg.contains("biota"));
    assert!(svg.contains("2025-04-01"));
    assert!(!svg.contains("private.example.invalid"));
    assert!(!svg.contains("private abstract payload"));
    assert!(!svg.contains("private lineage payload"));
    assert!(!svg.contains("Private Contact"));
    let extensionless = temporary.path().join("coastal-metadata");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Iso19115
    );
}

#[test]
fn converts_marc21_iso2709_records_without_exposing_catalog_urls() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.marc");
    let output = temporary.path().join("marc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Marc);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("MARC21 ISO 2709 record preview"));
    assert!(svg.contains("Records: 2"));
    assert!(svg.contains("MARC21 ISO 2709 Record"));
    assert!(svg.contains("Example Author"));
    assert!(svg.contains("[URL omitted]"));
    assert!(!svg.contains("private.example.invalid"));
    let extensionless = temporary.path().join("iso2709-records");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Marc
    );
}

#[test]
fn converts_tmx_translation_memory_without_exposing_segments() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.tmx");
    let output = temporary.path().join("tmx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Tmx);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("TMX translation memory"));
    assert!(svg.contains("variants=4"));
    assert!(svg.contains("de, en, ja"));
    assert!(!svg.contains("Hello world"));
    assert!(!svg.contains("private note"));
    let extensionless = temporary.path().join("translation-memory");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Tmx
    );
}

#[test]
fn converts_tbx_terminology_without_exposing_term_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.tbx");
    let output = temporary.path().join("tbx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Tbx);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("TBX terminology base"));
    assert!(svg.contains("dialect=TBX"));
    assert!(svg.contains("terms=2"));
    assert!(svg.contains("en, ja"));
    assert!(!svg.contains("gearbox"));
    assert!(!svg.contains("private definition"));
    assert!(!svg.contains("example.invalid"));
    let extensionless = temporary.path().join("terminology");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Tbx
    );
}

#[test]
fn converts_gbxml_building_model_without_exposing_geometry_or_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gbxml");
    let output = temporary.path().join("gbxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::GbXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("gbXML building model"));
    assert!(svg.contains("version=8.01"));
    assert!(svg.contains("campuses=1"));
    assert!(svg.contains("spaces=1"));
    assert!(svg.contains("openings=1"));
    assert!(!svg.contains("Private site"));
    assert!(!svg.contains("999"));
    assert!(!svg.contains("private material"));
    let extensionless = temporary.path().join("building-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::GbXml
    );
}

#[test]
fn converts_fhir_xml_without_exposing_clinical_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fhir.xml");
    let output = temporary.path().join("fhir-xml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::FhirXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("FHIR XML"));
    assert!(svg.contains("resources=3"));
    assert!(svg.contains("Patient"));
    assert!(svg.contains("Observation"));
    assert!(!svg.contains("patient-secret"));
    assert!(!svg.contains("private narrative"));
    assert!(!svg.contains("private result"));
    let extensionless = temporary.path().join("fhir-bundle");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::FhirXml
    );
    let generic_xml = temporary.path().join("generic.xml");
    fs::write(
        &generic_xml,
        r#"<config><link>https://hl7.org/fhir/Patient</link></config>"#,
    )
    .unwrap();
    assert_eq!(
        SourceFormat::detect(&generic_xml).unwrap(),
        SourceFormat::Xml
    );
}

#[test]
fn converts_idml_package_without_extracting_private_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.idml");
    let output = temporary.path().join("idml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Idml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("IDML InDesign package"));
    assert!(svg.contains("stories=1"));
    assert!(svg.contains("masterSpreads=1"));
    assert!(svg.contains("images=0"));
    assert!(!svg.contains("private story text"));
    assert!(!svg.contains("example.invalid"));
    assert!(!svg.contains("story-1"));
    let extensionless = temporary.path().join("indesign-package");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Idml
    );
}

#[test]
fn converts_xpdl_workflow_without_executing_process_logic() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xpdl");
    let output = temporary.path().join("xpdl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xpdl);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XPDL workflow definition"));
    assert!(svg.contains("activities=2"));
    assert!(svg.contains("transitions=1"));
    assert!(!svg.contains("private participant"));
    assert!(!svg.contains("secret"));
    let extensionless = temporary.path().join("workflow-definition");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xpdl
    );
}

#[test]
fn converts_onix_without_exposing_publishing_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.onix");
    let output = temporary.path().join("onix-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Onix);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ONIX for Books message"));
    assert!(svg.contains("release=3.0"));
    assert!(svg.contains("identifiers=1"));
    assert!(svg.contains("prices=1"));
    assert!(!svg.contains("Private Book Title"));
    assert!(!svg.contains("9780000000000"));
    assert!(!svg.contains("999"));
    assert!(!svg.contains("private description"));
    let extensionless = temporary.path().join("book-message");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Onix
    );
}

#[test]
fn converts_oai_pmh_without_harvesting_or_exposing_records() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.oaipmh");
    let output = temporary.path().join("oaipmh-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OaiPmh);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OAI-PMH response"));
    assert!(svg.contains("listRecords=1"));
    assert!(svg.contains("headers=1"));
    assert!(svg.contains("metadata=1"));
    assert!(!svg.contains("private-id"));
    assert!(!svg.contains("private title"));
    assert!(!svg.contains("private-token"));
    assert!(!svg.contains("example.invalid"));
    let extensionless = temporary.path().join("harvest-response");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OaiPmh
    );
}

#[test]
fn converts_cda_without_exposing_phi_or_narrative() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cda");
    let output = temporary.path().join("cda-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Cda);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("CDA clinical document"));
    assert!(svg.contains("sections=1"));
    assert!(svg.contains("observations=1"));
    assert!(!svg.contains("patient-secret"));
    assert!(!svg.contains("private narrative"));
    assert!(!svg.contains("private-value"));
    let extensionless = temporary.path().join("clinical-document");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Cda
    );
}

#[test]
fn converts_iso20022_without_exposing_financial_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.iso20022.xml");
    let output = temporary.path().join("iso20022-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Iso20022);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ISO 20022 message"));
    assert!(svg.contains("transactions=1"));
    assert!(svg.contains("accounts=1"));
    assert!(!svg.contains("Private Debtor"));
    assert!(!svg.contains("DESECRET"));
    assert!(!svg.contains("999.00"));
    assert!(!svg.contains("private remittance"));
    let extensionless = temporary.path().join("payment-message");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Iso20022
    );
}

#[test]
fn converts_sbml_without_evaluating_equations() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sbml");
    let output = temporary.path().join("sbml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Sbml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SBML model"));
    assert!(svg.contains("species=1"));
    assert!(svg.contains("reactions=1"));
    assert!(!svg.contains("private-equation"));
    assert!(!svg.contains("initialAmount"));
    let extensionless = temporary.path().join("systems-biology-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Sbml
    );
}

#[test]
fn converts_cellml_without_evaluating_mathml_or_imports() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cellml");
    let output = temporary.path().join("cellml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Cellml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("CellML model"));
    assert!(svg.contains("components=1"));
    assert!(svg.contains("variables=1"));
    assert!(!svg.contains("private-equation"));
    assert!(!svg.contains("example.invalid"));
    let extensionless = temporary.path().join("cell-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Cellml
    );
}

#[test]
fn converts_ocel_xml_without_exposing_event_log_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xmlocel");
    let output = temporary.path().join("ocel-xml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OcelXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OCEL XML event log"));
    assert!(svg.contains("eventTypes=1"));
    assert!(svg.contains("events=1"));
    assert!(svg.contains("objects=1"));
    assert!(!svg.contains("private-event"));
    assert!(!svg.contains("private-value"));
    let extensionless = temporary.path().join("event-log");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OcelXml
    );
}

#[test]
fn converts_energyplus_idf_without_running_simulation() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.idf");
    let output = temporary.path().join("idf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::EnergyPlusIdf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("EnergyPlus IDF input"));
    assert!(svg.contains("Building"));
    assert!(svg.contains("Zone"));
    assert!(!svg.contains("Private Building"));
    assert!(!svg.contains("Private Material"));
    let extensionless = temporary.path().join("energy-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::EnergyPlusIdf
    );
    let compact = temporary.path().join("compact.idf");
    fs::write(&compact, "Version, 24.1;Building, Compact, 0.0, City;").unwrap();
    let compact_output = temporary.path().join("compact-idf-out");
    let compact_report =
        convert_path(&compact, &compact_output, &ConvertOptions::default()).unwrap();
    assert_eq!(compact_report.source_format, SourceFormat::EnergyPlusIdf);
    let compact_svg = fs::read_to_string(compact_output.join("page-0001.svg")).unwrap();
    assert!(compact_svg.contains("Building"));
}

#[test]
fn converts_energyplus_epw_without_exposing_weather_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.epw");
    let output = temporary.path().join("epw-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::EnergyPlusEpw);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("EnergyPlus EPW weather"));
    assert!(svg.contains("Hourly rows"));
    assert!(svg.contains("2"));
    assert!(!svg.contains("Private City"));
    assert!(!svg.contains("10.0"));
    let extensionless = temporary.path().join("weather-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::EnergyPlusEpw
    );
}

#[test]
fn converts_rinex_without_exposing_station_or_measurements() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rnx");
    let output = temporary.path().join("rinex-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Rinex);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("RINEX GNSS data"));
    assert!(svg.contains("epochs"));
    assert!(svg.contains("satelliteRows=3"));
    assert!(!svg.contains("PRIVATE-STATION"));
    assert!(!svg.contains("12345.0"));
    let extensionless = temporary.path().join("gnss-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Rinex
    );
}

#[test]
fn converts_acis_sat_without_tessellating_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sat");
    let output = temporary.path().join("sat-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Sat);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ACIS SAT model"));
    assert!(svg.contains("solid"));
    assert!(svg.contains("vertex"));
    assert!(!svg.contains("private-solid-id"));
    assert!(!svg.contains("private-vertex-id"));
    let extensionless = temporary.path().join("acis-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Sat
    );
}

#[test]
fn converts_sedml_without_running_models_or_simulations() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sedml");
    let output = temporary.path().join("sedml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Sedml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SED-ML experiment"));
    assert!(svg.contains("models=1"));
    assert!(svg.contains("simulations=1"));
    assert!(svg.contains("plots=1"));
    assert!(!svg.contains("private-equation"));
    assert!(!svg.contains("private.xml"));
    let extensionless = temporary.path().join("experiment");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Sedml
    );
}

#[test]
fn converts_sbgnml_without_exposing_pathway_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sbgnml");
    let output = temporary.path().join("sbgnml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Sbgnml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SBGN-ML map"));
    assert!(svg.contains("arcs=1"));
    assert!(svg.contains("labels=1"));
    assert!(!svg.contains("private-glyph"));
    assert!(!svg.contains("Private protein"));
    let extensionless = temporary.path().join("pathway-map");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Sbgnml
    );
}

#[test]
fn converts_omex_without_extracting_or_executing_archive_members() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.omex");
    let output = temporary.path().join("omex-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Omex);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("COMBINE/OMEX archive"));
    assert!(svg.contains("SBML=1"));
    assert!(svg.contains("SED-ML=1"));
    assert!(svg.contains("external references=1"));
    assert!(!svg.contains("external.xml"));
    let extensionless = temporary.path().join("combine-archive");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Omex
    );
}

#[test]
fn converts_xdmf_without_opening_hdf5_arrays() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xdmf");
    let output = temporary.path().join("xdmf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xdmf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XDMF mesh metadata"));
    assert!(svg.contains("grids=1"));
    assert!(svg.contains("attributes=1"));
    assert!(!svg.contains("private.h5"));
    assert!(!svg.contains("PrivateTemperature"));
    let extensionless = temporary.path().join("mesh-metadata");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xdmf
    );
}

#[test]
fn converts_pvd_without_opening_vtk_sidecars() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pvd");
    let output = temporary.path().join("pvd-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Pvd);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("VTK PVD collection"));
    assert!(svg.contains("remoteRefs=1"));
    assert!(!svg.contains("mesh_0000.vtu"));
    assert!(!svg.contains("example.invalid"));
    let extensionless = temporary.path().join("vtk-collection");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Pvd
    );
}

#[test]
fn converts_fds_without_running_fire_simulation() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fds");
    let output = temporary.path().join("fds-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Fds);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("FDS input deck"));
    assert!(svg.contains("MESH"));
    assert!(svg.contains("OBST"));
    assert!(!svg.contains("private-fire"));
    assert!(!svg.contains("private-device"));
    let extensionless = temporary.path().join("fire-input");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Fds
    );
}

#[test]
fn converts_abiword_without_exposing_document_text() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.abw");
    let output = temporary.path().join("abw-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Abiword);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("AbiWord document"));
    assert!(svg.contains("paragraphs=2"));
    assert!(svg.contains("tables=1"));
    assert!(!svg.contains("Private paragraph"));
    assert!(!svg.contains("private link"));
    let extensionless = temporary.path().join("word-document");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Abiword
    );
}

#[test]
fn converts_neuroml_without_exposing_model_values() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nml");
    let output = temporary.path().join("neuroml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Neuroml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("NeuroML model"));
    assert!(svg.contains("morphologies=1"));
    assert!(svg.contains("populations=1"));
    assert!(!svg.contains("private.cell.nml"));
    assert!(!svg.contains("tauRise"));
    let extensionless = temporary.path().join("neural-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Neuroml
    );
}

#[test]
fn converts_biopax_without_resolving_owl_or_pathway_links() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.biopax.xml");
    let output = temporary.path().join("biopax-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Biopax);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("BioPAX pathway"));
    assert!(svg.contains("pathways=1"));
    assert!(svg.contains("proteins=1"));
    assert!(svg.contains("Reactions"));
    assert!(!svg.contains("private-pathway"));
    assert!(!svg.contains("Private protein"));
    assert!(!svg.contains("biopax-level3.owl"));
    let extensionless = temporary.path().join("pathway-data");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Biopax
    );
}

#[test]
fn converts_xsd_without_fetching_schema_dependencies() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xsd");
    let output = temporary.path().join("xsd-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xsd);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XML Schema definition"));
    assert!(svg.contains("complexTypes=1"));
    assert!(svg.contains("imports=1"));
    assert!(!svg.contains("private-common.xsd"));
    assert!(!svg.contains("secret.xsd"));
    assert!(!svg.contains("private"));
    let extensionless = temporary.path().join("schema-document");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xsd
    );
}

#[test]
fn converts_xslt_without_executing_templates_or_xpath() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xsl");
    let output = temporary.path().join("xslt-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xslt);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XSLT stylesheet"));
    assert!(svg.contains("templates=2"));
    assert!(svg.contains("applyTemplates=1"));
    assert!(!svg.contains("private-import.xsl"));
    assert!(!svg.contains("private-template"));
    assert!(!svg.contains("private"));
    let extensionless = temporary.path().join("transform-stylesheet");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xslt
    );
}

#[test]
fn converts_xsl_fo_without_running_formatter_or_loading_media() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fo");
    let output = temporary.path().join("xslfo-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::XslFo);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XSL-FO layout"));
    assert!(svg.contains("Pages"));
    assert!(svg.contains("Tables"));
    assert!(!svg.contains("private paragraph"));
    assert!(!svg.contains("example.invalid"));
    let extensionless = temporary.path().join("fo-layout");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::XslFo
    );
}

#[test]
fn converts_xproc_without_running_pipeline_steps() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xproc");
    let output = temporary.path().join("xproc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xproc);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XProc pipeline"));
    assert!(svg.contains("inputs=2"));
    assert!(svg.contains("httpOps=0"));
    assert!(!svg.contains("private.xml"));
    assert!(!svg.contains("example.invalid"));
    let extensionless = temporary.path().join("pipeline-document");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xproc
    );
}

#[test]
fn converts_wadl_without_contacting_endpoints() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wadl");
    let output = temporary.path().join("wadl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Wadl);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("WADL web application"));
    assert!(svg.contains("methods=1"));
    assert!(svg.contains("representations=1"));
    assert!(!svg.contains("example.invalid"));
    assert!(!svg.contains("private"));
    let extensionless = temporary.path().join("rest-description");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Wadl
    );
}

#[test]
fn converts_opensearch_without_search_requests() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.osdd");
    let output = temporary.path().join("opensearch-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::OpenSearch);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("OpenSearch description"));
    assert!(svg.contains("Search URLs"));
    assert!(!svg.contains("Private Search"));
    assert!(!svg.contains("example.invalid"));
    let extensionless = temporary.path().join("search-description");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::OpenSearch
    );
}

#[test]
fn converts_saml_metadata_without_exposing_endpoints_or_certificates() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.saml.xml");
    let output = temporary.path().join("saml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Saml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("SAML metadata"));
    assert!(svg.contains("IdP=1"));
    assert!(svg.contains("certificates=1"));
    assert!(!svg.contains("example.invalid"));
    assert!(!svg.contains("private-certificate"));
    let extensionless = temporary.path().join("identity-metadata");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Saml
    );
}

#[test]
fn converts_xacml_without_evaluating_authorization_policy() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xacml");
    let output = temporary.path().join("xacml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Xacml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("XACML policy"));
    assert!(svg.contains("policies=1"));
    assert!(svg.contains("rules=1"));
    assert!(svg.contains("targets=1"));
    assert!(!svg.contains("private-policy"));
    assert!(!svg.contains("private-value"));
    assert!(!svg.contains("example.invalid"));
    let extensionless = temporary.path().join("authorization-policy");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Xacml
    );
}

#[test]
fn converts_legacy_openoffice_xml_packages_through_odf_engines() {
    let temporary = TempDir::new().unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let cases = [
        ("sample.sxw", SourceFormat::Odt),
        ("sample.sxc", SourceFormat::Ods),
        ("sample.sxi", SourceFormat::Odp),
    ];
    for (filename, expected) in cases {
        let input = manifest.join(filename);
        let output = temporary.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, expected, "{filename}");
        assert!(report.page_count >= 1, "{filename}");
    }
}

#[test]
fn converts_legacy_visio_binary_container_without_decoding_opaque_payloads() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.vsd");
    let output = temporary.path().join("vsd-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Vsd);
    assert_eq!(report.page_count, 1);
    assert_eq!(report.warnings.len(), 2);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Legacy Visio binary document"));
    assert!(svg.contains("VisioDocument"));
    assert!(svg.contains("Pages/Page1"));
    assert!(!svg.contains("opaque page payload"));
    let extensionless = temporary.path().join("legacy-visio");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Vsd
    );
}

#[test]
fn converts_hdf5_and_cgns_superblocks_without_reading_dataset_payloads() {
    let temporary = TempDir::new().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (filename, expected) in [
        ("sample.h5", SourceFormat::Hdf5),
        ("sample.cgns", SourceFormat::Cgns),
        ("sample.exo", SourceFormat::Exodus),
    ] {
        let input = root.join(filename);
        let output = temporary.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, expected, "{filename}");
        assert_eq!(report.page_count, 1, "{filename}");
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        assert!(
            svg.contains(if expected == SourceFormat::Exodus {
                "Dimensions"
            } else {
                "Superblock version"
            }),
            "{filename}"
        );
        if expected != SourceFormat::Exodus {
            assert!(svg.contains("Signature offset"), "{filename}");
        }
    }
    let extensionless = temporary.path().join("scientific-container");
    fs::copy(root.join("sample.h5"), &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Hdf5
    );
    let user_block = temporary.path().join("user-block.h5");
    let mut wrapped = vec![0u8; 512];
    wrapped.extend_from_slice(&fs::read(root.join("sample.h5")).unwrap());
    fs::write(&user_block, wrapped).unwrap();
    let wrapped_output = temporary.path().join("user-block-out");
    let wrapped_report =
        convert_path(&user_block, &wrapped_output, &ConvertOptions::default()).unwrap();
    assert_eq!(wrapped_report.source_format, SourceFormat::Hdf5);
    let wrapped_svg = fs::read_to_string(wrapped_output.join("page-0001.svg")).unwrap();
    assert!(wrapped_svg.contains("512"));
    let exodus_alias = temporary.path().join("mesh.e");
    fs::copy(root.join("sample.exo"), &exodus_alias).unwrap();
    assert_eq!(
        SourceFormat::detect(&exodus_alias).unwrap(),
        SourceFormat::Exodus
    );
}

#[test]
fn converts_iwork_packages_without_decoding_opaque_iwa_records() {
    let temporary = TempDir::new().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (extension, title) in [
        ("pages", "Apple Pages package"),
        ("numbers", "Apple Numbers package"),
        ("key", "Apple Keynote package"),
    ] {
        let input = root.join(format!("sample.{extension}"));
        let output = temporary.path().join(format!("{extension}-out"));
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, SourceFormat::Iwork, "{extension}");
        assert_eq!(report.page_count, 1, "{extension}");
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        assert!(svg.contains(title), "{extension}");
        assert!(!svg.contains("opaque"), "{extension}");
    }
    let extensionless = temporary.path().join("iwork-package");
    fs::copy(root.join("sample.pages"), &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Iwork
    );
}

#[test]
fn converts_dwg_header_without_loading_binary_object_sections() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.dwg");
    let output = temporary.path().join("dwg-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dwg);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("AC1027"));
    assert!(svg.contains("AutoCAD 2013"));
    let extensionless = temporary.path().join("drawing");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Dwg
    );
}

#[test]
fn converts_ac1009_dwg_entities_into_real_vector_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_r12.dwg");
    let output = temporary.path().join("dwg-r12-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dwg);
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A LINE, CIRCLE, ARC and open POLYLINE were decoded into actual paths,
    // not just a header-metadata table.
    assert!(svg.contains("<path"));
    assert!(svg.contains(">HI<") || svg.contains("HI</tspan"));
    assert!(svg.contains("#ff0000")); // explicit LINE color (ACI 1)
    assert!(!svg.contains("Autodesk DWG header"));
}

#[test]
fn expands_ac1009_dwg_insert_block_references_into_real_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_r12_insert.dwg");
    let output = temporary.path().join("dwg-r12-insert-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dwg);
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The fixture inserts a "PAIR" block (itself two nested INSERTs of a
    // "DOT" block) plus one direct LINE, for three drawn paths total —
    // proof the block/table sections were decoded and INSERT was expanded,
    // not just skipped.
    assert_eq!(svg.matches("<path ").count(), 3);
}

#[test]
fn draws_ac1009_dwg_attdef_attrib_text_and_dimension_block_geometry() {
    let temporary = TempDir::new().unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_r12_attr_dim.dwg");
    let output = temporary.path().join("dwg-r12-attr-dim-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dwg);
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // ATTDEF's default value and ATTRIB's displayed value are drawn...
    assert!(svg.contains("PARTNO-001"));
    assert!(svg.contains("REV-A"));
    // ...but entities flagged invisible are not.
    assert!(!svg.contains("SHOULD_NOT_APPEAR"));
    // The DIMENSION entity expanded its cached "*D1" block into 3 LINEs
    // (two extension lines and the dimension line) at identity transform.
    assert_eq!(svg.matches("<path ").count(), 3);
}

#[test]
fn draws_ac1009_dwg_viewport_border_and_reports_shape_entities_distinctly() {
    let temporary = TempDir::new().unwrap();
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_r12_shape_viewport.dwg");
    let output = temporary.path().join("dwg-r12-shape-viewport-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dwg);
    assert_eq!(report.page_count, 1);
    // The VIEWPORT entity's border rectangle and the reference LINE are
    // both drawn; the SHAPE entity draws nothing (its glyph lives in an
    // external, un-embedded .SHX file) but is called out by name.
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].contains("SHAPE"));
    assert!(report.warnings[0].contains(".SHX"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("<path ").count(), 2);
}

#[test]
fn applies_ac1009_dwg_ocs_extrusion_to_circle_and_polyline() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_r12_ocs.dwg");
    let output = temporary.path().join("dwg-r12-ocs-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dwg);
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The CIRCLE has a (1,1,1)-tilted extrusion, so it must foreshorten
    // into a genuine ellipse (unequal arc radii), not stay a plain circle.
    let ellipse_path = svg
        .lines()
        .find(|l| l.contains(" A "))
        .expect("expected the tilted CIRCLE's elliptical path");
    let after_a = ellipse_path.split(" A ").nth(1).unwrap();
    let mut nums = after_a.split_whitespace();
    let rx: f64 = nums.next().unwrap().parse().unwrap();
    let ry: f64 = nums.next().unwrap().parse().unwrap();
    assert!(
        (rx - ry).abs() > rx * 0.05,
        "expected an ellipse (rx != ry), got rx={rx} ry={ry}"
    );
    // Two drawn shapes: the ellipse and the mirrored polyline.
    assert_eq!(svg.matches("<path ").count(), 2);
}

#[test]
fn converts_rhino_3dm_marker_without_loading_binary_model_chunks() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.3dm");
    let output = temporary.path().join("3dm-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Rhino3dm);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Rhino 3DM header"));
    assert!(svg.contains("3D Geometry File Format"));
    let extensionless = temporary.path().join("rhino-model");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Rhino3dm
    );
}

#[test]
fn converts_access_ace_and_jet_headers_without_opening_database_objects() {
    let temporary = TempDir::new().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for extension in ["accdb", "mdb"] {
        let input = root.join(format!("sample.{extension}"));
        let output = temporary.path().join(format!("{extension}-out"));
        let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        assert_eq!(report.source_format, SourceFormat::Access, "{extension}");
        assert_eq!(report.page_count, 1, "{extension}");
        let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
        assert!(
            svg.contains("Microsoft Access database header"),
            "{extension}"
        );
        assert!(svg.contains(if extension == "accdb" {
            "ACE / ACCDB"
        } else {
            "Jet / MDB"
        }));
    }
    let extensionless = temporary.path().join("access-database");
    fs::copy(root.join("sample.accdb"), &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::Access
    );
}

#[test]
fn converts_3dxml_manifest_and_product_structure_without_decoding_reps() {
    let temporary = TempDir::new().unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.3dxml");
    let output = temporary.path().join("3dxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::ThreeDXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Dassault 3DXML package"));
    assert!(svg.contains("Reference3D"));
    assert!(svg.contains("ProductStructure.3dxml"));
    assert!(!svg.contains("opaque tessellation"));
    let extensionless = temporary.path().join("cad-package");
    fs::copy(&input, &extensionless).unwrap();
    assert_eq!(
        SourceFormat::detect(&extensionless).unwrap(),
        SourceFormat::ThreeDXml
    );
}
