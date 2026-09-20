//! Bounded DICOM Part 10 image, media-file-set and Structured Report previews.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use dicom_encoding::adapters::PixelDataObject;
use dicom_object::file::ReadPreamble;
use dicom_object::{DicomAttribute as _, DicomObject as _, OpenFileOptions, Tag};
use dicom_parser::dataset::read::{DataSetReaderOptions, ValueReadStrategy};
use dicom_parser::dataset::{DataSetReader, DataToken};
use dicom_pixeldata::PixelDecoder;
use dicom_pixeldata::image::ImageFormat;
use dicom_transfer_syntax_registry::{TransferSyntaxIndex as _, TransferSyntaxRegistry};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, SourceMeta};
use crate::jpeg2000::parse_raw_codestream;
use crate::table::{TableAlign, TableData};

const MAX_DICOM_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DICOM_FRAME_PIXELS: u64 = 20_000_000;
const MAX_DICOM_TOTAL_PIXELS: u64 = 100_000_000;
const MAX_DICOM_DECODED_SAMPLE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DICOM_FRAME_PNG_BYTES: usize = 64 * 1024 * 1024;
const MAX_DICOM_TOTAL_DATA_URI_BYTES: usize = 512 * 1024 * 1024;
const MAX_DICOM_DIMENSION: u32 = 100_000;
const MAX_DICOM_FRAMES: u32 = 1024;
const MAX_DICOM_FILE_META_BYTES: usize = 1024 * 1024;
const MAX_DICOM_METADATA_VALUE_BYTES: u32 = 8 * 1024 * 1024;
const MAX_DICOM_METADATA_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DICOM_ELEMENTS: usize = 100_000;
const MAX_DICOM_STRUCTURE_TOKENS: usize = 500_000;
const MAX_DICOM_SEQUENCE_DEPTH: usize = 64;
const MAX_DICOMDIR_RECORDS: usize = 100_000;
const MAX_DICOMDIR_IMAGE_REFERENCES: usize = 20_000;
const MAX_DICOMDIR_TOTAL_INPUT_BYTES: u64 = 512 * 1024 * 1024;
const DICOM_DIRECTORY_STORAGE_UID: &str = "1.2.840.10008.1.3.10";
const ENCAPSULATED_PDF_STORAGE_UID: &str = "1.2.840.10008.5.1.4.1.1.104.1";
const DICOM_SR_SOP_CLASS_UIDS: &[&str] = &[
    "1.2.840.10008.5.1.4.1.1.88.11", // Basic Text SR
    "1.2.840.10008.5.1.4.1.1.88.22", // Enhanced SR
    "1.2.840.10008.5.1.4.1.1.88.33", // Comprehensive SR
    "1.2.840.10008.5.1.4.1.1.88.34", // Comprehensive 3D SR
    "1.2.840.10008.5.1.4.1.1.88.35", // Extensible SR
    "1.2.840.10008.5.1.4.1.1.88.40", // Procedure Log
    "1.2.840.10008.5.1.4.1.1.88.50", // Mammography CAD SR
    "1.2.840.10008.5.1.4.1.1.88.65", // Chest CAD SR
    "1.2.840.10008.5.1.4.1.1.88.67", // X-Ray Radiation Dose SR
    "1.2.840.10008.5.1.4.1.1.88.68", // Radiopharmaceutical Radiation Dose SR
    "1.2.840.10008.5.1.4.1.1.88.69", // Colon CAD SR
    "1.2.840.10008.5.1.4.1.1.88.70", // Implantation Plan SR
    "1.2.840.10008.5.1.4.1.1.88.71", // Acquisition Context SR
    "1.2.840.10008.5.1.4.1.1.88.72", // Simplified Adult Echo SR
    "1.2.840.10008.5.1.4.1.1.88.73", // Patient Radiation Dose SR
    "1.2.840.10008.5.1.4.1.1.88.74", // Planned Imaging Agent Administration SR
    "1.2.840.10008.5.1.4.1.1.88.75", // Performed Imaging Agent Administration SR
    "1.2.840.10008.5.1.4.1.1.88.76", // Enhanced X-Ray Radiation Dose SR
    "1.2.840.10008.5.1.4.1.1.88.77", // Waveform Annotation SR
];
const MAX_DICOM_SR_ROWS: usize = 200_000;
const MAX_DICOM_SR_DEPTH: usize = 64;
const MAX_DICOM_SR_DISPLAY_BYTES: usize = 512;
const MAX_DICOM_EMBEDDED_PDF_BYTES: u32 = 64 * 1024 * 1024;
const EXPLICIT_VR_LITTLE_ENDIAN_UID: &str = "1.2.840.10008.1.2.1";
const JPEG_2000_LOSSLESS_UID: &str = "1.2.840.10008.1.2.4.90";
const JPEG_2000_UID: &str = "1.2.840.10008.1.2.4.91";
const JPEG_2000_UNSUPPORTED_UIDS: &[&str] = &[
    "1.2.840.10008.1.2.4.92",
    "1.2.840.10008.1.2.4.93",
    "1.2.840.10008.1.2.4.201",
    "1.2.840.10008.1.2.4.202",
    "1.2.840.10008.1.2.4.203",
];
type ExplicitVRElement<'a> = (u16, u16, [u8; 2], &'a [u8], usize);

struct DicomFileMeta {
    media_storage_sop_class_uid: String,
    transfer_syntax_uid: String,
    dataset_offset: usize,
}

struct DicomDirectoryRecord {
    offset: u32,
    next_offset: u32,
    lower_level_offset: u32,
    in_use: bool,
    record_type: String,
    file_id: Option<Vec<String>>,
}

struct DicomDirectoryPageSink<'a> {
    downstream: &'a mut dyn PageConsumer,
    page_count: usize,
    warnings: Vec<String>,
}

struct DicomPdfPageSink<'a> {
    downstream: &'a mut dyn PageConsumer,
}

struct DicomSrPageSink<'a> {
    downstream: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for DicomDirectoryPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        let number = self.page_count.saturating_add(1);
        page.number = number;
        page.source_format = "dicomdir".into();
        page.title = format!("DICOMDIR Image {number}");
        page.description = format!(
            "DICOMDIR image page {number} ({}x{})",
            page.width as u32, page.height as u32
        );
        for warning in &self.warnings {
            page.warn(warning.clone());
        }
        self.downstream.consume(page)?;
        self.page_count = number;
        Ok(())
    }
}

impl PageConsumer for DicomPdfPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "dicom".into();
        page.title = format!("DICOM Encapsulated PDF — {}", page.title);
        page.warn("DICOM patient/study attributes are not added to the SVG; identifying content already present inside the embedded PDF is preserved, so this preview is not de-identification");
        self.downstream.consume(page)
    }
}

impl PageConsumer for DicomSrPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "dicom-sr".into();
        page.title = "DICOM Structured Report".into();
        page.description =
            "DICOM Structured Reporting content is rendered as a bounded inert table; no clinical workflow or external reference is executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.downstream.consume(page)
    }
}

pub(crate) fn looks_like_dicom_prefix(bytes: &[u8]) -> bool {
    bytes.get(128..132) == Some(b"DICM")
}

pub(crate) fn looks_like_dicomdir_prefix(bytes: &[u8]) -> bool {
    looks_like_dicom_prefix(bytes)
        && parse_file_meta(bytes)
            .is_ok_and(|meta| meta.media_storage_sop_class_uid == DICOM_DIRECTORY_STORAGE_UID)
}

pub(crate) fn looks_like_dicom_sr_prefix(bytes: &[u8]) -> bool {
    looks_like_dicom_prefix(bytes)
        && parse_file_meta(bytes)
            .is_ok_and(|meta| is_dicom_sr_sop_class(&meta.media_storage_sop_class_uid))
}

