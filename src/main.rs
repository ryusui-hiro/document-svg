use std::io::Read;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use document_svg::{ConvertOptions, ReverseOptions, convert_path, svg_to_document};

const MAX_TRANSFORM_INPUT_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "docsvg",
    version,
    about = "Convert documents, comic archives, diagrams, CAD/CAM/3D, simulation, raster, chart, and math inputs to SVG pages; use 'reverse' to export SVG pages",
    after_help = "Inputs include PDF/FDF/XFDF form data (.fdf/.xfdf), legacy Word Binary (.doc; CFB .dot templates), legacy PowerPoint Binary text previews (.ppt), legacy Excel (.xls/.xlsb), SYLK/DIF spreadsheets (.slk/.dif), FASTA/FASTQ sequences (.fa/.fasta/.fq/.fastq), Newick phylogenetic trees (.nwk/.newick/.tree), Stockholm alignments (.sto/.stockholm), CLUSTAL alignments (.aln/.clustal/.clustalw), NEXUS trees (.nex/.nexus), GenBank flat files (.gb/.gbk/.genbank), EMBL-Bank flat files (.embl/.emb), UniProtKB/Swiss-Prot protein flat files (.dat/.uniprot/.swissprot), RIS bibliography (.ris), SPICE/ngspice netlists (.cir/.sp/.spice/.ckt/.net), KiCad legacy Eeschema schematics (.sch), KiCad 6+ S-expression schematics (.kicad_sch), LTspice schematics (.asc Version signature), Autodesk EAGLE XML schematics (.sch eagle signature), Microsoft Project XML (.xml/.mspdi), OOXML Transitional/Strict and OpenDocument Office files (including Visio .vsdx/.vsdm/.vstx/.vstm/.vdx), iCalendar (.ics), legacy vCalendar (.vcs), vCard contacts (.vcf), MIME e-mail (.eml), Apple Mail EMLX (.emlx), Outlook messages (.msg), MBOX archives (.mbox), MHTML archives (.mht/.mhtml), DocBook 4/5 (.dbk/.docbook), TEI P5 scholarly XML (.tei/.tei.xml), ALTO OCR/layout XML (.alto/.alto.xml), METS archive XML (.mets/.mets.xml), MARCXML (.marcxml/.marc.xml), MODS XML (.mods/.mods.xml), MARC21 ISO 2709 (.marc/.iso2709), PREMIS XML (.premis/.premis.xml), IIIF Presentation JSON (.iiif.json/.manifest.json), EAD finding-aid XML (.ead/.ead.xml), EAC-CPF authority XML (.eac-cpf/.eac), Dublin Core XML (.dc.xml/.dublin.xml), ISO 19115/19139 metadata XML (.iso19115/.iso19139/.gmd.xml), UBL business documents (.ubl/.ubl.xml), XBRL 2.1 instances (.xbrl/.xbrl.xml), LandXML civil models (.landxml/.landxml.xml), MathML 3 formulas (.mathml/.mml), XMP metadata (.xmp/.xmp.xml), XDP/XFA packages (.xdp/.xdp.xml), Excel SpreadsheetML 2003 (.spreadsheetml/.xmlss), S1000D Data Module XML (.s1000d/.dmodule), DITA topics/maps (.dita/.ditamap), PDB coordinate models (.pdb/.ent), HWPX (.hwpx), COLLADA (.dae), X3D (.x3d), XMind (.xmind), NIfTI (.nii/.nii.gz), FITS (.fits/.fit/.fts/.fits.gz), MRC (.mrc/.map/.mrc.gz), NetCDF classic (.nc/.nc3/.cdf), SQLite (.sqlite/.sqlite3/.db), mmCIF/PDBx (.cif/.mmcif), MOL2 (.mol2), RDF Turtle (.ttl/.nt/.nq), EPS/PostScript (.eps/.ps), FictionBook 2 e-books (.fb2/.fb2.zip), PalmDOC/MOBI (.mobi/.prc/.azw), XPS/OpenXPS, CBZ comic archives, multi-page TIFF/BigTIFF, DICOM images (.dcm/.dicom), DICOM Structured Reports (SR), Encapsulated PDF Storage, DICOMDIR file sets, and V2000 and core V3000 MOL/SDF chemical structures (.mol/.sdf/.sd) and RXN reaction diagrams (.rxn), raster PNG/JPEG/BMP/GIF/WebP/Netpbm, SubRip/WebVTT subtitles (.srt/.vtt), TTML/DFXP subtitles (.ttml/.dfxp), XLIFF localization files (.xlf/.xliff), Jupyter notebooks (.ipynb), Quarto/R Markdown (.qmd/.Rmd), HTML/EPUB/FictionBook 2 (.fb2/.fb2.zip)/PalmDOC MOBI (.mobi/.prc/.azw), Markdown/plain text/AsciiDoc/reStructuredText (.rst/.rest), Org-mode (.org), GNU gettext catalogs (.po/.pot), BibTeX bibliographies (.bib/.bibtex), draw.io, GraphML graphs (.graphml), BPMN 2.0 diagrams (.bpmn/.bpmn2), CMMN 1.1 case plans (.cmmn), DMN 1.1–1.5 decision tables (.dmn), ReqIF 1.0.1/1.2 requirements (.reqif), XMI 2.1–2.5 models (.xmi), DOT/Mermaid/PlantUML/D2/Excalidraw, CAD/CAM/3D, glTF/GLB (.gltf/.glb), ASCII XYZ (.xyz), PCD (.pcd), ASTM E57 (.e57), Leica PTS/PTX (.pts/.ptx), and ASPRS LAS/LAZ (.las/.laz) point clouds, ANSYS CDB (.cdb), Abaqus (.inp), LS-DYNA Keyword (.k/.key), MEDIT ASCII/binary (.mesh/.meshb), OFF (.off), IFC SPF (.ifc), IFCXML (.ifcxml), and IFCZIP (.ifczip), Nastran Bulk Data (.bdf/.nas), KiCad PCB (.kicad_pcb), UNV/UFF (.unv), SU2 CFD meshes (.su2), OpenFOAM polyMesh cases (.foam), Tecplot ASCII CAE meshes (.dat/.tec/.tecplot/.tp), EnSight Gold ASCII cases (.case/.geo), PLOT3D ASCII grids (.p3d/.plot3d/.p3), VRML97 meshes (.wrl/.vrml), Gmsh/VTK, NetCDF classic (.nc/.nc3/.cdf), CSV/TSV, ARFF datasets (.arff), JSON-LD (.jsonld), OpenAPI/Swagger (.openapi.json/.swagger.json/.openapi.yaml/.swagger.yaml), AsyncAPI (.asyncapi.json/.asyncapi.yaml/.asyncapi.yml), JSON Schema (.schema.json/.jsonschema/.schema.yaml/.schema.yml), HAR (.har), WARC (.warc/.warc.gz), WACZ (.wacz), Postman Collection v2.1 (.postman_collection.json), GraphQL SDL (.graphql/.graphqls/.gql), Protocol Buffers (.proto), Kubernetes manifests (.k8s.yaml/.k8s.yml/.kubernetes.yaml/.kube.yaml), Docker Compose (compose.yaml/compose.yml/docker-compose.yaml/docker-compose.yml), GitHub Actions workflows (.github/workflows/*.yaml|*.yml), JUnit XML reports (junit.xml/test-results.xml/TEST-*.xml), SARIF 2.1.0 (.sarif/.sarif.json), Terraform JSON plans (tfplan.json/.tfplan.json), CycloneDX BOMs (bom.json/bom.xml/*.cdx.json/*.cdx.xml), SPDX JSON/tag:value (spdx.json/*.spdx.json/.spdx), JaCoCo/Cobertura coverage (jacoco.xml/coverage.xml), LCOV tracefiles (.info/lcov.info), JSON Patch (.jsonpatch/.json-patch), JSON Merge Patch (.mergepatch/.json-merge-patch), CSL-JSON (.csl.json/.csl-json), JSON Feed (.jsonfeed/.json-feed), CloudEvents JSON (.cloudevent.json/.cloud-event.json/.cloudevents.json/.ce.json/.cloudevent/.cloudevents), FHIR JSON (.fhir.json/.fhirjson/.fhir/.bundle.fhir.json/.fhir-bundle.json), Avro JSON (.avsc/.avpr/.avro.json/.avro-schema.json), ASAM OpenDRIVE (.xodr/.opendrive), ASAM OpenCRG (.crg), ASAM OpenSCENARIO (.xosc/.openscenario), ASAM OpenLABEL (.openlabel.json/.openlabel), OGC CityJSON (.cityjson/.city.json), OASIS STIX JSON (.stix.json/.stix), OASIS TAXII JSON (.taxii.json/.taxii), WSDL (.wsdl), OPML (.opml/.opml.xml), Apple Property List (.plist/.plist.xml), JSON, TOML configuration (.toml), YAML 1.2 configuration (.yaml/.yml), Java Properties (.properties), generic XML fallback (.xml), and JSON Text Sequences (.jsons/.jsonseq/.jsonl), ESRI ASCII Grid (.asc), dBASE III/III+ tables (.dbf), GeoPackage (.gpkg), RFC 8142 GeoJSON Text Sequences (.geojsons/.geojsonseq/.geojsonl), TopoJSON (.topojson), GeoJSON/GeoRSS/RSS/Atom/GML/GPX/KML/KMZ/Shapefile (.shp) maps, WKT/EWKT geometry, charts, LaTeX, QR, SVG, and EMF/WMF.\nReverse: docsvg reverse INPUT.svg --output OUTPUT.ext\nINPUT may also be a directory containing SVG pages; outputs include Office, draw.io, CAD/CAM/3D, simulation, diagram, table, math, PNG, WebP, HTML, UI code, and Data URI formats."
)]
struct Cli {
    /// Source PDF/FDF/XFDF form data (.fdf/.xfdf), legacy Word binary (.doc/.dot), text-only legacy PowerPoint binary (.ppt), Microsoft Project XML (.xml/.mspdi), legacy Excel, OOXML Transitional/Strict, OpenDocument, and Visio XML, iCalendar/vCalendar/vCard, EML/EMLX/Outlook MSG/MBOX/MHTML, gettext PO/POT catalogs, BibTeX bibliographies, JSON Text Sequences, NetCDF classic datasets, ARFF datasets, FASTA/FASTQ sequences, SYLK/DIF spreadsheets, JSON-LD, OpenAPI/Swagger, AsyncAPI, JSON Schema, HAR, WARC, WACZ, Postman, GraphQL, Protocol Buffers, Kubernetes, Docker Compose and GitHub Actions workflow and JUnit report, SARIF, Terraform plan, CycloneDX BOM, SPDX, coverage, LCOV, JSON Patch, JSON Merge Patch, CSL-JSON, JSON Feed, CloudEvents JSON, FHIR JSON, Avro schema/protocol, ASAM OpenDRIVE, ASAM OpenCRG, ASAM OpenSCENARIO, ASAM OpenLABEL, OGC CityJSON and OASIS STIX, TAXII, WSDL, OPML, Apple Property List, TEI, ALTO, METS, MARCXML, MODS, MARC21, PREMIS, IIIF Presentation, EAD, EAC-CPF, Dublin Core, ISO 19115/19139, UBL, XBRL, LandXML, MathML, XMP, XDP/XFA, SpreadsheetML, CML, RDF/XML and S1000D documents, and GraphML graphs, SubRip/WebVTT/TTML subtitles and XLIFF localization files, ESRI ASCII Grid/dBASE III/III+ tables/GeoPackage/GeoJSON Text Sequence/TopoJSON/RSS/Atom/GeoRSS/GML/GPX/KML/KMZ/Shapefile/WKT maps, DICOM medical images, Structured Reports and DICOMDIR file sets, CBZ, TIFF, Jupyter/Quarto, reStructuredText/Org-mode, ASCII XYZ, PCD, ASTM E57, Leica PTS/PTX, or LAS/LAZ point clouds, Abaqus/LS-DYNA/MEDIT/OFF/IFC/IFCZIP/Nastran/SU2/OpenFOAM/Tecplot/EnSight/PLOT3D/VRML/KiCad PCB/glTF/GLB, web/text, diagram, CAD/CAM/3D, simulation, raster, chart, math, QR, or SVG file.
    input: PathBuf,

