//! Bounded HL7 FHIR XML previews.
//!
//! FHIR XML may contain protected clinical data and narrative XHTML. This
//! adapter validates resource envelopes and renders resource-type structure
//! only; clinical values, identifiers, narratives, references and terminology
//! services remain inert.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::Path;

use quick_xml::NsReader;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_FHIR_XML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FHIR_XML_EVENTS: usize = 1_000_000;
const MAX_FHIR_XML_NODES: usize = 500_000;
const MAX_FHIR_XML_DEPTH: usize = 128;
const MAX_FHIR_XML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_FHIR_XML_ROWS: usize = 100_000;
const MAX_FHIR_XML_RESOURCES: usize = 100_000;
const MAX_FHIR_XML_DISPLAY_BYTES: usize = 512;
const FHIR_NAMESPACE: &str = "http://hl7.org/fhir";

const RESOURCE_NAMES: &[&str] = &[
    "Account",
    "ActivityDefinition",
    "AdverseEvent",
    "AllergyIntolerance",
    "Appointment",
    "AppointmentResponse",
    "AuditEvent",
    "Basic",
    "Binary",
    "BiologicallyDerivedProduct",
    "BodyStructure",
    "Bundle",
    "CapabilityStatement",
    "CarePlan",
    "CareTeam",
    "CatalogEntry",
    "ChargeItem",
    "ChargeItemDefinition",
    "Claim",
    "ClaimResponse",
    "ClinicalImpression",
    "CodeSystem",
    "Communication",
    "CommunicationRequest",
    "CompartmentDefinition",
    "Composition",
    "ConceptMap",
    "Condition",
    "Consent",
    "Contract",
    "Coverage",
    "DetectedIssue",
    "Device",
    "DeviceDefinition",
    "DeviceMetric",
    "DeviceRequest",
    "DeviceUseStatement",
    "DiagnosticReport",
    "DocumentManifest",
    "DocumentReference",
    "DomainResource",
    "EffectEvidenceSynthesis",
    "Encounter",
    "Endpoint",
    "EnrollmentRequest",
    "EnrollmentResponse",
    "EpisodeOfCare",
    "EventDefinition",
    "Evidence",
    "EvidenceVariable",
    "ExampleScenario",
    "ExplanationOfBenefit",
    "FamilyMemberHistory",
    "Flag",
    "Goal",
    "GraphDefinition",
    "Group",
    "GuidanceResponse",
    "HealthcareService",
    "ImagingStudy",
    "Immunization",
    "ImmunizationEvaluation",
    "ImmunizationRecommendation",
    "ImplementationGuide",
    "InsurancePlan",
    "Invoice",
    "Library",
    "Linkage",
    "List",
    "Location",
    "Measure",
    "MeasureReport",
    "Media",
    "Medication",
    "MedicationAdministration",
    "MedicationDispense",
    "MedicationKnowledge",
    "MedicationRequest",
    "MedicationStatement",
    "MedicinalProduct",
    "MedicinalProductAuthorization",
    "MedicinalProductContraindication",
    "MedicinalProductIndication",
    "MedicinalProductIngredient",
    "MedicinalProductInteraction",
    "MedicinalProductManufactured",
    "MedicinalProductPackaged",
    "MedicinalProductPharmaceutical",
    "MedicinalProductUndesirableEffect",
    "MessageDefinition",
    "MessageHeader",
    "MolecularSequence",
    "NamingSystem",
    "NutritionOrder",
    "Observation",
    "OperationDefinition",
    "OperationOutcome",
    "Organization",
    "OrganizationAffiliation",
    "Parameters",
    "Patient",
    "PaymentNotice",
    "PaymentReconciliation",
    "Person",
    "PlanDefinition",
    "Practitioner",
    "PractitionerRole",
    "Procedure",
    "Provenance",
    "Questionnaire",
    "QuestionnaireResponse",
    "RelatedPerson",
    "RequestGroup",
    "ResearchDefinition",
    "ResearchElementDefinition",
    "ResearchStudy",
    "ResearchSubject",
    "RiskAssessment",
    "RiskEvidenceSynthesis",
    "Schedule",
    "SearchParameter",
    "ServiceRequest",
    "Slot",
    "Specimen",
    "StructureDefinition",
    "StructureMap",
    "Subscription",
    "Substance",
    "SupplyDelivery",
    "SupplyRequest",
    "Task",
    "TerminologyCapabilities",
    "TestReport",
    "TestScript",
    "ValueSet",
    "VerificationResult",
    "VisionPrescription",
];

#[derive(Default)]
struct Summary {
    root_type: String,
    resources: usize,
    bundles: usize,
    entries: usize,
    narratives: usize,
    contained: usize,
    extensions: usize,
    types: BTreeSet<String>,
    rows: Vec<Vec<String>>,
}