fn is_dicom_sr_sop_class(uid: &str) -> bool {
    DICOM_SR_SOP_CLASS_UIDS.contains(&uid)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_DICOM_INPUT_BYTES),
        "DICOM input",
    )?;
    if !looks_like_dicom_prefix(&bytes) {
        return Err(Error::Unsupported(
            "only DICOM Part 10 files with the 128-byte preamble and DICM prefix are supported"
                .into(),
        ));
    }

    let file_meta = parse_file_meta(&bytes)?;
    if file_meta.media_storage_sop_class_uid == DICOM_DIRECTORY_STORAGE_UID {
        return Err(Error::Unsupported(
            "DICOMDIR must be converted as a media file set, not as an image object".into(),
        ));
    }
    let encapsulated_pdf = file_meta.media_storage_sop_class_uid == ENCAPSULATED_PDF_STORAGE_UID;
    let structured_report = is_dicom_sr_sop_class(&file_meta.media_storage_sop_class_uid);
    preflight_dataset_structure(
        &bytes[file_meta.dataset_offset..],
        &file_meta.transfer_syntax_uid,
        !encapsulated_pdf && !structured_report,
        encapsulated_pdf.then_some(MAX_DICOM_EMBEDDED_PDF_BYTES),
    )?;

    let object = OpenFileOptions::new()
        .read_preamble(ReadPreamble::Always)
        .from_reader(Cursor::new(bytes.as_slice()))
        .map_err(|error| Error::InvalidInput(format!("invalid DICOM file: {error}")))?;
    drop(bytes);

    let dataset_sop_class_uid = required_text(&object, Tag(0x0008, 0x0016), "SOP Class UID")?;
    if dataset_sop_class_uid != file_meta.media_storage_sop_class_uid {
        return Err(Error::InvalidInput(
            "DICOM Media Storage SOP Class UID does not match the data set SOP Class UID".into(),
        ));
    }

    if encapsulated_pdf {
        return convert_encapsulated_pdf(&object, options, sink);
    }
    if structured_report {
        return convert_structured_report(&object, options, sink, &dataset_sop_class_uid);
    }

    let rows = required_u16(&object, Tag(0x0028, 0x0010), "Rows")? as u32;
    let columns = required_u16(&object, Tag(0x0028, 0x0011), "Columns")? as u32;
    let samples = required_u16(&object, Tag(0x0028, 0x0002), "Samples per Pixel")?;
    let bits_allocated = required_u16(&object, Tag(0x0028, 0x0100), "Bits Allocated")?;
    let bits_stored = required_u16(&object, Tag(0x0028, 0x0101), "Bits Stored")?;
    let high_bit = required_u16(&object, Tag(0x0028, 0x0102), "High Bit")?;
    let pixel_representation = required_u16(&object, Tag(0x0028, 0x0103), "Pixel Representation")?;
    let photometric = required_text(&object, Tag(0x0028, 0x0004), "Photometric Interpretation")?;
    let frames = optional_frame_count(&object)?;

    if rows == 0 || columns == 0 || rows > MAX_DICOM_DIMENSION || columns > MAX_DICOM_DIMENSION {
        return Err(Error::LimitExceeded(format!(
            "DICOM image dimensions {columns}x{rows} are outside the supported range"
        )));
    }
    if !matches!(samples, 1 | 3) {
        return Err(Error::Unsupported(format!(
            "DICOM Samples per Pixel {samples} is unsupported; only grayscale or RGB images are rendered"
        )));
    }
    if !matches!(bits_allocated, 8 | 16)
        || bits_stored == 0
        || bits_stored > bits_allocated
        || high_bit != bits_stored - 1
        || pixel_representation > 1
    {
        return Err(Error::Unsupported(
            "DICOM pixel depth or representation is unsupported; expected 8/16-bit integer samples"
                .into(),
        ));
    }
    if samples == 3 && required_u16(&object, Tag(0x0028, 0x0006), "Planar Configuration")? > 1 {
        return Err(Error::InvalidInput(
            "DICOM Planar Configuration must be 0 or 1".into(),
        ));
    }
    if !matches!(
        photometric.as_str(),
        "MONOCHROME1" | "MONOCHROME2" | "RGB" | "YBR_FULL" | "YBR_FULL_422"
    ) {
        return Err(Error::Unsupported(format!(
            "DICOM Photometric Interpretation '{photometric}' is unsupported"
        )));
    }
    if (samples == 1) != photometric.starts_with("MONOCHROME") {
        return Err(Error::InvalidInput(
            "DICOM Samples per Pixel and Photometric Interpretation disagree".into(),
        ));
    }
    if frames == 0 || frames > MAX_DICOM_FRAMES {
        return Err(Error::LimitExceeded(format!(
            "DICOM Number of Frames {frames} is outside the supported range 1..={MAX_DICOM_FRAMES}"
        )));
    }
    if frames as usize > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "DICOM contains {frames} frames, exceeding the page limit {}",
            options.max_pages
        )));
    }

    let pixels_per_frame = u64::from(rows)
        .checked_mul(u64::from(columns))
        .ok_or_else(|| Error::LimitExceeded("DICOM pixel count overflowed".into()))?;
    if pixels_per_frame > MAX_DICOM_FRAME_PIXELS {
        return Err(Error::LimitExceeded(format!(
            "DICOM frame contains {pixels_per_frame} pixels; maximum is {MAX_DICOM_FRAME_PIXELS}"
        )));
    }
    let total_pixels = pixels_per_frame
        .checked_mul(u64::from(frames))
        .ok_or_else(|| Error::LimitExceeded("DICOM total pixel count overflowed".into()))?;
    if total_pixels > MAX_DICOM_TOTAL_PIXELS {
        return Err(Error::LimitExceeded(format!(
            "DICOM frames contain {total_pixels} pixels; maximum is {MAX_DICOM_TOTAL_PIXELS}"
        )));
    }
    let sample_bytes = pixels_per_frame
        .checked_mul(u64::from(frames))
        .and_then(|count| count.checked_mul(u64::from(samples)))
        .and_then(|count| count.checked_mul(u64::from(bits_allocated / 8)))
        .ok_or_else(|| Error::LimitExceeded("DICOM decoded sample size overflowed".into()))?;
    if sample_bytes > MAX_DICOM_DECODED_SAMPLE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "DICOM decoded samples require {sample_bytes} bytes; maximum is {MAX_DICOM_DECODED_SAMPLE_BYTES}"
        )));
    }

    if JPEG_2000_UNSUPPORTED_UIDS.contains(&file_meta.transfer_syntax_uid.as_str()) {
        return Err(Error::Unsupported(format!(
            "DICOM JPEG 2000 Part 2 and High-Throughput JPEG 2000 transfer syntaxes are not supported ({})",
            file_meta.transfer_syntax_uid
        )));
    }
    if matches!(
        file_meta.transfer_syntax_uid.as_str(),
        JPEG_2000_LOSSLESS_UID | JPEG_2000_UID
    ) {
        preflight_jpeg2000_frames(
            &object,
            frames,
            columns,
            rows,
            samples,
            bits_stored,
            pixel_representation,
        )?;
    }

    let has_icc_profile = object
        .attr_opt(Tag(0x0028, 0x2000))
        .map_err(|error| {
            Error::InvalidInput(format!("invalid DICOM ICC profile attribute: {error}"))
        })?
        .is_some();
    let decoded = object.decode_pixel_data().map_err(|error| {
        Error::Unsupported(format!("DICOM pixel data cannot be decoded: {error}"))
    })?;
    if decoded.rows() != rows
        || decoded.columns() != columns
        || decoded.number_of_frames() != frames
    {
        return Err(Error::InvalidInput(
            "DICOM decoded image dimensions do not match the declared attributes".into(),
        ));
    }
    let image_options = dicom_pixeldata::ConvertOptions::new().force_8bit();
    let mut warnings = vec![
        "DICOM patient/study metadata is not written to SVG; burned-in pixel annotations are not removed, so this preview is not de-identification".into(),
        "DICOM patient orientation and physical pixel spacing are not applied; frames retain their stored pixel orientation and size".into(),
    ];
    if bits_allocated > 8 {
        warnings.push("DICOM samples are converted to 8-bit display images using the available Modality/VOI transformations".into());
    }
    if frames > 1 {
        warnings.push(format!(
            "DICOM multi-frame image was emitted as {frames} sequential SVG pages"
        ));
    }
    if has_icc_profile {
        warnings.push("DICOM ICC color profile is not applied".into());
    }

    let mut total_uri_bytes = 0usize;
    for frame in 0..frames {
        let image = decoded
            .to_dynamic_image_with_options(frame, &image_options)
            .map_err(|error| {
                Error::InvalidInput(format!(
                    "DICOM frame {} cannot be converted to an image: {error}",
                    frame + 1
                ))
            })?;
        let width = image.width();
        let height = image.height();
        let mut png = Cursor::new(Vec::new());
        image
            .write_to(&mut png, ImageFormat::Png)
            .map_err(|error| {
                Error::InvalidInput(format!(
                    "DICOM frame {} PNG encoding failed: {error}",
                    frame + 1
                ))
            })?;
        let png = png.into_inner();
        if png.len() > MAX_DICOM_FRAME_PNG_BYTES {
            return Err(Error::LimitExceeded(format!(
                "DICOM frame {} PNG exceeds {MAX_DICOM_FRAME_PNG_BYTES} bytes",
                frame + 1
            )));
        }
        let data_uri = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png));
        total_uri_bytes = total_uri_bytes.checked_add(data_uri.len()).ok_or_else(|| {
            Error::LimitExceeded("DICOM total image data URI size overflowed".into())
        })?;
        if total_uri_bytes > MAX_DICOM_TOTAL_DATA_URI_BYTES {
            return Err(Error::LimitExceeded(format!(
                "DICOM frames exceed {MAX_DICOM_TOTAL_DATA_URI_BYTES} bytes of embedded PNG data"
            )));
        }

        let page_number = frame as usize + 1;
        let mut page = Page::new(page_number, f64::from(width), f64::from(height), "dicom");
        page.title = format!("DICOM Image Frame {page_number}");
        page.description = format!("DICOM image frame {page_number} ({width}x{height})");
        for warning in &warnings {
            page.warn(warning.clone());
        }
        page.nodes.push(Node::Image {
            id: format!("dicom-frame-{page_number}"),
            href: data_uri,
            x: 0.0,
            y: 0.0,
            width: f64::from(width),
            height: f64::from(height),
            transform: IDENTITY,
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "dicom:image-frame".into(),
                ..Default::default()
            },
        });
        sink.consume(page)?;
    }
    Ok(warnings)
}

