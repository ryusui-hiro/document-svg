use std::collections::{HashMap, HashSet};
use std::path::Path;

use base64::Engine;
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};
use crate::ooxml::{
    Relationships, ZipPackage, attribute, color_from_hex, local_name, parse_f64, parse_i64,
    qualified_attribute, text_advance_factor,
};

const TWIPS_PER_POINT: f64 = 20.0;
const EMU_PER_POINT: f64 = 12_700.0;
const DEFAULT_PAGE_WIDTH: f64 = 612.0;
const DEFAULT_PAGE_HEIGHT: f64 = 792.0;
const PAGE_FIELD_MARKER: &str = "\u{e000}DOCSVG_PAGE\u{e001}";
const PAGE_BREAK_MARKER: &str = "\u{e000}DOCSVG_BREAK\u{e001}";
const NOTE_FIELD_START: char = '\u{e100}';
const NOTE_FIELD_END: char = '\u{e101}';

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut package = ZipPackage::open(path, options.max_zip_entry_bytes)?;
    let document_part = "word/document.xml";
    if !package.contains(document_part) {
        return Err(Error::InvalidInput(
            "DOCX is missing word/document.xml".into(),
        ));
    }
    let styles = package
        .read_optional("word/styles.xml")?
        .map(|xml| Styles::parse(&xml, options.max_xml_events))
        .transpose()?
        .unwrap_or_default();
    let numbering = package
        .read_optional("word/numbering.xml")?
        .map(|xml| Numbering::parse(&xml, options.max_xml_events))
        .transpose()?
        .unwrap_or_default();
    let footnotes = package
        .read_optional("word/footnotes.xml")?
        .map(|xml| parse_notes(&xml, "footnote", &styles, options.max_xml_events))
        .transpose()?
        .unwrap_or_default();
    let endnotes = package
        .read_optional("word/endnotes.xml")?
        .map(|xml| parse_notes(&xml, "endnote", &styles, options.max_xml_events))
        .transpose()?
        .unwrap_or_default();
    let comments = package
        .read_optional("word/comments.xml")?
        .map(|xml| parse_notes(&xml, "comment", &styles, options.max_xml_events))
        .transpose()?
        .unwrap_or_default();
    let relationships = package.relationships(document_part, options.max_xml_events)?;
    let document_xml = package.read(document_part)?;
    let mut document = parse_document(
        &document_xml,
        &styles,
        &numbering,
        &relationships,
        document_part,
        &mut package,
        options.max_xml_events,
    )?;
    let mut story_ids = Vec::<(String, &'static str)>::new();
    for section in &document.section_ranges {
        for (id, kind) in section.stories.ids() {
            if !story_ids.iter().any(|(existing, _)| existing == id) {
                story_ids.push((id.to_owned(), kind));
            }
        }
    }
    for (story_id, story_kind) in story_ids {
        let Some(story_part) = relationships.target(&story_id, document_part) else {
            document
                .warnings
                .push(format!("{story_kind} relationship {story_id} is missing"));
            continue;
        };
        let Some(story_xml) = package.read_optional(&story_part)? else {
            document
                .warnings
                .push(format!("{story_kind} part {story_part} is missing"));
            continue;
        };
        let story_relationships = package.relationships(&story_part, options.max_xml_events)?;
        let story = parse_document(
            &story_xml,
            &styles,
            &numbering,
            &story_relationships,
            &story_part,
            &mut package,
            options.max_xml_events,
        )?;
        document.warnings.extend(
            story
                .warnings
                .into_iter()
                .map(|warning| format!("{story_kind}: {warning}")),
        );
        document.stories.insert(story_id, story.blocks);
    }
    render_document(
        document,
        &numbering,
        &footnotes,
        &endnotes,
        &comments,
        sink,
        options.max_pages,
    )
}

fn parse_notes(
    xml: &[u8],
    element_name: &str,
    styles: &Styles,
    max_events: usize,
) -> Result<HashMap<String, Vec<Paragraph>>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut notes = HashMap::new();
    let mut note_id = None::<String>;
    let mut paragraphs = Vec::<Paragraph>::new();
    let mut paragraph = None::<Paragraph>;
    let mut run = None::<TextRun>;
    let mut text = String::new();
    let mut stack = Vec::<String>::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "DOCX {element_name}s.xml event limit exceeded"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if name == element_name {
                    let id = attribute(&start, b"id").unwrap_or_default();
                    if id.parse::<i64>().is_ok_and(|value| value >= 0) {
                        note_id = Some(id);
                        paragraphs.clear();
                    }
                } else if note_id.is_some() {
                    match name.as_str() {
                        "p" => paragraph = Some(Paragraph::default()),
                        "r" => run = Some(default_text_run(styles)),
                        "t" => text.clear(),
                        _ => apply_note_run_property(&start, &name, &stack, run.as_mut()),
                    }
                }
                stack.push(name);
            }
            Event::Empty(start) if note_id.is_some() => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                match name.as_str() {
                    "tab" => run
                        .get_or_insert_with(|| default_text_run(styles))
                        .text
                        .push('\t'),
                    "br" => run
                        .get_or_insert_with(|| default_text_run(styles))
                        .text
                        .push('\n'),
                    _ => apply_note_run_property(&start, &name, &stack, run.as_mut()),
                }
            }
            Event::Text(value)
                if note_id.is_some() && stack.last().is_some_and(|name| name == "t") =>
            {
                text.push_str(&value.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid DOCX note text: {error}"))
                })?);
            }
            Event::End(end) => {
                let qualified_name = end.name();
                let name = local_name(qualified_name.as_ref());
                if note_id.is_some() {
                    match name {
                        b"t" => {
                            run.get_or_insert_with(|| default_text_run(styles))
                                .text
                                .push_str(&text);
                            text.clear();
                        }
                        b"r" => {
                            if let (Some(paragraph), Some(run)) = (paragraph.as_mut(), run.take()) {
                                paragraph.runs.push(run);
                            }
                        }
                        b"p" => {
                            if let Some(mut finished) = paragraph.take() {
                                if let Some(run) = run.take() {
                                    finished.runs.push(run);
                                }
                                finished.style = styles.default.clone();
                                paragraphs.push(finished);
                            }
                        }
                        _ if name == element_name.as_bytes() => {
                            if let Some(id) = note_id.take() {
                                notes.insert(id, std::mem::take(&mut paragraphs));
                            }
                        }
                        _ => {}
                    }
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(notes)
}

fn apply_note_run_property(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    run: Option<&mut TextRun>,
) {
    let Some(run) = run else {
        return;
    };
    match name {
        "rFonts" => {
            apply_docx_run_fonts(run, start);
        }
        "sz" if stack.iter().any(|item| item == "rPr") => {
            run.font_size = parse_i64(attribute(start, b"val"), 18) as f64 / 2.0;
        }
        "b" => run.bold = toggle_value(start),
        "i" => run.italic = toggle_value(start),
        "vertAlign" => apply_docx_vertical_alignment(
            run,
            attribute(start, b"val").as_deref().unwrap_or_default(),
        ),
        "color" => {
            if let Some(color) = attribute(start, b"val")
                && color != "auto"
            {
                run.fill = Paint::solid(color_from_hex(&color, "#000000"));
            }
        }
        _ => {}
    }
}

fn docx_font_family(start: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    let latin = attribute(start, b"ascii").or_else(|| attribute(start, b"hAnsi"));
    let east_asian = attribute(start, b"eastAsia");
    match (latin, east_asian) {
        (Some(latin), Some(east_asian)) if !latin.eq_ignore_ascii_case(&east_asian) => {
            Some(format!("{latin}, {east_asian}"))
        }
        (Some(latin), _) => Some(latin),
        (None, Some(east_asian)) => Some(east_asian),
        (None, None) => attribute(start, b"cs"),
    }
}

fn apply_docx_run_fonts(run: &mut TextRun, start: &quick_xml::events::BytesStart<'_>) {
    let latin = attribute(start, b"ascii").or_else(|| attribute(start, b"hAnsi"));
    let east_asian = attribute(start, b"eastAsia");
    if let Some(latin) = latin {
        run.font_family = if let Some(east_asian) = east_asian
            && !latin.eq_ignore_ascii_case(&east_asian)
        {
            format!("{latin}, {east_asian}")
        } else {
            latin
        };
    } else if let Some(east_asian) = east_asian
        && !run
            .font_family
            .split(',')
            .any(|font| font.trim().eq_ignore_ascii_case(&east_asian))
    {
        run.font_family = format!("{}, {east_asian}", run.font_family);
    }
}

#[derive(Clone, Debug)]
struct PageSetup {
    width: f64,
    height: f64,
    margin_top: f64,
    margin_right: f64,
    margin_bottom: f64,
    margin_left: f64,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            width: DEFAULT_PAGE_WIDTH,
            height: DEFAULT_PAGE_HEIGHT,
            margin_top: 72.0,
            margin_right: 72.0,
            margin_bottom: 72.0,
            margin_left: 72.0,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct ParagraphStyle {
    font_family: Option<String>,
    font_size: Option<f64>,
    bold: Option<bool>,
    italic: Option<bool>,
    color: Option<String>,
    alignment: Option<TextAnchor>,
    space_before: Option<f64>,
    space_after: Option<f64>,
    line_spacing: Option<f64>,
    left_indent: Option<f64>,
    right_indent: Option<f64>,
    first_line_indent: Option<f64>,
}

impl ParagraphStyle {
    fn merge(&mut self, other: &Self) {
        macro_rules! inherit {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field.clone_from(&other.$field);
                }
            };
        }
        inherit!(font_family);
        inherit!(font_size);
        inherit!(bold);
        inherit!(italic);
        inherit!(color);
        inherit!(alignment);
        inherit!(space_before);
        inherit!(space_after);
        inherit!(line_spacing);
        inherit!(left_indent);
        inherit!(right_indent);
        inherit!(first_line_indent);
    }
}

#[derive(Clone, Debug)]
struct Styles {
    default: ParagraphStyle,
    by_id: HashMap<String, ParagraphStyle>,
    based_on: HashMap<String, String>,
}

impl Default for Styles {
    fn default() -> Self {
        Self {
            default: ParagraphStyle {
                font_family: Some(
                    "Calibri, Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into(),
                ),
                font_size: Some(11.0),
                color: Some("#000000".into()),
                space_after: Some(8.0),
                line_spacing: Some(1.2),
                ..ParagraphStyle::default()
            },
            by_id: HashMap::new(),
            based_on: HashMap::new(),
        }
    }
}

