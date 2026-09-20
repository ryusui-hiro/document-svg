use std::fs;
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Matrix, Node, Page, TextAnchor, compose};
use crate::svg::{SvgOptions, write_page};

const FORMAT_SNIFF_BYTES: u64 = 4096;

pub(crate) fn read_limited_file(path: &Path, max_bytes: u64, context: &str) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    Read::take(&mut file, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "{context} exceeds maximum bytes ({max_bytes})"
        )));
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceFormat {
    Pdf,
    #[serde(rename = "fdf")]
    Fdf,
    #[serde(rename = "xfdf")]
    Xfdf,
    #[serde(rename = "landxml")]
    LandXml,
    #[serde(rename = "mathml")]
    Mathml,
    #[serde(rename = "xmp")]
    Xmp,
    #[serde(rename = "xdp")]
    Xdp,
    #[serde(rename = "spreadsheetml")]
    Spreadsheetml,
    ProjectXml,
    Xml,
    Bpmn,
    Dmn,
    Cmmn,
    Reqif,
    Xmi,
    Ppt,
    Pptx,
    Xls,
    Xlsb,
    Xlsx,
    Doc,
    GeoJson,
    GeoJsonSeq,
    TopoJson,
    Geopackage,
    GeoRss,
    Feed,
    Gml,
    CityGml,
    CityJson,
    GraphGml,
    Gpx,
    Wkt,
    Kml,
    Kmz,
    Shapefile,
    Dbf,
    #[serde(rename = "esri_ascii_grid")]
    EsriAsciiGrid,
    #[serde(rename = "properties")]
    Properties,
    #[serde(rename = "plist")]
    Plist,
    #[serde(rename = "premis")]
    Premis,
    Docx,
    Drawio,
    Dxf,
    Gerber,
    #[serde(rename = "kicad_pcb")]
    KicadPcb,
    #[serde(rename = "kicad_sch_legacy")]
    KicadSchLegacy,
    #[serde(rename = "kicad_sch")]
    KicadSch,
    #[serde(rename = "ltspice_asc")]
    LtspiceAsc,
    #[serde(rename = "eagle_sch")]
    EagleSch,
    Hpgl,
    Dot,
    Mermaid,
    Markdown,
    Chart,
    Json,
    JsonLd,
    #[serde(rename = "iiif")]
    Iiif,
    JsonSeq,
    Graphml,
    Gexf,
    Xgmml,
    Toml,
    Yaml,
    Bib,
    Tex,
    Qr,
    Raster,
    Jpeg2000,
    Gcode,
    Excellon,
    Stl,
    Simulation,
    Gltf,
    Collada,
    X3d,
    Vrml,
    Step,
    Obj,
    Ply,
    Pcd,
    Pts,
    Ptx,
    Xyz,
    E57,
    Las,
    Unv,
    Csv,
    Emf,
    Excalidraw,
    PlantUml,
    D2,
    ThreeMf,
    Iges,
    Ifc,
    IfcXml,
    IfcZip,
    Html,
    Docbook,
    #[serde(rename = "dublin-core")]
    DublinCore,
    #[serde(rename = "iso19115")]
    Iso19115,
    #[serde(rename = "ubl")]
    Ubl,
    #[serde(rename = "xbrl")]
    Xbrl,
    Dif,
    Dita,
    #[serde(rename = "ead")]
    Ead,
    #[serde(rename = "eac-cpf")]
    EacCpf,
    Fasta,
    Fastq,
    Pdb,
    Hwpx,
    Xmind,
    Nifti,
    Fits,
    Gff3,
    Gtf,
    Mrc,
    Netcdf,
    Sqlite,
    Sylk,
    Cif,
    #[serde(rename = "cml")]
    Cml,
    #[serde(rename = "rdf-xml")]
    RdfXml,
    #[serde(rename = "bcfzip")]
    Bcfzip,
    #[serde(rename = "flat-opc")]
    FlatOpc,
    #[serde(rename = "aasx")]
    Aasx,
    #[serde(rename = "openscad")]
    OpenScad,
    #[serde(rename = "amf")]
    Amf,
    #[serde(rename = "plmxml")]
    PlmXml,
    #[serde(rename = "stepxml")]
    StepXml,
    #[serde(rename = "qif")]
    Qif,
    #[serde(rename = "b2mml")]
    B2mml,
    #[serde(rename = "jdf")]
    Jdf,
    #[serde(rename = "xjdf")]
    Xjdf,
    #[serde(rename = "tmx")]
    Tmx,
    #[serde(rename = "tbx")]
    Tbx,
    #[serde(rename = "gbxml")]
    GbXml,
    #[serde(rename = "idml")]
    Idml,
    #[serde(rename = "oaipmh")]
    OaiPmh,
    #[serde(rename = "onix")]
    Onix,
    #[serde(rename = "xpdl")]
    Xpdl,
    Mol2,
    Turtle,
    Eps,
    Epub,
    Fb2,
    Mobi,
    Eml,
    Emlx,
    Mbox,
    Mhtml,
    #[serde(rename = "mets")]
    Mets,
    #[serde(rename = "mods")]
    Mods,
    Msg,
    Ical,
    Vcalendar,
    Vcard,
    Vcf,
    Wig,
    Odt,
    Ods,
    Odp,
    Odg,
    Visio,
    Rtf,
    Xps,
    Tiff,
    Dicom,
    #[serde(rename = "dicom-sr")]
    DicomSr,
    DicomDir,
    Rxn,
    Mol,
    Sdf,
    Cbz,
    Abaqus,
    Nastran,
    #[serde(rename = "op2")]
    Op2,
    LsDyna,
    Jupyter,
    Jats,
    #[serde(rename = "tei")]
    Tei,
    Quarto,
    Medit,
    Off,
    Asciidoc,
    Arff,
    Bed,
    BedGraph,
    Rst,
    Org,
    Sam,
    #[serde(rename = "s1000d")]
    S1000d,
    Maf,
    #[serde(rename = "marcxml")]
    Marcxml,
    #[serde(rename = "marc21")]
    Marc,
    Po,
    Srt,
    Ttml,
    Vtt,
    Xliff,
    Svg,
    Su2,
    OpenFoam,
    OpenDrive,
    OpenCrg,
    OpenScenario,
    #[serde(rename = "openlabel")]
    OpenLabel,
    #[serde(rename = "openfoam-field")]
    OpenFoamField,
    Tecplot,
    Ensight,
    Plot3d,
    Newick,
    Stockholm,
    Clustal,
    Nexus,
    Genbank,
    Embl,
    Uniprot,
    Ris,
    Spice,
    #[serde(rename = "openapi")]
    Openapi,
    #[serde(rename = "asyncapi")]
    Asyncapi,
    #[serde(rename = "alto")]
    Alto,
    Wsdl,
    #[serde(rename = "opml")]
    Opml,
    #[serde(rename = "jsonschema")]
    JsonSchema,
    Cdb,
    Har,
    Warc,
    Wacz,
    Postman,
    Graphql,
    Protobuf,
    Kubernetes,
    Compose,
    #[serde(rename = "github-actions")]
    GithubActions,
    Junit,
    Sarif,
    #[serde(rename = "terraform-plan")]
    TerraformPlan,
    CycloneDx,
    Spdx,
    #[serde(rename = "stix-json")]
    StixJson,
    #[serde(rename = "taxii-json")]
    TaxiiJson,
    Coverage,
    #[serde(rename = "csl-json")]
    CslJson,
    Lcov,
    JsonPatch,
    JsonMergePatch,
    JsonFeed,
    #[serde(rename = "cloudevents")]
    CloudEvents,
    #[serde(rename = "fhir-json")]
    FhirJson,
    #[serde(rename = "fhir-xml")]
    FhirXml,
    #[serde(rename = "cda")]
    Cda,
    #[serde(rename = "iso20022")]
    Iso20022,
    #[serde(rename = "sbml")]
    Sbml,
    #[serde(rename = "cellml")]
    Cellml,
    #[serde(rename = "ocel-xml")]
    OcelXml,
    #[serde(rename = "energyplus-idf")]
    EnergyPlusIdf,
    #[serde(rename = "energyplus-epw")]
    EnergyPlusEpw,
    #[serde(rename = "rinex")]
    Rinex,
    #[serde(rename = "sat")]
    Sat,
    #[serde(rename = "sedml")]
    Sedml,
    #[serde(rename = "sbgnml")]
    Sbgnml,
    #[serde(rename = "omex")]
    Omex,
    #[serde(rename = "xdmf")]
    Xdmf,
    #[serde(rename = "pvd")]
    Pvd,
    #[serde(rename = "fds")]
    Fds,
    #[serde(rename = "abiword")]
    Abiword,
    #[serde(rename = "neuroml")]
    Neuroml,
    #[serde(rename = "biopax")]
    Biopax,
    #[serde(rename = "xsd")]
    Xsd,
    #[serde(rename = "xslt")]
    Xslt,
    #[serde(rename = "xsl-fo")]
    XslFo,
    #[serde(rename = "hdf5")]
    Hdf5,
    #[serde(rename = "cgns")]
    Cgns,
    #[serde(rename = "exodus")]
    Exodus,
    #[serde(rename = "iwork")]
    Iwork,
    #[serde(rename = "dwg")]
    Dwg,
    #[serde(rename = "3dm")]
    Rhino3dm,
    #[serde(rename = "access")]
    Access,
    #[serde(rename = "3dxml")]
    ThreeDXml,
    #[serde(rename = "ipc2581")]
    Ipc2581,
    #[serde(rename = "jt")]
    Jt,
    #[serde(rename = "vsd")]
    Vsd,
    #[serde(rename = "xproc")]
    Xproc,
    #[serde(rename = "wadl")]
    Wadl,
    #[serde(rename = "opensearch")]
    OpenSearch,
    #[serde(rename = "saml")]
    Saml,
    #[serde(rename = "xacml")]
    Xacml,
    Avro,
    #[serde(rename = "otlp-json")]
    OtlpJson,
    #[serde(rename = "ocel-json")]
    OcelJson,
    #[serde(rename = "json-api")]
    JsonApi,
}