    /// Directory that receives page-NNNN.svg and conversion.json.
    #[arg(short, long)]
    output: PathBuf,

    /// PDF page worker count. Office XML stays page-streamed.
    #[arg(short = 'j', long, default_value_t = 1)]
    jobs: usize,

    /// Maximum input size in MiB.
    #[arg(long, default_value_t = 512)]
    max_input_mib: u64,

    /// Maximum expanded PDF stream or ZIP part/image (Office, EPUB, 3MF, CBZ) in MiB.
    #[arg(long, default_value_t = 128)]
    max_entry_mib: u64,

    /// Maximum number of output pages/slides/sheets.
    #[arg(long, default_value_t = 10_000)]
    max_pages: usize,

    /// Maximum XML-like parser events per input part.
    #[arg(long, default_value_t = 5_000_000)]
    max_xml_events: usize,

    /// Omit provenance metadata from SVG output.
    #[arg(long)]
    no_metadata: bool,

    /// Floating-point precision for SVG coordinates.
    #[arg(long, default_value_t = 5)]
    precision: usize,

    /// Outline embedded PDF fonts for maximum fidelity instead of editable text.
    #[arg(long)]
    outline_embedded_pdf_text: bool,

    /// Keep a copy of the draw.io source in each SVG so it can be opened as an
    /// editable diagram again, in draw.io or with `docsvg reverse`.
    #[arg(long)]
    embed_drawio_source: bool,

