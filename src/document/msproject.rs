//! Bounded Microsoft Project XML interchange preview.
//!
//! Project XML is read as task data only. The preview shows supplied task
//! dates, progress, summary bars, milestones, and simple dependency arrows;
//! it does not recalculate schedules or load external references.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};

const MAX_PROJECT_XML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PROJECT_XML_EVENTS: usize = 1_000_000;
const MAX_PROJECT_XML_DEPTH: usize = 64;
const MAX_PROJECT_TASKS: usize = 100_000;
const MAX_PROJECT_DEPENDENCIES: usize = 500_000;
const MAX_PROJECT_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROJECT_FIELD_BYTES: usize = 1024 * 1024;
const TASKS_PER_PAGE: usize = 25;
const MAX_DEPENDENCY_EDGES_PER_PAGE: usize = 1_000;
const MILLIS_PER_DAY: i64 = 86_400_000;
const PROJECT_XML_NAMESPACE: &str = "http://schemas.microsoft.com/project";
const PROJECT_XML_NAMESPACE_HTTPS: &str = "https://schemas.microsoft.com/project";
const PAGE_WIDTH: f64 = 792.0;
const PAGE_HEIGHT: f64 = 612.0;
const AXIS_X: f64 = 296.0;
const AXIS_WIDTH: f64 = 464.0;
const TASKS_TOP: f64 = 111.0;
const TASK_ROW_HEIGHT: f64 = 17.0;

#[derive(Clone, Debug, Default)]
struct ProjectTask {
    uid: Option<u32>,
    name: String,
    start: Option<i64>,
    finish: Option<i64>,
    percent_complete: u8,
    outline_level: u8,
    summary: bool,
    milestone: bool,
    predecessors: Vec<u32>,
}

#[derive(Default)]
struct TaskBuilder {
    uid: Option<u32>,
    name: String,
    start: Option<i64>,
    finish: Option<i64>,
    percent_complete: u8,
    outline_level: u8,
    summary: bool,
    milestone: bool,
    predecessors: Vec<u32>,
}

#[derive(Clone, Copy)]
enum CaptureField {
    ProjectName,
    Uid,
    Name,
    Start,
    Finish,
    PercentComplete,
    OutlineLevel,
    Summary,
    Milestone,
    PredecessorUid,
}

struct Capture {
    field: CaptureField,
    depth: usize,
    text: String,
}

struct ParsedProject {
    name: String,
    tasks: Vec<ProjectTask>,
    warnings: Vec<String>,
}

pub(crate) fn looks_like_project_xml_prefix(bytes: &[u8]) -> bool {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start) | Event::Empty(start)) => {
                return local_name(start.name().as_ref()) == b"Project"
                    && has_project_namespace(&start);
            }
            Ok(Event::Eof) | Err(_) => return false,
            Ok(_) => buffer.clear(),
        }
        buffer.clear();
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_PROJECT_XML_BYTES),
        "Microsoft Project XML input",
    )?;
    let project = parse_project(&bytes, options.max_xml_events.min(MAX_PROJECT_XML_EVENTS))?;
    render_project(project, options.max_pages, sink)
}