struct FhirXmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for FhirXmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "fhir-xml".into();
        if page.title.is_empty() {
            page.title = "FHIR XML".into();
        }
        page.description =
            "FHIR XML resource structure is rendered inertly; clinical values, narrative XHTML, identifiers and references are not displayed or resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let mut reader = NsReader::from_reader(Cursor::new(prefix));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    loop {
        match reader.read_resolved_event_into(&mut buffer) {
            Ok((ResolveResult::Bound(namespace), Event::Start(element)))
            | Ok((ResolveResult::Bound(namespace), Event::Empty(element))) => {
                let element_name = element.name();
                let local_name = crate::ooxml::local_name(element_name.as_ref());
                let local = String::from_utf8_lossy(local_name);
                return namespace.as_ref() == FHIR_NAMESPACE.as_bytes() && is_resource_name(&local);
            }
            Ok((_, Event::Eof)) | Err(_) => return false,
            _ => buffer.clear(),
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
        options.max_input_bytes.min(MAX_FHIR_XML_BYTES),
        "FHIR XML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_FHIR_XML_EVENTS),
            max_nodes: MAX_FHIR_XML_NODES,
            max_depth: MAX_FHIR_XML_DEPTH,
            max_text_bytes: MAX_FHIR_XML_TEXT_BYTES,
        },
        "FHIR XML",
    )?;
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| namespace != FHIR_NAMESPACE)
    {
        return Err(Error::InvalidInput(
            "FHIR XML root uses an unsupported namespace".into(),
        ));
    }
    if !is_resource_name(&root.name) {
        return Err(Error::InvalidInput(format!(
            "FHIR XML root '{}' is not a recognized resource",
            root.name
        )));
    }
    let mut summary = Summary {
        root_type: root.name.clone(),
        ..Summary::default()
    };
    collect_resources(&root, &mut summary, true)?;
    if summary.resources == 0 {
        return Err(Error::InvalidInput(
            "FHIR XML contains no resource elements".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Root",
        &summary.root_type,
        &format!(
            "resources={} bundles={}",
            summary.resources, summary.bundles
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Types",
        &summary.types.len().to_string(),
        &display_types(&summary.types),
    )?;
    push_row(
        &mut summary.rows,
        "Bundle entries",
        &summary.entries.to_string(),
        &format!(
            "contained={} narratives={}",
            summary.contained, summary.narratives
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Extensions",
        &summary.extensions.to_string(),
        "extension elements counted; values omitted",
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "FHIR XML".into(),
        },
        HtmlBlock::Paragraph {
            text: "HL7 FHIR XML resource structure is summarized without exposing clinical values or narrative content.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "FHIR XML clinical values, text, identifiers, names, addresses, telecom, coded displays, URLs, references, extensions and contained payloads are omitted or redacted".into(),
        "FHIR XML schemas, profiles, terminology services, narrative XHTML, URLs and clinical operations are never fetched, validated or executed".into(),
    ];
    let mut page_sink = FhirXmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_resources(element: &XmlElement, summary: &mut Summary, is_resource: bool) -> Result<()> {
    if is_resource && is_resource_name(&element.name) {
        summary.resources = summary.resources.saturating_add(1);
        if summary.resources > MAX_FHIR_XML_RESOURCES {
            return Err(Error::LimitExceeded(format!(
                "FHIR XML resources exceed {MAX_FHIR_XML_RESOURCES}"
            )));
        }
        summary.types.insert(element.name.clone());
        if element.name.eq_ignore_ascii_case("Bundle") {
            summary.bundles = summary.bundles.saturating_add(1);
        }
    }
    if element.name.eq_ignore_ascii_case("entry") {
        summary.entries = summary.entries.saturating_add(1);
    }
    if element.name.eq_ignore_ascii_case("text")
        && element
            .children
            .iter()
            .any(|child| child.name.eq_ignore_ascii_case("div"))
    {
        summary.narratives = summary.narratives.saturating_add(1);
    }
    if element.name.eq_ignore_ascii_case("contained") {
        summary.contained = summary.contained.saturating_add(1);
    }
    if element.name.eq_ignore_ascii_case("extension") {
        summary.extensions = summary.extensions.saturating_add(1);
    }
    for child in &element.children {
        let child_is_resource = is_resource_name(&child.name)
            && (element.name.eq_ignore_ascii_case("resource")
                || element.name.eq_ignore_ascii_case("Bundle")
                || element.name.eq_ignore_ascii_case("entry")
                || element.name.eq_ignore_ascii_case("contained")
                || is_resource);
        collect_resources(child, summary, child_is_resource)?;
    }
    Ok(())
}

fn is_resource_name(name: &str) -> bool {
    RESOURCE_NAMES
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

fn display_types(types: &BTreeSet<String>) -> String {
    if types.is_empty() {
        return "none".into();
    }
    let mut values = types.iter().take(16).cloned().collect::<Vec<_>>();
    if types.len() > values.len() {
        values.push(format!("+{} more", types.len() - values.len()));
    }
    values.join(", ")
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_FHIR_XML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "FHIR XML rows exceed {MAX_FHIR_XML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_FHIR_XML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_FHIR_XML_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