    /// draw.io stencil XML file or directory to draw shape libraries with.
    /// Repeat for more than one.
    #[arg(long = "stencils", value_name = "PATH")]
    stencil_paths: Vec<PathBuf>,
}

#[derive(Debug, Parser)]
#[command(
    name = "docsvg reverse",
    about = "Package SVG pages into Office, draw.io, CAD/CAM/3D, simulation, diagram, table, math, image, or UI-code formats"
)]
struct ReverseCli {
    /// Source SVG file or directory containing SVG pages.
    input: PathBuf,

    /// Destination Office, draw.io, CAD/CAM/3D, simulation, diagram, table, math, PNG, WebP, HTML, UI-code, or Data URI file.
    #[arg(short, long)]
    output: PathBuf,

    /// Maximum combined SVG input size in MiB.
    #[arg(long, default_value_t = 512)]
    max_input_mib: u64,

    /// Maximum number of SVG pages.
    #[arg(long, default_value_t = 10_000)]
    max_pages: usize,
}

#[derive(Debug, Parser)]
#[command(
    name = "docsvg transform",
    about = "Transform and optimize SVG files (minify, monochrome, responsive, clean paths, strip empty groups)"
)]
struct TransformCli {
    /// Source SVG file (maximum 512 MiB), or '-' for standard input.
    input: PathBuf,