fn parse_project(bytes: &[u8], max_events: usize) -> Result<ParsedProject> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut capture = None::<Capture>;
    let mut active_task = None::<TaskBuilder>;
    let mut project_name = None::<String>;
    let mut tasks = Vec::new();
    let mut event_count = 0usize;
    let mut total_text_bytes = 0usize;
    let mut dependency_count = 0usize;
    let mut invalid_date_values = 0usize;
    let mut invalid_numeric_values = 0usize;
    let mut saw_root = false;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_events {
            return Err(Error::LimitExceeded(format!(
                "Microsoft Project XML exceeds {max_events} parser events"
            )));
        }
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| Error::InvalidInput(format!("invalid Project XML: {error}")))?;
        match event {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                let parent = stack.last().map(String::as_str).unwrap_or_default();
                let depth = stack.len().saturating_add(1);
                if depth > MAX_PROJECT_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "Microsoft Project XML exceeds {MAX_PROJECT_XML_DEPTH} levels of nesting"
                    )));
                }
                if !saw_root {
                    if name != "Project" || !has_project_namespace(&start) {
                        return Err(Error::Unsupported(
                            "XML is not a Microsoft Project XML interchange document".into(),
                        ));
                    }
                    saw_root = true;
                }
                if name == "Task" && parent == "Tasks" {
                    if active_task.is_some() {
                        return Err(Error::InvalidInput(
                            "Microsoft Project XML contains nested Task elements".into(),
                        ));
                    }
                    if tasks.len() >= MAX_PROJECT_TASKS {
                        return Err(Error::LimitExceeded(format!(
                            "Microsoft Project XML exceeds {MAX_PROJECT_TASKS} tasks"
                        )));
                    }
                    active_task = Some(TaskBuilder::default());
                }
                let field = if active_task.is_some() {
                    if parent == "Task" {
                        match name.as_str() {
                            "UID" => Some(CaptureField::Uid),
                            "Name" => Some(CaptureField::Name),
                            "Start" => Some(CaptureField::Start),
                            "Finish" => Some(CaptureField::Finish),
                            "PercentComplete" => Some(CaptureField::PercentComplete),
                            "OutlineLevel" => Some(CaptureField::OutlineLevel),
                            "Summary" => Some(CaptureField::Summary),
                            "Milestone" => Some(CaptureField::Milestone),
                            _ => None,
                        }
                    } else if parent == "PredecessorLink" && name == "PredecessorUID" {
                        Some(CaptureField::PredecessorUid)
                    } else {
                        None
                    }
                } else if parent == "Project" && name == "Name" && project_name.is_none() {
                    Some(CaptureField::ProjectName)
                } else {
                    None
                };
                if let Some(field) = field {
                    capture = Some(Capture {
                        field,
                        depth,
                        text: String::new(),
                    });
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                let parent = stack.last().map(String::as_str).unwrap_or_default();
                if !saw_root {
                    if name != "Project" || !has_project_namespace(&start) {
                        return Err(Error::Unsupported(
                            "XML is not a Microsoft Project XML interchange document".into(),
                        ));
                    }
                    saw_root = true;
                }
                if name == "Task" && parent == "Tasks" {
                    return Err(Error::InvalidInput(
                        "Microsoft Project XML contains an empty Task element".into(),
                    ));
                }
            }
            Event::Text(text) => {
                let decoded = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid Project XML text: {error}"))
                })?;
                let unescaped = quick_xml::escape::unescape(&decoded).map_err(|error| {
                    Error::InvalidInput(format!("invalid Project XML entity: {error}"))
                })?;
                append_capture(
                    capture.as_mut(),
                    stack.len(),
                    &unescaped,
                    &mut total_text_bytes,
                )?;
            }
            Event::CData(text) => {
                let decoded = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid Project XML CDATA: {error}"))
                })?;
                append_capture(
                    capture.as_mut(),
                    stack.len(),
                    &decoded,
                    &mut total_text_bytes,
                )?;
            }
            Event::GeneralRef(reference) => {
                let decoded = crate::ooxml::decode_xml_reference(&reference, "Project XML")?;
                append_capture(
                    capture.as_mut(),
                    stack.len(),
                    &decoded,
                    &mut total_text_bytes,
                )?;
            }
            Event::End(end) => {
                let name = String::from_utf8_lossy(local_name(end.name().as_ref())).into_owned();
                if stack.last().map(String::as_str) != Some(name.as_str()) {
                    return Err(Error::InvalidInput(
                        "Microsoft Project XML has mismatched element tags".into(),
                    ));
                }
                if capture
                    .as_ref()
                    .is_some_and(|capture| capture.depth == stack.len())
                {
                    let captured = capture.take().expect("capture matched its depth");
                    apply_field(
                        captured.field,
                        captured.text,
                        &mut active_task,
                        &mut project_name,
                        &mut invalid_date_values,
                        &mut invalid_numeric_values,
                        &mut dependency_count,
                    )?;
                }
                if name == "Task" && stack.len() == 3 && stack.get(1).is_some_and(|v| v == "Tasks")
                {
                    let builder = active_task.take().ok_or_else(|| {
                        Error::InvalidInput("Microsoft Project Task state was lost".into())
                    })?;
                    tasks.push(finish_task(builder, tasks.len() + 1));
                }
                stack.pop();
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "Microsoft Project XML document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !saw_root || !stack.is_empty() || active_task.is_some() || capture.is_some() {
        return Err(Error::InvalidInput(
            "Microsoft Project XML ended inside an incomplete document or Task".into(),
        ));
    }
    if tasks.is_empty() {
        return Err(Error::InvalidInput(
            "Microsoft Project XML contains no Tasks/Task records".into(),
        ));
    }
    let mut warnings = Vec::new();
    if invalid_date_values > 0 {
        warnings.push(format!(
            "{invalid_date_values} invalid Microsoft Project date value(s) were omitted"
        ));
    }
    if invalid_numeric_values > 0 {
        warnings.push(format!(
            "{invalid_numeric_values} invalid or out-of-range Microsoft Project progress/outline value(s) were clamped or defaulted"
        ));
    }
    Ok(ParsedProject {
        name: project_name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Microsoft Project schedule".into()),
        tasks,
        warnings,
    })
}