fn preflight_jpeg2000_frames(
    object: &impl PixelDataObject,
    frames: u32,
    columns: u32,
    rows: u32,
    samples: u16,
    bits_stored: u16,
    pixel_representation: u16,
) -> Result<()> {
    for frame in 0..frames {
        let frame_data = object.frame_pixel_data(frame).ok_or_else(|| {
            Error::InvalidInput(format!(
                "DICOM JPEG 2000 frame {} could not be assembled from its fragments",
                frame + 1
            ))
        })?;
        let header = parse_raw_codestream(&frame_data)?;
        let image_pixels = u64::from(header.width)
            .checked_mul(u64::from(header.height))
            .ok_or_else(|| Error::LimitExceeded("JPEG 2000 pixel count overflowed".into()))?;
        if image_pixels > MAX_DICOM_FRAME_PIXELS {
            return Err(Error::LimitExceeded(format!(
                "DICOM JPEG 2000 frame contains {image_pixels} pixels; maximum is {MAX_DICOM_FRAME_PIXELS}"
            )));
        }
        if header.width != columns || header.height != rows {
            return Err(Error::InvalidInput(format!(
                "DICOM JPEG 2000 frame {} dimensions {}x{} do not match declared {}x{}",
                frame + 1,
                header.width,
                header.height,
                columns,
                rows
            )));
        }
        if header.components.len() != usize::from(samples) {
            return Err(Error::InvalidInput(format!(
                "DICOM JPEG 2000 frame {} has {} codestream components; expected {samples}",
                frame + 1,
                header.components.len()
            )));
        }
        for component in &header.components {
            if component.width != columns || component.height != rows {
                return Err(Error::Unsupported(
                    "subsampled DICOM JPEG 2000 components are not supported".into(),
                ));
            }
            if component.precision != bits_stored || component.signed != (pixel_representation == 1)
            {
                return Err(Error::InvalidInput(
                    "DICOM JPEG 2000 component precision or signedness does not match Image Pixel attributes"
                        .into(),
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn convert_directory(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_DICOM_INPUT_BYTES),
        "DICOMDIR input",
    )?;
    if !looks_like_dicom_prefix(&bytes) {
        return Err(Error::Unsupported(
            "DICOMDIR must be a DICOM Part 10 file with the DICM prefix".into(),
        ));
    }
    let file_meta = parse_file_meta(&bytes)?;
    if file_meta.media_storage_sop_class_uid != DICOM_DIRECTORY_STORAGE_UID {
        return Err(Error::Unsupported(
            "file does not contain the DICOM Media Storage Directory SOP Class".into(),
        ));
    }
    if file_meta.transfer_syntax_uid != EXPLICIT_VR_LITTLE_ENDIAN_UID {
        return Err(Error::Unsupported(
            "DICOMDIR requires Explicit VR Little Endian according to PS3.10".into(),
        ));
    }
    preflight_dataset_structure(
        &bytes[file_meta.dataset_offset..],
        &file_meta.transfer_syntax_uid,
        false,
        None,
    )?;
    let record_offsets = collect_directory_record_offsets(
        &bytes[file_meta.dataset_offset..],
        file_meta.dataset_offset,
        &file_meta.transfer_syntax_uid,
    )?;

    let object = OpenFileOptions::new()
        .read_preamble(ReadPreamble::Always)
        .from_reader(Cursor::new(bytes.as_slice()))
        .map_err(|error| Error::InvalidInput(format!("invalid DICOMDIR file: {error}")))?;
    drop(bytes);

    let dataset_sop_class_uid = required_text(&object, Tag(0x0008, 0x0016), "SOP Class UID")?;
    if dataset_sop_class_uid != DICOM_DIRECTORY_STORAGE_UID {
        return Err(Error::InvalidInput(
            "DICOMDIR Media Storage SOP Class UID does not match its data set".into(),
        ));
    }
    let sequence = object.attr(Tag(0x0004, 0x1220)).map_err(|error| {
        Error::InvalidInput(format!(
            "DICOMDIR Directory Record Sequence is missing: {error}"
        ))
    })?;
    let item_count = sequence.num_items().ok_or_else(|| {
        Error::InvalidInput("DICOMDIR Directory Record Sequence is not a sequence".into())
    })? as usize;
    if item_count == 0 || item_count > MAX_DICOMDIR_RECORDS || item_count != record_offsets.len() {
        return Err(Error::InvalidInput(format!(
            "DICOMDIR Directory Record Sequence has {item_count} items, but {} item offsets were parsed",
            record_offsets.len()
        )));
    }
    let mut records = Vec::with_capacity(item_count);
    for (index, offset) in record_offsets.iter().copied().enumerate() {
        let item = sequence.item(index as u32).map_err(|error| {
            Error::InvalidInput(format!("DICOMDIR record {} is invalid: {error}", index + 1))
        })?;
        let item = item.right().ok_or_else(|| {
            Error::InvalidInput(format!(
                "DICOMDIR record {} is not a sequence item",
                index + 1
            ))
        })?;
        let next_offset = required_u32(
            &item,
            Tag(0x0004, 0x1400),
            "Offset of Next Directory Record",
        )?;
        let record_in_use = required_u16(&item, Tag(0x0004, 0x1410), "Record In-use Flag")?;
        let lower_level_offset = required_u32(
            &item,
            Tag(0x0004, 0x1420),
            "Offset of Referenced Lower-Level Directory Entity",
        )?;
        if !matches!(record_in_use, 0 | 0xffff) {
            return Err(Error::InvalidInput(format!(
                "DICOMDIR record {} has an invalid In-use flag",
                index + 1
            )));
        }
        let record_type = required_text(&item, Tag(0x0004, 0x1430), "Directory Record Type")?;
        let file_id = if record_in_use == 0xffff && record_type == "IMAGE" {
            Some(read_file_id(&item, index + 1)?)
        } else {
            None
        };
        records.push(DicomDirectoryRecord {
            offset,
            next_offset,
            lower_level_offset,
            in_use: record_in_use == 0xffff,
            record_type,
            file_id,
        });
    }

    let root_offset = required_u32(
        &object,
        Tag(0x0004, 0x1200),
        "Offset of First Directory Record of the Root Directory Entity",
    )?;
    let (ordered_images, unreferenced_records) = ordered_image_records(&records, root_offset)?;
    if ordered_images.len() > MAX_DICOMDIR_IMAGE_REFERENCES {
        return Err(Error::LimitExceeded(format!(
            "DICOMDIR has {} image references; maximum is {MAX_DICOMDIR_IMAGE_REFERENCES}",
            ordered_images.len()
        )));
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let root_directory = fs::canonicalize(parent)?;
    let mut seen_paths = HashSet::new();
    let mut total_referenced_bytes = 0u64;
    let mut warnings = vec![
        "DICOMDIR hierarchy is followed to order referenced images; patient/study/series labels and directory metadata are omitted".into(),
        "DICOMDIR File IDs are resolved within the DICOMDIR folder; paths that leave the file set are refused".into(),
    ];
    if unreferenced_records > 0 {
        warnings.push(format!(
            "{unreferenced_records} unlinked DICOMDIR directory record(s) were omitted"
        ));
    }
    let mut skipped_files = 0usize;
    let mut duplicate_files = 0usize;
    let mut page_sink = DicomDirectoryPageSink {
        downstream: sink,
        page_count: 0,
        warnings: warnings.clone(),
    };
    for record_index in ordered_images {
        let file_id = records[record_index].file_id.as_deref().ok_or_else(|| {
            Error::InvalidInput("active DICOMDIR IMAGE record is missing File ID".into())
        })?;
        let Some(file_path) = resolve_file_id(&root_directory, file_id)? else {
            skipped_files = skipped_files.saturating_add(1);
            continue;
        };
        if !seen_paths.insert(file_path.clone()) {
            duplicate_files = duplicate_files.saturating_add(1);
            continue;
        }
        let file_bytes = fs::metadata(&file_path)?.len();
        total_referenced_bytes = total_referenced_bytes.saturating_add(file_bytes);
        if total_referenced_bytes > MAX_DICOMDIR_TOTAL_INPUT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "DICOMDIR referenced files exceed {MAX_DICOMDIR_TOTAL_INPUT_BYTES} bytes"
            )));
        }
        let remaining_pages = options.max_pages.saturating_sub(page_sink.page_count);
        if remaining_pages == 0 {
            return Err(Error::LimitExceeded(format!(
                "DICOMDIR output exceeds the {}-page limit",
                options.max_pages
            )));
        }
        let child_options = ConvertOptions {
            max_pages: remaining_pages,
            ..options.clone()
        };
        match convert(&file_path, &child_options, &mut page_sink) {
            Ok(child_warnings) => {
                for warning in child_warnings {
                    if !warnings.contains(&warning) {
                        warnings.push(warning);
                    }
                }
            }
            Err(Error::LimitExceeded(message)) => return Err(Error::LimitExceeded(message)),
            Err(Error::Io(_)) | Err(Error::InvalidInput(_)) | Err(Error::Unsupported(_)) => {
                skipped_files = skipped_files.saturating_add(1);
            }
            Err(error) => return Err(error),
        }
    }
    if skipped_files > 0 {
        warnings.push(format!(
            "{skipped_files} missing or unsupported DICOMDIR image file(s) were omitted"
        ));
    }
    if duplicate_files > 0 {
        warnings.push(format!(
            "{duplicate_files} duplicate DICOMDIR File ID reference(s) were omitted"
        ));
    }
    if page_sink.page_count == 0 {
        return Err(Error::InvalidInput(
            "DICOMDIR contains no readable referenced image frames".into(),
        ));
    }
    Ok(warnings)
}

fn required_u16<O: dicom_object::DicomObject>(object: &O, tag: Tag, label: &str) -> Result<u16> {
    object
        .attr(tag)
        .map_err(|error| {
            Error::InvalidInput(format!(
                "DICOM {label} (0x{:04X},0x{:04X}) is missing: {error}",
                tag.0, tag.1
            ))
        })?
        .to_u16()
        .map_err(|error| Error::InvalidInput(format!("DICOM {label} is invalid: {error}")))
}

fn required_text<O: dicom_object::DicomObject>(
    object: &O,
    tag: Tag,
    label: &str,
) -> Result<String> {
    object
        .attr(tag)
        .map_err(|error| {
            Error::InvalidInput(format!(
                "DICOM {label} (0x{:04X},0x{:04X}) is missing: {error}",
                tag.0, tag.1
            ))
        })?
        .to_str()
        .map(|value| value.trim().to_string())
        .map_err(|error| Error::InvalidInput(format!("DICOM {label} is invalid: {error}")))
}

fn required_u32<O: dicom_object::DicomObject>(object: &O, tag: Tag, label: &str) -> Result<u32> {
    object
        .attr(tag)
        .map_err(|error| {
            Error::InvalidInput(format!(
                "DICOM {label} (0x{:04X},0x{:04X}) is missing: {error}",
                tag.0, tag.1
            ))
        })?
        .to_u32()
        .map_err(|error| Error::InvalidInput(format!("DICOM {label} is invalid: {error}")))
}

fn optional_frame_count(object: &dicom_object::DefaultDicomObject) -> Result<u32> {
    let Some(value) = object.attr_opt(Tag(0x0028, 0x0008)).map_err(|error| {
        Error::InvalidInput(format!("DICOM Number of Frames is invalid: {error}"))
    })?
    else {
        return Ok(1);
    };
    let text = value.to_str().map_err(|error| {
        Error::InvalidInput(format!("DICOM Number of Frames is invalid: {error}"))
    })?;
    text.trim()
        .parse::<u32>()
        .map_err(|error| Error::InvalidInput(format!("DICOM Number of Frames is invalid: {error}")))
}

fn convert_encapsulated_pdf(
    object: &dicom_object::DefaultDicomObject,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mime_type = required_text(
        object,
        Tag(0x0042, 0x0012),
        "MIME Type of Encapsulated Document",
    )?;
    if !mime_type.eq_ignore_ascii_case("application/pdf") {
        return Err(Error::Unsupported(format!(
            "DICOM Encapsulated PDF Storage declares unsupported MIME type '{mime_type}'"
        )));
    }

    let doc_attr = object.attr(Tag(0x0042, 0x0011)).map_err(|error| {
        Error::InvalidInput(format!(
            "DICOM Encapsulated Document (0042,0011) is missing: {error}"
        ))
    })?;
    let document = doc_attr.to_bytes().map_err(|error| {
        Error::InvalidInput(format!("DICOM Encapsulated Document is invalid: {error}"))
    })?;
    let declared_length = object
        .attr_opt(Tag(0x0042, 0x0015))
        .map_err(|error| {
            Error::InvalidInput(format!(
                "DICOM Encapsulated Document Length is invalid: {error}"
            ))
        })?
        .map(|value| {
            value.to_u32().map_err(|error| {
                Error::InvalidInput(format!(
                    "DICOM Encapsulated Document Length is invalid: {error}"
                ))
            })
        })
        .transpose()?;

    let pdf_bytes = if let Some(length) = declared_length {
        let length = usize::try_from(length).map_err(|_| {
            Error::LimitExceeded("DICOM Encapsulated Document Length overflows".into())
        })?;
        if length > MAX_DICOM_EMBEDDED_PDF_BYTES as usize {
            return Err(Error::LimitExceeded(format!(
                "DICOM Encapsulated PDF exceeds {MAX_DICOM_EMBEDDED_PDF_BYTES} bytes"
            )));
        }
        match document.len().checked_sub(length) {
            Some(0) => &document[..length],
            Some(1) if length % 2 == 1 && document[length] == 0 => &document[..length],
            _ => {
                return Err(Error::InvalidInput(
                    "DICOM Encapsulated Document Length does not match its value, allowing only the required zero pad byte".into(),
                ));
            }
        }
    } else {
        if document.len() > MAX_DICOM_EMBEDDED_PDF_BYTES as usize {
            return Err(Error::LimitExceeded(format!(
                "DICOM Encapsulated PDF exceeds {MAX_DICOM_EMBEDDED_PDF_BYTES} bytes"
            )));
        }
        &document
    };
    if !pdf_bytes[..pdf_bytes.len().min(1024)]
        .windows(5)
        .any(|window| window == b"%PDF-")
    {
        return Err(Error::InvalidInput(
            "DICOM Encapsulated Document is not a PDF stream".into(),
        ));
    }

    let mut page_sink = DicomPdfPageSink { downstream: sink };
    let mut warnings = crate::pdf::convert_bytes(pdf_bytes, options, &mut page_sink)?;
    warnings.push("DICOM patient/study attributes are not added to SVG; identifying content already inside the embedded PDF is preserved, so this preview is not de-identification".into());
    Ok(warnings)
}

#[derive(Default)]
struct DicomSrSummary {
    items: usize,
    max_depth: usize,
    containers: usize,
    value_types: BTreeMap<String, usize>,
    rows: Vec<Vec<String>>,
}

fn convert_structured_report(
    object: &dicom_object::DefaultDicomObject,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    sop_class_uid: &str,
) -> Result<Vec<String>> {
    const CONTENT_SEQUENCE: Tag = Tag(0x0040, 0xa730);
    let content = object
        .attr_opt(CONTENT_SEQUENCE)
        .map_err(|error| {
            Error::InvalidInput(format!("DICOM SR Content Sequence is invalid: {error}"))
        })?
        .ok_or_else(|| Error::InvalidInput("DICOM SR Content Sequence is missing".into()))?;
    let item_count = content
        .num_items()
        .ok_or_else(|| Error::InvalidInput("DICOM SR Content Sequence is not a sequence".into()))?
        as usize;
    if item_count == 0 {
        return Err(Error::InvalidInput(
            "DICOM SR Content Sequence contains no content items".into(),
        ));
    }

    let mut summary = DicomSrSummary::default();
    for index in 0..item_count {
        let item = content.item(index as u32).map_err(|error| {
            Error::InvalidInput(format!("DICOM SR content item {index} is invalid: {error}"))
        })?;
        let item = item.right().ok_or_else(|| {
            Error::InvalidInput(format!("DICOM SR content item {index} is not a data set"))
        })?;
        walk_sr_item(item, 1, &mut summary)?;
    }

    let completion = sr_text(object, Tag(0x0040, 0xa491))?.unwrap_or_else(|| "-".into());
    let verification = sr_text(object, Tag(0x0040, 0xa493))?.unwrap_or_else(|| "-".into());
    let value_types = summary
        .value_types
        .iter()
        .map(|(kind, count)| format!("{kind}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    let metadata = format!(
        "SOP class: {}\nCompletion: {completion}\nVerification: {verification}\nContent items: {}\nMaximum content depth: {}\nValue types: {}",
        sr_sop_class_name(sop_class_uid),
        summary.items,
        summary.max_depth,
        display_or_dash(&value_types),
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "DICOM Structured Report".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Depth".into(),
                "Type".into(),
                "Concept".into(),
                "Value".into(),
                "Rel.".into(),
            ],
            rows: summary.rows,
            alignments: vec![
                TableAlign::Right,
                TableAlign::Left,
                TableAlign::Left,
                TableAlign::Left,
                TableAlign::Left,
            ],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "DICOM Structured Report content items, coded concepts, numeric values and document status are shown; patient/study attributes, referenced SOP Instance UIDs, image/audio bodies and other identifiers are omitted".into(),
        "DICOM SR content can contain protected health information and this preview is not de-identification; content traversal is bounded and no clinical workflow, URI dereference, script or network operation runs".into(),
    ];
    let mut page_sink = DicomSrPageSink {
        downstream: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn walk_sr_item(
    item: &dicom_object::InMemDicomObject,
    depth: usize,
    summary: &mut DicomSrSummary,
) -> Result<()> {
    const VALUE_TYPE: Tag = Tag(0x0040, 0xa040);
    const RELATIONSHIP_TYPE: Tag = Tag(0x0040, 0xa010);
    const CONCEPT_NAME_SEQUENCE: Tag = Tag(0x0040, 0xa043);
    const CONTENT_SEQUENCE: Tag = Tag(0x0040, 0xa730);
    const UNITS_SEQUENCE: Tag = Tag(0x0040, 0x08ea);

    if depth > MAX_DICOM_SR_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "DICOM SR content depth exceeds {MAX_DICOM_SR_DEPTH}"
        )));
    }
    if summary.items >= MAX_DICOM_SR_ROWS {
        return Err(Error::LimitExceeded(format!(
            "DICOM SR content items exceed {MAX_DICOM_SR_ROWS}"
        )));
    }
    summary.items = summary.items.saturating_add(1);
    summary.max_depth = summary.max_depth.max(depth);

    let value_type = sr_text(item, VALUE_TYPE)?.unwrap_or_else(|| "UNKNOWN".into());
    *summary.value_types.entry(value_type.clone()).or_default() += 1;
    if value_type.eq_ignore_ascii_case("CONTAINER") {
        summary.containers = summary.containers.saturating_add(1);
    }
    let concept = sr_code(item, CONCEPT_NAME_SEQUENCE)?.unwrap_or_else(|| "[unnamed]".into());
    let value = sr_item_value(item, &value_type, UNITS_SEQUENCE)?;
    let relationship = sr_text(item, RELATIONSHIP_TYPE)?.unwrap_or_else(|| "-".into());
    summary.rows.push(vec![
        depth.to_string(),
        truncate_dicom_sr(&value_type),
        truncate_dicom_sr(&concept),
        truncate_dicom_sr(&value),
        truncate_dicom_sr(&relationship),
    ]);

    if let Some(sequence) = item.attr_opt(CONTENT_SEQUENCE).map_err(|error| {
        Error::InvalidInput(format!(
            "DICOM SR nested Content Sequence is invalid: {error}"
        ))
    })? {
        let nested_count = sequence.num_items().ok_or_else(|| {
            Error::InvalidInput("DICOM SR nested Content Sequence is not a sequence".into())
        })? as usize;
        for index in 0..nested_count {
            let child = sequence.item(index as u32).map_err(|error| {
                Error::InvalidInput(format!(
                    "DICOM SR nested content item {index} is invalid: {error}"
                ))
            })?;
            walk_sr_item(child, depth.saturating_add(1), summary)?;
        }
    }
    Ok(())
}