impl Styles {
    fn parse(xml: &[u8], max_events: usize) -> Result<Self> {
        let mut styles = Self::default();
        let mut reader = Reader::from_reader(xml);
        reader.config_mut().trim_text(true);
        let mut buffer = Vec::new();
        let mut stack = Vec::<String>::new();
        let mut current_id = None::<String>;
        let mut current_based_on = None::<String>;
        let mut current = ParagraphStyle::default();
        let mut in_defaults = false;
        let mut events = 0usize;
        loop {
            events += 1;
            if events > max_events {
                return Err(Error::LimitExceeded(
                    "styles.xml event limit exceeded".into(),
                ));
            }
            match reader.read_event_into(&mut buffer)? {
                Event::Start(start) => {
                    let name =
                        String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                    if name == "style" {
                        current_id = attribute(&start, b"styleId");
                        current_based_on = None;
                        current = ParagraphStyle::default();
                    } else if name == "docDefaults" {
                        in_defaults = true;
                    } else if name == "basedOn" && current_id.is_some() {
                        current_based_on = attribute(&start, b"val");
                    }
                    apply_style_property(&start, &name, &stack, &mut current);
                    stack.push(name);
                }
                Event::Empty(start) => {
                    let name =
                        String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                    if name == "basedOn" && current_id.is_some() {
                        current_based_on = attribute(&start, b"val");
                    }
                    apply_style_property(&start, &name, &stack, &mut current);
                }
                Event::End(end) => {
                    match local_name(end.name().as_ref()) {
                        b"style" => {
                            if let Some(id) = current_id.take() {
                                if let Some(parent) = current_based_on.take() {
                                    styles.based_on.insert(id.clone(), parent);
                                }
                                styles.by_id.insert(id, current.clone());
                            }
                        }
                        b"docDefaults" => {
                            styles.default.merge(&current);
                            current = ParagraphStyle::default();
                            in_defaults = false;
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
        let _ = in_defaults;
        Ok(styles)
    }

    fn resolve(&self, id: &str) -> ParagraphStyle {
        let mut result = self.default.clone();
        let mut chain = Vec::new();
        let mut current = id;
        let mut visited = HashSet::new();
        for _ in 0..32 {
            if !visited.insert(current.to_owned()) {
                break;
            }
            if let Some(style) = self.by_id.get(current) {
                chain.push(style);
            }
            let Some(parent) = self.based_on.get(current) else {
                break;
            };
            current = parent;
        }
        for style in chain.into_iter().rev() {
            result.merge(style);
        }
        result
    }
}

#[derive(Clone, Debug, Default)]
struct Numbering {
    instances: HashMap<String, String>,
    levels: HashMap<(String, usize), NumberLevel>,
}

#[derive(Clone, Debug)]
struct NumberLevel {
    format: String,
    text: String,
    start: usize,
    left_indent: Option<f64>,
    hanging_indent: Option<f64>,
}

impl Default for NumberLevel {
    fn default() -> Self {
        Self {
            format: "decimal".into(),
            text: "%1.".into(),
            start: 1,
            left_indent: None,
            hanging_indent: None,
        }
    }
}

impl Numbering {
    fn parse(xml: &[u8], max_events: usize) -> Result<Self> {
        let mut reader = Reader::from_reader(xml);
        reader.config_mut().trim_text(true);
        let mut buffer = Vec::new();
        let mut result = Self::default();
        let mut abstract_id = None::<String>;
        let mut level = None::<(usize, NumberLevel)>;
        let mut number_id = None::<String>;
        let mut events = 0usize;
        loop {
            events += 1;
            if events > max_events {
                return Err(Error::LimitExceeded(
                    "DOCX numbering.xml event limit exceeded".into(),
                ));
            }
            match reader.read_event_into(&mut buffer)? {
                Event::Start(start) | Event::Empty(start) => {
                    match local_name(start.name().as_ref()) {
                        b"abstractNum" => abstract_id = attribute(&start, b"abstractNumId"),
                        b"lvl" => {
                            level = Some((
                                parse_i64(attribute(&start, b"ilvl"), 0).max(0) as usize,
                                NumberLevel::default(),
                            ));
                        }
                        b"start" => {
                            if let Some((_, level)) = level.as_mut() {
                                level.start =
                                    parse_i64(attribute(&start, b"val"), 1).max(1) as usize;
                            }
                        }
                        b"numFmt" => {
                            if let Some((_, level)) = level.as_mut() {
                                level.format =
                                    attribute(&start, b"val").unwrap_or_else(|| "decimal".into());
                            }
                        }
                        b"lvlText" => {
                            if let Some((_, level)) = level.as_mut() {
                                level.text =
                                    attribute(&start, b"val").unwrap_or_else(|| "%1.".into());
                            }
                        }
                        b"ind" => {
                            if let Some((_, level)) = level.as_mut() {
                                level.left_indent = attribute(&start, b"left")
                                    .or_else(|| attribute(&start, b"start"))
                                    .and_then(|value| value.parse::<f64>().ok())
                                    .map(|value| value / TWIPS_PER_POINT);
                                level.hanging_indent = attribute(&start, b"hanging")
                                    .and_then(|value| value.parse::<f64>().ok())
                                    .map(|value| value / TWIPS_PER_POINT);
                            }
                        }
                        b"num" => number_id = attribute(&start, b"numId"),
                        b"abstractNumId" if number_id.is_some() => {
                            if let (Some(number_id), Some(target)) =
                                (number_id.as_ref(), attribute(&start, b"val"))
                            {
                                result.instances.insert(number_id.clone(), target);
                            }
                        }
                        _ => {}
                    }
                }
                Event::End(end) => match local_name(end.name().as_ref()) {
                    b"lvl" => {
                        if let (Some(abstract_id), Some((level_index, level))) =
                            (abstract_id.as_ref(), level.take())
                        {
                            result
                                .levels
                                .insert((abstract_id.clone(), level_index), level);
                        }
                    }
                    b"abstractNum" => abstract_id = None,
                    b"num" => number_id = None,
                    _ => {}
                },
                Event::Eof => break,
                _ => {}
            }
            buffer.clear();
        }
        Ok(result)
    }

    fn level(&self, number_id: &str, level: usize) -> Option<&NumberLevel> {
        let abstract_id = self.instances.get(number_id)?;
        self.levels
            .get(&(abstract_id.clone(), level))
            .or_else(|| self.levels.get(&(abstract_id.clone(), 0)))
    }

    fn marker(
        &self,
        number_id: &str,
        level: usize,
        counters: &mut HashMap<(String, usize), usize>,
    ) -> Option<String> {
        let specification = self.level(number_id, level)?;
        counters.retain(|(id, item_level), _| id != number_id || *item_level <= level);
        let counter = counters
            .entry((number_id.to_owned(), level))
            .or_insert(specification.start.saturating_sub(1));
        *counter += 1;
        if specification.format == "bullet" {
            return Some(specification.text.clone());
        }
        let mut marker = specification.text.clone();
        for referenced_level in 0..9 {
            let placeholder = format!("%{}", referenced_level + 1);
            if !marker.contains(&placeholder) {
                continue;
            }
            let referenced = self.level(number_id, referenced_level)?;
            let value = counters
                .get(&(number_id.to_owned(), referenced_level))
                .copied()
                .unwrap_or(referenced.start);
            marker = marker.replace(&placeholder, &format_list_number(value, &referenced.format));
        }
        Some(marker)
    }
}

fn format_list_number(value: usize, format: &str) -> String {
    match format {
        "lowerLetter" => alphabetic_number(value, false),
        "upperLetter" => alphabetic_number(value, true),
        "lowerRoman" => roman_number(value).to_ascii_lowercase(),
        "upperRoman" => roman_number(value),
        _ => value.to_string(),
    }
}

fn alphabetic_number(mut value: usize, uppercase: bool) -> String {
    if value == 0 {
        return String::new();
    }
    let mut output = Vec::new();
    while value > 0 {
        value -= 1;
        let base = if uppercase { b'A' } else { b'a' };
        output.push((base + (value % 26) as u8) as char);
        value /= 26;
    }
    output.into_iter().rev().collect()
}

fn roman_number(mut value: usize) -> String {
    let mut output = String::new();
    for (number, numeral) in [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ] {
        while value >= number {
            output.push_str(numeral);
            value -= number;
        }
    }
    output
}

fn apply_style_property(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    style: &mut ParagraphStyle,
) {
    match name {
        "rFonts" => {
            style.font_family = docx_font_family(start);
        }
        "sz" if stack
            .iter()
            .any(|item| matches!(item.as_str(), "rPr" | "rPrDefault")) =>
        {
            style.font_size = Some(parse_i64(attribute(start, b"val"), 22) as f64 / 2.0);
        }
        "b" => style.bold = Some(toggle_value(start)),
        "i" => style.italic = Some(toggle_value(start)),
        "color" => {
            if let Some(value) = attribute(start, b"val")
                && value != "auto"
            {
                style.color = Some(color_from_hex(&value, "#000000"));
            }
        }
        "jc" => {
            style.alignment = Some(match attribute(start, b"val").as_deref() {
                Some("center") => TextAnchor::Middle,
                Some("right" | "end") => TextAnchor::End,
                _ => TextAnchor::Start,
            });
        }
        "spacing" => {
            style.space_before = attribute(start, b"before")
                .and_then(|value| value.parse::<f64>().ok())
                .map(|value| value / TWIPS_PER_POINT);
            style.space_after = attribute(start, b"after")
                .and_then(|value| value.parse::<f64>().ok())
                .map(|value| value / TWIPS_PER_POINT);
            style.line_spacing = attribute(start, b"line")
                .and_then(|value| value.parse::<f64>().ok())
                .map(|value| (value / 240.0).max(0.8));
        }
        "ind" => {
            style.left_indent = attribute(start, b"left")
                .or_else(|| attribute(start, b"start"))
                .and_then(|value| value.parse::<f64>().ok())
                .map(|value| value / TWIPS_PER_POINT);
            style.right_indent = attribute(start, b"right")
                .or_else(|| attribute(start, b"end"))
                .and_then(|value| value.parse::<f64>().ok())
                .map(|value| value / TWIPS_PER_POINT);
            style.first_line_indent = attribute(start, b"firstLine")
                .and_then(|value| value.parse::<f64>().ok())
                .map(|value| value / TWIPS_PER_POINT);
        }
        _ => {}
    }
}

#[derive(Clone, Debug, Default)]
struct Paragraph {
    runs: Vec<TextRun>,
    style: ParagraphStyle,
    page_break_before: bool,
    images: Vec<InlineImage>,
    numbering_id: String,
    numbering_level: usize,
    text_boxes: Vec<FloatingTextBox>,
}

#[derive(Clone, Debug)]
struct InlineImage {
    href: String,
    width: f64,
    height: f64,
    alt_text: String,
    floating: bool,
    x: f64,
    y: f64,
    horizontal_relative: String,
    vertical_relative: String,
}

#[derive(Clone, Debug)]
struct FloatingTextBox {
    paragraphs: Vec<Paragraph>,
    width: f64,
    height: f64,
    x: f64,
    y: f64,
    horizontal_relative: String,
    vertical_relative: String,
    fill: Paint,
    stroke: Stroke,
    alt_text: String,
    floating: bool,
}

impl Default for FloatingTextBox {
    fn default() -> Self {
        Self {
            paragraphs: Vec::new(),
            width: 144.0,
            height: 72.0,
            x: 0.0,
            y: 0.0,
            horizontal_relative: "paragraph".into(),
            vertical_relative: "paragraph".into(),
            fill: Paint::None,
            stroke: Stroke {
                paint: Paint::solid("#808080"),
                width: 0.75,
                miter_limit: 10.0,
                ..Stroke::default()
            },
            alt_text: String::new(),
            floating: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Table {
    rows: Vec<TableRow>,
    grid_widths: Vec<f64>,
}

#[derive(Clone, Debug, Default)]
struct TableRow {
    cells: Vec<TableCell>,
}

#[derive(Clone, Debug, Default)]
struct TableCell {
    paragraphs: Vec<Paragraph>,
    width: Option<f64>,
    fill: Option<String>,
    grid_span: usize,
}

#[derive(Clone, Debug)]
enum Block {
    Paragraph(Box<Paragraph>),
    Table(Table),
}

#[derive(Clone, Debug)]
struct DocumentModel {
    page: PageSetup,
    blocks: Vec<Block>,
    section_ranges: Vec<SectionRange>,
    stories: HashMap<String, Vec<Block>>,
    warnings: Vec<String>,
}

#[derive(Clone, Debug)]
struct SectionRange {
    end_block: usize,
    setup: PageSetup,
    stories: StoryReferences,
}

#[derive(Clone, Debug, Default)]
struct StoryReferences {
    header_default: Option<String>,
    header_first: Option<String>,
    header_even: Option<String>,
    footer_default: Option<String>,
    footer_first: Option<String>,
    footer_even: Option<String>,
    title_page: bool,
}

impl StoryReferences {
    fn inherit_references(&mut self, previous: &Self) {
        macro_rules! inherit {
            ($field:ident) => {
                if self.$field.is_none() {
                    self.$field.clone_from(&previous.$field);
                }
            };
        }
        inherit!(header_default);
        inherit!(header_first);
        inherit!(header_even);
        inherit!(footer_default);
        inherit!(footer_first);
        inherit!(footer_even);
    }

    fn ids(&self) -> impl Iterator<Item = (&str, &'static str)> {
        [
            (self.header_default.as_deref(), "header"),
            (self.header_first.as_deref(), "header"),
            (self.header_even.as_deref(), "header"),
            (self.footer_default.as_deref(), "footer"),
            (self.footer_first.as_deref(), "footer"),
            (self.footer_even.as_deref(), "footer"),
        ]
        .into_iter()
        .filter_map(|(id, kind)| id.map(|id| (id, kind)))
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_document(
    xml: &[u8],
    styles: &Styles,
    numbering: &Numbering,
    relationships: &Relationships,
    document_part: &str,
    package: &mut ZipPackage<std::fs::File>,
    max_events: usize,
) -> Result<DocumentModel> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut page = PageSetup::default();
    let mut blocks = Vec::new();
    let mut section_ranges = Vec::<SectionRange>::new();
    let mut section_stories = StoryReferences::default();
    let mut section_after_paragraph = None::<(PageSetup, StoryReferences)>;
    let mut paragraph = None::<Paragraph>;
    let mut run = None::<TextRun>;
    let mut direct_paragraph_style = ParagraphStyle::default();
    let mut style_id = String::new();
    let mut text = String::new();
    let mut table = None::<Table>;
    let mut row = None::<TableRow>;
    let mut cell = None::<TableCell>;
    let mut image_relationship_id = String::new();
    let mut image_width = 0.0;
    let mut image_height = 0.0;
    let mut image_alt_text = String::new();
    let mut image_floating = false;
    let mut image_x = 0.0;
    let mut image_y = 0.0;
    let mut image_horizontal_relative = String::new();
    let mut image_vertical_relative = String::new();
    let mut position_capture = None::<bool>;
    let mut position_text = String::new();
    let mut field_active = false;
    let mut field_separated = false;
    let mut field_instruction = String::new();
    let mut text_box = None::<FloatingTextBox>;
    let mut text_box_has_content = false;
    let mut paragraph_context_stack =
        Vec::<(Option<Paragraph>, Option<TextRun>, ParagraphStyle, String)>::new();
    let mut alternate_choices = Vec::<bool>::new();
    let mut warnings = Vec::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "document.xml exceeds {max_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if stack
                    .iter()
                    .any(|item| matches!(item.as_str(), "oMath" | "oMathPara"))
                    && matches!(
                        name.as_str(),
                        "acc"
                            | "bar"
                            | "box"
                            | "eqArr"
                            | "func"
                            | "groupChr"
                            | "limLow"
                            | "limUpp"
                            | "m"
                            | "nary"
                    )
                {
                    warnings.push(format!(
                        "DOCX OMML element {name} is rendered as linear text"
                    ));
                }
                apply_section_story_property(&start, &name, &stack, &mut section_stories);
                if name == "AlternateContent" {
                    alternate_choices.push(false);
                } else if name == "Choice"
                    && attribute(&start, b"Requires").is_some_and(|value| {
                        value
                            .split_whitespace()
                            .any(|requirement| requirement == "wps")
                    })
                    && let Some(choice) = alternate_choices.last_mut()
                {
                    *choice = true;
                }
                if alternate_choices.last().copied().unwrap_or(false)
                    && (name == "Fallback" || stack.iter().any(|item| item == "Fallback"))
                {
                    stack.push(name);
                    buffer.clear();
                    continue;
                }
                match name.as_str() {
                    "tbl" => table = Some(Table::default()),
                    "tr" => row = Some(TableRow::default()),
                    "tc" => cell = Some(TableCell::default()),
                    "p" => {
                        paragraph = Some(Paragraph::default());
                        direct_paragraph_style = ParagraphStyle::default();
                        style_id.clear();
                    }
                    "r" => {
                        let mut math_run =
                            paragraph_text_run(styles, &style_id, &direct_paragraph_style);
                        if stack.iter().any(|item| item == "sub") {
                            apply_docx_vertical_alignment(&mut math_run, "subscript");
                        } else if stack.iter().any(|item| item == "sup") {
                            apply_docx_vertical_alignment(&mut math_run, "superscript");
                        }
                        run = Some(math_run);
                    }
                    "rad" if stack.iter().any(|item| item == "oMath") => {
                        push_docx_math_marker(
                            &mut paragraph,
                            styles,
                            &style_id,
                            &direct_paragraph_style,
                            "√(",
                        );
                    }
                    "d" if stack.iter().any(|item| item == "oMath") => {
                        push_docx_math_marker(
                            &mut paragraph,
                            styles,
                            &style_id,
                            &direct_paragraph_style,
                            "(",
                        );
                    }
                    "num" if stack.iter().any(|item| item == "f") => {
                        push_docx_math_marker(
                            &mut paragraph,
                            styles,
                            &style_id,
                            &direct_paragraph_style,
                            "(",
                        );
                    }
                    "anchor" => image_floating = true,
                    "inline" => image_floating = false,
                    "positionH" => {
                        image_horizontal_relative =
                            attribute(&start, b"relativeFrom").unwrap_or_default();
                    }
                    "positionV" => {
                        image_vertical_relative =
                            attribute(&start, b"relativeFrom").unwrap_or_default();
                    }
                    "wsp" => {
                        text_box.get_or_insert_with(FloatingTextBox::default);
                    }
                    "shape" if stack.iter().any(|item| item == "pict") => {
                        let mut builder = text_box.take().unwrap_or_default();
                        apply_vml_text_box_shape(&start, &mut builder);
                        text_box = Some(builder);
                    }
                    "txbxContent" => {
                        let builder = text_box.get_or_insert_with(FloatingTextBox::default);
                        if image_width > 0.0 {
                            builder.width = image_width;
                        }
                        if image_height > 0.0 {
                            builder.height = image_height;
                        }
                        builder.x = image_x;
                        builder.y = image_y;
                        if !image_horizontal_relative.is_empty() {
                            builder
                                .horizontal_relative
                                .clone_from(&image_horizontal_relative);
                        }
                        if !image_vertical_relative.is_empty() {
                            builder
                                .vertical_relative
                                .clone_from(&image_vertical_relative);
                        }
                        builder.alt_text.clone_from(&image_alt_text);
                        builder.floating = image_floating;
                        paragraph_context_stack.push((
                            paragraph.take(),
                            run.take(),
                            std::mem::take(&mut direct_paragraph_style),
                            std::mem::take(&mut style_id),
                        ));
                        text_box_has_content = true;
                    }
                    "srgbClr" if text_box.is_some() && stack.iter().any(|item| item == "spPr") => {
                        let color = color_from_hex(
                            &attribute(&start, b"val").unwrap_or_default(),
                            "#000000",
                        );
                        if let Some(text_box) = text_box.as_mut() {
                            if stack.iter().any(|item| item == "ln") {
                                text_box.stroke.paint = Paint::solid(color);
                            } else {
                                text_box.fill = Paint::solid(color);
                            }
                        }
                    }
                    "noFill" if text_box.is_some() && stack.iter().any(|item| item == "spPr") => {
                        if let Some(text_box) = text_box.as_mut() {
                            if stack.iter().any(|item| item == "ln") {
                                text_box.stroke.paint = Paint::None;
                            } else {
                                text_box.fill = Paint::None;
                            }
                        }
                    }
                    "ln" if text_box.is_some() && stack.iter().any(|item| item == "spPr") => {
                        if let Some(text_box) = text_box.as_mut() {
                            text_box.stroke.width =
                                parse_f64(attribute(&start, b"w"), 9_525.0) / EMU_PER_POINT;
                        }
                    }
                    "posOffset" => {
                        position_capture = Some(stack.iter().any(|item| item == "positionH"));
                        position_text.clear();
                    }
                    "rStyle" => {
                        if let (Some(run), Some(style_id)) =
                            (run.as_mut(), attribute(&start, b"val"))
                        {
                            apply_run_style(run, &styles.resolve(&style_id));
                        }
                    }
                    "t" => text.clear(),
                    "br" => append_document_break(&start, &mut run, styles),
                    "instrText" => field_instruction.clear(),
                    "fldChar" => apply_field_character(
                        &start,
                        &mut field_active,
                        &mut field_separated,
                        &mut field_instruction,
                        &mut run,
                        styles,
                    ),
                    "fldSimple" => {
                        let instruction = attribute(&start, b"instr").unwrap_or_default();
                        if instruction.split_whitespace().any(|item| item == "PAGE") {
                            run.get_or_insert_with(|| default_text_run(styles))
                                .text
                                .push_str(PAGE_FIELD_MARKER);
                            field_active = true;
                            field_separated = true;
                        }
                    }
                    "footnoteReference" | "endnoteReference" | "commentReference" => {
                        append_note_reference(&start, &name, &mut run, styles)
                    }
                    _ => apply_document_property(
                        &start,
                        &name,
                        &stack,
                        &mut page,
                        &mut paragraph,
                        &mut run,
                        &mut direct_paragraph_style,
                        &mut style_id,
                        &mut table,
                        &mut cell,
                        &mut image_relationship_id,
                        &mut image_width,
                        &mut image_height,
                        &mut image_alt_text,
                    ),
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                apply_section_story_property(&start, &name, &stack, &mut section_stories);
                if alternate_choices.last().copied().unwrap_or(false)
                    && stack.iter().any(|item| item == "Fallback")
                {
                    buffer.clear();
                    continue;
                }
                match name.as_str() {
                    "srgbClr" | "noFill" | "ln"
                        if text_box.is_some() && stack.iter().any(|item| item == "spPr") =>
                    {
                        apply_text_box_shape_property(&start, &name, &stack, text_box.as_mut());
                    }
                    "tab" => {
                        run.get_or_insert_with(|| default_text_run(styles))
                            .text
                            .push('\t');
                    }
                    "br" => append_document_break(&start, &mut run, styles),
                    "fldChar" => apply_field_character(
                        &start,
                        &mut field_active,
                        &mut field_separated,
                        &mut field_instruction,
                        &mut run,
                        styles,
                    ),
                    "fldSimple" => {
                        let instruction = attribute(&start, b"instr").unwrap_or_default();
                        if instruction.split_whitespace().any(|item| item == "PAGE") {
                            run.get_or_insert_with(|| default_text_run(styles))
                                .text
                                .push_str(PAGE_FIELD_MARKER);
                        }
                    }
                    "rStyle" => {
                        if let (Some(run), Some(style_id)) =
                            (run.as_mut(), attribute(&start, b"val"))
                        {
                            apply_run_style(run, &styles.resolve(&style_id));
                        }
                    }
                    "footnoteReference" | "endnoteReference" | "commentReference" => {
                        append_note_reference(&start, &name, &mut run, styles)
                    }
                    _ => apply_document_property(
                        &start,
                        &name,
                        &stack,
                        &mut page,
                        &mut paragraph,
                        &mut run,
                        &mut direct_paragraph_style,
                        &mut style_id,
                        &mut table,
                        &mut cell,
                        &mut image_relationship_id,
                        &mut image_width,
                        &mut image_height,
                        &mut image_alt_text,
                    ),
                }
            }
            Event::Text(value) if stack.last().is_some_and(|name| name == "instrText") => {
                field_instruction.push_str(&value.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid DOCX field instruction: {error}"))
                })?);
            }
            Event::Text(value) if stack.last().is_some_and(|name| name == "posOffset") => {
                position_text.push_str(&value.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid DOCX drawing position: {error}"))
                })?);
            }
            Event::Text(value)
                if stack.last().is_some_and(|name| name == "t")
                    && !(field_active && field_separated) =>
            {
                text.push_str(
                    &value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid DOCX text: {error}"))
                    })?,
                );
            }
            Event::End(end) => {
                if alternate_choices.last().copied().unwrap_or(false)
                    && stack.iter().any(|item| item == "Fallback")
                {
                    stack.pop();
                    buffer.clear();
                    continue;
                }
                match local_name(end.name().as_ref()) {
                    b"posOffset" => {
                        let value = position_text.parse::<f64>().unwrap_or(0.0) / EMU_PER_POINT;
                        if position_capture.take().unwrap_or(false) {
                            image_x = value;
                        } else {
                            image_y = value;
                        }
                    }
                    b"t" => {
                        if !(field_active && field_separated) {
                            run.get_or_insert_with(|| default_text_run(styles))
                                .text
                                .push_str(&text);
                        }
                        text.clear();
                    }
                    b"fldSimple" => {
                        field_active = false;
                        field_separated = false;
                    }
                    b"txbxContent" => {
                        if let Some((saved_paragraph, saved_run, saved_style, saved_style_id)) =
                            paragraph_context_stack.pop()
                        {
                            paragraph = saved_paragraph;
                            run = saved_run;
                            direct_paragraph_style = saved_style;
                            style_id = saved_style_id;
                        }
                        if text_box_has_content && let Some(text_box) = text_box.take() {
                            paragraph
                                .get_or_insert_with(Paragraph::default)
                                .text_boxes
                                .push(text_box);
                        }
                        text_box_has_content = false;
                    }
                    b"r" => {
                        if let (Some(paragraph), Some(run)) = (paragraph.as_mut(), run.take()) {
                            paragraph.runs.push(run);
                        }
                    }
                    b"drawing" | b"pict" => {
                        if !image_relationship_id.is_empty() {
                            if let Some(part) =
                                relationships.target(&image_relationship_id, document_part)
                            {
                                if let Some(bytes) = package.read_optional(&part)? {
                                    let href = format!(
                                        "data:{};base64,{}",
                                        mime_type(&part),
                                        base64::engine::general_purpose::STANDARD.encode(bytes)
                                    );
                                    paragraph
                                        .get_or_insert_with(Paragraph::default)
                                        .images
                                        .push(InlineImage {
                                            href,
                                            width: image_width.max(1.0),
                                            height: image_height.max(1.0),
                                            alt_text: image_alt_text.clone(),
                                            floating: image_floating,
                                            x: image_x,
                                            y: image_y,
                                            horizontal_relative: image_horizontal_relative.clone(),
                                            vertical_relative: image_vertical_relative.clone(),
                                        });
                                }
                            } else {
                                warnings.push(format!(
                                    "image relationship {image_relationship_id} is missing"
                                ));
                            }
                        }
                        if paragraph_context_stack.is_empty() {
                            if text_box_has_content && let Some(text_box) = text_box.take() {
                                paragraph
                                    .get_or_insert_with(Paragraph::default)
                                    .text_boxes
                                    .push(text_box);
                            }
                            text_box_has_content = false;
                            text_box = None;
                        }
                        image_relationship_id.clear();
                        image_width = 0.0;
                        image_height = 0.0;
                        image_alt_text.clear();
                        image_floating = false;
                        image_x = 0.0;
                        image_y = 0.0;
                        image_horizontal_relative.clear();
                        image_vertical_relative.clear();
                    }
                    b"p" => {
                        if let Some(mut finished) = paragraph.take() {
                            if let Some(run) = run.take() {
                                finished.runs.push(run);
                            }
                            let mut resolved = styles.resolve(&style_id);
                            resolved.merge(&direct_paragraph_style);
                            if let Some(level) =
                                numbering.level(&finished.numbering_id, finished.numbering_level)
                            {
                                if resolved.left_indent.is_none() {
                                    resolved.left_indent = level.left_indent;
                                }
                                if resolved.first_line_indent.is_none() {
                                    resolved.first_line_indent =
                                        level.hanging_indent.map(|value| -value);
                                }
                            }
                            finished.style = resolved;
                            if !paragraph_context_stack.is_empty() {
                                if let Some(text_box) = text_box.as_mut() {
                                    text_box.paragraphs.push(finished);
                                }
                            } else if let Some(cell) = cell.as_mut() {
                                cell.paragraphs.push(finished);
                            } else {
                                blocks.push(Block::Paragraph(Box::new(finished)));
                            }
                        }
                        if let Some((setup, stories)) = section_after_paragraph.take() {
                            push_section_range(&mut section_ranges, blocks.len(), setup, stories);
                        }
                    }
                    b"num" if stack.iter().any(|item| item == "f") => {
                        push_docx_math_marker(
                            &mut paragraph,
                            styles,
                            &style_id,
                            &direct_paragraph_style,
                            ")/(",
                        );
                    }
                    b"den" if stack.iter().any(|item| item == "f") => {
                        push_docx_math_marker(
                            &mut paragraph,
                            styles,
                            &style_id,
                            &direct_paragraph_style,
                            ")",
                        );
                    }
                    b"rad" if stack.iter().any(|item| item == "oMath") => {
                        push_docx_math_marker(
                            &mut paragraph,
                            styles,
                            &style_id,
                            &direct_paragraph_style,
                            ")",
                        );
                    }
                    b"d" if stack.iter().any(|item| item == "oMath") => {
                        push_docx_math_marker(
                            &mut paragraph,
                            styles,
                            &style_id,
                            &direct_paragraph_style,
                            ")",
                        );
                    }
                    b"tc" => {
                        if let (Some(row), Some(cell)) = (row.as_mut(), cell.take()) {
                            row.cells.push(cell);
                        }
                    }
                    b"tr" => {
                        if let (Some(table), Some(row)) = (table.as_mut(), row.take()) {
                            let contains_page_break = row.cells.iter().any(|cell| {
                                cell.paragraphs.iter().any(|paragraph| {
                                    paragraph
                                        .runs
                                        .iter()
                                        .any(|run| run.text.contains(PAGE_BREAK_MARKER))
                                })
                            });
                            if contains_page_break {
                                warnings.push(
                                    "DOCX page break inside a table cell was hidden; table pagination may differ from Word"
                                        .into(),
                                );
                            }
                            table.rows.push(row);
                        }
                    }
                    b"tbl" => {
                        if let Some(table) = table.take() {
                            blocks.push(Block::Table(table));
                        }
                    }
                    b"sectPr" => {
                        let stories = std::mem::take(&mut section_stories);
                        if stack.iter().any(|item| item == "p") {
                            section_after_paragraph = Some((page.clone(), stories));
                        } else {
                            push_section_range(
                                &mut section_ranges,
                                blocks.len(),
                                page.clone(),
                                stories,
                            );
                        }
                    }
                    _ => {}
                }
                if local_name(end.name().as_ref()) == b"AlternateContent" {
                    alternate_choices.pop();
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if section_ranges.last().map_or(0, |section| section.end_block) < blocks.len()
        || section_ranges.is_empty()
    {
        push_section_range(
            &mut section_ranges,
            blocks.len(),
            page.clone(),
            StoryReferences::default(),
        );
    }
    let initial_page = section_ranges
        .first()
        .map(|section| section.setup.clone())
        .unwrap_or_else(|| page.clone());
    Ok(DocumentModel {
        page: initial_page,
        blocks,
        section_ranges,
        stories: HashMap::new(),
        warnings,
    })
}

fn push_section_range(
    ranges: &mut Vec<SectionRange>,
    end_block: usize,
    setup: PageSetup,
    mut stories: StoryReferences,
) {
    if let Some(previous) = ranges.last() {
        stories.inherit_references(&previous.stories);
    }
    if let Some(last) = ranges.last_mut()
        && last.end_block == end_block
    {
        last.setup = setup;
        last.stories = stories;
        return;
    }
    ranges.push(SectionRange {
        end_block,
        setup,
        stories,
    });
}

fn apply_section_story_property(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    stories: &mut StoryReferences,
) {
    if !stack.iter().any(|item| item == "sectPr") {
        return;
    }
    match name {
        "headerReference" | "footerReference" => {
            let Some(relationship_id) = qualified_attribute(start, b"r:id") else {
                return;
            };
            let reference_type = attribute(start, b"type").unwrap_or_else(|| "default".into());
            match (name, reference_type.as_str()) {
                ("headerReference", "first") => stories.header_first = Some(relationship_id),
                ("headerReference", "even") => stories.header_even = Some(relationship_id),
                ("headerReference", _) => stories.header_default = Some(relationship_id),
                ("footerReference", "first") => stories.footer_first = Some(relationship_id),
                ("footerReference", "even") => stories.footer_even = Some(relationship_id),
                ("footerReference", _) => stories.footer_default = Some(relationship_id),
                _ => {}
            }
        }
        "titlePg" => stories.title_page = toggle_value(start),
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_document_property(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    page: &mut PageSetup,
    paragraph: &mut Option<Paragraph>,
    run: &mut Option<TextRun>,
    paragraph_style: &mut ParagraphStyle,
    style_id: &mut String,
    table: &mut Option<Table>,
    cell: &mut Option<TableCell>,
    image_relationship_id: &mut String,
    image_width: &mut f64,
    image_height: &mut f64,
    image_alt_text: &mut String,
) {
    match name {
        "pgSz" => {
            page.width = parse_i64(attribute(start, b"w"), 12_240) as f64 / TWIPS_PER_POINT;
            page.height = parse_i64(attribute(start, b"h"), 15_840) as f64 / TWIPS_PER_POINT;
            if attribute(start, b"orient").as_deref() == Some("landscape")
                && page.width < page.height
            {
                std::mem::swap(&mut page.width, &mut page.height);
            }
        }
        "pgMar" => {
            page.margin_top = parse_i64(attribute(start, b"top"), 1_440) as f64 / TWIPS_PER_POINT;
            page.margin_right =
                parse_i64(attribute(start, b"right"), 1_440) as f64 / TWIPS_PER_POINT;
            page.margin_bottom =
                parse_i64(attribute(start, b"bottom"), 1_440) as f64 / TWIPS_PER_POINT;
            page.margin_left = parse_i64(attribute(start, b"left"), 1_440) as f64 / TWIPS_PER_POINT;
        }
        "pStyle" => *style_id = attribute(start, b"val").unwrap_or_default(),
        "numId" if stack.iter().any(|item| item == "numPr") => {
            if let Some(paragraph) = paragraph.as_mut() {
                paragraph.numbering_id = attribute(start, b"val").unwrap_or_default();
            }
        }
        "ilvl" if stack.iter().any(|item| item == "numPr") => {
            if let Some(paragraph) = paragraph.as_mut() {
                paragraph.numbering_level = parse_i64(attribute(start, b"val"), 0).max(0) as usize;
            }
        }
        "pageBreakBefore" => {
            if let Some(paragraph) = paragraph.as_mut() {
                paragraph.page_break_before = toggle_value(start);
            }
        }
        "rFonts" => {
            if let Some(run) = run.as_mut() {
                apply_docx_run_fonts(run, start);
            }
        }
        "sz" if stack.iter().any(|item| item == "rPr") => {
            if let Some(run) = run.as_mut() {
                run.font_size = parse_i64(attribute(start, b"val"), 22) as f64 / 2.0;
            }
        }
        "b" if stack.iter().any(|item| item == "rPr") => {
            if let Some(run) = run.as_mut() {
                run.bold = toggle_value(start);
            }
        }
        "i" if stack.iter().any(|item| item == "rPr") => {
            if let Some(run) = run.as_mut() {
                run.italic = toggle_value(start);
            }
        }
        "vertAlign" if stack.iter().any(|item| item == "rPr") => {
            if let Some(run) = run.as_mut() {
                apply_docx_vertical_alignment(
                    run,
                    attribute(start, b"val").as_deref().unwrap_or_default(),
                );
            }
        }
        "color" if stack.iter().any(|item| item == "rPr") => {
            if let (Some(run), Some(value)) = (run.as_mut(), attribute(start, b"val"))
                && value != "auto"
            {
                run.fill = Paint::solid(color_from_hex(&value, "#000000"));
            }
        }
        "jc" => {
            paragraph_style.alignment = Some(match attribute(start, b"val").as_deref() {
                Some("center") => TextAnchor::Middle,
                Some("right" | "end") => TextAnchor::End,
                _ => TextAnchor::Start,
            });
        }
        "spacing" => apply_style_property(start, name, stack, paragraph_style),
        "ind" => apply_style_property(start, name, stack, paragraph_style),
        "gridCol" => {
            if let Some(table) = table.as_mut() {
                table
                    .grid_widths
                    .push(parse_i64(attribute(start, b"w"), 0).max(0) as f64 / TWIPS_PER_POINT);
            }
        }
        "tcW" => {
            if let Some(cell) = cell.as_mut() {
                cell.width =
                    Some(parse_i64(attribute(start, b"w"), 0).max(0) as f64 / TWIPS_PER_POINT);
            }
        }
        "gridSpan" => {
            if let Some(cell) = cell.as_mut() {
                cell.grid_span = parse_i64(attribute(start, b"val"), 1).max(1) as usize;
            }
        }
        "shd" => {
            if let (Some(cell), Some(fill)) = (cell.as_mut(), attribute(start, b"fill"))
                && fill != "auto"
            {
                cell.fill = Some(color_from_hex(&fill, "#FFFFFF"));
            }
        }
        "blip" => *image_relationship_id = attribute(start, b"embed").unwrap_or_default(),
        "extent" => {
            *image_width = parse_f64(attribute(start, b"cx"), 0.0) / EMU_PER_POINT;
            *image_height = parse_f64(attribute(start, b"cy"), 0.0) / EMU_PER_POINT;
        }
        "docPr" => {
            *image_alt_text = attribute(start, b"descr")
                .or_else(|| attribute(start, b"name"))
                .unwrap_or_default();
        }
        _ => {}
    }
}

fn default_text_run(styles: &Styles) -> TextRun {
    TextRun {
        font_family: styles
            .default
            .font_family
            .clone()
            .unwrap_or_else(|| "Calibri, Arial, sans-serif".into()),
        font_size: styles.default.font_size.unwrap_or(11.0),
        bold: styles.default.bold.unwrap_or(false),
        italic: styles.default.italic.unwrap_or(false),
        fill: Paint::solid(styles.default.color.as_deref().unwrap_or("#000000")),
        ..TextRun::default()
    }
}

fn paragraph_text_run(
    styles: &Styles,
    style_id: &str,
    direct_paragraph_style: &ParagraphStyle,
) -> TextRun {
    let mut run = default_text_run(styles);
    let mut style = styles.resolve(style_id);
    style.merge(direct_paragraph_style);
    apply_run_style(&mut run, &style);
    run
}

fn push_docx_math_marker(
    paragraph: &mut Option<Paragraph>,
    styles: &Styles,
    style_id: &str,
    direct_paragraph_style: &ParagraphStyle,
    text: &str,
) {
    let mut marker = paragraph_text_run(styles, style_id, direct_paragraph_style);
    marker.text = text.into();
    paragraph
        .get_or_insert_with(Paragraph::default)
        .runs
        .push(marker);
}

fn apply_run_style(run: &mut TextRun, style: &ParagraphStyle) {
    if let Some(font_family) = &style.font_family {
        run.font_family.clone_from(font_family);
    }
    if let Some(font_size) = style.font_size {
        run.font_size = font_size;
    }
    if let Some(bold) = style.bold {
        run.bold = bold;
    }
    if let Some(italic) = style.italic {
        run.italic = italic;
    }
    if let Some(color) = &style.color {
        run.fill = Paint::solid(color);
    }
}

fn apply_docx_vertical_alignment(run: &mut TextRun, alignment: &str) {
    let original_size = run.font_size;
    match alignment {
        "superscript" => {
            run.font_size *= 0.75;
            run.baseline_shift += original_size * 0.35;
        }
        "subscript" => {
            run.font_size *= 0.75;
            run.baseline_shift -= original_size * 0.2;
        }
        _ => {}
    }
}

fn toggle_value(start: &quick_xml::events::BytesStart<'_>) -> bool {
    !matches!(
        attribute(start, b"val").as_deref(),
        Some("0" | "false" | "off")
    )
}

fn apply_field_character(
    start: &quick_xml::events::BytesStart<'_>,
    active: &mut bool,
    separated: &mut bool,
    instruction: &mut String,
    run: &mut Option<TextRun>,
    styles: &Styles,
) {
    match attribute(start, b"fldCharType")
        .or_else(|| attribute(start, b"type"))
        .as_deref()
    {
        Some("begin") => {
            *active = true;
            *separated = false;
            instruction.clear();
        }
        Some("separate") => {
            if *active && instruction.split_whitespace().any(|item| item == "PAGE") {
                run.get_or_insert_with(|| default_text_run(styles))
                    .text
                    .push_str(PAGE_FIELD_MARKER);
            }
            *separated = true;
        }
        Some("end") => {
            *active = false;
            *separated = false;
            instruction.clear();
        }
        _ => {}
    }
}

fn append_note_reference(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    run: &mut Option<TextRun>,
    styles: &Styles,
) {
    let id = attribute(start, b"id").unwrap_or_default();
    if id.parse::<i64>().is_ok_and(|value| value >= 0) {
        let kind = match name {
            "endnoteReference" => 'E',
            "commentReference" => 'C',
            _ => 'F',
        };
        run.get_or_insert_with(|| default_text_run(styles))
            .text
            .push_str(&format!("{NOTE_FIELD_START}{kind}{id}{NOTE_FIELD_END}"));
    }
}

fn apply_text_box_shape_property(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    text_box: Option<&mut FloatingTextBox>,
) {
    let Some(text_box) = text_box else {
        return;
    };
    match name {
        "srgbClr" => {
            let color = color_from_hex(&attribute(start, b"val").unwrap_or_default(), "#000000");
            if stack.iter().any(|item| item == "ln") {
                text_box.stroke.paint = Paint::solid(color);
            } else {
                text_box.fill = Paint::solid(color);
            }
        }
        "noFill" => {
            if stack.iter().any(|item| item == "ln") {
                text_box.stroke.paint = Paint::None;
            } else {
                text_box.fill = Paint::None;
            }
        }
        "ln" => {
            text_box.stroke.width = parse_f64(attribute(start, b"w"), 9_525.0) / EMU_PER_POINT;
        }
        _ => {}
    }
}

fn apply_vml_text_box_shape(
    start: &quick_xml::events::BytesStart<'_>,
    text_box: &mut FloatingTextBox,
) {
    text_box.floating = true;
    if let Some(style) = attribute(start, b"style") {
        for declaration in style.split(';') {
            let Some((name, value)) = declaration.split_once(':') else {
                continue;
            };
            let Some(points) = vml_length_points(value.trim()) else {
                continue;
            };
            match name.trim() {
                "margin-left" | "left" => text_box.x = points,
                "margin-top" | "top" => text_box.y = points,
                "width" => text_box.width = points.max(1.0),
                "height" => text_box.height = points.max(1.0),
                _ => {}
            }
        }
    }
    if attribute(start, b"filled").as_deref() == Some("f") {
        text_box.fill = Paint::None;
    } else if let Some(fill) = attribute(start, b"fillcolor") {
        text_box.fill = Paint::solid(color_from_hex(fill.trim_start_matches('#'), "#FFFFFF"));
    }
    if attribute(start, b"stroked").as_deref() == Some("f") {
        text_box.stroke.paint = Paint::None;
    } else if let Some(stroke) = attribute(start, b"strokecolor") {
        text_box.stroke.paint =
            Paint::solid(color_from_hex(stroke.trim_start_matches('#'), "#000000"));
    }
    if let Some(weight) =
        attribute(start, b"strokeweight").and_then(|value| vml_length_points(&value))
    {
        text_box.stroke.width = weight;
    }
}

fn append_document_break(
    start: &quick_xml::events::BytesStart<'_>,
    run: &mut Option<TextRun>,
    styles: &Styles,
) {
    let text = &mut run.get_or_insert_with(|| default_text_run(styles)).text;
    if attribute(start, b"type").as_deref() == Some("page") {
        text.push_str(PAGE_BREAK_MARKER);
    } else {
        text.push('\n');
    }
}

fn vml_length_points(value: &str) -> Option<f64> {
    let split = value.find(|character: char| {
        !(character.is_ascii_digit() || matches!(character, '.' | '-' | '+'))
    });
    let (number, unit) = split.map_or((value, "pt"), |index| value.split_at(index));
    let number = number.trim().parse::<f64>().ok()?;
    Some(match unit.trim().to_ascii_lowercase().as_str() {
        "in" => number * 72.0,
        "cm" => number * 72.0 / 2.54,
        "mm" => number * 72.0 / 25.4,
        "px" => number * 0.75,
        _ => number,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NoteReference {
    kind: char,
    id: String,
}

struct Layout<'a> {
    setup: PageSetup,
    numbering: &'a Numbering,
    footnotes: &'a HashMap<String, Vec<Paragraph>>,
    endnotes: &'a HashMap<String, Vec<Paragraph>>,
    comments: &'a HashMap<String, Vec<Paragraph>>,
    stories: &'a HashMap<String, Vec<Block>>,
    story_references: StoryReferences,
    section_first_page: usize,
    sink: &'a mut dyn PageConsumer,
    max_pages: usize,
    page: Page,
    y: f64,
    warnings: Vec<String>,
    node_counter: usize,
    numbering_counters: HashMap<(String, usize), usize>,
    pending_notes: Vec<NoteReference>,
}

fn render_document(
    document: DocumentModel,
    numbering: &Numbering,
    footnotes: &HashMap<String, Vec<Paragraph>>,
    endnotes: &HashMap<String, Vec<Paragraph>>,
    comments: &HashMap<String, Vec<Paragraph>>,
    sink: &mut dyn PageConsumer,
    max_pages: usize,
) -> Result<Vec<String>> {
    let initial_story_references = document
        .section_ranges
        .first()
        .map(|section| section.stories.clone())
        .unwrap_or_default();
    let mut layout = Layout {
        setup: document.page.clone(),
        numbering,
        footnotes,
        endnotes,
        comments,
        stories: &document.stories,
        story_references: initial_story_references,
        section_first_page: 1,
        sink,
        max_pages,
        page: Page::new(1, document.page.width, document.page.height, "docx"),
        y: document.page.margin_top,
        warnings: document.warnings,
        node_counter: 0,
        numbering_counters: HashMap::new(),
        pending_notes: Vec::new(),
    };
    layout.page.description = "DOCX page approximated with deterministic Rust layout".into();
    layout.add_repeating_stories()?;
    let mut block_start = 0usize;
    for (section_index, section) in document.section_ranges.iter().enumerate() {
        if section_index > 0 {
            layout.switch_section(&section.setup, &section.stories)?;
        }
        let block_end = section
            .end_block
            .min(document.blocks.len())
            .max(block_start);
        for block in &document.blocks[block_start..block_end] {
            match block {
                Block::Paragraph(paragraph) => layout.paragraph(paragraph)?,
                Block::Table(table) => layout.table(table)?,
            }
        }
        block_start = block_end;
    }
    for block in &document.blocks[block_start..] {
        match block {
            Block::Paragraph(paragraph) => layout.paragraph(paragraph)?,
            Block::Table(table) => layout.table(table)?,
        }
    }
    layout.flush_page(false)?;
    Ok(deduplicate(layout.warnings))
}

impl Layout<'_> {
    fn add_repeating_stories(&mut self) -> Result<()> {
        let first_page = self.page.number == self.section_first_page;
        let even_page = self.page.number.is_multiple_of(2);
        let header_id = if first_page && self.story_references.title_page {
            self.story_references
                .header_first
                .as_ref()
                .or(self.story_references.header_default.as_ref())
        } else if even_page {
            self.story_references
                .header_even
                .as_ref()
                .or(self.story_references.header_default.as_ref())
        } else {
            self.story_references.header_default.as_ref()
        }
        .cloned();
        let footer_id = if first_page && self.story_references.title_page {
            self.story_references
                .footer_first
                .as_ref()
                .or(self.story_references.footer_default.as_ref())
        } else if even_page {
            self.story_references
                .footer_even
                .as_ref()
                .or(self.story_references.footer_default.as_ref())
        } else {
            self.story_references.footer_default.as_ref()
        }
        .cloned();
        let header = header_id
            .as_deref()
            .and_then(|id| self.stories.get(id))
            .map_or(&[][..], Vec::as_slice);
        let footer = footer_id
            .as_deref()
            .and_then(|id| self.stories.get(id))
            .map_or(&[][..], Vec::as_slice);
        self.add_story(header, false)?;
        self.add_story(footer, true)
    }

    fn add_story(&mut self, blocks: &[Block], footer: bool) -> Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        let available =
            (self.setup.width - self.setup.margin_left - self.setup.margin_right).max(12.0);
        let mut y = if footer {
            self.setup.height - self.setup.margin_bottom + 10.0
        } else {
            10.0
        };
        for block in blocks {
            match block {
                Block::Paragraph(paragraph) => {
                    let materialized = materialize_page_fields(&paragraph.runs, self.page.number);
                    let lines = wrap_runs(&materialized, available);
                    for runs in lines {
                        let font_size = runs.iter().map(|run| run.font_size).fold(10.0, f64::max);
                        y += font_size;
                        let anchor = paragraph.style.alignment.unwrap_or(TextAnchor::Start);
                        let x = match anchor {
                            TextAnchor::Start => self.setup.margin_left,
                            TextAnchor::Middle => self.setup.width / 2.0,
                            TextAnchor::End => self.setup.width - self.setup.margin_right,
                        };
                        self.node_counter += 1;
                        self.page.nodes.push(Node::Text {
                            id: format!(
                                "docx-{}-{}-{}",
                                if footer { "footer" } else { "header" },
                                self.page.number,
                                self.node_counter
                            ),
                            x,
                            y,
                            runs,
                            anchor,
                            transform: IDENTITY,
                            opacity: 1.0,
                            stroke: Stroke::default(),
                            clip_id: None,
                            meta: SourceMeta {
                                kind: if footer { "footer" } else { "header" }.into(),
                                semantic_role: "repeating-story".into(),
                                ..SourceMeta::default()
                            },
                        });
                        y += font_size * 0.15;
                    }
                    for image in &paragraph.images {
                        self.node_counter += 1;
                        self.page.nodes.push(Node::Image {
                            id: format!(
                                "docx-{}-image-{}-{}",
                                if footer { "footer" } else { "header" },
                                self.page.number,
                                self.node_counter
                            ),
                            href: image.href.clone(),
                            x: self.setup.margin_left,
                            y,
                            width: image.width.min(available),
                            height: image.height,
                            transform: IDENTITY,
                            opacity: 1.0,
                            clip_id: None,
                            meta: SourceMeta {
                                kind: if footer {
                                    "footer-image"
                                } else {
                                    "header-image"
                                }
                                .into(),
                                alt_text: image.alt_text.clone(),
                                ..SourceMeta::default()
                            },
                        });
                    }
                }
                Block::Table(table) => self.add_story_table(table, &mut y, footer),
            }
        }
        Ok(())
    }

    fn add_story_table(&mut self, table: &Table, y: &mut f64, footer: bool) {
        let available =
            (self.setup.width - self.setup.margin_left - self.setup.margin_right).max(12.0);
        let column_count = table
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.grid_span.max(1))
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0)
            .max(table.grid_widths.len());
        if column_count == 0 {
            return;
        }
        let widths = if table.grid_widths.len() >= column_count {
            let total = table.grid_widths.iter().take(column_count).sum::<f64>();
            if total > 0.0 {
                table
                    .grid_widths
                    .iter()
                    .take(column_count)
                    .map(|width| width / total * available)
                    .collect::<Vec<_>>()
            } else {
                vec![available / column_count as f64; column_count]
            }
        } else {
            vec![available / column_count as f64; column_count]
        };
        for (row_index, row) in table.rows.iter().enumerate() {
            let row_height = table_row_height(row, &widths).max(16.0);
            let mut x = self.setup.margin_left;
            let mut column_index = 0usize;
            for cell in &row.cells {
                if column_index >= widths.len() {
                    break;
                }
                let span = cell.grid_span.max(1).min(widths.len() - column_index);
                let width = widths[column_index..column_index + span]
                    .iter()
                    .sum::<f64>();
                self.node_counter += 1;
                self.page.nodes.push(Node::Path {
                    id: format!("docx-story-table-cell-{}", self.node_counter),
                    d: format!(
                        "M {} {} H {} V {} H {} Z",
                        fmt(x),
                        fmt(*y),
                        fmt(x + width),
                        fmt(*y + row_height),
                        fmt(x)
                    ),
                    fill_rule: "nonzero".into(),
                    fill: cell.fill.as_deref().map_or(Paint::None, Paint::solid),
                    stroke: Stroke {
                        paint: Paint::solid("#808080"),
                        width: 0.5,
                        miter_limit: 10.0,
                        ..Stroke::default()
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: if footer {
                            "footer-table-cell"
                        } else {
                            "header-table-cell"
                        }
                        .into(),
                        source_id: format!("R{}C{}", row_index + 1, column_index + 1),
                        semantic_role: "repeating-story-cell".into(),
                        ..SourceMeta::default()
                    },
                });
                let mut cell_y = *y + 2.0;
                for paragraph in &cell.paragraphs {
                    let materialized = materialize_page_fields(&paragraph.runs, self.page.number);
                    for runs in wrap_runs(&materialized, (width - 4.0).max(6.0)) {
                        let font_size = runs.iter().map(|run| run.font_size).fold(9.0, f64::max);
                        cell_y += font_size;
                        if cell_y > *y + row_height - 1.0 {
                            break;
                        }
                        self.node_counter += 1;
                        self.page.nodes.push(Node::Text {
                            id: format!("docx-story-table-text-{}", self.node_counter),
                            x: x + 2.0,
                            y: cell_y,
                            runs,
                            anchor: TextAnchor::Start,
                            transform: IDENTITY,
                            opacity: 1.0,
                            stroke: Stroke::default(),
                            clip_id: None,
                            meta: SourceMeta {
                                kind: if footer {
                                    "footer-table-text"
                                } else {
                                    "header-table-text"
                                }
                                .into(),
                                semantic_role: "repeating-story-cell".into(),
                                ..SourceMeta::default()
                            },
                        });
                        cell_y += 1.0;
                    }
                }
                x += width;
                column_index += span;
            }
            *y += row_height;
        }
    }

    fn paragraph(&mut self, paragraph: &Paragraph) -> Result<()> {
        if paragraph.page_break_before && !self.page.nodes.is_empty() {
            self.flush_page(true)?;
        }
        let style = &paragraph.style;
        self.y += style.space_before.unwrap_or(0.0);
        let left = self.setup.margin_left + style.left_indent.unwrap_or(0.0);
        let right = self.setup.margin_right + style.right_indent.unwrap_or(0.0);
        let available = (self.setup.width - left - right).max(12.0);
        for text_box in &paragraph.text_boxes {
            if !text_box.floating && self.y + text_box.height > self.bottom() {
                self.flush_page(true)?;
            }
            self.floating_text_box(text_box, self.y);
            if !text_box.floating {
                self.y += text_box.height + 4.0;
            }
        }
        for image in &paragraph.images {
            if !image.floating && self.y + image.height > self.bottom() {
                self.flush_page(true)?;
            }
            let displayed_width = image.width.min(available);
            let displayed_height = image.height * (displayed_width / image.width.max(1.0));
            let (image_x, image_y) = if image.floating {
                floating_image_position(image, &self.setup, self.y)
            } else {
                (left, self.y)
            };
            self.node_counter += 1;
            self.page.nodes.push(Node::Image {
                id: format!("docx-image-{}", self.node_counter),
                href: image.href.clone(),
                x: image_x,
                y: image_y,
                width: displayed_width,
                height: displayed_height,
                transform: IDENTITY,
                opacity: 1.0,
                clip_id: None,
                meta: SourceMeta {
                    kind: if image.floating {
                        "floating-image"
                    } else {
                        "image"
                    }
                    .into(),
                    alt_text: image.alt_text.clone(),
                    ..SourceMeta::default()
                },
            });
            if !image.floating {
                self.y += displayed_height + 4.0;
            }
        }
        let numbered_runs = self.numbered_runs(paragraph);
        let line_spacing = style.line_spacing.unwrap_or(1.2);
        let page_segments = split_runs_on_page_breaks(&numbered_runs);
        let mut paragraph_line_index = 0usize;
        for (segment_index, segment) in page_segments.iter().enumerate() {
            let materialized = self.materialize_fields(segment);
            for runs in wrap_runs(&materialized, available) {
                if runs.is_empty() {
                    continue;
                }
                let font_size = runs.iter().map(|run| run.font_size).fold(11.0, f64::max);
                let line_height = font_size * line_spacing;
                if self.y + line_height > self.bottom() {
                    self.flush_page(true)?;
                }
                self.y += font_size;
                let anchor = style.alignment.unwrap_or(TextAnchor::Start);
                let first_indent = if paragraph_line_index == 0 {
                    style.first_line_indent.unwrap_or(0.0)
                } else {
                    0.0
                };
                let x = match anchor {
                    TextAnchor::Start => left + first_indent,
                    TextAnchor::Middle => left + available / 2.0,
                    TextAnchor::End => left + available,
                };
                self.node_counter += 1;
                self.page.nodes.push(Node::Text {
                    id: format!("docx-text-{}", self.node_counter),
                    x,
                    y: self.y,
                    runs,
                    anchor,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "text".into(),
                        semantic_role: "paragraph".into(),
                        ..SourceMeta::default()
                    },
                });
                self.y += line_height - font_size;
                paragraph_line_index += 1;
            }
            if segment_index + 1 < page_segments.len() {
                self.flush_page(true)?;
            }
        }
        self.y += style.space_after.unwrap_or(8.0);
        Ok(())
    }

    fn floating_text_box(&mut self, text_box: &FloatingTextBox, paragraph_y: f64) {
        let (x, y) = floating_text_box_position(text_box, &self.setup, paragraph_y);
        let width = text_box.width.max(1.0);
        let height = text_box.height.max(1.0);
        self.node_counter += 1;
        let clip_id = format!("docx-text-box-clip-{}", self.node_counter);
        self.page.clips.push(crate::ir::ClipPath {
            id: clip_id.clone(),
            d: format!(
                "M {} {} H {} V {} H {} Z",
                fmt(x),
                fmt(y),
                fmt(x + width),
                fmt(y + height),
                fmt(x)
            ),
            transform: IDENTITY,
            fill_rule: "nonzero".into(),
            parent_id: None,
            additional_paths: Vec::new(),
        });
        self.page.nodes.push(Node::Path {
            id: format!("docx-floating-text-box-{}", self.node_counter),
            d: format!(
                "M {} {} H {} V {} H {} Z",
                fmt(x),
                fmt(y),
                fmt(x + width),
                fmt(y + height),
                fmt(x)
            ),
            fill_rule: "nonzero".into(),
            fill: text_box.fill.clone(),
            stroke: text_box.stroke.clone(),
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                kind: "floating-text-box".into(),
                alt_text: text_box.alt_text.clone(),
                ..SourceMeta::default()
            },
        });
        let mut text_y = y + 4.0;
        for paragraph in &text_box.paragraphs {
            for image in &paragraph.images {
                let image_width = image.width.min((width - 8.0).max(1.0));
                let image_height = image.height * (image_width / image.width.max(1.0));
                if text_y + image_height > y + height - 2.0 {
                    return;
                }
                self.node_counter += 1;
                self.page.nodes.push(Node::Image {
                    id: format!("docx-text-box-image-{}", self.node_counter),
                    href: image.href.clone(),
                    x: x + 4.0,
                    y: text_y,
                    width: image_width,
                    height: image_height,
                    transform: IDENTITY,
                    opacity: 1.0,
                    clip_id: Some(clip_id.clone()),
                    meta: SourceMeta {
                        kind: "text-box-image".into(),
                        alt_text: image.alt_text.clone(),
                        ..SourceMeta::default()
                    },
                });
                text_y += image_height + 2.0;
            }
            let numbered = self.numbered_runs(paragraph);
            let materialized = self.materialize_fields(&numbered);
            for runs in wrap_runs(&materialized, (width - 8.0).max(6.0)) {
                let font_size = runs.iter().map(|run| run.font_size).fold(11.0, f64::max);
                text_y += font_size;
                if text_y > y + height - 2.0 {
                    return;
                }
                let anchor = paragraph.style.alignment.unwrap_or(TextAnchor::Start);
                let text_x = match anchor {
                    TextAnchor::Start => x + 4.0,
                    TextAnchor::Middle => x + width / 2.0,
                    TextAnchor::End => x + width - 4.0,
                };
                self.node_counter += 1;
                self.page.nodes.push(Node::Text {
                    id: format!("docx-text-box-text-{}", self.node_counter),
                    x: text_x,
                    y: text_y,
                    runs,
                    anchor,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: Some(clip_id.clone()),
                    meta: SourceMeta {
                        kind: "text-box-text".into(),
                        semantic_role: "text-box".into(),
                        ..SourceMeta::default()
                    },
                });
                text_y += font_size * 0.2;
            }
        }
    }

    fn numbered_runs(&mut self, paragraph: &Paragraph) -> Vec<TextRun> {
        if paragraph.numbering_id.is_empty() || paragraph.numbering_id == "0" {
            return paragraph.runs.clone();
        }
        let Some(marker) = self.numbering.marker(
            &paragraph.numbering_id,
            paragraph.numbering_level,
            &mut self.numbering_counters,
        ) else {
            return paragraph.runs.clone();
        };
        let mut marker_run = paragraph.runs.first().cloned().unwrap_or(TextRun {
            text: String::new(),
            font_family: "Calibri, Arial, sans-serif".into(),
            font_size: 11.0,
            fill: Paint::solid("#000000"),
            ..TextRun::default()
        });
        marker_run.text = format!("{marker} ");
        std::iter::once(marker_run)
            .chain(paragraph.runs.iter().cloned())
            .collect()
    }

    fn materialize_fields(&mut self, runs: &[TextRun]) -> Vec<TextRun> {
        let (runs, references) = materialize_note_fields(runs, self.page.number);
        for reference in references {
            if !self.pending_notes.contains(&reference) {
                self.pending_notes.push(reference);
            }
        }
        runs
    }

    fn table(&mut self, table: &Table) -> Result<()> {
        let available = self.setup.width - self.setup.margin_left - self.setup.margin_right;
        let column_count = table
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.grid_span.max(1))
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0)
            .max(table.grid_widths.len());
        if column_count == 0 {
            return Ok(());
        }
        let widths = if table.grid_widths.len() >= column_count {
            let total: f64 = table.grid_widths.iter().take(column_count).sum();
            if total > 0.0 {
                table
                    .grid_widths
                    .iter()
                    .take(column_count)
                    .map(|width| width / total * available)
                    .collect::<Vec<_>>()
            } else {
                vec![available / column_count as f64; column_count]
            }
        } else {
            vec![available / column_count as f64; column_count]
        };
        for (row_index, row) in table.rows.iter().enumerate() {
            let row_height = table_row_height(row, &widths).max(20.0);
            if self.y + row_height > self.bottom() {
                self.flush_page(true)?;
            }
            let mut x = self.setup.margin_left;
            let mut column_index = 0usize;
            for cell in &row.cells {
                if column_index >= widths.len() {
                    break;
                }
                let span = cell.grid_span.max(1).min(widths.len() - column_index);
                let width = widths[column_index..column_index + span]
                    .iter()
                    .sum::<f64>();
                self.node_counter += 1;
                self.page.nodes.push(Node::Path {
                    id: format!("docx-table-cell-{}", self.node_counter),
                    d: format!(
                        "M {} {} H {} V {} H {} Z",
                        fmt(x),
                        fmt(self.y),
                        fmt(x + width),
                        fmt(self.y + row_height),
                        fmt(x)
                    ),
                    fill_rule: "nonzero".into(),
                    fill: cell.fill.as_deref().map_or(Paint::None, Paint::solid),
                    stroke: Stroke {
                        paint: Paint::solid("#808080"),
                        width: 0.5,
                        miter_limit: 10.0,
                        ..Stroke::default()
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "table-cell".into(),
                        source_id: format!("R{}C{}", row_index + 1, column_index + 1),
                        semantic_role: "cell".into(),
                        ..SourceMeta::default()
                    },
                });
                let mut cell_y = self.y + 3.0;
                for paragraph in &cell.paragraphs {
                    let numbered_runs = self.numbered_runs(paragraph);
                    let materialized = self.materialize_fields(&numbered_runs);
                    let lines = wrap_runs(&materialized, (width - 6.0).max(6.0));
                    for runs in lines {
                        let font = runs.iter().map(|run| run.font_size).fold(10.0, f64::max);
                        cell_y += font;
                        self.node_counter += 1;
                        self.page.nodes.push(Node::Text {
                            id: format!("docx-table-text-{}", self.node_counter),
                            x: x + 3.0,
                            y: cell_y,
                            runs,
                            anchor: TextAnchor::Start,
                            transform: IDENTITY,
                            opacity: 1.0,
                            stroke: Stroke::default(),
                            clip_id: None,
                            meta: SourceMeta {
                                kind: "table-text".into(),
                                semantic_role: "cell".into(),
                                ..SourceMeta::default()
                            },
                        });
                        cell_y += font * 0.2;
                    }
                }
                x += width;
                column_index += span;
            }
            self.y += row_height;
        }
        self.y += 8.0;
        Ok(())
    }

    fn estimated_note_height(&self) -> f64 {
        if self.pending_notes.is_empty() {
            return 0.0;
        }
        let paragraph_count = self
            .pending_notes
            .iter()
            .map(|reference| {
                let notes = match reference.kind {
                    'E' => self.endnotes,
                    'C' => self.comments,
                    _ => self.footnotes,
                };
                notes
                    .get(&reference.id)
                    .map_or(1, |paragraphs| paragraphs.len().max(1))
            })
            .sum::<usize>();
        8.0 + paragraph_count as f64 * 11.0
    }

    fn add_page_notes(&mut self) {
        if self.pending_notes.is_empty() {
            return;
        }
        let references = std::mem::take(&mut self.pending_notes);
        let available =
            (self.setup.width - self.setup.margin_left - self.setup.margin_right).max(12.0);
        let mut rendered = Vec::<(NoteReference, Vec<Vec<TextRun>>)>::new();
        let mut total_height = 8.0;
        for reference in references {
            let notes = match reference.kind {
                'E' => self.endnotes,
                'C' => self.comments,
                _ => self.footnotes,
            };
            let Some(paragraphs) = notes.get(&reference.id) else {
                self.warnings.push(format!(
                    "DOCX {} {} is referenced but missing",
                    match reference.kind {
                        'E' => "endnote",
                        'C' => "comment",
                        _ => "footnote",
                    },
                    reference.id
                ));
                continue;
            };
            let mut lines = Vec::new();
            for (paragraph_index, paragraph) in paragraphs.iter().enumerate() {
                let mut runs = materialize_page_fields(&paragraph.runs, self.page.number);
                for run in &mut runs {
                    run.font_size = run.font_size.min(9.0);
                }
                if paragraph_index == 0 {
                    let mut marker = runs.first().cloned().unwrap_or(TextRun {
                        font_family: "Calibri, Arial, sans-serif".into(),
                        font_size: 9.0,
                        fill: Paint::solid("#000000"),
                        ..TextRun::default()
                    });
                    marker.text = format!("{} ", reference.id);
                    marker.baseline_shift = 2.0;
                    runs.insert(0, marker);
                }
                let wrapped = wrap_runs(&runs, available);
                total_height += wrapped.len().max(1) as f64 * 10.5;
                lines.extend(wrapped);
            }
            rendered.push((reference, lines));
        }
        if rendered.is_empty() {
            return;
        }
        let mut y = (self.setup.height - self.setup.margin_bottom - total_height)
            .max(self.setup.margin_top);
        self.node_counter += 1;
        self.page.nodes.push(Node::Path {
            id: format!("docx-note-separator-{}", self.node_counter),
            d: format!(
                "M {} {} H {}",
                fmt(self.setup.margin_left),
                fmt(y),
                fmt(self.setup.margin_left + available.min(72.0))
            ),
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: Stroke {
                paint: Paint::solid("#606060"),
                width: 0.5,
                miter_limit: 10.0,
                ..Stroke::default()
            },
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                kind: "note-separator".into(),
                ..SourceMeta::default()
            },
        });
        y += 8.0;
        for (reference, lines) in rendered {
            for runs in lines {
                let font_size = runs.iter().map(|run| run.font_size).fold(9.0, f64::max);
                y += font_size;
                self.node_counter += 1;
                self.page.nodes.push(Node::Text {
                    id: format!("docx-note-{}-{}", reference.id, self.node_counter),
                    x: self.setup.margin_left,
                    y,
                    runs,
                    anchor: TextAnchor::Start,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta {
                        kind: match reference.kind {
                            'E' => "endnote",
                            'C' => "comment",
                            _ => "footnote",
                        }
                        .into(),
                        semantic_role: "note".into(),
                        ..SourceMeta::default()
                    },
                });
                y += 10.5 - font_size;
            }
        }
    }

