//! PlantUML diagram parser and SVG vector renderer.
//!
//! Supports PlantUML sequence diagrams (`participant`, `actor`, `A -> B : text`, `A --> B : text`)
//! and component / class / flow diagrams (`[A] -> [B]`, `A --> B : label`).

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::diagram::{
    DiagramEdge, DiagramGraph, DiagramNode, NodeShape, SequenceDiagram, SequenceMessage,
    SequenceParticipant, layout_and_render_graph, layout_and_render_sequence,
};
use crate::error::{Error, Result};

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "plantuml input")?;
    let source = String::from_utf8(bytes)
        .map_err(|e| Error::InvalidInput(format!("plantuml file is not valid UTF-8: {e}")))?;

    if is_plantuml_sequence_diagram(&source) {
        let seq = parse_plantuml_sequence(&source)?;
        let page = layout_and_render_sequence(&seq, options)?;
        sink.consume(page)?;
    } else {
        let graph = parse_plantuml_graph(&source)?;
        let page = layout_and_render_graph(&graph, options)?;
        sink.consume(page)?;
    }

    Ok(Vec::new())
}

/// True when `source` should be parsed as a sequence diagram rather than a
/// structural (class/component/use-case/...) graph.
///
/// A class diagram commonly pairs `class`/`interface` blocks with a plain
/// association line carrying multiplicities and a label, e.g.
/// `Dog "1" --> "1" Owner : belongs to`. That line's arrow-plus-colon shape
/// is indistinguishable from a sequence-diagram message in isolation, so
/// without the class-diagram-marker override below, a single such line flips
/// classification for the whole file and the class/inheritance content is
/// never parsed. Those markers only appear in class diagrams, so their
/// presence anywhere in the file settles it either way.
fn is_plantuml_sequence_diagram(source: &str) -> bool {
    let has_class_diagram_marker = source.lines().any(|l| {
        let trimmed = l.trim();
        trimmed.starts_with("class ")
            || trimmed.starts_with("abstract class ")
            || trimmed.starts_with("interface ")
            || trimmed.starts_with("enum ")
            || trimmed.contains("<|--")
            || trimmed.contains("--|>")
            || trimmed.contains("..|>")
            || trimmed.contains("<|..")
    });
    if has_class_diagram_marker {
        return false;
    }
    source.lines().any(|l| {
        let trimmed = l.trim();
        trimmed.starts_with("participant ")
            || trimmed.starts_with("actor ")
            || trimmed.starts_with("boundary ")
            || trimmed.starts_with("control ")
            || trimmed.starts_with("entity ")
            || trimmed.starts_with("database ")
            || trimmed.starts_with("collections ")
            || trimmed.starts_with("queue ")
            || ((trimmed.contains("->")
                || trimmed.contains("<-")
                || trimmed.contains("-->")
                || trimmed.contains("<--"))
                && trimmed.contains(':')
                && !trimmed.starts_with('[')
                && !trimmed.starts_with("class ")
                && !trimmed.contains("<|--")
                && !trimmed.contains("--|>"))
    })
}

const SEQUENCE_PARTICIPANT_PREFIXES: [&str; 8] = [
    "participant ",
    "actor ",
    "boundary ",
    "control ",
    "entity ",
    "database ",
    "collections ",
    "queue ",
];