#[cfg(not(target_arch = "wasm32"))]
impl SourceFormat {
    pub fn detect(path: &Path) -> Result<Self> {
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if filename.ends_with(".xjdf") {
            return Ok(Self::Xjdf);
        }
        if filename.ends_with(".idml") {
            return Ok(Self::Idml);
        }
        if filename.ends_with(".tmx") || filename.ends_with(".tmx.xml") {
            return Ok(Self::Tmx);
        }
        if filename.ends_with(".tbx") || filename.ends_with(".tbx.xml") {
            return Ok(Self::Tbx);
        }
        if filename.ends_with(".gbxml") || filename.ends_with(".gbxml.xml") {
            return Ok(Self::GbXml);
        }
        if filename.ends_with(".fhir.xml") || filename.ends_with(".fhirxml") {
            return Ok(Self::FhirXml);
        }
        if filename.ends_with(".oaipmh")
            || filename.ends_with(".oai-pmh")
            || filename.ends_with(".oai.xml")
        {
            return Ok(Self::OaiPmh);
        }
        if filename.ends_with(".onix")
            || filename.ends_with(".onix.xml")
            || filename.ends_with(".onix3")
        {
            return Ok(Self::Onix);
        }
        if filename.ends_with(".xpdl") || filename.ends_with(".xpdl.xml") {
            return Ok(Self::Xpdl);
        }
        if filename.ends_with(".cda")
            || filename.ends_with(".cda.xml")
            || filename.ends_with(".ccd.xml")
        {
            return Ok(Self::Cda);
        }
        if filename.ends_with(".iso20022.xml")
            || filename.ends_with(".pain.xml")
            || filename.ends_with(".pacs.xml")
            || filename.ends_with(".camt.xml")
        {
            return Ok(Self::Iso20022);
        }
        if filename.ends_with(".sbml") || filename.ends_with(".sbml.xml") {
            return Ok(Self::Sbml);
        }
        if filename.ends_with(".cellml") || filename.ends_with(".cellml.xml") {
            return Ok(Self::Cellml);
        }
        if filename.ends_with(".xmlocel") || filename.ends_with(".ocel.xml") {
            return Ok(Self::OcelXml);
        }
        if filename.ends_with(".idf") || filename.ends_with(".energyplus.idf") {
            return Ok(Self::EnergyPlusIdf);
        }
        if filename.ends_with(".epw") || filename.ends_with(".energyplus.epw") {
            return Ok(Self::EnergyPlusEpw);
        }
        if filename.ends_with(".rnx")
            || filename.ends_with(".rinex")
            || filename.ends_with(".obs")
            || filename.ends_with(".nav")
            || filename.ends_with(".gnav")
        {
            return Ok(Self::Rinex);
        }
        if filename.ends_with(".sat") {
            return Ok(Self::Sat);
        }
        if filename.ends_with(".sedml")
            || filename.ends_with(".sedml.xml")
            || filename.ends_with(".sed-ml.xml")
        {
            return Ok(Self::Sedml);
        }
        if filename.ends_with(".sbgn")
            || filename.ends_with(".sbgnml")
            || filename.ends_with(".sbgn.xml")
        {
            return Ok(Self::Sbgnml);
        }
        if filename.ends_with(".omex")
            || filename.ends_with(".omex.zip")
            || filename.ends_with(".combine")
        {
            return Ok(Self::Omex);
        }
        if filename.ends_with(".xdmf") || filename.ends_with(".xmf") {
            return Ok(Self::Xdmf);
        }
        if filename.ends_with(".pvd") {
            return Ok(Self::Pvd);
        }
        if filename.ends_with(".fds") {
            return Ok(Self::Fds);
        }
        if filename.ends_with(".abw") {
            return Ok(Self::Abiword);
        }
        if filename.ends_with(".nml")
            || filename.ends_with(".neuroml")
            || filename.ends_with(".neuroml.xml")
        {
            return Ok(Self::Neuroml);
        }
        if filename.ends_with(".biopax") || filename.ends_with(".biopax.xml") {
            return Ok(Self::Biopax);
        }
        if filename.ends_with(".xsd")
            || filename.ends_with(".xsd.xml")
            || filename.ends_with(".schema.xsd")
        {
            return Ok(Self::Xsd);
        }
        if filename.ends_with(".xsl")
            || filename.ends_with(".xslt")
            || filename.ends_with(".xsl.xml")
        {
            return Ok(Self::Xslt);
        }
        if filename.ends_with(".fo")
            || filename.ends_with(".xslfo")
            || filename.ends_with(".xsl-fo.xml")
            || filename.ends_with(".fo.xml")
        {
            return Ok(Self::XslFo);
        }
        if filename.ends_with(".xpl")
            || filename.ends_with(".xproc")
            || filename.ends_with(".xproc.xml")
        {
            return Ok(Self::Xproc);
        }
        if filename.ends_with(".wadl") || filename.ends_with(".wadl.xml") {
            return Ok(Self::Wadl);
        }
        if filename.ends_with(".osdd")
            || filename.ends_with(".opensearch")
            || filename.ends_with(".opensearchdescription.xml")
        {
            return Ok(Self::OpenSearch);
        }
        if filename.ends_with(".saml")
            || filename.ends_with(".saml.xml")
            || filename.ends_with(".saml-metadata.xml")
        {
            return Ok(Self::Saml);
        }
        if filename.ends_with(".xacml")
            || filename.ends_with(".xacml.xml")
            || filename.ends_with(".policy.xml")
        {
            return Ok(Self::Xacml);
        }

        let in_github_workflows = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("workflows"))
            && path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case(".github"));
        if in_github_workflows
            && path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| {
                    value.eq_ignore_ascii_case("yaml") || value.eq_ignore_ascii_case("yml")
                })
        {
            return Ok(Self::GithubActions);
        }
        if filename == "junit.xml"
            || filename == "test-results.xml"
            || filename == "test-report.xml"
            || filename.ends_with(".junit.xml")
            || filename.ends_with(".junit-report.xml")
            || filename.ends_with(".test-results.xml")
            || (filename.starts_with("test-") && filename.ends_with(".xml"))
        {
            return Ok(Self::Junit);
        }
        if filename == "jacoco.xml"
            || filename == "cobertura.xml"
            || filename == "coverage.xml"
            || filename.ends_with(".jacoco.xml")
            || filename.ends_with(".cobertura.xml")
            || filename.ends_with(".coverage.xml")
        {
            return Ok(Self::Coverage);
        }
        if filename == "lcov.info"
            || filename == "coverage.info"
            || filename.ends_with(".lcov.info")
        {
            return Ok(Self::Lcov);
        }
        if filename.ends_with(".foamfield") || filename.ends_with(".foam-field") {
            return Ok(Self::OpenFoamField);
        }
        if filename.ends_with(".csl.json")
            || filename.ends_with(".csl-json")
            || filename.ends_with(".cite.json")
        {
            return Ok(Self::CslJson);
        }
        if filename.ends_with(".jsonpatch")
            || filename.ends_with(".json-patch")
            || filename.ends_with(".patch.json")
        {
            return Ok(Self::JsonPatch);
        }
        if filename.ends_with(".mergepatch")
            || filename.ends_with(".json-merge-patch")
            || filename.ends_with(".merge-patch.json")
        {
            return Ok(Self::JsonMergePatch);
        }
        if filename.ends_with(".jsonfeed")
            || filename.ends_with(".json-feed")
            || filename.ends_with(".feed.json")
        {
            return Ok(Self::JsonFeed);
        }
        if filename.ends_with(".iiif.json")
            || filename.ends_with(".iiif-manifest.json")
            || filename.ends_with(".manifest.json")
        {
            return Ok(Self::Iiif);
        }
        if filename.ends_with(".cloudevent.json")
            || filename.ends_with(".cloud-event.json")
            || filename.ends_with(".cloudevents.json")
            || filename.ends_with(".ce.json")
            || filename.ends_with(".cloudevent")
            || filename.ends_with(".cloudevents")
        {
            return Ok(Self::CloudEvents);
        }
        if filename.ends_with(".fhir.json")
            || filename.ends_with(".fhirjson")
            || filename.ends_with(".fhir")
            || filename.ends_with(".bundle.fhir.json")
            || filename.ends_with(".fhir-bundle.json")
        {
            return Ok(Self::FhirJson);
        }
        if filename.ends_with(".avsc")
            || filename.ends_with(".avpr")
            || filename.ends_with(".avro.json")
            || filename.ends_with(".avro-schema.json")
        {
            return Ok(Self::Avro);
        }
        if filename.ends_with(".otlp.json")
            || filename.ends_with(".otlp-json")
            || filename.ends_with(".otel.json")
            || filename.ends_with(".otlp.trace.json")
            || filename.ends_with(".otlp.metrics.json")
            || filename.ends_with(".otlp.logs.json")
        {
            return Ok(Self::OtlpJson);
        }
        if filename.ends_with(".jsonocel")
            || filename.ends_with(".ocel.json")
            || filename.ends_with(".ocel-json")
        {
            return Ok(Self::OcelJson);
        }
        if filename.ends_with(".jsonapi")
            || filename.ends_with(".json-api.json")
            || filename.ends_with(".jsonapi.json")
        {
            return Ok(Self::JsonApi);
        }
        if filename.ends_with(".sarif") || filename.ends_with(".sarif.json") {
            return Ok(Self::Sarif);
        }
        if filename == "tfplan.json"
            || filename == "terraform-plan.json"
            || filename.ends_with(".tfplan.json")
            || filename.ends_with(".terraform-plan.json")
        {
            return Ok(Self::TerraformPlan);
        }
        if filename == "bom.json"
            || filename == "bom.xml"
            || filename.ends_with(".cdx.json")
            || filename.ends_with(".cdx.xml")
        {
            return Ok(Self::CycloneDx);
        }
        if filename == "spdx.json"
            || filename == "sbom.spdx.json"
            || filename.ends_with(".spdx.json")
            || filename == "spdx"
            || filename.ends_with(".spdx")
            || filename.ends_with(".spdx.txt")
        {
            return Ok(Self::Spdx);
        }
        if filename.ends_with(".stix.json")
            || filename.ends_with(".stix-json")
            || filename.ends_with(".stix")
        {
            return Ok(Self::StixJson);
        }
        if filename.ends_with(".taxii.json")
            || filename.ends_with(".taxii-json")
            || filename.ends_with(".taxii")
        {
            return Ok(Self::TaxiiJson);
        }
        if filename.ends_with(".opendrive.xml") || filename.ends_with(".open-drive.xml") {
            return Ok(Self::OpenDrive);
        }
        if filename.ends_with(".crg") || filename.ends_with(".opencrg") {
            return Ok(Self::OpenCrg);
        }
        if filename.ends_with(".openscenario.xml") || filename.ends_with(".open-scenario.xml") {
            return Ok(Self::OpenScenario);
        }
        if filename.ends_with(".openlabel.json")
            || filename.ends_with(".openlabel")
            || filename.ends_with(".open-label.json")
        {
            return Ok(Self::OpenLabel);
        }
        if filename.ends_with(".wsdl") || filename.ends_with(".wsdl.xml") {
            return Ok(Self::Wsdl);
        }
        if filename.ends_with(".opml") || filename.ends_with(".opml.xml") {
            return Ok(Self::Opml);
        }
        if filename.ends_with(".plist") || filename.ends_with(".plist.xml") {
            return Ok(Self::Plist);
        }
        if filename.ends_with(".premis") || filename.ends_with(".premis.xml") {
            return Ok(Self::Premis);
        }
        if filename.ends_with(".tei") || filename.ends_with(".tei.xml") {
            return Ok(Self::Tei);
        }
        if filename.ends_with(".ead") || filename.ends_with(".ead.xml") {
            return Ok(Self::Ead);
        }
        if filename.ends_with(".eac-cpf")
            || filename.ends_with(".eac-cpf.xml")
            || filename.ends_with(".eac")
        {
            return Ok(Self::EacCpf);
        }
        if filename.ends_with(".dublin-core.xml")
            || filename.ends_with(".dublin.xml")
            || filename.ends_with(".dc.xml")
        {
            return Ok(Self::DublinCore);
        }
        if filename.ends_with(".iso19115")
            || filename.ends_with(".iso19115.xml")
            || filename.ends_with(".iso19139")
            || filename.ends_with(".iso19139.xml")
            || filename.ends_with(".gmd.xml")
        {
            return Ok(Self::Iso19115);
        }
        if filename.ends_with(".ubl") || filename.ends_with(".ubl.xml") {
            return Ok(Self::Ubl);
        }
        if filename.ends_with(".xbrl")
            || filename.ends_with(".xbrl.xml")
            || filename.ends_with(".xbrli")
        {
            return Ok(Self::Xbrl);
        }
        if filename.ends_with(".fdf") {
            return Ok(Self::Fdf);
        }
        if filename.ends_with(".xfdf") || filename.ends_with(".xfdf.xml") {
            return Ok(Self::Xfdf);
        }
        if filename.ends_with(".landxml") || filename.ends_with(".landxml.xml") {
            return Ok(Self::LandXml);
        }
        if filename.ends_with(".mathml")
            || filename.ends_with(".mathml.xml")
            || filename.ends_with(".mml")
        {
            return Ok(Self::Mathml);
        }
        if filename.ends_with(".s1000d")
            || filename.ends_with(".dmodule")
            || filename.ends_with(".dmodule.xml")
        {
            return Ok(Self::S1000d);
        }
        if filename.ends_with(".alto") || filename.ends_with(".alto.xml") {
            return Ok(Self::Alto);
        }
        if filename.ends_with(".mets") || filename.ends_with(".mets.xml") {
            return Ok(Self::Mets);
        }
        if filename.ends_with(".marcxml") || filename.ends_with(".marc.xml") {
            return Ok(Self::Marcxml);
        }
        if filename.ends_with(".mods") || filename.ends_with(".mods.xml") {
            return Ok(Self::Mods);
        }
        if filename.ends_with(".marc") || filename.ends_with(".iso2709") {
            return Ok(Self::Marc);
        }
        if filename.ends_with(".citygml")
            || filename.ends_with(".citygml.xml")
            || filename.ends_with(".citygml.gml")
        {
            return Ok(Self::CityGml);
        }
        if filename.ends_with(".cityjson")
            || filename.ends_with(".city.json")
            || filename.ends_with(".cityjson.json")
        {
            return Ok(Self::CityJson);
        }

        if filename.ends_with(".chart.json") {
            return Ok(Self::Chart);
        } else if filename.ends_with(".fb2.zip") {
            return Ok(Self::Fb2);
        } else if filename.ends_with(".nii.gz") {
            return Ok(Self::Nifti);
        } else if filename.ends_with(".fits.gz") {
            return Ok(Self::Fits);
        } else if filename.ends_with(".mrc.gz") {
            return Ok(Self::Mrc);
        } else if filename.ends_with(".excalidraw.json") {
            return Ok(Self::Excalidraw);
        } else if filename.ends_with(".drawio.xml") || filename.ends_with(".dio.xml") {
            return Ok(Self::Drawio);
        } else if filename.ends_with(".openapi.json")
            || filename.ends_with(".swagger.json")
            || filename.ends_with(".openapi.yaml")
            || filename.ends_with(".openapi.yml")
            || filename.ends_with(".swagger.yaml")
            || filename.ends_with(".swagger.yml")
        {
            return Ok(Self::Openapi);
        } else if filename.ends_with(".asyncapi.json")
            || filename.ends_with(".asyncapi.yaml")
            || filename.ends_with(".asyncapi.yml")
        {
            return Ok(Self::Asyncapi);
        } else if filename.ends_with(".warc") || filename.ends_with(".warc.gz") {
            return Ok(Self::Warc);
        } else if filename.ends_with(".postman_collection.json") {
            return Ok(Self::Postman);
        } else if filename.ends_with(".graphql") || filename.ends_with(".graphqls") {
            return Ok(Self::Graphql);
        } else if filename.ends_with(".proto") {
            return Ok(Self::Protobuf);
        } else if filename.ends_with(".k8s.yaml")
            || filename.ends_with(".k8s.yml")
            || filename.ends_with(".kubernetes.yaml")
            || filename.ends_with(".kubernetes.yml")
            || filename.ends_with(".kube.yaml")
            || filename.ends_with(".kube.yml")
        {
            return Ok(Self::Kubernetes);
        } else if filename == "compose.yaml"
            || filename == "compose.yml"
            || filename == "docker-compose.yaml"
            || filename == "docker-compose.yml"
            || filename.ends_with(".compose.yaml")
            || filename.ends_with(".compose.yml")
        {
            return Ok(Self::Compose);
        } else if filename.ends_with(".schema.json")
            || filename.ends_with(".jsonschema")
            || filename.ends_with(".schema.yaml")
            || filename.ends_with(".schema.yml")
            || filename.ends_with(".jsonschema.yaml")
            || filename.ends_with(".jsonschema.yml")
        {
            return Ok(Self::JsonSchema);
        }
        if filename == "dicomdir" || filename.ends_with(".dicomdir") {
            return Ok(Self::DicomDir);
        }
        if filename.ends_with(".dicom-sr")
            || filename.ends_with(".dicom-sr.dcm")
            || filename.ends_with(".sr.dcm")
        {
            return Ok(Self::DicomSr);
        }

        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);

        if let Some(ref ext) = extension {
            match ext.as_str() {
                "pdf" => return Ok(Self::Pdf),
                "mspdi" => return Ok(Self::ProjectXml),
                "ppt" => return Ok(Self::Ppt),
                "pptx" | "pptm" | "potx" | "potm" | "ppsx" | "ppam" | "sldx" | "sldm" => {
                    return Ok(Self::Pptx);
                }
                "xlsx" | "xlsm" | "xltx" | "xltm" | "xlam" => return Ok(Self::Xlsx),
                "xlsb" => return Ok(Self::Xlsb),
                "xls" | "xlt" | "xla" => return Ok(Self::Xls),
                "accdb" | "accde" | "accdr" | "mdb" | "mde" | "mda" | "mdt" | "accdt" | "ade" => {
                    return Ok(Self::Access);
                }
                "doc" => return Ok(Self::Doc),
                // `.dot` is also Graphviz; identify an OLE Word template by
                // its WordDocument stream before falling back to Graphviz.
                "dot" => {
                    return Ok(
                        if crate::document::legacy_doc::looks_like_legacy_doc(path) {
                            Self::Doc
                        } else {
                            Self::Dot
                        },
                    );
                }
                "geojson" => {
                    let prefix = read_format_prefix(path)?;
                    return Ok(
                        if crate::geospatial::geojson_seq::looks_like_rfc8142_prefix(&prefix) {
                            Self::GeoJsonSeq
                        } else {
                            Self::GeoJson
                        },
                    );
                }
                "geojsons" | "geojsonseq" | "geojson-seq" | "geojsonl" => {
                    return Ok(Self::GeoJsonSeq);
                }
                "jsons" | "jsonseq" | "jsonl" => return Ok(Self::JsonSeq),
                "toml" => return Ok(Self::Toml),
                "yaml" | "yml" => {
                    let prefix = read_format_prefix(path)?;
                    return Ok(
                        if crate::document::kubernetes::looks_like_yaml_prefix(&prefix) {
                            Self::Kubernetes
                        } else if crate::document::compose::looks_like_yaml_prefix(&prefix) {
                            Self::Compose
                        } else if crate::document::github_actions::looks_like_yaml_prefix(&prefix) {
                            Self::GithubActions
                        } else if crate::document::asyncapi::looks_like_yaml_prefix(&prefix) {
                            Self::Asyncapi
                        } else if crate::document::jsonschema::looks_like_yaml_prefix(&prefix) {
                            Self::JsonSchema
                        } else if crate::document::openapi::looks_like_yaml_prefix(&prefix) {
                            Self::Openapi
                        } else {
                            Self::Yaml
                        },
                    );
                }
                "properties" => return Ok(Self::Properties),
                "opml" => return Ok(Self::Opml),
                "plist" => return Ok(Self::Plist),
                "premis" => return Ok(Self::Premis),
                "topojson" => return Ok(Self::TopoJson),
                "gpkg" => return Ok(Self::Geopackage),
                "rss" | "atom" | "georss" => {
                    let prefix = read_format_prefix(path)?;
                    return Ok(
                        if crate::geospatial::georss::looks_like_georss_prefix(&prefix) {
                            Self::GeoRss
                        } else {
                            Self::Feed
                        },
                    );
                }
                "gml" => {
                    let prefix = read_format_prefix(path)?;
                    return Ok(if crate::document::citygml::looks_like_prefix(&prefix) {
                        Self::CityGml
                    } else if crate::document::gml_graph::looks_like_prefix(&prefix) {
                        Self::GraphGml
                    } else {
                        Self::Gml
                    });
                }
                "gpx" => return Ok(Self::Gpx),
                "wkt" | "ewkt" => return Ok(Self::Wkt),
                "kml" => return Ok(Self::Kml),
                "kmz" => return Ok(Self::Kmz),
                "shp" => return Ok(Self::Shapefile),
                "dbf" => return Ok(Self::Dbf),
                "asc" => {
                    let prefix = read_format_prefix(path)?;
                    if crate::geospatial::ascii_grid::looks_like_prefix(&prefix) {
                        return Ok(Self::EsriAsciiGrid);
                    }
                    if crate::cad::ltspice::looks_like_prefix(&prefix) {
                        return Ok(Self::LtspiceAsc);
                    }
                    return Ok(Self::Asciidoc);
                }
                "docx" | "docm" | "dotx" | "dotm" => return Ok(Self::Docx),
                "vsd" => {
                    if crate::document::legacy_visio::looks_like_legacy_visio(path) {
                        return Ok(Self::Vsd);
                    }
                    return Err(Error::InvalidInput(
                        "VSD input is not a legacy Visio Compound File Binary document".into(),
                    ));
                }
                "vss" | "vst" | "vsw" => return Ok(Self::Vsd),
                "vsdx" | "vsdm" | "vstx" | "vstm" => return Ok(Self::Visio),
                "drawio" | "dio" => return Ok(Self::Drawio),
                "xml" => {
                    let prefix = read_format_prefix(path)?;
                    let filename = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_ascii_lowercase)
                        .unwrap_or_default();
                    if filename.ends_with(".reqif.xml") {
                        return Ok(Self::Reqif);
                    }
                    if crate::document::ipc2581::looks_like_prefix(&prefix) {
                        return Ok(Self::Ipc2581);
                    }
                    if crate::document::flatopc::looks_like_prefix(&prefix) {
                        return Ok(Self::FlatOpc);
                    }
                    if crate::document::fhir_xml::looks_like_prefix(&prefix) {
                        return Ok(Self::FhirXml);
                    }
                    if crate::document::tmx::looks_like_prefix(&prefix) {
                        return Ok(Self::Tmx);
                    }
                    if crate::document::tbx::looks_like_prefix(&prefix) {
                        return Ok(Self::Tbx);
                    }
                    if crate::document::gbxml::looks_like_prefix(&prefix) {
                        return Ok(Self::GbXml);
                    }
                    if crate::document::oaipmh::looks_like_prefix(&prefix) {
                        return Ok(Self::OaiPmh);
                    }
                    if crate::document::onix::looks_like_prefix(&prefix) {
                        return Ok(Self::Onix);
                    }
                    if crate::document::xpdl::looks_like_prefix(&prefix) {
                        return Ok(Self::Xpdl);
                    }
                    if crate::document::cda::looks_like_prefix(&prefix) {
                        return Ok(Self::Cda);
                    }
                    if crate::document::iso20022::looks_like_prefix(&prefix) {
                        return Ok(Self::Iso20022);
                    }
                    if crate::document::sbml::looks_like_prefix(&prefix) {
                        return Ok(Self::Sbml);
                    }
                    if crate::document::cellml::looks_like_prefix(&prefix) {
                        return Ok(Self::Cellml);
                    }
                    if crate::document::ocel_xml::looks_like_prefix(&prefix) {
                        return Ok(Self::OcelXml);
                    }
                    if crate::document::sedml::looks_like_prefix(&prefix) {
                        return Ok(Self::Sedml);
                    }
                    if crate::document::sbgnml::looks_like_prefix(&prefix) {
                        return Ok(Self::Sbgnml);
                    }
                    if crate::document::xdmf::looks_like_prefix(&prefix) {
                        return Ok(Self::Xdmf);
                    }
                    if crate::document::pvd::looks_like_prefix(&prefix) {
                        return Ok(Self::Pvd);
                    }
                    if crate::document::abiword::looks_like_prefix(&prefix) {
                        return Ok(Self::Abiword);
                    }
                    if crate::document::neuroml::looks_like_prefix(&prefix) {
                        return Ok(Self::Neuroml);
                    }
                    if crate::document::biopax::looks_like_prefix(&prefix) {
                        return Ok(Self::Biopax);
                    }
                    if crate::document::xsd::looks_like_prefix(&prefix) {
                        return Ok(Self::Xsd);
                    }
                    if crate::document::xslt::looks_like_prefix(&prefix) {
                        return Ok(Self::Xslt);
                    }
                    if crate::document::xslfo::looks_like_prefix(&prefix) {
                        return Ok(Self::XslFo);
                    }
                    if crate::document::xproc::looks_like_prefix(&prefix) {
                        return Ok(Self::Xproc);
                    }
                    if crate::document::wadl::looks_like_prefix(&prefix) {
                        return Ok(Self::Wadl);
                    }
                    if crate::document::opensearch::looks_like_prefix(&prefix) {
                        return Ok(Self::OpenSearch);
                    }
                    if crate::document::saml::looks_like_prefix(&prefix) {
                        return Ok(Self::Saml);
                    }
                    if crate::document::xacml::looks_like_prefix(&prefix) {
                        return Ok(Self::Xacml);
                    }
                    if crate::document::cyclonedx::looks_like_xml_prefix(&prefix) {
                        return Ok(Self::CycloneDx);
                    }
                    if crate::document::msproject::looks_like_project_xml_prefix(&prefix) {
                        return Ok(Self::ProjectXml);
                    }
                    if crate::document::junit::looks_like_prefix(&prefix) {
                        return Ok(Self::Junit);
                    }
                    if crate::document::coverage::looks_like_prefix(&prefix) {
                        return Ok(Self::Coverage);
                    }
                    return Self::detect_from_content(path);
                }
                "info" | "lcov" => {
                    let prefix = read_format_prefix(path)?;
                    if crate::document::lcov::looks_like_prefix(&prefix) {
                        return Ok(Self::Lcov);
                    }
                }
                "bpmn" | "bpmn2" => return Ok(Self::Bpmn),
                "dmn" => return Ok(Self::Dmn),
                "cmmn" => return Ok(Self::Cmmn),
                "reqif" => return Ok(Self::Reqif),
                "xmi" => return Ok(Self::Xmi),
                "vdx" => return Ok(Self::Visio),
                "dxf" => return Ok(Self::Dxf),
                "cir" | "sp" | "spice" | "ckt" | "net" => return Ok(Self::Spice),
                "kicad_pcb" => return Ok(Self::KicadPcb),
                "kicad_sch" => return Ok(Self::KicadSch),
                "sch" => {
                    let prefix = read_format_prefix(path)?;
                    if crate::cad::kicad_sch::looks_like_prefix(&prefix) {
                        return Ok(Self::KicadSchLegacy);
                    }
                    if crate::cad::eagle::looks_like_prefix(&prefix) {
                        return Ok(Self::EagleSch);
                    }
                }
                "gbr" | "gerber" | "gtl" | "gbl" | "gts" | "gbs" | "gto" | "gbo" | "gko"
                | "gm1" | "gm2" | "art" | "pho" | "cmp" | "sol" | "sts" => {
                    return Ok(Self::Gerber);
                }
                "stc" => {
                    return Ok(
                        if crate::document::ods::looks_like_legacy_calc_archive(path) {
                            Self::Ods
                        } else {
                            Self::Gerber
                        },
                    );
                }
                "plt" | "hpgl" | "hpg" | "gl2" | "prn" => return Ok(Self::Hpgl),
                "gcode" | "ngc" | "tap" | "gco" | "cnc" => return Ok(Self::Gcode),
                "drl" | "drd" | "xln" | "exc" => return Ok(Self::Excellon),
                "stl" => return Ok(Self::Stl),
                "gltf" | "glb" => return Ok(Self::Gltf),
                "3dm" => return Ok(Self::Rhino3dm),
                "jt" => return Ok(Self::Jt),
                "3dxml" => return Ok(Self::ThreeDXml),
                "ipc2581" | "ipc-2581" | "cvg" => return Ok(Self::Ipc2581),
                "dae" => return Ok(Self::Collada),
                "x3d" => return Ok(Self::X3d),
                "wrl" | "vrml" => return Ok(Self::Vrml),
                "ply" => return Ok(Self::Ply),
                "pcd" => return Ok(Self::Pcd),
                "pts" => return Ok(Self::Pts),
                "ptx" => return Ok(Self::Ptx),
                "xyz" => return Ok(Self::Xyz),
                "e57" => return Ok(Self::E57),
                "las" | "laz" => return Ok(Self::Las),
                "3mf" => return Ok(Self::ThreeMf),
                "ifc" => {
                    let prefix = read_format_prefix(path)?;
                    return Ok(if crate::cad::ifc::looks_like_ifcxml_prefix(&prefix) {
                        Self::IfcXml
                    } else {
                        Self::Ifc
                    });
                }
                "ifcxml" => return Ok(Self::IfcXml),
                "ifczip" => return Ok(Self::IfcZip),
                "step" | "stp" | "p21" | "stpnc" => return Ok(Self::Step),
                "iges" | "igs" => return Ok(Self::Iges),
                "obj" => return Ok(Self::Obj),
                "unv" => return Ok(Self::Unv),
                "su2" => return Ok(Self::Su2),
                "foam" => return Ok(Self::OpenFoam),
                "case" => return Ok(Self::Ensight),
                "geo" => {
                    let prefix = read_format_prefix(path)?;
                    if crate::cad::simulation::looks_like_ensight_prefix(&prefix) {
                        return Ok(Self::Ensight);
                    }
                }
                // `.dat` is used by many unrelated binary/text formats; only
                // claim it after the conservative Tecplot signature check.
                "dat" => {
                    let prefix = read_format_prefix(path)?;
                    if crate::document::uniprot::looks_like_prefix(&prefix) {
                        return Ok(Self::Uniprot);
                    }
                    if crate::cad::simulation::looks_like_tecplot_prefix(&prefix) {
                        return Ok(Self::Tecplot);
                    }
                }
                "tec" | "tecplot" | "tp" => return Ok(Self::Tecplot),
                "p3d" | "plot3d" | "p3" => return Ok(Self::Plot3d),
                "msh" | "vtk" | "vtu" | "vtp" | "vti" | "vtr" | "vts" => {
                    return Ok(Self::Simulation);
                }
                "cgns" => return Ok(Self::Cgns),
                "h5" | "hdf5" | "hdf" | "h5part" => return Ok(Self::Hdf5),
                "exo" | "ex2" | "ex2m" | "exii" => return Ok(Self::Exodus),
                "dwg" => return Ok(Self::Dwg),
                "e" => {
                    let prefix = read_format_prefix(path)?;
                    if crate::document::netcdf::looks_like_prefix(&prefix) {
                        return Ok(Self::Exodus);
                    }
                }
                "gv" => return Ok(Self::Dot),
                "mmd" | "mermaid" => return Ok(Self::Mermaid),
                "puml" | "plantuml" | "iuml" | "pu" | "wsd" => return Ok(Self::PlantUml),
                "d2" => return Ok(Self::D2),
                "excalidraw" => return Ok(Self::Excalidraw),
                "html" | "htm" | "xhtml" => return Ok(Self::Html),
                "dbk" | "docbook" => return Ok(Self::Docbook),
                "dc" | "dublin" | "dublincore" => return Ok(Self::DublinCore),
                "dif" => return Ok(Self::Dif),
                "fa" | "fasta" | "fna" | "faa" => return Ok(Self::Fasta),
                "fq" | "fastq" => return Ok(Self::Fastq),
                "dita" | "ditamap" => return Ok(Self::Dita),
                "ead" => return Ok(Self::Ead),
                "eac" | "eac-cpf" => return Ok(Self::EacCpf),
                "pdb" | "ent" => return Ok(Self::Pdb),
                "hwpx" => return Ok(Self::Hwpx),
                "xmind" => return Ok(Self::Xmind),
                "nii" => return Ok(Self::Nifti),
                "fits" | "fit" | "fts" => return Ok(Self::Fits),
                "gff" | "gff3" => return Ok(Self::Gff3),
                "gtf" => return Ok(Self::Gtf),
                "mrc" | "map" => return Ok(Self::Mrc),
                "nc3" | "cdf" => return Ok(Self::Netcdf),
                "nc" => {
                    let prefix = read_format_prefix(path)?;
                    return Ok(if crate::document::netcdf::looks_like_prefix(&prefix) {
                        Self::Netcdf
                    } else {
                        Self::Gcode
                    });
                }
                "sqlite" | "sqlite3" | "db" => return Ok(Self::Sqlite),
                "slk" => return Ok(Self::Sylk),
                "cif" | "mmcif" => return Ok(Self::Cif),
                "cml" => return Ok(Self::Cml),
                "rdf" => return Ok(Self::RdfXml),
                "bcfzip" => return Ok(Self::Bcfzip),
                "flatopc" | "fopc" => return Ok(Self::FlatOpc),
                "aasx" => return Ok(Self::Aasx),
                "scad" => return Ok(Self::OpenScad),
                "amf" => return Ok(Self::Amf),
                "plmxml" => return Ok(Self::PlmXml),
                "stepxml" | "stpx" => return Ok(Self::StepXml),
                "qif" => return Ok(Self::Qif),
                "b2mml" => return Ok(Self::B2mml),
                "jdf" | "jmf" => return Ok(Self::Jdf),
                "xjdf" => return Ok(Self::Xjdf),
                "mol2" => return Ok(Self::Mol2),
                "ttl" | "nt" | "nq" => return Ok(Self::Turtle),
                "eps" | "ps" => return Ok(Self::Eps),
                "eml" => return Ok(Self::Eml),
                "emlx" => return Ok(Self::Emlx),
                "mbox" => return Ok(Self::Mbox),
                "mht" | "mhtml" => return Ok(Self::Mhtml),
                "mets" => return Ok(Self::Mets),
                "msg" => return Ok(Self::Msg),
                "ics" => return Ok(Self::Ical),
                "vcs" => return Ok(Self::Vcalendar),
                "vcf" => {
                    let prefix = read_format_prefix(path)?;
                    return Ok(if crate::document::vcf::looks_like_prefix(&prefix) {
                        Self::Vcf
                    } else {
                        Self::Vcard
                    });
                }
                "wig" | "wiggle" => return Ok(Self::Wig),
                "vcard" => return Ok(Self::Vcard),
                "rtf" => return Ok(Self::Rtf),
                "xps" | "oxps" | "dwfx" => return Ok(Self::Xps),
                "tif" | "tiff" => return Ok(Self::Tiff),
                "jp2" | "j2k" | "j2c" | "jpc" | "jpx" => return Ok(Self::Jpeg2000),
                "dicom-sr" | "sr" => return Ok(Self::DicomSr),
                "dcm" | "dicom" => {
                    let prefix = read_format_prefix(path)?;
                    return Ok(
                        if crate::document::dicom::looks_like_dicomdir_prefix(&prefix) {
                            Self::DicomDir
                        } else if crate::document::dicom::looks_like_dicom_sr_prefix(&prefix) {
                            Self::DicomSr
                        } else {
                            Self::Dicom
                        },
                    );
                }
                "mol" => return Ok(Self::Mol),
                "sdf" | "sd" => return Ok(Self::Sdf),
                "rxn" => return Ok(Self::Rxn),
                "cbz" => return Ok(Self::Cbz),
                "cdb" => return Ok(Self::Cdb),
                "har" => return Ok(Self::Har),
                "warc" => return Ok(Self::Warc),
                "wacz" => return Ok(Self::Wacz),
                "postman_collection" => return Ok(Self::Postman),
                "graphql" | "graphqls" | "gql" => return Ok(Self::Graphql),
                "proto" => return Ok(Self::Protobuf),
                "inp" => return Ok(Self::Abaqus),
                "op2" => return Ok(Self::Op2),
                "bdf" | "nas" | "nastran" => return Ok(Self::Nastran),
                "k" => return Ok(Self::LsDyna),
                "key" => {
                    let prefix = read_format_prefix(path)?;
                    return Ok(if prefix.starts_with(b"PK\x03\x04") {
                        Self::Iwork
                    } else {
                        Self::LsDyna
                    });
                }
                "pages" | "numbers" => return Ok(Self::Iwork),
                "ipynb" => return Ok(Self::Jupyter),
                "jats" | "nxml" => return Ok(Self::Jats),
                "tei" => return Ok(Self::Tei),
                "alto" => return Ok(Self::Alto),
                "qmd" | "rmd" | "rmarkdown" => return Ok(Self::Quarto),
                "meshb" => return Ok(Self::Medit),
                "medit" => return Ok(Self::Medit),
                "off" => return Ok(Self::Off),
                "epub" => return Ok(Self::Epub),
                "fb2" => return Ok(Self::Fb2),
                "mobi" | "prc" | "azw" => return Ok(Self::Mobi),
                "odt" | "ott" | "fodt" | "sxw" | "stw" | "sxg" => return Ok(Self::Odt),
                "ods" | "ots" | "fods" | "sxc" => return Ok(Self::Ods),
                "odp" | "otp" | "fodp" | "sxi" | "sti" => return Ok(Self::Odp),
                "odg" | "otg" | "fodg" | "sxd" | "std" => return Ok(Self::Odg),
                "md" | "markdown" | "mdown" | "mkd" | "mkdn" | "mdwn" | "mdx" | "txt" | "text" => {
                    return Ok(Self::Markdown);
                }
                "adoc" | "asciidoc" => return Ok(Self::Asciidoc),
                "arff" => return Ok(Self::Arff),
                "bed" | "bed3" | "bed4" | "bed5" | "bed6" | "bed9" | "bed12" => {
                    return Ok(Self::Bed);
                }
                "bedgraph" | "bg" => return Ok(Self::BedGraph),
                "rst" | "rest" => return Ok(Self::Rst),
                "sam" => return Ok(Self::Sam),
                "iso19115" | "iso19139" | "gmd" => return Ok(Self::Iso19115),
                "ubl" => return Ok(Self::Ubl),
                "xbrl" | "xbrli" => return Ok(Self::Xbrl),
                "fdf" => return Ok(Self::Fdf),
                "xfdf" => return Ok(Self::Xfdf),
                "landxml" => return Ok(Self::LandXml),
                "mathml" | "mml" => return Ok(Self::Mathml),
                "xmp" => return Ok(Self::Xmp),
                "xdp" => return Ok(Self::Xdp),
                "spreadsheetml" | "xmlss" => return Ok(Self::Spreadsheetml),
                "s1000d" | "dmodule" => return Ok(Self::S1000d),
                "maf" => return Ok(Self::Maf),
                "marcxml" => return Ok(Self::Marcxml),
                "marc" | "iso2709" => return Ok(Self::Marc),
                "mods" => return Ok(Self::Mods),
                "nwk" | "newick" | "tree" => return Ok(Self::Newick),
                "sto" | "stockholm" => return Ok(Self::Stockholm),
                "aln" | "clustal" | "clustalw" => return Ok(Self::Clustal),
                "nex" | "nexus" => return Ok(Self::Nexus),
                "gb" | "gbk" | "genbank" => return Ok(Self::Genbank),
                "embl" | "emb" => return Ok(Self::Embl),
                "uniprot" | "swissprot" => return Ok(Self::Uniprot),
                "ris" => return Ok(Self::Ris),
                "org" => return Ok(Self::Org),
                "po" | "pot" => return Ok(Self::Po),
                "bib" | "bibtex" => return Ok(Self::Bib),
                "srt" => return Ok(Self::Srt),
                "vtt" => return Ok(Self::Vtt),
                "ttml" | "dfxp" => return Ok(Self::Ttml),
                "xlf" | "xliff" => return Ok(Self::Xliff),
                "csv" | "tsv" | "tab" => return Ok(Self::Csv),
                "emf" | "wmf" => return Ok(Self::Emf),
                "svgz" => return Ok(Self::Svg),
                "chart" => return Ok(Self::Chart),
                "tex" | "latex" | "ltx" => return Ok(Self::Tex),
                "qr" | "qrcode" => return Ok(Self::Qr),
                "png" | "jpg" | "jpeg" | "jpe" | "jfif" | "bmp" | "dib" | "gif" | "webp"
                | "pbm" | "pgm" | "ppm" | "pnm" | "pam" => {
                    return Ok(Self::Raster);
                }
                "svg" => return Ok(Self::Svg),
                "xodr" | "opendrive" => return Ok(Self::OpenDrive),
                "crg" | "opencrg" => return Ok(Self::OpenCrg),
                "xosc" | "openscenario" => return Ok(Self::OpenScenario),
                "openlabel" => return Ok(Self::OpenLabel),
                "wsdl" => return Ok(Self::Wsdl),
                "json" => {
                    let bytes = read_format_prefix(path)?;
                    let text = String::from_utf8_lossy(&bytes);
                    if crate::document::postman::looks_like_prefix(&bytes) {
                        return Ok(Self::Postman);
                    }
                    if crate::document::kubernetes::looks_like_json_prefix(&bytes) {
                        return Ok(Self::Kubernetes);
                    }
                    if crate::document::compose::looks_like_json_prefix(&bytes) {
                        return Ok(Self::Compose);
                    }
                    if crate::document::sarif::looks_like_prefix(&bytes) {
                        return Ok(Self::Sarif);
                    }
                    if crate::document::terraform::looks_like_prefix(&bytes) {
                        return Ok(Self::TerraformPlan);
                    }
                    if crate::document::cyclonedx::looks_like_json_prefix(&bytes) {
                        return Ok(Self::CycloneDx);
                    }
                    if crate::document::spdx::looks_like_prefix(&bytes) {
                        return Ok(Self::Spdx);
                    }
                    if crate::document::stix::looks_like_prefix(&bytes) {
                        return Ok(Self::StixJson);
                    }
                    if crate::document::taxii::looks_like_prefix(&bytes) {
                        return Ok(Self::TaxiiJson);
                    }
                    if crate::document::jsonpatch::looks_like_prefix(&bytes) {
                        return Ok(Self::JsonPatch);
                    }
                    if crate::document::jsonfeed::looks_like_prefix(&bytes) {
                        return Ok(Self::JsonFeed);
                    }
                    if crate::document::iiif::looks_like_prefix(&bytes) {
                        return Ok(Self::Iiif);
                    }
                    if crate::document::csl_json::looks_like_prefix(&bytes) {
                        return Ok(Self::CslJson);
                    }
                    if crate::document::cloudevents::looks_like_prefix(&bytes) {
                        return Ok(Self::CloudEvents);
                    }
                    if crate::document::fhir::looks_like_prefix(&bytes) {
                        return Ok(Self::FhirJson);
                    }
                    if crate::document::avro::looks_like_prefix(&bytes) {
                        return Ok(Self::Avro);
                    }
                    if crate::document::otlp::looks_like_prefix(&bytes) {
                        return Ok(Self::OtlpJson);
                    }
                    if crate::document::ocel::looks_like_prefix(&bytes) {
                        return Ok(Self::OcelJson);
                    }
                    if crate::document::jsonapi::looks_like_prefix(&bytes) {
                        return Ok(Self::JsonApi);
                    }
                    if crate::document::openlabel::looks_like_prefix(&bytes) {
                        return Ok(Self::OpenLabel);
                    }
                    if crate::document::cityjson::looks_like_prefix(&bytes) {
                        return Ok(Self::CityJson);
                    }
                    if crate::document::har::looks_like_prefix(&bytes) {
                        return Ok(Self::Har);
                    }
                    if crate::document::asyncapi::looks_like_json_prefix(&bytes) {
                        return Ok(Self::Asyncapi);
                    }
                    if crate::document::jsonschema::looks_like_json_prefix(&bytes) {
                        return Ok(Self::JsonSchema);
                    }
                    if crate::document::openapi::looks_like_json_prefix(&bytes) {
                        return Ok(Self::Openapi);
                    }
                    if crate::geospatial::topojson::looks_like_topojson_prefix(&bytes) {
                        return Ok(Self::TopoJson);
                    }
                    if crate::geospatial::geojson_seq::looks_like_rfc8142_prefix(&bytes) {
                        return Ok(Self::GeoJsonSeq);
                    }
                    if crate::document::json_seq::looks_like_prefix(&bytes) {
                        return Ok(Self::JsonSeq);
                    }
                    if crate::document::jsonld::looks_like_prefix(&bytes) {
                        return Ok(Self::JsonLd);
                    }
                    if looks_like_geojson(&bytes) {
                        return Ok(Self::GeoJson);
                    }
                    if looks_like_chart_json_text(&text) {
                        return Ok(Self::Chart);
                    }
                    if looks_like_excalidraw_json_text(&text) {
                        return Ok(Self::Excalidraw);
                    }
                    return Ok(Self::Json);
                }
                "jsonld" | "json-ld" => return Ok(Self::JsonLd),
                "iiif" => return Ok(Self::Iiif),
                "graphml" => return Ok(Self::Graphml),
                "gexf" => return Ok(Self::Gexf),
                "xgmml" => return Ok(Self::Xgmml),
                _ => {}
            }
        }

        // Content-based sniffing fallback when extension is missing, unknown, or generic
        Self::detect_from_content(path)
    }

    fn detect_from_content(path: &Path) -> Result<Self> {
        if crate::document::hwpx::looks_like_hwpx_archive(path) {
            return Ok(Self::Hwpx);
        }
        if crate::document::bcf::looks_like_archive(path) {
            return Ok(Self::Bcfzip);
        }
        if crate::document::aasx::looks_like_archive(path) {
            return Ok(Self::Aasx);
        }
        if crate::document::idml::looks_like_archive(path) {
            return Ok(Self::Idml);
        }
        if crate::document::iwork::looks_like_archive(path) {
            return Ok(Self::Iwork);
        }
        if crate::document::threedxml::looks_like_archive(path) {
            return Ok(Self::ThreeDXml);
        }
        if crate::document::omex::looks_like_archive(path) {
            return Ok(Self::Omex);
        }
        if crate::document::xmind::looks_like_xmind_archive(path) {
            return Ok(Self::Xmind);
        }
        if crate::document::wacz::looks_like_archive(path) {
            return Ok(Self::Wacz);
        }
        let buf = read_format_prefix(path)?;

        if crate::document::hdf5::looks_like_file(path) {
            return Ok(Self::Hdf5);
        }
        if crate::document::dwg::looks_like_prefix(&buf) {
            return Ok(Self::Dwg);
        }
        if crate::document::rhino3dm::looks_like_prefix(&buf) {
            return Ok(Self::Rhino3dm);
        }
        if crate::document::jt::looks_like_prefix(&buf) {
            return Ok(Self::Jt);
        }
        if crate::document::access::looks_like_prefix(&buf) {
            return Ok(Self::Access);
        }
        if crate::document::flatopc::looks_like_prefix(&buf) {
            return Ok(Self::FlatOpc);
        }
        if crate::document::ipc2581::looks_like_prefix(&buf) {
            return Ok(Self::Ipc2581);
        }
        if crate::document::openscad::looks_like_prefix(&buf) {
            return Ok(Self::OpenScad);
        }
        if crate::cad::amf::looks_like_prefix(&buf) {
            return Ok(Self::Amf);
        }
        if crate::document::plmxml::looks_like_prefix(&buf) {
            return Ok(Self::PlmXml);
        }
        if crate::document::stepxml::looks_like_prefix(&buf) {
            return Ok(Self::StepXml);
        }
        if crate::document::qif::looks_like_prefix(&buf) {
            return Ok(Self::Qif);
        }
        if crate::document::b2mml::looks_like_prefix(&buf) {
            return Ok(Self::B2mml);
        }
        if crate::document::jdf::looks_like_prefix(&buf) {
            return Ok(if crate::document::jdf::looks_like_xjdf_prefix(&buf) {
                Self::Xjdf
            } else {
                Self::Jdf
            });
        }
        if crate::document::fhir_xml::looks_like_prefix(&buf) {
            return Ok(Self::FhirXml);
        }
        if crate::document::tmx::looks_like_prefix(&buf) {
            return Ok(Self::Tmx);
        }
        if crate::document::tbx::looks_like_prefix(&buf) {
            return Ok(Self::Tbx);
        }
        if crate::document::gbxml::looks_like_prefix(&buf) {
            return Ok(Self::GbXml);
        }
        if crate::document::oaipmh::looks_like_prefix(&buf) {
            return Ok(Self::OaiPmh);
        }
        if crate::document::onix::looks_like_prefix(&buf) {
            return Ok(Self::Onix);
        }
        if crate::document::xpdl::looks_like_prefix(&buf) {
            return Ok(Self::Xpdl);
        }
        if crate::document::cda::looks_like_prefix(&buf) {
            return Ok(Self::Cda);
        }
        if crate::document::iso20022::looks_like_prefix(&buf) {
            return Ok(Self::Iso20022);
        }
        if crate::document::sbml::looks_like_prefix(&buf) {
            return Ok(Self::Sbml);
        }
        if crate::document::cellml::looks_like_prefix(&buf) {
            return Ok(Self::Cellml);
        }
        if crate::document::ocel_xml::looks_like_prefix(&buf) {
            return Ok(Self::OcelXml);
        }
        if crate::document::energyplus::looks_like_idf_prefix(&buf) {
            return Ok(Self::EnergyPlusIdf);
        }
        if crate::document::energyplus::looks_like_epw_prefix(&buf) {
            return Ok(Self::EnergyPlusEpw);
        }
        if crate::document::rinex::looks_like_prefix(&buf) {
            return Ok(Self::Rinex);
        }
        if crate::cad::sat::looks_like_prefix(&buf) {
            return Ok(Self::Sat);
        }
        if crate::document::sedml::looks_like_prefix(&buf) {
            return Ok(Self::Sedml);
        }
        if crate::document::sbgnml::looks_like_prefix(&buf) {
            return Ok(Self::Sbgnml);
        }
        if crate::document::xdmf::looks_like_prefix(&buf) {
            return Ok(Self::Xdmf);
        }
        if crate::document::pvd::looks_like_prefix(&buf) {
            return Ok(Self::Pvd);
        }
        if crate::document::abiword::looks_like_prefix(&buf) {
            return Ok(Self::Abiword);
        }
        if crate::document::neuroml::looks_like_prefix(&buf) {
            return Ok(Self::Neuroml);
        }
        if crate::document::biopax::looks_like_prefix(&buf) {
            return Ok(Self::Biopax);
        }
        if crate::document::xsd::looks_like_prefix(&buf) {
            return Ok(Self::Xsd);
        }
        if crate::document::xslt::looks_like_prefix(&buf) {
            return Ok(Self::Xslt);
        }
        if crate::document::xslfo::looks_like_prefix(&buf) {
            return Ok(Self::XslFo);
        }
        if crate::document::xproc::looks_like_prefix(&buf) {
            return Ok(Self::Xproc);
        }
        if crate::document::wadl::looks_like_prefix(&buf) {
            return Ok(Self::Wadl);
        }
        if crate::document::opensearch::looks_like_prefix(&buf) {
            return Ok(Self::OpenSearch);
        }
        if crate::document::saml::looks_like_prefix(&buf) {
            return Ok(Self::Saml);
        }
        if crate::document::xacml::looks_like_prefix(&buf) {
            return Ok(Self::Xacml);
        }
        if crate::document::fds::looks_like_prefix(&buf) {
            return Ok(Self::Fds);
        }

        if crate::document::plist::looks_like_prefix(&buf) {
            return Ok(Self::Plist);
        }
        if crate::document::premis::looks_like_prefix(&buf) {
            return Ok(Self::Premis);
        }
        if crate::cad::openfoam_field::looks_like_prefix(&buf) {
            return Ok(Self::OpenFoamField);
        }
        if crate::document::nifti::looks_like_prefix(&buf) {
            return Ok(Self::Nifti);
        }
        if crate::document::fits::looks_like_prefix(&buf) {
            return Ok(Self::Fits);
        }
        if crate::document::gff::looks_like_gff3_prefix(&buf) {
            return Ok(Self::Gff3);
        }
        if crate::document::gff::looks_like_gtf_prefix(&buf) {
            return Ok(Self::Gtf);
        }
        if crate::document::sam::looks_like_prefix(&buf) {
            return Ok(Self::Sam);
        }
        if crate::document::maf::looks_like_prefix(&buf) {
            return Ok(Self::Maf);
        }
        if crate::document::newick::looks_like_prefix(&buf) {
            return Ok(Self::Newick);
        }
        if crate::document::stockholm::looks_like_prefix(&buf) {
            return Ok(Self::Stockholm);
        }
        if crate::document::clustal::looks_like_prefix(&buf) {
            return Ok(Self::Clustal);
        }
        if crate::document::nexus::looks_like_prefix(&buf) {
            return Ok(Self::Nexus);
        }
        if crate::document::genbank::looks_like_prefix(&buf) {
            return Ok(Self::Genbank);
        }
        if crate::document::uniprot::looks_like_prefix(&buf) {
            return Ok(Self::Uniprot);
        }
        if crate::document::ris::looks_like_prefix(&buf) {
            return Ok(Self::Ris);
        }
        if crate::cad::spice::looks_like_prefix(&buf) {
            return Ok(Self::Spice);
        }
        if crate::document::embl::looks_like_prefix(&buf) {
            return Ok(Self::Embl);
        }
        if crate::document::mrc::looks_like_prefix(&buf) {
            return Ok(Self::Mrc);
        }
        if crate::document::netcdf::looks_like_prefix(&buf) {
            return Ok(Self::Netcdf);
        }
        if crate::geospatial::geopackage::looks_like_geopackage_prefix(&buf) {
            return Ok(Self::Geopackage);
        }
        if crate::document::sqlite::looks_like_prefix(&buf) {
            return Ok(Self::Sqlite);
        }
        if crate::document::sylk::looks_like_prefix(&buf) {
            return Ok(Self::Sylk);
        }
        if crate::document::dif::looks_like_prefix(&buf) {
            return Ok(Self::Dif);
        }
        if crate::document::fastx::looks_like_fastq_prefix(&buf) {
            return Ok(Self::Fastq);
        }
        if crate::document::fastx::looks_like_fasta_prefix(&buf) {
            return Ok(Self::Fasta);
        }
        if crate::document::vcf::looks_like_prefix(&buf) {
            return Ok(Self::Vcf);
        }
        if crate::document::wig::looks_like_prefix(&buf) {
            return Ok(Self::Wig);
        }
        if crate::document::cif::looks_like_prefix(&buf) {
            return Ok(Self::Cif);
        }
        if crate::document::cml::looks_like_prefix(&buf) {
            return Ok(Self::Cml);
        }
        if crate::document::mol2::looks_like_prefix(&buf) {
            return Ok(Self::Mol2);
        }
        if crate::document::turtle::looks_like_prefix(&buf) {
            return Ok(Self::Turtle);
        }
        if crate::document::eps::looks_like_prefix(&buf) {
            return Ok(Self::Eps);
        }

        if crate::document::molfile::looks_like_rxn_prefix(&buf) {
            return Ok(Self::Rxn);
        }

        if let Some(format) = crate::document::molfile::looks_like_prefix(&buf) {
            return Ok(format);
        }

        if crate::cad::collada::looks_like_prefix(&buf) {
            return Ok(Self::Collada);
        }
        if crate::cad::x3d::looks_like_prefix(&buf) {
            return Ok(Self::X3d);
        }
        if crate::cad::vrml::looks_like_prefix(&buf) {
            return Ok(Self::Vrml);
        }

        if crate::geospatial::topojson::looks_like_topojson_prefix(&buf) {
            return Ok(Self::TopoJson);
        }
        if crate::geospatial::geojson_seq::looks_like_rfc8142_prefix(&buf) {
            return Ok(Self::GeoJsonSeq);
        }

        if crate::document::json_seq::looks_like_prefix(&buf) {
            return Ok(Self::JsonSeq);
        }
        if crate::document::har::looks_like_prefix(&buf) {
            return Ok(Self::Har);
        }
        if crate::document::warc::looks_like_prefix(&buf)
            || (buf.starts_with(b"\x1f\x8b") && crate::document::warc::looks_like_gzip_file(path))
        {
            return Ok(Self::Warc);
        }
        if crate::document::postman::looks_like_prefix(&buf) {
            return Ok(Self::Postman);
        }
        if crate::document::compose::looks_like_json_prefix(&buf) {
            return Ok(Self::Compose);
        }
        if crate::document::sarif::looks_like_prefix(&buf) {
            return Ok(Self::Sarif);
        }
        if crate::document::terraform::looks_like_prefix(&buf) {
            return Ok(Self::TerraformPlan);
        }
        if crate::document::cyclonedx::looks_like_json_prefix(&buf) {
            return Ok(Self::CycloneDx);
        }
        if crate::document::spdx::looks_like_prefix(&buf) {
            return Ok(Self::Spdx);
        }
        if crate::document::stix::looks_like_prefix(&buf) {
            return Ok(Self::StixJson);
        }
        if crate::document::taxii::looks_like_prefix(&buf) {
            return Ok(Self::TaxiiJson);
        }
        if crate::document::spdx::looks_like_tag_prefix(&buf) {
            return Ok(Self::Spdx);
        }
        if crate::document::protobuf::looks_like_prefix(&buf) {
            return Ok(Self::Protobuf);
        }
        if crate::document::graphql::looks_like_prefix(&buf) {
            return Ok(Self::Graphql);
        }
        if crate::document::openapi::looks_like_json_prefix(&buf) {
            return Ok(Self::Openapi);
        }
        if crate::document::asyncapi::looks_like_json_prefix(&buf) {
            return Ok(Self::Asyncapi);
        }
        if crate::document::jsonschema::looks_like_json_prefix(&buf) {
            return Ok(Self::JsonSchema);
        }
        if crate::document::arff::looks_like_prefix(&buf) {
            return Ok(Self::Arff);
        }
        if crate::document::bed::looks_like_bedgraph_prefix(&buf) {
            return Ok(Self::BedGraph);
        }
        if crate::document::bed::looks_like_bed_prefix(&buf) {
            return Ok(Self::Bed);
        }
        if crate::document::jsonld::looks_like_prefix(&buf) {
            return Ok(Self::JsonLd);
        }
        if crate::document::graphml::looks_like_prefix(&buf) {
            return Ok(Self::Graphml);
        }
        if crate::document::gexf::looks_like_prefix(&buf) {
            return Ok(Self::Gexf);
        }
        if crate::document::xgmml::looks_like_prefix(&buf) {
            return Ok(Self::Xgmml);
        }

        if crate::document::msproject::looks_like_project_xml_prefix(&buf) {
            return Ok(Self::ProjectXml);
        }

        if crate::document::bpmn::looks_like_prefix(&buf) {
            return Ok(Self::Bpmn);
        }
        if crate::document::dmn::looks_like_prefix(&buf) {
            return Ok(Self::Dmn);
        }
        if crate::document::cmmn::looks_like_prefix(&buf) {
            return Ok(Self::Cmmn);
        }
        if crate::document::reqif::looks_like_prefix(&buf) {
            return Ok(Self::Reqif);
        }
        if crate::document::xmi::looks_like_prefix(&buf) {
            return Ok(Self::Xmi);
        }

        if crate::cad::ifc::looks_like_ifcxml_prefix(&buf) {
            return Ok(Self::IfcXml);
        }

        if looks_like_geojson(&buf) {
            return Ok(Self::GeoJson);
        }

        if buf.starts_with(b"\x1f\x8b")
            && let Ok(file) = std::fs::File::open(path)
        {
            let mut decoder = flate2::read::MultiGzDecoder::new(file);
            let mut prefix = Vec::with_capacity(FORMAT_SNIFF_BYTES as usize);
            if std::io::Read::take(&mut decoder, FORMAT_SNIFF_BYTES)
                .read_to_end(&mut prefix)
                .is_ok()
            {
                let text = String::from_utf8_lossy(&prefix);
                let trimmed = text.trim_start();
                if trimmed.starts_with("<svg")
                    || (trimmed.starts_with("<?xml") && trimmed.contains("<svg"))
                {
                    return Ok(Self::Svg);
                }
            }
        }

        if buf.starts_with(b"%PDF-") {
            return Ok(Self::Pdf);
        }
        if crate::document::fdf::looks_like_prefix(&buf) {
            return Ok(Self::Fdf);
        }
        if crate::document::xfdf::looks_like_prefix(&buf) {
            return Ok(Self::Xfdf);
        }
        if crate::document::landxml::looks_like_prefix(&buf) {
            return Ok(Self::LandXml);
        }
        if crate::document::mathml::looks_like_prefix(&buf) {
            return Ok(Self::Mathml);
        }
        if crate::document::xmp::looks_like_prefix(&buf) {
            return Ok(Self::Xmp);
        }
        if crate::document::xdp::looks_like_prefix(&buf) {
            return Ok(Self::Xdp);
        }
        if crate::document::spreadsheetml::looks_like_prefix(&buf) {
            return Ok(Self::Spreadsheetml);
        }
        if buf.starts_with(b"glTF") {
            return Ok(Self::Gltf);
        }
        if buf.starts_with(b"LASF") {
            return Ok(Self::Las);
        }
        if crate::document::dicom::looks_like_dicomdir_prefix(&buf) {
            return Ok(Self::DicomDir);
        }
        if crate::document::dicom::looks_like_dicom_sr_prefix(&buf) {
            return Ok(Self::DicomSr);
        }
        if crate::document::dicom::looks_like_dicom_prefix(&buf) {
            return Ok(Self::Dicom);
        }
        if crate::geospatial::shapefile::looks_like_shapefile_prefix(&buf) {
            return Ok(Self::Shapefile);
        }
        if crate::geospatial::dbase::looks_like_prefix(&buf) {
            return Ok(Self::Dbf);
        }
        if crate::geospatial::ascii_grid::looks_like_prefix(&buf) {
            return Ok(Self::EsriAsciiGrid);
        }
        if crate::cad::ltspice::looks_like_prefix(&buf) {
            return Ok(Self::LtspiceAsc);
        }
        if crate::cad::simulation::looks_like_cdb_prefix(&buf) {
            return Ok(Self::Cdb);
        }
        if crate::cad::simulation::looks_like_su2_prefix(&buf) {
            return Ok(Self::Su2);
        }
        if crate::cad::simulation::looks_like_tecplot_prefix(&buf) {
            return Ok(Self::Tecplot);
        }
        if crate::cad::simulation::looks_like_ensight_prefix(&buf) {
            return Ok(Self::Ensight);
        }
        if crate::cad::simulation::looks_like_plot3d_prefix(&buf) {
            return Ok(Self::Plot3d);
        }
        if buf.starts_with(crate::cad::dxf::binary::BINARY_DXF_SENTINEL) {
            return Ok(Self::Dxf);
        }
        if buf.starts_with(b"II*\0")
            || buf.starts_with(b"MM\0*")
            || buf.starts_with(b"II+\0\x08\0\0\0")
            || buf.starts_with(b"MM\0+\0\x08\0\0")
        {
            return Ok(Self::Tiff);
        }
        if crate::jpeg2000::looks_like_prefix(&buf) {
            return Ok(Self::Jpeg2000);
        }
        if crate::vectorize::is_pnm(&buf) {
            return Ok(Self::Raster);
        }
        if buf.starts_with(b"\x89PNG\r\n\x1a\n")
            || buf.starts_with(b"\xff\xd8\xff")
            || buf.starts_with(b"BM")
            || crate::vectorize::is_gif(&buf)
            || crate::vectorize::is_webp(&buf)
        {
            return Ok(Self::Raster);
        }
        if buf.starts_with(b"PK\x03\x04")
            && let Ok(mut archive) =
                zip::ZipArchive::new(std::io::BufReader::new(std::fs::File::open(path)?))
        {
            if archive.len() > crate::ooxml::MAX_ZIP_PACKAGE_ENTRIES {
                return Err(Error::LimitExceeded(format!(
                    "ZIP package contains {} entries; maximum is {}",
                    archive.len(),
                    crate::ooxml::MAX_ZIP_PACKAGE_ENTRIES
                )));
            }
            let mut has_word = false;
            let mut has_ppt = false;
            let mut has_xl = false;
            let mut has_xlsb = false;
            let mut has_visio = false;
            let mut has_fb2 = false;
            let mut has_3d = false;
            let mut has_epub = false;
            let mut has_kml = false;
            let mut has_ifczip = false;
            let has_xps = if let Ok(mut relationships) = archive.by_name("_rels/.rels") {
                let mut bytes = Vec::new();
                std::io::Read::take(&mut relationships, 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .is_ok()
                    && bytes.len() <= 1024 * 1024
                    && String::from_utf8_lossy(&bytes)
                        .to_ascii_lowercase()
                        .contains("fixedrepresentation")
            } else {
                false
            };
            if has_xps {
                return Ok(Self::Xps);
            }
            for i in 0..archive.len().min(10_000) {
                if let Ok(entry) = archive.by_index(i) {
                    let name = entry.name();
                    if name == "word/document.xml" {
                        has_word = true;
                    } else if name == "ppt/presentation.xml" {
                        has_ppt = true;
                    } else if name == "xl/workbook.xml" {
                        has_xl = true;
                    } else if name == "xl/workbook.bin" {
                        has_xlsb = true;
                    } else if name == "visio/document.xml" {
                        has_visio = true;
                    } else if !entry.is_dir() && name.to_ascii_lowercase().ends_with(".fb2") {
                        has_fb2 = true;
                    } else if name.starts_with("3D/") && name.ends_with(".model") {
                        has_3d = true;
                    } else if name == "META-INF/container.xml" {
                        has_epub = true;
                    } else if !entry.is_dir() && name.to_ascii_lowercase().ends_with(".kml") {
                        has_kml = true;
                    } else if !entry.is_dir()
                        && !name.contains('/')
                        && !name.contains('\\')
                        && name.to_ascii_lowercase().ends_with(".ifc")
                    {
                        has_ifczip = true;
                    }
                }
            }
            if has_word {
                return Ok(Self::Docx);
            }
            if has_ppt {
                return Ok(Self::Pptx);
            }
            if has_xl {
                return Ok(Self::Xlsx);
            }
            if has_xlsb {
                return Ok(Self::Xlsb);
            }
            if has_visio {
                return Ok(Self::Visio);
            }
            if has_fb2 {
                return Ok(Self::Fb2);
            }
            if has_3d {
                return Ok(Self::ThreeMf);
            }
            if has_epub {
                return Ok(Self::Epub);
            }
            if has_kml {
                return Ok(Self::Kmz);
            }
            if has_ifczip {
                return Ok(Self::IfcZip);
            }
        }
        if crate::cad::ifc::looks_like_ifc_prefix(&buf) {
            return Ok(Self::Ifc);
        }
        if buf.starts_with(b"ISO-10303-21;") || buf.windows(13).any(|w| w == b"ISO-10303-21;") {
            return Ok(Self::Step);
        }
        if crate::cad::kicad::looks_like_prefix(&buf) {
            return Ok(Self::KicadPcb);
        }
        if crate::cad::kicad_sch_modern::looks_like_prefix(&buf) {
            return Ok(Self::KicadSch);
        }
        if crate::cad::kicad_sch::looks_like_prefix(&buf) {
            return Ok(Self::KicadSchLegacy);
        }
        if crate::cad::eagle::looks_like_prefix(&buf) {
            return Ok(Self::EagleSch);
        }
        if buf.starts_with(b"0\nSECTION")
            || buf.starts_with(b"  0\r\nSECTION")
            || buf.starts_with(b"  0\nSECTION")
            || buf.starts_with(b"0\r\nSECTION")
        {
            return Ok(Self::Dxf);
        }
        if buf.starts_with(b"%FSLA") || buf.starts_with(b"%MO") || buf.starts_with(b"G04") {
            return Ok(Self::Gerber);
        }
        if buf.starts_with(b"IN;")
            || buf.starts_with(b"SP")
            || buf.starts_with(b"PU")
            || buf.starts_with(b"PD")
        {
            return Ok(Self::Hpgl);
        }
        if buf.starts_with(b"M48") || buf.starts_with(b"T01") || buf.starts_with(b"FMAT,2") {
            return Ok(Self::Excellon);
        }
        if buf.starts_with(b"G0 ")
            || buf.starts_with(b"G1 ")
            || buf.starts_with(b"G21")
            || buf.starts_with(b"G90")
            || buf.starts_with(b"(Generated")
            || buf.starts_with(b";Generated")
        {
            return Ok(Self::Gcode);
        }
        if buf.starts_with(b"solid ") {
            return Ok(Self::Stl);
        }
        if buf.starts_with(b"ply\n") || buf.starts_with(b"ply\r\n") {
            return Ok(Self::Ply);
        }
        if crate::cad::pcd::looks_like_pcd_prefix(&buf) {
            return Ok(Self::Pcd);
        }
        if crate::cad::ptx::looks_like_prefix(&buf) {
            return Ok(Self::Ptx);
        }
        if crate::cad::pts::looks_like_prefix(&buf) {
            return Ok(Self::Pts);
        }
        if crate::cad::e57::looks_like_prefix(&buf) {
            return Ok(Self::E57);
        }
        if crate::cad::xyz::looks_like_prefix(&buf) {
            return Ok(Self::Xyz);
        }
        if buf.starts_with(b"$MeshFormat")
            || buf.starts_with(b"# vtk DataFile Version")
            || buf.starts_with(b"<VTKFile")
        {
            return Ok(Self::Simulation);
        }
        if crate::cad::simulation::looks_like_unv_prefix(&buf) {
            return Ok(Self::Unv);
        }
        if crate::document::op2::looks_like_prefix(&buf) {
            return Ok(Self::Op2);
        }
        if buf
            .windows(8)
            .any(|w| w == b"S      1" || w == b"S0000001" || w == b"S       1")
        {
            return Ok(Self::Iges);
        }
        // EMF (Enhanced Metafile): 0x00000001 at 0..4 and " EMF" (0x464D4520) at 40..44
        if buf.len() >= 44 && buf[0..4] == [0x01, 0x00, 0x00, 0x00] && &buf[40..44] == b" EMF" {
            return Ok(Self::Emf);
        }
        // WMF (Aldus Placeable Metafile header key 0x9AC6CDD7 or standard WMF header)
        if (buf.len() >= 4 && buf[0..4] == [0xD7, 0xCD, 0xC6, 0x9A])
            || (buf.len() >= 10
                && (buf[0..4] == [0x01, 0x00, 0x09, 0x00] || buf[0..4] == [0x02, 0x00, 0x09, 0x00])
                && (buf[6..10] == [0x00, 0x03, 0x00, 0x00]
                    || buf[6..10] == [0x00, 0x01, 0x00, 0x00]))
        {
            return Ok(Self::Emf);
        }

        // Text heuristics
        let text = String::from_utf8_lossy(&buf);
        let trimmed = text.trim();
        if crate::cad::opencrg::looks_like_prefix(&buf) {
            return Ok(Self::OpenCrg);
        }
        if looks_like_legacy_visio_xml(trimmed) {
            return Ok(Self::Visio);
        }
        if crate::document::ttml::looks_like_ttml_prefix(&buf) {
            return Ok(Self::Ttml);
        }
        if crate::document::xliff::looks_like_xliff_prefix(&buf) {
            return Ok(Self::Xliff);
        }
        if looks_like_vcalendar(&buf) {
            return Ok(Self::Vcalendar);
        }
        if crate::geospatial::kml::looks_like_kml_prefix(&buf) {
            return Ok(Self::Kml);
        }
        if crate::geospatial::gpx::looks_like_gpx_prefix(&buf) {
            return Ok(Self::Gpx);
        }
        if crate::geospatial::gml::looks_like_gml_prefix(&buf) {
            return Ok(Self::Gml);
        }
        if crate::document::gml_graph::looks_like_prefix(&buf) {
            return Ok(Self::GraphGml);
        }
        if crate::geospatial::georss::looks_like_georss_prefix(&buf) {
            return Ok(Self::GeoRss);
        }
        if crate::document::rdfxml::looks_like_prefix(&buf) {
            return Ok(Self::RdfXml);
        }
        if crate::document::feed::looks_like_prefix(&buf) {
            return Ok(Self::Feed);
        }
        if crate::geospatial::wkt::looks_like_wkt_prefix(&buf) {
            return Ok(Self::Wkt);
        }
        if looks_like_ical(&buf) {
            return Ok(Self::Ical);
        }
        if crate::document::vcard::looks_like_vcard_prefix(&buf) {
            return Ok(Self::Vcard);
        }
        if crate::document::fb2::looks_like_prefix(&buf) {
            return Ok(Self::Fb2);
        }
        if crate::document::jats::looks_like_prefix(&buf) {
            return Ok(Self::Jats);
        }
        if crate::document::tei::looks_like_prefix(&buf) {
            return Ok(Self::Tei);
        }
        if crate::document::ead::looks_like_prefix(&buf) {
            return Ok(Self::Ead);
        }
        if crate::document::eac_cpf::looks_like_prefix(&buf) {
            return Ok(Self::EacCpf);
        }
        if crate::document::dc::looks_like_prefix(&buf) {
            return Ok(Self::DublinCore);
        }
        if crate::document::iso19115::looks_like_prefix(&buf) {
            return Ok(Self::Iso19115);
        }
        if crate::document::ubl::looks_like_prefix(&buf) {
            return Ok(Self::Ubl);
        }
        if crate::document::xbrl::looks_like_prefix(&buf) {
            return Ok(Self::Xbrl);
        }
        if crate::document::s1000d::looks_like_prefix(&buf) {
            return Ok(Self::S1000d);
        }
        if crate::document::alto::looks_like_prefix(&buf) {
            return Ok(Self::Alto);
        }
        if crate::document::mets::looks_like_prefix(&buf) {
            return Ok(Self::Mets);
        }
        if crate::document::marcxml::looks_like_prefix(&buf) {
            return Ok(Self::Marcxml);
        }
        if crate::document::marc::looks_like_prefix(&buf) {
            return Ok(Self::Marc);
        }
        if crate::document::mods::looks_like_prefix(&buf) {
            return Ok(Self::Mods);
        }
        if crate::document::docbook::looks_like_prefix(&buf) {
            return Ok(Self::Docbook);
        }
        if crate::document::dita::looks_like_prefix(&buf) {
            return Ok(Self::Dita);
        }
        if crate::document::pdb::looks_like_prefix(&buf) {
            return Ok(Self::Pdb);
        }
        if crate::document::mobi::looks_like_prefix(&buf) {
            return Ok(Self::Mobi);
        }
        if crate::document::legacy_visio::looks_like_legacy_visio(path) {
            return Ok(Self::Vsd);
        }
        if crate::document::legacy_doc::looks_like_legacy_doc(path) {
            return Ok(Self::Doc);
        }
        if crate::document::legacy_ppt::looks_like_legacy_ppt(path) {
            return Ok(Self::Ppt);
        }
        if crate::document::msg::looks_like_msg_file(path) {
            return Ok(Self::Msg);
        }
        if crate::document::mbox::looks_like_mbox_prefix(&buf) {
            return Ok(Self::Mbox);
        }
        if looks_like_mhtml(&buf) {
            return Ok(Self::Mhtml);
        }
        if crate::document::emlx::looks_like_emlx_prefix(&buf) {
            return Ok(Self::Emlx);
        }
        if looks_like_eml(&buf) {
            return Ok(Self::Eml);
        }
        if looks_like_jupyter_notebook(&buf) {
            return Ok(Self::Jupyter);
        }
        if looks_like_medit_mesh(&buf) {
            return Ok(Self::Medit);
        }
        if looks_like_medit_binary(&buf) {
            return Ok(Self::Medit);
        }
        if looks_like_off_mesh(&buf) {
            return Ok(Self::Off);
        }
        if looks_like_lsdyna_keyword(&buf) {
            return Ok(Self::LsDyna);
        }
        if crate::document::rst::looks_like_rst_prefix(&buf) {
            return Ok(Self::Rst);
        }
        if crate::document::orgmode::looks_like_org_prefix(&buf) {
            return Ok(Self::Org);
        }
        if crate::document::po::looks_like_po_prefix(&buf) {
            return Ok(Self::Po);
        }
        if crate::document::bib::looks_like_bibtex_prefix(&buf) {
            return Ok(Self::Bib);
        }
        if let Some(kind) = crate::document::subtitle::looks_like_subtitle_prefix(&buf) {
            return Ok(match kind {
                crate::document::subtitle::SubtitleKind::Srt => Self::Srt,
                crate::document::subtitle::SubtitleKind::Vtt => Self::Vtt,
            });
        }
        if looks_like_nastran_bdf(&buf) {
            return Ok(Self::Nastran);
        }
        if looks_like_abaqus_inp(&buf) {
            return Ok(Self::Abaqus);
        }
        if trimmed.contains("<mxfile")
            || trimmed.contains("<mxGraphModel")
            || trimmed.contains("<diagram")
        {
            return Ok(Self::Drawio);
        }
        if crate::geospatial::xml_tree::looks_like_root(&buf, b"mxlibrary", None)
            || crate::geospatial::xml_tree::looks_like_root(&buf, b"shapes", None)
        {
            return Ok(Self::Drawio);
        }
        if (trimmed.starts_with("<?xml") && (trimmed.contains("<svg") || trimmed.contains("<SVG")))
            || trimmed.starts_with("<svg")
            || trimmed.starts_with("<SVG")
        {
            return Ok(Self::Svg);
        }
        if trimmed.contains("<!DOCTYPE html") || trimmed.contains("<html") {
            return Ok(Self::Html);
        }
        if crate::document::xml::looks_like_xml_prefix(&buf) {
            if crate::cad::opendrive::looks_like_prefix(&buf) {
                return Ok(Self::OpenDrive);
            }
            if crate::cad::openscenario::looks_like_prefix(&buf) {
                return Ok(Self::OpenScenario);
            }
            if crate::document::citygml::looks_like_prefix(&buf) {
                return Ok(Self::CityGml);
            }
            if crate::document::wsdl::looks_like_prefix(&buf) {
                return Ok(Self::Wsdl);
            }
            if crate::document::opml::looks_like_prefix(&buf) {
                return Ok(Self::Opml);
            }
            if crate::document::plist::looks_like_prefix(&buf) {
                return Ok(Self::Plist);
            }
            if crate::document::cyclonedx::looks_like_xml_prefix(&buf) {
                return Ok(Self::CycloneDx);
            }
            if crate::document::junit::looks_like_prefix(&buf) {
                return Ok(Self::Junit);
            }
            if crate::document::coverage::looks_like_prefix(&buf) {
                return Ok(Self::Coverage);
            }
            return Ok(Self::Xml);
        }
        if crate::document::kubernetes::looks_like_yaml_prefix(&buf) {
            return Ok(Self::Kubernetes);
        }
        if crate::document::compose::looks_like_yaml_prefix(&buf) {
            return Ok(Self::Compose);
        }
        if crate::document::github_actions::looks_like_yaml_prefix(&buf) {
            return Ok(Self::GithubActions);
        }
        if crate::document::openapi::looks_like_yaml_prefix(&buf) {
            return Ok(Self::Openapi);
        }
        if crate::document::asyncapi::looks_like_yaml_prefix(&buf) {
            return Ok(Self::Asyncapi);
        }
        if crate::document::jsonschema::looks_like_yaml_prefix(&buf) {
            return Ok(Self::JsonSchema);
        }
        if trimmed.contains("@startuml") {
            return Ok(Self::PlantUml);
        }
        if trimmed.starts_with("digraph")
            || trimmed.starts_with("strict digraph")
            || trimmed.starts_with("graph ")
        {
            return Ok(Self::Dot);
        }
        if trimmed.starts_with("sequenceDiagram")
            || trimmed.starts_with("flowchart")
            || trimmed.starts_with("graph TD")
            || trimmed.starts_with("graph LR")
            || trimmed.starts_with("classDiagram")
            || trimmed.starts_with("stateDiagram")
            || trimmed.starts_with("erDiagram")
            || trimmed.starts_with("gantt")
            || trimmed.starts_with("pie")
        {
            return Ok(Self::Mermaid);
        }
        if trimmed.starts_with("\\documentclass")
            || trimmed.starts_with("\\begin{equation}")
            || trimmed.starts_with("$$")
        {
            return Ok(Self::Tex);
        }
        if crate::document::jsonpatch::looks_like_prefix(&buf) {
            return Ok(Self::JsonPatch);
        }
        if crate::document::jsonfeed::looks_like_prefix(&buf) {
            return Ok(Self::JsonFeed);
        }
        if crate::document::iiif::looks_like_prefix(&buf) {
            return Ok(Self::Iiif);
        }
        if crate::document::csl_json::looks_like_prefix(&buf) {
            return Ok(Self::CslJson);
        }
        if crate::document::cloudevents::looks_like_prefix(&buf) {
            return Ok(Self::CloudEvents);
        }
        if crate::document::fhir::looks_like_prefix(&buf) {
            return Ok(Self::FhirJson);
        }
        if crate::document::avro::looks_like_prefix(&buf) {
            return Ok(Self::Avro);
        }
        if crate::document::otlp::looks_like_prefix(&buf) {
            return Ok(Self::OtlpJson);
        }
        if crate::document::ocel::looks_like_prefix(&buf) {
            return Ok(Self::OcelJson);
        }
        if crate::document::jsonapi::looks_like_prefix(&buf) {
            return Ok(Self::JsonApi);
        }
        if crate::document::openlabel::looks_like_prefix(&buf) {
            return Ok(Self::OpenLabel);
        }
        if crate::document::cityjson::looks_like_prefix(&buf) {
            return Ok(Self::CityJson);
        }
        if crate::document::json::looks_like_json_prefix(&buf) {
            if looks_like_chart_json_text(trimmed) {
                return Ok(Self::Chart);
            }
            if looks_like_excalidraw_json_text(trimmed) {
                return Ok(Self::Excalidraw);
            }
            return Ok(Self::Json);
        }
        if crate::document::lcov::looks_like_prefix(&buf) {
            return Ok(Self::Lcov);
        }
        if trimmed.starts_with("= ") || trimmed.starts_with("== ") {
            return Ok(Self::Asciidoc);
        }
        if trimmed.starts_with("# ")
            || trimmed.contains("\n## ")
            || trimmed.contains("\n| ---")
            || (trimmed.starts_with('|') && trimmed.contains(" | ") && trimmed.contains("\n|"))
        {
            return Ok(Self::Markdown);
        }
        if trimmed.contains("->")
            && (trimmed.contains(": {")
                || trimmed.contains("shape:")
                || trimmed.contains("direction:"))
        {
            return Ok(Self::D2);
        }
        if (trimmed.starts_with("v ") || trimmed.starts_with("#"))
            && (trimmed.contains("\nv ") || trimmed.contains("\r\nv "))
            && (trimmed.contains("\nf ") || trimmed.contains("\r\nf ") || trimmed.contains("\nl "))
        {
            return Ok(Self::Obj);
        }
        let sample_lines: Vec<&str> = trimmed
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(10)
            .collect();
        if sample_lines.len() >= 2 {
            let comma_count = sample_lines[0].matches(',').count();
            if comma_count >= 1
                && sample_lines
                    .iter()
                    .all(|l| l.matches(',').count() == comma_count)
            {
                return Ok(Self::Csv);
            }
            let tab_count = sample_lines[0].matches('\t').count();
            if tab_count >= 1
                && sample_lines
                    .iter()
                    .all(|l| l.matches('\t').count() == tab_count)
            {
                return Ok(Self::Csv);
            }
        }

        let ext_info = path
            .extension()
            .and_then(|v| v.to_str())
            .map(|s| format!(".{s}"))
            .unwrap_or_else(|| "no extension".into());
        Err(Error::Unsupported(format!(
            "cannot determine format for {} ({ext_info}); specify a recognized file extension or document header",
            path.display()
        )))
    }
}

