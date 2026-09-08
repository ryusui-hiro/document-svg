use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use std::path::Path;

use base64::Engine;
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{
    ClipPath, GlowEffect, GradientStop, IDENTITY, ImageColorEffect, LineJoin, LinearGradient,
    MaskDefinition, Matrix, Node, OuterShadow, Page, Paint, RadialGradient, SourceMeta, Stroke,
    TextAnchor, TextRun, TilingPatternDefinition, compose,
};
use crate::ooxml::chart::{parse_chart, render_chart};
use crate::ooxml::{
    Relationships, ZipPackage, attribute, color_from_hex, local_name, parse_i64,
    qualified_attribute, text_advance_factor,
};

const EMU_PER_POINT: f64 = 12_700.0;
const DEFAULT_SLIDE_WIDTH: f64 = 720.0;
const DEFAULT_SLIDE_HEIGHT: f64 = 540.0;
const MAX_METAFILE_BYTES: usize = 12 * 1024 * 1024;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut package = ZipPackage::open(path, options.max_zip_entry_bytes)?;
    let presentation_part = "ppt/presentation.xml";
    if !package.contains(presentation_part) {
        return Err(Error::InvalidInput(
            "PPTX is missing ppt/presentation.xml".into(),
        ));
    }
    let presentation = package.read(presentation_part)?;
    let presentation_relationships =
        package.relationships(presentation_part, options.max_xml_events)?;
    let (width, height, slide_ids) = parse_presentation(&presentation, options.max_xml_events)?;
    if slide_ids.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "PPTX contains {} slides; maximum is {}",
            slide_ids.len(),
            options.max_pages
        )));
    }
    let theme_part =
        relationship_target_of_type(&presentation_relationships, presentation_part, "/theme")
            .unwrap_or_else(|| "ppt/theme/theme1.xml".into());
    let theme = package
        .read_optional(&theme_part)?
        .map(|xml| Theme::parse(&xml, options.max_xml_events))
        .transpose()?
        .unwrap_or_default();
    let mut theme_cache = HashMap::from([(theme_part, theme.clone())]);
    let mut warnings = Vec::new();
    for (index, relationship_id) in slide_ids.iter().enumerate() {
        let Some(slide_part) =
            presentation_relationships.target(relationship_id, presentation_part)
        else {
            return Err(Error::InvalidInput(format!(
                "PPTX slide relationship {relationship_id} is missing or external"
            )));
        };
        let slide_xml = package.read(&slide_part)?;
        let relationships = package.relationships(&slide_part, options.max_xml_events)?;
        let empty_placeholders = HashMap::new();
        let mut master_page = None::<Page>;
        let mut layout_page = None::<Page>;
        let mut inherited_placeholders = HashMap::new();
        let mut text_styles = PresentationTextStyles::default();
        let mut slide_theme = theme.clone();
        if let Some(layout_part) =
            relationship_target_of_type(&relationships, &slide_part, "/slideLayout")
        {
            let layout_xml = package.read(&layout_part)?;
            let layout_relationships =
                package.relationships(&layout_part, options.max_xml_events)?;
            let mut master_placeholders = HashMap::new();
            if let Some(master_part) =
                relationship_target_of_type(&layout_relationships, &layout_part, "/slideMaster")
            {
                let master_xml = package.read(&master_part)?;
                let master_relationships =
                    package.relationships(&master_part, options.max_xml_events)?;
                if let Some(master_theme_part) =
                    relationship_target_of_type(&master_relationships, &master_part, "/theme")
                {
                    slide_theme = if let Some(cached) = theme_cache.get(&master_theme_part) {
                        cached.clone()
                    } else {
                        let parsed = package
                            .read_optional(&master_theme_part)?
                            .map(|xml| Theme::parse(&xml, options.max_xml_events))
                            .transpose()?
                            .unwrap_or_else(|| theme.clone());
                        theme_cache.insert(master_theme_part, parsed.clone());
                        parsed
                    };
                }
                text_styles =
                    parse_master_text_styles(&master_xml, &slide_theme, options.max_xml_events)?;
                master_placeholders = parse_placeholder_geometries(
                    &master_xml,
                    &empty_placeholders,
                    options.max_xml_events,
                )?;
                let mut parsed_master = parse_slide(
                    &master_xml,
                    index + 1,
                    width,
                    height,
                    &slide_theme,
                    &master_relationships,
                    &master_part,
                    &mut package,
                    options.max_xml_events,
                    "master",
                    &empty_placeholders,
                    &text_styles,
                )?;
                remove_placeholder_nodes(&mut parsed_master);
                master_page = Some(parsed_master);
            }
            inherited_placeholders = parse_placeholder_geometries(
                &layout_xml,
                &master_placeholders,
                options.max_xml_events,
            )?;
            let mut parsed_layout = parse_slide(
                &layout_xml,
                index + 1,
                width,
                height,
                &slide_theme,
                &layout_relationships,
                &layout_part,
                &mut package,
                options.max_xml_events,
                "layout",
                &master_placeholders,
                &text_styles,
            )?;
            remove_placeholder_nodes(&mut parsed_layout);
            layout_page = Some(parsed_layout);
        }
        let mut slide_page = parse_slide(
            &slide_xml,
            index + 1,
            width,
            height,
            &slide_theme,
            &relationships,
            &slide_part,
            &mut package,
            options.max_xml_events,
            "slide",
            &inherited_placeholders,
            &text_styles,
        )?;
        let (chart_nodes, chart_warnings) = parse_pptx_chart_frames(
            &slide_xml,
            index + 1,
            &relationships,
            &slide_part,
            &mut package,
            options.max_xml_events,
        )?;
        slide_page.nodes.extend(chart_nodes);
        for warning in chart_warnings {
            slide_page.warn(warning);
        }
        let (table_nodes, table_clips, table_warnings) =
            parse_pptx_table_frames(&slide_xml, index + 1, &slide_theme, options.max_xml_events)?;
        slide_page.nodes.extend(table_nodes);
        slide_page.clips.extend(table_clips);
        for warning in table_warnings {
            slide_page.warn(warning);
        }
        let (smartart_nodes, smartart_clips, smartart_masks, smartart_patterns, smartart_warnings) =
            parse_pptx_smartart_frames(
                &slide_xml,
                index + 1,
                &slide_theme,
                &text_styles,
                &relationships,
                &slide_part,
                &mut package,
                options.max_xml_events,
            )?;
        slide_page.nodes.extend(smartart_nodes);
        slide_page.clips.extend(smartart_clips);
        slide_page.masks.extend(smartart_masks);
        slide_page.patterns.extend(smartart_patterns);
        for warning in smartart_warnings {
            slide_page.warn(warning);
        }
        let (embedded_nodes, embedded_warnings) = parse_pptx_embedded_frames(
            &slide_xml,
            index + 1,
            &relationships,
            &slide_part,
            options.max_xml_events,
        )?;
        slide_page.nodes.extend(embedded_nodes);
        for warning in embedded_warnings {
            slide_page.warn(warning);
        }
        let page = merge_page_layers(master_page, layout_page, slide_page);
        warnings.extend(page.warnings.iter().cloned());
        sink.consume(page)?;
    }
    Ok(deduplicate(warnings))
}

fn relationship_target_of_type(
    relationships: &Relationships,
    owner_part: &str,
    suffix: &str,
) -> Option<String> {
    relationships
        .ids_of_type(suffix)
        .next()
        .and_then(|(id, _)| relationships.target(id, owner_part))
}

fn xml_contains_local_element(xml: &[u8], expected: &[u8]) -> bool {
    let mut cursor = 0usize;
    while cursor < xml.len() {
        if xml[cursor] != b'<' {
            cursor += 1;
            continue;
        }
        let mut start = cursor + 1;
        if xml.get(start) == Some(&b'/') {
            start += 1;
        }
        if !xml
            .get(start)
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            cursor = start.saturating_add(1);
            continue;
        }
        let mut end = start;
        while xml
            .get(end)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b'/' | b'>'))
        {
            end += 1;
        }
        let qualified = &xml[start..end];
        let local = qualified
            .iter()
            .rposition(|byte| *byte == b':')
            .map_or(qualified, |index| &qualified[index + 1..]);
        if local == expected {
            return true;
        }
        cursor = end.max(cursor + 1);
    }
    false
}

#[derive(Default)]
struct PptxChartFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    relationship_id: String,
    name: String,
}

#[allow(clippy::too_many_arguments)]
fn parse_pptx_chart_frames(
    xml: &[u8],
    page_number: usize,
    relationships: &Relationships,
    slide_part: &str,
    package: &mut ZipPackage<std::fs::File>,
    max_events: usize,
) -> Result<(Vec<Node>, Vec<String>)> {
    if !xml_contains_local_element(xml, b"chart") {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut frame = None::<PptxChartFrame>;
    let mut nodes = Vec::new();
    let mut warnings = Vec::new();
    let mut frame_index = 0usize;
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "slide chart frames exceed {max_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if name == "graphicFrame" {
                    frame = Some(PptxChartFrame::default());
                } else {
                    apply_pptx_chart_frame_event(&start, &name, &stack, frame.as_mut());
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                apply_pptx_chart_frame_event(&start, &name, &stack, frame.as_mut());
            }
            Event::End(end) => {
                if local_name(end.name().as_ref()) == b"graphicFrame"
                    && let Some(frame) = frame.take()
                    && !frame.relationship_id.is_empty()
                {
                    frame_index += 1;
                    if let Some(chart_part) =
                        relationships.target(&frame.relationship_id, slide_part)
                    {
                        if let Some(chart_xml) = package.read_optional(&chart_part)? {
                            nodes.extend(render_chart(
                                &parse_chart(&chart_xml, max_events)?,
                                frame.x,
                                frame.y,
                                frame.width,
                                frame.height,
                                &format!("pptx-chart-{page_number}-{frame_index}"),
                            ));
                        } else {
                            warnings.push(format!("chart part {chart_part} is missing"));
                        }
                    } else {
                        warnings.push(format!(
                            "chart relationship {} is missing",
                            frame.relationship_id
                        ));
                    }
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((nodes, deduplicate(warnings)))
}

fn apply_pptx_chart_frame_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    frame: Option<&mut PptxChartFrame>,
) {
    let Some(frame) = frame else {
        return;
    };
    match name {
        "off" if stack.iter().any(|item| item == "xfrm") => {
            frame.x = parse_i64(attribute(start, b"x"), 0) as f64 / EMU_PER_POINT;
            frame.y = parse_i64(attribute(start, b"y"), 0) as f64 / EMU_PER_POINT;
        }
        "ext" if stack.iter().any(|item| item == "xfrm") => {
            frame.width = parse_i64(attribute(start, b"cx"), 0).max(0) as f64 / EMU_PER_POINT;
            frame.height = parse_i64(attribute(start, b"cy"), 0).max(0) as f64 / EMU_PER_POINT;
        }
        "cNvPr" => frame.name = attribute(start, b"name").unwrap_or_default(),
        "chart" => frame.relationship_id = attribute(start, b"id").unwrap_or_default(),
        _ => {}
    }
}

#[derive(Default)]
struct PptxTableFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    rotation: f64,
    name: String,
    is_table: bool,
    first_row: bool,
    band_row: bool,
    column_widths: Vec<f64>,
    rows: Vec<PptxTableRow>,
}

#[derive(Default)]
struct PptxTableRow {
    height: f64,
    cells: Vec<PptxTableCell>,
}

struct PptxTableCell {
    paragraphs: Vec<Paragraph>,
    fill: Option<Paint>,
    margin_left: f64,
    margin_right: f64,
    margin_top: f64,
    margin_bottom: f64,
    vertical_anchor: Option<String>,
    grid_span: usize,
    row_span: usize,
    horizontal_merge: bool,
    vertical_merge: bool,
    text_outer_shadow: Option<OuterShadow>,
    text_glow: Option<GlowEffect>,
}

impl Default for PptxTableCell {
    fn default() -> Self {
        Self {
            paragraphs: Vec::new(),
            fill: None,
            margin_left: 7.2,
            margin_right: 7.2,
            margin_top: 3.0,
            margin_bottom: 3.0,
            vertical_anchor: None,
            grid_span: 1,
            row_span: 1,
            horizontal_merge: false,
            vertical_merge: false,
            text_outer_shadow: None,
            text_glow: None,
        }
    }
}

type PptxTableRender = (Vec<Node>, Vec<ClipPath>, Vec<String>);

fn parse_pptx_table_frames(
    xml: &[u8],
    page_number: usize,
    theme: &Theme,
    max_events: usize,
) -> Result<PptxTableRender> {
    if !xml_contains_local_element(xml, b"tbl") {
        return Ok((Vec::new(), Vec::new(), Vec::new()));
    }
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut frame = None::<PptxTableFrame>;
    let mut row = None::<PptxTableRow>;
    let mut cell = None::<PptxTableCell>;
    let mut paragraph = None::<Paragraph>;
    let mut run = None::<TextRun>;
    let mut text_buffer = String::new();
    let mut frames = Vec::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "slide table frames exceed {max_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                match name.as_str() {
                    "graphicFrame" => frame = Some(PptxTableFrame::default()),
                    "tbl" if frame.is_some() => {
                        if let Some(frame) = frame.as_mut() {
                            frame.is_table = true;
                        }
                    }
                    "tr" if frame.as_ref().is_some_and(|frame| frame.is_table) => {
                        row = Some(PptxTableRow {
                            height: parse_i64(attribute(&start, b"h"), 0).max(0) as f64
                                / EMU_PER_POINT,
                            cells: Vec::new(),
                        });
                    }
                    "tc" if row.is_some() => cell = Some(PptxTableCell::default()),
                    "p" if cell.is_some() && stack.iter().any(|item| item == "txBody") => {
                        paragraph = Some(Paragraph::default());
                    }
                    "r" | "fld"
                        if paragraph.is_some() && stack.iter().any(|item| item == "txBody") =>
                    {
                        run = Some(default_table_run(theme));
                    }
                    "t" if paragraph.is_some() => text_buffer.clear(),
                    _ => apply_pptx_table_event(
                        &start,
                        &name,
                        &stack,
                        theme,
                        frame.as_mut(),
                        cell.as_mut(),
                        paragraph.as_mut(),
                        run.as_mut(),
                    ),
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if name == "br" && paragraph.is_some() {
                    run.get_or_insert_with(|| default_table_run(theme))
                        .text
                        .push('\n');
                } else {
                    apply_pptx_table_event(
                        &start,
                        &name,
                        &stack,
                        theme,
                        frame.as_mut(),
                        cell.as_mut(),
                        paragraph.as_mut(),
                        run.as_mut(),
                    );
                }
            }
            Event::Text(text) if stack.last().is_some_and(|item| item == "t") => {
                text_buffer.push_str(&text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid PPTX table text: {error}"))
                })?);
            }
            Event::GeneralRef(reference) if stack.last().is_some_and(|item| item == "t") => {
                text_buffer.push_str(&reference.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid PPTX table reference: {error}"))
                })?);
            }
            Event::End(end) => {
                let name = String::from_utf8_lossy(local_name(end.name().as_ref())).into_owned();
                match name.as_str() {
                    "t" => {
                        run.get_or_insert_with(|| default_table_run(theme))
                            .text
                            .push_str(&text_buffer);
                        text_buffer.clear();
                    }
                    "r" | "fld" => {
                        if let (Some(paragraph), Some(run)) = (paragraph.as_mut(), run.take()) {
                            paragraph.runs.push(run);
                        }
                    }
                    "p" => {
                        if let Some(mut finished) = paragraph.take() {
                            if let Some(run) = run.take() {
                                finished.runs.push(run);
                            }
                            if let Some(cell) = cell.as_mut() {
                                cell.paragraphs.push(finished);
                            }
                        }
                    }
                    "tc" => {
                        if let (Some(row), Some(cell)) = (row.as_mut(), cell.take()) {
                            row.cells.push(cell);
                        }
                    }
                    "tr" => {
                        if let (Some(frame), Some(row)) = (frame.as_mut(), row.take()) {
                            frame.rows.push(row);
                        }
                    }
                    "graphicFrame" => {
                        if let Some(frame) = frame.take()
                            && frame.is_table
                        {
                            frames.push(frame);
                        }
                    }
                    _ => {}
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    let mut nodes = Vec::new();
    let mut clips = Vec::new();
    let mut warnings = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        render_pptx_table(
            frame,
            page_number,
            index + 1,
            theme,
            &mut nodes,
            &mut clips,
            &mut warnings,
        );
    }
    Ok((nodes, clips, deduplicate(warnings)))
}

#[allow(clippy::too_many_arguments)]
fn apply_pptx_table_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    theme: &Theme,
    frame: Option<&mut PptxTableFrame>,
    cell: Option<&mut PptxTableCell>,
    paragraph: Option<&mut Paragraph>,
    run: Option<&mut TextRun>,
) {
    if stack
        .iter()
        .any(|item| matches!(item.as_str(), "hiddenFill" | "hiddenLine" | "hiddenEffects"))
    {
        return;
    }
    match name {
        "cNvPr" if !stack.iter().any(|item| item == "pic") => {
            if let Some(frame) = frame {
                frame.name = attribute(start, b"name").unwrap_or_default();
            }
        }
        "xfrm" if frame.is_some() => {
            if let Some(frame) = frame {
                frame.rotation = parse_i64(attribute(start, b"rot"), 0) as f64 / 60_000.0;
            }
        }
        "off" if frame.is_some() && stack.iter().any(|item| item == "xfrm") => {
            if let Some(frame) = frame {
                frame.x = parse_i64(attribute(start, b"x"), 0) as f64 / EMU_PER_POINT;
                frame.y = parse_i64(attribute(start, b"y"), 0) as f64 / EMU_PER_POINT;
            }
        }
        "ext" if frame.is_some() && stack.iter().any(|item| item == "xfrm") => {
            if let Some(frame) = frame {
                frame.width = parse_i64(attribute(start, b"cx"), 0).max(0) as f64 / EMU_PER_POINT;
                frame.height = parse_i64(attribute(start, b"cy"), 0).max(0) as f64 / EMU_PER_POINT;
            }
        }
        "tblPr" => {
            if let Some(frame) = frame {
                frame.first_row = drawingml_toggle(attribute(start, b"firstRow"));
                frame.band_row = drawingml_toggle(attribute(start, b"bandRow"));
            }
        }
        "gridCol" => {
            if let Some(frame) = frame {
                frame
                    .column_widths
                    .push(parse_i64(attribute(start, b"w"), 0).max(0) as f64 / EMU_PER_POINT);
            }
        }
        "gridSpan" => {
            if let Some(cell) = cell {
                cell.grid_span = parse_i64(attribute(start, b"val"), 1).max(1) as usize;
            }
        }
        "rowSpan" => {
            if let Some(cell) = cell {
                cell.row_span = parse_i64(attribute(start, b"val"), 1).max(1) as usize;
            }
        }
        "hMerge" => {
            if let Some(cell) = cell {
                cell.horizontal_merge =
                    attribute(start, b"val").is_none_or(|value| drawingml_toggle(Some(value)));
            }
        }
        "vMerge" => {
            if let Some(cell) = cell {
                cell.vertical_merge =
                    attribute(start, b"val").is_none_or(|value| drawingml_toggle(Some(value)));
            }
        }
        "marL" | "marR" | "marT" | "marB" => {
            if let Some(cell) = cell {
                let value = parse_i64(attribute(start, b"w"), 0).max(0) as f64 / EMU_PER_POINT;
                match name {
                    "marL" => cell.margin_left = value,
                    "marR" => cell.margin_right = value,
                    "marT" => cell.margin_top = value,
                    _ => cell.margin_bottom = value,
                }
            }
        }
        "bodyPr" => {
            if let Some(cell) = cell {
                cell.vertical_anchor = attribute(start, b"anchor");
            }
        }
        "pPr" => {
            if let Some(paragraph) = paragraph {
                paragraph.alignment = Some(match attribute(start, b"algn").as_deref() {
                    Some("ctr") => TextAnchor::Middle,
                    Some("r") => TextAnchor::End,
                    _ => TextAnchor::Start,
                });
                paragraph.level = parse_i64(attribute(start, b"lvl"), 0).max(0) as usize;
            }
        }
        "rPr" | "endParaRPr" => {
            if let Some(run) = run {
                if let Some(size) = attribute(start, b"sz") {
                    run.font_size = size.parse::<f64>().unwrap_or(1_200.0) / 100.0;
                }
                if let Some(bold) = attribute(start, b"b") {
                    run.bold = drawingml_toggle(Some(bold));
                }
                if let Some(italic) = attribute(start, b"i") {
                    run.italic = drawingml_toggle(Some(italic));
                }
            }
        }
        "outerShdw"
            if stack.iter().any(|item| item == "effectLst")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(cell) = cell {
                cell.text_outer_shadow = Some(drawingml_outer_shadow(start).0);
            }
        }
        "glow"
            if stack.iter().any(|item| item == "effectLst")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(cell) = cell {
                cell.text_glow = Some(drawingml_glow(start));
            }
        }
        "latin" | "ea" | "cs" => {
            if let Some(run) = run
                && let Some(typeface) = attribute(start, b"typeface")
                && !typeface.is_empty()
            {
                run.font_family = office_font_stack(&typeface);
            }
        }
        "srgbClr" | "schemeClr"
            if stack.iter().any(|item| item == "outerShdw")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(shadow) = cell.and_then(|cell| cell.text_outer_shadow.as_mut()) {
                shadow.color = if name == "schemeClr" {
                    theme.color(&attribute(start, b"val").unwrap_or_default())
                } else {
                    color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000")
                };
            }
        }
        "srgbClr" | "schemeClr"
            if stack.iter().any(|item| item == "glow")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(glow) = cell.and_then(|cell| cell.text_glow.as_mut()) {
                glow.color = if name == "schemeClr" {
                    theme.color(&attribute(start, b"val").unwrap_or_default())
                } else {
                    color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000")
                };
            }
        }
        "srgbClr" | "schemeClr" => {
            let color = if name == "schemeClr" {
                theme.color(&attribute(start, b"val").unwrap_or_default())
            } else {
                color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000")
            };
            if table_color_targets_cell_fill(stack) {
                if let Some(cell) = cell {
                    cell.fill = Some(Paint::solid(color));
                }
            } else if let Some(run) = run {
                run.fill = Paint::solid(color);
            }
        }
        "alpha"
            if stack.iter().any(|item| item == "outerShdw")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(shadow) = cell.and_then(|cell| cell.text_outer_shadow.as_mut()) {
                shadow.opacity = (parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0)
                    .clamp(0.0, 1.0);
            }
        }
        "alpha"
            if stack.iter().any(|item| item == "glow")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(glow) = cell.and_then(|cell| cell.text_glow.as_mut()) {
                glow.opacity = (parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0)
                    .clamp(0.0, 1.0);
            }
        }
        "hueOff" | "hueMod" | "satOff" | "satMod" | "lumOff" | "lumMod" | "tint" | "shade"
        | "alphaOff" | "alphaMod"
            if stack.iter().any(|item| item == "glow")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(glow) = cell.and_then(|cell| cell.text_glow.as_mut()) {
                apply_glow_color_transform(
                    glow,
                    name,
                    parse_i64(attribute(start, b"val"), 0) as f64,
                );
            }
        }
        "alpha" => {
            let opacity = parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0;
            if table_color_targets_cell_fill(stack) {
                if let Some(paint) = cell.and_then(|cell| cell.fill.as_mut()) {
                    set_paint_opacity(paint, opacity);
                }
            } else if let Some(run) = run {
                set_paint_opacity(&mut run.fill, opacity);
            }
        }
        "noFill" if table_color_targets_cell_fill(stack) => {
            if let Some(cell) = cell {
                cell.fill = Some(Paint::None);
            }
        }
        _ => {}
    }
}

fn table_color_targets_cell_fill(stack: &[String]) -> bool {
    stack.iter().any(|item| item == "tcPr")
        && !stack.iter().any(|item| {
            matches!(
                item.as_str(),
                "lnL" | "lnR" | "lnT" | "lnB" | "lnTlToBr" | "lnBlToTr"
            )
        })
}

fn default_table_run(theme: &Theme) -> TextRun {
    TextRun {
        font_family: theme.minor_font.clone(),
        font_size: 12.0,
        fill: Paint::None,
        ..TextRun::default()
    }
}

fn drawingml_toggle(value: Option<String>) -> bool {
    value.is_some_and(|value| matches!(value.as_str(), "1" | "true" | "on"))
}

fn drawingml_outer_shadow(start: &quick_xml::events::BytesStart<'_>) -> (OuterShadow, bool) {
    (
        OuterShadow {
            color: "#000000".into(),
            opacity: 1.0,
            blur_radius: (parse_i64(attribute(start, b"blurRad"), 0).max(0) as f64 / EMU_PER_POINT)
                .min(512.0),
            distance: (parse_i64(attribute(start, b"dist"), 0).max(0) as f64 / EMU_PER_POINT)
                .min(4_096.0),
            direction_degrees: parse_i64(attribute(start, b"dir"), 0) as f64 / 60_000.0,
        },
        !attribute(start, b"rotWithShape").is_some_and(|value| value == "0" || value == "false"),
    )
}

fn drawingml_glow(start: &quick_xml::events::BytesStart<'_>) -> GlowEffect {
    GlowEffect {
        color: "#000000".into(),
        opacity: 1.0,
        radius: (parse_i64(attribute(start, b"rad"), 0).max(0) as f64 / EMU_PER_POINT).min(512.0),
    }
}

fn apply_glow_color_transform(glow: &mut GlowEffect, operation: &str, raw_value: f64) {
    if matches!(operation, "alphaOff" | "alphaMod") {
        glow.opacity = transformed_alpha(glow.opacity, operation, raw_value);
    } else {
        glow.color = transformed_drawingml_color(&glow.color, operation, raw_value);
    }
}