    /// Destination transformed SVG file, or '-' for standard output.
    #[arg(short, long)]
    output: PathBuf,

    /// Remove comments, empty spaces, and minify SVG output.
    #[arg(long)]
    minify: bool,

    /// Unify all fill and stroke colors to a single monochrome color (e.g. "#000000").
    #[arg(long)]
    monochrome: Option<String>,

    /// Remove fixed width/height and ensure viewBox is present for responsive scaling.
    #[arg(long)]
    responsive: bool,

    /// Round coordinates and path numbers to N decimal digits.
    #[arg(long)]
    precision: Option<usize>,

    /// Remove <metadata>, <desc>, and data-* attributes.
    #[arg(long)]
    remove_metadata: bool,

    /// Clean and optimize SVG path data (remove redundant zero-length segments and duplicate closes).
    #[arg(long)]
    clean_paths: bool,

    /// Strip empty <g> groups that contain no child rendering elements.
    #[arg(long)]
    strip_empty_groups: bool,
}

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments.get(1).is_some_and(|arg| arg == "convert") {
        arguments.remove(1);
    } else if arguments.get(1).is_some_and(|arg| arg == "reverse") {
        arguments.remove(1);
        arguments[0] = "docsvg reverse".into();
        return reverse(ReverseCli::parse_from(arguments));
    } else if arguments.get(1).is_some_and(|arg| arg == "transform") {
        arguments.remove(1);
        arguments[0] = "docsvg transform".into();
        return transform(TransformCli::parse_from(arguments));
    }
    convert(Cli::parse_from(arguments))
}