pub(crate) fn looks_like_geojson(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if !text.starts_with('{') {
        return false;
    }
    let has_coordinates = text.contains("\"coordinates\"");
    let has_geometry_type = [
        "\"Point\"",
        "\"MultiPoint\"",
        "\"LineString\"",
        "\"MultiLineString\"",
        "\"Polygon\"",
        "\"MultiPolygon\"",
        "\"GeometryCollection\"",
    ]
    .iter()
    .any(|kind| text.contains(kind));
    (text.contains("\"FeatureCollection\"") && text.contains("\"features\""))
        || (text.contains("\"Feature\"") && text.contains("\"geometry\""))
        || (has_coordinates && has_geometry_type)
}

fn looks_like_chart_json_text(text: &str) -> bool {
    text.contains("\"series\"")
        && text.contains("\"labels\"")
        && [
            "\"bar\"",
            "\"line\"",
            "\"pie\"",
            "\"doughnut\"",
            "\"area\"",
            "\"scatter\"",
        ]
        .iter()
        .any(|kind| text.contains(kind))
}

fn looks_like_excalidraw_json_text(text: &str) -> bool {
    (text.contains("\"type\"") && text.contains("\"excalidraw\""))
        || (text.contains("\"elements\"")
            && (text.contains("\"appState\"") || text.contains("\"files\"")))
}