fn sr_text<O: dicom_object::DicomObject>(object: &O, tag: Tag) -> Result<Option<String>> {
    let Some(value) = object.attr_opt(tag).map_err(|error| {
        Error::InvalidInput(format!(
            "DICOM SR attribute (0x{:04X},0x{:04X}) is invalid: {error}",
            tag.0, tag.1
        ))
    })?
    else {
        return Ok(None);
    };
    let text = value.to_str().map_err(|error| {
        Error::InvalidInput(format!(
            "DICOM SR attribute (0x{:04X},0x{:04X}) is not text: {error}",
            tag.0, tag.1
        ))
    })?;
    let text = text.trim();
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(redact_dicom_sr_text(text)))
    }
}

fn sr_code(object: &dicom_object::InMemDicomObject, tag: Tag) -> Result<Option<String>> {
    let Some(value) = object.attr_opt(tag).map_err(|error| {
        Error::InvalidInput(format!("DICOM SR code sequence is invalid: {error}"))
    })?
    else {
        return Ok(None);
    };
    let items = value
        .items()
        .ok_or_else(|| Error::InvalidInput("DICOM SR code attribute is not a sequence".into()))?;
    let Some(item) = items.first() else {
        return Ok(None);
    };
    let meaning = sr_text(item, Tag(0x0008, 0x0104))?.unwrap_or_default();
    let value = sr_text(item, Tag(0x0008, 0x0100))?.unwrap_or_default();
    let designator = sr_text(item, Tag(0x0008, 0x0102))?.unwrap_or_default();
    if meaning.is_empty() && value.is_empty() {
        return Ok(None);
    }
    if !meaning.is_empty() {
        Ok(Some(meaning))
    } else if !designator.is_empty() {
        Ok(Some(format!("{designator}:{value}")))
    } else {
        Ok(Some(value))
    }
}