pub fn parse_plantuml_sequence(source: &str) -> Result<SequenceDiagram> {
    let mut participants = Vec::new();
    let mut participant_info = Vec::new();
    let mut messages = Vec::new();
    let mut has_autonumber = false;
    // Maps a declared alias (`participant "Web App" as W`) to the display name
    // the diagram should show, so `W -> O: ...` arrows still resolve to the
    // right lifeline while the box itself shows "Web App", not the bare "W".
    let mut alias_to_display: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // First pass: collect every participant/actor/... declaration regardless of
    // where messages referencing it appear, since PlantUML allows declarations
    // and messages in either order.
    for line in source.lines() {
        let line = line.trim();
        if line == "autonumber" || line.starts_with("autonumber ") {
            has_autonumber = true;
            continue;
        }
        for prefix in SEQUENCE_PARTICIPANT_PREFIXES {
            if let Some(rest) = line.strip_prefix(prefix) {
                let is_actor = prefix.starts_with("actor");
                let rest = rest.trim();
                let (display, alias) = if let Some(alias_pos) = rest.find(" as ") {
                    (
                        rest[..alias_pos].trim().trim_matches('"').to_string(),
                        rest[alias_pos + 4..].trim().trim_matches('"').to_string(),
                    )
                } else {
                    let name = rest.trim_matches('"').to_string();
                    (name.clone(), name)
                };
                if !display.is_empty() {
                    if !participants.contains(&display) {
                        participants.push(display.clone());
                        participant_info.push(SequenceParticipant {
                            id: alias.clone(),
                            label: display.clone(),
                            is_actor,
                        });
                    }
                    if !alias.is_empty() {
                        alias_to_display.insert(alias, display);
                    }
                }
                break;
            }
        }
    }

    let resolve = |token: String,
                   participants: &mut Vec<String>,
                   participant_info: &mut Vec<SequenceParticipant>|
     -> String {
        let resolved = alias_to_display
            .get(&token)
            .cloned()
            .unwrap_or(token.clone());
        if !participants.contains(&resolved) {
            participants.push(resolved.clone());
            participant_info.push(SequenceParticipant {
                id: token,
                label: resolved.clone(),
                is_actor: false,
            });
        }
        resolved
    };

    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('@')
            || line.starts_with('\'')
            || line.starts_with("/'")
            || line.starts_with("autonumber")
            || line.starts_with("activate ")
            || line.starts_with("deactivate ")
            || line.starts_with("alt ")
            || line.starts_with("opt ")
            || line.starts_with("loop ")
            || line.starts_with("else")
            || line == "end"
            || line.is_empty()
        {
            continue;
        }

        if SEQUENCE_PARTICIPANT_PREFIXES
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            continue;
        }

        // Message parsing: A -> B : msg, A --> B : msg, A <- B : msg, A <-- B : msg
        let arrow_info = if line.contains("-->") {
            Some(("-->", true, false))
        } else if line.contains("<--") {
            Some(("<--", true, true))
        } else if line.contains("->") {
            Some(("->", false, false))
        } else if line.contains("<-") {
            Some(("<-", false, true))
        } else {
            None
        };

        if let Some((arrow, is_dotted, is_reverse, arrow_idx)) =
            arrow_info.and_then(|(arrow, d, r)| line.find(arrow).map(|idx| (arrow, d, r, idx)))
        {
            let left_side = line[..arrow_idx].trim().trim_matches('"').to_string();
            let remainder = &line[arrow_idx + arrow.len()..];
            let (right_side, text) = if let Some(colon_idx) = remainder.find(':') {
                (
                    remainder[..colon_idx].trim().trim_matches('"').to_string(),
                    remainder[colon_idx + 1..].trim().to_string(),
                )
            } else {
                (
                    remainder.trim().trim_matches('"').to_string(),
                    String::new(),
                )
            };

            let (from, to) = if is_reverse {
                (right_side, left_side)
            } else {
                (left_side, right_side)
            };

            if !from.is_empty() && !to.is_empty() {
                let from = resolve(from, &mut participants, &mut participant_info);
                let to = resolve(to, &mut participants, &mut participant_info);
                messages.push(SequenceMessage {
                    from,
                    to,
                    text,
                    is_dotted,
                });
            }
        }
    }

    if participants.is_empty() && messages.is_empty() {
        return Err(Error::InvalidInput(
            "no sequence elements found in PlantUML".into(),
        ));
    }

    Ok(SequenceDiagram {
        participants,
        participant_info,
        messages,
        has_autonumber,
        raw_source: source.to_string(),
    })
}