fn read_format_prefix(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(FORMAT_SNIFF_BYTES as usize);
    std::io::Read::take(&mut file, FORMAT_SNIFF_BYTES).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn looks_like_legacy_visio_xml(text: &str) -> bool {
    let mut remaining = text.trim_start();
    loop {
        if let Some(after_declaration) = remaining.strip_prefix("<?") {
            let Some((_, tail)) = after_declaration.split_once("?>") else {
                return false;
            };
            remaining = tail.trim_start();
            continue;
        }
        if let Some(after_comment) = remaining.strip_prefix("<!--") {
            let Some((_, tail)) = after_comment.split_once("-->") else {
                return false;
            };
            remaining = tail.trim_start();
            continue;
        }
        break;
    }
    let Some(tag) = remaining.strip_prefix('<') else {
        return false;
    };
    let name = tag
        .split(|character: char| {
            character.is_ascii_whitespace() || character == '/' || character == '>'
        })
        .next()
        .unwrap_or_default();
    name.rsplit(':').next() == Some("VisioDocument")
}

fn looks_like_eml(bytes: &[u8]) -> bool {
    let mut has_from = false;
    let mut has_subject_or_date = false;
    let mut saw_header_body_separator = false;
    for raw_line in bytes.split(|byte| *byte == b'\n').take(64) {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.is_empty() {
            saw_header_body_separator = true;
            break;
        }
        if line.first().is_some_and(u8::is_ascii_whitespace) {
            continue;
        }
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            return false;
        };
        let name = &line[..colon];
        has_from |= name.eq_ignore_ascii_case(b"From");
        has_subject_or_date |=
            name.eq_ignore_ascii_case(b"Subject") || name.eq_ignore_ascii_case(b"Date");
    }
    saw_header_body_separator && has_from && has_subject_or_date
}