fn append_capture(
    capture: Option<&mut Capture>,
    depth: usize,
    text: &str,
    total_text_bytes: &mut usize,
) -> Result<()> {
    let Some(capture) = capture.filter(|capture| capture.depth == depth) else {
        return Ok(());
    };
    *total_text_bytes = total_text_bytes.saturating_add(text.len());
    if *total_text_bytes > MAX_PROJECT_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Microsoft Project XML text exceeds {MAX_PROJECT_TEXT_BYTES} bytes"
        )));
    }
    if capture.text.len().saturating_add(text.len()) > MAX_PROJECT_FIELD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Microsoft Project XML field exceeds {MAX_PROJECT_FIELD_BYTES} bytes"
        )));
    }
    capture.text.push_str(text);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_field(
    field: CaptureField,
    text: String,
    active_task: &mut Option<TaskBuilder>,
    project_name: &mut Option<String>,
    invalid_date_values: &mut usize,
    invalid_numeric_values: &mut usize,
    dependency_count: &mut usize,
) -> Result<()> {
    let value = text.trim();
    match field {
        CaptureField::ProjectName => *project_name = Some(value.to_owned()),
        CaptureField::Uid => {
            if let Some(task) = active_task.as_mut() {
                task.uid = value.parse().ok();
            }
        }
        CaptureField::Name => {
            if let Some(task) = active_task.as_mut() {
                task.name = value.to_owned();
            }
        }
        CaptureField::Start => {
            if let Some(task) = active_task.as_mut() {
                task.start = parse_project_timestamp(value);
                if !value.is_empty() && task.start.is_none() {
                    *invalid_date_values = invalid_date_values.saturating_add(1);
                }
            }
        }
        CaptureField::Finish => {
            if let Some(task) = active_task.as_mut() {
                task.finish = parse_project_timestamp(value);
                if !value.is_empty() && task.finish.is_none() {
                    *invalid_date_values = invalid_date_values.saturating_add(1);
                }
            }
        }
        CaptureField::PercentComplete => {
            if let Some(task) = active_task.as_mut()
                && !value.is_empty()
            {
                match value.parse::<i64>() {
                    Ok(value) => {
                        task.percent_complete = value.clamp(0, 100) as u8;
                        if !(0..=100).contains(&value) {
                            *invalid_numeric_values = invalid_numeric_values.saturating_add(1);
                        }
                    }
                    Err(_) => {
                        *invalid_numeric_values = invalid_numeric_values.saturating_add(1);
                    }
                }
            }
        }
        CaptureField::OutlineLevel => {
            if let Some(task) = active_task.as_mut()
                && !value.is_empty()
            {
                match value.parse::<u8>() {
                    Ok(value) => task.outline_level = value.min(8),
                    Err(_) => *invalid_numeric_values = invalid_numeric_values.saturating_add(1),
                }
            }
        }
        CaptureField::Summary => {
            if let Some(task) = active_task.as_mut() {
                task.summary = parse_project_bool(value);
            }
        }
        CaptureField::Milestone => {
            if let Some(task) = active_task.as_mut() {
                task.milestone = parse_project_bool(value);
            }
        }
        CaptureField::PredecessorUid => {
            if !value.is_empty() {
                let predecessor = value.parse::<u32>().map_err(|_| {
                    Error::InvalidInput("Microsoft Project PredecessorUID is invalid".into())
                })?;
                *dependency_count = dependency_count.saturating_add(1);
                if *dependency_count > MAX_PROJECT_DEPENDENCIES {
                    return Err(Error::LimitExceeded(format!(
                        "Microsoft Project XML exceeds {MAX_PROJECT_DEPENDENCIES} predecessor links"
                    )));
                }
                if let Some(task) = active_task.as_mut() {
                    task.predecessors.push(predecessor);
                }
            }
        }
    }
    Ok(())
}