fn sr_item_value(
    item: &dicom_object::InMemDicomObject,
    value_type: &str,
    units_sequence: Tag,
) -> Result<String> {
    match value_type.to_ascii_uppercase().as_str() {
        "TEXT" => Ok(sr_text(item, Tag(0x0040, 0xa160))?.unwrap_or_else(|| "[empty text]".into())),
        "NUM" => {
            let number = sr_text(item, Tag(0x0040, 0xa30a))?
                .unwrap_or_else(|| "[missing numeric value]".into());
            let units = sr_code(item, units_sequence)?.unwrap_or_default();
            Ok(if units.is_empty() {
                number
            } else {
                format!("{number} {units}")
            })
        }
        "CODE" => Ok(sr_code(item, Tag(0x0040, 0xa168))?.unwrap_or_else(|| "[coded value]".into())),
        "CONTAINER" => Ok("[container]".into()),
        "DATE" => {
            Ok(sr_text(item, Tag(0x0040, 0xa121))?.unwrap_or_else(|| "[date omitted]".into()))
        }
        "DATETIME" => {
            Ok(sr_text(item, Tag(0x0040, 0xa120))?.unwrap_or_else(|| "[date-time omitted]".into()))
        }
        "TIME" => {
            Ok(sr_text(item, Tag(0x0040, 0xa122))?.unwrap_or_else(|| "[time omitted]".into()))
        }
        "SCOORD" | "SCOORD3D" | "IMAGE" | "COMPOSITE" | "WAVEFORM" | "TCOORD" => {
            Ok("[referenced object omitted]".into())
        }
        _ => Ok("[value omitted]".into()),
    }
}

fn sr_sop_class_name(uid: &str) -> &str {
    match uid {
        "1.2.840.10008.5.1.4.1.1.88.11" => "Basic Text SR",
        "1.2.840.10008.5.1.4.1.1.88.22" => "Enhanced SR",
        "1.2.840.10008.5.1.4.1.1.88.33" => "Comprehensive SR",
        "1.2.840.10008.5.1.4.1.1.88.34" => "Comprehensive 3D SR",
        "1.2.840.10008.5.1.4.1.1.88.35" => "Extensible SR",
        "1.2.840.10008.5.1.4.1.1.88.40" => "Procedure Log",
        "1.2.840.10008.5.1.4.1.1.88.50" => "Mammography CAD SR",
        "1.2.840.10008.5.1.4.1.1.88.65" => "Chest CAD SR",
        "1.2.840.10008.5.1.4.1.1.88.67" => "X-Ray Radiation Dose SR",
        "1.2.840.10008.5.1.4.1.1.88.68" => "Radiopharmaceutical Radiation Dose SR",
        "1.2.840.10008.5.1.4.1.1.88.69" => "Colon CAD SR",
        "1.2.840.10008.5.1.4.1.1.88.70" => "Implantation Plan SR",
        "1.2.840.10008.5.1.4.1.1.88.71" => "Acquisition Context SR",
        "1.2.840.10008.5.1.4.1.1.88.72" => "Simplified Adult Echo SR",
        "1.2.840.10008.5.1.4.1.1.88.73" => "Patient Radiation Dose SR",
        "1.2.840.10008.5.1.4.1.1.88.74" => "Planned Imaging Agent Administration SR",
        "1.2.840.10008.5.1.4.1.1.88.75" => "Performed Imaging Agent Administration SR",
        "1.2.840.10008.5.1.4.1.1.88.76" => "Enhanced X-Ray Radiation Dose SR",
        "1.2.840.10008.5.1.4.1.1.88.77" => "Waveform Annotation SR",
        _ => "DICOM Structured Report",
    }
}