fn looks_like_mhtml(bytes: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let header = prefix.split("\r\n\r\n").next().unwrap_or(&prefix);
    header.contains("multipart/related") && header.contains("text/html")
}

fn looks_like_ical(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes)
        .trim_start_matches('\u{feff}')
        .to_ascii_uppercase();
    text.contains("BEGIN:VCALENDAR")
        && (text.contains("BEGIN:VEVENT")
            || text.contains("BEGIN:VTODO")
            || text.contains("BEGIN:VJOURNAL")
            || text.contains("BEGIN:VFREEBUSY"))
}

fn looks_like_vcalendar(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_uppercase();
    text.contains("BEGIN:VCALENDAR")
        && text.contains("VERSION:1.0")
        && (text.contains("BEGIN:VEVENT") || text.contains("BEGIN:VTODO"))
}

fn looks_like_nastran_bdf(bytes: &[u8]) -> bool {
    bytes.split(|byte| *byte == b'\n').any(|raw_line| {
        let line = String::from_utf8_lossy(raw_line);
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('$') {
            return false;
        }
        let first = if line.contains(',') {
            line.split(',').next().unwrap_or_default().trim()
        } else {
            let first_fixed = line.get(..8).unwrap_or_default().trim();
            if first_fixed.split_whitespace().count() > 1 {
                line.split_whitespace().next().unwrap_or_default()
            } else {
                first_fixed
            }
        };
        first.eq_ignore_ascii_case("GRID") || first.eq_ignore_ascii_case("GRID*")
    })
}

fn looks_like_lsdyna_keyword(bytes: &[u8]) -> bool {
    let mut has_keyword = false;
    let mut has_nodes = false;
    let mut has_elements = false;
    for raw_line in bytes.split(|byte| *byte == b'\n') {
        let line = String::from_utf8_lossy(raw_line);
        let line = line.trim().trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('$') {
            continue;
        }
        let upper = line.to_ascii_uppercase();
        has_keyword |= upper == "*KEYWORD" || upper.starts_with("*KEYWORD ");
        has_nodes |= upper == "*NODE" || upper.starts_with("*NODE ") || upper.starts_with("*NODE,");
        has_elements |= upper.starts_with("*ELEMENT_");
    }
    has_keyword || (has_nodes && has_elements)
}

fn looks_like_jupyter_notebook(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with('{')
        && text.contains("\"nbformat\"")
        && text.contains("\"cells\"")
        && text.contains("\"cell_type\"")
}

fn looks_like_medit_mesh(bytes: &[u8]) -> bool {
    for raw_line in bytes.split(|byte| *byte == b'\n') {
        let line = String::from_utf8_lossy(raw_line);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        return line
            .split_whitespace()
            .next()
            .is_some_and(|keyword| keyword.eq_ignore_ascii_case("MeshVersionFormatted"));
    }
    false
}

fn looks_like_medit_binary(bytes: &[u8]) -> bool {
    if bytes.len() < 20 {
        return false;
    }
    let order = match bytes.get(..4) {
        Some([1, 0, 0, 0]) => true,
        Some([0, 0, 0, 1]) => false,
        _ => return false,
    };
    let read_i32 = |offset: usize| -> Option<i32> {
        let raw: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
        Some(if order {
            i32::from_le_bytes(raw)
        } else {
            i32::from_be_bytes(raw)
        })
    };
    let Some(version) = read_i32(4) else {
        return false;
    };
    if !(1..=4).contains(&version) || read_i32(8) != Some(3) {
        return false;
    }
    let dimension_offset = if version >= 3 { 20 } else { 16 };
    read_i32(dimension_offset).is_some_and(|dimension| matches!(dimension, 2 | 3))
}

fn looks_like_off_mesh(bytes: &[u8]) -> bool {
    for raw_line in bytes.split(|byte| *byte == b'\n') {
        let line = String::from_utf8_lossy(raw_line);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let header = line.split_whitespace().next().unwrap_or_default();
        return ["OFF", "COFF", "NOFF", "CNOFF"]
            .iter()
            .any(|supported| header.eq_ignore_ascii_case(supported));
    }
    false
}

fn looks_like_abaqus_inp(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let mut has_heading = false;
    let mut has_nodes = false;
    let mut has_elements = false;
    for line in text
        .lines()
        .map(str::trim)
        .map(|line| line.trim_start_matches('\u{feff}').trim())
    {
        if line.starts_with("**") || !line.starts_with('*') {
            continue;
        }
        let keyword = line
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_uppercase();
        match keyword.as_str() {
            "*HEADING" => has_heading = true,
            "*NODE" => has_nodes = true,
            "*ELEMENT" => has_elements = true,
            _ => {}
        }
    }
    has_nodes && (has_heading || has_elements)
}

