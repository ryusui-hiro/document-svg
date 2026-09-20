//! Bounded Dassault Systèmes 3DXML package previews.
//!
//! A 3DXML exchange file is a ZIP container with a manifest, a product
//! structure XML document, and one or more 3DRep representations.  This
//! adapter follows only the local manifest/root XML relationship and renders
//! structural counts.  Binary tessellation, exact NURBS geometry, textures,
//! external files, and viewer automation remain inert.

use std::fs;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::ooxml::ZipPackage;
use crate::table::{TableAlign, TableData};

const MAX_3DXML_BYTES: u64 = 512 * 1024 * 1024;
const MAX_3DXML_XML_BYTES: u64 = 32 * 1024 * 1024;
const MAX_3DXML_EVENTS: usize = 1_000_000;
const MAX_3DXML_NODES: usize = 500_000;
const MAX_3DXML_DEPTH: usize = 128;
const MAX_3DXML_ROWS: usize = 200_000;
const MAX_3DXML_TARGET_BYTES: usize = 1024;

#[derive(Default)]
struct Summary {
    manifest: bool,
    root_target: String,
    product_structure: usize,
    reference_3d: usize,
    instance_3d: usize,
    reference_rep: usize,
    instance_rep: usize,
    reps: usize,
    geometry_nodes: usize,
    metadata_nodes: usize,
}

struct ThreeDXmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for ThreeDXmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "3dxml".into();
        if page.title.is_empty() {
            page.title = "Dassault 3DXML package".into();
        }
        page.description = "3DXML manifest and product-structure metadata are rendered as bounded inert rows; binary representations are not decoded".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_archive(path: &Path) -> bool {
    if fs::metadata(path)
        .ok()
        .is_none_or(|metadata| metadata.len() > MAX_3DXML_BYTES)
    {
        return false;
    }
    let Ok(mut package) = ZipPackage::open(path, MAX_3DXML_XML_BYTES) else {
        return false;
    };
    ["Manifest.xml", "_3DXML/Manifest.xml", "manifest.xml"]
        .into_iter()
        .any(|name| {
            package.contains(name)
                && package
                    .read_optional_limited(name, 64 * 1024)
                    .ok()
                    .flatten()
                    .is_some_and(|bytes| looks_like_manifest(&bytes))
        })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let metadata = fs::metadata(path)?;
    let max_bytes = options.max_input_bytes.min(MAX_3DXML_BYTES);
    if metadata.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "3DXML input exceeds maximum bytes ({max_bytes})"
        )));
    }
    let mut package = ZipPackage::open(path, options.max_zip_entry_bytes)?;
    let manifest_bytes = read_first_present(
        &mut package,
        &["Manifest.xml", "_3DXML/Manifest.xml", "manifest.xml"],
        MAX_3DXML_XML_BYTES,
    )?
    .ok_or_else(|| Error::InvalidInput("3DXML package has no Manifest.xml".into()))?;
    let manifest = parse_xml(&manifest_bytes, "3DXML manifest")?;
    let root_target = find_named_text(&manifest, "Root")
        .and_then(|value| safe_target(value.trim()))
        .ok_or_else(|| Error::InvalidInput("3DXML manifest has no safe Root target".into()))?;
    let root_bytes = package
        .read_limited(&root_target, MAX_3DXML_XML_BYTES)
        .or_else(|_| {
            let basename = root_target.rsplit('/').next().unwrap_or(&root_target);
            package.read_limited(basename, MAX_3DXML_XML_BYTES)
        })?;
    let root = parse_xml(&root_bytes, "3DXML product structure")?;
    let summary = Summary {
        manifest: true,
        root_target,
        product_structure: count_named(&root, "ProductStructure"),
        reference_3d: count_named(&root, "Reference3D"),
        instance_3d: count_named(&root, "Instance3D"),
        reference_rep: count_named(&root, "ReferenceRep"),
        instance_rep: count_named(&root, "InstanceRep"),
        reps: count_named(&root, "Rep")
            + count_named(&root, "Representation")
            + count_named(&root, "3DRep"),
        geometry_nodes: count_named(&root, "Polygon")
            + count_named(&root, "Triangle")
            + count_named(&root, "VertexBuffer"),
        metadata_nodes: count_named(&root, "PLMEntity") + count_named(&root, "UserRefProperties"),
    };
    let rows = vec![
        vec![
            "Manifest".into(),
            if summary.manifest {
                "present"
            } else {
                "missing"
            }
            .into(),
        ],
        vec!["Root target".into(), summary.root_target.clone()],
        vec![
            "ProductStructure".into(),
            summary.product_structure.to_string(),
        ],
        vec!["Reference3D".into(), summary.reference_3d.to_string()],
        vec!["Instance3D".into(), summary.instance_3d.to_string()],
        vec!["ReferenceRep".into(), summary.reference_rep.to_string()],
        vec!["InstanceRep".into(), summary.instance_rep.to_string()],
        vec!["Representations".into(), summary.reps.to_string()],
        vec![
            "Geometry markers".into(),
            summary.geometry_nodes.to_string(),
        ],
        vec![
            "Metadata markers".into(),
            summary.metadata_nodes.to_string(),
        ],
        vec!["ZIP entries".into(), package.entry_count().to_string()],
    ];
    if rows.len() > MAX_3DXML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "3DXML rows exceed {MAX_3DXML_ROWS}"
        )));
    }
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Dassault 3DXML package".into(),
        },
        HtmlBlock::Paragraph {
            text: "The local Manifest.xml → Root relationship is validated and the product structure is summarized without decoding binary 3DRep payloads or opening external resources.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Metric".into(), "Value".into()],
            rows,
            alignments: vec![TableAlign::Left, TableAlign::Right],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "3DXML tessellation, NURBS/exact geometry, transforms, materials, textures, annotations, and binary 3DRep data are not decoded".into(),
        "External files, URLs, scripts, plug-in automation, and viewer operations remain inert and are never opened".into(),
    ];
    let mut page_sink = ThreeDXmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn read_first_present(
    package: &mut ZipPackage<std::fs::File>,
    names: &[&str],
    max_bytes: u64,
) -> Result<Option<Vec<u8>>> {
    for name in names {
        if package.contains(name) {
            return package.read_limited(name, max_bytes).map(Some);
        }
    }
    Ok(None)
}