    fn bottom(&self) -> f64 {
        self.setup.height - self.setup.margin_bottom - self.estimated_note_height()
    }

    fn switch_section(
        &mut self,
        setup: &PageSetup,
        story_references: &StoryReferences,
    ) -> Result<()> {
        self.flush_page(false)?;
        let page_number = self.page.number;
        self.setup = setup.clone();
        self.story_references.clone_from(story_references);
        self.section_first_page = page_number;
        self.page = Page::new(page_number, self.setup.width, self.setup.height, "docx");
        self.page.description =
            "DOCX section page approximated with deterministic Rust layout".into();
        self.y = self.setup.margin_top;
        self.add_repeating_stories()
    }

    fn flush_page(&mut self, create_next: bool) -> Result<()> {
        if self.page.number > self.max_pages {
            return Err(Error::LimitExceeded(format!(
                "DOCX pagination exceeds {} pages",
                self.max_pages
            )));
        }
        if self.page.nodes.is_empty() && !create_next {
            return Ok(());
        }
        self.add_page_notes();
        self.page.warnings = self.warnings.clone();
        let next_number = self.page.number + 1;
        let finished = std::mem::replace(
            &mut self.page,
            Page::new(next_number, self.setup.width, self.setup.height, "docx"),
        );
        self.sink.consume(finished)?;
        self.y = self.setup.margin_top;
        if create_next {
            self.add_repeating_stories()?;
        }
        Ok(())
    }
}