impl std::fmt::Display for SourceFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Pdf => "PDF",
            Self::Fdf => "FDF",
            Self::Xfdf => "XFDF",
            Self::LandXml => "LANDXML",
            Self::Mathml => "MATHML",
            Self::Xmp => "XMP",
            Self::Xdp => "XDP",
            Self::Spreadsheetml => "SPREADSHEETML",
            Self::ProjectXml => "MS PROJECT XML",
            Self::Xml => "XML",
            Self::Bpmn => "BPMN",
            Self::Dmn => "DMN",
            Self::Cmmn => "CMMN",
            Self::Reqif => "REQIF",
            Self::Xmi => "XMI",
            Self::Ppt => "PPT",
            Self::Pptx => "PPTX",
            Self::Xls => "XLS",
            Self::Xlsb => "XLSB",
            Self::Xlsx => "XLSX",
            Self::Doc => "DOC",
            Self::GeoJson => "GEOJSON",
            Self::GeoJsonSeq => "GEOJSONSEQ",
            Self::TopoJson => "TOPOJSON",
            Self::Geopackage => "GEOPACKAGE",
            Self::GeoRss => "GEORSS",
            Self::Feed => "FEED",
            Self::Gml => "GML",
            Self::CityGml => "CITYGML",
            Self::CityJson => "CITYJSON",
            Self::GraphGml => "GRAPH GML",
            Self::Gpx => "GPX",
            Self::Wkt => "WKT",
            Self::Kml => "KML",
            Self::Kmz => "KMZ",
            Self::Shapefile => "SHAPEFILE",
            Self::Dbf => "DBF",
            Self::EsriAsciiGrid => "ESRI ASCII GRID",
            Self::Properties => "JAVA PROPERTIES",
            Self::Plist => "PLIST",
            Self::Premis => "PREMIS",
            Self::Docx => "DOCX",
            Self::Drawio => "DRAWIO",
            Self::Dxf => "DXF",
            Self::Gerber => "GERBER",
            Self::KicadPcb => "KICAD PCB",
            Self::KicadSchLegacy => "KICAD LEGACY SCH",
            Self::KicadSch => "KICAD SCH",
            Self::LtspiceAsc => "LTSPICE ASC",
            Self::EagleSch => "EAGLE SCH",
            Self::Hpgl => "HPGL",
            Self::Gcode => "GCODE",
            Self::Excellon => "EXCELLON",
            Self::Stl => "STL",
            Self::Step => "STEP",
            Self::Obj => "OBJ",
            Self::Ply => "PLY",
            Self::Pcd => "PCD",
            Self::Pts => "PTS",
            Self::Ptx => "PTX",
            Self::Xyz => "XYZ",
            Self::E57 => "E57",
            Self::Las => "LAS",
            Self::Unv => "UNV",
            Self::ThreeMf => "3MF",
            Self::Gltf => "GLTF",
            Self::Collada => "COLLADA",
            Self::X3d => "X3D",
            Self::Vrml => "VRML",
            Self::Iges => "IGES",
            Self::Ifc => "IFC",
            Self::IfcXml => "IFCXML",
            Self::IfcZip => "IFCZIP",
            Self::Su2 => "SU2",
            Self::OpenFoam => "OPENFOAM",
            Self::OpenDrive => "OPENDRIVE",
            Self::OpenCrg => "OPENCRG",
            Self::OpenScenario => "OPENSCENARIO",
            Self::OpenLabel => "OPENLABEL",
            Self::OpenFoamField => "OPENFOAM-FIELD",
            Self::Tecplot => "TECPLOT",
            Self::Ensight => "ENSIGHT",
            Self::Plot3d => "PLOT3D",
            Self::Newick => "NEWICK",
            Self::Stockholm => "STOCKHOLM",
            Self::Clustal => "CLUSTAL",
            Self::Nexus => "NEXUS",
            Self::Genbank => "GENBANK",
            Self::Embl => "EMBL",
            Self::Uniprot => "UNIPROT",
            Self::Ris => "RIS",
            Self::Spice => "SPICE",
            Self::Openapi => "OPENAPI",
            Self::Asyncapi => "ASYNCAPI",
            Self::Alto => "ALTO",
            Self::Wsdl => "WSDL",
            Self::Opml => "OPML",
            Self::JsonSchema => "JSONSCHEMA",
            Self::Simulation => "SIMULATION",
            Self::Dot => "DOT",
            Self::Mermaid => "MERMAID",
            Self::PlantUml => "PLANTUML",
            Self::D2 => "D2",
            Self::Excalidraw => "EXCALIDRAW",
            Self::Html => "HTML",
            Self::Docbook => "DOCBOOK",
            Self::DublinCore => "DUBLIN-CORE",
            Self::Iso19115 => "ISO 19115",
            Self::Ubl => "UBL",
            Self::Xbrl => "XBRL",
            Self::Dif => "DIF",
            Self::Fasta => "FASTA",
            Self::Fastq => "FASTQ",
            Self::Dita => "DITA",
            Self::Ead => "EAD",
            Self::EacCpf => "EAC-CPF",
            Self::Pdb => "PDB",
            Self::Hwpx => "HWPX",
            Self::Xmind => "XMIND",
            Self::Nifti => "NIFTI",
            Self::Fits => "FITS",
            Self::Gff3 => "GFF3",
            Self::Gtf => "GTF",
            Self::Mrc => "MRC",
            Self::Netcdf => "NETCDF",
            Self::Sqlite => "SQLITE",
            Self::Sylk => "SYLK",
            Self::Cif => "CIF",
            Self::Cml => "CML",
            Self::RdfXml => "RDF-XML",
            Self::Bcfzip => "BCFZIP",
            Self::FlatOpc => "FLAT-OPC",
            Self::Aasx => "AASX",
            Self::OpenScad => "OPENSCAD",
            Self::Amf => "AMF",
            Self::PlmXml => "PLMXML",
            Self::StepXml => "STEPXML",
            Self::Qif => "QIF",
            Self::B2mml => "B2MML",
            Self::Jdf => "JDF",
            Self::Xjdf => "XJDF",
            Self::Tmx => "TMX",
            Self::Tbx => "TBX",
            Self::GbXml => "GBXML",
            Self::Idml => "IDML",
            Self::OaiPmh => "OAIPMH",
            Self::Onix => "ONIX",
            Self::Xpdl => "XPDL",
            Self::Cda => "CDA",
            Self::Iso20022 => "ISO20022",
            Self::Sbml => "SBML",
            Self::Cellml => "CELLML",
            Self::OcelXml => "OCEL-XML",
            Self::EnergyPlusIdf => "ENERGYPLUS-IDF",
            Self::EnergyPlusEpw => "ENERGYPLUS-EPW",
            Self::Rinex => "RINEX",
            Self::Sat => "SAT",
            Self::Sedml => "SEDML",
            Self::Sbgnml => "SBGNML",
            Self::Omex => "OMEX",
            Self::Xdmf => "XDMF",
            Self::Pvd => "PVD",
            Self::Fds => "FDS",
            Self::Abiword => "ABIWORD",
            Self::Neuroml => "NEUROML",
            Self::Biopax => "BIOPAX",
            Self::Xsd => "XSD",
            Self::Xslt => "XSLT",
            Self::XslFo => "XSL-FO",
            Self::Hdf5 => "HDF5",
            Self::Cgns => "CGNS",
            Self::Exodus => "EXODUS",
            Self::Iwork => "IWORK",
            Self::Dwg => "DWG",
            Self::Rhino3dm => "3DM",
            Self::Access => "ACCESS",
            Self::ThreeDXml => "3DXML",
            Self::Ipc2581 => "IPC2581",
            Self::Jt => "JT",
            Self::Xproc => "XPROC",
            Self::Wadl => "WADL",
            Self::OpenSearch => "OPENSEARCH",
            Self::Saml => "SAML",
            Self::Xacml => "XACML",
            Self::Mol2 => "MOL2",
            Self::Turtle => "TURTLE",
            Self::Eps => "EPS",
            Self::Epub => "EPUB",
            Self::Fb2 => "FB2",
            Self::Mobi => "MOBI",
            Self::Eml => "EML",
            Self::Emlx => "EMLX",
            Self::Mbox => "MBOX",
            Self::Mhtml => "MHTML",
            Self::Mets => "METS",
            Self::Msg => "MSG",
            Self::Ical => "ICAL",
            Self::Vcalendar => "VCALENDAR",
            Self::Vcard => "VCARD",
            Self::Vcf => "VCF",
            Self::Wig => "WIG",
            Self::Odt => "ODT",
            Self::Ods => "ODS",
            Self::Odp => "ODP",
            Self::Odg => "ODG",
            Self::Vsd => "VSD",
            Self::Visio => "VISIO",
            Self::Rtf => "RTF",
            Self::Xps => "XPS",
            Self::Tiff => "TIFF",
            Self::Jpeg2000 => "JPEG2000",
            Self::Dicom => "DICOM",
            Self::DicomSr => "DICOM SR",
            Self::DicomDir => "DICOMDIR",
            Self::Rxn => "RXN",
            Self::Mol => "MOL",
            Self::Sdf => "SDF",
            Self::Cbz => "CBZ",
            Self::Cdb => "CDB",
            Self::Har => "HAR",
            Self::Warc => "WARC",
            Self::Wacz => "WACZ",
            Self::Postman => "POSTMAN",
            Self::Graphql => "GRAPHQL",
            Self::Protobuf => "PROTOBUF",
            Self::Kubernetes => "KUBERNETES",
            Self::Compose => "COMPOSE",
            Self::GithubActions => "GITHUB-ACTIONS",
            Self::Junit => "JUNIT",
            Self::Sarif => "SARIF",
            Self::TerraformPlan => "TERRAFORM-PLAN",
            Self::CycloneDx => "CYCLONEDX",
            Self::Spdx => "SPDX",
            Self::StixJson => "STIX-JSON",
            Self::TaxiiJson => "TAXII-JSON",
            Self::Coverage => "COVERAGE",
            Self::Lcov => "LCOV",
            Self::JsonPatch => "JSONPATCH",
            Self::JsonMergePatch => "JSONMERGEPATCH",
            Self::JsonFeed => "JSONFEED",
            Self::CloudEvents => "CLOUDEVENTS",
            Self::FhirJson => "FHIR-JSON",
            Self::FhirXml => "FHIR-XML",
            Self::Avro => "AVRO",
            Self::OtlpJson => "OTLP-JSON",
            Self::OcelJson => "OCEL-JSON",
            Self::JsonApi => "JSON-API",
            Self::CslJson => "CSL-JSON",
            Self::Abaqus => "ABAQUS",
            Self::Nastran => "NASTRAN",
            Self::Op2 => "OP2",
            Self::LsDyna => "LSDYNA",
            Self::Jupyter => "JUPYTER",
            Self::Jats => "JATS",
            Self::Tei => "TEI",
            Self::Quarto => "QUARTO",
            Self::Medit => "MEDIT",
            Self::Off => "OFF",
            Self::Markdown => "MARKDOWN",
            Self::Asciidoc => "ASCIIDOC",
            Self::Arff => "ARFF",
            Self::Bed => "BED",
            Self::BedGraph => "BEDGRAPH",
            Self::Rst => "RST",
            Self::Sam => "SAM",
            Self::S1000d => "S1000D",
            Self::Maf => "MAF",
            Self::Marcxml => "MARCXML",
            Self::Marc => "MARC21",
            Self::Mods => "MODS",
            Self::Org => "ORG",
            Self::Po => "PO",
            Self::Srt => "SRT",
            Self::Ttml => "TTML",
            Self::Vtt => "VTT",
            Self::Xliff => "XLIFF",
            Self::Csv => "CSV",
            Self::Emf => "EMF",
            Self::Chart => "CHART",
            Self::Json => "JSON",
            Self::JsonLd => "JSON-LD",
            Self::Iiif => "IIIF",
            Self::JsonSeq => "JSONSEQ",
            Self::Graphml => "GRAPHML",
            Self::Gexf => "GEXF",
            Self::Xgmml => "XGMML",
            Self::Toml => "TOML",
            Self::Yaml => "YAML",
            Self::Bib => "BIB",
            Self::Tex => "TEX",
            Self::Qr => "QR",
            Self::Raster => "RASTER",
            Self::Svg => "SVG",
        })
    }
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub max_input_bytes: u64,
    /// Maximum expanded PDF stream or ZIP entry/image bytes.
    pub max_zip_entry_bytes: u64,
    pub max_pages: usize,
    pub max_xml_events: usize,
    pub include_metadata: bool,
    pub precision: usize,
    pub jobs: usize,
    /// Render embedded PDF fonts as exact glyph outlines instead of editable
    /// substitute-font text. This improves fidelity but requires the caller to
    /// verify the source font's outline/embedding rights.
    pub outline_embedded_pdf_text: bool,
    /// draw.io shape libraries to draw `shape=mxgraph.<library>.<name>` with.
    ///
    /// Each entry is a stencil XML file or a directory of them, as shipped in
    /// draw.io's own `stencils` folder. Without one, a shape from a library is
    /// drawn as a labelled placeholder, because the outline lives in the
    /// library rather than in the diagram. A shape the diagram carries inline,
    /// as `shape=stencil(...)`, is always drawn.
    pub stencil_paths: Vec<std::path::PathBuf>,
    /// Keep a copy of a draw.io page's own source in the SVG it produces, in
    /// the `content` attribute draw.io itself uses.
    ///
    /// The SVG stays a picture for every renderer, and both draw.io and
    /// [`crate::svg_to_document`] can restore the editable diagram from it.
    /// Off by default: it roughly doubles the output and puts the source
    /// document inside a file that is usually shared as an image.
    pub embed_drawio_source: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            max_input_bytes: 512 * 1024 * 1024,
            max_zip_entry_bytes: 128 * 1024 * 1024,
            max_pages: 10_000,
            max_xml_events: 5_000_000,
            include_metadata: true,
            precision: 5,
            jobs: 1,
            outline_embedded_pdf_text: false,
            stencil_paths: Vec::new(),
            embed_drawio_source: false,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PageReport {
    pub number: usize,
    pub svg: String,
    pub width_points: f64,
    pub height_points: f64,
    pub node_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<String>,
    pub estimated_ir_bytes: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConversionReport {
    pub converter: &'static str,
    pub version: &'static str,
    pub source: String,
    pub source_format: SourceFormat,
    pub output_directory: String,
    pub elapsed_ms: u128,
    pub input_bytes: u64,
    pub page_count: usize,
    pub largest_page_ir_bytes: usize,
    pub pages: Vec<PageReport>,
    pub warnings: Vec<String>,
}

#[cfg(not(target_arch = "wasm32"))]
pub fn convert_path(
    input: impl AsRef<Path>,
    output_directory: impl AsRef<Path>,
    options: &ConvertOptions,
) -> Result<ConversionReport> {
    let started = Instant::now();
    let input = input.as_ref();
    let output_directory = output_directory.as_ref();
    let metadata = fs::metadata(input)?;
    if !metadata.is_file() {
        return Err(Error::InvalidInput(format!(
            "{} is not a regular file",
            input.display()
        )));
    }
    if metadata.len() > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "input is {} bytes; maximum is {} bytes",
            metadata.len(),
            options.max_input_bytes
        )));
    }
    if options.jobs == 0 {
        return Err(Error::InvalidInput("jobs must be at least 1".into()));
    }
    fs::create_dir_all(output_directory)?;
    if fs::read_dir(output_directory)?.next().is_some() {
        return Err(Error::InvalidInput("output directory must be empty".into()));
    }
    let format = SourceFormat::detect(input)?;
    let mut sink = PageSink::new(output_directory, options);
    let warnings = match format {
        SourceFormat::Pdf => crate::pdf::convert(input, options, &mut sink)?,
        SourceFormat::Fdf => crate::document::fdf::convert(input, options, &mut sink)?,
        SourceFormat::Xfdf => crate::document::xfdf::convert(input, options, &mut sink)?,
        SourceFormat::LandXml => crate::document::landxml::convert(input, options, &mut sink)?,
        SourceFormat::Mathml => crate::document::mathml::convert(input, options, &mut sink)?,
        SourceFormat::Xmp => crate::document::xmp::convert(input, options, &mut sink)?,
        SourceFormat::Xdp => crate::document::xdp::convert(input, options, &mut sink)?,
        SourceFormat::Spreadsheetml => {
            crate::document::spreadsheetml::convert(input, options, &mut sink)?
        }
        SourceFormat::ProjectXml => crate::document::msproject::convert(input, options, &mut sink)?,
        SourceFormat::Xml => crate::document::xml::convert(input, options, &mut sink)?,
        SourceFormat::Bpmn => crate::document::bpmn::convert(input, options, &mut sink)?,
        SourceFormat::Dmn => crate::document::dmn::convert(input, options, &mut sink)?,
        SourceFormat::Cmmn => crate::document::cmmn::convert(input, options, &mut sink)?,
        SourceFormat::Reqif => crate::document::reqif::convert(input, options, &mut sink)?,
        SourceFormat::Xmi => crate::document::xmi::convert(input, options, &mut sink)?,
        SourceFormat::Pptx => crate::ooxml::pptx::convert(input, options, &mut sink)?,
        SourceFormat::Xls => crate::ooxml::xls::convert(input, options, &mut sink)?,
        SourceFormat::Xlsb => crate::ooxml::xlsb::convert(input, options, &mut sink)?,
        SourceFormat::GeoJson => crate::geospatial::geojson::convert(input, options, &mut sink)?,
        SourceFormat::GeoJsonSeq => {
            crate::geospatial::geojson_seq::convert(input, options, &mut sink)?
        }
        SourceFormat::TopoJson => crate::geospatial::topojson::convert(input, options, &mut sink)?,
        SourceFormat::Geopackage => {
            crate::geospatial::geopackage::convert(input, options, &mut sink)?
        }
        SourceFormat::Gml => crate::geospatial::gml::convert(input, options, &mut sink)?,
        SourceFormat::CityGml => crate::document::citygml::convert(input, options, &mut sink)?,
        SourceFormat::CityJson => crate::document::cityjson::convert(input, options, &mut sink)?,
        SourceFormat::GraphGml => crate::document::gml_graph::convert(input, options, &mut sink)?,
        SourceFormat::GeoRss => crate::geospatial::georss::convert(input, options, &mut sink)?,
        SourceFormat::Feed => crate::document::feed::convert(input, options, &mut sink)?,
        SourceFormat::Gpx => crate::geospatial::gpx::convert(input, options, &mut sink)?,
        SourceFormat::Wkt => crate::geospatial::wkt::convert(input, options, &mut sink)?,
        SourceFormat::Kml => crate::geospatial::kml::convert_kml(input, options, &mut sink)?,
        SourceFormat::Kmz => crate::geospatial::kml::convert_kmz(input, options, &mut sink)?,
        SourceFormat::Shapefile => {
            crate::geospatial::shapefile::convert(input, options, &mut sink)?
        }
        SourceFormat::Dbf => crate::geospatial::dbase::convert(input, options, &mut sink)?,
        SourceFormat::EsriAsciiGrid => {
            crate::geospatial::ascii_grid::convert(input, options, &mut sink)?
        }
        SourceFormat::Xlsx => crate::ooxml::xlsx::convert(input, options, &mut sink)?,
        SourceFormat::Docx => crate::ooxml::docx::convert(input, options, &mut sink)?,
        SourceFormat::Ppt => crate::document::legacy_ppt::convert(input, options, &mut sink)?,
        SourceFormat::Doc => {
            crate::document::legacy_doc::convert(input, options, "doc", &mut sink)?
        }
        SourceFormat::Vsd => crate::document::legacy_visio::convert(input, options, &mut sink)?,
        SourceFormat::Drawio => crate::drawio::convert(input, options, &mut sink)?,
        SourceFormat::KicadPcb => crate::cad::kicad::convert(input, options, &mut sink)?,
        SourceFormat::KicadSchLegacy => crate::cad::kicad_sch::convert(input, options, &mut sink)?,
        SourceFormat::KicadSch => crate::cad::kicad_sch_modern::convert(input, options, &mut sink)?,
        SourceFormat::LtspiceAsc => crate::cad::ltspice::convert(input, options, &mut sink)?,
        SourceFormat::EagleSch => crate::cad::eagle::convert(input, options, &mut sink)?,
        SourceFormat::Ifc => {
            let file = std::fs::File::open(input)?;
            crate::cad::ifc::convert(file, options, &mut sink)?
        }
        SourceFormat::IfcXml => {
            let file = std::fs::File::open(input)?;
            crate::cad::ifc::convert_ifcxml(file, options, &mut sink)?
        }
        SourceFormat::IfcZip => crate::cad::ifc::convert_ifczip(input, options, &mut sink)?,
        SourceFormat::Dxf
        | SourceFormat::Gerber
        | SourceFormat::Hpgl
        | SourceFormat::Gcode
        | SourceFormat::Excellon
        | SourceFormat::Stl
        | SourceFormat::Ply
        | SourceFormat::Pcd
        | SourceFormat::Las
        | SourceFormat::ThreeMf
        | SourceFormat::Gltf
        | SourceFormat::Collada
        | SourceFormat::X3d
        | SourceFormat::Step
        | SourceFormat::Iges
        | SourceFormat::Obj
        | SourceFormat::Simulation => crate::cad::convert(input, options, &mut sink)?,
        SourceFormat::Unv => {
            let file = std::fs::File::open(input)?;
            crate::cad::simulation::convert_unv(file, options, &mut sink)?
        }
        SourceFormat::Su2 => {
            let file = std::fs::File::open(input)?;
            crate::cad::simulation::convert_su2(file, options, &mut sink)?
        }
        SourceFormat::OpenFoam => {
            crate::cad::simulation::convert_openfoam(input, options, &mut sink)?
        }
        SourceFormat::OpenDrive => crate::cad::opendrive::convert(input, options, &mut sink)?,
        SourceFormat::OpenCrg => crate::cad::opencrg::convert(input, options, &mut sink)?,
        SourceFormat::OpenScenario => crate::cad::openscenario::convert(input, options, &mut sink)?,
        SourceFormat::OpenLabel => crate::document::openlabel::convert(input, options, &mut sink)?,
        SourceFormat::OpenFoamField => {
            crate::cad::openfoam_field::convert(input, options, &mut sink)?
        }
        SourceFormat::Vrml => {
            let file = std::fs::File::open(input)?;
            crate::cad::vrml::convert(file, options, &mut sink)?
        }
        SourceFormat::Tecplot => {
            let file = std::fs::File::open(input)?;
            crate::cad::simulation::convert_tecplot(file, options, &mut sink)?
        }
        SourceFormat::Ensight => {
            let extension = input
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if extension.eq_ignore_ascii_case("case") {
                crate::cad::simulation::convert_ensight_case(input, options, &mut sink)?
            } else {
                let file = std::fs::File::open(input)?;
                crate::cad::simulation::convert_ensight_geometry(file, options, &mut sink)?
            }
        }
        SourceFormat::Plot3d => {
            let file = std::fs::File::open(input)?;
            crate::cad::simulation::convert_plot3d(file, options, &mut sink)?
        }
        SourceFormat::Ptx => {
            let file = std::fs::File::open(input)?;
            crate::cad::ptx::convert(file, options, &mut sink)?
        }
        SourceFormat::Pts => {
            let file = std::fs::File::open(input)?;
            crate::cad::pts::convert(file, options, &mut sink)?
        }
        SourceFormat::E57 => crate::cad::e57::convert(input, options, &mut sink)?,
        SourceFormat::Xyz => {
            let file = std::fs::File::open(input)?;
            crate::cad::xyz::convert(file, options, &mut sink)?
        }
        SourceFormat::Dot | SourceFormat::Mermaid => {
            crate::diagram::convert(input, options, &mut sink)?
        }
        SourceFormat::PlantUml => crate::diagram::plantuml::convert(input, options, &mut sink)?,
        SourceFormat::D2 => crate::diagram::d2::convert(input, options, &mut sink)?,
        SourceFormat::Excalidraw => crate::diagram::excalidraw::convert(input, options, &mut sink)?,
        SourceFormat::Html => crate::document::html::convert(input, options, &mut sink)?,
        SourceFormat::Docbook => crate::document::docbook::convert(input, options, &mut sink)?,
        SourceFormat::DublinCore => crate::document::dc::convert(input, options, &mut sink)?,
        SourceFormat::Iso19115 => crate::document::iso19115::convert(input, options, &mut sink)?,
        SourceFormat::Ubl => crate::document::ubl::convert(input, options, &mut sink)?,
        SourceFormat::Xbrl => crate::document::xbrl::convert(input, options, &mut sink)?,
        SourceFormat::Dif => crate::document::dif::convert(input, options, &mut sink)?,
        SourceFormat::Fasta => crate::document::fastx::convert_fasta(input, options, &mut sink)?,
        SourceFormat::Fastq => crate::document::fastx::convert_fastq(input, options, &mut sink)?,
        SourceFormat::Dita => crate::document::dita::convert(input, options, &mut sink)?,
        SourceFormat::Ead => crate::document::ead::convert(input, options, &mut sink)?,
        SourceFormat::EacCpf => crate::document::eac_cpf::convert(input, options, &mut sink)?,
        SourceFormat::Pdb => crate::document::pdb::convert(input, options, &mut sink)?,
        SourceFormat::Hwpx => crate::document::hwpx::convert(input, options, &mut sink)?,
        SourceFormat::Xmind => crate::document::xmind::convert(input, options, &mut sink)?,
        SourceFormat::Nifti => crate::document::nifti::convert(input, options, &mut sink)?,
        SourceFormat::Fits => crate::document::fits::convert(input, options, &mut sink)?,
        SourceFormat::Gff3 => crate::document::gff::convert(
            input,
            options,
            &mut sink,
            crate::document::gff::FeatureFormat::Gff3,
        )?,
        SourceFormat::Gtf => crate::document::gff::convert(
            input,
            options,
            &mut sink,
            crate::document::gff::FeatureFormat::Gtf,
        )?,
        SourceFormat::Mrc => crate::document::mrc::convert(input, options, &mut sink)?,
        SourceFormat::Netcdf => crate::document::netcdf::convert(input, options, &mut sink)?,
        SourceFormat::Exodus => crate::document::netcdf::convert_exodus(input, options, &mut sink)?,
        SourceFormat::Sqlite => crate::document::sqlite::convert(input, options, &mut sink)?,
        SourceFormat::Sylk => crate::document::sylk::convert(input, options, &mut sink)?,
        SourceFormat::Cif => crate::document::cif::convert(input, options, &mut sink)?,
        SourceFormat::Cml => crate::document::cml::convert(input, options, &mut sink)?,
        SourceFormat::RdfXml => crate::document::rdfxml::convert(input, options, &mut sink)?,
        SourceFormat::Bcfzip => crate::document::bcf::convert(input, options, &mut sink)?,
        SourceFormat::FlatOpc => crate::document::flatopc::convert(input, options, &mut sink)?,
        SourceFormat::Aasx => crate::document::aasx::convert(input, options, &mut sink)?,
        SourceFormat::OpenScad => crate::document::openscad::convert(input, options, &mut sink)?,
        SourceFormat::Amf => crate::cad::amf::convert(input, options, &mut sink)?,
        SourceFormat::PlmXml => crate::document::plmxml::convert(input, options, &mut sink)?,
        SourceFormat::StepXml => crate::document::stepxml::convert(input, options, &mut sink)?,
        SourceFormat::Qif => crate::document::qif::convert(input, options, &mut sink)?,
        SourceFormat::B2mml => crate::document::b2mml::convert(input, options, &mut sink)?,
        SourceFormat::Jdf => crate::document::jdf::convert(input, options, &mut sink)?,
        SourceFormat::Xjdf => crate::document::jdf::convert_xjdf(input, options, &mut sink)?,
        SourceFormat::Tmx => crate::document::tmx::convert(input, options, &mut sink)?,
        SourceFormat::Tbx => crate::document::tbx::convert(input, options, &mut sink)?,
        SourceFormat::GbXml => crate::document::gbxml::convert(input, options, &mut sink)?,
        SourceFormat::Idml => crate::document::idml::convert(input, options, &mut sink)?,
        SourceFormat::OaiPmh => crate::document::oaipmh::convert(input, options, &mut sink)?,
        SourceFormat::Onix => crate::document::onix::convert(input, options, &mut sink)?,
        SourceFormat::Xpdl => crate::document::xpdl::convert(input, options, &mut sink)?,
        SourceFormat::Cda => crate::document::cda::convert(input, options, &mut sink)?,
        SourceFormat::Iso20022 => crate::document::iso20022::convert(input, options, &mut sink)?,
        SourceFormat::Sbml => crate::document::sbml::convert(input, options, &mut sink)?,
        SourceFormat::Cellml => crate::document::cellml::convert(input, options, &mut sink)?,
        SourceFormat::OcelXml => crate::document::ocel_xml::convert(input, options, &mut sink)?,
        SourceFormat::EnergyPlusIdf => {
            crate::document::energyplus::convert_idf(input, options, &mut sink)?
        }
        SourceFormat::EnergyPlusEpw => {
            crate::document::energyplus::convert_epw(input, options, &mut sink)?
        }
        SourceFormat::Rinex => crate::document::rinex::convert(input, options, &mut sink)?,
        SourceFormat::Sat => crate::cad::sat::convert(input, options, &mut sink)?,
        SourceFormat::Sedml => crate::document::sedml::convert(input, options, &mut sink)?,
        SourceFormat::Sbgnml => crate::document::sbgnml::convert(input, options, &mut sink)?,
        SourceFormat::Omex => crate::document::omex::convert(input, options, &mut sink)?,
        SourceFormat::Xdmf => crate::document::xdmf::convert(input, options, &mut sink)?,
        SourceFormat::Pvd => crate::document::pvd::convert(input, options, &mut sink)?,
        SourceFormat::Fds => crate::document::fds::convert(input, options, &mut sink)?,
        SourceFormat::Abiword => crate::document::abiword::convert(input, options, &mut sink)?,
        SourceFormat::Neuroml => crate::document::neuroml::convert(input, options, &mut sink)?,
        SourceFormat::Biopax => crate::document::biopax::convert(input, options, &mut sink)?,
        SourceFormat::Xsd => crate::document::xsd::convert(input, options, &mut sink)?,
        SourceFormat::Xslt => crate::document::xslt::convert(input, options, &mut sink)?,
        SourceFormat::XslFo => crate::document::xslfo::convert(input, options, &mut sink)?,
        SourceFormat::Hdf5 => crate::document::hdf5::convert_hdf5(input, options, &mut sink)?,
        SourceFormat::Cgns => crate::document::hdf5::convert_cgns(input, options, &mut sink)?,
        SourceFormat::Xproc => crate::document::xproc::convert(input, options, &mut sink)?,
        SourceFormat::Wadl => crate::document::wadl::convert(input, options, &mut sink)?,
        SourceFormat::OpenSearch => {
            crate::document::opensearch::convert(input, options, &mut sink)?
        }
        SourceFormat::Saml => crate::document::saml::convert(input, options, &mut sink)?,
        SourceFormat::Xacml => crate::document::xacml::convert(input, options, &mut sink)?,
        SourceFormat::Mol2 => crate::document::mol2::convert(input, options, &mut sink)?,
        SourceFormat::Turtle => crate::document::turtle::convert(input, options, &mut sink)?,
        SourceFormat::Eps => crate::document::eps::convert(input, options, &mut sink)?,
        SourceFormat::Epub => crate::document::epub::convert(input, options, &mut sink)?,
        SourceFormat::Fb2 => crate::document::fb2::convert(input, options, &mut sink)?,
        SourceFormat::Mobi => crate::document::mobi::convert(input, options, &mut sink)?,
        SourceFormat::Eml => crate::document::eml::convert(input, options, &mut sink)?,
        SourceFormat::Emlx => crate::document::emlx::convert(input, options, &mut sink)?,
        SourceFormat::Mbox => crate::document::mbox::convert(input, options, &mut sink)?,
        SourceFormat::Mhtml => crate::document::mhtml::convert(input, options, &mut sink)?,
        SourceFormat::Mets => crate::document::mets::convert(input, options, &mut sink)?,
        SourceFormat::Msg => crate::document::msg::convert(input, options, &mut sink)?,
        SourceFormat::Ical => crate::document::ical::convert(input, options, &mut sink)?,
        SourceFormat::Vcalendar => {
            crate::document::ical::convert_vcalendar(input, options, &mut sink)?
        }
        SourceFormat::Vcard => crate::document::vcard::convert(input, options, &mut sink)?,
        SourceFormat::Vcf => crate::document::vcf::convert(input, options, &mut sink)?,
        SourceFormat::Wig => crate::document::wig::convert(input, options, &mut sink)?,
        SourceFormat::Odt => crate::document::odt::convert(input, options, &mut sink)?,
        SourceFormat::Ods => crate::document::ods::convert(input, options, &mut sink)?,
        SourceFormat::Odp => crate::document::odp::convert(input, options, &mut sink)?,
        SourceFormat::Odg => crate::document::odg::convert(input, options, &mut sink)?,
        SourceFormat::Iwork => crate::document::iwork::convert(input, options, &mut sink)?,
        SourceFormat::Dwg => crate::document::dwg::convert(input, options, &mut sink)?,
        SourceFormat::Rhino3dm => crate::document::rhino3dm::convert(input, options, &mut sink)?,
        SourceFormat::Access => crate::document::access::convert(input, options, &mut sink)?,
        SourceFormat::ThreeDXml => crate::document::threedxml::convert(input, options, &mut sink)?,
        SourceFormat::Ipc2581 => crate::document::ipc2581::convert(input, options, &mut sink)?,
        SourceFormat::Jt => crate::document::jt::convert(input, options, &mut sink)?,
        SourceFormat::Visio => {
            if input
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("vdx"))
            {
                crate::document::vdx::convert(input, options, &mut sink)?
            } else {
                crate::document::vsdx::convert(input, options, &mut sink)?
            }
        }
        SourceFormat::Rtf => crate::document::rtf::convert(input, options, &mut sink)?,
        SourceFormat::Xps => crate::document::xps::convert(input, options, &mut sink)?,
        SourceFormat::Tiff => crate::document::tiff::convert(input, options, &mut sink)?,
        SourceFormat::Jpeg2000 => crate::document::jpeg2000::convert(input, options, &mut sink)?,
        SourceFormat::Dicom | SourceFormat::DicomSr => {
            crate::document::dicom::convert(input, options, &mut sink)?
        }
        SourceFormat::DicomDir => {
            crate::document::dicom::convert_directory(input, options, &mut sink)?
        }
        SourceFormat::Mol | SourceFormat::Sdf => {
            crate::document::molfile::convert(input, options, format, &mut sink)?
        }
        SourceFormat::Rxn => crate::document::molfile::convert_rxn(input, options, &mut sink)?,
        SourceFormat::Cbz => crate::document::cbz::convert(input, options, &mut sink)?,
        SourceFormat::Cdb => {
            let file = std::fs::File::open(input)?;
            crate::cad::simulation::convert_cdb(file, options, &mut sink)?
        }
        SourceFormat::Op2 => crate::document::op2::convert(input, options, &mut sink)?,
        SourceFormat::Har => crate::document::har::convert(input, options, &mut sink)?,
        SourceFormat::Warc => crate::document::warc::convert(input, options, &mut sink)?,
        SourceFormat::Wacz => crate::document::wacz::convert(input, options, &mut sink)?,
        SourceFormat::Postman => crate::document::postman::convert(input, options, &mut sink)?,
        SourceFormat::Graphql => crate::document::graphql::convert(input, options, &mut sink)?,
        SourceFormat::Protobuf => crate::document::protobuf::convert(input, options, &mut sink)?,
        SourceFormat::Abaqus => {
            let file = std::fs::File::open(input)?;
            crate::cad::simulation::convert_abaqus(file, options, &mut sink)?
        }
        SourceFormat::Nastran => {
            let file = std::fs::File::open(input)?;
            crate::cad::simulation::convert_nastran(file, options, &mut sink)?
        }
        SourceFormat::LsDyna => {
            let file = std::fs::File::open(input)?;
            crate::cad::simulation::convert_lsdyna(file, options, &mut sink)?
        }
        SourceFormat::Jupyter => {
            let file = std::fs::File::open(input)?;
            crate::document::jupyter::convert(file, options, &mut sink)?
        }
        SourceFormat::Jats => crate::document::jats::convert(input, options, &mut sink)?,
        SourceFormat::Tei => crate::document::tei::convert(input, options, &mut sink)?,
        SourceFormat::Quarto => crate::document::quarto::convert(input, options, &mut sink)?,
        SourceFormat::Medit => {
            let file = std::fs::File::open(input)?;
            crate::cad::simulation::convert_medit(file, options, &mut sink)?
        }
        SourceFormat::Off => {
            let file = std::fs::File::open(input)?;
            crate::cad::off::convert(file, options, &mut sink)?
        }
        SourceFormat::Markdown => crate::document::markdown::convert(input, options, &mut sink)?,
        SourceFormat::Asciidoc => crate::document::asciidoc::convert(input, options, &mut sink)?,
        SourceFormat::Arff => crate::document::arff::convert(input, options, &mut sink)?,
        SourceFormat::Bed => crate::document::bed::convert(
            input,
            options,
            &mut sink,
            crate::document::bed::BedFormat::Bed,
        )?,
        SourceFormat::BedGraph => crate::document::bed::convert(
            input,
            options,
            &mut sink,
            crate::document::bed::BedFormat::BedGraph,
        )?,
        SourceFormat::Rst => crate::document::rst::convert(input, options, &mut sink)?,
        SourceFormat::Sam => crate::document::sam::convert(input, options, &mut sink)?,
        SourceFormat::S1000d => crate::document::s1000d::convert(input, options, &mut sink)?,
        SourceFormat::Maf => crate::document::maf::convert(input, options, &mut sink)?,
        SourceFormat::Marcxml => crate::document::marcxml::convert(input, options, &mut sink)?,
        SourceFormat::Marc => crate::document::marc::convert(input, options, &mut sink)?,
        SourceFormat::Mods => crate::document::mods::convert(input, options, &mut sink)?,
        SourceFormat::Newick => crate::document::newick::convert(input, options, &mut sink)?,
        SourceFormat::Stockholm => crate::document::stockholm::convert(input, options, &mut sink)?,
        SourceFormat::Clustal => crate::document::clustal::convert(input, options, &mut sink)?,
        SourceFormat::Nexus => crate::document::nexus::convert(input, options, &mut sink)?,
        SourceFormat::Genbank => crate::document::genbank::convert(input, options, &mut sink)?,
        SourceFormat::Embl => crate::document::embl::convert(input, options, &mut sink)?,
        SourceFormat::Uniprot => crate::document::uniprot::convert(input, options, &mut sink)?,
        SourceFormat::Ris => crate::document::ris::convert(input, options, &mut sink)?,
        SourceFormat::Spice => crate::cad::spice::convert(input, options, &mut sink)?,
        SourceFormat::Openapi => crate::document::openapi::convert(input, options, &mut sink)?,
        SourceFormat::Asyncapi => crate::document::asyncapi::convert(input, options, &mut sink)?,
        SourceFormat::Alto => crate::document::alto::convert(input, options, &mut sink)?,
        SourceFormat::Wsdl => crate::document::wsdl::convert(input, options, &mut sink)?,
        SourceFormat::Opml => crate::document::opml::convert(input, options, &mut sink)?,
        SourceFormat::JsonSchema => {
            crate::document::jsonschema::convert(input, options, &mut sink)?
        }
        SourceFormat::Kubernetes => {
            crate::document::kubernetes::convert(input, options, &mut sink)?
        }
        SourceFormat::Compose => crate::document::compose::convert(input, options, &mut sink)?,
        SourceFormat::GithubActions => {
            crate::document::github_actions::convert(input, options, &mut sink)?
        }
        SourceFormat::Junit => crate::document::junit::convert(input, options, &mut sink)?,
        SourceFormat::Sarif => crate::document::sarif::convert(input, options, &mut sink)?,
        SourceFormat::TerraformPlan => {
            crate::document::terraform::convert(input, options, &mut sink)?
        }
        SourceFormat::CycloneDx => crate::document::cyclonedx::convert(input, options, &mut sink)?,
        SourceFormat::Spdx => crate::document::spdx::convert(input, options, &mut sink)?,
        SourceFormat::StixJson => crate::document::stix::convert(input, options, &mut sink)?,
        SourceFormat::TaxiiJson => crate::document::taxii::convert(input, options, &mut sink)?,
        SourceFormat::Coverage => crate::document::coverage::convert(input, options, &mut sink)?,
        SourceFormat::Lcov => crate::document::lcov::convert(input, options, &mut sink)?,
        SourceFormat::JsonPatch => crate::document::jsonpatch::convert(input, options, &mut sink)?,
        SourceFormat::JsonMergePatch => {
            crate::document::jsonmergepatch::convert(input, options, &mut sink)?
        }
        SourceFormat::JsonFeed => crate::document::jsonfeed::convert(input, options, &mut sink)?,
        SourceFormat::CloudEvents => {
            crate::document::cloudevents::convert(input, options, &mut sink)?
        }
        SourceFormat::FhirJson => crate::document::fhir::convert(input, options, &mut sink)?,
        SourceFormat::FhirXml => crate::document::fhir_xml::convert(input, options, &mut sink)?,
        SourceFormat::Avro => crate::document::avro::convert(input, options, &mut sink)?,
        SourceFormat::OtlpJson => crate::document::otlp::convert(input, options, &mut sink)?,
        SourceFormat::OcelJson => crate::document::ocel::convert(input, options, &mut sink)?,
        SourceFormat::JsonApi => crate::document::jsonapi::convert(input, options, &mut sink)?,
        SourceFormat::CslJson => crate::document::csl_json::convert(input, options, &mut sink)?,
        SourceFormat::Org => crate::document::orgmode::convert(input, options, &mut sink)?,
        SourceFormat::Po => crate::document::po::convert(input, options, &mut sink)?,
        SourceFormat::Bib => crate::document::bib::convert(input, options, &mut sink)?,
        SourceFormat::Srt => crate::document::subtitle::convert(
            input,
            options,
            &mut sink,
            crate::document::subtitle::SubtitleKind::Srt,
        )?,
        SourceFormat::Ttml => crate::document::ttml::convert(input, options, &mut sink)?,
        SourceFormat::Xliff => crate::document::xliff::convert(input, options, &mut sink)?,
        SourceFormat::Vtt => crate::document::subtitle::convert(
            input,
            options,
            &mut sink,
            crate::document::subtitle::SubtitleKind::Vtt,
        )?,
        SourceFormat::Csv => crate::table::convert(input, options, &mut sink)?,
        SourceFormat::Chart => crate::chart::convert(input, options, &mut sink)?,
        SourceFormat::Json => crate::document::json::convert(input, options, &mut sink)?,
        SourceFormat::JsonLd => crate::document::jsonld::convert(input, options, &mut sink)?,
        SourceFormat::Iiif => crate::document::iiif::convert(input, options, &mut sink)?,
        SourceFormat::JsonSeq => crate::document::json_seq::convert(input, options, &mut sink)?,
        SourceFormat::Graphml => crate::document::graphml::convert(input, options, &mut sink)?,
        SourceFormat::Gexf => crate::document::gexf::convert(input, options, &mut sink)?,
        SourceFormat::Xgmml => crate::document::xgmml::convert(input, options, &mut sink)?,
        SourceFormat::Toml => crate::document::toml::convert(input, options, &mut sink)?,
        SourceFormat::Yaml => crate::document::yaml::convert(input, options, &mut sink)?,
        SourceFormat::Properties => {
            crate::document::properties::convert(input, options, &mut sink)?
        }
        SourceFormat::Plist => crate::document::plist::convert(input, options, &mut sink)?,
        SourceFormat::Premis => crate::document::premis::convert(input, options, &mut sink)?,
        SourceFormat::Tex => crate::math::convert(input, options, &mut sink)?,
        SourceFormat::Qr => crate::qr::convert(input, options, &mut sink)?,
        SourceFormat::Raster => crate::vectorize::convert(input, options, &mut sink)?,
        SourceFormat::Emf => crate::metafile::convert(input, options, &mut sink)?,
        SourceFormat::Svg => crate::svg::convert(input, options, &mut sink)?,
    };
    let pages = sink.finish()?;
    let largest_page_ir_bytes = pages
        .iter()
        .map(|page| page.estimated_ir_bytes)
        .max()
        .unwrap_or(0);
    let report = ConversionReport {
        converter: "document-svg",
        version: env!("CARGO_PKG_VERSION"),
        source: input.to_string_lossy().into_owned(),
        source_format: format,
        output_directory: output_directory.to_string_lossy().into_owned(),
        elapsed_ms: started.elapsed().as_millis(),
        input_bytes: metadata.len(),
        page_count: pages.len(),
        largest_page_ir_bytes,
        pages,
        warnings,
    };
    let manifest_path = output_directory.join("conversion.json");
    let mut temporary = tempfile::NamedTempFile::new_in(output_directory)?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, &report)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    temporary
        .persist_noclobber(manifest_path)
        .map_err(|error| error.error)?;
    Ok(report)
}