pub fn parse_plantuml_graph(source: &str) -> Result<DiagramGraph> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut node_set = std::collections::HashSet::new();

    let mut alias_map = std::collections::HashMap::new();

    let add_node = |id: &str,
                    label: &str,
                    shape: NodeShape,
                    nodes: &mut Vec<DiagramNode>,
                    node_set: &mut std::collections::HashSet<String>,
                    alias_map: &mut std::collections::HashMap<String, String>| {
        let clean_id = id
            .trim()
            .trim_matches(|c| c == '[' || c == ']' || c == '"' || c == '(' || c == ')')
            .to_string();
        let clean_label = label
            .trim()
            .trim_matches(|c| c == '[' || c == ']' || c == '"' || c == '(' || c == ')')
            .to_string();
        if !clean_id.is_empty() {
            alias_map.insert(clean_id.clone(), clean_id.clone());
            if !clean_label.is_empty() {
                alias_map.insert(clean_label.clone(), clean_id.clone());
            }
            if !node_set.contains(&clean_id) {
                node_set.insert(clean_id.clone());
                nodes.push(DiagramNode {
                    id: clean_id.clone(),
                    label: if clean_label.is_empty() {
                        clean_id
                    } else {
                        clean_label
                    },
                    shape,
                    x: 0.0,
                    y: 0.0,
                    width: 140.0,
                    height: 50.0,
                });
            }
        }
    };

    let parse_decl = |rest: &str| -> (String, String) {
        let t = rest.trim().trim_end_matches('{').trim();
        if let Some(as_pos) = t.find(" as ") {
            let label = t[..as_pos].trim().trim_matches('"').to_string();
            let alias = t[as_pos + 4..].trim().trim_matches('"').to_string();
            (alias, label)
        } else {
            let name = t.trim_matches('"').to_string();
            (name.clone(), name)
        }
    };

    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('@')
            || line.starts_with('\'')
            || line.starts_with("/'")
            || line.is_empty()
        {
            continue;
        }

        let decl_shape = if let Some(rest) = line.strip_prefix("class ") {
            Some((rest, NodeShape::Box))
        } else if let Some(rest) = line.strip_prefix("database ") {
            Some((rest, NodeShape::Cylinder))
        } else if let Some(rest) = line
            .strip_prefix("interface ")
            .or_else(|| line.strip_prefix("actor "))
        {
            Some((rest, NodeShape::Circle))
        } else if let Some(rest) = line
            .strip_prefix("rectangle ")
            .or_else(|| line.strip_prefix("package "))
            .or_else(|| line.strip_prefix("node "))
            .or_else(|| line.strip_prefix("component "))
            .or_else(|| line.strip_prefix("artifact "))
            .or_else(|| line.strip_prefix("frame "))
            .or_else(|| line.strip_prefix("card "))
        {
            Some((rest, NodeShape::Box))
        } else if let Some(rest) = line
            .strip_prefix("cloud ")
            .or_else(|| line.strip_prefix("storage "))
            .or_else(|| line.strip_prefix("folder "))
        {
            Some((rest, NodeShape::Cylinder))
        } else {
            line.strip_prefix("usecase ")
                .or_else(|| line.strip_prefix("state "))
                .map(|rest| (rest, NodeShape::Rounded))
        };

        if let Some((rest, shape)) = decl_shape {
            let (name_part, extends_list, implements_list) = split_class_relations(rest);
            let (id, label) = parse_decl(name_part);
            add_node(
                &id,
                &label,
                shape,
                &mut nodes,
                &mut node_set,
                &mut alias_map,
            );
            for parent in &extends_list {
                add_node(
                    parent,
                    parent,
                    NodeShape::Box,
                    &mut nodes,
                    &mut node_set,
                    &mut alias_map,
                );
                edges.push(DiagramEdge {
                    from: id.clone(),
                    to: parent.clone(),
                    label: None,
                });
            }
            for interface in &implements_list {
                add_node(
                    interface,
                    interface,
                    NodeShape::Circle,
                    &mut nodes,
                    &mut node_set,
                    &mut alias_map,
                );
                edges.push(DiagramEdge {
                    from: id.clone(),
                    to: interface.clone(),
                    label: None,
                });
            }
            continue;
        }

        // Broad set of UML edge connection arrows
        let recognized_arrows = [
            ("<|--", true),
            ("<|..", true),
            ("--|>", false),
            ("..|>", false),
            ("<--", true),
            ("-->", false),
            ("..>", false),
            ("<..", true),
            ("*--", false),
            ("--*", false),
            ("o--", false),
            ("--o", false),
            ("<-", true),
            ("->", false),
            ("--", false),
            ("..", false),
        ];

        let mut matched_arrow = None;
        for (arr, is_rev) in recognized_arrows {
            if let Some(idx) = line.find(arr) {
                matched_arrow = Some((arr, is_rev, idx));
                break;
            }
        }

        if let Some((arr, is_rev, arr_idx)) = matched_arrow {
            let left_str = line[..arr_idx].trim();
            let remainder = &line[arr_idx + arr.len()..];
            let (right_str, label) = if let Some(colon_idx) = remainder.find(':') {
                (
                    remainder[..colon_idx].trim(),
                    Some(remainder[colon_idx + 1..].trim().to_string()),
                )
            } else {
                (remainder.trim(), None)
            };

            let left_name = strip_endpoint_multiplicity(left_str, false);
            let right_name = strip_endpoint_multiplicity(right_str, true);

            let (from_name, to_name) = if is_rev {
                (right_name, left_name)
            } else {
                (left_name, right_name)
            };

            let from_key = alias_map.get(&from_name).cloned().unwrap_or(from_name);
            let to_key = alias_map.get(&to_name).cloned().unwrap_or(to_name);

            if !from_key.is_empty() && !to_key.is_empty() {
                add_node(
                    &from_key,
                    &from_key,
                    NodeShape::Rounded,
                    &mut nodes,
                    &mut node_set,
                    &mut alias_map,
                );
                add_node(
                    &to_key,
                    &to_key,
                    NodeShape::Rounded,
                    &mut nodes,
                    &mut node_set,
                    &mut alias_map,
                );
                edges.push(DiagramEdge {
                    from: from_key,
                    to: to_key,
                    label,
                });
            }
        }
    }

    if nodes.is_empty() {
        return Err(Error::InvalidInput(
            "no graph nodes or edges found in PlantUML".into(),
        ));
    }

    Ok(DiagramGraph {
        title: None,
        is_directed: true,
        nodes,
        edges,
        raw_source: source.to_string(),
    })
}