fn finish_task(builder: TaskBuilder, index: usize) -> ProjectTask {
    ProjectTask {
        uid: builder.uid,
        name: if builder.name.trim().is_empty() {
            format!("Task {index}")
        } else {
            builder.name.trim().to_owned()
        },
        start: builder.start,
        finish: builder.finish,
        percent_complete: builder.percent_complete,
        outline_level: builder.outline_level,
        summary: builder.summary,
        milestone: builder.milestone,
        predecessors: builder.predecessors,
    }
}

fn parse_project_timestamp(value: &str) -> Option<i64> {
    if value.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp_millis())
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|date| date.and_utc().timestamp_millis())
        })
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|date| date.and_utc().timestamp_millis())
        })
}

fn parse_project_bool(value: &str) -> bool {
    matches!(value.trim(), "1" | "true" | "TRUE" | "True")
}

fn render_project(
    mut project: ParsedProject,
    max_pages: usize,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    for task in &mut project.tasks {
        if let (Some(start), Some(finish)) = (task.start, task.finish)
            && finish < start
        {
            task.start = None;
            task.finish = None;
            project.warnings.push(format!(
                "task '{}' has Finish earlier than Start and its bar was omitted",
                task.name
            ));
        }
    }
    let page_count = project.tasks.len().div_ceil(TASKS_PER_PAGE);
    if page_count > max_pages {
        return Err(Error::LimitExceeded(format!(
            "Microsoft Project schedule needs {page_count} pages for {} tasks; maximum is {max_pages}",
            project.tasks.len()
        )));
    }
    let bounds = project
        .tasks
        .iter()
        .flat_map(|task| [task.start, task.finish].into_iter().flatten())
        .fold(None::<(i64, i64)>, |bounds, value| {
            Some(match bounds {
                Some((minimum, maximum)) => (minimum.min(value), maximum.max(value)),
                None => (value, value),
            })
        });
    let (minimum, mut maximum) = bounds.unwrap_or((0, MILLIS_PER_DAY));
    if maximum <= minimum {
        maximum = minimum.saturating_add(MILLIS_PER_DAY);
    }
    let range = maximum.saturating_sub(minimum).max(1) as f64;
    let mut uid_to_index = HashMap::new();
    let mut duplicate_uids = false;
    for (index, task) in project.tasks.iter().enumerate() {
        if let Some(uid) = task.uid
            && uid_to_index.insert(uid, index).is_some()
        {
            duplicate_uids = true;
        }
    }
    let mut warnings = project.warnings;
    if bounds.is_none() {
        push_warning_once(
            &mut warnings,
            "no valid Start/Finish dates were found; task names and progress are shown without schedule bars",
        );
    }
    if project
        .tasks
        .iter()
        .any(|task| task.start.is_none() || task.finish.is_none())
    {
        push_warning_once(
            &mut warnings,
            "tasks without both Start and Finish dates are shown without a schedule bar",
        );
    }
    if project
        .tasks
        .iter()
        .any(|task| !task.predecessors.is_empty())
    {
        push_warning_once(
            &mut warnings,
            "predecessor relationships are drawn as simple arrows; calendars, lag, and dependency types are not recalculated",
        );
    }
    if duplicate_uids {
        push_warning_once(
            &mut warnings,
            "duplicate Microsoft Project task UID values make some predecessor targets ambiguous",
        );
    }
    let mut predecessor_edges_per_page = vec![0usize; page_count];
    let mut missing_edges = false;
    let mut cross_page_edges = false;
    let mut truncated_edges = false;
    for (task_index, task) in project.tasks.iter().enumerate() {
        for predecessor in &task.predecessors {
            let Some(&predecessor_index) = uid_to_index.get(predecessor) else {
                missing_edges = true;
                continue;
            };
            if predecessor_index / TASKS_PER_PAGE != task_index / TASKS_PER_PAGE {
                cross_page_edges = true;
                continue;
            }
            if project.tasks[predecessor_index].finish.is_none() || task.start.is_none() {
                missing_edges = true;
                continue;
            }
            let page_index = task_index / TASKS_PER_PAGE;
            predecessor_edges_per_page[page_index] =
                predecessor_edges_per_page[page_index].saturating_add(1);
            if predecessor_edges_per_page[page_index] > MAX_DEPENDENCY_EDGES_PER_PAGE {
                truncated_edges = true;
            }
        }
    }
    if truncated_edges {
        push_warning_once(
            &mut warnings,
            "predecessor arrow count per page exceeded the preview limit; remaining arrows were omitted",
        );
    }
    if missing_edges {
        push_warning_once(
            &mut warnings,
            "predecessor links with missing task dates or task IDs were omitted",
        );
    }
    if cross_page_edges {
        push_warning_once(
            &mut warnings,
            "predecessor arrows between tasks on different pages are omitted",
        );
    }
    warnings = deduplicate(warnings);

    for (page_index, page_tasks) in project.tasks.chunks(TASKS_PER_PAGE).enumerate() {
        let page_number = page_index + 1;
        let first_task_index = page_index * TASKS_PER_PAGE;
        let mut page = Page::new(page_number, PAGE_WIDTH, PAGE_HEIGHT, "msproject");
        page.title = format!(
            "{} — tasks {}–{}",
            project.name,
            first_task_index + 1,
            first_task_index + page_tasks.len()
        );
        page.description = format!(
            "Microsoft Project schedule page {page_number} with {} task rows",
            page_tasks.len()
        );
        for warning in &warnings {
            page.warn(warning.clone());
        }
        push_text(
            &mut page,
            "project-title",
            32.0,
            34.0,
            &truncate(&project.name, 70),
            18.0,
            true,
            "#172554",
            "office:project-title",
        );
        let range_text = if bounds.is_some() {
            format!(
                "{} – {}  ·  {} tasks",
                format_project_date(minimum),
                format_project_date(maximum),
                project.tasks.len()
            )
        } else {
            format!("{} tasks  ·  no scheduled dates", project.tasks.len())
        };
        push_text(
            &mut page,
            "project-range",
            32.0,
            54.0,
            &range_text,
            9.0,
            false,
            "#475569",
            "office:project-metadata",
        );
        push_text(
            &mut page,
            "task-column-heading",
            32.0,
            91.0,
            "Task",
            9.0,
            true,
            "#334155",
            "office:project-column-heading",
        );
        push_text(
            &mut page,
            "progress-column-heading",
            251.0,
            91.0,
            "%",
            9.0,
            true,
            "#334155",
            "office:project-column-heading",
        );
        let axis_bottom = TASKS_TOP + TASK_ROW_HEIGHT * TASKS_PER_PAGE as f64;
        for tick in 0..=4 {
            let fraction = f64::from(tick) / 4.0;
            let x = AXIS_X + AXIS_WIDTH * fraction;
            let date = minimum.saturating_add((range * fraction).round() as i64);
            push_line(
                &mut page,
                format!("timeline-grid-{tick}"),
                x,
                TASKS_TOP - 6.0,
                x,
                axis_bottom,
                "#CBD5E1",
                0.55,
            );
            push_text(
                &mut page,
                format!("timeline-label-{tick}"),
                (x - 25.0).max(AXIS_X),
                91.0,
                &format_project_date(date),
                8.0,
                false,
                "#475569",
                "office:project-date-label",
            );
        }
        let page_uid_to_row = page_tasks
            .iter()
            .enumerate()
            .filter_map(|(row, task)| task.uid.map(|uid| (uid, row)))
            .collect::<HashMap<_, _>>();
        let mut edge_count = 0usize;
        for (row, task) in page_tasks.iter().enumerate() {
            let target_start = task.start.map(|date| date_to_x(date, minimum, range));
            for predecessor in &task.predecessors {
                let Some(&predecessor_row) = page_uid_to_row.get(predecessor) else {
                    continue;
                };
                let predecessor_task = &page_tasks[predecessor_row];
                let (Some(predecessor_finish), Some(target_start)) =
                    (predecessor_task.finish, target_start)
                else {
                    continue;
                };
                if edge_count >= MAX_DEPENDENCY_EDGES_PER_PAGE {
                    break;
                }
                let start_x = date_to_x(predecessor_finish, minimum, range);
                let end_x = target_start;
                let start_y = TASKS_TOP + predecessor_row as f64 * TASK_ROW_HEIGHT + 8.0;
                let end_y = TASKS_TOP + row as f64 * TASK_ROW_HEIGHT + 8.0;
                push_dependency(
                    &mut page,
                    format!("dependency-{page_number}-{edge_count}"),
                    start_x,
                    start_y,
                    end_x,
                    end_y,
                );
                edge_count += 1;
            }
        }
        for (row, task) in page_tasks.iter().enumerate() {
            let y = TASKS_TOP + row as f64 * TASK_ROW_HEIGHT;
            push_line(
                &mut page,
                format!("task-row-{page_number}-{row}"),
                32.0,
                y + TASK_ROW_HEIGHT,
                760.0,
                y + TASK_ROW_HEIGHT,
                "#E2E8F0",
                0.45,
            );
            let indent = f64::from(task.outline_level.min(8)) * 7.0;
            push_text(
                &mut page,
                format!("task-label-{page_number}-{row}"),
                32.0 + indent,
                y + 12.0,
                &truncate(&task.name, 30),
                8.5,
                task.summary,
                if task.summary { "#1E293B" } else { "#334155" },
                "office:project-task",
            );
            push_text(
                &mut page,
                format!("task-progress-{page_number}-{row}"),
                251.0,
                y + 12.0,
                &format!("{}%", task.percent_complete),
                8.0,
                false,
                "#475569",
                "office:project-progress",
            );
            let start = task.start;
            let finish = task.finish;
            if task.milestone || (start.is_some() && start == finish) {
                if let Some(date) = start.or(finish) {
                    push_milestone(
                        &mut page,
                        format!("milestone-{page_number}-{row}"),
                        date_to_x(date, minimum, range),
                        y + 8.0,
                        task.summary,
                    );
                }
                continue;
            }
            if let (Some(start), Some(finish)) = (start, finish) {
                let x1 = date_to_x(start, minimum, range);
                let x2 = date_to_x(finish, minimum, range);
                let width = (x2 - x1).max(2.0);
                if task.summary {
                    push_summary_bar(
                        &mut page,
                        format!("summary-bar-{page_number}-{row}"),
                        x1,
                        y + 8.0,
                        width,
                    );
                } else {
                    let bar_width = width.min(AXIS_X + AXIS_WIDTH - x1);
                    push_bar(
                        &mut page,
                        format!("task-bar-{page_number}-{row}"),
                        x1,
                        y + 4.5,
                        bar_width,
                        7.0,
                        "#60A5FA",
                    );
                    let progress_width = bar_width * f64::from(task.percent_complete) / 100.0;
                    if progress_width > 0.0 {
                        push_bar(
                            &mut page,
                            format!("task-progress-bar-{page_number}-{row}"),
                            x1,
                            y + 4.5,
                            progress_width,
                            7.0,
                            "#2563EB",
                        );
                    }
                }
            }
        }
        if page_number < page_count {
            push_text(
                &mut page,
                "page-number",
                PAGE_WIDTH - 52.0,
                PAGE_HEIGHT - 18.0,
                &format!("{page_number} / {page_count}"),
                8.0,
                false,
                "#64748B",
                "office:page-number",
            );
        }
        sink.consume(page)?;
    }
    Ok(warnings)
}