fn materialize_note_fields(
    runs: &[TextRun],
    page_number: usize,
) -> (Vec<TextRun>, Vec<NoteReference>) {
    let mut output = Vec::new();
    let mut references = Vec::new();
    for source in materialize_page_fields(runs, page_number) {
        let mut remaining = source.text.as_str();
        while let Some(start) = remaining.find(NOTE_FIELD_START) {
            if start > 0 {
                let mut run = source.clone();
                run.text = remaining[..start].to_owned();
                output.push(run);
            }
            let marker_start = start + NOTE_FIELD_START.len_utf8();
            let Some(relative_end) = remaining[marker_start..].find(NOTE_FIELD_END) else {
                let mut run = source.clone();
                run.text = remaining[start..].to_owned();
                output.push(run);
                remaining = "";
                break;
            };
            let marker_end = marker_start + relative_end;
            let marker = &remaining[marker_start..marker_end];
            if let Some(kind) = marker.chars().next() {
                let id = marker[kind.len_utf8()..].to_owned();
                if matches!(kind, 'F' | 'E' | 'C') && !id.is_empty() {
                    references.push(NoteReference {
                        kind,
                        id: id.clone(),
                    });
                    let mut run = source.clone();
                    run.text = id;
                    run.font_size *= 0.75;
                    run.baseline_shift += source.font_size * 0.35;
                    output.push(run);
                }
            }
            remaining = &remaining[marker_end + NOTE_FIELD_END.len_utf8()..];
        }
        if !remaining.is_empty() {
            let mut run = source.clone();
            run.text = remaining.to_owned();
            output.push(run);
        }
    }
    (output, references)
}