/// Splits a Java-style `class Dog extends Animal implements Pet, Runnable`
/// declaration into the class's own name plus its extends/implements
/// targets. This is documented, real-world PlantUML syntax (plantuml.com/
/// class-diagram), an alternative to declaring `class Dog` and the
/// relationship separately as `Animal <|-- Dog`. Without splitting it here,
/// the whole clause was read as one literal class name — "Dog extends
/// Animal" — instead of a Dog node with an inheritance edge to Animal.
fn split_class_relations(rest: &str) -> (&str, Vec<String>, Vec<String>) {
    let mut name_part = rest.trim().trim_end_matches('{').trim();
    let mut implements_list = Vec::new();
    let mut extends_list = Vec::new();
    if let Some(idx) = name_part.find(" implements ") {
        let after = &name_part[idx + " implements ".len()..];
        implements_list = after
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        name_part = name_part[..idx].trim();
    }
    if let Some(idx) = name_part.find(" extends ") {
        let after = &name_part[idx + " extends ".len()..];
        extends_list = after
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        name_part = name_part[..idx].trim();
    }
    (name_part, extends_list, implements_list)
}

/// Splits a UML association endpoint into its class/component name, dropping
/// a multiplicity annotation adjacent to the arrow (`Dog "1" --> "0..*" Owner`).
/// `multiplicity_first` is true for the right-hand endpoint, where the
/// multiplicity precedes the name; false for the left-hand endpoint, where it
/// follows. Without this, an association's multiplicity was left embedded in
/// the endpoint text (e.g. the left endpoint of `Dog "1" -->` became the
/// literal string `Dog "1`) since the general-purpose bracket/quote trim only
/// strips matching characters from the very edges of the string, not an
/// interior quoted token.
fn strip_endpoint_multiplicity(side: &str, multiplicity_first: bool) -> String {
    let s = side.trim();
    let split = if multiplicity_first {
        s.strip_prefix('"').and_then(|rest| {
            rest.find('"')
                .map(|end| (&rest[..end], rest[end + 1..].trim()))
        })
    } else {
        s.strip_suffix('"').and_then(|rest| {
            rest.rfind('"')
                .map(|start| (&rest[start + 1..], rest[..start].trim()))
        })
    };
    let name = match split {
        Some((multiplicity, name)) if is_multiplicity_token(multiplicity) => name,
        _ => s,
    };
    name.trim_matches(|c| c == '[' || c == ']' || c == '"')
        .to_string()
}