fn date_to_x(date: i64, minimum: i64, range: f64) -> f64 {
    AXIS_X + ((date.saturating_sub(minimum) as f64 / range).clamp(0.0, 1.0) * AXIS_WIDTH)
}

fn format_project_date(timestamp_millis: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_millis)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "invalid date".into())
}

fn truncate(value: &str, maximum_chars: usize) -> String {
    let mut characters = value.chars();
    let value = characters.by_ref().take(maximum_chars).collect::<String>();
    if characters.next().is_some() {
        format!("{value}…")
    } else {
        value
    }
}

#[allow(clippy::too_many_arguments)]
fn push_text(
    page: &mut Page,
    id: impl Into<String>,
    x: f64,
    y: f64,
    text: &str,
    font_size: f64,
    bold: bool,
    color: &str,
    role: &str,
) {
    page.nodes.push(Node::Text {
        id: id.into(),
        x,
        y,
        runs: vec![TextRun {
            text: text.to_owned(),
            font_family: "Arial, sans-serif".into(),
            font_size,
            bold,
            fill: Paint::solid(color),
            ..TextRun::default()
        }],
        anchor: TextAnchor::Start,
        transform: IDENTITY,
        opacity: 1.0,
        stroke: Stroke::default(),
        clip_id: None,
        meta: SourceMeta {
            semantic_role: role.into(),
            ..SourceMeta::default()
        },
    });
}

