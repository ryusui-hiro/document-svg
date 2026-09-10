//! What a style's shape name draws.
//!
//! `canonical_shape` resolves the name draw.io writes onto one this module
//! draws, and `shape_paths` builds that shape's geometry in its own box.

use super::*;

/// Resolve draw.io's shape aliases onto the small set this module draws.
///
/// Shape-library names (`shape=mxgraph.<library>.<name>`) are matched on their
/// last segment where the library is one whose shapes are plain flowchart or
/// basic geometry; everything else falls through to the caller's placeholder.
pub(super) fn canonical_shape(name: &str) -> Option<&'static str> {
    let tail = name.rsplit('.').next().unwrap_or(name);
    let library_shape = name.starts_with("mxgraph.flowchart.")
        || name.starts_with("mxgraph.basic.")
        || name.starts_with("mxgraph.arrows.")
        || name.starts_with("mxgraph.floorplan.");
    let candidate = if library_shape { tail } else { name };
    Some(match candidate {
        "" | "rect" | "rectangle" | "box" | "rounded_rectangle" | "rounded_rectangle_2" => {
            "rectangle"
        }
        "ellipse" | "oval" | "circle" | "start_1" | "start_2" | "terminator"
        | "on-page_reference" | "connector" => "ellipse",
        "doubleellipse" | "double_ellipse" => "doubleEllipse",
        "rhombus" | "diamond" | "decision" | "sort" => "rhombus",
        "triangle" | "isometric_triangle" => "triangle",
        "hexagon" | "preparation" => "hexagon",
        "parallelogram" | "data" | "input_output" => "parallelogram",
        "trapezoid" | "manual_operation" => "trapezoid",
        "step" | "sequential_data" => "step",
        "process" | "predefined_process" | "subprocess" => "process",
        "cylinder" | "cylinder2" | "cylinder3" | "datastore" | "database" | "direct_data"
        | "stored_data" => "cylinder",
        "cloud" | "cloud_1" | "cloud_2" => "cloud",
        "document" | "document_1" | "document_2" => "document",
        "multidocument" | "multi-document" | "multipledocuments" => "multiDocument",
        "note" | "note2" | "annotation_1" | "annotation_2" => "note",
        "card" | "punched_card" => "card",
        "internalstorage" | "internal_storage" => "internalStorage",
        "cube" | "isocube" => "cube",
        "tape" | "paper_tape" | "sequential_access_storage" => "tape",
        "actor" | "umlactor" | "user" => "actor",
        "or" | "summing_junction_2" => "or",
        "xor" => "xor",
        "datastorage" | "storage" | "direct_access_storage" => "dataStorage",
        "delay" => "delay",
        "display" => "display",
        "manualinput" | "manual_input" => "manualInput",
        "offpageconnector" | "off-page_reference" => "offPageConnector",
        "looplimit" | "loop_limit" => "loopLimit",
        "collate" | "collate_1" => "collate",
        "extract" | "extract_1" | "extract_or_measurement" => "extract",
        "merge" | "merge_1" | "merge_or_storage" => "merge",
        "cross" => "cross",
        "singlearrow" | "arrow" => "singleArrow",
        "doublearrow" => "doubleArrow",
        "swimlane" => "swimlane",
        "group" => "group",
        "umllifeline" | "lifeline" => "umlLifeline",
        "umlframe" | "frame" => "umlFrame",
        "message" | "envelope" => "message",
        "callout" => "callout",
        "text" => "text",
        // mxLabel is a rectangle that carries an icon beside its text, not a
        // bare label: it draws its fill and border like any other shape.
        "label" => "rectangle",
        "image" => "image",
        "line" | "hline" => "line",
        "waypoint" => "waypoint",
        // Floor plan walls and openings are geometry rather than stencils, so
        // draw.io implements them in code and this module can too.
        "wall" => "wall",
        "wallcorner" => "wallCorner",
        "window" => "window",
        "doorleft" => "doorLeft",
        "doorright" => "doorRight",
        "doordouble" => "doorDouble",
        "wallu" => "wallU",
        // A cloud service tile is a plain box; the glyph it carries is named
        // separately by the style and drawn on top of it.
        "mxgraph.aws4.resourceicon" | "mxgraph.aws4.producticon" | "mxgraph.aws4.group" => {
            "rectangle"
        }
        "mxgraph.gcp2.doublerect" => "doubleRect",
        // A BPMN activity is a box whose corner style and size the style names
        // itself rather than through `rounded`.
        "mxgraph.bpmn.task" | "mxgraph.bpmn.activity" | "mxgraph.bpmn2.task" => "bpmnTask",
        "mxgraph.arrows2.arrow" => "singleArrow",
        "startstate" => "startState",
        "component" => "component",
        // Plain style names draw.io registers without a library prefix.
        "umldestroy" => "umlDestroy",
        "lollipop" => "lollipop",
        "requiredinterface" => "requiredInterface",
        "corner" => "corner",
        "switch" => "switch",
        "module" => "module",
        "crossbar" => "crossbar",
        "dimension" => "dimension",
        "partconcellipse" => "partConcEllipse",
        // The AWS 3D set's flat connectors, which lie on the ground rather than
        // standing on a box.
        "mxgraph.aws3d.dashedarrowlessedge" => "isometricDashedEdge",
        "mxgraph.aws3d.dashededge" => "isometricDashedEdgeOne",
        "mxgraph.aws3d.dashededgedouble" => "isometricDashedEdgeTwo",
        "mxgraph.aws3d.arrowne" => "isometricArrowNE",
        "mxgraph.aws3d.arrowse" => "isometricArrowSE",
        "mxgraph.aws3d.arrowsw" => "isometricArrowSW",
        "mxgraph.aws3d.arrownw" => "isometricArrowNW",
        "mxgraph.aws3d.arrowlessne" => "isometricArrowlessNE",
        "arc" => "basicArc",
        "pie" => "pie",
        "drop" => "drop",
        "obtuse_triangle" => "obtuseTriangle",
        "polygon" => "polyShape",
        "mxgraph.mockup.markup.line" => "mockupLine",
        "mxgraph.mockup.buttons.button" => "mockupButton",
        "mxgraph.mockup.forms.checkbox" => "mockupCheckbox",
        "mxgraph.bootstrap.x" => "crossLines",
        // SysML draws its activity and flow nodes in code rather than as
        // stencils. Each is a rounded body with square ports let into it.
        "mxgraph.sysml.flowfinal" => "sysmlFlowFinal",
        "mxgraph.sysml.iscontrol" => "sysmlIsControl",
        "mxgraph.sysml.objflowl" => "sysmlObjectFlowLeft",
        "mxgraph.sysml.objflowr" => "sysmlObjectFlowRight",
        "mxgraph.sysml.itemflowleft" => "sysmlItemFlowLeft",
        "mxgraph.sysml.itemflowright" => "sysmlItemFlowRight",
        "mxgraph.sysml.paramdgm" => "sysmlParametricDiagram",
        "mxgraph.sysml.port1" => "sysmlPort",
        "isorectangle" => "isoRectangle",
        "isocube2" => "isoCube2",
        "curlybracket" => "curlyBracket",
        "mxgraph.infographic.ribbonsimple" => "ribbonSimple",
        "mxgraph.infographic.barcallout" => "barCallout",
        "rectcallout" => "rectCallout",
        "roundrectcallout" => "roundRectCallout",
        "mxgraph.infographic.cylinder" => "infographicCylinder",
        "mxgraph.infographic.shadedcube" => "shadedCube",
        "mxgraph.mockup.markup.curlybrace" => "curlyBrace",
        "mxgraph.arrows2.uturnarrow" => "uTurnArrow",
        "stairs" => "stairs",
        "stairsrest" => "stairsRest",
        "room" => "room",
        "doorbypass" => "doorBypass",
        "mxgraph.bootstrap.rrect" => "bootstrapRoundRect",
        "mxgraph.bootstrap.checkbox" => "bootstrapCheckbox",
        "mxgraph.bootstrap.horlines" => "horLines",
        // A button rounded on one edge only, so that a row or column of them
        // reads as one control. Every library draws these the same way.
        "mxgraph.bootstrap.topbutton" | "mxgraph.mockup.containers.topbutton" => "topButton",
        "mxgraph.bootstrap.bottombutton" | "mxgraph.mockup.containers.bottombutton" => {
            "bottomButton"
        }
        "mxgraph.bootstrap.leftbutton"
        | "mxgraph.mockup.leftbutton"
        | "mxgraph.mockup.containers.leftbutton" => "leftButton",
        "mxgraph.bootstrap.rightbutton"
        | "mxgraph.mockup.rightbutton"
        | "mxgraph.mockup.containers.rightbutton" => "rightButton",
        "mxgraph.mockup.rrect"
        | "mxgraph.mockup.misc.rrect"
        | "mxgraph.mockup.forms.rrect"
        | "mxgraph.mockup.containers.rrect" => "bootstrapRoundRect",
        // A rectangle inset from its own box by the margins the style names.
        "mxgraph.mockup.containers.marginrect" => "marginRoundRect",
        "mxgraph.mockup.containers.marginrect2" | "mxgraph.gmdl.marginrect" => "marginRect",
        // A box open along the bottom, for the top of a form field.
        "mxgraph.mockup.forms.urect" => "uRect",
        // An anchor is a handle to draw connectors from; it has no outline of
        // its own, which is why draw.io paints nothing for one.
        "mxgraph.bootstrap.anchor"
        | "mxgraph.mockup.anchor"
        | "mxgraph.mockup.misc.anchor"
        | "mxgraph.mockup.forms.anchor"
        | "mxgraph.mockup.containers.anchor"
        | "mxgraph.ios7ui.anchor"
        | "mxgraph.android.anchor" => "group",
        "mxgraph.mockup.forms.searchbox" => "searchBox",
        "mxgraph.mockup.forms.combobox" => "comboBox",
        "mxgraph.mockup.graphics.simpleicon" => "simpleIcon",
        "mxgraph.mockup.graphics.icongrid" => "iconGrid",
        "mxgraph.lean_mapping.outside_sources" => "outsideSources",
        "mxgraph.lean_mapping.manufacturing_process" => "manufacturingProcess",
        "mxgraph.lean_mapping.schedule" => "rectangle",
        "mxgraph.lean_mapping.data_box" => "dataBox",
        "mxgraph.lean_mapping.push_arrow" => "pushArrow",
        "mxgraph.lean_mapping.physical_pull" => "physicalPull",
        "mxgraph.lean_mapping.inventory_box" => "inventoryBox",
        "mxgraph.lean_mapping.timeline2" => "leanTimeline",
        "mxgraph.lean_mapping.fifo_lane" => "fifoLane",
        "mxgraph.ios7ui.horlines" => "horLines",
        "mxgraph.ios7ui.phone" => "iosPhone",
        "mxgraph.ios7ui.icongrid" => "iosIconGrid",
        "mxgraph.rackgeneral.container" | "mxgraph.rack.general.1u_rack_unit" => "rackContainer",
        "plus" => "cross",
        "folder" | "umlpackage" | "mxgraph.sysml.package" | "package" => "folder",
        "endstate" => "endState",
        "partialrectangle" => "partialRectangle",
        // A table is a swimlane: a title band over the rows it holds, and each
        // row is one too, with no band of its own.
        "table" | "tablerow" => "swimlane",
        _ => return None,
    })
}