fn recover_table_metrics(values: &[f64]) -> Vec<f64> {
    let positive = values
        .iter()
        .copied()
        .filter(|value| *value > 0.0 && value.is_finite())
        .collect::<Vec<_>>();
    let fallback = if positive.is_empty() {
        1.0
    } else {
        positive.iter().sum::<f64>() / positive.len() as f64
    };
    values
        .iter()
        .map(|value| {
            if *value > 0.0 && value.is_finite() {
                *value
            } else {
                fallback
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_pptx_table(
    frame: &PptxTableFrame,
    page_number: usize,
    table_index: usize,
    theme: &Theme,
    nodes: &mut Vec<Node>,
    clips: &mut Vec<ClipPath>,
    warnings: &mut Vec<String>,
) {
    if frame.column_widths.is_empty() || frame.rows.is_empty() {
        warnings.push(format!(
            "PPTX table {} has no grid columns or rows",
            frame.name
        ));
        return;
    }
    let column_metrics = recover_table_metrics(&frame.column_widths);
    let row_metrics =
        recover_table_metrics(&frame.rows.iter().map(|row| row.height).collect::<Vec<_>>());
    let raw_width = column_metrics.iter().sum::<f64>();
    let raw_height = row_metrics.iter().sum::<f64>();
    if (frame.width <= 0.0 && raw_width <= 0.0) || (frame.height <= 0.0 && raw_height <= 0.0) {
        warnings.push(format!(
            "PPTX table {} has invalid grid extents",
            frame.name
        ));
        return;
    }
    let width = if frame.width > 0.0 {
        frame.width
    } else {
        raw_width
    };
    let height = if frame.height > 0.0 {
        frame.height
    } else {
        raw_height
    };
    let scale_x = width / raw_width;
    let scale_y = height / raw_height;
    let mut column_positions = Vec::with_capacity(frame.column_widths.len() + 1);
    column_positions.push(frame.x);
    for column_width in &column_metrics {
        let next = column_positions.last().copied().unwrap_or(frame.x) + column_width * scale_x;
        column_positions.push(next);
    }
    let mut row_positions = Vec::with_capacity(frame.rows.len() + 1);
    row_positions.push(frame.y);
    for row_height in &row_metrics {
        let next = row_positions.last().copied().unwrap_or(frame.y) + row_height * scale_y;
        row_positions.push(next);
    }
    let transform = rotation_matrix(
        frame.rotation,
        frame.x + width / 2.0,
        frame.y + height / 2.0,
    );
    let accent = theme.color("accent1");
    let border_color = "#1F2937";
    let band_color = transformed_drawingml_color(&accent, "tint", 90_000.0);
    for (row_index, row) in frame.rows.iter().enumerate() {
        for (column_index, cell) in row.cells.iter().enumerate() {
            if column_index >= frame.column_widths.len() {
                warnings.push(format!(
                    "PPTX table {} row {} has more cells than grid columns",
                    frame.name,
                    row_index + 1
                ));
                break;
            }
            if cell.horizontal_merge || cell.vertical_merge {
                continue;
            }
            let end_column = (column_index + cell.grid_span)
                .min(frame.column_widths.len())
                .max(column_index + 1);
            let end_row = (row_index + cell.row_span)
                .min(frame.rows.len())
                .max(row_index + 1);
            let x = column_positions[column_index];
            let y = row_positions[row_index];
            let cell_width = column_positions[end_column] - x;
            let cell_height = row_positions[end_row] - y;
            let fallback_fill = if frame.first_row && row_index == 0 {
                Paint::solid(accent.clone())
            } else if frame.band_row && row_index % 2 == 1 {
                Paint::solid(band_color.clone())
            } else {
                Paint::solid("#FFFFFF")
            };
            let fill = cell.fill.clone().unwrap_or(fallback_fill);
            let source_id = format!(
                "{}!R{}C{}",
                if frame.name.is_empty() {
                    format!("Table {table_index}")
                } else {
                    frame.name.clone()
                },
                row_index + 1,
                column_index + 1
            );
            let meta = SourceMeta {
                kind: "table-cell".into(),
                source_id,
                semantic_role: "table-cell".into(),
                ..SourceMeta::default()
            };
            nodes.push(Node::Path {
                id: format!(
                    "pptx-table-{page_number}-{table_index}-cell-{}-{}",
                    row_index + 1,
                    column_index + 1
                ),
                d: rectangle_path(x, y, cell_width, cell_height),
                fill_rule: "nonzero".into(),
                fill,
                stroke: Stroke {
                    paint: Paint::solid(border_color),
                    width: 0.75,
                    miter_limit: 10.0,
                    ..Stroke::default()
                },
                transform,
                clip_id: None,
                meta: meta.clone(),
            });
            let clip_id = format!(
                "pptx-table-{page_number}-{table_index}-clip-{}-{}",
                row_index + 1,
                column_index + 1
            );
            clips.push(ClipPath {
                id: clip_id.clone(),
                d: rectangle_path(x, y, cell_width, cell_height),
                transform,
                fill_rule: "nonzero".into(),
                parent_id: None,
                additional_paths: Vec::new(),
            });
            let available_width = (cell_width - cell.margin_left - cell.margin_right).max(4.0);
            let mut lines = Vec::<(Vec<TextRun>, TextAnchor, f64)>::new();
            for paragraph in &cell.paragraphs {
                let anchor = paragraph.alignment.unwrap_or(TextAnchor::Start);
                let mut paragraph_runs = paragraph.runs.clone();
                for run in &mut paragraph_runs {
                    if run.font_family.is_empty() {
                        run.font_family.clone_from(&theme.minor_font);
                    }
                    if run.font_size <= 0.0 {
                        run.font_size = 12.0;
                    }
                    if matches!(run.fill, Paint::None) {
                        run.fill = if frame.first_row && row_index == 0 {
                            Paint::solid("#FFFFFF")
                        } else {
                            Paint::solid("#000000")
                        };
                    }
                }
                for runs in wrap_pptx_runs(&paragraph_runs, available_width) {
                    if runs.is_empty() {
                        continue;
                    }
                    let font_size = runs.iter().map(|run| run.font_size).fold(10.0, f64::max);
                    lines.push((runs, anchor, font_size * 1.15));
                }
            }
            let total_text_height = lines.iter().map(|(_, _, height)| height).sum::<f64>();
            let mut text_y = match cell.vertical_anchor.as_deref() {
                Some("b") => y + cell_height - cell.margin_bottom - total_text_height,
                Some("ctr") => y + (cell_height - total_text_height).max(0.0) / 2.0,
                None if frame.first_row && row_index == 0 => {
                    y + (cell_height - total_text_height).max(0.0) / 2.0
                }
                _ => y + cell.margin_top,
            };
            for (line_index, (runs, anchor, line_height)) in lines.into_iter().enumerate() {
                let font_size = runs.iter().map(|run| run.font_size).fold(10.0, f64::max);
                text_y += font_size;
                let text_x = match anchor {
                    TextAnchor::Start => x + cell.margin_left,
                    TextAnchor::Middle => x + cell_width / 2.0,
                    TextAnchor::End => x + cell_width - cell.margin_right,
                };
                nodes.push(Node::Text {
                    id: format!(
                        "pptx-table-{page_number}-{table_index}-text-{}-{}-{}",
                        row_index + 1,
                        column_index + 1,
                        line_index + 1
                    ),
                    x: text_x,
                    y: text_y,
                    runs,
                    anchor,
                    transform,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: Some(clip_id.clone()),
                    meta: SourceMeta {
                        kind: "table-cell-text".into(),
                        outer_shadow: cell.text_outer_shadow.clone(),
                        glow: cell.text_glow.clone(),
                        ..meta.clone()
                    },
                });
                text_y += line_height - font_size;
            }
        }
    }
}

#[derive(Default)]
struct EmbeddedObjectFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    relationship_id: String,
    preview_relationship_id: String,
    name: String,
    program_id: String,
}

fn parse_pptx_embedded_frames(
    xml: &[u8],
    page_number: usize,
    relationships: &Relationships,
    slide_part: &str,
    max_events: usize,
) -> Result<(Vec<Node>, Vec<String>)> {
    if !xml_contains_local_element(xml, b"oleObj") {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut frame = None::<EmbeddedObjectFrame>;
    let mut nodes = Vec::new();
    let mut warnings = Vec::new();
    let mut frame_index = 0usize;
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "slide embedded-object frames exceed {max_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if name == "graphicFrame" {
                    frame = Some(EmbeddedObjectFrame::default());
                } else {
                    apply_embedded_object_frame_event(&start, &name, &stack, frame.as_mut());
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                apply_embedded_object_frame_event(&start, &name, &stack, frame.as_mut());
            }
            Event::End(end) => {
                if local_name(end.name().as_ref()) == b"graphicFrame"
                    && let Some(frame) = frame.take()
                    && (!frame.relationship_id.is_empty() || !frame.program_id.is_empty())
                {
                    frame_index += 1;
                    let has_preview = !frame.preview_relationship_id.is_empty()
                        && (relationships
                            .target(&frame.preview_relationship_id, slide_part)
                            .is_some()
                            || relationships
                                .external_target(&frame.preview_relationship_id)
                                .is_some());
                    let label = if !frame.name.is_empty() {
                        frame.name.clone()
                    } else if !frame.program_id.is_empty() {
                        frame.program_id.clone()
                    } else {
                        "Embedded object".into()
                    };
                    if let Some(part) = relationships.target(&frame.relationship_id, slide_part) {
                        warnings.push(format!(
                            "embedded object {part} was rendered as a static {}; activation is unavailable in SVG",
                            if has_preview { "preview" } else { "placeholder" }
                        ));
                    } else if let Some(target) =
                        relationships.external_target(&frame.relationship_id)
                    {
                        warnings.push(format!(
                            "external embedded object {target} was rendered as a static {}; activation is unavailable in SVG",
                            if has_preview { "preview" } else { "placeholder" }
                        ));
                    } else {
                        warnings.push(format!(
                            "embedded-object relationship {} is missing; a static placeholder was rendered",
                            frame.relationship_id
                        ));
                    }
                    if !has_preview {
                        let width = frame.width.max(144.0);
                        let height = frame.height.max(72.0);
                        let meta = SourceMeta {
                            kind: "embedded-object-placeholder".into(),
                            source_id: label.clone(),
                            semantic_role: "embedded-object".into(),
                            alt_text: frame.program_id.clone(),
                            ..SourceMeta::default()
                        };
                        nodes.push(Node::Path {
                            id: format!("pptx-ole-{page_number}-{frame_index}"),
                            d: rectangle_path(frame.x, frame.y, width, height),
                            fill_rule: "nonzero".into(),
                            fill: Paint::solid("#F2F2F2"),
                            stroke: Stroke {
                                paint: Paint::solid("#7A7A7A"),
                                width: 1.0,
                                miter_limit: 10.0,
                                ..Stroke::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: meta.clone(),
                        });
                        nodes.push(Node::Text {
                            id: format!("pptx-ole-{page_number}-{frame_index}-label"),
                            x: frame.x + width / 2.0,
                            y: frame.y + height / 2.0 + 4.0,
                            runs: vec![TextRun {
                                text: label,
                                font_family: "Arial, sans-serif".into(),
                                font_size: 12.0,
                                fill: Paint::solid("#333333"),
                                ..TextRun::default()
                            }],
                            anchor: TextAnchor::Middle,
                            transform: IDENTITY,
                            opacity: 1.0,
                            stroke: Stroke::default(),
                            clip_id: None,
                            meta: SourceMeta {
                                kind: "embedded-object-placeholder-label".into(),
                                ..meta
                            },
                        });
                    }
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((nodes, deduplicate(warnings)))
}

fn apply_embedded_object_frame_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    frame: Option<&mut EmbeddedObjectFrame>,
) {
    let Some(frame) = frame else {
        return;
    };
    match name {
        "off"
            if stack.iter().any(|item| item == "xfrm")
                && !stack.iter().any(|item| item == "pic") =>
        {
            frame.x = parse_i64(attribute(start, b"x"), 0) as f64 / EMU_PER_POINT;
            frame.y = parse_i64(attribute(start, b"y"), 0) as f64 / EMU_PER_POINT;
        }
        "ext"
            if stack.iter().any(|item| item == "xfrm")
                && !stack.iter().any(|item| item == "pic") =>
        {
            frame.width = parse_i64(attribute(start, b"cx"), 0).max(0) as f64 / EMU_PER_POINT;
            frame.height = parse_i64(attribute(start, b"cy"), 0).max(0) as f64 / EMU_PER_POINT;
        }
        "cNvPr" if !stack.iter().any(|item| item == "pic") => {
            frame.name = attribute(start, b"name").unwrap_or_default();
        }
        "oleObj" => {
            frame.relationship_id = attribute(start, b"id").unwrap_or_default();
            frame.name = attribute(start, b"name").unwrap_or_else(|| frame.name.clone());
            frame.program_id = attribute(start, b"progId").unwrap_or_default();
        }
        "blip" if stack.iter().any(|item| item == "oleObj") => {
            frame.preview_relationship_id = attribute(start, b"embed").unwrap_or_default();
        }
        _ => {}
    }
}

#[derive(Default)]
struct SmartArtFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    data_relationship_id: String,
    name: String,
}

type SmartArtRender = (
    Vec<Node>,
    Vec<ClipPath>,
    Vec<MaskDefinition>,
    Vec<TilingPatternDefinition>,
    Vec<String>,
);

#[allow(clippy::too_many_arguments)]
fn parse_pptx_smartart_frames(
    xml: &[u8],
    page_number: usize,
    theme: &Theme,
    text_styles: &PresentationTextStyles,
    relationships: &Relationships,
    slide_part: &str,
    package: &mut ZipPackage<std::fs::File>,
    max_events: usize,
) -> Result<SmartArtRender> {
    if !xml_contains_local_element(xml, b"relIds") {
        return Ok((Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()));
    }
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut frame = None::<SmartArtFrame>;
    let mut frames = Vec::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "PPTX SmartArt frame event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if name == "graphicFrame" {
                    frame = Some(SmartArtFrame::default());
                } else {
                    apply_smartart_frame_event(&start, &name, &stack, frame.as_mut());
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                apply_smartart_frame_event(&start, &name, &stack, frame.as_mut());
            }
            Event::End(end) => {
                if local_name(end.name().as_ref()) == b"graphicFrame"
                    && let Some(frame) = frame.take()
                    && !frame.data_relationship_id.is_empty()
                {
                    frames.push(frame);
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    let mut nodes = Vec::new();
    let mut clips = Vec::new();
    let mut masks = Vec::new();
    let mut patterns = Vec::new();
    let mut warnings = Vec::new();
    for (index, frame) in frames.into_iter().enumerate() {
        let Some(data_part) = relationships.target(&frame.data_relationship_id, slide_part) else {
            warnings.push(format!(
                "SmartArt data relationship {} is missing",
                frame.data_relationship_id
            ));
            continue;
        };
        let Some(data_xml) = package.read_optional(&data_part)? else {
            warnings.push(format!("SmartArt data part {data_part} is missing"));
            continue;
        };
        let drawing_relationship_id =
            parse_smartart_drawing_relationship_id(&data_xml, max_events)?;
        let drawing_part = drawing_relationship_id
            .as_deref()
            .and_then(|id| relationships.target(id, slide_part))
            .or_else(|| relationship_target_of_type(relationships, slide_part, "/diagramDrawing"));
        let Some(drawing_part) = drawing_part else {
            warnings.push(format!(
                "SmartArt {} has no cached diagramDrawing relationship",
                frame.name
            ));
            continue;
        };
        let Some(drawing_xml) = package.read_optional(&drawing_part)? else {
            warnings.push(format!("SmartArt cached drawing {drawing_part} is missing"));
            continue;
        };
        let drawing_relationships = package.relationships(&drawing_part, max_events)?;
        let mut drawing_page = parse_slide(
            &drawing_xml,
            page_number,
            frame.width.max(1.0),
            frame.height.max(1.0),
            theme,
            &drawing_relationships,
            &drawing_part,
            package,
            max_events,
            &format!("smartart-{}", index + 1),
            &HashMap::new(),
            text_styles,
        )?;
        let translation = [1.0, 0.0, 0.0, 1.0, frame.x, frame.y];
        nodes.push(Node::Group {
            id: format!("pptx-smartart-{page_number}-{}", index + 1),
            nodes: std::mem::take(&mut drawing_page.nodes),
            transform: translation,
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                kind: "smartart".into(),
                source_id: if frame.name.is_empty() {
                    drawing_part.clone()
                } else {
                    frame.name
                },
                semantic_role: "diagram".into(),
                ..SourceMeta::default()
            },
        });
        clips.append(&mut drawing_page.clips);
        masks.append(&mut drawing_page.masks);
        patterns.append(&mut drawing_page.patterns);
        warnings.extend(
            drawing_page
                .warnings
                .into_iter()
                .map(|warning| format!("SmartArt: {warning}")),
        );
    }
    Ok((nodes, clips, masks, patterns, deduplicate(warnings)))
}

fn apply_smartart_frame_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    frame: Option<&mut SmartArtFrame>,
) {
    let Some(frame) = frame else {
        return;
    };
    match name {
        "off" if stack.iter().any(|item| item == "xfrm") => {
            frame.x = parse_i64(attribute(start, b"x"), 0) as f64 / EMU_PER_POINT;
            frame.y = parse_i64(attribute(start, b"y"), 0) as f64 / EMU_PER_POINT;
        }
        "ext" if stack.iter().any(|item| item == "xfrm") => {
            frame.width = parse_i64(attribute(start, b"cx"), 0).max(0) as f64 / EMU_PER_POINT;
            frame.height = parse_i64(attribute(start, b"cy"), 0).max(0) as f64 / EMU_PER_POINT;
        }
        "cNvPr" => frame.name = attribute(start, b"name").unwrap_or_default(),
        "relIds" => frame.data_relationship_id = attribute(start, b"dm").unwrap_or_default(),
        _ => {}
    }
}

fn parse_smartart_drawing_relationship_id(xml: &[u8], max_events: usize) -> Result<Option<String>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    for event_index in 0..max_events {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) | Event::Empty(start)
                if local_name(start.name().as_ref()) == b"dataModelExt" =>
            {
                return Ok(attribute(&start, b"relId"));
            }
            Event::Eof => return Ok(None),
            _ => {}
        }
        buffer.clear();
        if event_index + 1 == max_events {
            return Err(Error::LimitExceeded(
                "PPTX SmartArt data event limit exceeded".into(),
            ));
        }
    }
    Ok(None)
}

fn remove_placeholder_nodes(page: &mut Page) {
    page.nodes
        .retain(|node| node_meta(node).semantic_role != "placeholder");
}

fn node_meta(node: &Node) -> &SourceMeta {
    match node {
        Node::Path { meta, .. }
        | Node::Text { meta, .. }
        | Node::Image { meta, .. }
        | Node::Group { meta, .. } => meta,
    }
}

fn node_meta_mut(node: &mut Node) -> &mut SourceMeta {
    match node {
        Node::Path { meta, .. }
        | Node::Text { meta, .. }
        | Node::Image { meta, .. }
        | Node::Group { meta, .. } => meta,
    }
}

fn take_background(page: &mut Page) -> Option<Node> {
    page.nodes
        .iter()
        .position(|node| node_meta(node).kind == "background")
        .map(|index| page.nodes.remove(index))
}

fn merge_page_layers(mut master: Option<Page>, mut layout: Option<Page>, mut slide: Page) -> Page {
    let master_background = master.as_mut().and_then(take_background);
    let layout_background = layout.as_mut().and_then(take_background);
    let slide_background = take_background(&mut slide);
    let mut nodes = Vec::new();
    if let Some(background) = slide_background.or(layout_background).or(master_background) {
        nodes.push(background);
    }
    if let Some(mut master) = master {
        nodes.append(&mut master.nodes);
        slide.clips.append(&mut master.clips);
        slide.masks.append(&mut master.masks);
        slide.patterns.append(&mut master.patterns);
        for warning in master.warnings {
            slide.warn(format!("master: {warning}"));
        }
    }
    if let Some(mut layout) = layout {
        nodes.append(&mut layout.nodes);
        slide.clips.append(&mut layout.clips);
        slide.masks.append(&mut layout.masks);
        slide.patterns.append(&mut layout.patterns);
        for warning in layout.warnings {
            slide.warn(format!("layout: {warning}"));
        }
    }
    nodes.append(&mut slide.nodes);
    slide.nodes = nodes;
    slide
}

fn parse_presentation(xml: &[u8], max_events: usize) -> Result<(f64, f64, Vec<String>)> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut width = DEFAULT_SLIDE_WIDTH;
    let mut height = DEFAULT_SLIDE_HEIGHT;
    let mut slides = Vec::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "presentation.xml event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) | Event::Empty(start) => match local_name(start.name().as_ref()) {
                b"sldSz" => {
                    width = parse_i64(attribute(&start, b"cx"), 9_144_000) as f64 / EMU_PER_POINT;
                    height = parse_i64(attribute(&start, b"cy"), 6_858_000) as f64 / EMU_PER_POINT;
                }
                b"sldId" => {
                    if let Some(id) = qualified_attribute(&start, b"r:id") {
                        slides.push(id);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((width, height, slides))
}

#[derive(Clone, Debug)]
struct Theme {
    colors: HashMap<String, String>,
    major_font: String,
    minor_font: String,
    effect_styles: Vec<ThemeEffectStyle>,
}

#[derive(Clone, Debug, Default)]
struct ThemeEffectStyle {
    outer_shadow: Option<ShapeShadow>,
    glow: Option<GlowEffect>,
}

impl Default for Theme {
    fn default() -> Self {
        let colors = HashMap::from([
            ("dk1".into(), "#000000".into()),
            ("lt1".into(), "#FFFFFF".into()),
            ("dk2".into(), "#1F1F1F".into()),
            ("lt2".into(), "#F2F2F2".into()),
            ("accent1".into(), "#4472C4".into()),
            ("accent2".into(), "#ED7D31".into()),
            ("accent3".into(), "#A5A5A5".into()),
            ("accent4".into(), "#FFC000".into()),
            ("accent5".into(), "#5B9BD5".into()),
            ("accent6".into(), "#70AD47".into()),
            ("hlink".into(), "#0563C1".into()),
            ("folHlink".into(), "#954F72".into()),
        ]);
        Self {
            colors,
            major_font: "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into(),
            minor_font: "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into(),
            effect_styles: Vec::new(),
        }
    }
}

impl Theme {
    fn parse(xml: &[u8], max_events: usize) -> Result<Self> {
        let mut theme = Self::default();
        let mut reader = Reader::from_reader(xml);
        reader.config_mut().trim_text(true);
        let mut buffer = Vec::new();
        let mut stack = Vec::<String>::new();
        let mut current_effect_style = None::<ThemeEffectStyle>;
        let mut events = 0usize;
        loop {
            events += 1;
            if events > max_events {
                return Err(Error::LimitExceeded(
                    "theme XML event limit exceeded".into(),
                ));
            }
            match reader.read_event_into(&mut buffer)? {
                Event::Start(start) => {
                    let name =
                        String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                    if name == "effectStyle"
                        && stack.last().is_some_and(|item| item == "effectStyleLst")
                    {
                        current_effect_style = Some(ThemeEffectStyle::default());
                    } else if apply_theme_effect_event(
                        &start,
                        &name,
                        &stack,
                        &theme,
                        current_effect_style.as_mut(),
                    ) {
                    } else if matches!(name.as_str(), "srgbClr" | "sysClr") {
                        if let Some(parent) = stack.last() {
                            let value = if name == "sysClr" {
                                attribute(&start, b"lastClr").or_else(|| attribute(&start, b"val"))
                            } else {
                                attribute(&start, b"val")
                            };
                            if let Some(value) = value {
                                theme
                                    .colors
                                    .insert(parent.clone(), color_from_hex(&value, "#000000"));
                            }
                        }
                    } else if name == "latin" {
                        let font = attribute(&start, b"typeface").unwrap_or_default();
                        if !font.is_empty() && stack.iter().any(|item| item == "majorFont") {
                            theme.major_font = office_font_stack(&font);
                        } else if !font.is_empty() && stack.iter().any(|item| item == "minorFont") {
                            theme.minor_font = office_font_stack(&font);
                        }
                    }
                    stack.push(name);
                }
                Event::Empty(start) => {
                    let name =
                        String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                    if name == "effectStyle"
                        && stack.last().is_some_and(|item| item == "effectStyleLst")
                    {
                        theme.effect_styles.push(ThemeEffectStyle::default());
                    } else if apply_theme_effect_event(
                        &start,
                        &name,
                        &stack,
                        &theme,
                        current_effect_style.as_mut(),
                    ) {
                    } else if matches!(name.as_str(), "srgbClr" | "sysClr") {
                        if let Some(parent) = stack.last() {
                            let value = if name == "sysClr" {
                                attribute(&start, b"lastClr").or_else(|| attribute(&start, b"val"))
                            } else {
                                attribute(&start, b"val")
                            };
                            if let Some(value) = value {
                                theme
                                    .colors
                                    .insert(parent.clone(), color_from_hex(&value, "#000000"));
                            }
                        }
                    } else if name == "latin" {
                        let font = attribute(&start, b"typeface").unwrap_or_default();
                        if !font.is_empty() && stack.iter().any(|item| item == "majorFont") {
                            theme.major_font = office_font_stack(&font);
                        } else if !font.is_empty() && stack.iter().any(|item| item == "minorFont") {
                            theme.minor_font = office_font_stack(&font);
                        }
                    }
                }
                Event::End(end) => {
                    if local_name(end.name().as_ref()) == b"effectStyle"
                        && let Some(style) = current_effect_style.take()
                    {
                        theme.effect_styles.push(style);
                    }
                    stack.pop();
                }
                Event::Eof => break,
                _ => {}
            }
            buffer.clear();
        }
        Ok(theme)
    }

    fn color(&self, name: &str) -> String {
        let resolved = match name {
            "tx1" => "dk1",
            "tx2" => "dk2",
            "bg1" => "lt1",
            "bg2" => "lt2",
            _ => name,
        };
        self.colors
            .get(resolved)
            .cloned()
            .unwrap_or_else(|| "#000000".into())
    }

    fn effect_style(&self, index: usize) -> Option<&ThemeEffectStyle> {
        index
            .checked_sub(1)
            .and_then(|index| self.effect_styles.get(index))
    }
}

fn apply_theme_effect_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    theme: &Theme,
    style: Option<&mut ThemeEffectStyle>,
) -> bool {
    let Some(style) = style else {
        return false;
    };
    match name {
        "outerShdw" if stack.iter().any(|item| item == "effectLst") => {
            let (effect, rotate_with_shape) = drawingml_outer_shadow(start);
            style.outer_shadow = Some(ShapeShadow {
                effect,
                rotate_with_shape,
            });
            true
        }
        "glow" if stack.iter().any(|item| item == "effectLst") => {
            style.glow = Some(drawingml_glow(start));
            true
        }
        "srgbClr" | "sysClr" | "schemeClr"
            if stack
                .iter()
                .any(|item| matches!(item.as_str(), "outerShdw" | "glow")) =>
        {
            let color = match name {
                "schemeClr" => theme.color(&attribute(start, b"val").unwrap_or_default()),
                "sysClr" => color_from_hex(
                    &attribute(start, b"lastClr")
                        .or_else(|| attribute(start, b"val"))
                        .unwrap_or_default(),
                    "#000000",
                ),
                _ => color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000"),
            };
            if stack.iter().any(|item| item == "outerShdw") {
                if let Some(shadow) = style.outer_shadow.as_mut() {
                    shadow.effect.color = color;
                }
            } else if let Some(glow) = style.glow.as_mut() {
                glow.color = color;
            }
            true
        }
        "alpha" if stack.iter().any(|item| item == "outerShdw") => {
            if let Some(shadow) = style.outer_shadow.as_mut() {
                shadow.effect.opacity = (parse_i64(attribute(start, b"val"), 100_000) as f64
                    / 100_000.0)
                    .clamp(0.0, 1.0);
            }
            true
        }
        "alpha" if stack.iter().any(|item| item == "glow") => {
            if let Some(glow) = style.glow.as_mut() {
                glow.opacity = (parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0)
                    .clamp(0.0, 1.0);
            }
            true
        }
        "hueOff" | "hueMod" | "satOff" | "satMod" | "lumOff" | "lumMod" | "tint" | "shade"
        | "alphaOff" | "alphaMod"
            if stack.iter().any(|item| item == "outerShdw") =>
        {
            if let Some(shadow) = style.outer_shadow.as_mut() {
                let raw_value = parse_i64(attribute(start, b"val"), 0) as f64;
                if matches!(name, "alphaOff" | "alphaMod") {
                    shadow.effect.opacity =
                        transformed_alpha(shadow.effect.opacity, name, raw_value);
                } else {
                    shadow.effect.color =
                        transformed_drawingml_color(&shadow.effect.color, name, raw_value);
                }
            }
            true
        }
        "hueOff" | "hueMod" | "satOff" | "satMod" | "lumOff" | "lumMod" | "tint" | "shade"
        | "alphaOff" | "alphaMod"
            if stack.iter().any(|item| item == "glow") =>
        {
            if let Some(glow) = style.glow.as_mut() {
                apply_glow_color_transform(
                    glow,
                    name,
                    parse_i64(attribute(start, b"val"), 0) as f64,
                );
            }
            true
        }
        _ => false,
    }
}