#[allow(clippy::too_many_arguments)]
fn push_line(
    page: &mut Page,
    id: impl Into<String>,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    color: &str,
    width: f64,
) {
    page.nodes.push(Node::Path {
        id: id.into(),
        d: format!("M {x1} {y1} L {x2} {y2}"),
        fill_rule: "nonzero".into(),
        fill: Paint::None,
        stroke: Stroke {
            paint: Paint::solid(color),
            width,
            ..Stroke::default()
        },
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta::default(),
    });
}

fn push_bar(
    page: &mut Page,
    id: impl Into<String>,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    color: &str,
) {
    if width <= 0.0 || !width.is_finite() {
        return;
    }
    page.nodes.push(Node::Path {
        id: id.into(),
        d: format!("M {x} {y} H {} V {} H {x} Z", x + width, y + height),
        fill_rule: "nonzero".into(),
        fill: Paint::solid(color),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "office:project-task-bar".into(),
            ..SourceMeta::default()
        },
    });
}

fn push_summary_bar(page: &mut Page, id: impl Into<String>, x: f64, y: f64, width: f64) {
    let end = x + width;
    page.nodes.push(Node::Path {
        id: id.into(),
        d: format!(
            "M {x} {y} V {} M {x} {y} H {end} M {end} {y} V {}",
            y + 5.0,
            y + 5.0
        ),
        fill_rule: "nonzero".into(),
        fill: Paint::None,
        stroke: Stroke {
            paint: Paint::solid("#334155"),
            width: 1.8,
            ..Stroke::default()
        },
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "office:project-summary-bar".into(),
            ..SourceMeta::default()
        },
    });
}