/// Output bounds for [`convert_bytes`]. These limits cover generated SVG and
/// selectable/searchable text geometry; input, expanded ZIP parts, XML events,
/// and page count are bounded by [`ConvertOptions`].
#[derive(Clone, Copy, Debug)]
pub struct ByteConvertLimits {
    pub max_page_svg_bytes: usize,
    pub max_total_svg_bytes: usize,
    pub max_page_text_spans: usize,
    pub max_total_text_spans: usize,
    pub max_total_text_bytes: usize,
}

impl Default for ByteConvertLimits {
    fn default() -> Self {
        Self {
            max_page_svg_bytes: 64 * 1024 * 1024,
            max_total_svg_bytes: 256 * 1024 * 1024,
            max_page_text_spans: 5_000,
            max_total_text_spans: 50_000,
            max_total_text_bytes: 8 * 1024 * 1024,
        }
    }
}

/// One self-contained SVG page produced by [`convert_bytes`].
#[derive(Clone, Debug, Serialize)]
pub struct SvgPage {
    pub number: usize,
    pub svg: String,
    pub width_points: f64,
    pub height_points: f64,
    pub node_count: usize,
    pub warning_count: usize,
    pub warnings: Vec<String>,
    pub estimated_ir_bytes: usize,
    pub text_spans: Vec<TextSpan>,
}

