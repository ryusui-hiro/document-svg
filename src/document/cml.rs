//! Bounded Chemical Markup Language (CML) previews.
//!
//! CML represents molecules, reactions, spectra and related chemistry data in
//! XML. This adapter exposes molecule/atom/bond structure and element counts
//! without resolving dictionaries, conventions, URLs or performing chemistry.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_CML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CML_XML_EVENTS: usize = 1_000_000;
const MAX_CML_XML_NODES: usize = 500_000;
const MAX_CML_XML_DEPTH: usize = 128;
const MAX_CML_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_CML_ROWS: usize = 100_000;
const MAX_CML_DISPLAY_BYTES: usize = 512;
const CML_NAMESPACE: &str = "http://www.xml-cml.org/schema";

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"cml", None)
        && String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains("xml-cml.org/schema")
}

struct CmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for CmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "cml".into();
        if page.title.is_empty() {
            page.title = "CML chemical document".into();
        }
        page.description = "CML molecule/reaction metadata is rendered as a bounded inert summary; dictionaries, URLs and chemistry operations are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    molecules: usize,
    atoms: usize,
    bonds: usize,
    reactions: usize,
    spectra: usize,
    properties: usize,
    elements: BTreeMap<String, usize>,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_CML_BYTES),
        "CML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_CML_XML_EVENTS),
            max_nodes: MAX_CML_XML_NODES,
            max_depth: MAX_CML_XML_DEPTH,
            max_text_bytes: MAX_CML_TEXT_BYTES,
        },
        "CML",
    )?;
    if !root.name.eq_ignore_ascii_case("cml") {
        return Err(Error::InvalidInput("CML root must cml".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| namespace != CML_NAMESPACE)
    {
        return Err(Error::InvalidInput(
            "CML namespace is missing or unsupported".into(),
        ));
    }
    let mut summary = Summary {
        molecules: count_named(&root, "molecule"),
        atoms: count_named(&root, "atom"),
        bonds: count_named(&root, "bond"),
        reactions: count_named(&root, "reaction"),
        spectra: count_named(&root, "spectrum"),
        properties: count_named(&root, "property"),
        ..Summary::default()
    };
    for atom in descendants_named(&root, "atom") {
        if let Some(element) = atom.attribute("elementType") {
            *summary.elements.entry(element.to_owned()).or_default() += 1;
        }
    }
    for molecule in descendants_named(&root, "molecule")
        .into_iter()
        .take(MAX_CML_ROWS)
    {
        let name = molecule
            .attribute("title")
            .or_else(|| molecule.attribute("id"))
            .unwrap_or("[unnamed molecule]");
        let atoms = count_named(molecule, "atom");
        let bonds = count_named(molecule, "bond");
        push_row(
            &mut summary.rows,
            "Molecule",
            name,
            &format!("atoms={atoms} bonds={bonds}"),
        )?;
    }
    if summary.molecules == 0 && summary.reactions == 0 && summary.spectra == 0 {
        return Err(Error::InvalidInput(
            "CML document contains no molecule, reaction or spectrum structure".into(),
        ));
    }
    let distribution = summary
        .elements
        .iter()
        .map(|(element, count)| format!("{element}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    push_row(
        &mut summary.rows,
        "Document",
        "CML",
        &format!(
            "molecules={} reactions={} spectra={}",
            summary.molecules, summary.reactions, summary.spectra
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Atoms/bonds",
        &format!("{}/{}", summary.atoms, summary.bonds),
        "coordinates and bond payloads omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Elements",
        &distribution,
        "elementType counts",
    )?;
    push_row(
        &mut summary.rows,
        "Properties",
        &summary.properties.to_string(),
        "values/dictionaries omitted",
    )?;
    let metadata = format!(
        "Molecules: {}\nAtoms: {}\nBonds: {}\nReactions: {}\nSpectra: {}\nProperties: {}",
        summary.molecules,
        summary.atoms,
        summary.bonds,
        summary.reactions,
        summary.spectra,
        summary.properties
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "CML chemical document".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "CML molecule/reaction/spectrum structure, atom/bond counts and element distribution are shown; coordinates, charges, dictionaries, conventions, property values and URLs are omitted or redacted".into(),
        "CML XML traversal and rows are bounded; DTD/entities, external dictionaries/resources, reaction evaluation, geometry, valence repair and chemical calculation never run".into(),
    ];
    let mut page_sink = CmlPageSink {
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
fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_CML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "CML rows exceed {MAX_CML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_CML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_CML_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;
    #[test]
    fn recognizes_cml_namespace() {
        assert!(looks_like_prefix(
            br#"<cml xmlns="http://www.xml-cml.org/schema"><molecule/></cml>"#
        ));
    }
    #[test]
    fn rejects_generic_cml() {
        assert!(!looks_like_prefix(br#"<cml><molecule/></cml>"#));
    }
}