fn push_milestone(page: &mut Page, id: impl Into<String>, x: f64, y: f64, summary: bool) {
    let radius = if summary { 4.5 } else { 4.0 };
    page.nodes.push(Node::Path {
        id: id.into(),
        d: format!(
            "M {x} {} L {} {y} L {x} {} L {} {y} Z",
            y - radius,
            x + radius,
            y + radius,
            x - radius
        ),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#DC2626"),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "office:project-milestone".into(),
            ..SourceMeta::default()
        },
    });
}

fn push_dependency(page: &mut Page, id: String, x1: f64, y1: f64, x2: f64, y2: f64) {
    let direction = if x2 >= x1 { 1.0 } else { -1.0 };
    let elbow = (x1 + direction * 6.0).clamp(AXIS_X, AXIS_X + AXIS_WIDTH);
    page.nodes.push(Node::Path {
        id: id.clone(),
        d: format!("M {x1} {y1} H {elbow} V {y2} H {x2}"),
        fill_rule: "nonzero".into(),
        fill: Paint::None,
        stroke: Stroke {
            paint: Paint::solid("#64748B"),
            width: 0.75,
            ..Stroke::default()
        },
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "office:project-dependency".into(),
            ..SourceMeta::default()
        },
    });
    let head = 3.0;
    page.nodes.push(Node::Path {
        id: format!("{id}-arrow"),
        d: format!(
            "M {x2} {y2} L {} {} L {} {} Z",
            x2 - direction * head,
            y2 - head,
            x2 - direction * head,
            y2 + head
        ),
        fill_rule: "nonzero".into(),
        fill: Paint::solid("#64748B"),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "office:project-dependency-arrow".into(),
            ..SourceMeta::default()
        },
    });
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.into());
    }
}