fn truncate_dicom_sr(value: &str) -> String {
    if value.len() <= MAX_DICOM_SR_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_DICOM_SR_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn redact_dicom_sr_text(value: &str) -> String {
    let redacted = value
        .split_whitespace()
        .map(|token| {
            if token.contains("://") {
                "[URL omitted]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    truncate_dicom_sr(&redacted)
}

fn display_or_dash(value: &str) -> String {
    if value.is_empty() {
        "-".into()
    } else {
        truncate_dicom_sr(value)
    }
}

fn read_file_id<O: dicom_object::DicomObject>(
    record: &O,
    record_number: usize,
) -> Result<Vec<String>> {
    let components = record
        .attr(Tag(0x0004, 0x1500))
        .map_err(|error| {
            Error::InvalidInput(format!(
                "DICOMDIR IMAGE record {record_number} is missing Referenced File ID: {error}"
            ))
        })?
        .to_primitive_value()
        .map_err(|error| Error::InvalidInput(format!("invalid DICOMDIR File ID: {error}")))?
        .to_multi_str()
        .into_owned();
    validate_file_id_components(&components)?;
    Ok(components)
}

fn validate_file_id_components(components: &[String]) -> Result<()> {
    if components.is_empty() || components.len() > 8 {
        return Err(Error::InvalidInput(
            "DICOMDIR File ID must have one to eight path components".into(),
        ));
    }
    for component in components {
        if component.is_empty()
            || component.len() > 8
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(Error::InvalidInput(
                "DICOMDIR File ID component violates the PS3.10 character/length rules".into(),
            ));
        }
    }
    Ok(())
}

fn resolve_file_id(root: &Path, components: &[String]) -> Result<Option<PathBuf>> {
    validate_file_id_components(components)?;
    let mut candidate = root.to_path_buf();
    for component in components {
        candidate.push(component);
    }
    let canonical = match fs::canonicalize(&candidate) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !canonical.starts_with(root) {
        return Err(Error::InvalidInput(
            "DICOMDIR File ID resolves outside the file-set directory".into(),
        ));
    }
    if !fs::metadata(&canonical)?.is_file() {
        return Ok(None);
    }
    Ok(Some(canonical))
}

fn ordered_image_records(
    records: &[DicomDirectoryRecord],
    root_offset: u32,
) -> Result<(Vec<usize>, usize)> {
    if root_offset == 0 {
        return Err(Error::InvalidInput(
            "DICOMDIR Root Directory Entity offset is zero".into(),
        ));
    }
    let mut record_by_offset = HashMap::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        if record_by_offset.insert(record.offset, index).is_some() {
            return Err(Error::InvalidInput(
                "DICOMDIR contains duplicate directory record offsets".into(),
            ));
        }
    }
    let mut pending = vec![(root_offset, 0usize)];
    let mut visited = HashSet::with_capacity(records.len());
    let mut images = Vec::new();
    while let Some((offset, depth)) = pending.pop() {
        if depth > MAX_DICOM_SEQUENCE_DEPTH {
            return Err(Error::LimitExceeded(format!(
                "DICOMDIR hierarchy exceeds {MAX_DICOM_SEQUENCE_DEPTH} levels"
            )));
        }
        let index = *record_by_offset.get(&offset).ok_or_else(|| {
            Error::InvalidInput(format!(
                "DICOMDIR directory pointer {offset} does not identify a directory record"
            ))
        })?;
        if !visited.insert(offset) {
            return Err(Error::InvalidInput(
                "DICOMDIR record pointer graph contains a cycle or duplicate link".into(),
            ));
        }
        let record = &records[index];
        if record.in_use && record.record_type == "IMAGE" && record.file_id.is_some() {
            images.push(index);
            if images.len() > MAX_DICOMDIR_IMAGE_REFERENCES {
                return Err(Error::LimitExceeded(format!(
                    "DICOMDIR contains more than {MAX_DICOMDIR_IMAGE_REFERENCES} image records"
                )));
            }
        }
        // LIFO traversal: push the sibling first so child records render before it.
        if record.next_offset != 0 {
            pending.push((record.next_offset, depth));
        }
        if record.lower_level_offset != 0 {
            pending.push((record.lower_level_offset, depth.saturating_add(1)));
        }
    }
    Ok((images, records.len().saturating_sub(visited.len())))
}

struct PositionTrackingReader<'a> {
    inner: Cursor<&'a [u8]>,
    position: Rc<Cell<usize>>,
}

impl Read for PositionTrackingReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(output)?;
        self.position.set(self.position.get().saturating_add(count));
        Ok(count)
    }
}

fn parse_file_meta(bytes: &[u8]) -> Result<DicomFileMeta> {
    const PREFIX_END: usize = 132;
    if bytes.len() < PREFIX_END || !looks_like_dicom_prefix(bytes) {
        return Err(Error::InvalidInput(
            "DICOM Part 10 preamble or prefix is truncated".into(),
        ));
    }
    let (group, element, vr, value, after_group_length) =
        parse_explicit_vr_element(bytes, PREFIX_END)?;
    if (group, element, vr) != (0x0002, 0x0000, *b"UL") || value.len() != 4 {
        return Err(Error::InvalidInput(
            "DICOM File Meta Information must start with (0002,0000) UL".into(),
        ));
    }
    let meta_content_length = usize::try_from(u32::from_le_bytes(value.try_into().unwrap()))
        .map_err(|_| {
            Error::LimitExceeded("DICOM File Meta Information length overflowed".into())
        })?;
    if meta_content_length > MAX_DICOM_FILE_META_BYTES {
        return Err(Error::LimitExceeded(format!(
            "DICOM File Meta Information exceeds {MAX_DICOM_FILE_META_BYTES} bytes"
        )));
    }
    let meta_end = after_group_length
        .checked_add(meta_content_length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| {
            Error::InvalidInput("DICOM File Meta Information exceeds the file".into())
        })?;
    let mut offset = after_group_length;
    let mut element_count = 1usize;
    let mut transfer_syntax_uid = None;
    let mut media_storage_sop_class_uid = None;
    while offset < meta_end {
        element_count = element_count.saturating_add(1);
        if element_count > 256 {
            return Err(Error::LimitExceeded(
                "DICOM File Meta Information contains more than 256 elements".into(),
            ));
        }
        let (group, element, vr, value, next) = parse_explicit_vr_element(bytes, offset)?;
        if group != 0x0002 || next > meta_end {
            return Err(Error::InvalidInput(
                "DICOM File Meta Information has an invalid group or length".into(),
            ));
        }
        if element == 0x0002 || element == 0x0010 {
            let label = if element == 0x0002 {
                "Media Storage SOP Class UID"
            } else {
                "Transfer Syntax UID"
            };
            if vr != *b"UI" {
                return Err(Error::InvalidInput(format!(
                    "DICOM {label} must use UI value representation"
                )));
            }
            let uid = std::str::from_utf8(value)
                .map_err(|_| Error::InvalidInput(format!("DICOM {label} is not ASCII")))?
                .trim_matches(['\0', ' '])
                .to_string();
            if uid.is_empty()
                || uid.len() > 64
                || !uid
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
            {
                return Err(Error::InvalidInput(format!("DICOM {label} is invalid")));
            }
            if element == 0x0002 {
                media_storage_sop_class_uid = Some(uid);
            } else {
                transfer_syntax_uid = Some(uid);
            }
        }
        offset = next;
    }
    if offset != meta_end {
        return Err(Error::InvalidInput(
            "DICOM File Meta Information does not end at its declared boundary".into(),
        ));
    }
    let transfer_syntax_uid = transfer_syntax_uid.ok_or_else(|| {
        Error::InvalidInput("DICOM Transfer Syntax UID (0002,0010) is missing".into())
    })?;
    let media_storage_sop_class_uid = media_storage_sop_class_uid.ok_or_else(|| {
        Error::InvalidInput("DICOM Media Storage SOP Class UID (0002,0002) is missing".into())
    })?;
    if transfer_syntax_uid == "1.2.840.10008.1.2.1.99" {
        return Err(Error::Unsupported(
            "Deflated Explicit VR Little Endian DICOM data sets are unsupported".into(),
        ));
    }
    if TransferSyntaxRegistry.get(&transfer_syntax_uid).is_none() {
        return Err(Error::Unsupported(format!(
            "DICOM Transfer Syntax {transfer_syntax_uid} is not registered"
        )));
    }
    Ok(DicomFileMeta {
        media_storage_sop_class_uid,
        transfer_syntax_uid,
        dataset_offset: meta_end,
    })
}