fn parse_xml(bytes: &[u8], context: &str) -> Result<XmlElement> {
    parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: MAX_3DXML_EVENTS,
            max_nodes: MAX_3DXML_NODES,
            max_depth: MAX_3DXML_DEPTH,
            max_text_bytes: MAX_3DXML_XML_BYTES as usize,
        },
        context,
    )
}

fn find_named_text<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    if element.name.eq_ignore_ascii_case(name) && !element.text.trim().is_empty() {
        return Some(element.text.trim());
    }
    element
        .children
        .iter()
        .find_map(|child| find_named_text(child, name))
}

fn safe_target(value: &str) -> Option<String> {
    let target = value
        .strip_prefix("urn:3DXML:")
        .or_else(|| value.strip_prefix("urn:3dxml:"))
        .unwrap_or(value)
        .replace('\\', "/");
    if target.is_empty()
        || target.len() > MAX_3DXML_TARGET_BYTES
        || target.contains('\0')
        || target
            .split('/')
            .any(|part| part == ".." || part.is_empty())
    {
        return None;
    }
    Some(target.trim_start_matches('/').to_owned())
}

fn looks_like_manifest(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.contains("3DXML")
        && (text.contains("<Manifest") || text.contains(":Manifest"))
        && (text.contains("<Root") || text.contains(":Root"))
}

fn count_named(element: &XmlElement, name: &str) -> usize {
    usize::from(element.name.eq_ignore_ascii_case(name))
        + element
            .children
            .iter()
            .map(|child| count_named(child, name))
            .sum::<usize>()
}