fn deduplicate(warnings: Vec<String>) -> Vec<String> {
    let mut unique = Vec::new();
    for warning in warnings {
        push_warning_once(&mut unique, &warning);
    }
    unique
}

fn has_project_namespace(start: &BytesStart<'_>) -> bool {
    start
        .attributes()
        .with_checks(false)
        .flatten()
        .any(|attribute| {
            let key = attribute.key.as_ref();
            if key != b"xmlns" && !key.starts_with(b"xmlns:") {
                return false;
            }
            attribute.value.as_ref() == PROJECT_XML_NAMESPACE.as_bytes()
                || attribute.value.as_ref() == PROJECT_XML_NAMESPACE_HTTPS.as_bytes()
        })
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_project_xml_root_and_namespace() {
        assert!(looks_like_project_xml_prefix(
            br#"<?xml version="1.0"?><Project xmlns="http://schemas.microsoft.com/project">"#
        ));
        assert!(looks_like_project_xml_prefix(
            br#"<p:Project xmlns:p="https://schemas.microsoft.com/project">"#
        ));
        assert!(!looks_like_project_xml_prefix(
            br#"<Project xmlns="urn:other">"#
        ));
    }

    #[test]
    fn parses_project_timestamps_with_or_without_offsets() {
        let utc = parse_project_timestamp("2026-09-01T08:00:00Z").unwrap();
        let offset = parse_project_timestamp("2026-09-01T10:00:00+02:00").unwrap();
        let naive = parse_project_timestamp("2026-09-01T08:00:00").unwrap();
        assert_eq!(utc, offset);
        assert_eq!(utc, naive);
        assert!(parse_project_timestamp("2026-02-30T08:00:00").is_none());
    }

    #[test]
    fn rejects_doctypes_and_external_entities() {
        let xml = br#"<!DOCTYPE Project [<!ENTITY external SYSTEM "file:///etc/passwd">]><Project xmlns="http://schemas.microsoft.com/project"><Name>&external;</Name><Tasks><Task><UID>1</UID><Name>Task</Name></Task></Tasks></Project>"#;
        assert!(matches!(
            parse_project(xml, MAX_PROJECT_XML_EVENTS),
            Err(Error::InvalidInput(message)) if message.contains("document type")
        ));
    }
}