fn parse_master_text_styles(
    xml: &[u8],
    theme: &Theme,
    max_events: usize,
) -> Result<PresentationTextStyles> {
    let mut result = PresentationTextStyles::default();
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut section = String::new();
    let mut current = None::<(usize, PptxTextStyle)>;
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "PPTX master text-style event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                match name.as_str() {
                    "titleStyle" | "bodyStyle" | "otherStyle" => section.clone_from(&name),
                    _ => apply_master_text_style_event(&start, &name, theme, &mut current),
                }
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                apply_master_text_style_event(&start, &name, theme, &mut current);
            }
            Event::End(end) => {
                let name = String::from_utf8_lossy(local_name(end.name().as_ref())).into_owned();
                if text_level(&name).is_some()
                    && let Some((level, style)) = current.take()
                {
                    let target = match section.as_str() {
                        "titleStyle" => &mut result.title,
                        "bodyStyle" => &mut result.body,
                        _ => &mut result.other,
                    };
                    if target.len() <= level {
                        target.resize(level + 1, PptxTextStyle::default());
                    }
                    target[level] = style;
                } else if matches!(name.as_str(), "titleStyle" | "bodyStyle" | "otherStyle") {
                    section.clear();
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(result)
}

fn apply_master_text_style_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    theme: &Theme,
    current: &mut Option<(usize, PptxTextStyle)>,
) {
    if let Some(level) = text_level(name) {
        *current = Some((
            level,
            PptxTextStyle {
                alignment: attribute(start, b"algn").map(|alignment| match alignment.as_str() {
                    "ctr" => TextAnchor::Middle,
                    "r" => TextAnchor::End,
                    _ => TextAnchor::Start,
                }),
                margin_left: parse_i64(attribute(start, b"marL"), 0) as f64 / EMU_PER_POINT,
                ..PptxTextStyle::default()
            },
        ));
        return;
    }
    let Some((_, style)) = current.as_mut() else {
        return;
    };
    match name {
        "defRPr" => {
            if let Some(size) = attribute(start, b"sz") {
                style.font_size = size.parse::<f64>().unwrap_or(0.0) / 100.0;
            }
            style.bold =
                attribute(start, b"b").is_some_and(|value| value == "1" || value == "true");
            style.italic =
                attribute(start, b"i").is_some_and(|value| value == "1" || value == "true");
        }
        "latin" | "ea" | "cs" => {
            if let Some(typeface) = attribute(start, b"typeface") {
                style.font_family = match typeface.as_str() {
                    "+mj-lt" | "+mj-ea" => theme.major_font.clone(),
                    "+mn-lt" | "+mn-ea" => theme.minor_font.clone(),
                    _ => office_font_stack(&typeface),
                };
            }
        }
        "srgbClr" => {
            style.fill = Some(Paint::solid(color_from_hex(
                &attribute(start, b"val").unwrap_or_default(),
                "#000000",
            )));
        }
        "schemeClr" => {
            style.fill = Some(Paint::solid(
                theme.color(&attribute(start, b"val").unwrap_or_default()),
            ));
        }
        "buChar" => style.bullet = attribute(start, b"char").unwrap_or_default(),
        "buNone" => style.bullet.clear(),
        _ => {}
    }
}

fn text_level(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("lvl")?.strip_suffix("pPr")?;
    digits.parse::<usize>().ok()?.checked_sub(1)
}

#[derive(Clone, Debug, Default)]
struct Shape {
    kind: ShapeKind,
    id: String,
    name: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    text_x: Option<f64>,
    text_y: Option<f64>,
    text_width: Option<f64>,
    text_height: Option<f64>,
    text_rotation: f64,
    text_margin_left: Option<f64>,
    text_margin_right: Option<f64>,
    text_margin_top: Option<f64>,
    text_margin_bottom: Option<f64>,
    rotation: f64,
    preset: String,
    fill: Paint,
    stroke: Stroke,
    paragraphs: Vec<Paragraph>,
    picture_relationship_id: String,
    media_relationship_id: String,
    media_kind: String,
    embedded_object_preview: bool,
    alt_text: String,
    custom_geometry: bool,
    custom_paths: Vec<CustomGeometryPath>,
    custom_guides: HashMap<String, String>,
    preset_adjustments: HashMap<String, f64>,
    placeholder_key: String,
    placeholder_type: String,
    has_explicit_transform: bool,
    vertical_anchor: String,
    gradient_stops: Vec<GradientStop>,
    gradient_current_offset: Option<f64>,
    gradient_angle: f64,
    gradient_kind: String,
    group_transform: Option<Matrix>,
    outer_shadow: Option<ShapeShadow>,
    text_outer_shadow: Option<OuterShadow>,
    glow: Option<GlowEffect>,
    text_glow: Option<GlowEffect>,
    has_direct_shape_effects: bool,
    image_crop: Option<ImageCrop>,
    pattern_fill: Option<DrawingPatternFill>,
    image_effects: Vec<ImageColorEffect>,
    duotone_color_index: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct ImageCrop {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

#[derive(Clone, Debug)]
struct DrawingPatternFill {
    preset: String,
    foreground: PatternColor,
    background: PatternColor,
}

#[derive(Clone, Debug)]
struct PatternColor {
    color: String,
    opacity: f64,
}

impl DrawingPatternFill {
    fn new(preset: String) -> Self {
        Self {
            preset,
            foreground: PatternColor {
                color: "#000000".into(),
                opacity: 1.0,
            },
            background: PatternColor {
                color: "#FFFFFF".into(),
                opacity: 1.0,
            },
        }
    }
}

#[derive(Clone, Debug)]
struct ShapeShadow {
    effect: OuterShadow,
    rotate_with_shape: bool,
}

#[derive(Clone, Copy, Debug)]
struct GroupTransform {
    offset_x: f64,
    offset_y: f64,
    extent_x: f64,
    extent_y: f64,
    child_offset_x: f64,
    child_offset_y: f64,
    child_extent_x: f64,
    child_extent_y: f64,
    rotation: f64,
    flip_horizontal: bool,
    flip_vertical: bool,
}

impl Default for GroupTransform {
    fn default() -> Self {
        Self {
            offset_x: 0.0,
            offset_y: 0.0,
            extent_x: 1.0,
            extent_y: 1.0,
            child_offset_x: 0.0,
            child_offset_y: 0.0,
            child_extent_x: 1.0,
            child_extent_y: 1.0,
            rotation: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct CustomGeometryPath {
    width: f64,
    height: f64,
    commands: Vec<CustomGeometryCommand>,
}

#[derive(Clone, Debug)]
enum CustomGeometryCommand {
    Move(Vec<(String, String)>),
    Line(Vec<(String, String)>),
    Cubic(Vec<(String, String)>),
    Quadratic(Vec<(String, String)>),
    Arc {
        width_radius: String,
        height_radius: String,
        start_angle: String,
        sweep_angle: String,
    },
    Close,
}

#[derive(Clone, Debug, Default)]
struct ShapeGeometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    rotation: f64,
    preset: String,
    vertical_anchor: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ShapeKind {
    #[default]
    Shape,
    Picture,
    Connector,
}

#[derive(Clone, Debug, Default)]
struct Paragraph {
    runs: Vec<TextRun>,
    alignment: Option<TextAnchor>,
    level: usize,
}

#[derive(Clone, Debug, Default)]
struct PptxTextStyle {
    font_family: String,
    font_size: f64,
    bold: bool,
    italic: bool,
    fill: Option<Paint>,
    alignment: Option<TextAnchor>,
    margin_left: f64,
    bullet: String,
}

#[derive(Clone, Debug, Default)]
struct PresentationTextStyles {
    title: Vec<PptxTextStyle>,
    body: Vec<PptxTextStyle>,
    other: Vec<PptxTextStyle>,
}

impl PresentationTextStyles {
    fn style(&self, placeholder_key: &str, level: usize) -> PptxTextStyle {
        let styles = if placeholder_key.contains("type:title")
            || placeholder_key.contains("type:ctrTitle")
        {
            &self.title
        } else if placeholder_key.contains("type:body")
            || placeholder_key.contains("type:obj")
            || placeholder_key.starts_with("idx:")
        {
            &self.body
        } else {
            &self.other
        };
        styles
            .get(level)
            .or_else(|| styles.first())
            .cloned()
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaintTarget {
    ShapeFill,
    ShapeStroke,
    Text,
}

#[derive(Clone, Copy, Debug, Default)]
struct AlternateContentState {
    parent_active: bool,
    branch_active: bool,
    selected: bool,
}

fn alternate_choice_is_supported(start: &quick_xml::events::BytesStart<'_>) -> bool {
    attribute(start, b"Requires").is_some_and(|value| {
        let mut requirements = value.split_whitespace();
        let Some(first) = requirements.next() else {
            return false;
        };
        first == "asvg" && requirements.all(|requirement| requirement == "asvg")
    })
}

fn alternate_content_is_active(states: &[AlternateContentState]) -> bool {
    states.last().is_none_or(|state| state.branch_active)
}

#[allow(clippy::too_many_arguments)]
fn parse_slide(
    xml: &[u8],
    page_number: usize,
    width: f64,
    height: f64,
    theme: &Theme,
    relationships: &Relationships,
    slide_part: &str,
    package: &mut ZipPackage<std::fs::File>,
    max_events: usize,
    id_namespace: &str,
    inherited_placeholders: &HashMap<String, ShapeGeometry>,
    text_styles: &PresentationTextStyles,
) -> Result<Page> {
    let mut page = Page::new(page_number, width, height, "pptx");
    if let Some(background_picture) = parse_background_picture(xml, max_events, page_number)? {
        let mut background_shape = Shape {
            kind: ShapeKind::Picture,
            id: format!("pptx-{id_namespace}-{page_number}-background-image"),
            name: "Slide background image".into(),
            x: 0.0,
            y: 0.0,
            width,
            height,
            picture_relationship_id: background_picture.relationship_id,
            image_crop: background_picture.crop,
            has_explicit_transform: true,
            ..Shape::default()
        };
        background_shape.alt_text = "Slide background image".into();
        append_shape(
            &mut page,
            background_shape,
            relationships,
            slide_part,
            package,
            theme,
            text_styles,
            id_namespace,
        )?;
        if let Some(node) = page.nodes.last_mut() {
            let meta = node_meta_mut(node);
            meta.kind = "background".into();
            meta.semantic_role = "background".into();
            meta.source_id = slide_part.into();
        }
    }
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut shape: Option<Shape> = None;
    let mut current_run: Option<TextRun> = None;
    let mut current_paragraph: Option<Paragraph> = None;
    let mut text_buffer = String::new();
    let mut events = 0usize;
    let mut shape_counter = 0usize;
    let mut background = None::<String>;
    let mut group_stack = Vec::<GroupTransform>::new();
    let mut alternate_content = Vec::<AlternateContentState>::new();
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "slide {page_number} exceeds {max_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if name == "AlternateContent" {
                    alternate_content.push(AlternateContentState {
                        parent_active: alternate_content_is_active(&alternate_content),
                        ..AlternateContentState::default()
                    });
                    stack.push(name);
                    buffer.clear();
                    continue;
                }
                if name == "Choice" {
                    if let Some(state) = alternate_content.last_mut() {
                        state.branch_active = state.parent_active
                            && !state.selected
                            && alternate_choice_is_supported(&start);
                        state.selected |= state.branch_active;
                    }
                    stack.push(name);
                    buffer.clear();
                    continue;
                }
                if name == "Fallback" {
                    if let Some(state) = alternate_content.last_mut() {
                        state.branch_active = state.parent_active && !state.selected;
                        state.selected |= state.branch_active;
                    }
                    stack.push(name);
                    buffer.clear();
                    continue;
                }
                if !alternate_content_is_active(&alternate_content) {
                    stack.push(name);
                    buffer.clear();
                    continue;
                }
                if name == "grpSp" {
                    group_stack.push(GroupTransform::default());
                } else if shape.is_none()
                    && let Some(group) = group_stack.last_mut()
                {
                    apply_group_transform_event(&start, &name, &stack, group);
                }
                let group_transform = current_group_transform(&group_stack);
                match name.as_str() {
                    "sp" => {
                        shape_counter += 1;
                        shape = Some(Shape {
                            id: format!("pptx-{id_namespace}-{page_number}-{shape_counter}"),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::None,
                                width: 1.0,
                                miter_limit: 10.0,
                                ..Stroke::default()
                            },
                            group_transform,
                            ..Shape::default()
                        });
                    }
                    "pic" => {
                        shape_counter += 1;
                        shape = Some(Shape {
                            kind: ShapeKind::Picture,
                            id: format!("pptx-{id_namespace}-{page_number}-{shape_counter}"),
                            embedded_object_preview: stack.iter().any(|item| item == "oleObj"),
                            group_transform,
                            ..Shape::default()
                        });
                    }
                    "cxnSp" => {
                        shape_counter += 1;
                        shape = Some(Shape {
                            kind: ShapeKind::Connector,
                            id: format!("pptx-{id_namespace}-{page_number}-{shape_counter}"),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#000000"),
                                width: 1.0,
                                miter_limit: 10.0,
                                ..Stroke::default()
                            },
                            preset: "line".into(),
                            group_transform,
                            ..Shape::default()
                        });
                    }
                    "p" if shape.is_some() && stack.iter().any(|item| item == "txBody") => {
                        current_paragraph = Some(Paragraph::default());
                    }
                    "r" if shape.is_some() && stack.iter().any(|item| item == "txBody") => {
                        current_run = Some(TextRun {
                            font_family: String::new(),
                            font_size: 0.0,
                            fill: Paint::None,
                            ..TextRun::default()
                        });
                    }
                    "t" if shape.is_some() && stack.iter().any(|item| item == "txBody") => {
                        text_buffer.clear();
                    }
                    _ => apply_start(
                        &start,
                        &name,
                        &stack,
                        shape.as_mut(),
                        current_run.as_mut(),
                        current_paragraph.as_mut(),
                        theme,
                        &mut background,
                    ),
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if !alternate_content_is_active(&alternate_content) {
                    buffer.clear();
                    continue;
                }
                if shape.is_none()
                    && let Some(group) = group_stack.last_mut()
                {
                    apply_group_transform_event(&start, &name, &stack, group);
                }
                if name == "br" {
                    if let Some(run) = current_run.as_mut() {
                        run.text.push('\n');
                    } else if let Some(paragraph) = current_paragraph.as_mut() {
                        paragraph.runs.push(TextRun {
                            text: "\n".into(),
                            font_family: String::new(),
                            font_size: 0.0,
                            fill: Paint::None,
                            ..TextRun::default()
                        });
                    }
                } else {
                    apply_start(
                        &start,
                        &name,
                        &stack,
                        shape.as_mut(),
                        current_run.as_mut(),
                        current_paragraph.as_mut(),
                        theme,
                        &mut background,
                    );
                }
            }
            Event::Text(text) => {
                if alternate_content_is_active(&alternate_content)
                    && stack.last().is_some_and(|item| item == "t")
                {
                    text_buffer.push_str(&text.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid slide text: {error}"))
                    })?);
                }
            }
            Event::End(end) => {
                let name = String::from_utf8_lossy(local_name(end.name().as_ref())).into_owned();
                if name == "AlternateContent" {
                    alternate_content.pop();
                    stack.pop();
                    buffer.clear();
                    continue;
                }
                if matches!(name.as_str(), "Choice" | "Fallback") {
                    if let Some(state) = alternate_content.last_mut() {
                        state.branch_active = false;
                    }
                    stack.pop();
                    buffer.clear();
                    continue;
                }
                if !alternate_content_is_active(&alternate_content) {
                    stack.pop();
                    buffer.clear();
                    continue;
                }
                match name.as_str() {
                    "t" => {
                        if current_run.is_none() {
                            current_run = Some(TextRun {
                                font_family: String::new(),
                                font_size: 0.0,
                                fill: Paint::None,
                                ..TextRun::default()
                            });
                        }
                        if let Some(run) = current_run.as_mut() {
                            run.text.push_str(&text_buffer);
                        }
                        text_buffer.clear();
                    }
                    "r" => {
                        if let (Some(paragraph), Some(run)) =
                            (current_paragraph.as_mut(), current_run.take())
                        {
                            paragraph.runs.push(run);
                        }
                    }
                    "p" => {
                        if let Some(run) = current_run.take() {
                            current_paragraph
                                .get_or_insert_with(Paragraph::default)
                                .runs
                                .push(run);
                        }
                        if let (Some(shape), Some(paragraph)) =
                            (shape.as_mut(), current_paragraph.take())
                        {
                            shape.paragraphs.push(paragraph);
                        }
                    }
                    "gradFill" => {
                        if let Some(shape) = shape.as_mut() {
                            shape.gradient_current_offset = None;
                        }
                    }
                    "sp" | "pic" | "cxnSp" => {
                        if let Some(mut finished) = shape.take() {
                            inherit_shape_geometry(&mut finished, inherited_placeholders);
                            append_shape(
                                &mut page,
                                finished,
                                relationships,
                                slide_part,
                                package,
                                theme,
                                text_styles,
                                id_namespace,
                            )?;
                        }
                    }
                    "grpSp" => {
                        group_stack.pop();
                    }
                    _ => {}
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if let Some(color) = background {
        page.nodes.insert(
            0,
            Node::Path {
                id: format!("pptx-{id_namespace}-{page_number}-background"),
                d: format!("M 0 0 H {} V {} H 0 Z", fmt(width), fmt(height)),
                fill_rule: "nonzero".into(),
                fill: Paint::solid(color),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    kind: "background".into(),
                    source_id: slide_part.into(),
                    ..SourceMeta::default()
                },
            },
        );
    }
    page.title = page
        .nodes
        .iter()
        .find_map(|node| match node {
            Node::Text { runs, .. } => {
                let value = runs.iter().map(|run| run.text.as_str()).collect::<String>();
                (!value.trim().is_empty()).then(|| value.trim().to_owned())
            }
            _ => None,
        })
        .unwrap_or_default();
    Ok(page)
}

#[derive(Clone, Debug)]
struct BackgroundPicture {
    relationship_id: String,
    crop: Option<ImageCrop>,
}

fn parse_background_picture(
    xml: &[u8],
    max_events: usize,
    page_number: usize,
) -> Result<Option<BackgroundPicture>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut relationship_id = None::<String>;
    let mut crop = None::<ImageCrop>;
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "slide {page_number} background exceeds {max_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if stack.iter().any(|item| item == "bg") {
                    collect_background_picture_event(
                        &start,
                        &name,
                        &stack,
                        &mut relationship_id,
                        &mut crop,
                    );
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if stack.iter().any(|item| item == "bg") {
                    collect_background_picture_event(
                        &start,
                        &name,
                        &stack,
                        &mut relationship_id,
                        &mut crop,
                    );
                }
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(relationship_id.map(|relationship_id| BackgroundPicture {
        relationship_id,
        crop,
    }))
}

fn collect_background_picture_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    relationship_id: &mut Option<String>,
    crop: &mut Option<ImageCrop>,
) {
    if matches!(name, "blip" | "svgBlip")
        && stack.iter().any(|item| item == "blipFill")
        && let Some(value) = attribute(start, b"embed").or_else(|| attribute(start, b"link"))
        && !value.is_empty()
    {
        *relationship_id = Some(value);
    } else if name == "srcRect" && stack.iter().any(|item| item == "blipFill") {
        *crop = Some(ImageCrop {
            left: parse_i64(attribute(start, b"l"), 0) as f64 / 100_000.0,
            top: parse_i64(attribute(start, b"t"), 0) as f64 / 100_000.0,
            right: parse_i64(attribute(start, b"r"), 0) as f64 / 100_000.0,
            bottom: parse_i64(attribute(start, b"b"), 0) as f64 / 100_000.0,
        });
    }
}

fn apply_group_transform_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    group: &mut GroupTransform,
) {
    match name {
        "xfrm" if stack.iter().any(|item| item == "grpSpPr") => {
            group.rotation = parse_i64(attribute(start, b"rot"), 0) as f64 / 60_000.0;
            group.flip_horizontal =
                attribute(start, b"flipH").is_some_and(|value| value == "1" || value == "true");
            group.flip_vertical =
                attribute(start, b"flipV").is_some_and(|value| value == "1" || value == "true");
        }
        "off" if stack.iter().any(|item| item == "xfrm") => {
            group.offset_x = parse_i64(attribute(start, b"x"), 0) as f64 / EMU_PER_POINT;
            group.offset_y = parse_i64(attribute(start, b"y"), 0) as f64 / EMU_PER_POINT;
        }
        "ext" if stack.iter().any(|item| item == "xfrm") => {
            group.extent_x =
                parse_i64(attribute(start, b"cx"), 12_700).max(1) as f64 / EMU_PER_POINT;
            group.extent_y =
                parse_i64(attribute(start, b"cy"), 12_700).max(1) as f64 / EMU_PER_POINT;
        }
        "chOff" if stack.iter().any(|item| item == "xfrm") => {
            group.child_offset_x = parse_i64(attribute(start, b"x"), 0) as f64 / EMU_PER_POINT;
            group.child_offset_y = parse_i64(attribute(start, b"y"), 0) as f64 / EMU_PER_POINT;
        }
        "chExt" if stack.iter().any(|item| item == "xfrm") => {
            group.child_extent_x =
                parse_i64(attribute(start, b"cx"), 12_700).max(1) as f64 / EMU_PER_POINT;
            group.child_extent_y =
                parse_i64(attribute(start, b"cy"), 12_700).max(1) as f64 / EMU_PER_POINT;
        }
        _ => {}
    }
}

fn current_group_transform(groups: &[GroupTransform]) -> Option<Matrix> {
    (!groups.is_empty()).then(|| {
        groups
            .iter()
            .fold(IDENTITY, |matrix, group| compose(matrix, group.matrix()))
    })
}