fn parse_explicit_vr_element(bytes: &[u8], offset: usize) -> Result<ExplicitVRElement<'_>> {
    let header_end = offset
        .checked_add(8)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| Error::InvalidInput("truncated explicit-VR DICOM element header".into()))?;
    let group = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
    let element = u16::from_le_bytes(bytes[offset + 2..offset + 4].try_into().unwrap());
    let vr: [u8; 2] = bytes[offset + 4..offset + 6].try_into().unwrap();
    let (value_offset, value_length) = if matches!(
        &vr,
        b"OB" | b"OD" | b"OF" | b"OL" | b"OV" | b"OW" | b"SQ" | b"UC" | b"UR" | b"UT" | b"UN"
    ) {
        let long_header_end = offset
            .checked_add(12)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| Error::InvalidInput("truncated long explicit-VR DICOM header".into()))?;
        (
            long_header_end,
            usize::try_from(u32::from_le_bytes(
                bytes[offset + 8..offset + 12].try_into().unwrap(),
            ))
            .map_err(|_| Error::LimitExceeded("DICOM element length overflowed".into()))?,
        )
    } else {
        (
            header_end,
            usize::from(u16::from_le_bytes(
                bytes[offset + 6..offset + 8].try_into().unwrap(),
            )),
        )
    };
    let value_end = value_offset
        .checked_add(value_length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| Error::InvalidInput("explicit-VR DICOM element exceeds the file".into()))?;
    Ok((
        group,
        element,
        vr,
        &bytes[value_offset..value_end],
        value_end,
    ))
}

fn preflight_dataset_structure(
    bytes: &[u8],
    transfer_syntax_uid: &str,
    require_pixel_data: bool,
    max_encapsulated_document_bytes: Option<u32>,
) -> Result<()> {
    let transfer_syntax = TransferSyntaxRegistry
        .get(transfer_syntax_uid)
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "DICOM Transfer Syntax {transfer_syntax_uid} is not registered"
            ))
        })?;
    let options = DataSetReaderOptions::default().value_read(ValueReadStrategy::Raw);
    let reader =
        DataSetReader::new_with_ts_options(Cursor::new(bytes), transfer_syntax, options)
            .map_err(|error| Error::InvalidInput(format!("invalid DICOM data set: {error}")))?;
    let mut element_count = 0usize;
    let mut token_count = 0usize;
    let mut value_bytes = 0u64;
    let mut encapsulated_document_count = 0usize;
    let mut encapsulated_document_bytes = 0u64;
    let mut sequence_depth = 0usize;
    let mut encapsulated_pixel_sequence_depth = None;
    let mut nested_raw_pixel_value_pending = false;
    let mut nested_pixel_bytes = 0u64;
    let mut found_pixel_data = false;
    for token in reader {
        token_count = token_count.saturating_add(1);
        if token_count > MAX_DICOM_STRUCTURE_TOKENS {
            return Err(Error::LimitExceeded(format!(
                "DICOM data set exceeds {MAX_DICOM_STRUCTURE_TOKENS} structural tokens"
            )));
        }
        let token = token.map_err(|error| {
            Error::InvalidInput(format!("invalid DICOM data set structure: {error}"))
        })?;
        match token {
            DataToken::ElementHeader(header) => {
                element_count = element_count.saturating_add(1);
                if element_count > MAX_DICOM_ELEMENTS {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM data set exceeds {MAX_DICOM_ELEMENTS} elements"
                    )));
                }
                let length = header.len.get().ok_or_else(|| {
                    Error::InvalidInput("DICOM element has an undefined primitive length".into())
                })?;
                if header.tag == Tag(0x7fe0, 0x0010) {
                    if sequence_depth == 0 {
                        if max_encapsulated_document_bytes.is_some() {
                            return Err(Error::InvalidInput(
                                "DICOM Encapsulated PDF Storage unexpectedly contains Pixel Data"
                                    .into(),
                            ));
                        }
                        found_pixel_data = true;
                        break;
                    }
                    if u64::from(length) > MAX_DICOM_INPUT_BYTES {
                        return Err(Error::LimitExceeded(
                            "nested DICOM Pixel Data exceeds the input-size limit".into(),
                        ));
                    }
                    nested_raw_pixel_value_pending = true;
                    nested_pixel_bytes = nested_pixel_bytes.saturating_add(u64::from(length));
                    if nested_pixel_bytes > MAX_DICOM_INPUT_BYTES {
                        return Err(Error::LimitExceeded(
                            "nested DICOM Pixel Data exceeds the input-size limit".into(),
                        ));
                    }
                    continue;
                }
                if header.tag == Tag(0x0042, 0x0011) {
                    let Some(max_bytes) = max_encapsulated_document_bytes else {
                        return Err(Error::Unsupported(
                            "Encapsulated Document is supported only for DICOM Encapsulated PDF Storage".into(),
                        ));
                    };
                    if header.vr.to_bytes() != *b"OB" {
                        return Err(Error::InvalidInput(
                            "DICOM Encapsulated Document must use the OB value representation"
                                .into(),
                        ));
                    }
                    encapsulated_document_count = encapsulated_document_count.saturating_add(1);
                    if encapsulated_document_count > 1 {
                        return Err(Error::InvalidInput(
                            "DICOM contains more than one Encapsulated Document value".into(),
                        ));
                    }
                    if length > max_bytes {
                        return Err(Error::LimitExceeded(format!(
                            "DICOM Encapsulated Document exceeds {max_bytes} bytes"
                        )));
                    }
                    encapsulated_document_bytes = encapsulated_document_bytes
                        .checked_add(u64::from(length))
                        .ok_or_else(|| {
                            Error::LimitExceeded(
                                "DICOM Encapsulated Document size overflowed".into(),
                            )
                        })?;
                    if encapsulated_document_bytes > u64::from(max_bytes) {
                        return Err(Error::LimitExceeded(format!(
                            "DICOM Encapsulated Document values exceed {max_bytes} bytes"
                        )));
                    }
                    continue;
                }
                if length > MAX_DICOM_METADATA_VALUE_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM metadata element {:?} exceeds {MAX_DICOM_METADATA_VALUE_BYTES} bytes",
                        header.tag
                    )));
                }
                value_bytes = value_bytes.saturating_add(u64::from(length));
                if value_bytes > MAX_DICOM_METADATA_TOTAL_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM metadata values exceed {MAX_DICOM_METADATA_TOTAL_BYTES} bytes"
                    )));
                }
            }
            DataToken::PixelSequenceStart => {
                if sequence_depth == 0 {
                    found_pixel_data = true;
                    break;
                }
                if encapsulated_pixel_sequence_depth.is_some() {
                    return Err(Error::InvalidInput(
                        "nested encapsulated DICOM Pixel Data sequences are invalid".into(),
                    ));
                }
                sequence_depth = sequence_depth.saturating_add(1);
                encapsulated_pixel_sequence_depth = Some(sequence_depth);
                nested_pixel_bytes = nested_pixel_bytes.saturating_add(8);
                if nested_pixel_bytes > MAX_DICOM_INPUT_BYTES {
                    return Err(Error::LimitExceeded(
                        "nested DICOM Pixel Data exceeds the input-size limit".into(),
                    ));
                }
                if sequence_depth > MAX_DICOM_SEQUENCE_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM sequence nesting exceeds {MAX_DICOM_SEQUENCE_DEPTH}"
                    )));
                }
                element_count = element_count.saturating_add(1);
                if element_count > MAX_DICOM_ELEMENTS {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM data set exceeds {MAX_DICOM_ELEMENTS} elements/items"
                    )));
                }
            }
            DataToken::SequenceStart { len, .. } | DataToken::ItemStart { len } => {
                let maximum = if encapsulated_pixel_sequence_depth.is_some() {
                    u32::try_from(MAX_DICOM_INPUT_BYTES).unwrap()
                } else {
                    u32::try_from(MAX_DICOM_METADATA_TOTAL_BYTES).unwrap()
                };
                if len.get().is_some_and(|length| length > maximum) {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM sequence/item exceeds {maximum} bytes"
                    )));
                }
                sequence_depth = sequence_depth.saturating_add(1);
                if sequence_depth > MAX_DICOM_SEQUENCE_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM sequence nesting exceeds {MAX_DICOM_SEQUENCE_DEPTH}"
                    )));
                }
                element_count = element_count.saturating_add(1);
                if element_count > MAX_DICOM_ELEMENTS {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM data set exceeds {MAX_DICOM_ELEMENTS} elements/items"
                    )));
                }
                if encapsulated_pixel_sequence_depth.is_some() {
                    nested_pixel_bytes =
                        nested_pixel_bytes.saturating_add(u64::from(len.get().unwrap_or_default()));
                    if nested_pixel_bytes > MAX_DICOM_INPUT_BYTES {
                        return Err(Error::LimitExceeded(
                            "nested DICOM Pixel Data exceeds the input-size limit".into(),
                        ));
                    }
                }
            }
            DataToken::SequenceEnd => {
                if encapsulated_pixel_sequence_depth == Some(sequence_depth) {
                    encapsulated_pixel_sequence_depth = None;
                }
                sequence_depth = sequence_depth.saturating_sub(1);
            }
            DataToken::ItemEnd => {
                sequence_depth = sequence_depth.saturating_sub(1);
            }
            DataToken::ItemValue(value) => {
                if encapsulated_pixel_sequence_depth.is_some() {
                    if value.len() > MAX_DICOM_INPUT_BYTES as usize {
                        return Err(Error::LimitExceeded(
                            "nested DICOM pixel fragment exceeds the input-size limit".into(),
                        ));
                    }
                    continue;
                }
                if value.len() > MAX_DICOM_METADATA_VALUE_BYTES as usize {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM encapsulated fragment exceeds {MAX_DICOM_METADATA_VALUE_BYTES} bytes"
                    )));
                }
                value_bytes = value_bytes.saturating_add(value.len() as u64);
                if value_bytes > MAX_DICOM_METADATA_TOTAL_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM metadata and nested pixel fragments exceed {MAX_DICOM_METADATA_TOTAL_BYTES} bytes"
                    )));
                }
            }
            DataToken::OffsetTable(values) => {
                if encapsulated_pixel_sequence_depth.is_some() {
                    if values.len() > MAX_DICOM_INPUT_BYTES as usize / 4 {
                        return Err(Error::LimitExceeded(
                            "nested DICOM offset table exceeds the input-size limit".into(),
                        ));
                    }
                    continue;
                }
                let size = values.len().saturating_mul(std::mem::size_of::<u32>());
                value_bytes = value_bytes.saturating_add(size as u64);
                if value_bytes > MAX_DICOM_METADATA_TOTAL_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "DICOM metadata and nested pixel fragments exceed {MAX_DICOM_METADATA_TOTAL_BYTES} bytes"
                    )));
                }
            }
            DataToken::PrimitiveValue(_) => {
                if nested_raw_pixel_value_pending {
                    nested_raw_pixel_value_pending = false;
                }
            }
        }
    }
    if require_pixel_data && !found_pixel_data {
        return Err(Error::Unsupported(
            "DICOM data set does not contain a Pixel Data element".into(),
        ));
    }
    Ok(())
}