fn materialize_page_fields(runs: &[TextRun], page_number: usize) -> Vec<TextRun> {
    runs.iter()
        .cloned()
        .map(|mut run| {
            if run.text.contains(PAGE_FIELD_MARKER) {
                run.text = run
                    .text
                    .replace(PAGE_FIELD_MARKER, &page_number.to_string());
            }
            run
        })
        .collect()
}

fn floating_image_position(image: &InlineImage, setup: &PageSetup, paragraph_y: f64) -> (f64, f64) {
    let x = match image.horizontal_relative.as_str() {
        "page" => image.x,
        "margin" | "leftMargin" | "rightMargin" | "column" => setup.margin_left + image.x,
        _ => setup.margin_left + image.x,
    };
    let y = match image.vertical_relative.as_str() {
        "page" => image.y,
        "margin" | "topMargin" | "bottomMargin" => setup.margin_top + image.y,
        "paragraph" | "line" => paragraph_y + image.y,
        _ => paragraph_y + image.y,
    };
    (x, y)
}

fn floating_text_box_position(
    text_box: &FloatingTextBox,
    setup: &PageSetup,
    paragraph_y: f64,
) -> (f64, f64) {
    if !text_box.floating {
        return (setup.margin_left + text_box.x, paragraph_y + text_box.y);
    }
    let x = match text_box.horizontal_relative.as_str() {
        "page" => text_box.x,
        "margin" | "leftMargin" | "rightMargin" | "column" => setup.margin_left + text_box.x,
        _ => setup.margin_left + text_box.x,
    };
    let y = match text_box.vertical_relative.as_str() {
        "page" => text_box.y,
        "margin" | "topMargin" | "bottomMargin" => setup.margin_top + text_box.y,
        "paragraph" | "line" => paragraph_y + text_box.y,
        _ => paragraph_y + text_box.y,
    };
    (x, y)
}

