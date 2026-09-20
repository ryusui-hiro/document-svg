//! Bounded EAC-CPF archival-authority previews.
//!
//! EAC-CPF describes corporate bodies, persons and families related to
//! archival materials. This adapter renders identity names and structural
//! relationship/date counts while keeping authority URIs and descriptions
//! inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_EAC_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EAC_XML_EVENTS: usize = 1_000_000;
const MAX_EAC_XML_NODES: usize = 500_000;
const MAX_EAC_XML_DEPTH: usize = 96;
const MAX_EAC_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_EAC_ROWS: usize = 200_000;
const MAX_EAC_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"eac-cpf", None);
    if !root {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<cpfdescription") || text.contains("<identity") || text.contains("<relations")
}

struct EacPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for EacPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "eac-cpf".into();
        if page.title.is_empty() {
            page.title = "EAC-CPF archival authority".into();
        }
        page.description =
            "EAC-CPF identity and relationship metadata is rendered inertly; authority URIs, descriptions and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    entity_type: String,
    names: usize,
    dates: usize,
    descriptions: usize,
    cpf_relations: usize,
    resource_relations: usize,
    function_relations: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_EAC_BYTES),
        "EAC-CPF input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_EAC_XML_EVENTS),
            max_nodes: MAX_EAC_XML_NODES,
            max_depth: MAX_EAC_XML_DEPTH,
            max_text_bytes: MAX_EAC_TEXT_BYTES,
        },
        "EAC-CPF",
    )?;
    if !root.name.eq_ignore_ascii_case("eac-cpf") {
        return Err(Error::InvalidInput("EAC-CPF XML root must eac-cpf".into()));
    }
    let mut summary = Summary::default();
    if let Some(description) = descendants_named(&root, "cpfDescription").first().copied() {
        if let Some(identity) = description.children_named("identity").next() {
            summary.entity_type = first_text(identity, "entityType");
            let entity_type = summary.entity_type.clone();
            for entry in identity.children_named("nameEntry") {
                summary.names = summary.names.saturating_add(1);
                let name = first_text(entry, "part");
                push_row(&mut summary, "name", &entity_type, &name, "identity")?;
            }
        }
        let dates = count_named(description, "existDates") + count_named(description, "date");
        summary.dates = dates;
        if dates > 0 {
            let entity_type = summary.entity_type.clone();
            push_row(
                &mut summary,
                "dates",
                &entity_type,
                &format!("{dates} date elements"),
                "metadata",
            )?;
        }
        summary.descriptions = count_named(description, "biogHist")
            + count_named(description, "description")
            + count_named(description, "history")
            + count_named(description, "occupation")
            + count_named(description, "function");
        if summary.descriptions > 0 {
            let entity_type = summary.entity_type.clone();
            let description_count = summary.descriptions;
            push_row(
                &mut summary,
                "description",
                &entity_type,
                &format!("{description_count} elements"),
                "text omitted",
            )?;
        }
    }
    summary.cpf_relations = count_named(&root, "cpfRelation");
    summary.resource_relations = count_named(&root, "resourceRelation");
    summary.function_relations = count_named(&root, "functionRelation");
    if summary.cpf_relations + summary.resource_relations + summary.function_relations > 0 {
        let relation_detail = format!(
            "CPF {}, resource {}, function {}",
            summary.cpf_relations, summary.resource_relations, summary.function_relations
        );
        push_row(
            &mut summary,
            "relations",
            "—",
            &relation_detail,
            "targets omitted",
        )?;
    }
    if summary.names == 0 && summary.entity_type.is_empty() {
        return Err(Error::InvalidInput(
            "EAC-CPF document contains no cpfDescription identity".into(),
        ));
    }
    let metadata = format!(
        "Entity type: {}\nNames: {}\nDate elements: {}\nDescription elements: {}\nCPF relations: {}\nResource relations: {}\nFunction relations: {}",
        display_or_dash(&summary.entity_type),
        summary.names,
        summary.dates,
        summary.descriptions,
        summary.cpf_relations,
        summary.resource_relations,
        summary.function_relations
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "EAC-CPF archival authority".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Kind".into(),
                "Type".into(),
                "Name/Count".into(),
                "Detail".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "EAC-CPF identity names and relationship/date counts are shown; authority IDs, URIs, biographies, descriptions and target resources are omitted or redacted".into(),
        "EAC-CPF XML traversal and rendered rows are bounded; external entities, XInclude, scripts and linked archival resources are never opened".into(),
    ];
    let mut page_sink = EacPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn count_named(element: &XmlElement, name: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| usize::from(child.name.eq_ignore_ascii_case(name)) + count_named(child, name))
        .sum()
}

fn descendants_named<'a>(element: &'a XmlElement, name: &str) -> Vec<&'a XmlElement> {
    let mut result = Vec::new();
    for child in &element.children {
        if child.name.eq_ignore_ascii_case(name) {
            result.push(child);
        }
        result.extend(descendants_named(child, name));
    }
    result
}

fn first_text(parent: &XmlElement, name: &str) -> String {
    parent
        .children_named(name)
        .next()
        .map(|child| safe_text(child.text.trim()))
        .unwrap_or_default()
}

fn safe_text(value: &str) -> String {
    if value.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(value)
    }
}

fn push_row(
    summary: &mut Summary,
    kind: &str,
    type_name: &str,
    value: &str,
    detail: &str,
) -> Result<()> {
    if summary.rows.len() >= MAX_EAC_ROWS {
        return Err(Error::LimitExceeded(format!(
            "EAC-CPF rendered rows exceed {MAX_EAC_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(kind),
        truncate(type_name),
        truncate(value),
        truncate(detail),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_EAC_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_EAC_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_eac_cpf_root() {
        assert!(looks_like_prefix(br#"<eac-cpf xmlns="urn:isbn:1-931666-33-7"><cpfDescription><identity><entityType>person</entityType><nameEntry><part>Ada</part></nameEntry></identity></cpfDescription></eac-cpf>"#));
        assert!(!looks_like_prefix(br#"<eac-cpf><control/></eac-cpf>"#));
    }

    #[test]
    fn counts_identity_names_and_relations() {
        let xml = br#"<eac-cpf><cpfDescription><identity><entityType>corporateBody</entityType><nameEntry><part>Archive</part></nameEntry></identity><description><existDates><date>1900</date></existDates><biogHist><p>private</p></biogHist></description></cpfDescription><relations><resourceRelation xlink:href="https://private.example.invalid/item"/></relations></eac-cpf>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "EAC-CPF",
        )
        .unwrap();
        let mut summary = Summary::default();
        let description = root.children_named("cpfDescription").next().unwrap();
        let identity = description.children_named("identity").next().unwrap();
        summary.entity_type = first_text(identity, "entityType");
        let entity_type = summary.entity_type.clone();
        for entry in identity.children_named("nameEntry") {
            summary.names += 1;
            push_row(
                &mut summary,
                "name",
                &entity_type,
                &first_text(entry, "part"),
                "identity",
            )
            .unwrap();
        }
        assert_eq!(summary.names, 1);
        assert_eq!(count_named(&root, "resourceRelation"), 1);
    }
}