impl GroupTransform {
    fn matrix(&self) -> Matrix {
        let scale_x = self.extent_x / self.child_extent_x.max(1e-12);
        let scale_y = self.extent_y / self.child_extent_y.max(1e-12);
        let base = [
            scale_x,
            0.0,
            0.0,
            scale_y,
            self.offset_x - self.child_offset_x * scale_x,
            self.offset_y - self.child_offset_y * scale_y,
        ];
        let center_x = self.offset_x + self.extent_x / 2.0;
        let center_y = self.offset_y + self.extent_y / 2.0;
        let horizontal = if self.flip_horizontal { -1.0 } else { 1.0 };
        let vertical = if self.flip_vertical { -1.0 } else { 1.0 };
        let flip = [
            horizontal,
            0.0,
            0.0,
            vertical,
            center_x * (1.0 - horizontal),
            center_y * (1.0 - vertical),
        ];
        compose(
            rotation_matrix(self.rotation, center_x, center_y),
            compose(flip, base),
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_start(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    shape: Option<&mut Shape>,
    current_run: Option<&mut TextRun>,
    current_paragraph: Option<&mut Paragraph>,
    theme: &Theme,
    background: &mut Option<String>,
) {
    if stack
        .iter()
        .any(|item| matches!(item.as_str(), "hiddenFill" | "hiddenLine" | "hiddenEffects"))
    {
        return;
    }
    let in_background = stack.iter().any(|item| item == "bgPr");
    let target = paint_target(stack);
    let mut shape = shape;
    match name {
        "cNvPr" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.name = attribute(start, b"name").unwrap_or_default();
                shape.alt_text = attribute(start, b"descr").unwrap_or_default();
            }
        }
        "off" if stack.iter().any(|item| item == "txXfrm") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.text_x = Some(parse_i64(attribute(start, b"x"), 0) as f64 / EMU_PER_POINT);
                shape.text_y = Some(parse_i64(attribute(start, b"y"), 0) as f64 / EMU_PER_POINT);
            }
        }
        "ext" if stack.iter().any(|item| item == "txXfrm") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.text_width =
                    Some(parse_i64(attribute(start, b"cx"), 0).max(0) as f64 / EMU_PER_POINT);
                shape.text_height =
                    Some(parse_i64(attribute(start, b"cy"), 0).max(0) as f64 / EMU_PER_POINT);
            }
        }
        "off" if stack.iter().any(|item| item == "xfrm") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.has_explicit_transform = true;
                shape.x = parse_i64(attribute(start, b"x"), 0) as f64 / EMU_PER_POINT;
                shape.y = parse_i64(attribute(start, b"y"), 0) as f64 / EMU_PER_POINT;
            }
        }
        "ext" if stack.iter().any(|item| item == "xfrm") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.has_explicit_transform = true;
                shape.width = parse_i64(attribute(start, b"cx"), 0) as f64 / EMU_PER_POINT;
                shape.height = parse_i64(attribute(start, b"cy"), 0) as f64 / EMU_PER_POINT;
            }
        }
        "xfrm" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.rotation = parse_i64(attribute(start, b"rot"), 0) as f64 / 60_000.0;
            }
        }
        "txXfrm" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.text_rotation = parse_i64(attribute(start, b"rot"), 0) as f64 / 60_000.0;
            }
        }
        "prstGeom" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.preset = attribute(start, b"prst").unwrap_or_else(|| "rect".into());
            }
        }
        "custGeom" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.custom_geometry = true;
            }
        }
        "gd" if stack.iter().any(|item| item == "custGeom") => {
            if let Some(shape) = shape.as_deref_mut()
                && let (Some(guide_name), Some(formula)) =
                    (attribute(start, b"name"), attribute(start, b"fmla"))
            {
                shape.custom_guides.insert(guide_name, formula);
            }
        }
        "gd" if stack.iter().any(|item| item == "prstGeom")
            && stack.iter().any(|item| item == "avLst") =>
        {
            if let Some(shape) = shape.as_deref_mut()
                && let (Some(guide_name), Some(formula)) =
                    (attribute(start, b"name"), attribute(start, b"fmla"))
                && let Some(value) = formula
                    .strip_prefix("val ")
                    .and_then(|value| value.parse::<f64>().ok())
            {
                shape.preset_adjustments.insert(guide_name, value);
            }
        }
        "path"
            if stack.iter().any(|item| item == "pathLst")
                && stack.iter().any(|item| item == "custGeom") =>
        {
            if let Some(shape) = shape.as_deref_mut() {
                shape.custom_paths.push(CustomGeometryPath {
                    width: attribute(start, b"w")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(21_600.0),
                    height: attribute(start, b"h")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(21_600.0),
                    commands: Vec::new(),
                });
            }
        }
        "moveTo" if stack.iter().any(|item| item == "custGeom") => {
            push_custom_geometry_command(
                shape.as_deref_mut(),
                CustomGeometryCommand::Move(Vec::new()),
            );
        }
        "lnTo" if stack.iter().any(|item| item == "custGeom") => {
            push_custom_geometry_command(
                shape.as_deref_mut(),
                CustomGeometryCommand::Line(Vec::new()),
            );
        }
        "cubicBezTo" if stack.iter().any(|item| item == "custGeom") => {
            push_custom_geometry_command(
                shape.as_deref_mut(),
                CustomGeometryCommand::Cubic(Vec::new()),
            );
        }
        "quadBezTo" if stack.iter().any(|item| item == "custGeom") => {
            push_custom_geometry_command(
                shape.as_deref_mut(),
                CustomGeometryCommand::Quadratic(Vec::new()),
            );
        }
        "arcTo" if stack.iter().any(|item| item == "custGeom") => {
            push_custom_geometry_command(
                shape.as_deref_mut(),
                CustomGeometryCommand::Arc {
                    width_radius: attribute(start, b"wR").unwrap_or_else(|| "0".into()),
                    height_radius: attribute(start, b"hR").unwrap_or_else(|| "0".into()),
                    start_angle: attribute(start, b"stAng").unwrap_or_else(|| "0".into()),
                    sweep_angle: attribute(start, b"swAng").unwrap_or_else(|| "0".into()),
                },
            );
        }
        "close" if stack.iter().any(|item| item == "custGeom") => {
            push_custom_geometry_command(shape.as_deref_mut(), CustomGeometryCommand::Close);
        }
        "pt" if stack.iter().any(|item| item == "custGeom") => {
            if let Some(path) = shape
                .as_deref_mut()
                .and_then(|shape| shape.custom_paths.last_mut())
            {
                let point = (
                    attribute(start, b"x").unwrap_or_else(|| "0".into()),
                    attribute(start, b"y").unwrap_or_else(|| "0".into()),
                );
                if let Some(
                    CustomGeometryCommand::Move(points)
                    | CustomGeometryCommand::Line(points)
                    | CustomGeometryCommand::Cubic(points)
                    | CustomGeometryCommand::Quadratic(points),
                ) = path.commands.last_mut()
                {
                    points.push(point);
                }
            }
        }
        "glow"
            if stack.iter().any(|item| item == "effectLst")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(shape) = shape.as_deref_mut() {
                let effect = drawingml_glow(start);
                if stack
                    .iter()
                    .any(|item| matches!(item.as_str(), "rPr" | "defRPr" | "endParaRPr"))
                {
                    shape.text_glow = Some(effect);
                } else if stack.iter().any(|item| item == "spPr") {
                    shape.glow = Some(effect);
                }
            }
        }
        "bodyPr" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.vertical_anchor = attribute(start, b"anchor").unwrap_or_default();
                shape.text_margin_left =
                    Some(parse_i64(attribute(start, b"lIns"), 91_440) as f64 / EMU_PER_POINT);
                shape.text_margin_right =
                    Some(parse_i64(attribute(start, b"rIns"), 91_440) as f64 / EMU_PER_POINT);
                shape.text_margin_top =
                    Some(parse_i64(attribute(start, b"tIns"), 45_720) as f64 / EMU_PER_POINT);
                shape.text_margin_bottom =
                    Some(parse_i64(attribute(start, b"bIns"), 45_720) as f64 / EMU_PER_POINT);
            }
        }
        "ph" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.placeholder_key = placeholder_key(start);
                shape.placeholder_type = attribute(start, b"type").unwrap_or_else(|| "body".into());
            }
        }
        "ln" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.stroke.width =
                    parse_i64(attribute(start, b"w"), 12_700) as f64 / EMU_PER_POINT;
            }
        }
        "prstDash" => {
            if let Some(shape) = shape.as_deref_mut() {
                let width = shape.stroke.width.max(0.25);
                shape.stroke.dash_array = match attribute(start, b"val").as_deref() {
                    Some("dash") | Some("sysDash") => vec![4.0 * width, 3.0 * width],
                    Some("dot") | Some("sysDot") => vec![width, 2.0 * width],
                    Some("dashDot") => vec![4.0 * width, 2.0 * width, width, 2.0 * width],
                    _ => Vec::new(),
                };
            }
        }
        "effectLst"
            if stack.iter().any(|item| item == "spPr")
                && !stack.iter().any(|item| {
                    matches!(
                        item.as_str(),
                        "style" | "rPr" | "defRPr" | "endParaRPr" | "hiddenEffects"
                    )
                }) =>
        {
            if let Some(shape) = shape.as_deref_mut() {
                shape.has_direct_shape_effects = true;
            }
        }
        "outerShdw"
            if stack.iter().any(|item| item == "effectLst")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(shape) = shape.as_deref_mut() {
                let (effect, rotate_with_shape) = drawingml_outer_shadow(start);
                if stack
                    .iter()
                    .any(|item| matches!(item.as_str(), "rPr" | "defRPr" | "endParaRPr"))
                {
                    shape.text_outer_shadow = Some(effect);
                } else if stack.iter().any(|item| item == "spPr") {
                    shape.outer_shadow = Some(ShapeShadow {
                        effect,
                        rotate_with_shape,
                    });
                }
            }
        }
        "effectRef" if stack.iter().any(|item| item == "style") => {
            if let Some(shape) = shape.as_deref_mut()
                && !shape.has_direct_shape_effects
                && let Some(style) =
                    theme.effect_style(parse_i64(attribute(start, b"idx"), 0).max(0) as usize)
            {
                if shape.outer_shadow.is_none() {
                    shape.outer_shadow.clone_from(&style.outer_shadow);
                }
                if shape.glow.is_none() {
                    shape.glow.clone_from(&style.glow);
                }
            }
        }
        "round" if target == PaintTarget::ShapeStroke => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.stroke.line_join = LineJoin::Round;
            }
        }
        "bevel" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.stroke.line_join = LineJoin::Bevel;
            }
        }
        "pattFill" => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.pattern_fill = Some(DrawingPatternFill::new(
                    attribute(start, b"prst").unwrap_or_default(),
                ));
            }
        }
        "gs" if stack.iter().any(|item| item == "gradFill") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.gradient_current_offset =
                    Some(parse_i64(attribute(start, b"pos"), 0) as f64 / 100_000.0);
            }
        }
        "lin" if stack.iter().any(|item| item == "gradFill") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.gradient_angle = parse_i64(attribute(start, b"ang"), 0) as f64 / 60_000.0;
                shape.gradient_kind = "linear".into();
            }
        }
        "path" if stack.iter().any(|item| item == "gradFill") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.gradient_kind = attribute(start, b"path").unwrap_or_else(|| "circle".into());
            }
        }
        "alpha"
            if stack
                .iter()
                .any(|item| matches!(item.as_str(), "duotone" | "clrChange")) =>
        {
            if let Some(shape) = shape.as_deref_mut() {
                apply_image_effect_alpha(
                    shape,
                    stack,
                    parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0,
                );
            }
        }
        "alpha" if stack.iter().any(|item| item == "gradFill") => {
            if let Some(shape) = shape.as_deref_mut()
                && let Some(stop) = shape.gradient_stops.last_mut()
            {
                stop.opacity = parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0;
            }
        }
        "alpha" if stack.iter().any(|item| item == "pattFill") => {
            if let Some(color) = shape
                .as_deref_mut()
                .and_then(|shape| active_pattern_color_mut(shape, stack))
            {
                color.opacity = (parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0)
                    .clamp(0.0, 1.0);
            }
        }
        "alpha"
            if stack.iter().any(|item| item == "outerShdw")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(shadow) = shape
                .as_deref_mut()
                .and_then(|shape| active_outer_shadow_mut(shape, stack))
            {
                shadow.opacity = (parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0)
                    .clamp(0.0, 1.0);
            }
        }
        "alpha"
            if stack.iter().any(|item| item == "glow")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(glow) = shape
                .as_deref_mut()
                .and_then(|shape| active_glow_mut(shape, stack))
            {
                glow.opacity = (parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0)
                    .clamp(0.0, 1.0);
            }
        }
        "alpha" => {
            if stack.iter().any(|item| item == "style") {
                return;
            }
            apply_paint_opacity(
                shape.as_deref_mut(),
                current_run,
                target,
                parse_i64(attribute(start, b"val"), 100_000) as f64 / 100_000.0,
            );
        }
        "hueOff" | "hueMod" | "satOff" | "satMod" | "lumOff" | "lumMod" | "tint" | "shade"
        | "alphaOff" | "alphaMod"
            if stack
                .iter()
                .any(|item| matches!(item.as_str(), "duotone" | "clrChange")) =>
        {
            if let Some(shape) = shape.as_deref_mut() {
                apply_image_effect_color_transform(
                    shape,
                    stack,
                    name,
                    parse_i64(attribute(start, b"val"), 0) as f64,
                );
            }
        }
        "hueOff" | "hueMod" | "satOff" | "satMod" | "lumOff" | "lumMod" | "tint" | "shade"
        | "alphaOff" | "alphaMod"
            if stack.iter().any(|item| item == "pattFill") =>
        {
            if let Some(color) = shape
                .as_deref_mut()
                .and_then(|shape| active_pattern_color_mut(shape, stack))
            {
                let raw_value = parse_i64(attribute(start, b"val"), 0) as f64;
                if matches!(name, "alphaOff" | "alphaMod") {
                    color.opacity = transformed_alpha(color.opacity, name, raw_value);
                } else {
                    color.color = transformed_drawingml_color(&color.color, name, raw_value);
                }
            }
        }
        "hueOff" | "hueMod" | "satOff" | "satMod" | "lumOff" | "lumMod" | "tint" | "shade"
        | "alphaOff" | "alphaMod"
            if stack.iter().any(|item| item == "glow")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(glow) = shape
                .as_deref_mut()
                .and_then(|shape| active_glow_mut(shape, stack))
            {
                apply_glow_color_transform(
                    glow,
                    name,
                    parse_i64(attribute(start, b"val"), 0) as f64,
                );
            }
        }
        "hueOff" | "hueMod" | "satOff" | "satMod" | "lumOff" | "lumMod" | "tint" | "shade"
        | "alphaOff" | "alphaMod" => {
            if stack.iter().any(|item| item == "style") {
                return;
            }
            apply_drawingml_color_transform(
                shape.as_deref_mut(),
                current_run,
                target,
                name,
                parse_i64(attribute(start, b"val"), 0) as f64,
                stack.iter().any(|item| item == "gradFill"),
            );
        }
        "srgbClr" | "sysClr" | "schemeClr" | "prstClr"
            if stack
                .iter()
                .any(|item| matches!(item.as_str(), "duotone" | "clrChange")) =>
        {
            if let Some(shape) = shape.as_deref_mut() {
                let color = match name {
                    "schemeClr" => theme.color(&attribute(start, b"val").unwrap_or_default()),
                    "sysClr" => color_from_hex(
                        &attribute(start, b"lastClr")
                            .or_else(|| attribute(start, b"val"))
                            .unwrap_or_default(),
                        "#000000",
                    ),
                    "prstClr" => {
                        drawingml_preset_color(&attribute(start, b"val").unwrap_or_default())
                    }
                    _ => color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000"),
                };
                set_image_effect_color(shape, stack, color);
            }
        }
        "srgbClr" | "sysClr" | "schemeClr" if stack.iter().any(|item| item == "pattFill") => {
            if let Some(color) = shape
                .as_deref_mut()
                .and_then(|shape| active_pattern_color_mut(shape, stack))
            {
                color.color = match name {
                    "schemeClr" => theme.color(&attribute(start, b"val").unwrap_or_default()),
                    "sysClr" => color_from_hex(
                        &attribute(start, b"lastClr")
                            .or_else(|| attribute(start, b"val"))
                            .unwrap_or_default(),
                        "#000000",
                    ),
                    _ => color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000"),
                };
            }
        }
        "srgbClr"
            if stack.iter().any(|item| item == "outerShdw")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(shadow) = shape
                .as_deref_mut()
                .and_then(|shape| active_outer_shadow_mut(shape, stack))
            {
                shadow.color =
                    color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000");
            }
        }
        "schemeClr"
            if stack.iter().any(|item| item == "outerShdw")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(shadow) = shape
                .as_deref_mut()
                .and_then(|shape| active_outer_shadow_mut(shape, stack))
            {
                shadow.color = theme.color(&attribute(start, b"val").unwrap_or_default());
            }
        }
        "srgbClr"
            if stack.iter().any(|item| item == "glow")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(glow) = shape
                .as_deref_mut()
                .and_then(|shape| active_glow_mut(shape, stack))
            {
                glow.color =
                    color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000");
            }
        }
        "schemeClr"
            if stack.iter().any(|item| item == "glow")
                && !stack.iter().any(|item| item == "hiddenEffects") =>
        {
            if let Some(glow) = shape
                .as_deref_mut()
                .and_then(|shape| active_glow_mut(shape, stack))
            {
                glow.color = theme.color(&attribute(start, b"val").unwrap_or_default());
            }
        }
        "srgbClr" => {
            if stack.iter().any(|item| item == "style") {
                return;
            }
            let color = color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000");
            if stack.iter().any(|item| item == "gradFill") {
                append_gradient_stop(shape.as_deref_mut(), color.clone());
            } else {
                apply_color(shape.as_deref_mut(), current_run, target, color.clone());
            }
            if in_background {
                *background = Some(color);
            }
        }
        "schemeClr" => {
            if stack.iter().any(|item| item == "style") {
                return;
            }
            let color = theme.color(&attribute(start, b"val").unwrap_or_default());
            if stack.iter().any(|item| item == "gradFill") {
                append_gradient_stop(shape.as_deref_mut(), color.clone());
            } else {
                apply_color(shape.as_deref_mut(), current_run, target, color.clone());
            }
            if in_background {
                *background = Some(color);
            }
        }
        "noFill" => {
            if let Some(shape) = shape {
                match target {
                    PaintTarget::ShapeStroke => shape.stroke.paint = Paint::None,
                    PaintTarget::ShapeFill => {
                        shape.fill = Paint::None;
                        shape.gradient_stops.clear();
                    }
                    PaintTarget::Text => {}
                }
            }
        }
        "duotone" if stack.iter().any(|item| item == "blip") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.image_effects.push(ImageColorEffect::Duotone {
                    dark: "#000000".into(),
                    light: "#FFFFFF".into(),
                });
                shape.duotone_color_index = 0;
            }
        }
        "grayscl" if stack.iter().any(|item| item == "blip") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.image_effects.push(ImageColorEffect::Grayscale);
            }
        }
        "lum" if stack.iter().any(|item| item == "blip") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.image_effects.push(ImageColorEffect::Luminance {
                    brightness: (parse_i64(attribute(start, b"bright"), 0) as f64 / 100_000.0)
                        .clamp(-1.0, 1.0),
                    contrast: (parse_i64(attribute(start, b"contrast"), 0) as f64 / 100_000.0)
                        .clamp(-1.0, 1.0),
                });
            }
        }
        "clrChange" if stack.iter().any(|item| item == "blip") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.image_effects.push(ImageColorEffect::ColorChange {
                    from: "#000000".into(),
                    to: "#000000".into(),
                    to_opacity: 1.0,
                });
            }
        }
        "srcRect" if stack.iter().any(|item| item == "blipFill") => {
            if let Some(shape) = shape.as_deref_mut() {
                shape.image_crop = Some(ImageCrop {
                    left: parse_i64(attribute(start, b"l"), 0) as f64 / 100_000.0,
                    top: parse_i64(attribute(start, b"t"), 0) as f64 / 100_000.0,
                    right: parse_i64(attribute(start, b"r"), 0) as f64 / 100_000.0,
                    bottom: parse_i64(attribute(start, b"b"), 0) as f64 / 100_000.0,
                });
            }
        }
        "blip" | "svgBlip" => {
            if let Some(shape) = shape
                && let Some(relationship_id) =
                    attribute(start, b"embed").or_else(|| attribute(start, b"link"))
                && !relationship_id.is_empty()
            {
                shape.picture_relationship_id = relationship_id;
            }
        }
        "videoFile" | "audioFile" => {
            if let Some(shape) = shape {
                shape.media_relationship_id = attribute(start, b"link").unwrap_or_default();
                shape.media_kind = name.trim_end_matches("File").into();
            }
        }
        "media" if stack.iter().any(|item| item == "nvPr") => {
            if let Some(shape) = shape
                && let Some(relationship_id) = attribute(start, b"embed")
            {
                shape.media_relationship_id = relationship_id;
                if shape.media_kind.is_empty() {
                    shape.media_kind = "video".into();
                }
            }
        }
        "rPr" | "defRPr" => {
            if let Some(run) = current_run {
                if let Some(size) = attribute(start, b"sz") {
                    run.font_size = size.parse::<f64>().unwrap_or(1_200.0) / 100.0;
                }
                if let Some(bold) = attribute(start, b"b") {
                    run.bold = bold == "1" || bold == "true";
                }
                if let Some(italic) = attribute(start, b"i") {
                    run.italic = italic == "1" || italic == "true";
                }
            }
        }
        "latin" | "ea" | "cs" => {
            if let Some(run) = current_run
                && let Some(typeface) = attribute(start, b"typeface")
            {
                run.font_family = match typeface.as_str() {
                    "+mj-lt" | "+mj-ea" => theme.major_font.clone(),
                    "+mn-lt" | "+mn-ea" => theme.minor_font.clone(),
                    _ if !typeface.is_empty() => office_font_stack(&typeface),
                    _ => run.font_family.clone(),
                };
            }
        }
        "pPr" => {
            if let Some(paragraph) = current_paragraph {
                paragraph.level = parse_i64(attribute(start, b"lvl"), 0).max(0) as usize;
                paragraph.alignment =
                    attribute(start, b"algn").map(|alignment| match alignment.as_str() {
                        "ctr" => TextAnchor::Middle,
                        "r" => TextAnchor::End,
                        _ => TextAnchor::Start,
                    });
            }
        }
        "hlinkClick" => {
            if let Some(run) = current_run {
                run.fill = Paint::solid(theme.color("hlink"));
            }
        }
        _ => {}
    }
}

fn active_outer_shadow_mut<'a>(
    shape: &'a mut Shape,
    stack: &[String],
) -> Option<&'a mut OuterShadow> {
    if stack.iter().any(|item| item == "hiddenEffects") {
        return None;
    }
    if stack
        .iter()
        .any(|item| matches!(item.as_str(), "rPr" | "defRPr" | "endParaRPr"))
    {
        shape.text_outer_shadow.as_mut()
    } else {
        shape.outer_shadow.as_mut().map(|shadow| &mut shadow.effect)
    }
}

fn active_glow_mut<'a>(shape: &'a mut Shape, stack: &[String]) -> Option<&'a mut GlowEffect> {
    if stack.iter().any(|item| item == "hiddenEffects") {
        return None;
    }
    if stack
        .iter()
        .any(|item| matches!(item.as_str(), "rPr" | "defRPr" | "endParaRPr"))
    {
        shape.text_glow.as_mut()
    } else {
        shape.glow.as_mut()
    }
}

fn active_pattern_color_mut<'a>(
    shape: &'a mut Shape,
    stack: &[String],
) -> Option<&'a mut PatternColor> {
    let pattern = shape.pattern_fill.as_mut()?;
    if stack.iter().any(|item| item == "fgClr") {
        Some(&mut pattern.foreground)
    } else if stack.iter().any(|item| item == "bgClr") {
        Some(&mut pattern.background)
    } else {
        None
    }
}

fn drawingml_preset_color(name: &str) -> String {
    match name {
        "white" => "#FFFFFF",
        "black" => "#000000",
        "red" => "#FF0000",
        "green" => "#008000",
        "blue" => "#0000FF",
        "yellow" => "#FFFF00",
        "gray" | "grey" => "#808080",
        "ltGray" | "ltGrey" => "#D3D3D3",
        "dkGray" | "dkGrey" => "#A9A9A9",
        _ => "#000000",
    }
    .into()
}

fn set_image_effect_color(shape: &mut Shape, stack: &[String], color: String) {
    if stack.iter().any(|item| item == "duotone") {
        let index = shape.duotone_color_index.min(1);
        if let Some(ImageColorEffect::Duotone { dark, light }) = shape.image_effects.last_mut() {
            if index == 0 {
                *dark = color;
            } else {
                *light = color;
            }
            shape.duotone_color_index = shape.duotone_color_index.saturating_add(1);
        }
    } else if stack.iter().any(|item| item == "clrChange")
        && let Some(ImageColorEffect::ColorChange { from, to, .. }) = shape.image_effects.last_mut()
    {
        if stack.iter().any(|item| item == "clrFrom") {
            *from = color;
        } else if stack.iter().any(|item| item == "clrTo") {
            *to = color;
        }
    }
}

fn apply_image_effect_color_transform(
    shape: &mut Shape,
    stack: &[String],
    operation: &str,
    raw_value: f64,
) {
    if stack.iter().any(|item| item == "duotone") {
        let index = shape.duotone_color_index.saturating_sub(1).min(1);
        if let Some(ImageColorEffect::Duotone { dark, light }) = shape.image_effects.last_mut() {
            let target = if index == 0 { dark } else { light };
            *target = transformed_drawingml_color(target, operation, raw_value);
        }
    } else if stack.iter().any(|item| item == "clrChange")
        && let Some(ImageColorEffect::ColorChange { from, to, .. }) = shape.image_effects.last_mut()
    {
        let target = if stack.iter().any(|item| item == "clrFrom") {
            from
        } else {
            to
        };
        *target = transformed_drawingml_color(target, operation, raw_value);
    }
}