fn split_runs_on_page_breaks(runs: &[TextRun]) -> Vec<Vec<TextRun>> {
    let mut segments = vec![Vec::<TextRun>::new()];
    for source in runs {
        let mut remaining = source.text.as_str();
        while let Some(index) = remaining.find(PAGE_BREAK_MARKER) {
            if index > 0 {
                let mut run = source.clone();
                run.text = remaining[..index].to_owned();
                segments
                    .last_mut()
                    .expect("segment always exists")
                    .push(run);
            }
            segments.push(Vec::new());
            remaining = &remaining[index + PAGE_BREAK_MARKER.len()..];
        }
        if !remaining.is_empty() {
            let mut run = source.clone();
            run.text = remaining.to_owned();
            segments
                .last_mut()
                .expect("segment always exists")
                .push(run);
        }
    }
    segments
}

fn wrap_runs(runs: &[TextRun], width: f64) -> Vec<Vec<TextRun>> {
    if runs.is_empty() {
        return vec![Vec::new()];
    }
    let cleaned_runs = runs
        .iter()
        .cloned()
        .map(|mut run| {
            if run.text.contains(PAGE_BREAK_MARKER) {
                run.text = run.text.replace(PAGE_BREAK_MARKER, " ");
            }
            run
        })
        .collect::<Vec<_>>();
    let runs = cleaned_runs.as_slice();
    let mut lines = vec![Vec::<TextRun>::new()];
    let mut line_width = 0.0;
    let mut pending_space = Vec::<DocxStyledCharacter>::new();
    let mut pending_space_width = 0.0;
    let mut word = Vec::<DocxStyledCharacter>::new();
    let mut word_width = 0.0;
    for run in runs {
        for character in run.text.chars() {
            if character == '\n' {
                place_docx_word(
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
            let styled = DocxStyledCharacter {
                character,
                run: run.clone(),
                width: run.font_size * text_advance_factor(character),
            };
            if character.is_whitespace() {
                place_docx_word(
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
    place_docx_word(
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

struct DocxStyledCharacter {
    character: char,
    run: TextRun,
    width: f64,
}

#[allow(clippy::too_many_arguments)]
fn place_docx_word(
    lines: &mut Vec<Vec<TextRun>>,
    line_width: &mut f64,
    pending_space: &mut Vec<DocxStyledCharacter>,
    pending_space_width: &mut f64,
    word: &mut Vec<DocxStyledCharacter>,
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
                append_docx_character(lines, character);
            }
            *line_width += *pending_space_width;
        }
        for character in word.drain(..) {
            append_docx_character(lines, character);
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
            append_docx_character(lines, character);
        }
    }
    pending_space.clear();
    *pending_space_width = 0.0;
    *word_width = 0.0;
}

fn append_docx_character(lines: &mut [Vec<TextRun>], character: DocxStyledCharacter) {
    let line = lines.last_mut().expect("line always exists");
    if let Some(last) = line.last_mut()
        && same_run_style(last, &character.run)
    {
        last.text.push(character.character);
    } else {
        let mut run = character.run;
        run.text = character.character.to_string();
        line.push(run);
    }
}

fn same_run_style(left: &TextRun, right: &TextRun) -> bool {
    left.font_family == right.font_family
        && (left.font_size - right.font_size).abs() < 1e-9
        && left.bold == right.bold
        && left.italic == right.italic
        && left.fill == right.fill
        && (left.baseline_shift - right.baseline_shift).abs() < 1e-9
}

fn table_row_height(row: &TableRow, widths: &[f64]) -> f64 {
    let mut column_index = 0usize;
    row.cells
        .iter()
        .map(|cell| {
            let remaining = widths.len().saturating_sub(column_index);
            let span = cell.grid_span.max(1).min(remaining.max(1));
            let width = if remaining == 0 {
                72.0
            } else {
                widths[column_index..column_index + span].iter().sum()
            };
            column_index = column_index.saturating_add(span);
            let width = width.max(6.0) - 6.0;
            cell.paragraphs
                .iter()
                .map(|paragraph| {
                    wrap_runs(&paragraph.runs, width)
                        .into_iter()
                        .map(|runs| runs.iter().map(|run| run.font_size).fold(10.0, f64::max) * 1.2)
                        .sum::<f64>()
                })
                .sum::<f64>()
                + 6.0
        })
        .fold(20.0, f64::max)
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
        _ => "image/png",
    }
}

fn fmt(value: f64) -> String {
    format!("{value:.4}")
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