/// Approximate selectable/searchable text geometry associated with a page.
/// Coordinates and transforms use SVG user units (points).
#[derive(Clone, Debug, Serialize)]
pub struct TextSpan {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub font_size: f64,
    pub transform: Matrix,
}

/// Document-level conversion details returned after all pages are emitted.
#[derive(Clone, Debug, Serialize)]
pub struct ByteConversionReport {
    pub converter: &'static str,
    pub version: &'static str,
    pub source: String,
    pub source_format: SourceFormat,
    pub elapsed_ms: u128,
    pub input_bytes: u64,
    pub page_count: usize,
    pub largest_page_ir_bytes: usize,
    pub warnings: Vec<String>,
}

/// Convert a supported document directly from memory and emit completed pages
/// as soon as they are rendered. This API is designed for browser/WASM and
/// server preview callers that should not need temporary files.
pub fn convert_bytes<F>(
    file_name: &str,
    bytes: &[u8],
    options: &ConvertOptions,
    limits: ByteConvertLimits,
    mut on_page: F,
) -> Result<ByteConversionReport>
where
    F: FnMut(SvgPage) -> Result<()>,
{
    let started = Instant::now();
    if bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "input is {} bytes; maximum is {} bytes",
            bytes.len(),
            options.max_input_bytes
        )));
    }
    if options.jobs == 0 {
        return Err(Error::InvalidInput("jobs must be at least 1".into()));
    }
    if limits.max_page_svg_bytes == 0
        || limits.max_total_svg_bytes == 0
        || limits.max_page_text_spans == 0
        || limits.max_total_text_spans == 0
        || limits.max_total_text_bytes == 0
    {
        return Err(Error::InvalidInput(
            "SVG and text-layer output limits must be greater than zero".into(),
        ));
    }
    let format = detect_in_memory_format(file_name, bytes)?;
    let mut sink = BytePageSink::new(options, limits, &mut on_page);
    let warnings = match format {
        SourceFormat::Pdf => crate::pdf::convert_bytes(bytes, options, &mut sink)?,
        SourceFormat::Docx => crate::ooxml::docx::convert_bytes(bytes, options, &mut sink)?,
        SourceFormat::Xlsx => crate::ooxml::xlsx::convert_bytes(bytes, options, &mut sink)?,
        SourceFormat::Pptx => crate::ooxml::pptx::convert_bytes(bytes, options, &mut sink)?,
        SourceFormat::FlatOpc => {
            crate::document::flatopc::convert_bytes(bytes, options, &mut sink)?
        }
        _ => {
            return Err(Error::Unsupported(format!(
                "in-memory preview does not support {}",
                format
            )));
        }
    };
    let (page_count, largest_page_ir_bytes, emitted_warnings) = sink.finish()?;
    let mut warnings = warnings;
    warnings.extend(emitted_warnings);
    warnings.sort();
    warnings.dedup();
    Ok(ByteConversionReport {
        converter: "document-svg",
        version: env!("CARGO_PKG_VERSION"),
        source: file_name.to_owned(),
        source_format: format,
        elapsed_ms: started.elapsed().as_millis(),
        input_bytes: bytes.len() as u64,
        page_count,
        largest_page_ir_bytes,
        warnings,
    })
}

fn detect_in_memory_format(file_name: &str, bytes: &[u8]) -> Result<SourceFormat> {
    if bytes.starts_with(b"%PDF-") {
        return Ok(SourceFormat::Pdf);
    }
    let extension = Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => Ok(SourceFormat::Pdf),
        "docx" | "docm" | "dotx" | "dotm" => Ok(SourceFormat::Docx),
        "xlsx" | "xlsm" | "xltx" | "xltm" | "xlam" => Ok(SourceFormat::Xlsx),
        "pptx" | "pptm" | "potx" | "potm" | "ppsx" | "ppam" | "sldx" | "sldm" => {
            Ok(SourceFormat::Pptx)
        }
        "flatopc" | "fopc" => Ok(SourceFormat::FlatOpc),
        _ => {
            if crate::document::flatopc::looks_like_prefix(bytes) {
                return Ok(SourceFormat::FlatOpc);
            }
            let package = crate::ooxml::ZipPackage::from_reader(Cursor::new(bytes), 1024 * 1024)?;
            let mut package = package;
            if package.contains("word/document.xml") {
                Ok(SourceFormat::Docx)
            } else if package.contains("xl/workbook.xml") {
                Ok(SourceFormat::Xlsx)
            } else if package.contains("ppt/presentation.xml") {
                Ok(SourceFormat::Pptx)
            } else {
                Err(Error::Unsupported(format!(
                    "cannot determine a supported in-memory format for {file_name:?}"
                )))
            }
        }
    }
}

struct BytePageSink<'a, F> {
    svg_options: SvgOptions,
    max_pages: usize,
    max_page_svg_bytes: usize,
    max_total_svg_bytes: usize,
    total_svg_bytes: usize,
    max_page_text_spans: usize,
    max_total_text_spans: usize,
    max_total_text_bytes: usize,
    total_text_spans: usize,
    total_text_bytes: usize,
    pages_emitted: usize,
    largest_page_ir_bytes: usize,
    warnings: Vec<String>,
    on_page: &'a mut F,
}

impl<'a, F> BytePageSink<'a, F> {
    fn new(options: &ConvertOptions, limits: ByteConvertLimits, on_page: &'a mut F) -> Self {
        Self {
            svg_options: SvgOptions {
                include_metadata: options.include_metadata,
                precision: options.precision,
            },
            max_pages: options.max_pages,
            max_page_svg_bytes: limits.max_page_svg_bytes,
            max_total_svg_bytes: limits.max_total_svg_bytes,
            total_svg_bytes: 0,
            max_page_text_spans: limits.max_page_text_spans,
            max_total_text_spans: limits.max_total_text_spans,
            max_total_text_bytes: limits.max_total_text_bytes,
            total_text_spans: 0,
            total_text_bytes: 0,
            pages_emitted: 0,
            largest_page_ir_bytes: 0,
            warnings: Vec::new(),
            on_page,
        }
    }

    fn finish(self) -> Result<(usize, usize, Vec<String>)> {
        if self.pages_emitted == 0 {
            return Err(Error::InvalidInput(
                "input contains no renderable pages".into(),
            ));
        }
        Ok((
            self.pages_emitted,
            self.largest_page_ir_bytes,
            self.warnings,
        ))
    }
}

impl<F> PageConsumer for BytePageSink<'_, F>
where
    F: FnMut(SvgPage) -> Result<()>,
{
    fn consume(&mut self, page: Page) -> Result<()> {
        if self.pages_emitted >= self.max_pages {
            return Err(Error::LimitExceeded(format!(
                "page count exceeds {}",
                self.max_pages
            )));
        }
        let estimated_ir_bytes = estimate_page_bytes(&page);
        let remaining_spans = self
            .max_total_text_spans
            .saturating_sub(self.total_text_spans)
            .min(self.max_page_text_spans);
        let remaining_text_bytes = self
            .max_total_text_bytes
            .saturating_sub(self.total_text_bytes);
        let (text_spans, text_was_truncated) =
            extract_text_spans(&page, remaining_spans, remaining_text_bytes);
        let text_bytes = text_spans.iter().map(|span| span.text.len()).sum::<usize>();
        self.total_text_spans += text_spans.len();
        self.total_text_bytes += text_bytes;
        self.largest_page_ir_bytes = self.largest_page_ir_bytes.max(estimated_ir_bytes);
        let mut page_warnings = page.warnings.clone();
        if text_was_truncated {
            page_warnings.push(
                "browser text selection/search layer reached its safety limit; later text may not be searchable or selectable".into(),
            );
        }
        self.warnings.extend(page_warnings.iter().cloned());
        let mut writer = LimitedBuffer::new(self.max_page_svg_bytes);
        let write_result = write_page(&page, &mut writer, self.svg_options);
        if writer.exceeded {
            return Err(Error::LimitExceeded(format!(
                "SVG page {} exceeds the {} byte preview limit",
                page.number, self.max_page_svg_bytes
            )));
        }
        write_result?;
        let svg_bytes = writer.bytes;
        let new_total = self
            .total_svg_bytes
            .checked_add(svg_bytes.len())
            .ok_or_else(|| Error::LimitExceeded("total SVG output size overflowed".into()))?;
        if new_total > self.max_total_svg_bytes {
            return Err(Error::LimitExceeded(format!(
                "SVG output exceeds the {} byte total preview limit",
                self.max_total_svg_bytes
            )));
        }
        let svg = String::from_utf8(svg_bytes).map_err(|error| {
            Error::InvalidInput(format!("SVG writer produced invalid UTF-8: {error}"))
        })?;
        self.total_svg_bytes = new_total;
        self.pages_emitted += 1;
        (self.on_page)(SvgPage {
            number: page.number,
            svg,
            width_points: page.width,
            height_points: page.height,
            node_count: page.nodes.len(),
            warning_count: page_warnings.len(),
            warnings: page_warnings,
            estimated_ir_bytes,
            text_spans,
        })
    }
}

fn extract_text_spans(
    page: &Page,
    max_spans: usize,
    max_text_bytes: usize,
) -> (Vec<TextSpan>, bool) {
    fn visit(
        nodes: &[Node],
        parent_transform: Matrix,
        output: &mut Vec<TextSpan>,
        text_bytes: &mut usize,
        max_spans: usize,
        max_text_bytes: usize,
        truncated: &mut bool,
    ) {
        for node in nodes {
            if *truncated {
                return;
            }
            match node {
                Node::Text {
                    x,
                    y,
                    runs,
                    anchor,
                    transform,
                    ..
                } => {
                    let text = runs.iter().map(|run| run.text.as_str()).collect::<String>();
                    if text.is_empty() {
                        continue;
                    }
                    let Some(next_text_bytes) = text_bytes.checked_add(text.len()) else {
                        *truncated = true;
                        return;
                    };
                    if output.len() >= max_spans || next_text_bytes > max_text_bytes {
                        *truncated = true;
                        return;
                    }
                    *text_bytes = next_text_bytes;
                    let width = runs.iter().map(estimated_run_width).sum::<f64>();
                    let font_size = runs
                        .iter()
                        .map(|run| run.font_size)
                        .filter(|size| size.is_finite() && *size > 0.0)
                        .fold(0.0_f64, f64::max)
                        .max(1.0);
                    let left = match anchor {
                        TextAnchor::Start => *x,
                        TextAnchor::Middle => *x - width / 2.0,
                        TextAnchor::End => *x - width,
                    };
                    output.push(TextSpan {
                        text,
                        x: left,
                        y: *y - font_size * 0.85,
                        width: width.max(font_size * 0.25),
                        height: font_size * 1.35,
                        font_size,
                        transform: compose(parent_transform, *transform),
                    });
                }
                Node::Group {
                    nodes, transform, ..
                } => visit(
                    nodes,
                    compose(parent_transform, *transform),
                    output,
                    text_bytes,
                    max_spans,
                    max_text_bytes,
                    truncated,
                ),
                Node::Path { .. } | Node::Image { .. } => {}
            }
        }
    }

    fn estimated_run_width(run: &crate::ir::TextRun) -> f64 {
        if let Some(width) = run
            .target_advance
            .filter(|width| width.is_finite() && *width > 0.0)
        {
            return width;
        }
        if run.glyph_x_offsets.len() > 1 {
            let first = run.glyph_x_offsets[0];
            let last = *run.glyph_x_offsets.last().unwrap_or(&first);
            let trailing = run.font_size * 0.55;
            return (last - first).abs() + trailing;
        }
        run.text
            .chars()
            .map(|character| {
                let code = u32::from(character);
                if matches!(code, 0x2e80..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff | 0x1f000..=0x1faff) {
                    run.font_size
                } else if character.is_whitespace() {
                    run.font_size * 0.3
                } else {
                    run.font_size * 0.55
                }
            })
            .sum()
    }

    let mut spans = Vec::new();
    let mut text_bytes = 0;
    let mut truncated = false;
    visit(
        &page.nodes,
        IDENTITY,
        &mut spans,
        &mut text_bytes,
        max_spans,
        max_text_bytes,
        &mut truncated,
    );
    (spans, truncated)
}

struct LimitedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl LimitedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(64 * 1024)),
            limit,
            exceeded: false,
        }
    }
}

impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > self.limit)
        {
            self.exceeded = true;
            return Err(std::io::Error::other("SVG output limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) trait PageConsumer {
    fn consume(&mut self, page: Page) -> Result<()>;
}

pub(crate) struct PageSink<'a> {
    output_directory: &'a Path,
    svg_options: SvgOptions,
    reports: Vec<PageReport>,
    max_pages: usize,
}

impl<'a> PageSink<'a> {
    fn new(output_directory: &'a Path, options: &ConvertOptions) -> Self {
        let reports = Vec::with_capacity(options.max_pages.min(1024));
        Self {
            output_directory,
            svg_options: SvgOptions {
                include_metadata: options.include_metadata,
                precision: options.precision,
            },
            reports,
            max_pages: options.max_pages,
        }
    }

    fn finish(self) -> Result<Vec<PageReport>> {
        if self.reports.is_empty() {
            return Err(Error::InvalidInput(
                "input contains no renderable pages".into(),
            ));
        }
        Ok(self.reports)
    }
}

impl PageConsumer for PageSink<'_> {
    fn consume(&mut self, page: Page) -> Result<()> {
        if self.reports.len() >= self.max_pages {
            return Err(Error::LimitExceeded(format!(
                "page count exceeds {}",
                self.max_pages
            )));
        }
        let file_name = format!("page-{:04}.svg", page.number);
        let final_path = self.output_directory.join(&file_name);
        let mut temporary = tempfile::NamedTempFile::new_in(self.output_directory)?;
        {
            let mut writer = BufWriter::with_capacity(64 * 1024, temporary.as_file_mut());
            write_page(&page, &mut writer, self.svg_options)?;
            writer.flush()?;
        }
        temporary
            .persist_noclobber(&final_path)
            .map_err(|error| error.error)?;
        let estimated_ir_bytes = estimate_page_bytes(&page);
        self.reports.push(PageReport {
            number: page.number,
            svg: file_name,
            width_points: page.width,
            height_points: page.height,
            node_count: page.nodes.len(),
            warning_count: page.warnings.len(),
            warnings: page.warnings,
            estimated_ir_bytes,
        });
        Ok(())
    }
}

fn estimate_page_bytes(page: &Page) -> usize {
    // Keep the existing serialized-size metric without allocating a second
    // copy of image data and paths for every page.
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, page).map_or(0, |()| counter.0)
}

#[derive(Default)]
struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("serialized page size overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_sniffing_reads_only_a_small_prefix_of_large_json() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("large.json");
        let mut file = std::fs::File::create(&input).unwrap();
        file.write_all(br#"{"type":"bar","labels":[],"series":[]}"#)
            .unwrap();
        file.set_len(512 * 1024 * 1024).unwrap();

        assert_eq!(
            read_format_prefix(&input).unwrap().len(),
            FORMAT_SNIFF_BYTES as usize
        );
        assert_eq!(SourceFormat::detect(&input).unwrap(), SourceFormat::Chart);
    }

    #[test]
    fn limited_file_reader_stops_at_the_configured_budget() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("growing.txt");
        fs::write(&input, b"12345").unwrap();

        assert!(matches!(
            read_limited_file(&input, 4, "test input"),
            Err(Error::LimitExceeded(message)) if message.contains("test input")
        ));
        assert_eq!(
            read_limited_file(&input, 5, "test input").unwrap(),
            b"12345"
        );
    }

    #[test]
    fn page_size_counter_matches_serialized_bytes_without_retaining_json() {
        let mut page = Page::new(1, 612.0, 792.0, "test");
        page.title = "日本語 / 中文 / quotes: \" \\ \n".into();
        page.description = "large path or image data".repeat(100_000);
        page.warnings.push("control: \t\r".into());
        assert_eq!(
            estimate_page_bytes(&page),
            serde_json::to_vec(&page).unwrap().len()
        );
    }
}