fn apply_image_effect_alpha(shape: &mut Shape, stack: &[String], opacity: f64) {
    if stack.iter().any(|item| item == "clrTo")
        && let Some(ImageColorEffect::ColorChange { to_opacity, .. }) =
            shape.image_effects.last_mut()
    {
        *to_opacity = opacity.clamp(0.0, 1.0);
    }
}

fn push_custom_geometry_command(shape: Option<&mut Shape>, command: CustomGeometryCommand) {
    if let Some(path) = shape.and_then(|shape| shape.custom_paths.last_mut()) {
        path.commands.push(command);
    }
}

fn paint_target(stack: &[String]) -> PaintTarget {
    if stack.iter().any(|item| item == "ln") {
        PaintTarget::ShapeStroke
    } else if stack
        .iter()
        .any(|item| matches!(item.as_str(), "rPr" | "defRPr" | "endParaRPr" | "buClr"))
    {
        PaintTarget::Text
    } else {
        PaintTarget::ShapeFill
    }
}

fn apply_color(
    shape: Option<&mut Shape>,
    run: Option<&mut TextRun>,
    target: PaintTarget,
    color: String,
) {
    match target {
        PaintTarget::Text => {
            if let Some(run) = run {
                run.fill = Paint::solid(color);
            }
        }
        PaintTarget::ShapeStroke => {
            if let Some(shape) = shape {
                shape.stroke.paint = Paint::solid(color);
            }
        }
        PaintTarget::ShapeFill => {
            if let Some(shape) = shape {
                shape.fill = Paint::solid(color);
            }
        }
    }
}

fn apply_paint_opacity(
    shape: Option<&mut Shape>,
    run: Option<&mut TextRun>,
    target: PaintTarget,
    opacity: f64,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    let paint = match target {
        PaintTarget::Text => run.map(|run| &mut run.fill),
        PaintTarget::ShapeStroke => shape.map(|shape| &mut shape.stroke.paint),
        PaintTarget::ShapeFill => shape.map(|shape| &mut shape.fill),
    };
    let Some(paint) = paint else {
        return;
    };
    set_paint_opacity(paint, opacity);
}

fn set_paint_opacity(paint: &mut Paint, opacity: f64) {
    let opacity = opacity.clamp(0.0, 1.0);
    match paint {
        Paint::Solid {
            opacity: current, ..
        } => *current = opacity,
        Paint::LinearGradient(gradient) => {
            for stop in &mut gradient.stops {
                stop.opacity = opacity;
            }
        }
        Paint::RadialGradient(gradient) => {
            for stop in &mut gradient.stops {
                stop.opacity = opacity;
            }
        }
        Paint::PatternRef {
            opacity: current, ..
        } => *current = opacity,
        Paint::None => {}
    }
}

fn apply_drawingml_color_transform(
    shape: Option<&mut Shape>,
    run: Option<&mut TextRun>,
    target: PaintTarget,
    operation: &str,
    raw_value: f64,
    in_gradient: bool,
) {
    if in_gradient {
        if let Some(stop) = shape.and_then(|shape| shape.gradient_stops.last_mut()) {
            if matches!(operation, "alphaOff" | "alphaMod") {
                stop.opacity = transformed_alpha(stop.opacity, operation, raw_value);
            } else {
                stop.color = transformed_drawingml_color(&stop.color, operation, raw_value);
            }
        }
        return;
    }
    let paint = match target {
        PaintTarget::Text => run.map(|run| &mut run.fill),
        PaintTarget::ShapeStroke => shape.map(|shape| &mut shape.stroke.paint),
        PaintTarget::ShapeFill => shape.map(|shape| &mut shape.fill),
    };
    let Some(paint) = paint else {
        return;
    };
    match paint {
        Paint::Solid { color, opacity } => {
            if matches!(operation, "alphaOff" | "alphaMod") {
                *opacity = transformed_alpha(*opacity, operation, raw_value);
            } else {
                *color = transformed_drawingml_color(color, operation, raw_value);
            }
        }
        Paint::LinearGradient(gradient) => {
            for stop in &mut gradient.stops {
                if matches!(operation, "alphaOff" | "alphaMod") {
                    stop.opacity = transformed_alpha(stop.opacity, operation, raw_value);
                } else {
                    stop.color = transformed_drawingml_color(&stop.color, operation, raw_value);
                }
            }
        }
        Paint::RadialGradient(gradient) => {
            for stop in &mut gradient.stops {
                if matches!(operation, "alphaOff" | "alphaMod") {
                    stop.opacity = transformed_alpha(stop.opacity, operation, raw_value);
                } else {
                    stop.color = transformed_drawingml_color(&stop.color, operation, raw_value);
                }
            }
        }
        Paint::PatternRef { opacity, .. } if matches!(operation, "alphaOff" | "alphaMod") => {
            *opacity = transformed_alpha(*opacity, operation, raw_value);
        }
        Paint::PatternRef { .. } | Paint::None => {}
    }
}

fn transformed_alpha(opacity: f64, operation: &str, raw_value: f64) -> f64 {
    match operation {
        "alphaOff" => (opacity + raw_value / 100_000.0).clamp(0.0, 1.0),
        "alphaMod" => (opacity * raw_value / 100_000.0).clamp(0.0, 1.0),
        _ => opacity,
    }
}

fn transformed_drawingml_color(color: &str, operation: &str, raw_value: f64) -> String {
    let Some(mut rgb) = parse_hex_rgb(color) else {
        return color.into();
    };
    let factor = raw_value / 100_000.0;
    match operation {
        "tint" => {
            for channel in &mut rgb {
                *channel += (1.0 - *channel) * factor.clamp(0.0, 1.0);
            }
        }
        "shade" => {
            for channel in &mut rgb {
                *channel *= factor.clamp(0.0, 1.0);
            }
        }
        _ => {
            let (mut hue, mut saturation, mut lightness) = rgb_to_hsl(rgb);
            match operation {
                "hueOff" => hue = (hue + raw_value / 60_000.0).rem_euclid(360.0),
                "hueMod" => hue = (hue * factor).rem_euclid(360.0),
                "satOff" => saturation = (saturation + factor).clamp(0.0, 1.0),
                "satMod" => saturation = (saturation * factor).clamp(0.0, 1.0),
                "lumOff" => lightness = (lightness + factor).clamp(0.0, 1.0),
                "lumMod" => lightness = (lightness * factor).clamp(0.0, 1.0),
                _ => return color.into(),
            }
            rgb = hsl_to_rgb(hue, saturation, lightness);
        }
    }
    format!(
        "#{:02X}{:02X}{:02X}",
        (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8
    )
}

fn parse_hex_rgb(color: &str) -> Option<[f64; 3]> {
    let color = color.trim_start_matches('#');
    if color.len() != 6 {
        return None;
    }
    Some([
        f64::from(u8::from_str_radix(&color[0..2], 16).ok()?) / 255.0,
        f64::from(u8::from_str_radix(&color[2..4], 16).ok()?) / 255.0,
        f64::from(u8::from_str_radix(&color[4..6], 16).ok()?) / 255.0,
    ])
}

fn rgb_to_hsl(rgb: [f64; 3]) -> (f64, f64, f64) {
    let maximum = rgb.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let minimum = rgb.into_iter().fold(f64::INFINITY, f64::min);
    let lightness = (maximum + minimum) * 0.5;
    let delta = maximum - minimum;
    if delta <= 1e-12 {
        return (0.0, 0.0, lightness);
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs()).max(1e-12);
    let hue = if (maximum - rgb[0]).abs() <= 1e-12 {
        60.0 * ((rgb[1] - rgb[2]) / delta).rem_euclid(6.0)
    } else if (maximum - rgb[1]).abs() <= 1e-12 {
        60.0 * ((rgb[2] - rgb[0]) / delta + 2.0)
    } else {
        60.0 * ((rgb[0] - rgb[1]) / delta + 4.0)
    };
    (hue, saturation.clamp(0.0, 1.0), lightness)
}

fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> [f64; 3] {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let x = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let rgb = match sector.floor() as usize {
        0 => [chroma, x, 0.0],
        1 => [x, chroma, 0.0],
        2 => [0.0, chroma, x],
        3 => [0.0, x, chroma],
        4 => [x, 0.0, chroma],
        _ => [chroma, 0.0, x],
    };
    let match_value = lightness - chroma * 0.5;
    rgb.map(|channel| channel + match_value)
}

fn append_gradient_stop(shape: Option<&mut Shape>, color: String) {
    let Some(shape) = shape else {
        return;
    };
    shape.gradient_stops.push(GradientStop {
        offset: shape.gradient_current_offset.unwrap_or(0.0).clamp(0.0, 1.0),
        color,
        opacity: 1.0,
    });
}

fn finalize_shape_gradient(shape: &mut Shape) {
    if shape.gradient_stops.is_empty() {
        return;
    }
    shape.gradient_stops.sort_by(|left, right| {
        left.offset
            .partial_cmp(&right.offset)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if shape.gradient_stops.len() == 1 {
        let mut second = shape.gradient_stops[0].clone();
        second.offset = 1.0;
        shape.gradient_stops.push(second);
    }
    let stops = std::mem::take(&mut shape.gradient_stops);
    if shape.gradient_kind == "circle" || shape.gradient_kind == "rect" {
        shape.fill = Paint::RadialGradient(Box::new(RadialGradient {
            fx: shape.x + shape.width / 2.0,
            fy: shape.y + shape.height / 2.0,
            fr: 0.0,
            cx: shape.x + shape.width / 2.0,
            cy: shape.y + shape.height / 2.0,
            radius: shape.width.max(shape.height) / 2.0,
            transform: IDENTITY,
            stops,
        }));
        return;
    }
    let radians = shape.gradient_angle * PI / 180.0;
    let direction_x = radians.cos();
    let direction_y = radians.sin();
    let half_length =
        direction_x.abs() * shape.width / 2.0 + direction_y.abs() * shape.height / 2.0;
    let center_x = shape.x + shape.width / 2.0;
    let center_y = shape.y + shape.height / 2.0;
    shape.fill = Paint::LinearGradient(Box::new(LinearGradient {
        x1: center_x - direction_x * half_length,
        y1: center_y - direction_y * half_length,
        x2: center_x + direction_x * half_length,
        y2: center_y + direction_y * half_length,
        stops,
    }));
}

fn finalize_shape_pattern(page: &mut Page, shape: &mut Shape, id_namespace: &str) {
    let Some(pattern) = shape.pattern_fill.take() else {
        return;
    };
    let pattern_id = format!(
        "pptx-pattern-{}-{}-{}",
        id_namespace,
        page.number,
        page.patterns.len() + 1
    );
    let foreground = Paint::Solid {
        color: pattern.foreground.color,
        opacity: pattern.foreground.opacity.clamp(0.0, 1.0),
    };
    let background = Paint::Solid {
        color: pattern.background.color,
        opacity: pattern.background.opacity.clamp(0.0, 1.0),
    };
    let (width, height, mut nodes) = match pattern.preset.as_str() {
        "pct5" => {
            let mut nodes = vec![pattern_fill_node(
                &pattern_id,
                1,
                "M 0 0 H 8 V 8 H 0 Z".into(),
                background,
            )];
            nodes.push(pattern_fill_node(
                &pattern_id,
                2,
                "M 0 0 H 1.8 V 1.8 H 0 Z".into(),
                foreground,
            ));
            (8.0, 8.0, nodes)
        }
        "pct90" => {
            let mut nodes = vec![pattern_fill_node(
                &pattern_id,
                1,
                "M 0 0 H 8 V 8 H 0 Z".into(),
                foreground,
            )];
            nodes.push(pattern_fill_node(
                &pattern_id,
                2,
                "M 0 0 H 2.53 V 2.53 H 0 Z".into(),
                background,
            ));
            (8.0, 8.0, nodes)
        }
        "narHorz" => {
            let mut nodes = vec![pattern_fill_node(
                &pattern_id,
                1,
                "M 0 0 H 2 V 2 H 0 Z".into(),
                background,
            )];
            nodes.push(pattern_fill_node(
                &pattern_id,
                2,
                "M 0 0 H 2 V 0.4 H 0 Z".into(),
                foreground,
            ));
            (2.0, 2.0, nodes)
        }
        "narVert" => {
            let mut nodes = vec![pattern_fill_node(
                &pattern_id,
                1,
                "M 0 0 H 1.5 V 1.5 H 0 Z".into(),
                background,
            )];
            nodes.push(pattern_fill_node(
                &pattern_id,
                2,
                "M 0 0 H 0.3 V 1.5 H 0 Z".into(),
                foreground,
            ));
            (1.5, 1.5, nodes)
        }
        "wdUpDiag" | "dkUpDiag" => {
            let tile = if pattern.preset == "dkUpDiag" {
                8.0
            } else {
                12.0
            };
            let stroke_width = if pattern.preset == "dkUpDiag" {
                3.0
            } else {
                2.0
            };
            let mut nodes = vec![pattern_fill_node(
                &pattern_id,
                1,
                format!("M 0 0 H {tile} V {tile} H 0 Z"),
                background,
            )];
            nodes.push(pattern_stroke_node(
                &pattern_id,
                2,
                format!(
                    "M -2 {tile} L {tile} -2 M 0 {} L {} 0",
                    tile + 2.0,
                    tile + 2.0
                ),
                foreground,
                stroke_width,
            ));
            (tile, tile, nodes)
        }
        "wdDnDiag" => {
            let mut nodes = vec![pattern_fill_node(
                &pattern_id,
                1,
                "M 0 0 H 12 V 12 H 0 Z".into(),
                background,
            )];
            nodes.push(pattern_stroke_node(
                &pattern_id,
                2,
                "M -2 0 L 12 14 M 0 -2 L 14 12".into(),
                foreground,
                2.0,
            ));
            (12.0, 12.0, nodes)
        }
        "openDmnd" => {
            let mut nodes = vec![pattern_fill_node(
                &pattern_id,
                1,
                "M 0 0 H 12 V 12 H 0 Z".into(),
                background,
            )];
            nodes.push(pattern_stroke_node(
                &pattern_id,
                2,
                "M 0 6 L 6 0 L 12 6 L 6 12 Z".into(),
                foreground,
                1.0,
            ));
            (12.0, 12.0, nodes)
        }
        unsupported => {
            page.warn(format!(
                "PPTX pattern preset {unsupported} is approximated by its background color"
            ));
            shape.fill = background;
            return;
        }
    };
    for node in &mut nodes {
        match node {
            Node::Path { meta, .. }
            | Node::Text { meta, .. }
            | Node::Image { meta, .. }
            | Node::Group { meta, .. } => {
                meta.source_id.clone_from(&shape.name);
            }
        }
    }
    page.patterns.push(TilingPatternDefinition {
        id: pattern_id.clone(),
        x: shape.x,
        y: shape.y,
        width,
        height,
        transform: IDENTITY,
        nodes,
    });
    shape.fill = Paint::PatternRef {
        id: pattern_id,
        opacity: 1.0,
    };
}

fn pattern_fill_node(id: &str, index: usize, d: String, fill: Paint) -> Node {
    Node::Path {
        id: format!("{id}-cell-{index}"),
        d,
        fill_rule: "nonzero".into(),
        fill,
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            kind: "drawingml-pattern-cell".into(),
            ..SourceMeta::default()
        },
    }
}

fn pattern_stroke_node(id: &str, index: usize, d: String, paint: Paint, width: f64) -> Node {
    Node::Path {
        id: format!("{id}-line-{index}"),
        d,
        fill_rule: "nonzero".into(),
        fill: Paint::None,
        stroke: Stroke {
            paint,
            width,
            miter_limit: 10.0,
            ..Stroke::default()
        },
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            kind: "drawingml-pattern-line".into(),
            ..SourceMeta::default()
        },
    }
}

fn placeholder_key(start: &quick_xml::events::BytesStart<'_>) -> String {
    if let Some(index) = attribute(start, b"idx")
        && !index.is_empty()
    {
        return format!("idx:{index}");
    }
    format!(
        "type:{}",
        attribute(start, b"type").unwrap_or_else(|| "body".into())
    )
}

fn inherit_shape_geometry(
    shape: &mut Shape,
    inherited_placeholders: &HashMap<String, ShapeGeometry>,
) {
    if shape.placeholder_key.is_empty() {
        return;
    }
    let geometry = inherited_placeholders
        .get(&shape.placeholder_key)
        .or_else(|| {
            matches!(
                shape.placeholder_type.as_str(),
                "title" | "ctrTitle" | "subTitle" | "ftr" | "sldNum" | "dt"
            )
            .then(|| inherited_placeholders.get(&format!("type:{}", shape.placeholder_type)))
            .flatten()
        });
    let Some(geometry) = geometry else {
        return;
    };
    if !shape.has_explicit_transform {
        shape.x = geometry.x;
        shape.y = geometry.y;
        shape.width = geometry.width;
        shape.height = geometry.height;
        shape.rotation = geometry.rotation;
        shape.vertical_anchor.clone_from(&geometry.vertical_anchor);
    } else {
        if shape.width <= 0.0 {
            shape.width = geometry.width;
        }
        if shape.height <= 0.0 {
            shape.height = geometry.height;
        }
    }
    if shape.preset.is_empty() {
        shape.preset.clone_from(&geometry.preset);
    }
    if shape.vertical_anchor.is_empty() {
        shape.vertical_anchor.clone_from(&geometry.vertical_anchor);
    }
}

fn parse_placeholder_geometries(
    xml: &[u8],
    inherited: &HashMap<String, ShapeGeometry>,
    max_events: usize,
) -> Result<HashMap<String, ShapeGeometry>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut current = None::<Shape>;
    let mut result = inherited.clone();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "PPTX placeholder geometry event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if name == "sp" {
                    current = Some(Shape::default());
                } else {
                    apply_placeholder_geometry_event(&start, &name, &stack, current.as_mut());
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                apply_placeholder_geometry_event(&start, &name, &stack, current.as_mut());
            }
            Event::End(end) => {
                if local_name(end.name().as_ref()) == b"sp"
                    && let Some(mut shape) = current.take()
                    && !shape.placeholder_key.is_empty()
                {
                    inherit_shape_geometry(&mut shape, inherited);
                    let geometry = ShapeGeometry {
                        x: shape.x,
                        y: shape.y,
                        width: shape.width,
                        height: shape.height,
                        rotation: shape.rotation,
                        preset: shape.preset,
                        vertical_anchor: shape.vertical_anchor,
                    };
                    if matches!(
                        shape.placeholder_type.as_str(),
                        "title" | "ctrTitle" | "subTitle" | "ftr" | "sldNum" | "dt"
                    ) && geometry.width > 0.0
                        && geometry.height > 0.0
                    {
                        result
                            .entry(format!("type:{}", shape.placeholder_type))
                            .or_insert_with(|| geometry.clone());
                    }
                    result.insert(shape.placeholder_key, geometry);
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(result)
}

fn apply_placeholder_geometry_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    shape: Option<&mut Shape>,
) {
    let Some(shape) = shape else {
        return;
    };
    match name {
        "ph" => {
            shape.placeholder_key = placeholder_key(start);
            shape.placeholder_type = attribute(start, b"type").unwrap_or_else(|| "body".into());
        }
        "off" if stack.iter().any(|item| item == "xfrm") => {
            shape.has_explicit_transform = true;
            shape.x = parse_i64(attribute(start, b"x"), 0) as f64 / EMU_PER_POINT;
            shape.y = parse_i64(attribute(start, b"y"), 0) as f64 / EMU_PER_POINT;
        }
        "ext" if stack.iter().any(|item| item == "xfrm") => {
            shape.has_explicit_transform = true;
            shape.width = parse_i64(attribute(start, b"cx"), 0) as f64 / EMU_PER_POINT;
            shape.height = parse_i64(attribute(start, b"cy"), 0) as f64 / EMU_PER_POINT;
        }
        "xfrm" => {
            shape.rotation = parse_i64(attribute(start, b"rot"), 0) as f64 / 60_000.0;
        }
        "prstGeom" => {
            shape.preset = attribute(start, b"prst").unwrap_or_else(|| "rect".into());
        }
        "bodyPr" => {
            shape.vertical_anchor = attribute(start, b"anchor").unwrap_or_default();
        }
        _ => {}
    }
}

#[derive(Clone, Copy, Debug)]
struct ImageGeometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    cropped: bool,
}

fn resolve_image_geometry(shape: &Shape) -> std::result::Result<ImageGeometry, String> {
    let Some(crop) = shape.image_crop else {
        return Ok(ImageGeometry {
            x: shape.x,
            y: shape.y,
            width: shape.width,
            height: shape.height,
            cropped: false,
        });
    };
    if [crop.left, crop.top, crop.right, crop.bottom]
        .iter()
        .all(|value| value.abs() <= 1e-12)
    {
        return Ok(ImageGeometry {
            x: shape.x,
            y: shape.y,
            width: shape.width,
            height: shape.height,
            cropped: false,
        });
    }
    if [crop.left, crop.top, crop.right, crop.bottom]
        .iter()
        .any(|value| !value.is_finite() || value.abs() > 10.0)
    {
        return Err("srcRect crop value exceeds the bounded ±1000% range".into());
    }
    let visible_width = 1.0 - crop.left - crop.right;
    let visible_height = 1.0 - crop.top - crop.bottom;
    if visible_width <= 1e-4 || visible_height <= 1e-4 {
        return Err("srcRect leaves less than 0.01% visible image area".into());
    }
    let geometry = ImageGeometry {
        x: shape.x - shape.width * crop.left / visible_width,
        y: shape.y - shape.height * crop.top / visible_height,
        width: shape.width / visible_width,
        height: shape.height / visible_height,
        cropped: true,
    };
    if [geometry.x, geometry.y, geometry.width, geometry.height]
        .iter()
        .any(|value| !value.is_finite() || value.abs() > 1e9)
    {
        return Err("srcRect produces unbounded image geometry".into());
    }
    Ok(geometry)
}

fn push_pptx_image_node(
    page: &mut Page,
    id: &str,
    href: String,
    geometry: ImageGeometry,
    transform: Matrix,
    clip_id: Option<String>,
    meta: &SourceMeta,
) {
    let has_effect =
        meta.outer_shadow.is_some() || meta.glow.is_some() || !meta.image_effects.is_empty();
    if clip_id.is_some() && has_effect {
        let mut source_meta = meta.clone();
        source_meta.outer_shadow = None;
        source_meta.glow = None;
        source_meta.image_effects.clear();
        page.nodes.push(Node::Group {
            id: id.into(),
            nodes: vec![Node::Image {
                id: format!("{id}-source"),
                href,
                x: geometry.x,
                y: geometry.y,
                width: geometry.width,
                height: geometry.height,
                transform,
                opacity: 1.0,
                clip_id,
                meta: source_meta,
            }],
            transform: IDENTITY,
            opacity: 1.0,
            clip_id: None,
            meta: meta.clone(),
        });
    } else {
        page.nodes.push(Node::Image {
            id: id.into(),
            href,
            x: geometry.x,
            y: geometry.y,
            width: geometry.width,
            height: geometry.height,
            transform,
            opacity: 1.0,
            clip_id,
            meta: meta.clone(),
        });
    }
}

fn shape_has_visible_text(shape: &Shape) -> bool {
    shape
        .paragraphs
        .iter()
        .any(|paragraph| paragraph.runs.iter().any(|run| !run.text.trim().is_empty()))
}

fn apply_default_placeholder_geometry(
    shape: &mut Shape,
    page_width: f64,
    page_height: f64,
) -> bool {
    let (x, y, width, height): (f64, f64, f64, f64) = match shape.placeholder_type.as_str() {
        "sldNum" => (page_width - 78.0, page_height - 24.0, 60.0, 16.0),
        "ftr" => (18.0, page_height - 24.0, page_width * 0.65, 16.0),
        "dt" => (18.0, page_height - 24.0, 120.0, 16.0),
        _ => return false,
    };
    shape.x = x;
    shape.y = y;
    shape.width = width.max(1.0);
    shape.height = height.max(1.0);
    shape.has_explicit_transform = true;
    true
}