/// Build the geometry for one shape in its own upright bounding box.
///
/// Returns `None` for a name this module does not draw, which the caller turns
/// into a placeholder rectangle plus a warning.
pub(super) fn shape_paths(name: &str, rect: Rect, style: &Style) -> Option<Paths> {
    let Rect { x, y, .. } = rect;
    let (width, height) = (rect.width, rect.height);
    let (right, bottom) = (rect.right(), rect.bottom());
    let (cx, cy) = rect.center();
    Some(match name {
        "rectangle" | "image" | "swimlane" => {
            if style.flag("rounded") {
                Paths::new(rounded_rect_path(rect, corner_radius(rect, style)))
            } else {
                Paths::new(rectangle_path(rect))
            }
        }
        "text" => Paths::new(rectangle_path(rect)),
        "ellipse" => Paths::new(ellipse_path(rect)),
        "doubleEllipse" => {
            let inset = (width / 5.0).min(height / 5.0).min(4.0);
            Paths::with(ellipse_path(rect), vec![ellipse_path(rect.grow(-inset))])
        }
        "rhombus" => Paths::new(polygon_path(&[(cx, y), (right, cy), (cx, bottom), (x, cy)])),
        "triangle" => Paths::new(polygon_path(&[(x, y), (right, cy), (x, bottom)])),
        "hexagon" => Paths::new(polygon_path(&[
            (x + width * 0.25, y),
            (x + width * 0.75, y),
            (right, cy),
            (x + width * 0.75, bottom),
            (x + width * 0.25, bottom),
            (x, cy),
        ])),
        "parallelogram" => {
            let dx = width * style.number("size", 0.2).clamp(0.0, 1.0);
            Paths::new(polygon_path(&[
                (x, bottom),
                (x + dx, y),
                (right, y),
                (right - dx, bottom),
            ]))
        }
        "trapezoid" => {
            let dx = width * style.number("size", 0.2).clamp(0.0, 0.5);
            Paths::new(polygon_path(&[
                (x, bottom),
                (x + dx, y),
                (right - dx, y),
                (right, bottom),
            ]))
        }
        "step" => {
            let dx = width * style.number("size", 0.2).clamp(0.0, 1.0);
            Paths::new(polygon_path(&[
                (x, y),
                (right - dx, y),
                (right, cy),
                (right - dx, bottom),
                (x, bottom),
                (x + dx, cy),
            ]))
        }
        "process" => {
            let dx = if style.flag("fixedsize") {
                style.number("size", 20.0)
            } else {
                width * style.number("size", 0.1).clamp(0.0, 0.5)
            };
            let outline = if style.flag("rounded") {
                rounded_rect_path(rect, corner_radius(rect, style))
            } else {
                rectangle_path(rect)
            };
            Paths::with(
                outline,
                vec![
                    line_path((x + dx, y), (x + dx, bottom)),
                    line_path((right - dx, y), (right - dx, bottom)),
                ],
            )
        }
        "cylinder" => {
            let dy = style.number("size", 15.0).min(height / 2.0).max(0.0);
            let rx = width / 2.0;
            let outline = format!(
                "M {} {} A {} {} 0 0 1 {} {} L {} {} A {} {} 0 0 1 {} {} Z",
                n(x),
                n(y + dy),
                n(rx),
                n(dy),
                n(right),
                n(y + dy),
                n(right),
                n(bottom - dy),
                n(rx),
                n(dy),
                n(x),
                n(bottom - dy)
            );
            let lid = format!(
                "M {} {} A {} {} 0 0 0 {} {}",
                n(x),
                n(y + dy),
                n(rx),
                n(dy),
                n(right),
                n(y + dy)
            );
            Paths::with(outline, vec![lid])
        }
        "cloud" => Paths::new(format!(
            "M {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} Z",
            n(x + 0.25 * width),
            n(y + 0.25 * height),
            n(x + 0.05 * width),
            n(y + 0.25 * height),
            n(x),
            n(y + 0.5 * height),
            n(x + 0.16 * width),
            n(y + 0.55 * height),
            n(x),
            n(y + 0.66 * height),
            n(x + 0.18 * width),
            n(y + 0.9 * height),
            n(x + 0.31 * width),
            n(y + 0.8 * height),
            n(x + 0.4 * width),
            n(bottom),
            n(x + 0.7 * width),
            n(bottom),
            n(x + 0.8 * width),
            n(y + 0.8 * height),
            n(right),
            n(y + 0.8 * height),
            n(right),
            n(y + 0.6 * height),
            n(x + 0.875 * width),
            n(y + 0.5 * height),
            n(right),
            n(y + 0.3 * height),
            n(x + 0.8 * width),
            n(y + 0.1 * height),
            n(x + 0.625 * width),
            n(y + 0.2 * height),
            n(x + 0.5 * width),
            n(y + 0.05 * height),
            n(x + 0.3 * width),
            n(y + 0.05 * height),
            n(x + 0.25 * width),
            n(y + 0.25 * height)
        )),
        "document" => {
            let dy = height * style.number("size", 0.3).clamp(0.0, 1.0);
            Paths::new(format!(
                "M {} {} L {} {} L {} {} Q {} {} {} {} Q {} {} {} {} Z",
                n(x),
                n(y),
                n(right),
                n(y),
                n(right),
                n(bottom - dy / 2.0),
                n(x + width * 0.75),
                n(bottom - dy * 1.4),
                n(cx),
                n(bottom - dy / 2.0),
                n(x + width * 0.25),
                n(bottom + dy * 0.4),
                n(x),
                n(bottom - dy / 2.0)
            ))
        }
        "multiDocument" => {
            let offset = (width.min(height) * 0.1).min(8.0);
            let front = Rect {
                x,
                y: y + offset * 2.0,
                width: width - offset * 2.0,
                height: height - offset * 2.0,
            };
            // Only the top and right edges of the sheets behind are visible, so
            // they are open polylines: closing them would draw a diagonal
            // across the front sheet.
            Paths::with(
                rectangle_path(front),
                vec![
                    polyline_path(&[
                        (x + offset, front.y),
                        (x + offset, y + offset),
                        (right - offset, y + offset),
                        (right - offset, front.bottom()),
                    ]),
                    polyline_path(&[
                        (x + offset * 2.0, y + offset),
                        (x + offset * 2.0, y),
                        (right, y),
                        (right, bottom - offset * 2.0),
                    ]),
                ],
            )
        }
        "note" => {
            let size = style.number("size", 15.0).min(width).min(height).max(0.0);
            Paths::with(
                polygon_path(&[
                    (x, y),
                    (right - size, y),
                    (right, y + size),
                    (right, bottom),
                    (x, bottom),
                ]),
                vec![format!(
                    "M {} {} L {} {} L {} {}",
                    n(right - size),
                    n(y),
                    n(right - size),
                    n(y + size),
                    n(right),
                    n(y + size)
                )],
            )
        }
        "card" => {
            let size = style
                .number("size", 30.0)
                .min(width / 2.0)
                .min(height / 2.0)
                .max(0.0);
            Paths::new(polygon_path(&[
                (x + size, y),
                (right, y),
                (right, bottom),
                (x, bottom),
                (x, y + size),
            ]))
        }
        "internalStorage" => {
            let dx = style.number("dx", 20.0).min(width);
            let dy = style.number("dy", 20.0).min(height);
            Paths::with(
                rectangle_path(rect),
                vec![
                    line_path((x + dx, y), (x + dx, bottom)),
                    line_path((x, y + dy), (right, y + dy)),
                ],
            )
        }
        "cube" => {
            let size = style
                .number("size", 20.0)
                .min(width / 2.0)
                .min(height / 2.0)
                .max(0.0);
            Paths::with(
                polygon_path(&[
                    (x, y + size),
                    (x + size, y),
                    (right, y),
                    (right, bottom - size),
                    (right - size, bottom),
                    (x, bottom),
                ]),
                vec![
                    format!(
                        "M {} {} L {} {} L {} {}",
                        n(x),
                        n(y + size),
                        n(right - size),
                        n(y + size),
                        n(right),
                        n(y)
                    ),
                    line_path((right - size, y + size), (right - size, bottom)),
                ],
            )
        }
        "tape" => {
            let dy = height * style.number("size", 0.4).clamp(0.0, 0.5);
            Paths::new(format!(
                "M {} {} Q {} {} {} {} Q {} {} {} {} L {} {} Q {} {} {} {} Q {} {} {} {} Z",
                n(x),
                n(y + dy / 2.0),
                n(x + width * 0.25),
                n(y + dy * 1.4),
                n(cx),
                n(y + dy / 2.0),
                n(x + width * 0.75),
                n(y - dy * 0.4),
                n(right),
                n(y + dy / 2.0),
                n(right),
                n(bottom - dy / 2.0),
                n(x + width * 0.75),
                n(bottom - dy * 1.4),
                n(cx),
                n(bottom - dy / 2.0),
                n(x + width * 0.25),
                n(bottom + dy * 0.4),
                n(x),
                n(bottom - dy / 2.0)
            ))
        }
        "actor" => {
            let arm = width * 2.0 / 6.0;
            Paths::new(format!(
                "M {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} C {} {} {} {} {} {} Z",
                n(x),
                n(bottom),
                n(x),
                n(y + 0.6 * height),
                n(x),
                n(y + 0.4 * height),
                n(cx),
                n(y + 0.4 * height),
                n(cx - arm),
                n(y + 0.4 * height),
                n(cx - arm),
                n(y),
                n(cx),
                n(y),
                n(cx + arm),
                n(y),
                n(cx + arm),
                n(y + 0.4 * height),
                n(cx),
                n(y + 0.4 * height),
                n(right),
                n(y + 0.4 * height),
                n(right),
                n(y + 0.6 * height),
                n(right),
                n(bottom)
            ))
        }
        "or" => Paths::new(format!(
            "M {} {} Q {} {} {} {} Q {} {} {} {} Z",
            n(x),
            n(y),
            n(right),
            n(y),
            n(right),
            n(cy),
            n(right),
            n(bottom),
            n(x),
            n(bottom)
        )),
        "xor" => Paths::new(format!(
            "M {} {} Q {} {} {} {} Q {} {} {} {} Q {} {} {} {} Z",
            n(x),
            n(y),
            n(right),
            n(y),
            n(right),
            n(cy),
            n(right),
            n(bottom),
            n(x),
            n(bottom),
            n(cx),
            n(cy),
            n(x),
            n(y)
        )),
        "dataStorage" => {
            let size = width * style.number("size", 0.1).clamp(0.0, 0.5);
            Paths::new(format!(
                "M {} {} L {} {} Q {} {} {} {} L {} {} Q {} {} {} {} Z",
                n(x + size),
                n(y),
                n(right),
                n(y),
                n(right - size * 2.0),
                n(cy),
                n(right),
                n(bottom),
                n(x + size),
                n(bottom),
                n(x - size),
                n(cy),
                n(x + size),
                n(y)
            ))
        }
        "delay" => Paths::new(format!(
            "M {} {} L {} {} A {} {} 0 0 1 {} {} L {} {} Z",
            n(x),
            n(y),
            n(cx),
            n(y),
            n(width / 2.0),
            n(height / 2.0),
            n(cx),
            n(bottom),
            n(x),
            n(bottom)
        )),
        "display" => {
            let dx = width * style.number("size", 0.25).clamp(0.0, 0.5);
            Paths::new(format!(
                "M {} {} L {} {} L {} {} Q {} {} {} {} L {} {} Z",
                n(x),
                n(cy),
                n(x + dx),
                n(y),
                n(right - dx),
                n(y),
                n(right),
                n(cy),
                n(right - dx),
                n(bottom),
                n(x + dx),
                n(bottom)
            ))
        }
        "manualInput" => {
            let size = style.number("size", 30.0).min(height).max(0.0);
            Paths::new(polygon_path(&[
                (x, y + size),
                (right, y),
                (right, bottom),
                (x, bottom),
            ]))
        }
        "offPageConnector" => {
            let size = height * style.number("size", 0.25).clamp(0.0, 1.0);
            Paths::new(polygon_path(&[
                (x, y),
                (right, y),
                (right, bottom - size),
                (cx, bottom),
                (x, bottom - size),
            ]))
        }
        "loopLimit" => {
            let size = (width.min(height) * 0.2).min(20.0);
            Paths::new(polygon_path(&[
                (x + size, y),
                (right - size, y),
                (right, y + size),
                (right, bottom),
                (x, bottom),
                (x, y + size),
            ]))
        }
        "collate" => Paths::new(format!(
            "M {} {} L {} {} L {} {} Z M {} {} L {} {} L {} {} Z",
            n(x),
            n(y),
            n(right),
            n(y),
            n(cx),
            n(cy),
            n(x),
            n(bottom),
            n(right),
            n(bottom),
            n(cx),
            n(cy)
        )),
        "extract" => Paths::new(polygon_path(&[(cx, y), (right, bottom), (x, bottom)])),
        "merge" => Paths::new(polygon_path(&[(x, y), (right, y), (cx, bottom)])),
        "cross" => {
            let arm = width.min(height) * style.number("size", 0.2).clamp(0.0, 0.5);
            Paths::new(polygon_path(&[
                (cx - arm, y),
                (cx + arm, y),
                (cx + arm, cy - arm),
                (right, cy - arm),
                (right, cy + arm),
                (cx + arm, cy + arm),
                (cx + arm, bottom),
                (cx - arm, bottom),
                (cx - arm, cy + arm),
                (x, cy + arm),
                (x, cy - arm),
                (cx - arm, cy - arm),
            ]))
        }
        "singleArrow" => {
            let head = width * style.number("arrowsize", 0.25).clamp(0.0, 1.0);
            let shaft = height * style.number("arrowwidth", 0.5).clamp(0.0, 1.0) / 2.0;
            Paths::new(polygon_path(&[
                (x, cy - shaft),
                (right - head, cy - shaft),
                (right - head, y),
                (right, cy),
                (right - head, bottom),
                (right - head, cy + shaft),
                (x, cy + shaft),
            ]))
        }
        "doubleArrow" => {
            let head = width * style.number("arrowsize", 0.25).clamp(0.0, 0.5);
            let shaft = height * style.number("arrowwidth", 0.5).clamp(0.0, 1.0) / 2.0;
            Paths::new(polygon_path(&[
                (x, cy),
                (x + head, y),
                (x + head, cy - shaft),
                (right - head, cy - shaft),
                (right - head, y),
                (right, cy),
                (right - head, bottom),
                (right - head, cy + shaft),
                (x + head, cy + shaft),
                (x + head, bottom),
            ]))
        }
        "line" => Paths::with(String::new(), vec![line_path((x, cy), (right, cy))]),
        "wallU" => {
            let thickness = style
                .number("wallthickness", 10.0)
                .clamp(0.0, (width / 2.0).min(height));
            Paths::new(polygon_path(&[
                (x, bottom),
                (x, y),
                (right, y),
                (right, bottom),
                (right - thickness, bottom),
                (right - thickness, y + thickness),
                (x + thickness, y + thickness),
                (x + thickness, bottom),
            ]))
        }
        "doubleRect" => {
            // Two boxes offset by a fixed eight units, the back one first.
            let offset = 8.0_f64.min(width / 2.0).min(height / 2.0);
            let front = Rect {
                x,
                y,
                width: width - offset,
                height: height - offset,
            };
            Paths::with(
                rounded_rect_path(front, 1.0),
                vec![rounded_rect_path(
                    Rect {
                        x: x + offset,
                        y: y + offset,
                        ..front
                    },
                    1.0,
                )],
            )
        }
        "bpmnTask" => {
            let radius = style
                .number("size", 10.0)
                .clamp(0.0, width.min(height) / 2.0);
            match style.text("rectstyle", "rounded").as_str() {
                "square" => Paths::new(rectangle_path(rect)),
                _ => Paths::new(rounded_rect_path(rect, radius)),
            }
        }
        // A UML component is a box with two sockets on its left edge.
        "component" => {
            let jetty_width = style.number("jettywidth", 32.0).clamp(0.0, width);
            let jetty_height = style.number("jettyheight", 12.0).clamp(0.0, height);
            let (x0, x1) = (x + jetty_width / 2.0, x + jetty_width);
            let y0 = y + 0.3 * height - jetty_height / 2.0;
            let y1 = y + 0.7 * height - jetty_height / 2.0;
            Paths::with(
                polygon_path(&[
                    (x0, y),
                    (right, y),
                    (right, bottom),
                    (x0, bottom),
                    (x0, y1 + jetty_height),
                    (x, y1 + jetty_height),
                    (x, y1),
                    (x0, y1),
                    (x0, y0 + jetty_height),
                    (x, y0 + jetty_height),
                    (x, y0),
                    (x0, y0),
                ]),
                vec![
                    polyline_path(&[
                        (x0, y0),
                        (x1, y0),
                        (x1, y0 + jetty_height),
                        (x0, y0 + jetty_height),
                    ]),
                    polyline_path(&[
                        (x0, y1),
                        (x1, y1),
                        (x1, y1 + jetty_height),
                        (x0, y1 + jetty_height),
                    ]),
                ],
            )
        }
        // A folder, which is also how UML draws a package: a tab over a box.
        "folder" => {
            let tab = folder_tab(rect, style);
            Paths::new(polygon_path(&[
                (tab.x, y),
                (tab.right(), y),
                (tab.right(), tab.bottom()),
                (right, tab.bottom()),
                (right, bottom),
                (x, bottom),
                (x, tab.bottom()),
                (tab.x, tab.bottom()),
            ]))
        }
        // A segment of a ring, between two angles measured from the top.
        "partConcEllipse" => {
            let start = style.number("startangle", 0.25).clamp(0.0, 1.0) * 2.0 * PI;
            let end = style.number("endangle", 0.75).clamp(0.0, 1.0) * 2.0 * PI;
            let inner = 1.0 - style.number("arcwidth", 0.5).clamp(0.0, 1.0);
            let (rx, ry) = (width / 2.0, height / 2.0);
            let (rx2, ry2) = (rx * inner, ry * inner);
            let mut span = end - start;
            if span < 0.0 {
                span += 2.0 * PI;
            }
            let large = u8::from(span >= PI);
            let outer_at = |angle: f64| (x + rx + angle.sin() * rx, y + ry - angle.cos() * ry);
            let inner_at = |angle: f64| (x + rx + angle.sin() * rx2, y + ry - angle.cos() * ry2);
            Paths::new(format!(
                "M {} {} A {} {} 0 {large} 1 {} {} L {} {} A {} {} 0 {large} 0 {} {} Z",
                n(outer_at(start).0),
                n(outer_at(start).1),
                n(rx),
                n(ry),
                n(outer_at(end).0),
                n(outer_at(end).1),
                n(inner_at(end).0),
                n(inner_at(end).1),
                n(rx2),
                n(ry2),
                n(inner_at(start).0),
                n(inner_at(start).1)
            ))
        }
        // A rule drawn across the middle of its own box.
        "mockupLine" => Paths::fill_only(String::new(), vec![line_path((x, cy), (right, cy))]),
        // A cross, for the close control on a mock-up window.
        "crossLines" => Paths::fill_only(
            String::new(),
            vec![
                line_path((x, y), (right, bottom)),
                line_path((right, y), (x, bottom)),
            ],
        ),
        // A mock-up button: rounded, or drawn as a chevron pointing right.
        "mockupButton" => {
            if style.text("buttonstyle", "round") == "chevron" {
                Paths::new(format!(
                    "M {} {} A {} {} 0 0 1 {} {} L {} {} A {} {} 0 0 1 {} {} L {} {} \
                     A {} {} 0 0 1 {} {} L {} {} A {} {} 0 0 1 {} {} L {} {} \
                     A {} {} 0 0 1 {} {} Z",
                    n(x),
                    n(y + height * 0.1),
                    n(width * 0.0372),
                    n(height * 0.1111),
                    n(x + width * 0.0334),
                    n(y),
                    n(x + width * 0.768),
                    n(y),
                    n(width * 0.0722),
                    n(height * 0.216),
                    n(x + width * 0.8014),
                    n(y + height * 0.0399),
                    n(x + width * 0.99),
                    n(y + height * 0.4585),
                    n(width * 0.09),
                    n(height * 0.1),
                    n(x + width * 0.99),
                    n(y + height * 0.5415),
                    n(x + width * 0.8014),
                    n(y + height * 0.9568),
                    n(width * 0.0722),
                    n(height * 0.216),
                    n(x + width * 0.768),
                    n(bottom),
                    n(x + width * 0.0334),
                    n(bottom),
                    n(width * 0.0372),
                    n(height * 0.1111),
                    n(x),
                    n(bottom - height * 0.1),
                ))
            } else {
                Paths::new(rounded_rect_path(rect, 10.0))
            }
        }
        // A square checkbox with a tick drawn across it.
        "mockupCheckbox" => Paths::with(
            rectangle_path(rect),
            vec![polyline_path(&[
                (x + width * 0.8, y + height * 0.2),
                (x + width * 0.4, y + height * 0.8),
                (x + width * 0.25, y + height * 0.6),
            ])],
        ),
        // A drop: a circle at the bottom, tapering to a point at the top.
        "drop" => {
            let radius = width.min(height) * 0.5;
            let fall = height - radius;
            let reach = (fall * fall - radius * radius).max(0.0).sqrt();
            let angle = reach.atan2(radius);
            let (dx, dy) = (radius * angle.sin(), radius * angle.cos());
            let waist = bottom - radius - dy;
            Paths::new(format!(
                "M {} {} L {} {} A {r} {r} 0 0 1 {} {} A {r} {r} 0 0 1 {} {} \
                 A {r} {r} 0 0 1 {} {} A {r} {r} 0 0 1 {} {} Z",
                n(cx),
                n(y),
                n(cx + dx),
                n(waist),
                n(cx + radius),
                n(bottom - radius),
                n(cx),
                n(bottom),
                n(cx - radius),
                n(bottom - radius),
                n(cx - dx),
                n(waist),
                r = n(radius),
            ))
        }
        // A triangle whose apex slides along the top edge.
        "obtuseTriangle" => {
            let apex = x + width * style.number("dx", 0.5).clamp(0.0, 1.0);
            Paths::new(polygon_path(&[(apex, bottom), (x, y), (right, bottom)]))
        }
        // A polygon the style spells out, as fractions of its own box. A side
        // may be curved, in which case its control point is given separately.
        "polyShape" => {
            let corners = numbers(&style.text("polycoords", ""))
                .chunks_exact(2)
                .map(|pair| (x + pair[0] * width, y + pair[1] * height))
                .collect::<Vec<_>>();
            if corners.len() < 2 {
                return Some(Paths::new(String::new()));
            }
            let curves = curve_controls(&style.text("polycurves", ""));
            let control = |side: usize| {
                curves
                    .get(side)
                    .copied()
                    .flatten()
                    .map(|(at, down)| (x + at * width, y + down * height))
            };
            let mut path = format!("M {} {}", n(corners[0].0), n(corners[0].1));
            let step = |path: &mut String, side: usize, to: (f64, f64)| match control(side) {
                Some((cx, cy)) => {
                    path.push_str(&format!(" Q {} {} {} {}", n(cx), n(cy), n(to.0), n(to.1)));
                }
                None => path.push_str(&format!(" L {} {}", n(to.0), n(to.1))),
            };
            for (side, corner) in corners.iter().enumerate().skip(1) {
                step(&mut path, side - 1, *corner);
            }
            if style.flag("polyline") {
                return Some(Paths::fill_only(String::new(), vec![path]));
            }
            // Closing the path draws the last side, so it only needs writing
            // out when it is curved.
            if control(corners.len() - 1).is_some() {
                step(&mut path, corners.len() - 1, corners[0]);
            }
            path.push_str(" Z");
            Paths::new(path)
        }
        // A lead time ladder: a square wave whose corners the style pins one by
        // one, each step naming where it turns and whether it turns up or down.
        "leanTimeline" => {
            let level = |step: usize| {
                if style.number(&format!("dy{step}"), 0.0) <= 0.5 {
                    y
                } else {
                    bottom
                }
            };
            let mut at = level(1);
            let mut points = vec![(x, at)];
            for step in 2..=6 {
                let turn = if step == 6 {
                    right
                } else {
                    x + style.number(&format!("dx{step}"), 0.0)
                };
                let next = level(step);
                if next != at {
                    points.push((turn, at));
                    at = next;
                }
                points.push((turn, at));
            }
            Paths::fill_only(String::new(), vec![polyline_path(&points)])
        }
        // A first-in first-out lane: a band with a box, a circle and a triangle
        // queued along it in the order they will be taken.
        "fifoLane" => {
            let band = (style.number("fontsize", 8.0) * 1.5).min(height);
            let (top, tall) = (y + band + 4.0, (height - band - 8.0).max(0.0));
            Paths::with(
                format!(
                    "{} {} {}",
                    rectangle_path(Rect {
                        x: x + width * 0.02,
                        y: top,
                        width: width * 0.26,
                        height: tall,
                    }),
                    ellipse_path(Rect {
                        x: x + width * 0.35,
                        y: top,
                        width: width * 0.26,
                        height: tall,
                    }),
                    polygon_path(&[
                        (x + width * 0.69, top),
                        (x + width * 0.98, top),
                        (x + width * 0.835, bottom - 4.0),
                    ])
                ),
                vec![
                    line_path((x, y + band), (right, y + band)),
                    line_path((x, bottom), (right, bottom)),
                ],
            )
        }
        // A phone: a rounded case with the screen, the earpiece and the home
        // button drawn on it as outlines.
        "iosPhone" => Paths::with(
            rounded_rect_path(rect, 25.0),
            vec![
                rectangle_path(Rect {
                    x: x + width * 0.0625,
                    y: y + height * 0.15,
                    width: width * 0.875,
                    height: height * 0.7,
                }),
                ellipse_path(Rect {
                    x: x + width * 0.4875,
                    y: y + height * 0.04125,
                    width: width * 0.025,
                    height: height * 0.0125,
                }),
                oval_rect_path(
                    Rect {
                        x: x + width * 0.375,
                        y: y + height * 0.075,
                        width: width * 0.25,
                        height: height * 0.01875,
                    },
                    width * 0.02,
                    height * 0.01,
                ),
                ellipse_path(Rect {
                    x: x + width * 0.4,
                    y: y + height * 0.875,
                    width: width * 0.2,
                    height: height * 0.1,
                }),
                oval_rect_path(
                    Rect {
                        x: x + width * 0.4575,
                        y: y + height * 0.905,
                        width: width * 0.085,
                        height: height * 0.04375,
                    },
                    height * 0.00625,
                    height * 0.00625,
                ),
            ],
        ),
        // A home screen: a grid of app tiles, each a tenth of its own width
        // clear of the next.
        "iosIconGrid" => {
            let (columns, rows) = grid_size(style, "4,7");
            let cell = |count: f64, span: f64| span / (count + (count - 1.0) * 0.1);
            let (tile_w, tile_h) = (cell(columns, width), cell(rows, height));
            let mut path = String::new();
            for column in 0..columns as u32 {
                for row in 0..rows as u32 {
                    if !path.is_empty() {
                        path.push(' ');
                    }
                    path.push_str(&rectangle_path(Rect {
                        x: x + tile_w * 1.1 * f64::from(column),
                        y: y + tile_h * 1.1 * f64::from(row),
                        width: tile_w,
                        height: tile_h,
                    }));
                }
            }
            Paths::fill_only(path, Vec::new())
        }
        // A room is four walls: the inside is wound the other way round, which
        // makes it a hole rather than a second filled box.
        "room" => {
            let wall = style.number("wallthickness", 10.0).max(0.0);
            Paths::new(format!(
                "{} {}",
                polygon_path(&[(x, bottom), (x, y), (right, y), (right, bottom)]),
                polygon_path(&[
                    (x + wall, y + wall),
                    (x + wall, bottom - wall),
                    (right - wall, bottom - wall),
                    (right - wall, y + wall),
                ])
            ))
        }
        // A flight of stairs with a landing: treads every twenty five pixels,
        // and an arrow across the landing saying which way they go up.
        "stairsRest" => {
            // draw.io lays these out over at least fifty pixels of width.
            let span = width.max(50.0).max(height);
            let (right, half) = (x + span, y + height * 0.5);
            let mut treads = Vec::new();
            let mut at = 25.0;
            while at < span - height * 0.5 {
                treads.push(line_path((x + at, y), (x + at, bottom)));
                at += 25.0;
            }
            treads.push(line_path((x, half), (right, half)));
            treads.push(polyline_path(&[
                (right, y),
                (right - height * 0.5, half),
                (right, bottom),
            ]));
            treads.push(line_path(
                (right - height * 0.5, y),
                (right - height * 0.5, bottom),
            ));
            Paths::with(
                rectangle_path(Rect {
                    x,
                    y,
                    width: span,
                    height,
                }),
                treads,
            )
        }
        // A sliding door: two panels that pass each other between two jambs.
        "doorBypass" => {
            let slide = width * style.number("dx", 0.5).clamp(0.0, 1.0);
            let jamb = |at: f64| {
                rectangle_path(Rect {
                    x: at,
                    y: cy - 5.0,
                    width: 5.0,
                    height: 10.0,
                })
            };
            let panel = |at: f64, top: f64| {
                rectangle_path(Rect {
                    x: at,
                    y: top,
                    width: width * 0.5,
                    height: 5.0,
                })
            };
            Paths::new(format!(
                "{} {} {} {}",
                jamb(x),
                jamb(right - 5.0),
                panel(x, cy),
                panel(x + slide, cy - 5.0)
            ))
        }
        // An end that stops one flow rather than the whole activity.
        "sysmlFlowFinal" => Paths::with(
            ellipse_path(rect),
            vec![
                line_path(
                    (x + width * 0.145, y + height * 0.145),
                    (x + width * 0.855, y + height * 0.855),
                ),
                line_path(
                    (x + width * 0.855, y + height * 0.145),
                    (x + width * 0.145, y + height * 0.855),
                ),
            ],
        ),
        // A rounded body with a square port let into one or both ends. Every
        // part is filled and stroked alike, so they are drawn as one path.
        "sysmlIsControl" | "sysmlObjectFlowLeft" | "sysmlObjectFlowRight" => {
            let port = |at: f64| {
                rectangle_path(Rect {
                    x: at,
                    y: cy - 10.0,
                    width: 10.0,
                    height: 20.0,
                })
            };
            // draw.io lets the body of a one-ended flow run past its own box.
            let body = |left: f64, width: f64| {
                rounded_rect_path(
                    Rect {
                        x: left,
                        y,
                        width,
                        height,
                    },
                    10.0,
                )
            };
            Paths::new(match name {
                "sysmlIsControl" => format!(
                    "{} {} {}",
                    port(x),
                    body(x + 10.0, width - 20.0),
                    port(right - 10.0)
                ),
                "sysmlObjectFlowLeft" => format!("{} {}", port(x), body(x + 10.0, width - 10.0)),
                _ => format!("{} {}", body(x, width - 10.0), port(right - 10.0)),
            })
        }
        // A flow carrying three items, each with its own port.
        "sysmlItemFlowLeft" | "sysmlItemFlowRight" => {
            let left = if name == "sysmlItemFlowLeft" {
                x
            } else {
                right - 20.0
            };
            let body = if name == "sysmlItemFlowLeft" {
                Rect {
                    x: x + 10.0,
                    y,
                    width: width - 10.0,
                    height,
                }
            } else {
                Rect {
                    x,
                    y,
                    width: width - 10.0,
                    height,
                }
            };
            let mut path = rectangle_path(body);
            for share in [0.25, 0.5, 0.75] {
                path.push(' ');
                path.push_str(&rectangle_path(Rect {
                    x: left,
                    y: y + height * share - 10.0,
                    width: 20.0,
                    height: 20.0,
                }));
            }
            Paths::new(path)
        }
        // A parametric diagram frame, with a port let into each side.
        "sysmlParametricDiagram" => Paths::with(
            rounded_rect_path(rect, 10.0),
            if height > 60.0 {
                [0.25, 0.75]
                    .into_iter()
                    .map(|share| {
                        rectangle_path(Rect {
                            x,
                            y: y + height * share - 10.0,
                            width: 20.0,
                            height: 20.0,
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            },
        ),
        // A port is drawn inset from the sides of the box it is given.
        "sysmlPort" => Paths::new(rectangle_path(Rect {
            x: x + width * 0.05,
            y,
            width: width * 0.9,
            height,
        })),
        // A slice of a circle, from one turn fraction round to another.
        "pie" => {
            let start = style.number("startangle", 0.25).clamp(0.0, 1.0);
            let end = style.number("endangle", 0.75).clamp(0.0, 1.0);
            let (rx, ry) = (width / 2.0, height / 2.0);
            let at = |turns: f64| {
                let angle = turns * 2.0 * PI;
                (cx + angle.sin() * rx, cy - angle.cos() * ry)
            };
            let mut span = end - start;
            if span < 0.0 {
                span += 1.0;
            }
            // A half turn is drawn as two quarters, because one arc of exactly
            // half an ellipse leaves its direction undefined.
            let arcs = if span >= 0.5 {
                let middle = at(start + span * 0.5);
                format!(
                    "A {} {} 0 0 1 {} {} A {} {} 0 0 1 {} {}",
                    n(rx),
                    n(ry),
                    n(middle.0),
                    n(middle.1),
                    n(rx),
                    n(ry),
                    n(at(end).0),
                    n(at(end).1)
                )
            } else {
                format!(
                    "A {} {} 0 0 1 {} {}",
                    n(rx),
                    n(ry),
                    n(at(end).0),
                    n(at(end).1)
                )
            };
            Paths::new(format!(
                "M {} {} L {} {} {arcs} Z",
                n(cx),
                n(cy),
                n(at(start).0),
                n(at(start).1)
            ))
        }
        // The top face of an isometric box, drawn on a thirty degree grid.
        "isoRectangle" => {
            let side = width.min(height / (PI / 6.0).tan());
            let (ox, oy) = (
                x + (width - side) / 2.0,
                y + (height - side) / 2.0 + side / 4.0,
            );
            // draw.io measures the near and far corners from the same offset.
            let dip = (0.5 - (PI / 6.0).tan()) / 2.0;
            Paths::new(polygon_path(&[
                (ox, oy + 0.25 * side),
                (ox + 0.5 * side, oy + dip * side),
                (ox + side, oy + 0.25 * side),
                (ox + 0.5 * side, oy + (0.5 - dip) * side),
            ]))
        }
        // An isometric cube, its three faces divided by a Y.
        "isoCube2" => {
            let angle = style.number("isoangle", 15.0).clamp(0.01, 94.0) * PI / 200.0;
            let iso = (width * angle.tan()).min(height * 0.5);
            Paths::with(
                polygon_path(&[
                    (cx, y),
                    (right, y + iso),
                    (right, bottom - iso),
                    (cx, bottom),
                    (x, bottom - iso),
                    (x, y + iso),
                ]),
                vec![
                    polyline_path(&[(x, y + iso), (cx, y + 2.0 * iso), (right, y + iso)]),
                    line_path((cx, y + 2.0 * iso), (cx, bottom)),
                ],
            )
        }
        // A brace drawn open, with no fill of its own.
        "curlyBracket" => {
            let reach = width * style.number("size", 0.5).clamp(0.0, 1.0);
            Paths::fill_only(
                String::new(),
                vec![polyline_path(&[
                    (right, y),
                    (x + reach, y),
                    (x + reach, cy),
                    (x, cy),
                    (x + reach, cy),
                    (x + reach, bottom),
                    (right, bottom),
                ])],
            )
        }
        "basicArc" => {
            let start = style.number("startangle", 0.25).clamp(0.0, 1.0) * 2.0 * PI;
            let end = style.number("endangle", 0.75).clamp(0.0, 1.0) * 2.0 * PI;
            let (rx, ry) = (width / 2.0, height / 2.0);
            let mut span = end - start;
            if span < 0.0 {
                span += 2.0 * PI;
            }
            let large = u8::from(span >= PI);
            let at = |angle: f64| (x + rx + angle.sin() * rx, y + ry - angle.cos() * ry);
            Paths::with(
                String::new(),
                vec![format!(
                    "M {} {} A {} {} 0 {large} 1 {} {}",
                    n(at(start).0),
                    n(at(start).1),
                    n(rx),
                    n(ry),
                    n(at(end).0),
                    n(at(end).1)
                )],
            )
        }
        "ribbonSimple" => {
            let notch1 = style.number("notch1", 20.0).clamp(0.0, width);
            let notch2 = style.number("notch2", 20.0).clamp(0.0, width);
            Paths::new(polygon_path(&[
                (x, bottom),
                (x + notch1, cy),
                (x, y),
                (right - notch2, y),
                (right, cy),
                (right - notch2, bottom),
            ]))
        }
        "infographicCylinder" => {
            let dy = 10.0_f64.min(height / 2.0);
            let rx = width / 2.0;
            Paths::with(
                format!(
                    "M {} {} A {} {} 0 0 1 {} {} L {} {} A {} {} 0 0 1 {} {} Z",
                    n(x),
                    n(y + dy),
                    n(rx),
                    n(dy),
                    n(right),
                    n(y + dy),
                    n(right),
                    n(bottom - dy),
                    n(rx),
                    n(dy),
                    n(x),
                    n(bottom - dy)
                ),
                vec![format!(
                    "M {} {} A {} {} 0 0 0 {} {}",
                    n(x),
                    n(y + dy),
                    n(rx),
                    n(dy),
                    n(right),
                    n(y + dy)
                )],
            )
        }
        "stairs" => {
            let step = 25.0;
            let mut treads = Vec::new();
            let mut at = step;
            while at < width && treads.len() < 400 {
                treads.push(line_path((x + at, y), (x + at, bottom)));
                at += step;
            }
            treads.push(line_path((x, cy), (right, cy)));
            treads.push(polyline_path(&[
                (right - step, y),
                (right, cy),
                (right - step, bottom),
            ]));
            Paths::with(rectangle_path(rect), treads)
        }
        "bootstrapRoundRect" => Paths::new(rounded_rect_path(
            rect,
            style
                .number("rsize", 10.0)
                .clamp(0.0, width.min(height) / 2.0),
        )),
        "searchBox" => {
            let radius = 5.0_f64.min(width / 4.0).min(height / 4.0);
            let centre = (right - 15.0 + radius, cy - 8.0 + radius);
            Paths::with(
                rectangle_path(rect),
                vec![
                    ellipse_path(Rect {
                        x: centre.0 - radius,
                        y: centre.1 - radius,
                        width: radius * 2.0,
                        height: radius * 2.0,
                    }),
                    line_path((right - 19.0, cy + 9.0), (right - 13.0, cy + 1.0)),
                ],
            )
        }
        "comboBox" => {
            let button = 30.0_f64.min(width);
            let arrow = (button * 0.3).min(height * 0.3);
            let centre = (right - button / 2.0, cy);
            Paths::with(
                rounded_rect_path(rect, 5.0_f64.min(width / 4.0).min(height / 4.0)),
                vec![
                    line_path((right - button, y), (right - button, bottom)),
                    polygon_path(&[
                        (centre.0 - arrow, centre.1 - arrow / 2.0),
                        (centre.0 + arrow, centre.1 - arrow / 2.0),
                        (centre.0, centre.1 + arrow / 2.0),
                    ]),
                ],
            )
        }
        "simpleIcon" => Paths::with(
            rectangle_path(rect),
            vec![
                line_path((x, y), (right, bottom)),
                line_path((x, bottom), (right, y)),
            ],
        ),
        "iconGrid" => {
            let (columns, rows) = grid_size(style, "3,3");
            let box_width = width / (columns + (columns - 1.0) * 0.5);
            let box_height = height / (rows + (rows - 1.0) * 0.5);
            let mut cells = String::new();
            for column in 0..columns as usize {
                for row in 0..rows as usize {
                    if !cells.is_empty() {
                        cells.push(' ');
                    }
                    cells.push_str(&rectangle_path(Rect {
                        x: x + box_width * 1.5 * column as f64,
                        y: y + box_height * 1.5 * row as f64,
                        width: box_width,
                        height: box_height,
                    }));
                }
            }
            Paths::new(cells)
        }
        // The saw-tooth roof lean mapping draws for a source outside the plant.
        "outsideSources" => Paths::new(polygon_path(&[
            (x, bottom),
            (x, y + height * 0.3),
            (x + width * 0.33, y + height * 0.02),
            (x + width * 0.33, y + height * 0.3),
            (x + width * 0.67, y + height * 0.02),
            (x + width * 0.67, y + height * 0.3),
            (right, y + height * 0.02),
            (right, bottom),
        ])),
        // A button rounded along one edge only.
        "topButton" | "bottomButton" | "leftButton" | "rightButton" => {
            let radius = style
                .number("rsize", 10.0)
                .clamp(0.0, width.min(height) / 2.0);
            let (near, far) = match name {
                "topButton" => ((x, y), (right, y)),
                "bottomButton" => ((right, bottom), (x, bottom)),
                "leftButton" => ((x, bottom), (x, y)),
                _ => ((right, y), (right, bottom)),
            };
            // The square corners are the two the rounded pair does not touch.
            let square = match name {
                "topButton" => [(right, bottom), (x, bottom)],
                "bottomButton" => [(x, y), (right, y)],
                "leftButton" => [(right, y), (right, bottom)],
                _ => [(x, bottom), (x, y)],
            };
            // Step in from each rounded corner towards the sides that meet it.
            let toward = |from: (f64, f64), to: (f64, f64)| {
                let (dx, dy) = (to.0 - from.0, to.1 - from.1);
                let length = dx.hypot(dy);
                if length <= 0.0 {
                    from
                } else {
                    (from.0 + dx / length * radius, from.1 + dy / length * radius)
                }
            };
            let (into_near, into_far) = (toward(near, square[1]), toward(far, square[0]));
            let (out_near, out_far) = (toward(near, far), toward(far, near));
            Paths::new(format!(
                "M {} {} A {r} {r} 0 0 1 {} {} L {} {} A {r} {r} 0 0 1 {} {} L {} {} L {} {} Z",
                n(into_near.0),
                n(into_near.1),
                n(out_near.0),
                n(out_near.1),
                n(out_far.0),
                n(out_far.1),
                n(into_far.0),
                n(into_far.1),
                n(square[0].0),
                n(square[0].1),
                n(square[1].0),
                n(square[1].1),
                r = n(radius),
            ))
        }
        // A rectangle inset by the margins the style names, rounded or not.
        "marginRect" | "marginRoundRect" => {
            let margin = style.number("rectmargin", 0.0);
            let inset = |side: &str| margin + style.number(side, 0.0);
            let inner = Rect {
                x: x + inset("rectmarginleft"),
                y: y + inset("rectmargintop"),
                width: width - inset("rectmarginleft") - inset("rectmarginright"),
                height: height - inset("rectmargintop") - inset("rectmarginbottom"),
            };
            if inner.width <= 0.0 || inner.height <= 0.0 {
                Paths::new(String::new())
            } else if name == "marginRect" {
                Paths::new(rectangle_path(inner))
            } else {
                Paths::new(rounded_rect_path(inner, 10.0))
            }
        }
        // Three sides of a box, open along the bottom.
        "uRect" => Paths::new(polyline_path(&[
            (x, bottom),
            (x, y),
            (right, y),
            (right, bottom),
        ])),
        // A ticked checkbox, rounded by three pixels whatever its size.
        "bootstrapCheckbox" => Paths::with(
            rounded_rect_path(rect, 3.0),
            vec![polyline_path(&[
                (x + width * 0.8, y + height * 0.2),
                (x + width * 0.4, y + height * 0.8),
                (x + width * 0.25, y + height * 0.6),
            ])],
        ),
        // A bar with a pointer hanging off its lower edge.
        "barCallout" => {
            let dx = style.number("dx", 0.5).clamp(0.0, width);
            let dy = style.number("dy", 0.5).clamp(0.0, height);
            let near = (dx - dy * 0.35).max(0.0);
            let far = (dx + dy * 0.35).min(width);
            Paths::new(polygon_path(&[
                (x, y),
                (right, y),
                (right, bottom - dy),
                (x + far, bottom - dy),
                (x + dx, bottom),
                (x + near, bottom - dy),
                (x, bottom - dy),
            ]))
        }
        // A speech box: the body sits above its tail, which hangs from `dx`.
        "rectCallout" => {
            let dx = style.number("dx", 0.5).clamp(0.0, width);
            let dy = style.number("dy", 0.5).clamp(0.0, height);
            Paths::new(polygon_path(&[
                (x + dx - dy * 0.5, bottom - dy),
                (x, bottom - dy),
                (x, y),
                (right, y),
                (right, bottom - dy),
                (x + dx + dy * 0.5, bottom - dy),
                (x + dx - dy, bottom),
            ]))
        }
        // The same box with rounded corners and a curved tail.
        "roundRectCallout" => {
            let dy = style.number("dy", 0.5).clamp(0.0, height);
            let radius = style
                .number("size", 10.0)
                .clamp(0.0, height)
                .min((height - dy) / 2.0)
                .min(width / 2.0)
                .max(0.0);
            let dx = style
                .number("dx", 0.5)
                .clamp(0.0, width)
                .max(radius + dy * 0.5)
                .min(width - radius - dy * 0.5);
            let waist = bottom - dy;
            Paths::new(format!(
                "M {} {} L {} {} A {r} {r} 0 0 1 {} {} L {} {} A {r} {r} 0 0 1 {} {} \
                 L {} {} A {r} {r} 0 0 1 {} {} L {} {} A {r} {r} 0 0 1 {} {} \
                 L {} {} A {} {} 0 0 1 {} {} A {} {} 0 0 0 {} {} Z",
                n(x + dx - dy * 0.5),
                n(waist),
                n(x + radius),
                n(waist),
                n(x),
                n(waist - radius),
                n(x),
                n(y + radius),
                n(x + radius),
                n(y),
                n(right - radius),
                n(y),
                n(right),
                n(y + radius),
                n(right),
                n(waist - radius),
                n(right - radius),
                n(waist),
                n(x + dx + dy * 0.5),
                n(waist),
                n(1.9 * dy),
                n(1.4 * dy),
                n(x + dx - dy),
                n(bottom),
                n(0.9 * dy),
                n(1.4 * dy),
                n(x + dx - dy * 0.5),
                n(waist),
                r = n(radius),
            ))
        }
        // A process box with a band across the top for its name.
        "manufacturingProcess" => {
            let band = (style.number("fontsize", 8.0) * 1.5).min(height);
            Paths::with(
                rectangle_path(rect),
                vec![line_path((x, y + band), (right, y + band))],
            )
        }
        // A data box: an open-topped box ruled into rows.
        "dataBox" => Paths::with(
            polyline_path(&[(x, bottom), (x, y), (right, y), (right, bottom)]),
            (1..5)
                .map(|row| {
                    let at = y + height * f64::from(row) * 0.2;
                    line_path((x, at), (right, at))
                })
                .collect(),
        ),
        // A striped arrow: material pushed downstream.
        "pushArrow" => Paths::with(
            polygon_path(&[
                (x, y + height * 0.17),
                (x + width * 0.75, y + height * 0.17),
                (x + width * 0.75, y),
                (right, cy),
                (x + width * 0.75, bottom),
                (x + width * 0.75, y + height * 0.83),
                (x, y + height * 0.83),
            ]),
            (0..3)
                .map(|stripe| {
                    let left = x + width * (0.12 * 2.0 * f64::from(stripe));
                    rectangle_path(Rect {
                        x: left,
                        y: y + height * 0.17,
                        width: width * 0.12,
                        height: height * 0.66,
                    })
                })
                .collect(),
        ),
        // A hooked arrow: material pulled by the next process.
        "physicalPull" => Paths::with(
            polygon_path(&[
                (x + width * 0.9071, y + height * 0.6191),
                (x + width * 0.9794, y + height * 0.4951),
                (right, y + height * 0.6438),
            ]),
            vec![format!(
                "M {} {} A {} {} 0 1 0 {} {}",
                n(x + width * 0.732),
                n(y + height * 0.0736),
                n(width * 0.4827),
                n(height * 0.4959),
                n(x + width * 0.9553),
                n(y + height * 0.6191)
            )],
        ),
        "inventoryBox" => Paths::with(
            polygon_path(&[(x, bottom), (cx, y), (right, bottom)]),
            vec![
                line_path(
                    (x + width * 0.4, y + height * 0.45),
                    (x + width * 0.6, y + height * 0.45),
                ),
                line_path((cx, y + height * 0.45), (cx, y + height * 0.85)),
                line_path(
                    (x + width * 0.4, y + height * 0.85),
                    (x + width * 0.6, y + height * 0.85),
                ),
            ],
        ),
        "horLines" => Paths::fill_only(
            rectangle_path(rect),
            vec![
                line_path((x, y), (right, y)),
                line_path((x, bottom), (right, bottom)),
            ],
        ),
        "rackContainer" => {
            // The numbered rail down the side is drawn as the band it occupies.
            let rail = if style
                .get("numberdisplay")
                .is_some_and(|value| value == "off")
            {
                0.0
            } else {
                24.0_f64.min(width / 2.0)
            };
            Paths::with(
                rectangle_path(Rect {
                    x: x + rail,
                    width: width - rail,
                    ..rect
                }),
                if rail > 0.0 {
                    vec![line_path((x + rail, y), (x + rail, bottom))]
                } else {
                    Vec::new()
                },
            )
        }
        // A dashed run across the ground, with a flat head at one or both ends.
        "isometricDashedEdge" | "isometricDashedEdgeOne" | "isometricDashedEdgeTwo" => {
            let mut heads = vec![line_path((x, y), (right, bottom))];
            if name != "isometricDashedEdge" {
                heads.push(polygon_path(&[
                    (right - 21.0, y + 5.5),
                    (right, y),
                    (right - 9.7, y + 12.2),
                ]));
            }
            if name == "isometricDashedEdgeTwo" {
                heads.push(polygon_path(&[
                    (x + 21.0, bottom - 5.5),
                    (x, bottom),
                    (x + 9.7, bottom - 12.2),
                ]));
            }
            Paths::fill_only(String::new(), heads)
        }
        "isometricArrowNE"
        | "isometricArrowSE"
        | "isometricArrowSW"
        | "isometricArrowNW"
        | "isometricArrowlessNE" => Paths::new(isometric_arrow_path(name, rect)),
        // A brace across the top half of the box, drawn as a line.
        "curlyBrace" => {
            let radius = (width * 0.125).min(height / 2.0);
            Paths::fill_only(
                String::new(),
                vec![format!(
                    "M {} {} A {} {} 0 0 1 {} {} L {} {} A {} {} 0 0 0 {} {} A {} {} 0 0 0 {} {} L {} {} A {} {} 0 0 1 {} {}",
                    n(x),
                    n(cy + radius),
                    n(radius),
                    n(radius),
                    n(x + radius),
                    n(cy),
                    n(cx - radius),
                    n(cy),
                    n(radius),
                    n(radius),
                    n(cx),
                    n(cy - radius),
                    n(radius),
                    n(radius),
                    n(cx + radius),
                    n(cy),
                    n(right - radius),
                    n(cy),
                    n(radius),
                    n(radius),
                    n(right),
                    n(cy + radius)
                )],
            )
        }
        "uTurnArrow" => {
            let head = style.number("arrowhead", 40.0).clamp(0.0, height);
            let dy = style.number("dy", 0.5).clamp(0.0, height);
            let dx = (height - head / 2.0 + dy) / 2.0;
            let dx2 = style.number("dx2", 20.0).max(0.0);
            let far = width.max(dx);
            Paths::new(format!(
                "M {} {} L {} {} L {} {} L {} {} A {} {} 0 0 0 {} {} L {} {} L {} {} L {} {} A {} {} 0 0 1 {} {} Z",
                n(x + dx),
                n(y),
                n(x + dx + dx2),
                n(y + head * 0.5),
                n(x + dx),
                n(y + head),
                n(x + dx),
                n(y + head / 2.0 + dy),
                n(dx - 2.0 * dy),
                n(dx - 2.0 * dy),
                n(x + dx),
                n(bottom - 2.0 * dy),
                n(x + far),
                n(bottom - 2.0 * dy),
                n(x + far),
                n(bottom),
                n(x + dx),
                n(bottom),
                n(dx),
                n(dx),
                n(x + dx),
                n(y + head / 2.0 - dy)
            ))
        }
        // The cross that ends a UML lifeline.
        "umlDestroy" => Paths::fill_only(
            String::new(),
            vec![
                line_path((right, y), (x, bottom)),
                line_path((x, y), (right, bottom)),
            ],
        ),
        // A provided interface: a circle on a stalk.
        "lollipop" => {
            let size = style.number("size", 10.0).clamp(0.0, height);
            Paths::with(
                ellipse_path(Rect {
                    x: cx - size / 2.0,
                    y,
                    width: size,
                    height: size,
                }),
                vec![line_path((cx, y + size), (cx, bottom))],
            )
        }
        // A required interface: the socket half of the same pair.
        "requiredInterface" => Paths::fill_only(
            String::new(),
            vec![format!(
                "M {} {} Q {} {} {} {} Q {} {} {} {}",
                n(x),
                n(y),
                n(right),
                n(y),
                n(right),
                n(cy),
                n(right),
                n(bottom),
                n(x),
                n(bottom)
            )],
        ),
        "corner" => {
            let dx = style.number("dx", 20.0).clamp(0.0, width);
            let dy = style.number("dy", 20.0).clamp(0.0, height);
            Paths::new(polygon_path(&[
                (x, y),
                (right, y),
                (right, y + dy),
                (x + dx, y + dy),
                (x + dx, bottom),
                (x, bottom),
            ]))
        }
        // Four quadratic curves that bow towards the centre.
        "switch" => Paths::new(format!(
            "M {} {} Q {} {} {} {} Q {} {} {} {} Q {} {} {} {} Q {} {} {} {} Z",
            n(x),
            n(y),
            n(cx),
            n(y + height * 0.5),
            n(right),
            n(y),
            n(x + width * 0.5),
            n(cy),
            n(right),
            n(bottom),
            n(cx),
            n(y + height * 0.5),
            n(x),
            n(bottom),
            n(x + width * 0.5),
            n(cy),
            n(x),
            n(y)
        )),
        // A module: a box with two tabs on its left edge.
        "module" => {
            let dx = style.number("jettywidth", 20.0).clamp(0.0, width);
            let dy = style.number("jettyheight", 10.0).clamp(0.0, height);
            let x0 = x + dx / 2.0;
            let x1 = x0 + dx / 2.0;
            let y0 = y + dy.min(height - dy).max(0.0);
            let y1 = (y0 + 2.0 * dy).min(bottom - dy).max(y0);
            Paths::with(
                polygon_path(&[
                    (x0, y),
                    (right, y),
                    (right, bottom),
                    (x0, bottom),
                    (x0, y1 + dy),
                    (x, y1 + dy),
                    (x, y1),
                    (x0, y1),
                    (x0, y0 + dy),
                    (x, y0 + dy),
                    (x, y0),
                    (x0, y0),
                ]),
                vec![
                    polyline_path(&[(x0, y0), (x1, y0), (x1, y0 + dy), (x0, y0 + dy)]),
                    polyline_path(&[(x0, y1), (x1, y1), (x1, y1 + dy), (x0, y1 + dy)]),
                ],
            )
        }
        "crossbar" => Paths::fill_only(
            String::new(),
            vec![
                line_path((x, y), (x, bottom)),
                line_path((right, y), (right, bottom)),
                line_path((x, cy), (right, cy)),
            ],
        ),
        // A measurement: two witness lines and a double-headed dimension line.
        "dimension" => {
            let stroke = style.number("strokewidth", 1.0).max(0.0) / 2.0;
            let arrow = 10.0 + 2.0 * stroke;
            let line = bottom - arrow / 2.0;
            Paths::fill_only(
                String::new(),
                vec![
                    line_path((x, y), (x, bottom)),
                    line_path((right, y), (right, bottom)),
                    line_path((x + stroke, line), (right - stroke, line)),
                    polyline_path(&[
                        (x + stroke + arrow, line - arrow / 2.0),
                        (x + stroke, line),
                        (x + stroke + arrow, line + arrow / 2.0),
                    ]),
                    polyline_path(&[
                        (right - stroke - arrow, line - arrow / 2.0),
                        (right - stroke, line),
                        (right - stroke - arrow, line + arrow / 2.0),
                    ]),
                ],
            )
        }
        "startState" => Paths::new(ellipse_path(rect)),
        "endState" => {
            let inset = width.min(height) * 0.2;
            Paths::with(ellipse_path(rect), vec![ellipse_path(rect.grow(-inset))])
        }
        "wall" => {
            let thickness = style.number("wallthickness", 10.0).clamp(0.0, height);
            Paths::new(rectangle_path(Rect {
                x,
                y: cy - thickness / 2.0,
                width,
                height: thickness,
            }))
        }
        "wallCorner" => {
            let thickness = style
                .number("wallthickness", 10.0)
                .clamp(0.0, width.min(height));
            Paths::new(polygon_path(&[
                (x, bottom),
                (x, y),
                (right, y),
                (right, y + thickness),
                (x + thickness, y + thickness),
                (x + thickness, bottom),
            ]))
        }
        "window" => {
            let thickness = style.number("wallthickness", 10.0).clamp(0.0, height);
            Paths::with(
                rectangle_path(Rect {
                    x,
                    y: cy - thickness / 2.0,
                    width,
                    height: thickness,
                }),
                vec![line_path((x, cy), (right, cy))],
            )
        }
        // A door is a frame of a fixed five units, with the leaf's swing drawn
        // as a quarter circle of the opening's width.
        "doorLeft" | "doorRight" => {
            let frame = 5.0_f64.min(height);
            let sweep = u8::from(name == "doorLeft");
            let (from, to) = if name == "doorLeft" {
                ((right, y + frame), (x, y + frame + width))
            } else {
                ((x, y + frame), (right, y + frame + width))
            };
            Paths::with(
                rectangle_path(Rect {
                    x,
                    y,
                    width,
                    height: frame,
                }),
                vec![format!(
                    "M {} {} A {} {} 0 0 {sweep} {} {} L {} {}",
                    n(from.0),
                    n(from.1),
                    n(width),
                    n(width),
                    n(to.0),
                    n(to.1),
                    n(if name == "doorLeft" { x } else { right }),
                    n(y + frame)
                )],
            )
        }
        "doorDouble" => {
            let frame = 5.0_f64.min(height);
            let half = width / 2.0;
            Paths::with(
                rectangle_path(Rect {
                    x,
                    y,
                    width,
                    height: frame,
                }),
                vec![
                    line_path((cx, y), (cx, y + frame)),
                    format!(
                        "M {} {} A {} {} 0 0 1 {} {} L {} {}",
                        n(cx),
                        n(y + frame),
                        n(half),
                        n(half),
                        n(x),
                        n(y + frame + half),
                        n(x),
                        n(y + frame)
                    ),
                    format!(
                        "M {} {} A {} {} 0 0 0 {} {} L {} {}",
                        n(cx),
                        n(y + frame),
                        n(half),
                        n(half),
                        n(right),
                        n(y + frame + half),
                        n(right),
                        n(y + frame)
                    ),
                ],
            )
        }
        // A group only positions its children; draw.io gives it no outline. A
        // waypoint is the same idea for an edge: a point a route passes
        // through, with nothing drawn at it.
        "group" | "waypoint" => Paths::new(String::new()),
        "partialRectangle" => {
            // Table and lane cells draw only the sides they switch on.
            let mut sides = Vec::new();
            if style.get("top") != Some("0") {
                sides.push(line_path((x, y), (right, y)));
            }
            if style.get("right") != Some("0") {
                sides.push(line_path((right, y), (right, bottom)));
            }
            if style.get("bottom") != Some("0") {
                sides.push(line_path((x, bottom), (right, bottom)));
            }
            if style.get("left") != Some("0") {
                sides.push(line_path((x, y), (x, bottom)));
            }
            Paths::fill_only(rectangle_path(rect), sides)
        }
        "umlLifeline" => {
            let head = style.number("size", 40.0).min(height).max(0.0);
            let head_rect = Rect {
                height: head,
                ..rect
            };
            let outline = if style.flag("rounded") {
                rounded_rect_path(head_rect, corner_radius(head_rect, style))
            } else {
                rectangle_path(head_rect)
            };
            Paths::with(outline, vec![line_path((cx, y + head), (cx, bottom))])
        }
        "umlFrame" => Paths::with(
            rectangle_path(rect),
            vec![polyline_path(&[
                (x + umlframe_tab(rect, style).width, y),
                (
                    x + umlframe_tab(rect, style).width,
                    y + umlframe_tab(rect, style).height,
                ),
                (x, y + umlframe_tab(rect, style).height),
            ])],
        ),
        "message" => Paths::with(
            rectangle_path(rect),
            vec![polyline_path(&[(x, y), (cx, cy), (right, y)])],
        ),
        "callout" => {
            let base = height * style.number("size", 0.3).clamp(0.0, 1.0);
            let position = width * style.number("position", 0.5).clamp(0.0, 1.0);
            let arrow = width * style.number("base", 0.2).clamp(0.0, 1.0);
            Paths::new(polygon_path(&[
                (x, y),
                (right, y),
                (right, bottom - base),
                (x + (position + arrow / 2.0).min(width), bottom - base),
                (x + position, bottom),
                (x + (position - arrow / 2.0).max(0.0), bottom - base),
                (x, bottom - base),
            ]))
        }
        _ => return None,
    })
}

/// The tab a UML frame carries in its top-left corner, which is also where the
/// frame's name goes.
pub(super) fn umlframe_tab(rect: Rect, style: &Style) -> Rect {
    Rect {
        width: style.number("width", 60.0).clamp(0.0, rect.width),
        height: style.number("height", 30.0).clamp(0.0, rect.height),
        ..rect
    }
}

/// The tab a folder carries, which a package also uses for its name.
pub(super) fn folder_tab(rect: Rect, style: &Style) -> Rect {
    let width = style.number("tabwidth", 60.0).clamp(0.0, rect.width);
    let height = style.number("tabheight", 20.0).clamp(0.0, rect.height);
    let right = if style
        .get("tabposition")
        .is_some_and(|value| value == "left")
    {
        rect.x + width
    } else {
        rect.right()
    };
    Rect {
        x: right - width,
        y: rect.y,
        width,
        height,
    }
}

/// The columns and rows a mock-up icon grid is laid out in, from its
/// `gridSize="columns,rows"` style.
pub(super) fn grid_size(style: &Style, default: &str) -> (f64, f64) {
    let value = style.text("gridsize", default);
    let mut parts = value.split(',').map(|part| {
        part.trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map_or(3.0, |value| value.clamp(1.0, 64.0).trunc())
    });
    (parts.next().unwrap_or(3.0), parts.next().unwrap_or(3.0))
}

/// One of the AWS 3D set's ground arrows: a shaft along the box's diagonal, a
/// flat head at one end and a rounded foot at the other.
pub(super) fn isometric_arrow_path(name: &str, rect: Rect) -> String {
    let Rect { x, y, .. } = rect;
    let (right, bottom) = (rect.right(), rect.bottom());
    // Each variant is the same drawing read from a different corner.
    let (head, foot, sweep) = match name {
        "isometricArrowNE" => ((right, y), (x, bottom), 1),
        "isometricArrowSE" => ((right, bottom), (x, y), 0),
        "isometricArrowSW" => ((x, bottom), (right, y), 1),
        _ => ((x, y), (right, bottom), 0),
    };
    let horizontal = if head.0 > foot.0 { -1.0 } else { 1.0 };
    let vertical = if head.1 > foot.1 { -1.0 } else { 1.0 };
    let head_point =
        |along: f64, across: f64| (head.0 + horizontal * along, head.1 + vertical * across);
    let foot_point =
        |along: f64, across: f64| (foot.0 - horizontal * along, foot.1 - vertical * across);
    if name == "isometricArrowlessNE" {
        let start = head_point(3.1, 0.0);
        let shaft = head_point(0.0, 1.8);
        let tip = foot_point(9.7, 3.5);
        return format!(
            "M {} {} L {} {} L {} {} A 6 3 0 0 {sweep} {} {} A 5.2 3 0 0 {sweep} {} {} A 6 2.8 0 0 {sweep} {} {} A 5 3 0 0 {sweep} {} {} Z",
            n(start.0),
            n(start.1),
            n(shaft.0),
            n(shaft.1),
            n(tip.0),
            n(tip.1),
            n(foot_point(9.0, 0.4).0),
            n(foot_point(9.0, 0.4).1),
            n(foot_point(1.0, 1.4).0),
            n(foot_point(1.0, 1.4).1),
            n(foot_point(3.0, 5.4).0),
            n(foot_point(3.0, 5.4).1),
            n(foot_point(6.7, 5.2).0),
            n(foot_point(6.7, 5.2).1)
        );
    }
    format!(
        "M {} {} L {} {} L {} {} L {} {} L {} {} L {} {} A 6 3 0 0 {sweep} {} {} A 5.2 3 0 0 {sweep} {} {} A 6 2.8 0 0 {sweep} {} {} A 5 3 0 0 {sweep} {} {} Z",
        n(head_point(17.0, 8.0).0),
        n(head_point(17.0, 8.0).1),
        n(head_point(21.0, 5.5).0),
        n(head_point(21.0, 5.5).1),
        n(head.0),
        n(head.1),
        n(head_point(9.7, 12.2).0),
        n(head_point(9.7, 12.2).1),
        n(head_point(13.9, 9.8).0),
        n(head_point(13.9, 9.8).1),
        n(foot_point(9.7, 3.5).0),
        n(foot_point(9.7, 3.5).1),
        n(foot_point(9.0, 0.4).0),
        n(foot_point(9.0, 0.4).1),
        n(foot_point(1.0, 1.4).0),
        n(foot_point(1.0, 1.4).1),
        n(foot_point(3.0, 5.4).0),
        n(foot_point(3.0, 5.4).1),
        n(foot_point(6.7, 5.2).0),
        n(foot_point(6.7, 5.2).1)
    )
}

/// The AWS 3D set's double-headed ground connector, whose head geometry is
/// fixed while its length follows the cell.
pub(super) fn isometric_double_edge_path(rect: Rect) -> String {
    let Rect { x, y, .. } = rect;
    let (right, bottom) = (rect.right(), rect.bottom());
    polygon_path(&[
        (x + 15.3, y + 61.9),
        (x + 30.8, y + 53.2),
        (x + 15.4, y + 44.2),
        (x, y + 53.2),
        (x + 15.4, y + 8.8),
        (x + 92.1, y),
        (x + 76.5, y + 8.8),
        (x + 92.1, y + 17.7),
        (x + 107.4, y + 8.8),
        (right - 15.3, bottom - 61.9),
        (right - 30.8, bottom - 53.2),
        (right - 15.4, bottom - 44.2),
        (right, bottom - 53.2),
        (right - 15.4, bottom - 8.8),
        (right - 92.1, bottom),
        (right - 76.5, bottom - 8.8),
        (right - 92.1, bottom - 17.7),
        (right - 107.4, bottom - 8.8),
    ])
}

/// Every finite number in a style value, in the order they are written.
///
/// draw.io stores a polygon's corners as a JSON array of pairs, which is the
/// only structured value a style carries; reading the numbers out in order is
/// enough to rebuild it without parsing JSON.
fn numbers(text: &str) -> Vec<f64> {
    let mut found = Vec::new();
    let mut token = String::new();
    for character in text.chars().chain(std::iter::once(' ')) {
        if character.is_ascii_digit() || matches!(character, '.' | '-' | '+' | 'e' | 'E') {
            token.push(character);
            continue;
        }
        if let Ok(value) = token.parse::<f64>()
            && value.is_finite()
        {
            found.push(value);
        }
        token.clear();
        if found.len() >= 512 {
            break;
        }
    }
    found
}

/// The control point of each side a `polyCurves` list bends.
///
/// draw.io writes one entry per side, either `null` for a straight one or
/// `["Q", x, y]` for a quadratic. The entries are read in order, so a straight
/// side has to keep its place in the list rather than be skipped.
fn curve_controls(text: &str) -> Vec<Option<(f64, f64)>> {
    let mut sides = Vec::new();
    let mut depth = 0usize;
    let mut item = String::new();
    for character in text.chars() {
        match character {
            '[' => {
                depth += 1;
                if depth > 1 {
                    item.push(character);
                }
            }
            ']' => {
                depth = depth.saturating_sub(1);
                if depth >= 1 {
                    item.push(character);
                } else {
                    break;
                }
            }
            ',' if depth == 1 => {
                sides.push(quadratic_control(&item));
                item.clear();
            }
            _ if depth >= 1 => item.push(character),
            _ => {}
        }
        if sides.len() >= 512 {
            break;
        }
    }
    if !item.trim().is_empty() {
        sides.push(quadratic_control(&item));
    }
    sides
}

fn quadratic_control(item: &str) -> Option<(f64, f64)> {
    if !item.contains('Q') {
        return None;
    }
    let values = numbers(item);
    match values[..] {
        [at, down, ..] => Some((at, down)),
        _ => None,
    }
}