fn is_multiplicity_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == '*' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `participant "Web App" as W` declares W as a short reference used in
    /// arrows, but the box itself must show the quoted display name. A prior
    /// version used whichever string followed " as " for both the lifeline's
    /// identity *and* its rendered label, so "Web App" never appeared —
    /// only the bare alias "W" did, on every participant that used one.
    #[test]
    fn participant_alias_resolves_to_its_display_name() {
        let source = "participant \"Web App\" as W\nactor Customer as C\nC -> W: place order\n";
        let seq = parse_plantuml_sequence(source).unwrap();
        assert_eq!(seq.participants, vec!["Web App", "Customer"]);
        assert_eq!(seq.messages.len(), 1);
        assert_eq!(seq.messages[0].from, "Customer");
        assert_eq!(seq.messages[0].to, "Web App");
        assert!(!seq.participant_info[0].is_actor);
        assert!(seq.participant_info[1].is_actor);
    }

    #[test]
    fn participant_without_alias_uses_its_own_name_as_the_label() {
        let seq = parse_plantuml_sequence("Alice -> Bob: hi\n").unwrap();
        assert_eq!(seq.participants, vec!["Alice", "Bob"]);
        assert_eq!(seq.messages[0].from, "Alice");
        assert_eq!(seq.messages[0].to, "Bob");
    }

    #[test]
    fn preserves_the_autonumber_directive() {
        let seq = parse_plantuml_sequence("autonumber\nAlice -> Bob: hi\n").unwrap();
        assert!(seq.has_autonumber);
    }

    #[test]
    fn alias_declared_after_first_use_still_resolves() {
        // PlantUML does not require declarations before the messages that use
        // them; the alias map must be built from a full pre-scan.
        let source =
            "W -> O: go\nparticipant \"Web App\" as W\nparticipant \"Order Service\" as O\n";
        let seq = parse_plantuml_sequence(source).unwrap();
        assert_eq!(seq.messages[0].from, "Web App");
        assert_eq!(seq.messages[0].to, "Order Service");
    }

    /// A real class diagram: `class` blocks, inheritance (`<|--`), interface
    /// realization (`..|>`), and one plain association carrying multiplicities
    /// and a label. Only that last line looks like a sequence-diagram message
    /// on its own; the file as a whole must still be routed to the graph
    /// parser so the class/inheritance content is not silently dropped.
    #[test]
    fn class_diagram_with_labeled_association_is_not_misread_as_a_sequence_diagram() {
        let source = "class Animal {\n  +String name\n}\nclass Dog\ninterface Pet\nAnimal <|-- Dog\nDog ..|> Pet\nDog \"1\" --> \"1\" Owner : belongs to\nclass Owner\n";
        assert!(!is_plantuml_sequence_diagram(source));
        let graph = parse_plantuml_graph(source).unwrap();
        let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"Animal"), "{ids:?}");
        assert!(ids.contains(&"Dog"), "{ids:?}");
        assert!(ids.contains(&"Owner"), "{ids:?}");
        let association = graph
            .edges
            .iter()
            .find(|e| e.label.as_deref() == Some("belongs to"))
            .expect("Dog -> Owner association edge");
        assert_eq!(association.from, "Dog");
        assert_eq!(association.to, "Owner");
    }

    #[test]
    fn a_genuine_sequence_diagram_is_still_detected_as_one() {
        assert!(is_plantuml_sequence_diagram(
            "participant Alice\nAlice -> Bob: hi\n"
        ));
    }

    /// `Dog "1" --> "1" Owner` must yield endpoint names "Dog" and "Owner",
    /// not the multiplicity-contaminated "Dog \"1" / "1\" Owner" that a plain
    /// edge-only bracket/quote trim would leave behind, since the quoted
    /// multiplicity sits inside the endpoint text, not at its very edge.
    #[test]
    fn association_multiplicity_is_stripped_from_endpoint_names() {
        assert_eq!(strip_endpoint_multiplicity("Dog \"1\"", false), "Dog");
        assert_eq!(strip_endpoint_multiplicity("\"1\" Owner", true), "Owner");
        assert_eq!(strip_endpoint_multiplicity("\"0..*\" Owner", true), "Owner");
        // No multiplicity present: falls back to ordinary bracket/quote trim.
        assert_eq!(strip_endpoint_multiplicity("[A]", false), "A");
        assert_eq!(strip_endpoint_multiplicity("Plain", true), "Plain");
    }

    #[test]
    fn class_extends_and_implements_split_into_name_and_relation_targets() {
        assert_eq!(
            split_class_relations("Dog extends Animal"),
            ("Dog", vec!["Animal".to_string()], vec![])
        );
        assert_eq!(
            split_class_relations("Dog implements Pet, Runnable {"),
            (
                "Dog",
                vec![],
                vec!["Pet".to_string(), "Runnable".to_string()]
            )
        );
        assert_eq!(
            split_class_relations("Dog extends Animal implements Pet"),
            ("Dog", vec!["Animal".to_string()], vec!["Pet".to_string()])
        );
        // No relation clause: the whole (trimmed) string is the class name.
        assert_eq!(
            split_class_relations("Animal {"),
            ("Animal", vec![], vec![])
        );
    }

    #[test]
    fn java_style_class_declaration_produces_a_clean_node_and_inheritance_edge() {
        let source = "class Animal\nclass Dog extends Animal\n";
        let graph = parse_plantuml_graph(source).unwrap();
        let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["Animal", "Dog"]);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from, "Dog");
        assert_eq!(graph.edges[0].to, "Animal");
    }
}