#[allow(clippy::too_many_arguments)]
fn append_shape(
    page: &mut Page,
    mut shape: Shape,
    relationships: &Relationships,
    slide_part: &str,
    package: &mut ZipPackage<std::fs::File>,
    theme: &Theme,
    text_styles: &PresentationTextStyles,
    id_namespace: &str,
) -> Result<()> {
    if (shape.width <= 0.0 || shape.height <= 0.0) && shape.kind != ShapeKind::Connector {
        let visible_text = shape_has_visible_text(&shape);
        let recovered = id_namespace == "slide"
            && visible_text
            && apply_default_placeholder_geometry(&mut shape, page.width, page.height);
        if !recovered {
            if id_namespace == "slide" && visible_text {
                page.warn(format!(
                    "shape {} has no explicit transform; placeholder inheritance is pending",
                    shape.name
                ));
            }
            return Ok(());
        }
    }
    finalize_shape_gradient(&mut shape);
    finalize_shape_pattern(page, &mut shape, id_namespace);
    let transform = compose(
        shape.group_transform.unwrap_or(IDENTITY),
        rotation_matrix(
            shape.rotation,
            shape.x + shape.width / 2.0,
            shape.y + shape.height / 2.0,
        ),
    );
    let mut meta = SourceMeta {
        kind: match shape.kind {
            ShapeKind::Picture => "image",
            ShapeKind::Connector | ShapeKind::Shape => "vector",
        }
        .into(),
        source_id: if shape.name.is_empty() {
            shape.id.clone()
        } else {
            shape.name.clone()
        },
        semantic_role: if shape.placeholder_key.is_empty() {
            String::new()
        } else {
            "placeholder".into()
        },
        alt_text: shape.alt_text.clone(),
        ..SourceMeta::default()
    };
    if let Some(mut shadow) = shape
        .outer_shadow
        .as_ref()
        .map(|shadow| shadow.effect.clone())
    {
        if shape
            .outer_shadow
            .as_ref()
            .is_some_and(|shadow| shadow.rotate_with_shape)
        {
            shadow.direction_degrees += shape.rotation;
        }
        meta.outer_shadow = Some(shadow);
    }
    meta.glow.clone_from(&shape.glow);
    meta.image_effects.clone_from(&shape.image_effects);
    if shape.kind == ShapeKind::Picture {
        if shape.embedded_object_preview {
            meta.kind = "embedded-object-preview".into();
            meta.semantic_role = "embedded-object".into();
        } else if !shape.media_kind.is_empty() {
            meta.kind = format!("{}-poster", shape.media_kind);
            meta.semantic_role.clone_from(&shape.media_kind);
        }
        let image_geometry = match resolve_image_geometry(&shape) {
            Ok(geometry) => geometry,
            Err(reason) => {
                page.warn(format!("picture {} crop was ignored: {reason}", shape.name));
                ImageGeometry {
                    x: shape.x,
                    y: shape.y,
                    width: shape.width,
                    height: shape.height,
                    cropped: false,
                }
            }
        };
        let crop_clip_id = image_geometry.cropped.then(|| {
            let clip_id = format!(
                "pptx-picture-crop-{}-{}",
                id_namespace,
                page.clips.len() + 1
            );
            page.clips.push(ClipPath {
                id: clip_id.clone(),
                d: rectangle_path(shape.x, shape.y, shape.width, shape.height),
                transform,
                fill_rule: "nonzero".into(),
                parent_id: None,
                additional_paths: Vec::new(),
            });
            clip_id
        });
        let mut poster_rendered = false;
        if let Some(part) = (!shape.picture_relationship_id.is_empty())
            .then(|| relationships.target(&shape.picture_relationship_id, slide_part))
            .flatten()
        {
            if let Some(bytes) = package.read_optional(&part)? {
                if let Some(label) = unsupported_metafile_label(&part) {
                    if bytes.len() <= MAX_METAFILE_BYTES {
                        match emf_core::converter::convert_to_svg(bytes.as_slice()) {
                            Ok(svg) => {
                                let mut metafile_meta = meta.clone();
                                metafile_meta.kind = "converted-metafile-image".into();
                                push_pptx_image_node(
                                    page,
                                    &shape.id,
                                    format!(
                                        "data:image/svg+xml;base64,{}",
                                        base64::engine::general_purpose::STANDARD.encode(svg)
                                    ),
                                    image_geometry,
                                    transform,
                                    crop_clip_id,
                                    &metafile_meta,
                                );
                            }
                            Err(error) => {
                                page.warn(format!(
                                    "picture {part} uses {label} but conversion failed ({error}); a static placeholder was rendered"
                                ));
                                append_unsupported_image_placeholder(
                                    page, &shape, transform, &meta, label,
                                );
                            }
                        }
                    } else {
                        page.warn(format!(
                            "picture {part} uses {label} and exceeds the {MAX_METAFILE_BYTES}-byte conversion limit; a static placeholder was rendered"
                        ));
                        append_unsupported_image_placeholder(page, &shape, transform, &meta, label);
                    }
                } else {
                    let mime = mime_type(&part);
                    let href = format!(
                        "data:{mime};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    );
                    push_pptx_image_node(
                        page,
                        &shape.id,
                        href,
                        image_geometry,
                        transform,
                        crop_clip_id,
                        &meta,
                    );
                }
                poster_rendered = true;
            } else {
                page.warn(format!("picture part {part} is missing"));
            }
        } else if let Some(target) = relationships.external_target(&shape.picture_relationship_id) {
            page.warn(format!("external picture {target} was not fetched"));
        } else if shape.media_relationship_id.is_empty() {
            page.warn(format!(
                "picture relationship {} is missing",
                shape.picture_relationship_id
            ));
        }
        if !shape.media_relationship_id.is_empty() {
            let media_kind = if shape.media_kind.is_empty() {
                "media"
            } else {
                &shape.media_kind
            };
            if let Some(part) = relationships.target(&shape.media_relationship_id, slide_part) {
                page.warn(format!(
                    "embedded {media_kind} {part} was rendered as a static poster; playback is unavailable in SVG"
                ));
            } else if let Some(target) = relationships.external_target(&shape.media_relationship_id)
            {
                page.warn(format!(
                    "external {media_kind} {target} was rendered as a static poster; playback is unavailable in SVG"
                ));
            } else {
                page.warn(format!(
                    "{media_kind} relationship {} is missing; a static placeholder was rendered",
                    shape.media_relationship_id
                ));
            }
            if !poster_rendered {
                append_static_media_placeholder(page, &shape, transform, &meta);
            }
        }
        return Ok(());
    }
    if !shape.picture_relationship_id.is_empty() {
        if let Some(part) = relationships.target(&shape.picture_relationship_id, slide_part) {
            if let Some(bytes) = package.read_optional(&part)? {
                let (clip_path, geometry_supported) = if shape.custom_geometry {
                    custom_geometry_path(&shape).map_or_else(
                        || {
                            (
                                rectangle_path(shape.x, shape.y, shape.width, shape.height),
                                false,
                            )
                        },
                        |path| (path, true),
                    )
                } else {
                    preset_path(&shape)
                };
                if !geometry_supported {
                    page.warn(format!(
                        "image-filled shape {} uses an approximated clip geometry",
                        shape.name
                    ));
                }
                let clip_id = format!(
                    "pptx-image-shape-clip-{}-{}",
                    id_namespace,
                    page.clips.len() + 1
                );
                page.clips.push(ClipPath {
                    id: clip_id.clone(),
                    d: clip_path,
                    transform,
                    fill_rule: "nonzero".into(),
                    parent_id: None,
                    additional_paths: Vec::new(),
                });
                let mut image_meta = meta.clone();
                image_meta.kind = "shape-image".into();
                let image_geometry = match resolve_image_geometry(&shape) {
                    Ok(geometry) => geometry,
                    Err(reason) => {
                        page.warn(format!(
                            "shape image {} crop was ignored: {reason}",
                            shape.name
                        ));
                        ImageGeometry {
                            x: shape.x,
                            y: shape.y,
                            width: shape.width,
                            height: shape.height,
                            cropped: false,
                        }
                    }
                };
                let image_id = format!("{}-image", shape.id);
                let image_has_effect = image_meta.outer_shadow.is_some()
                    || image_meta.glow.is_some()
                    || !image_meta.image_effects.is_empty();
                push_pptx_image_node(
                    page,
                    &image_id,
                    format!(
                        "data:{};base64,{}",
                        mime_type(&part),
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    ),
                    image_geometry,
                    transform,
                    Some(clip_id),
                    &image_meta,
                );
                if image_has_effect {
                    meta.outer_shadow = None;
                    meta.glow = None;
                    meta.image_effects.clear();
                }
                shape.fill = Paint::None;
            } else {
                page.warn(format!("shape image part {part} is missing"));
            }
        } else {
            page.warn(format!(
                "shape image relationship {} is missing",
                shape.picture_relationship_id
            ));
        }
    }
    if matches!(shape.fill, Paint::None)
        && matches!(shape.stroke.paint, Paint::None)
        && shape.paragraphs.is_empty()
    {
        return Ok(());
    }
    if !matches!(shape.fill, Paint::None) || !matches!(shape.stroke.paint, Paint::None) {
        let (path_data, geometry_supported) = if shape.custom_geometry {
            custom_geometry_path(&shape).map_or_else(
                || {
                    (
                        rectangle_path(shape.x, shape.y, shape.width, shape.height),
                        false,
                    )
                },
                |path| (path, true),
            )
        } else {
            preset_path(&shape)
        };
        if !geometry_supported && shape.custom_geometry {
            page.warn(format!(
                "custom geometry {} is approximated by its bounding box",
                shape.name
            ));
        } else if !geometry_supported && !shape.preset.is_empty() {
            page.warn(format!(
                "preset geometry {} ({}) is approximated by its bounding box",
                shape.name, shape.preset
            ));
        }
        page.nodes.push(Node::Path {
            id: shape.id.clone(),
            d: path_data,
            fill_rule: "nonzero".into(),
            fill: shape.fill.clone(),
            stroke: shape.stroke.clone(),
            transform,
            clip_id: None,
            meta: meta.clone(),
        });
    }
    let first_level = shape
        .paragraphs
        .first()
        .map_or(0, |paragraph| paragraph.level);
    let first_style = text_styles.style(&shape.placeholder_key, first_level);
    let estimated_first_font = if first_style.font_size > 0.0 {
        first_style.font_size
    } else {
        12.0
    };
    let text_x = shape.text_x.unwrap_or(shape.x);
    let text_y = shape.text_y.unwrap_or(shape.y);
    let text_width = shape.text_width.unwrap_or(shape.width).max(1.0);
    let text_height = shape.text_height.unwrap_or(shape.height).max(1.0);
    let text_transform = compose(
        transform,
        rotation_matrix(
            shape.text_rotation,
            text_x + text_width / 2.0,
            text_y + text_height / 2.0,
        ),
    );
    let left_inset = shape.text_margin_left.unwrap_or(5.0);
    let right_inset = shape.text_margin_right.unwrap_or(5.0);
    let top_inset = shape.text_margin_top.unwrap_or(6.0);
    let bottom_inset = shape.text_margin_bottom.unwrap_or(3.6);
    let mut y = match shape.vertical_anchor.as_str() {
        "ctr" => {
            text_y + top_inset + (text_height - top_inset - bottom_inset) / 2.0
                - estimated_first_font * 0.45
        }
        "b" => text_y + text_height - bottom_inset - estimated_first_font * 1.2,
        _ => text_y + top_inset,
    };
    let mut first_text_line = true;
    let mut text_node_index = 0usize;
    let mut text_meta = meta.clone();
    text_meta.outer_shadow.clone_from(&shape.text_outer_shadow);
    text_meta.glow.clone_from(&shape.text_glow);
    for (paragraph_index, mut paragraph) in shape.paragraphs.drain(..).enumerate() {
        if paragraph.runs.is_empty() {
            y += 14.0;
            continue;
        }
        let style = text_styles.style(&shape.placeholder_key, paragraph.level);
        for run in &mut paragraph.runs {
            if run.font_size <= 0.0 {
                run.font_size = if style.font_size > 0.0 {
                    style.font_size
                } else {
                    12.0
                };
            }
            if run.font_family.is_empty() {
                run.font_family = if style.font_family.is_empty() {
                    theme.minor_font.clone()
                } else {
                    style.font_family.clone()
                };
            }
            if matches!(run.fill, Paint::None) {
                run.fill = style
                    .fill
                    .clone()
                    .unwrap_or_else(|| Paint::solid("#000000"));
            }
            run.bold |= style.bold;
            run.italic |= style.italic;
        }
        let anchor = paragraph
            .alignment
            .or(style.alignment)
            .unwrap_or(TextAnchor::Start);
        let mut available_width =
            (text_width - left_inset - right_inset - style.margin_left).max(6.0);
        if shape.text_width.is_some() {
            let natural_width = paragraph
                .runs
                .iter()
                .flat_map(|run| {
                    run.text
                        .chars()
                        .map(move |character| run.font_size * text_advance_factor(character))
                })
                .sum::<f64>();
            available_width = available_width.max(natural_width);
        }
        let lines = wrap_pptx_runs(&paragraph.runs, available_width);
        for (line_index, runs) in lines.into_iter().enumerate() {
            let max_font = runs
                .iter()
                .map(|run| run.font_size)
                .fold(0.0, f64::max)
                .max(12.0);
            y += if first_text_line {
                first_text_line = false;
                max_font * 0.8
            } else {
                max_font
            };
            let (x, anchor) = match anchor {
                TextAnchor::Start => (text_x + left_inset + style.margin_left, TextAnchor::Start),
                TextAnchor::Middle => (text_x + text_width / 2.0, TextAnchor::Middle),
                TextAnchor::End => (
                    text_x + text_width - right_inset - style.margin_left,
                    TextAnchor::End,
                ),
            };
            if line_index == 0 && !style.bullet.is_empty() && anchor == TextAnchor::Start {
                text_node_index += 1;
                page.nodes.push(Node::Text {
                    id: format!(
                        "{}-bullet-{}-{}",
                        shape.id,
                        paragraph_index + 1,
                        text_node_index
                    ),
                    x: text_x + left_inset,
                    y,
                    runs: vec![TextRun {
                        text: style.bullet.clone(),
                        font_family: style.font_family.clone(),
                        font_size: max_font,
                        bold: style.bold,
                        italic: style.italic,
                        fill: style
                            .fill
                            .clone()
                            .unwrap_or_else(|| Paint::solid("#000000")),
                        baseline_shift: 0.0,
                        glyph_x_offsets: Vec::new(),
                        target_advance: None,
                    }],
                    anchor: TextAnchor::Start,
                    transform: text_transform,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "bullet".into(),
                        ..text_meta.clone()
                    },
                });
            }
            text_node_index += 1;
            page.nodes.push(Node::Text {
                id: format!(
                    "{}-text-{}-{}",
                    shape.id,
                    paragraph_index + 1,
                    text_node_index
                ),
                x,
                y,
                runs,
                anchor,
                transform: text_transform,
                opacity: 1.0,
                stroke: Stroke::default(),
                clip_id: None,
                meta: SourceMeta {
                    kind: "text".into(),
                    ..text_meta.clone()
                },
            });
        }
    }
    Ok(())
}

fn append_static_media_placeholder(
    page: &mut Page,
    shape: &Shape,
    transform: Matrix,
    meta: &SourceMeta,
) {
    page.nodes.push(Node::Path {
        id: format!("{}-placeholder", shape.id),
        d: rectangle_path(shape.x, shape.y, shape.width, shape.height),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#E6E6E6"),
        stroke: Stroke {
            paint: Paint::solid("#7A7A7A"),
            width: 1.0,
            miter_limit: 10.0,
            ..Stroke::default()
        },
        transform,
        clip_id: None,
        meta: SourceMeta {
            kind: format!("{}-placeholder", shape.media_kind),
            ..meta.clone()
        },
    });
    page.nodes.push(Node::Text {
        id: format!("{}-placeholder-label", shape.id),
        x: shape.x + shape.width / 2.0,
        y: shape.y + shape.height / 2.0 + 4.0,
        runs: vec![TextRun {
            text: if shape.media_kind.eq_ignore_ascii_case("audio") {
                "Audio".into()
            } else {
                "Video".into()
            },
            font_family: "Arial, sans-serif".into(),
            font_size: 12.0,
            fill: Paint::solid("#333333"),
            ..TextRun::default()
        }],
        anchor: TextAnchor::Middle,
        transform,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            kind: "media-placeholder-label".into(),
            ..meta.clone()
        },
    });
}

fn append_unsupported_image_placeholder(
    page: &mut Page,
    shape: &Shape,
    transform: Matrix,
    meta: &SourceMeta,
    label: &str,
) {
    page.nodes.push(Node::Path {
        id: format!("{}-unsupported-image-placeholder", shape.id),
        d: rectangle_path(shape.x, shape.y, shape.width, shape.height),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#F3F4F6"),
        stroke: Stroke {
            paint: Paint::solid("#9CA3AF"),
            width: 1.0,
            miter_limit: 10.0,
            dash_array: vec![4.0, 3.0],
            ..Stroke::default()
        },
        transform,
        clip_id: None,
        meta: SourceMeta {
            kind: "unsupported-vector-image-placeholder".into(),
            ..meta.clone()
        },
    });
    page.nodes.push(Node::Text {
        id: format!("{}-unsupported-image-label", shape.id),
        x: shape.x + shape.width / 2.0,
        y: shape.y + shape.height / 2.0 + 4.0,
        runs: vec![TextRun {
            text: format!("{label} preview unavailable"),
            font_family: "Arial, sans-serif".into(),
            font_size: shape.height.clamp(6.0, 12.0),
            fill: Paint::solid("#4B5563"),
            ..TextRun::default()
        }],
        anchor: TextAnchor::Middle,
        transform,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            kind: "unsupported-vector-image-placeholder-label".into(),
            ..meta.clone()
        },
    });
}

fn wrap_pptx_runs(runs: &[TextRun], width: f64) -> Vec<Vec<TextRun>> {
    if runs.is_empty() {
        return vec![Vec::new()];
    }
    let mut lines = vec![Vec::<TextRun>::new()];
    let mut line_width = 0.0;
    let mut pending_space = Vec::<PptxStyledCharacter>::new();
    let mut pending_space_width = 0.0;
    let mut word = Vec::<PptxStyledCharacter>::new();
    let mut word_width = 0.0;
    for run in runs {
        for character in run.text.chars() {
            if character == '\n' {
                place_pptx_word(
                    &mut lines,
                    &mut line_width,
                    &mut pending_space,
                    &mut pending_space_width,
                    &mut word,
                    &mut word_width,
                    width,
                );
                pending_space.clear();
                pending_space_width = 0.0;
                lines.push(Vec::new());
                line_width = 0.0;
                continue;
            }
            let styled = PptxStyledCharacter {
                character,
                run: run.clone(),
                width: run.font_size * text_advance_factor(character),
            };
            if character.is_whitespace() {
                place_pptx_word(
                    &mut lines,
                    &mut line_width,
                    &mut pending_space,
                    &mut pending_space_width,
                    &mut word,
                    &mut word_width,
                    width,
                );
                pending_space_width += styled.width;
                pending_space.push(styled);
            } else {
                word_width += styled.width;
                word.push(styled);
            }
        }
    }
    place_pptx_word(
        &mut lines,
        &mut line_width,
        &mut pending_space,
        &mut pending_space_width,
        &mut word,
        &mut word_width,
        width,
    );
    lines
}

struct PptxStyledCharacter {
    character: char,
    run: TextRun,
    width: f64,
}

#[allow(clippy::too_many_arguments)]
fn place_pptx_word(
    lines: &mut Vec<Vec<TextRun>>,
    line_width: &mut f64,
    pending_space: &mut Vec<PptxStyledCharacter>,
    pending_space_width: &mut f64,
    word: &mut Vec<PptxStyledCharacter>,
    word_width: &mut f64,
    width: f64,
) {
    if word.is_empty() {
        return;
    }
    if *word_width <= width {
        if *line_width > 0.0 && *line_width + *pending_space_width + *word_width > width {
            lines.push(Vec::new());
            *line_width = 0.0;
        }
        if *line_width > 0.0 {
            for character in pending_space.drain(..) {
                append_pptx_character(lines, character);
            }
            *line_width += *pending_space_width;
        }
        for character in word.drain(..) {
            append_pptx_character(lines, character);
        }
        *line_width += *word_width;
    } else {
        if *line_width > 0.0 {
            lines.push(Vec::new());
            *line_width = 0.0;
        }
        pending_space.clear();
        for character in word.drain(..) {
            if *line_width > 0.0 && *line_width + character.width > width {
                lines.push(Vec::new());
                *line_width = 0.0;
            }
            *line_width += character.width;
            append_pptx_character(lines, character);
        }
    }
    pending_space.clear();
    *pending_space_width = 0.0;
    *word_width = 0.0;
}

fn append_pptx_character(lines: &mut [Vec<TextRun>], character: PptxStyledCharacter) {
    let line = lines.last_mut().expect("line always exists");
    if let Some(last) = line.last_mut()
        && same_pptx_run_style(last, &character.run)
    {
        last.text.push(character.character);
    } else {
        let mut run = character.run;
        run.text = character.character.to_string();
        line.push(run);
    }
}

fn same_pptx_run_style(left: &TextRun, right: &TextRun) -> bool {
    left.font_family == right.font_family
        && (left.font_size - right.font_size).abs() < 1e-9
        && left.bold == right.bold
        && left.italic == right.italic
        && left.fill == right.fill
        && (left.baseline_shift - right.baseline_shift).abs() < 1e-9
}

fn custom_geometry_path(shape: &Shape) -> Option<String> {
    let mut output = String::new();
    for geometry in &shape.custom_paths {
        if geometry.commands.is_empty() {
            continue;
        }
        let coordinate_width = geometry.width.max(1.0);
        let coordinate_height = geometry.height.max(1.0);
        let resolve_point = |point: &(String, String)| {
            let mut visiting = HashSet::new();
            let x = custom_geometry_value(
                &point.0,
                coordinate_width,
                coordinate_height,
                &shape.custom_guides,
                &mut visiting,
                0,
            )?;
            visiting.clear();
            let y = custom_geometry_value(
                &point.1,
                coordinate_width,
                coordinate_height,
                &shape.custom_guides,
                &mut visiting,
                0,
            )?;
            Some((
                shape.x + x / coordinate_width * shape.width,
                shape.y + y / coordinate_height * shape.height,
            ))
        };
        let mut current = None::<(f64, f64)>;
        for command in &geometry.commands {
            match command {
                CustomGeometryCommand::Move(points) => {
                    let point = resolve_point(points.first()?)?;
                    output.push_str(&format!(" M {} {}", fmt(point.0), fmt(point.1)));
                    current = Some(point);
                }
                CustomGeometryCommand::Line(points) => {
                    for point in points {
                        let point = resolve_point(point)?;
                        output.push_str(&format!(" L {} {}", fmt(point.0), fmt(point.1)));
                        current = Some(point);
                    }
                }
                CustomGeometryCommand::Cubic(points) => {
                    for points in points.chunks_exact(3) {
                        let first = resolve_point(&points[0])?;
                        let second = resolve_point(&points[1])?;
                        let third = resolve_point(&points[2])?;
                        output.push_str(&format!(
                            " C {} {} {} {} {} {}",
                            fmt(first.0),
                            fmt(first.1),
                            fmt(second.0),
                            fmt(second.1),
                            fmt(third.0),
                            fmt(third.1)
                        ));
                        current = Some(third);
                    }
                }
                CustomGeometryCommand::Quadratic(points) => {
                    for points in points.chunks_exact(2) {
                        let control = resolve_point(&points[0])?;
                        let endpoint = resolve_point(&points[1])?;
                        output.push_str(&format!(
                            " Q {} {} {} {}",
                            fmt(control.0),
                            fmt(control.1),
                            fmt(endpoint.0),
                            fmt(endpoint.1)
                        ));
                        current = Some(endpoint);
                    }
                }
                CustomGeometryCommand::Arc {
                    width_radius,
                    height_radius,
                    start_angle,
                    sweep_angle,
                } => {
                    let current_point = current?;
                    let mut visiting = HashSet::new();
                    let width_radius = custom_geometry_value(
                        width_radius,
                        coordinate_width,
                        coordinate_height,
                        &shape.custom_guides,
                        &mut visiting,
                        0,
                    )?
                    .abs()
                        / coordinate_width
                        * shape.width;
                    visiting.clear();
                    let height_radius = custom_geometry_value(
                        height_radius,
                        coordinate_width,
                        coordinate_height,
                        &shape.custom_guides,
                        &mut visiting,
                        0,
                    )?
                    .abs()
                        / coordinate_height
                        * shape.height;
                    visiting.clear();
                    let start_degrees = custom_geometry_value(
                        start_angle,
                        coordinate_width,
                        coordinate_height,
                        &shape.custom_guides,
                        &mut visiting,
                        0,
                    )? / 60_000.0;
                    visiting.clear();
                    let sweep_degrees = custom_geometry_value(
                        sweep_angle,
                        coordinate_width,
                        coordinate_height,
                        &shape.custom_guides,
                        &mut visiting,
                        0,
                    )? / 60_000.0;
                    if width_radius <= 1e-12 || height_radius <= 1e-12 {
                        continue;
                    }
                    let start = start_degrees * PI / 180.0;
                    let center = (
                        current_point.0 - width_radius * start.cos(),
                        current_point.1 - height_radius * start.sin(),
                    );
                    let sweep_flag = usize::from(sweep_degrees >= 0.0);
                    if sweep_degrees.abs() >= 359.999 {
                        let middle = (start_degrees + sweep_degrees / 2.0) * PI / 180.0;
                        let middle_point = (
                            center.0 + width_radius * middle.cos(),
                            center.1 + height_radius * middle.sin(),
                        );
                        output.push_str(&format!(
                            " A {} {} 0 0 {} {} {}",
                            fmt(width_radius),
                            fmt(height_radius),
                            sweep_flag,
                            fmt(middle_point.0),
                            fmt(middle_point.1)
                        ));
                    }
                    let end = (start_degrees + sweep_degrees) * PI / 180.0;
                    let endpoint = (
                        center.0 + width_radius * end.cos(),
                        center.1 + height_radius * end.sin(),
                    );
                    output.push_str(&format!(
                        " A {} {} 0 {} {} {} {}",
                        fmt(width_radius),
                        fmt(height_radius),
                        usize::from(sweep_degrees.abs() > 180.0 && sweep_degrees.abs() < 359.999),
                        sweep_flag,
                        fmt(endpoint.0),
                        fmt(endpoint.1)
                    ));
                    current = Some(endpoint);
                }
                CustomGeometryCommand::Close => output.push_str(" Z"),
            }
        }
    }
    let output = output.trim().to_owned();
    (!output.is_empty()).then_some(output)
}