fn collect_directory_record_offsets(
    dataset: &[u8],
    absolute_dataset_offset: usize,
    transfer_syntax_uid: &str,
) -> Result<Vec<u32>> {
    let transfer_syntax = TransferSyntaxRegistry
        .get(transfer_syntax_uid)
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "unregistered DICOM transfer syntax {transfer_syntax_uid}"
            ))
        })?;
    let position = Rc::new(Cell::new(0usize));
    let source = PositionTrackingReader {
        inner: Cursor::new(dataset),
        position: position.clone(),
    };
    let options = DataSetReaderOptions::default()
        .value_read(ValueReadStrategy::Raw)
        .base_offset(absolute_dataset_offset as u64);
    let reader = DataSetReader::new_with_ts_options(source, transfer_syntax, options)
        .map_err(|error| Error::InvalidInput(format!("invalid DICOMDIR data set: {error}")))?;
    let mut sequence_depth = 0usize;
    let mut directory_sequence_depth = None;
    let mut found_sequence = false;
    let mut offsets = Vec::new();
    let mut token_count = 0usize;
    for token in reader {
        token_count = token_count.saturating_add(1);
        if token_count > MAX_DICOM_STRUCTURE_TOKENS {
            return Err(Error::LimitExceeded(format!(
                "DICOMDIR record sequence exceeds {MAX_DICOM_STRUCTURE_TOKENS} parser tokens"
            )));
        }
        match token.map_err(|error| {
            Error::InvalidInput(format!("invalid DICOMDIR record sequence: {error}"))
        })? {
            DataToken::SequenceStart { tag, .. } => {
                if sequence_depth == 0 && tag == Tag(0x0004, 0x1220) {
                    if directory_sequence_depth.is_some() {
                        return Err(Error::InvalidInput(
                            "DICOMDIR contains more than one root Directory Record Sequence".into(),
                        ));
                    }
                    directory_sequence_depth = Some(sequence_depth.saturating_add(1));
                    found_sequence = true;
                }
                sequence_depth = sequence_depth.saturating_add(1);
                if sequence_depth > MAX_DICOM_SEQUENCE_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "DICOMDIR data set nesting exceeds {MAX_DICOM_SEQUENCE_DEPTH}"
                    )));
                }
            }
            DataToken::ItemStart { .. } => {
                if directory_sequence_depth == Some(sequence_depth) {
                    let after_item_header = position.get();
                    let relative_offset = after_item_header.checked_sub(8).ok_or_else(|| {
                        Error::InvalidInput("DICOMDIR item offset underflowed".into())
                    })?;
                    let absolute_offset = absolute_dataset_offset
                        .checked_add(relative_offset)
                        .ok_or_else(|| {
                            Error::LimitExceeded("DICOMDIR item offset overflowed".into())
                        })?;
                    if dataset_absolute_bytes(
                        dataset,
                        absolute_dataset_offset,
                        absolute_offset,
                        absolute_offset.saturating_add(4),
                    )? != [0xfe, 0xff, 0x00, 0xe0]
                    {
                        return Err(Error::InvalidInput(
                            "DICOMDIR item offset does not point to an Item tag".into(),
                        ));
                    }
                    offsets.push(u32::try_from(absolute_offset).map_err(|_| {
                        Error::LimitExceeded("DICOMDIR item offset exceeds 32-bit range".into())
                    })?);
                    if offsets.len() > MAX_DICOMDIR_RECORDS {
                        return Err(Error::LimitExceeded(format!(
                            "DICOMDIR contains more than {MAX_DICOMDIR_RECORDS} records"
                        )));
                    }
                }
                sequence_depth = sequence_depth.saturating_add(1);
            }
            DataToken::SequenceEnd => {
                if directory_sequence_depth == Some(sequence_depth) {
                    break;
                }
                sequence_depth = sequence_depth.saturating_sub(1);
            }
            DataToken::ItemEnd => sequence_depth = sequence_depth.saturating_sub(1),
            DataToken::PixelSequenceStart => {
                sequence_depth = sequence_depth.saturating_add(1);
                if sequence_depth > MAX_DICOM_SEQUENCE_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "DICOMDIR data set nesting exceeds {MAX_DICOM_SEQUENCE_DEPTH}"
                    )));
                }
            }
            DataToken::ElementHeader(_)
            | DataToken::PrimitiveValue(_)
            | DataToken::ItemValue(_)
            | DataToken::OffsetTable(_) => {}
        }
    }
    if !found_sequence || offsets.is_empty() {
        return Err(Error::InvalidInput(
            "DICOMDIR contains no Directory Record Sequence items".into(),
        ));
    }
    Ok(offsets)
}

fn dataset_absolute_bytes(
    dataset: &[u8],
    absolute_dataset_offset: usize,
    start: usize,
    end: usize,
) -> Result<&[u8]> {
    let relative_start = start
        .checked_sub(absolute_dataset_offset)
        .ok_or_else(|| Error::InvalidInput("DICOMDIR item offset precedes the data set".into()))?;
    let relative_end = end
        .checked_sub(absolute_dataset_offset)
        .ok_or_else(|| Error::InvalidInput("DICOMDIR item offset precedes the data set".into()))?;
    dataset
        .get(relative_start..relative_end)
        .ok_or_else(|| Error::InvalidInput("DICOMDIR item offset exceeds the data set".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_part10_prefix_at_the_standard_offset() {
        let mut bytes = vec![0; 132];
        bytes[128..132].copy_from_slice(b"DICM");
        assert!(looks_like_dicom_prefix(&bytes));
        bytes[128..132].copy_from_slice(b"NOPE");
        assert!(!looks_like_dicom_prefix(&bytes));
        assert!(!looks_like_dicom_prefix(&bytes[..131]));
    }

    #[test]
    fn validates_jpeg2000_siz_dimensions_components_and_sample_precision() {
        let codestream = include_bytes!("../../tests/fixtures/sample_jpeg2000.j2k");
        let header = parse_raw_codestream(codestream).unwrap();
        assert_eq!(header.width, 4);
        assert_eq!(header.height, 4);
        assert_eq!(
            header.components,
            vec![crate::jpeg2000::Jpeg2000ComponentHeader {
                precision: 8,
                signed: false,
                width: 4,
                height: 4,
            }]
        );
        assert!(parse_raw_codestream(&codestream[..20]).is_err());
    }

    #[test]
    fn preflights_element_counts_before_building_the_dicom_object() {
        let source = include_bytes!("../../tests/fixtures/sample_multiframe.dcm");
        let source_meta = parse_file_meta(source).unwrap();
        let dataset_offset = source_meta.dataset_offset;
        let mut bytes = source[..dataset_offset].to_vec();
        for _ in 0..=MAX_DICOM_ELEMENTS {
            // Zero-length private LO element: a compact structural-token stress case.
            bytes.extend_from_slice(&[0x29, 0x00, 0x01, 0x00, b'L', b'O', 0x00, 0x00]);
        }
        bytes.extend_from_slice(&source[dataset_offset..]);
        let file_meta = parse_file_meta(&bytes).unwrap();
        assert!(matches!(
            preflight_dataset_structure(
                &bytes[file_meta.dataset_offset..],
                &file_meta.transfer_syntax_uid,
                true,
                None
            ),
            Err(Error::LimitExceeded(message)) if message.contains("100000 elements")
        ));
    }
}