fn convert(cli: Cli) -> anyhow::Result<()> {
    let options = ConvertOptions {
        max_input_bytes: cli.max_input_mib.saturating_mul(1024 * 1024),
        max_zip_entry_bytes: cli.max_entry_mib.saturating_mul(1024 * 1024),
        max_pages: cli.max_pages,
        max_xml_events: cli.max_xml_events,
        include_metadata: !cli.no_metadata,
        precision: cli.precision.min(12),
        jobs: cli.jobs,
        outline_embedded_pdf_text: cli.outline_embedded_pdf_text,
        embed_drawio_source: cli.embed_drawio_source,
        stencil_paths: cli.stencil_paths,
    };
    let report = convert_path(&cli.input, &cli.output, &options).with_context(|| {
        format!(
            "failed to convert {} into {}",
            cli.input.display(),
            cli.output.display()
        )
    })?;
    println!(
        "converted {} {} page(s) to {} in {} ms",
        report.source_format, report.page_count, report.output_directory, report.elapsed_ms
    );
    if !report.warnings.is_empty() {
        eprintln!(
            "completed with {} distinct warning(s); see conversion.json",
            report.warnings.len()
        );
    }
    Ok(())
}

fn reverse(cli: ReverseCli) -> anyhow::Result<()> {
    let options = ReverseOptions {
        max_input_bytes: cli.max_input_mib.saturating_mul(1024 * 1024),
        max_pages: cli.max_pages,
    };
    let report = svg_to_document(&cli.input, &cli.output, &options).with_context(|| {
        format!(
            "failed to package {} into {}",
            cli.input.display(),
            cli.output.display()
        )
    })?;
    println!(
        "packaged {} SVG page(s) into {} ({})",
        report.page_count, report.output, report.output_format
    );
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

fn transform(cli: TransformCli) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    let is_stdin = cli.input.as_os_str() == "-";
    let is_stdout = cli.output.as_os_str() == "-";

    if is_stdin {
        Read::take(
            &mut std::io::stdin(),
            MAX_TRANSFORM_INPUT_BYTES.saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .context("failed to read from standard input")?;
    } else {
        let mut file = std::fs::File::open(&cli.input)
            .with_context(|| format!("failed to open {}", cli.input.display()))?;
        Read::take(&mut file, MAX_TRANSFORM_INPUT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read {}", cli.input.display()))?;
    }

    if bytes.len() as u64 > MAX_TRANSFORM_INPUT_BYTES {
        anyhow::bail!("SVG transform input exceeds maximum bytes ({MAX_TRANSFORM_INPUT_BYTES})");
    }

    let options = document_svg::TransformOptions {
        minify: cli.minify,
        monochrome: cli.monochrome,
        responsive: cli.responsive,
        precision: cli.precision,
        remove_metadata: cli.remove_metadata,
        clean_paths: cli.clean_paths,
        strip_empty_groups: cli.strip_empty_groups,
    };
    let transformed = document_svg::transform_svg(&bytes, &options).with_context(|| {
        format!(
            "failed to transform {}",
            if is_stdin {
                "stdin".to_string()
            } else {
                cli.input.display().to_string()
            }
        )
    })?;

    if is_stdout {
        use std::io::Write;
        std::io::stdout().write_all(&transformed)?;
    } else {
        if let Some(parent) = cli.output.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&cli.output, &transformed)
            .with_context(|| format!("failed to write {}", cli.output.display()))?;
        println!(
            "transformed {} ({} bytes) -> {} ({} bytes)",
            cli.input.display(),
            bytes.len(),
            cli.output.display(),
            transformed.len()
        );
    }
    Ok(())
}