fn custom_geometry_value(
    token: &str,
    width: f64,
    height: f64,
    guides: &HashMap<String, String>,
    visiting: &mut HashSet<String>,
    depth: usize,
) -> Option<f64> {
    if depth > 32 {
        return None;
    }
    if let Ok(value) = token.parse::<f64>() {
        return Some(value);
    }
    let shortest = width.min(height);
    let longest = width.max(height);
    let builtin = match token {
        "l" | "t" => Some(0.0),
        "w" | "r" => Some(width),
        "h" | "b" => Some(height),
        "hc" => Some(width / 2.0),
        "vc" => Some(height / 2.0),
        "ss" => Some(shortest),
        "ls" => Some(longest),
        "cd2" => Some(10_800_000.0),
        "cd4" => Some(5_400_000.0),
        "cd8" => Some(2_700_000.0),
        "3cd4" => Some(16_200_000.0),
        "3cd8" => Some(8_100_000.0),
        "5cd8" => Some(13_500_000.0),
        "7cd8" => Some(18_900_000.0),
        _ => None,
    };
    if builtin.is_some() {
        return builtin;
    }
    for (prefix, value) in [
        ("wd", width),
        ("hd", height),
        ("ssd", shortest),
        ("lsd", longest),
    ] {
        if let Some(divisor) = token
            .strip_prefix(prefix)
            .and_then(|value| value.parse::<f64>().ok())
            && divisor.abs() > 1e-12
        {
            return Some(value / divisor);
        }
    }
    let formula = guides.get(token)?;
    if !visiting.insert(token.to_owned()) {
        return None;
    }
    let parts = formula.split_whitespace().collect::<Vec<_>>();
    let value =
        evaluate_custom_geometry_formula(&parts, width, height, guides, visiting, depth + 1);
    visiting.remove(token);
    value
}

fn evaluate_custom_geometry_formula(
    parts: &[&str],
    width: f64,
    height: f64,
    guides: &HashMap<String, String>,
    visiting: &mut HashSet<String>,
    depth: usize,
) -> Option<f64> {
    let value = |index: usize, visiting: &mut HashSet<String>| {
        custom_geometry_value(parts.get(index)?, width, height, guides, visiting, depth)
    };
    match *parts.first()? {
        "val" => value(1, visiting),
        "*/" => Some(value(1, visiting)? * value(2, visiting)? / value(3, visiting)?),
        "+-" => Some(value(1, visiting)? + value(2, visiting)? - value(3, visiting)?),
        "+/" => Some((value(1, visiting)? + value(2, visiting)?) / value(3, visiting)?),
        "?:" => Some(if value(1, visiting)? > 0.0 {
            value(2, visiting)?
        } else {
            value(3, visiting)?
        }),
        "abs" => Some(value(1, visiting)?.abs()),
        "sqrt" => Some(value(1, visiting)?.max(0.0).sqrt()),
        "max" => Some(value(1, visiting)?.max(value(2, visiting)?)),
        "min" => Some(value(1, visiting)?.min(value(2, visiting)?)),
        "pin" => {
            let minimum = value(1, visiting)?;
            let maximum = value(3, visiting)?;
            Some(value(2, visiting)?.clamp(minimum.min(maximum), minimum.max(maximum)))
        }
        "mod" => Some(
            (value(1, visiting)?.powi(2)
                + value(2, visiting)?.powi(2)
                + value(3, visiting)?.powi(2))
            .sqrt(),
        ),
        "sin" => Some(value(1, visiting)? * (value(2, visiting)? / 60_000.0 * PI / 180.0).sin()),
        "cos" => Some(value(1, visiting)? * (value(2, visiting)? / 60_000.0 * PI / 180.0).cos()),
        "tan" => Some(value(1, visiting)? * (value(2, visiting)? / 60_000.0 * PI / 180.0).tan()),
        "at2" => Some(value(2, visiting)?.atan2(value(1, visiting)?) * 180.0 / PI * 60_000.0),
        "cat2" => {
            let angle = value(3, visiting)?.atan2(value(2, visiting)?);
            Some(value(1, visiting)? * angle.cos())
        }
        "sat2" => {
            let angle = value(3, visiting)?.atan2(value(2, visiting)?);
            Some(value(1, visiting)? * angle.sin())
        }
        _ => None,
    }
}

fn preset_path(shape: &Shape) -> (String, bool) {
    let x = shape.x;
    let y = shape.y;
    let width = shape.width;
    let height = shape.height;
    let result = match shape.preset.as_str() {
        "" | "rect" | "flowChartProcess" => rectangle_path(x, y, width, height),
        "ellipse" | "flowChartConnector" => {
            let cx = x + width / 2.0;
            let cy = y + height / 2.0;
            format!(
                "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} Z",
                fmt(cx - width / 2.0),
                fmt(cy),
                fmt(width / 2.0),
                fmt(height / 2.0),
                fmt(cx + width / 2.0),
                fmt(cy),
                fmt(width / 2.0),
                fmt(height / 2.0),
                fmt(cx - width / 2.0),
                fmt(cy)
            )
        }
        "roundRect" => {
            let radius = width.min(height) * 0.1;
            format!(
                "M {} {} H {} A {} {} 0 0 1 {} {} V {} A {} {} 0 0 1 {} {} H {} A {} {} 0 0 1 {} {} V {} A {} {} 0 0 1 {} {} Z",
                fmt(x + radius),
                fmt(y),
                fmt(x + width - radius),
                fmt(radius),
                fmt(radius),
                fmt(x + width),
                fmt(y + radius),
                fmt(y + height - radius),
                fmt(radius),
                fmt(radius),
                fmt(x + width - radius),
                fmt(y + height),
                fmt(x + radius),
                fmt(radius),
                fmt(radius),
                fmt(x),
                fmt(y + height - radius),
                fmt(y + radius),
                fmt(radius),
                fmt(radius),
                fmt(x + radius),
                fmt(y)
            )
        }
        "round1Rect" => selective_round_rect_path(x, y, width, height, [true, false, false, false]),
        "round2SameRect" => {
            selective_round_rect_path(x, y, width, height, [true, true, false, false])
        }
        "round2DiagRect" => {
            selective_round_rect_path(x, y, width, height, [true, false, true, false])
        }
        "snip2SameRect" => snip_two_same_rect_path(shape),
        "corner" => corner_path(shape),
        "line" | "straightConnector1" => format!(
            "M {} {} L {} {}",
            fmt(x),
            fmt(y),
            fmt(x + width),
            fmt(y + height)
        ),
        "bentConnector2" => bent_connector_path(shape, 50_000.0),
        "bentConnector3" => bent_connector_path(
            shape,
            shape
                .preset_adjustments
                .get("adj1")
                .copied()
                .unwrap_or(50_000.0),
        ),
        "arc" => arc_preset_path(shape),
        "rightBrace" => brace_path(shape, false),
        "leftBrace" => brace_path(shape, true),
        "triangle" => polygon_path(&[
            (x + width / 2.0, y),
            (x + width, y + height),
            (x, y + height),
        ]),
        "rtTriangle" => polygon_path(&[(x, y), (x + width, y + height), (x, y + height)]),
        "diamond" => polygon_path(&[
            (x + width / 2.0, y),
            (x + width, y + height / 2.0),
            (x + width / 2.0, y + height),
            (x, y + height / 2.0),
        ]),
        "flowChartDecision" => polygon_path(&[
            (x + width / 2.0, y),
            (x + width, y + height / 2.0),
            (x + width / 2.0, y + height),
            (x, y + height / 2.0),
        ]),
        "flowChartManualInput" => {
            let skew = (width * 0.22).min(height * 0.48);
            polygon_path(&[
                (x + skew, y),
                (x + width, y),
                (x + width, y + height),
                (x, y + height),
            ])
        }
        "flowChartExtract" => polygon_path(&[
            (x + width / 2.0, y),
            (x + width, y + height),
            (x, y + height),
        ]),
        "flowChartCollate" => format!(
            "M {} {} H {} L {} {} H {} Z",
            fmt(x),
            fmt(y),
            fmt(x + width),
            fmt(x),
            fmt(y + height),
            fmt(x + width)
        ),
        "flowChartDelay" => flow_chart_delay_path(shape),
        "can" => can_path(shape),
        "cube" => cube_path(shape),
        "moon" => moon_path(shape),
        "donut" => donut_path(shape),
        "parallelogram" | "flowChartInputOutput" => polygon_path(&[
            (x + width * 0.2, y),
            (x + width, y),
            (x + width * 0.8, y + height),
            (x, y + height),
        ]),
        "trapezoid" => polygon_path(&[
            (x + width * 0.2, y),
            (x + width * 0.8, y),
            (x + width, y + height),
            (x, y + height),
        ]),
        "pentagon" => regular_polygon_path(x, y, width, height, 5, -90.0),
        "hexagon" => regular_polygon_path(x, y, width, height, 6, 0.0),
        "octagon" => regular_polygon_path(x, y, width, height, 8, 22.5),
        "chevron" => polygon_path(&[
            (x, y),
            (x + width * 0.72, y),
            (x + width, y + height / 2.0),
            (x + width * 0.72, y + height),
            (x, y + height),
            (x + width * 0.28, y + height / 2.0),
        ]),
        "rightArrow" => arrow_path(x, y, width, height, 0),
        "downArrow" => arrow_path(x, y, width, height, 1),
        "leftArrow" => arrow_path(x, y, width, height, 2),
        "upArrow" => arrow_path(x, y, width, height, 3),
        "leftRightArrow" => left_right_arrow_path(shape),
        "bentArrow" => bent_arrow_path(shape),
        "bentUpArrow" => bent_up_arrow_path(shape),
        "uturnArrow" => uturn_arrow_path(shape),
        "curvedLeftArrow" => curved_left_arrow_path(shape),
        "curvedUpArrow" => curved_up_arrow_path(shape),
        "curvedDownArrow" => curved_down_arrow_path(shape),
        "circularArrow" => circular_arrow_path(shape),
        "upDownArrow" => up_down_arrow_path(shape),
        "plus" | "mathPlus" => plus_path(x, y, width, height),
        "leftBracket" => left_bracket_path(shape),
        "rightBracket" => right_bracket_path(shape),
        "bracketPair" => bracket_pair_path(shape),
        "homePlate" => home_plate_path(shape),
        "wave" => wave_path(shape),
        "actionButtonForwardNext" => action_button_forward_path(shape),
        "irregularSeal1" => irregular_seal_path(shape, false),
        "irregularSeal2" => irregular_seal_path(shape, true),
        "star5" => star_path(x, y, width, height, 5),
        "wedgeRectCallout" => wedge_rect_callout_path(shape),
        "wedgeRoundRectCallout" => wedge_round_rect_callout_path(shape),
        "wedgeEllipseCallout" => wedge_ellipse_callout_path(shape),
        "borderCallout1" | "borderCallout2" => border_callout_path(shape),
        _ => return (rectangle_path(x, y, width, height), false),
    };
    (result, true)
}

fn bent_connector_path(shape: &Shape, adjustment: f64) -> String {
    let bend_x = shape.x + shape.width * adjustment.clamp(0.0, 100_000.0) / 100_000.0;
    format!(
        "M {} {} L {} {} L {} {} L {} {}",
        fmt(shape.x),
        fmt(shape.y),
        fmt(bend_x),
        fmt(shape.y),
        fmt(bend_x),
        fmt(shape.y + shape.height),
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height)
    )
}

fn arc_preset_path(shape: &Shape) -> String {
    let start_degrees = shape
        .preset_adjustments
        .get("adj1")
        .copied()
        .unwrap_or(16_200_000.0)
        / 60_000.0;
    let end_degrees = shape.preset_adjustments.get("adj2").copied().unwrap_or(0.0) / 60_000.0;
    let sweep = (end_degrees - start_degrees).rem_euclid(360.0);
    let start = start_degrees.to_radians();
    let end = end_degrees.to_radians();
    let radius_x = shape.width / 2.0;
    let radius_y = shape.height / 2.0;
    let center_x = shape.x + radius_x;
    let center_y = shape.y + radius_y;
    format!(
        "M {} {} A {} {} 0 {} 1 {} {}",
        fmt(center_x + radius_x * start.cos()),
        fmt(center_y + radius_y * start.sin()),
        fmt(radius_x),
        fmt(radius_y),
        usize::from(sweep > 180.0),
        fmt(center_x + radius_x * end.cos()),
        fmt(center_y + radius_y * end.sin())
    )
}

fn brace_path(shape: &Shape, left: bool) -> String {
    let adjustment = shape
        .preset_adjustments
        .get("adj1")
        .copied()
        .unwrap_or(8_333.0)
        .clamp(0.0, 100_000.0);
    let center_ratio = shape
        .preset_adjustments
        .get("adj2")
        .copied()
        .unwrap_or(50_000.0)
        .clamp(10_000.0, 90_000.0)
        / 100_000.0;
    let shoulder_ratio = (0.56 - 0.30 * ((adjustment - 8_333.0) / 91_667.0)).clamp(0.20, 0.78);
    let endpoint_x = if left { shape.x + shape.width } else { shape.x };
    let outer_x = if left {
        shape.x + shape.width * 0.04
    } else {
        shape.x + shape.width * 0.96
    };
    let shoulder_x = if left {
        shape.x + shape.width * (1.0 - shoulder_ratio)
    } else {
        shape.x + shape.width * shoulder_ratio
    };
    let center_y = shape.y + shape.height * center_ratio;
    let upper_y = shape.y + (center_y - shape.y) * 0.68;
    let lower_y = center_y + (shape.y + shape.height - center_y) * 0.32;
    let upper_control = shape.y + (upper_y - shape.y) * 0.53;
    let lower_control = lower_y + (shape.y + shape.height - lower_y) * 0.47;
    let cusp_gap = shape.height * 0.035;
    format!(
        "M {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {}",
        fmt(endpoint_x),
        fmt(shape.y),
        fmt(outer_x),
        fmt(shape.y),
        fmt(outer_x),
        fmt(upper_control),
        fmt(shoulder_x),
        fmt(upper_y),
        fmt(endpoint_x),
        fmt(upper_y + cusp_gap),
        fmt(endpoint_x),
        fmt(center_y - cusp_gap),
        fmt(shoulder_x),
        fmt(center_y),
        fmt(endpoint_x),
        fmt(center_y + cusp_gap),
        fmt(endpoint_x),
        fmt(lower_y - cusp_gap),
        fmt(shoulder_x),
        fmt(lower_y),
        fmt(outer_x),
        fmt(lower_control),
        fmt(outer_x),
        fmt(shape.y + shape.height),
        fmt(endpoint_x),
        fmt(shape.y + shape.height)
    )
}

fn can_path(shape: &Shape) -> String {
    let adjustment = shape
        .preset_adjustments
        .get("adj")
        .copied()
        .unwrap_or(25_000.0)
        .clamp(0.0, 50_000.0);
    let radius_y = (shape.height * adjustment / 200_000.0).max(shape.height * 0.02);
    let radius_x = shape.width / 2.0;
    let top_y = shape.y + radius_y;
    let bottom_y = shape.y + shape.height - radius_y;
    format!(
        "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} L {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} Z M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {}",
        fmt(shape.x),
        fmt(top_y),
        fmt(radius_x),
        fmt(radius_y),
        fmt(shape.x + shape.width),
        fmt(top_y),
        fmt(radius_x),
        fmt(radius_y),
        fmt(shape.x),
        fmt(top_y),
        fmt(shape.x),
        fmt(bottom_y),
        fmt(radius_x),
        fmt(radius_y),
        fmt(shape.x + shape.width),
        fmt(bottom_y),
        fmt(radius_x),
        fmt(radius_y),
        fmt(shape.x),
        fmt(bottom_y),
        fmt(shape.x),
        fmt(top_y),
        fmt(radius_x),
        fmt(radius_y),
        fmt(shape.x + shape.width),
        fmt(top_y),
        fmt(radius_x),
        fmt(radius_y),
        fmt(shape.x),
        fmt(top_y)
    )
}

fn flow_chart_delay_path(shape: &Shape) -> String {
    let radius_x = shape.width * 0.48;
    format!(
        "M {} {} H {} A {} {} 0 0 1 {} {} H {} Z",
        fmt(shape.x),
        fmt(shape.y),
        fmt(shape.x + shape.width - radius_x),
        fmt(radius_x),
        fmt(shape.height / 2.0),
        fmt(shape.x + shape.width - radius_x),
        fmt(shape.y + shape.height),
        fmt(shape.x)
    )
}

fn moon_path(shape: &Shape) -> String {
    let adjustment = shape
        .preset_adjustments
        .get("adj")
        .copied()
        .unwrap_or(37_500.0)
        .clamp(0.0, 100_000.0);
    let start_x = shape.x + shape.width * 0.58;
    let inner_radius_x = shape.width * (0.50 + adjustment / 200_000.0).clamp(0.5, 0.9);
    format!(
        "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 0 1 {} {} Z",
        fmt(start_x),
        fmt(shape.y),
        fmt(shape.width / 2.0),
        fmt(shape.height / 2.0),
        fmt(start_x),
        fmt(shape.y + shape.height),
        fmt(inner_radius_x),
        fmt(shape.height * 0.53),
        fmt(start_x),
        fmt(shape.y)
    )
}

fn cube_path(shape: &Shape) -> String {
    let adjustment = shape
        .preset_adjustments
        .get("adj")
        .copied()
        .unwrap_or(33_333.0)
        .clamp(0.0, 100_000.0)
        / 100_000.0;
    let depth = shape.width.min(shape.height) * (0.12 + adjustment * 0.16);
    format!(
        "M {} {} L {} {} H {} V {} L {} {} H {} Z M {} {} H {} L {} {} M {} {} V {}",
        fmt(shape.x),
        fmt(shape.y + depth),
        fmt(shape.x + depth),
        fmt(shape.y),
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height - depth),
        fmt(shape.x + shape.width - depth),
        fmt(shape.y + shape.height),
        fmt(shape.x),
        fmt(shape.x),
        fmt(shape.y + depth),
        fmt(shape.x + shape.width - depth),
        fmt(shape.x + shape.width),
        fmt(shape.y),
        fmt(shape.x + shape.width - depth),
        fmt(shape.y + depth),
        fmt(shape.y + shape.height)
    )
}

fn donut_path(shape: &Shape) -> String {
    let center_x = shape.x + shape.width / 2.0;
    let center_y = shape.y + shape.height / 2.0;
    let outer_x = shape.width / 2.0;
    let outer_y = shape.height / 2.0;
    let inner_x = outer_x * 0.56;
    let inner_y = outer_y * 0.56;
    format!(
        "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} Z M {} {} A {} {} 0 1 1 {} {} A {} {} 0 1 1 {} {} Z",
        fmt(center_x - outer_x),
        fmt(center_y),
        fmt(outer_x),
        fmt(outer_y),
        fmt(center_x + outer_x),
        fmt(center_y),
        fmt(outer_x),
        fmt(outer_y),
        fmt(center_x - outer_x),
        fmt(center_y),
        fmt(center_x + inner_x),
        fmt(center_y),
        fmt(inner_x),
        fmt(inner_y),
        fmt(center_x - inner_x),
        fmt(center_y),
        fmt(inner_x),
        fmt(inner_y),
        fmt(center_x + inner_x),
        fmt(center_y)
    )
}

fn snip_two_same_rect_path(shape: &Shape) -> String {
    let short_side = shape.width.min(shape.height);
    let left = short_side
        * shape
            .preset_adjustments
            .get("adj1")
            .copied()
            .unwrap_or(18_000.0)
            .clamp(0.0, 50_000.0)
        / 100_000.0;
    let right = short_side
        * shape
            .preset_adjustments
            .get("adj2")
            .copied()
            .unwrap_or(18_000.0)
            .clamp(0.0, 50_000.0)
        / 100_000.0;
    polygon_path(&[
        (shape.x + left, shape.y),
        (shape.x + shape.width - right, shape.y),
        (shape.x + shape.width, shape.y + right),
        (shape.x + shape.width, shape.y + shape.height),
        (shape.x, shape.y + shape.height),
        (shape.x, shape.y + left),
    ])
}

fn corner_path(shape: &Shape) -> String {
    let arm_x = shape.width
        * shape
            .preset_adjustments
            .get("adj1")
            .copied()
            .unwrap_or(28_000.0)
            .clamp(0.0, 100_000.0)
        / 100_000.0;
    let arm_y = shape.height
        * shape
            .preset_adjustments
            .get("adj2")
            .copied()
            .unwrap_or(28_000.0)
            .clamp(0.0, 100_000.0)
        / 100_000.0;
    polygon_path(&[
        (shape.x, shape.y),
        (shape.x + arm_x, shape.y),
        (shape.x + arm_x, shape.y + shape.height - arm_y),
        (shape.x + shape.width, shape.y + shape.height - arm_y),
        (shape.x + shape.width, shape.y + shape.height),
        (shape.x, shape.y + shape.height),
    ])
}

fn left_bracket_path(shape: &Shape) -> String {
    format!(
        "M {} {} H {} V {} H {}",
        fmt(shape.x + shape.width),
        fmt(shape.y),
        fmt(shape.x),
        fmt(shape.y + shape.height),
        fmt(shape.x + shape.width)
    )
}

fn right_bracket_path(shape: &Shape) -> String {
    format!(
        "M {} {} H {} V {} H {}",
        fmt(shape.x),
        fmt(shape.y),
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height),
        fmt(shape.x)
    )
}

fn bracket_pair_path(shape: &Shape) -> String {
    let inset = shape.width
        * shape
            .preset_adjustments
            .get("adj")
            .copied()
            .unwrap_or(16_667.0)
            .clamp(0.0, 50_000.0)
        / 100_000.0;
    format!(
        "M {} {} H {} V {} H {} M {} {} H {} V {} H {}",
        fmt(shape.x + inset),
        fmt(shape.y),
        fmt(shape.x),
        fmt(shape.y + shape.height),
        fmt(shape.x + inset),
        fmt(shape.x + shape.width - inset),
        fmt(shape.y),
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height),
        fmt(shape.x + shape.width - inset)
    )
}

fn home_plate_path(shape: &Shape) -> String {
    let tip = shape.width * 0.25;
    polygon_path(&[
        (shape.x, shape.y),
        (shape.x + shape.width - tip, shape.y),
        (shape.x + shape.width, shape.y + shape.height / 2.0),
        (shape.x + shape.width - tip, shape.y + shape.height),
        (shape.x, shape.y + shape.height),
    ])
}

fn left_right_arrow_path(shape: &Shape) -> String {
    let head_width = shape.width * 0.32;
    let shaft = shape.height * 0.24;
    let middle = shape.y + shape.height / 2.0;
    polygon_path(&[
        (shape.x, middle),
        (shape.x + head_width, shape.y),
        (shape.x + head_width, shape.y + shaft),
        (shape.x + shape.width - head_width, shape.y + shaft),
        (shape.x + shape.width - head_width, shape.y),
        (shape.x + shape.width, middle),
        (shape.x + shape.width - head_width, shape.y + shape.height),
        (
            shape.x + shape.width - head_width,
            shape.y + shape.height - shaft,
        ),
        (shape.x + head_width, shape.y + shape.height - shaft),
        (shape.x + head_width, shape.y + shape.height),
    ])
}

fn bent_arrow_path(shape: &Shape) -> String {
    let thickness = shape.width.min(shape.height) * 0.22;
    let turn_x = shape.x + shape.width * 0.62;
    let head = (shape.width * 0.38).min(shape.height * 0.38);
    polygon_path(&[
        (shape.x, shape.y + shape.height),
        (shape.x, shape.y + shape.height - thickness),
        (turn_x - thickness, shape.y + shape.height - thickness),
        (turn_x - thickness, shape.y + head),
        (turn_x - head, shape.y + head),
        (turn_x, shape.y),
        (turn_x + head, shape.y + head),
        (turn_x + thickness, shape.y + head),
        (turn_x + thickness, shape.y + shape.height),
    ])
}

fn bent_up_arrow_path(shape: &Shape) -> String {
    let thickness = shape.width.min(shape.height) * 0.22;
    let turn_y = shape.y + shape.height * 0.62;
    let head = (shape.width * 0.38).min(shape.height * 0.38);
    polygon_path(&[
        (shape.x, turn_y - thickness),
        (shape.x + shape.width - head, turn_y - thickness),
        (shape.x + shape.width - head, turn_y - head),
        (shape.x + shape.width, turn_y),
        (shape.x + shape.width - head, turn_y + head),
        (shape.x + shape.width - head, turn_y + thickness),
        (shape.x + thickness, turn_y + thickness),
        (shape.x + thickness, shape.y + shape.height),
        (shape.x, shape.y + shape.height),
    ])
}

fn uturn_arrow_path(shape: &Shape) -> String {
    let thickness = shape.width.min(shape.height) * 0.18;
    let radius = (shape.width * 0.36).min(shape.height * 0.36);
    let head = (shape.width * 0.22).min(shape.height * 0.22);
    let inner = (radius - thickness).max(0.01);
    format!(
        "M {} {} V {} A {} {} 0 0 1 {} {} H {} A {} {} 0 0 1 {} {} V {} H {} L {} {} L {} {} H {} V {} A {} {} 0 0 0 {} {} H {} A {} {} 0 0 0 {} {} V {} Z",
        fmt(shape.x),
        fmt(shape.y + shape.height),
        fmt(shape.y + radius),
        fmt(radius),
        fmt(radius),
        fmt(shape.x + radius),
        fmt(shape.y),
        fmt(shape.x + shape.width - radius),
        fmt(radius),
        fmt(radius),
        fmt(shape.x + shape.width),
        fmt(shape.y + radius),
        fmt(shape.y + shape.height - head),
        fmt(shape.x + shape.width - head),
        fmt(shape.x + shape.width - head / 2.0),
        fmt(shape.y + shape.height),
        fmt(shape.x + shape.width - head * 2.0),
        fmt(shape.y + shape.height - head),
        fmt(shape.x + shape.width - thickness),
        fmt(shape.y + radius),
        fmt(inner),
        fmt(inner),
        fmt(shape.x + shape.width - radius),
        fmt(shape.y + thickness),
        fmt(shape.x + radius),
        fmt(inner),
        fmt(inner),
        fmt(shape.x + thickness),
        fmt(shape.y + radius),
        fmt(shape.y + shape.height)
    )
}

fn curved_left_arrow_path(shape: &Shape) -> String {
    format!(
        "M {} {} C {} {} {} {} {} {} L {} {} L {} {} L {} {} L {} {} C {} {} {} {} {} {} Z",
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height * 0.2),
        fmt(shape.x + shape.width * 0.48),
        fmt(shape.y + shape.height * 0.16),
        fmt(shape.x + shape.width * 0.34),
        fmt(shape.y + shape.height * 0.42),
        fmt(shape.x + shape.width * 0.34),
        fmt(shape.y + shape.height * 0.62),
        fmt(shape.x + shape.width * 0.58),
        fmt(shape.y + shape.height * 0.62),
        fmt(shape.x + shape.width * 0.28),
        fmt(shape.y + shape.height),
        fmt(shape.x),
        fmt(shape.y + shape.height * 0.62),
        fmt(shape.x + shape.width * 0.2),
        fmt(shape.y + shape.height * 0.62),
        fmt(shape.x + shape.width * 0.2),
        fmt(shape.y + shape.height * 0.2),
        fmt(shape.x + shape.width * 0.52),
        fmt(shape.y),
        fmt(shape.x + shape.width),
        fmt(shape.y)
    )
}

fn curved_up_arrow_path(shape: &Shape) -> String {
    format!(
        "M {} {} C {} {} {} {} {} {} L {} {} L {} {} L {} {} L {} {} C {} {} {} {} {} {} Z",
        fmt(shape.x + shape.width * 0.8),
        fmt(shape.y + shape.height),
        fmt(shape.x + shape.width * 0.84),
        fmt(shape.y + shape.height * 0.48),
        fmt(shape.x + shape.width * 0.58),
        fmt(shape.y + shape.height * 0.34),
        fmt(shape.x + shape.width * 0.38),
        fmt(shape.y + shape.height * 0.34),
        fmt(shape.x + shape.width * 0.38),
        fmt(shape.y + shape.height * 0.58),
        fmt(shape.x),
        fmt(shape.y + shape.height * 0.28),
        fmt(shape.x + shape.width * 0.38),
        fmt(shape.y),
        fmt(shape.x + shape.width * 0.38),
        fmt(shape.y + shape.height * 0.2),
        fmt(shape.x + shape.width * 0.8),
        fmt(shape.y + shape.height * 0.2),
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height * 0.52),
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height)
    )
}

fn curved_down_arrow_path(shape: &Shape) -> String {
    format!(
        "M {} {} C {} {} {} {} {} {} L {} {} L {} {} L {} {} L {} {} C {} {} {} {} {} {} Z",
        fmt(shape.x + shape.width * 0.2),
        fmt(shape.y),
        fmt(shape.x + shape.width * 0.16),
        fmt(shape.y + shape.height * 0.52),
        fmt(shape.x + shape.width * 0.42),
        fmt(shape.y + shape.height * 0.66),
        fmt(shape.x + shape.width * 0.62),
        fmt(shape.y + shape.height * 0.66),
        fmt(shape.x + shape.width * 0.62),
        fmt(shape.y + shape.height * 0.42),
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height * 0.72),
        fmt(shape.x + shape.width * 0.62),
        fmt(shape.y + shape.height),
        fmt(shape.x + shape.width * 0.62),
        fmt(shape.y + shape.height * 0.8),
        fmt(shape.x + shape.width * 0.2),
        fmt(shape.y + shape.height * 0.8),
        fmt(shape.x),
        fmt(shape.y + shape.height * 0.48),
        fmt(shape.x),
        fmt(shape.y)
    )
}

fn up_down_arrow_path(shape: &Shape) -> String {
    let left = shape.x + shape.width * 0.36;
    let right = shape.x + shape.width * 0.64;
    let head = shape.height * 0.3;
    polygon_path(&[
        (shape.x + shape.width / 2.0, shape.y),
        (shape.x + shape.width, shape.y + head),
        (right, shape.y + head),
        (right, shape.y + shape.height - head),
        (shape.x + shape.width, shape.y + shape.height - head),
        (shape.x + shape.width / 2.0, shape.y + shape.height),
        (shape.x, shape.y + shape.height - head),
        (left, shape.y + shape.height - head),
        (left, shape.y + head),
        (shape.x, shape.y + head),
    ])
}

fn circular_arrow_path(shape: &Shape) -> String {
    let center_x = shape.x + shape.width / 2.0;
    let center_y = shape.y + shape.height / 2.0;
    let outer_x = shape.width * 0.48;
    let outer_y = shape.height * 0.48;
    let inner_x = shape.width * 0.28;
    let inner_y = shape.height * 0.28;
    format!(
        "M {} {} A {} {} 0 1 1 {} {} L {} {} L {} {} L {} {} L {} {} A {} {} 0 1 0 {} {} Z",
        fmt(center_x),
        fmt(center_y - outer_y),
        fmt(outer_x),
        fmt(outer_y),
        fmt(center_x - outer_x * 0.78),
        fmt(center_y + outer_y * 0.62),
        fmt(shape.x + shape.width * 0.08),
        fmt(shape.y + shape.height * 0.76),
        fmt(shape.x),
        fmt(shape.y + shape.height * 0.38),
        fmt(shape.x + shape.width * 0.38),
        fmt(shape.y + shape.height * 0.48),
        fmt(shape.x + shape.width * 0.24),
        fmt(shape.y + shape.height * 0.57),
        fmt(inner_x),
        fmt(inner_y),
        fmt(center_x),
        fmt(center_y - inner_y)
    )
}

fn irregular_seal_path(shape: &Shape, alternate: bool) -> String {
    let center_x = shape.x + shape.width / 2.0;
    let center_y = shape.y + shape.height / 2.0;
    let mut points = Vec::with_capacity(24);
    for index in 0..24 {
        let angle = -PI / 2.0 + index as f64 * 2.0 * PI / 24.0;
        let radius = if alternate {
            [1.0, 0.73, 0.92, 0.81][index % 4]
        } else {
            [1.0, 0.82, 0.95, 0.76][index % 4]
        };
        points.push((
            center_x + shape.width / 2.0 * radius * angle.cos(),
            center_y + shape.height / 2.0 * radius * angle.sin(),
        ));
    }
    polygon_path(&points)
}

fn wave_path(shape: &Shape) -> String {
    let adjustment = shape
        .preset_adjustments
        .get("adj1")
        .copied()
        .unwrap_or(6_000.0)
        .clamp(0.0, 25_000.0)
        / 100_000.0;
    let amplitude = shape.height * (0.12 + adjustment);
    format!(
        "M {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} L {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} Z",
        fmt(shape.x),
        fmt(shape.y + amplitude),
        fmt(shape.x + shape.width * 0.25),
        fmt(shape.y - amplitude),
        fmt(shape.x + shape.width * 0.25),
        fmt(shape.y + amplitude * 2.0),
        fmt(shape.x + shape.width * 0.5),
        fmt(shape.y + amplitude),
        fmt(shape.x + shape.width * 0.75),
        fmt(shape.y - amplitude),
        fmt(shape.x + shape.width * 0.75),
        fmt(shape.y + amplitude * 2.0),
        fmt(shape.x + shape.width),
        fmt(shape.y + amplitude),
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height - amplitude),
        fmt(shape.x + shape.width * 0.75),
        fmt(shape.y + shape.height + amplitude),
        fmt(shape.x + shape.width * 0.75),
        fmt(shape.y + shape.height - amplitude * 2.0),
        fmt(shape.x + shape.width * 0.5),
        fmt(shape.y + shape.height - amplitude),
        fmt(shape.x + shape.width * 0.25),
        fmt(shape.y + shape.height + amplitude),
        fmt(shape.x + shape.width * 0.25),
        fmt(shape.y + shape.height - amplitude * 2.0),
        fmt(shape.x),
        fmt(shape.y + shape.height - amplitude)
    )
}

fn action_button_forward_path(shape: &Shape) -> String {
    let radius = shape.width.min(shape.height) * 0.14;
    format!(
        "M {} {} H {} Q {} {} {} {} V {} Q {} {} {} {} H {} Q {} {} {} {} V {} Q {} {} {} {} Z M {} {} L {} {} L {} {} Z",
        fmt(shape.x + radius),
        fmt(shape.y),
        fmt(shape.x + shape.width - radius),
        fmt(shape.x + shape.width),
        fmt(shape.y),
        fmt(shape.x + shape.width),
        fmt(shape.y + radius),
        fmt(shape.y + shape.height - radius),
        fmt(shape.x + shape.width),
        fmt(shape.y + shape.height),
        fmt(shape.x + shape.width - radius),
        fmt(shape.y + shape.height),
        fmt(shape.x + radius),
        fmt(shape.x),
        fmt(shape.y + shape.height),
        fmt(shape.x),
        fmt(shape.y + shape.height - radius),
        fmt(shape.y + radius),
        fmt(shape.x),
        fmt(shape.y),
        fmt(shape.x + radius),
        fmt(shape.y),
        fmt(shape.x + shape.width * 0.32),
        fmt(shape.y + shape.height * 0.22),
        fmt(shape.x + shape.width * 0.72),
        fmt(shape.y + shape.height / 2.0),
        fmt(shape.x + shape.width * 0.32),
        fmt(shape.y + shape.height * 0.78)
    )
}

fn wedge_round_rect_callout_path(shape: &Shape) -> String {
    let tip_x = shape.x
        + shape.width / 2.0
        + shape.width
            * shape
                .preset_adjustments
                .get("adj1")
                .copied()
                .unwrap_or(-20_833.0)
            / 100_000.0;
    let tip_y = shape.y
        + shape.height / 2.0
        + shape.height
            * shape
                .preset_adjustments
                .get("adj2")
                .copied()
                .unwrap_or(62_500.0)
            / 100_000.0;
    let radius = (shape.width.min(shape.height)
        * shape
            .preset_adjustments
            .get("adj3")
            .copied()
            .unwrap_or(16_667.0)
        / 100_000.0)
        .clamp(0.0, shape.width.min(shape.height) / 2.0);
    let dx = (tip_x - (shape.x + shape.width / 2.0)) / shape.width.max(1e-12);
    let dy = (tip_y - (shape.y + shape.height / 2.0)) / shape.height.max(1e-12);
    let right = shape.x + shape.width;
    let bottom = shape.y + shape.height;
    if dx.abs() > dy.abs() && dx < 0.0 {
        let center = tip_y.clamp(shape.y + radius, bottom - radius);
        let half = (shape.height * 0.1).min((center - shape.y).min(bottom - center));
        format!(
            "M {} {} A {} {} 0 0 1 {} {} H {} A {} {} 0 0 1 {} {} V {} A {} {} 0 0 1 {} {} H {} A {} {} 0 0 1 {} {} V {} L {} {} L {} {} Z",
            fmt(shape.x),
            fmt(shape.y + radius),
            fmt(radius),
            fmt(radius),
            fmt(shape.x + radius),
            fmt(shape.y),
            fmt(right - radius),
            fmt(radius),
            fmt(radius),
            fmt(right),
            fmt(shape.y + radius),
            fmt(bottom - radius),
            fmt(radius),
            fmt(radius),
            fmt(right - radius),
            fmt(bottom),
            fmt(shape.x + radius),
            fmt(radius),
            fmt(radius),
            fmt(shape.x),
            fmt(bottom - radius),
            fmt(center + half),
            fmt(tip_x),
            fmt(tip_y),
            fmt(shape.x),
            fmt(center - half)
        )
    } else if dx.abs() > dy.abs() {
        let center = tip_y.clamp(shape.y + radius, bottom - radius);
        let half = (shape.height * 0.1).min((center - shape.y).min(bottom - center));
        format!(
            "M {} {} A {} {} 0 0 1 {} {} H {} A {} {} 0 0 1 {} {} V {} L {} {} L {} {} V {} A {} {} 0 0 1 {} {} H {} A {} {} 0 0 1 {} {} Z",
            fmt(shape.x),
            fmt(shape.y + radius),
            fmt(radius),
            fmt(radius),
            fmt(shape.x + radius),
            fmt(shape.y),
            fmt(right - radius),
            fmt(radius),
            fmt(radius),
            fmt(right),
            fmt(shape.y + radius),
            fmt(center - half),
            fmt(tip_x),
            fmt(tip_y),
            fmt(right),
            fmt(center + half),
            fmt(bottom - radius),
            fmt(radius),
            fmt(radius),
            fmt(right - radius),
            fmt(bottom),
            fmt(shape.x + radius),
            fmt(radius),
            fmt(radius),
            fmt(shape.x),
            fmt(bottom - radius)
        )
    } else if dy < 0.0 {
        let center = tip_x.clamp(shape.x + radius, right - radius);
        let half = (shape.width * 0.1).min((center - shape.x).min(right - center));
        format!(
            "M {} {} A {} {} 0 0 1 {} {} H {} L {} {} L {} {} H {} A {} {} 0 0 1 {} {} V {} A {} {} 0 0 1 {} {} H {} A {} {} 0 0 1 {} {} Z",
            fmt(shape.x),
            fmt(shape.y + radius),
            fmt(radius),
            fmt(radius),
            fmt(shape.x + radius),
            fmt(shape.y),
            fmt(center - half),
            fmt(tip_x),
            fmt(tip_y),
            fmt(center + half),
            fmt(shape.y),
            fmt(right - radius),
            fmt(radius),
            fmt(radius),
            fmt(right),
            fmt(shape.y + radius),
            fmt(bottom - radius),
            fmt(radius),
            fmt(radius),
            fmt(right - radius),
            fmt(bottom),
            fmt(shape.x + radius),
            fmt(radius),
            fmt(radius),
            fmt(shape.x),
            fmt(bottom - radius)
        )
    } else {
        let center = tip_x.clamp(shape.x + radius, right - radius);
        let half = (shape.width * 0.1).min((center - shape.x).min(right - center));
        format!(
            "M {} {} A {} {} 0 0 1 {} {} H {} A {} {} 0 0 1 {} {} V {} A {} {} 0 0 1 {} {} H {} L {} {} L {} {} H {} A {} {} 0 0 1 {} {} Z",
            fmt(shape.x),
            fmt(shape.y + radius),
            fmt(radius),
            fmt(radius),
            fmt(shape.x + radius),
            fmt(shape.y),
            fmt(right - radius),
            fmt(radius),
            fmt(radius),
            fmt(right),
            fmt(shape.y + radius),
            fmt(bottom - radius),
            fmt(radius),
            fmt(radius),
            fmt(right - radius),
            fmt(bottom),
            fmt(center + half),
            fmt(tip_x),
            fmt(tip_y),
            fmt(center - half),
            fmt(bottom),
            fmt(shape.x + radius),
            fmt(radius),
            fmt(radius),
            fmt(shape.x),
            fmt(bottom - radius)
        )
    }
}

fn wedge_ellipse_callout_path(shape: &Shape) -> String {
    let dx = shape.width
        * shape
            .preset_adjustments
            .get("adj1")
            .copied()
            .unwrap_or(-20_833.0)
        / 100_000.0;
    let dy = shape.height
        * shape
            .preset_adjustments
            .get("adj2")
            .copied()
            .unwrap_or(62_500.0)
        / 100_000.0;
    let center_x = shape.x + shape.width / 2.0;
    let center_y = shape.y + shape.height / 2.0;
    let angle = (dy * shape.width).atan2(dx * shape.height);
    let half = 11.0_f64.to_radians();
    let radius_x = shape.width / 2.0;
    let radius_y = shape.height / 2.0;
    let start = angle + half;
    let end = angle - half;
    format!(
        "M {} {} L {} {} A {} {} 0 1 1 {} {} Z",
        fmt(center_x + dx),
        fmt(center_y + dy),
        fmt(center_x + radius_x * start.cos()),
        fmt(center_y + radius_y * start.sin()),
        fmt(radius_x),
        fmt(radius_y),
        fmt(center_x + radius_x * end.cos()),
        fmt(center_y + radius_y * end.sin())
    )
}

fn border_callout_path(shape: &Shape) -> String {
    let start_x = shape.x
        + shape.width * shape.preset_adjustments.get("adj1").copied().unwrap_or(0.0) / 100_000.0;
    let start_y = shape.y
        + shape.height * shape.preset_adjustments.get("adj2").copied().unwrap_or(0.0) / 100_000.0;
    let end_x = shape.x
        + shape.width
            * shape
                .preset_adjustments
                .get("adj3")
                .copied()
                .unwrap_or(112_500.0)
            / 100_000.0;
    let end_y = shape.y
        + shape.height
            * shape
                .preset_adjustments
                .get("adj4")
                .copied()
                .unwrap_or(-38_333.0)
            / 100_000.0;
    format!(
        "{} M {} {} L {} {}",
        rectangle_path(shape.x, shape.y, shape.width, shape.height),
        fmt(start_x),
        fmt(start_y),
        fmt(end_x),
        fmt(end_y)
    )
}

fn wedge_rect_callout_path(shape: &Shape) -> String {
    let width = shape.width;
    let height = shape.height;
    let adjustment_x = shape
        .preset_adjustments
        .get("adj1")
        .copied()
        .unwrap_or(-20_833.0);
    let adjustment_y = shape
        .preset_adjustments
        .get("adj2")
        .copied()
        .unwrap_or(62_500.0);
    let dx_position = width * adjustment_x / 100_000.0;
    let dy_position = height * adjustment_y / 100_000.0;
    let x_position = width / 2.0 + dx_position;
    let y_position = height / 2.0 + dy_position;
    let diagonal_x = dx_position * height / width.max(1e-12);
    let distance_difference = dy_position.abs() - diagonal_x.abs();
    let x_group_1 = if dx_position > 0.0 { 7.0 } else { 2.0 };
    let x_group_2 = if dx_position > 0.0 { 10.0 } else { 5.0 };
    let x1 = width * x_group_1 / 12.0;
    let x2 = width * x_group_2 / 12.0;
    let y_group_1 = if dy_position > 0.0 { 7.0 } else { 2.0 };
    let y_group_2 = if dy_position > 0.0 { 10.0 } else { 5.0 };
    let y1 = height * y_group_1 / 12.0;
    let y2 = height * y_group_2 / 12.0;
    let left_tip_x = if dx_position > 0.0 { 0.0 } else { x_position };
    let left_x = if distance_difference > 0.0 {
        0.0
    } else {
        left_tip_x
    };
    let top_tip_x = if dy_position > 0.0 { x1 } else { x_position };
    let top_x = if distance_difference > 0.0 {
        top_tip_x
    } else {
        x1
    };
    let right_tip_x = if dx_position > 0.0 { x_position } else { width };
    let right_x = if distance_difference > 0.0 {
        width
    } else {
        right_tip_x
    };
    let bottom_tip_x = if dy_position > 0.0 { x_position } else { x1 };
    let bottom_x = if distance_difference > 0.0 {
        bottom_tip_x
    } else {
        x1
    };
    let left_tip_y = if dx_position > 0.0 { y1 } else { y_position };
    let left_y = if distance_difference > 0.0 {
        y1
    } else {
        left_tip_y
    };
    let top_tip_y = if dy_position > 0.0 { 0.0 } else { y_position };
    let top_y = if distance_difference > 0.0 {
        top_tip_y
    } else {
        0.0
    };
    let right_tip_y = if dx_position > 0.0 { y_position } else { y1 };
    let right_y = if distance_difference > 0.0 {
        y1
    } else {
        right_tip_y
    };
    let bottom_tip_y = if dy_position > 0.0 {
        y_position
    } else {
        height
    };
    let bottom_y = if distance_difference > 0.0 {
        bottom_tip_y
    } else {
        height
    };
    let point = |x: f64, y: f64| (shape.x + x, shape.y + y);
    polygon_path(&[
        point(0.0, 0.0),
        point(x1, 0.0),
        point(top_x, top_y),
        point(x2, 0.0),
        point(width, 0.0),
        point(width, y1),
        point(right_x, right_y),
        point(width, y2),
        point(width, height),
        point(x2, height),
        point(bottom_x, bottom_y),
        point(x1, height),
        point(0.0, height),
        point(0.0, y2),
        point(left_x, left_y),
        point(0.0, y1),
    ])
}

fn rectangle_path(x: f64, y: f64, width: f64, height: f64) -> String {
    format!(
        "M {} {} H {} V {} H {} Z",
        fmt(x),
        fmt(y),
        fmt(x + width),
        fmt(y + height),
        fmt(x)
    )
}

fn selective_round_rect_path(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    rounded: [bool; 4],
) -> String {
    let radius = width.min(height) * 0.1;
    let [top_left, top_right, bottom_right, bottom_left] = rounded;
    format!(
        "M {} {} H {} {} V {} {} H {} {} V {} {} Z",
        fmt(if top_left { x + radius } else { x }),
        fmt(y),
        fmt(if top_right {
            x + width - radius
        } else {
            x + width
        }),
        if top_right {
            format!(
                "A {} {} 0 0 1 {} {}",
                fmt(radius),
                fmt(radius),
                fmt(x + width),
                fmt(y + radius)
            )
        } else {
            String::new()
        },
        fmt(if bottom_right {
            y + height - radius
        } else {
            y + height
        }),
        if bottom_right {
            format!(
                "A {} {} 0 0 1 {} {}",
                fmt(radius),
                fmt(radius),
                fmt(x + width - radius),
                fmt(y + height)
            )
        } else {
            String::new()
        },
        fmt(if bottom_left { x + radius } else { x }),
        if bottom_left {
            format!(
                "A {} {} 0 0 1 {} {}",
                fmt(radius),
                fmt(radius),
                fmt(x),
                fmt(y + height - radius)
            )
        } else {
            String::new()
        },
        fmt(if top_left { y + radius } else { y }),
        if top_left {
            format!(
                "A {} {} 0 0 1 {} {}",
                fmt(radius),
                fmt(radius),
                fmt(x + radius),
                fmt(y)
            )
        } else {
            String::new()
        },
    )
}

fn polygon_path(points: &[(f64, f64)]) -> String {
    let Some((first, rest)) = points.split_first() else {
        return String::new();
    };
    let mut path = format!("M {} {}", fmt(first.0), fmt(first.1));
    for point in rest {
        path.push_str(&format!(" L {} {}", fmt(point.0), fmt(point.1)));
    }
    path.push_str(" Z");
    path
}

fn regular_polygon_path(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    sides: usize,
    angle_degrees: f64,
) -> String {
    let center_x = x + width / 2.0;
    let center_y = y + height / 2.0;
    let points = (0..sides)
        .map(|index| {
            let angle = (angle_degrees + index as f64 * 360.0 / sides as f64) * PI / 180.0;
            (
                center_x + angle.cos() * width / 2.0,
                center_y + angle.sin() * height / 2.0,
            )
        })
        .collect::<Vec<_>>();
    polygon_path(&points)
}

fn arrow_path(x: f64, y: f64, width: f64, height: f64, quarter_turns: usize) -> String {
    let points = [
        (0.0, 0.25),
        (0.62, 0.25),
        (0.62, 0.0),
        (1.0, 0.5),
        (0.62, 1.0),
        (0.62, 0.75),
        (0.0, 0.75),
    ]
    .map(|(mut px, mut py)| {
        for _ in 0..quarter_turns {
            (px, py) = (1.0 - py, px);
        }
        (x + px * width, y + py * height)
    });
    polygon_path(&points)
}

fn plus_path(x: f64, y: f64, width: f64, height: f64) -> String {
    polygon_path(&[
        (x + width * 0.35, y),
        (x + width * 0.65, y),
        (x + width * 0.65, y + height * 0.35),
        (x + width, y + height * 0.35),
        (x + width, y + height * 0.65),
        (x + width * 0.65, y + height * 0.65),
        (x + width * 0.65, y + height),
        (x + width * 0.35, y + height),
        (x + width * 0.35, y + height * 0.65),
        (x, y + height * 0.65),
        (x, y + height * 0.35),
        (x + width * 0.35, y + height * 0.35),
    ])
}

fn star_path(x: f64, y: f64, width: f64, height: f64, points: usize) -> String {
    let center_x = x + width / 2.0;
    let center_y = y + height / 2.0;
    let vertices = (0..points * 2)
        .map(|index| {
            let radius = if index % 2 == 0 { 1.0 } else { 0.4 };
            let angle = (-90.0 + index as f64 * 180.0 / points as f64) * PI / 180.0;
            (
                center_x + angle.cos() * width / 2.0 * radius,
                center_y + angle.sin() * height / 2.0 * radius,
            )
        })
        .collect::<Vec<_>>();
    polygon_path(&vertices)
}

fn rotation_matrix(degrees: f64, center_x: f64, center_y: f64) -> Matrix {
    if degrees.abs() < 1e-9 {
        return IDENTITY;
    }
    let radians = degrees * PI / 180.0;
    let cosine = radians.cos();
    let sine = radians.sin();
    [
        cosine,
        sine,
        -sine,
        cosine,
        center_x - cosine * center_x + sine * center_y,
        center_y - sine * center_x - cosine * center_y,
    ]
}

fn office_font_stack(typeface: &str) -> String {
    let normalized = typeface.trim();
    if normalized.contains("ヒラギノ角ゴ") || normalized.to_ascii_lowercase().contains("hiragino")
    {
        return "'Hiragino Sans', 'Yu Gothic', YuGothic, sans-serif".into();
    }
    if normalized.eq_ignore_ascii_case("meiryo") || normalized.contains("メイリオ") {
        return "Meiryo, 'Hiragino Sans', 'Yu Gothic', YuGothic, sans-serif".into();
    }
    if normalized.to_ascii_lowercase().contains("yu gothic") || normalized.contains("游ゴシック")
    {
        return "'Yu Gothic', YuGothic, 'Hiragino Sans', sans-serif".into();
    }
    normalized.to_owned()
}

fn mime_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("emf") => "image/x-emf",
        Some("wmf") => "image/x-wmf",
        _ => "image/png",
    }
}

fn unsupported_metafile_label(path: &str) -> Option<&'static str> {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("emf" | "emz") => Some("EMF"),
        Some("wmf" | "wmz") => Some("WMF"),
        _ => None,
    }
}

fn fmt(value: f64) -> String {
    let value = if value.abs() < 0.000_005 { 0.0 } else { value };
    format!("{value:.5}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn deduplicate(values: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        if !result.contains(&value) {
            result.push(value);
        }
    }
    result
}
