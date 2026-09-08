//! Deterministic SVG serialization for the public [`crate::ir`] model.
//!
//! Normal document conversion should use [`crate::convert_path`]. Call
//! [`write_page`] directly only when an application already has an
//! [`crate::ir::Page`] that it wants to serialize.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::Write;

use crate::error::Result;
use crate::ir::{
    ClipPath, ImageColorEffect, LineCap, LineJoin, Matrix, Node, Page, Paint, Stroke, TextAnchor,
    TilingPatternDefinition,
};

#[derive(Debug, Clone, Copy)]
pub struct SvgOptions {
    pub include_metadata: bool,
    pub precision: usize,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            include_metadata: true,
            precision: 5,
        }
    }
}

pub fn write_page<W: Write>(page: &Page, mut output: W, options: SvgOptions) -> Result<()> {
    let options = SvgOptions {
        precision: options.precision.min(12),
        ..options
    };
    let clip_parents = page
        .clips
        .iter()
        .map(|clip| (clip.id.clone(), clip.parent_id.clone()))
        .collect::<HashMap<_, _>>();
    writeln!(output, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(
        output,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{}pt\" height=\"{}pt\" viewBox=\"0 0 {} {}\" data-source-format=\"{}\" data-source-page=\"{}\">",
        number(page.width, options.precision),
        number(page.height, options.precision),
        number(page.width, options.precision),
        number(page.height, options.precision),
        escape_attr(&page.source_format),
        page.number
    )?;
    if !page.title.is_empty() {
        writeln!(output, "  <title>{}</title>", escape_text(&page.title))?;
    }
    if !page.description.is_empty() {
        writeln!(output, "  <desc>{}</desc>", escape_text(&page.description))?;
    }
    if options.include_metadata {
        let warnings = serde_json::to_string(&page.warnings)?;
        writeln!(
            output,
            "  <metadata id=\"docsvg-metadata\">{}</metadata>",
            escape_text(&format!(
                "{{\"page\":{},\"nodes\":{},\"warnings\":{warnings}}}",
                page.number,
                page.nodes.len()
            ))
        )?;
    }
    writeln!(
        output,
        "  <rect x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" fill=\"#FFFFFF\" data-role=\"page-background\"/>",
        number(page.width, options.precision),
        number(page.height, options.precision)
    )?;
    let mut gradient_use_index = 0usize;
    if !page.clips.is_empty()
        || !page.masks.is_empty()
        || !page.patterns.is_empty()
        || has_gradients(&page.nodes)
        || has_drawingml_effects(&page.nodes)
    {
        writeln!(output, "  <defs>")?;
        let mut shadow_ids = HashSet::new();
        write_drawingml_effect_definitions(
            &mut output,
            &page.nodes,
            page.width,
            page.height,
            options.precision,
            &mut shadow_ids,
        )?;
        for clip in &page.clips {
            write_clip(&mut output, clip, options.precision)?;
        }
        let mut gradient_index = 0usize;
        for pattern in &page.patterns {
            for node in &pattern.nodes {
                write_gradient_definitions(
                    &mut output,
                    node,
                    &mut gradient_index,
                    options.precision,
                )?;
            }
            write_tiling_pattern(
                &mut output,
                pattern,
                &mut gradient_use_index,
                options.precision,
                &clip_parents,
            )?;
        }
        for mask in &page.masks {
            for node in &mask.nodes {
                write_gradient_definitions(
                    &mut output,
                    node,
                    &mut gradient_index,
                    options.precision,
                )?;
            }
        }
        for node in &page.nodes {
            write_gradient_definitions(&mut output, node, &mut gradient_index, options.precision)?;
        }
        for mask in &page.masks {
            if !mask.transfer_values.is_empty() {
                write_mask_transfer_filter(
                    &mut output,
                    mask,
                    page.width,
                    page.height,
                    options.precision,
                )?;
            }
            writeln!(
                output,
                "    <mask id=\"{}\" maskUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" style=\"mask-type:{}\">",
                escape_attr(&mask.id),
                number(page.width, options.precision),
                number(page.height, options.precision),
                escape_attr(&mask.mask_type)
            )?;
            if !mask.transfer_values.is_empty() {
                writeln!(
                    output,
                    "      <g filter=\"url(#{}-transfer)\">",
                    escape_attr(&mask.id)
                )?;
            }
            for node in &mask.nodes {
                write_node(
                    &mut output,
                    node,
                    3 + usize::from(!mask.transfer_values.is_empty()),
                    &mut gradient_use_index,
                    options.precision,
                    &clip_parents,
                )?;
            }
            if !mask.transfer_values.is_empty() {
                writeln!(output, "      </g>")?;
            }
            writeln!(output, "    </mask>")?;
        }
        writeln!(output, "  </defs>")?;
    }
    let mut gradient_index = gradient_use_index;
    for node in &page.nodes {
        write_node(
            &mut output,
            node,
            1,
            &mut gradient_index,
            options.precision,
            &clip_parents,
        )?;
    }
    writeln!(output, "</svg>")?;
    Ok(())
}

fn write_mask_transfer_filter<W: Write>(
    output: &mut W,
    mask: &crate::ir::MaskDefinition,
    width: f64,
    height: f64,
    precision: usize,
) -> Result<()> {
    let table = mask
        .transfer_values
        .iter()
        .map(|value| number(value.clamp(0.0, 1.0), precision))
        .collect::<Vec<_>>()
        .join(" ");
    writeln!(
        output,
        "    <filter id=\"{}-transfer\" filterUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" color-interpolation-filters=\"sRGB\">",
        escape_attr(&mask.id),
        number(width, precision),
        number(height, precision)
    )?;
    if mask.mask_type == "luminance" {
        writeln!(
            output,
            "      <feColorMatrix type=\"matrix\" values=\"0.2126 0.7152 0.0722 0 0 0.2126 0.7152 0.0722 0 0 0.2126 0.7152 0.0722 0 0 0 0 0 1 0\"/>"
        )?;
        writeln!(
            output,
            "      <feComponentTransfer><feFuncR type=\"table\" tableValues=\"{table}\"/><feFuncG type=\"table\" tableValues=\"{table}\"/><feFuncB type=\"table\" tableValues=\"{table}\"/></feComponentTransfer>"
        )?;
    } else {
        writeln!(
            output,
            "      <feComponentTransfer><feFuncA type=\"table\" tableValues=\"{table}\"/></feComponentTransfer>"
        )?;
    }
    writeln!(output, "    </filter>")?;
    Ok(())
}

fn write_tiling_pattern<W: Write>(
    output: &mut W,
    pattern: &TilingPatternDefinition,
    gradient_index: &mut usize,
    precision: usize,
    clip_parents: &HashMap<String, Option<String>>,
) -> Result<()> {
    writeln!(
        output,
        "    <pattern id=\"{}\" patternUnits=\"userSpaceOnUse\" patternContentUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" patternTransform=\"{}\">",
        escape_attr(&pattern.id),
        number(pattern.x, precision),
        number(pattern.y, precision),
        number(pattern.width.abs().max(1e-9), precision),
        number(pattern.height.abs().max(1e-9), precision),
        matrix(pattern.transform, precision),
    )?;
    for node in &pattern.nodes {
        write_node(output, node, 3, gradient_index, precision, clip_parents)?;
    }
    writeln!(output, "    </pattern>")?;
    Ok(())
}

fn write_clip<W: Write>(output: &mut W, clip: &ClipPath, precision: usize) -> Result<()> {
    write!(
        output,
        "    <clipPath id=\"{}\" clipPathUnits=\"userSpaceOnUse\">",
        escape_attr(&clip.id),
    )?;
    write!(
        output,
        "<path d=\"{}\" transform=\"{}\" clip-rule=\"{}\"/>",
        escape_attr(&clip.d),
        matrix(clip.transform, precision),
        escape_attr(&clip.fill_rule)
    )?;
    for member in &clip.additional_paths {
        write!(
            output,
            "<path d=\"{}\" transform=\"{}\" clip-rule=\"{}\"/>",
            escape_attr(&member.d),
            matrix(member.transform, precision),
            escape_attr(&member.fill_rule)
        )?;
    }
    writeln!(output, "</clipPath>")?;
    Ok(())
}

fn write_drawingml_effect_definitions<W: Write>(
    output: &mut W,
    nodes: &[Node],
    page_width: f64,
    page_height: f64,
    precision: usize,
    emitted: &mut HashSet<String>,
) -> Result<()> {
    for node in nodes {
        let (id, meta, children) = match node {
            Node::Path { id, meta, .. }
            | Node::Text { id, meta, .. }
            | Node::Image { id, meta, .. } => (id, meta, None),
            Node::Group {
                id, meta, nodes, ..
            } => (id, meta, Some(nodes.as_slice())),
        };
        if let Some(filter_prefix) = drawingml_filter_prefix(meta)
            && emitted.insert(id.clone())
        {
            let shadow_values = meta.outer_shadow.as_ref().map(|shadow| {
                let angle = shadow.direction_degrees.to_radians();
                let distance = shadow.distance.clamp(0.0, 4_096.0);
                let blur_radius = shadow.blur_radius.clamp(0.0, 512.0);
                (
                    blur_radius,
                    distance * angle.cos(),
                    distance * angle.sin(),
                    blur_radius * 3.0 + distance + 1.0,
                )
            });
            let glow_radius = meta
                .glow
                .as_ref()
                .map_or(0.0, |glow| glow.radius.clamp(0.0, 512.0));
            let padding = shadow_values
                .map_or(0.0, |values| values.3)
                .max(glow_radius * 3.0 + 1.0);
            writeln!(
                output,
                "    <filter id=\"{}-{}\" filterUnits=\"userSpaceOnUse\" primitiveUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" color-interpolation-filters=\"sRGB\">",
                filter_prefix,
                escape_attr(id),
                number(-padding, precision),
                number(-padding, precision),
                number(page_width + padding * 2.0, precision),
                number(page_height + padding * 2.0, precision),
            )?;
            let image_input = write_image_color_effects(output, &meta.image_effects, precision)?;
            let alpha_input = if meta.image_effects.is_empty() {
                "SourceAlpha"
            } else {
                image_input.as_str()
            };
            if let (Some(shadow), Some((blur_radius, dx, dy, _))) =
                (&meta.outer_shadow, shadow_values)
            {
                writeln!(
                    output,
                    "      <feGaussianBlur in=\"{}\" stdDeviation=\"{}\" result=\"shadow-blur\"/>",
                    alpha_input,
                    number(blur_radius / 2.0, precision)
                )?;
                writeln!(
                    output,
                    "      <feOffset in=\"shadow-blur\" dx=\"{}\" dy=\"{}\" result=\"shadow-offset\"/>",
                    number(dx, precision),
                    number(dy, precision)
                )?;
                writeln!(
                    output,
                    "      <feFlood flood-color=\"{}\" flood-opacity=\"{}\" result=\"shadow-color\"/>",
                    escape_attr(&shadow.color),
                    number(shadow.opacity.clamp(0.0, 1.0), precision)
                )?;
                writeln!(
                    output,
                    "      <feComposite in=\"shadow-color\" in2=\"shadow-offset\" operator=\"in\" result=\"shadow\"/>"
                )?;
            }
            if let Some(glow) = &meta.glow {
                writeln!(
                    output,
                    "      <feGaussianBlur in=\"{}\" stdDeviation=\"{}\" result=\"glow-blur\"/>",
                    alpha_input,
                    number(glow_radius / 2.0, precision)
                )?;
                writeln!(
                    output,
                    "      <feFlood flood-color=\"{}\" flood-opacity=\"{}\" result=\"glow-color\"/>",
                    escape_attr(&glow.color),
                    number(glow.opacity.clamp(0.0, 1.0), precision)
                )?;
                writeln!(
                    output,
                    "      <feComposite in=\"glow-color\" in2=\"glow-blur\" operator=\"in\" result=\"glow\"/>"
                )?;
            }
            write!(output, "      <feMerge>")?;
            if meta.outer_shadow.is_some() {
                write!(output, "<feMergeNode in=\"shadow\"/>")?;
            }
            if meta.glow.is_some() {
                write!(output, "<feMergeNode in=\"glow\"/>")?;
            }
            writeln!(output, "<feMergeNode in=\"{}\"/></feMerge>", image_input)?;
            writeln!(output, "    </filter>")?;
        }
        if let Some(children) = children {
            write_drawingml_effect_definitions(
                output,
                children,
                page_width,
                page_height,
                precision,
                emitted,
            )?;
        }
    }
    Ok(())
}

fn write_image_color_effects<W: Write>(
    output: &mut W,
    effects: &[ImageColorEffect],
    precision: usize,
) -> Result<String> {
    let mut input = "SourceGraphic".to_owned();
    for (index, effect) in effects.iter().enumerate() {
        let number_index = index + 1;
        let result = format!("image-effect-{number_index}");
        match effect {
            ImageColorEffect::Duotone { dark, light } => {
                let dark = svg_hex_rgb(dark).unwrap_or([0.0; 3]);
                let light = svg_hex_rgb(light).unwrap_or([1.0; 3]);
                let gray = format!("{result}-gray");
                writeln!(
                    output,
                    "      <feColorMatrix in=\"{}\" type=\"matrix\" values=\"0.2126 0.7152 0.0722 0 0 0.2126 0.7152 0.0722 0 0 0.2126 0.7152 0.0722 0 0 0 0 0 1 0\" result=\"{}\"/>",
                    input, gray
                )?;
                writeln!(
                    output,
                    "      <feComponentTransfer in=\"{}\" result=\"{}\"><feFuncR type=\"linear\" slope=\"{}\" intercept=\"{}\"/><feFuncG type=\"linear\" slope=\"{}\" intercept=\"{}\"/><feFuncB type=\"linear\" slope=\"{}\" intercept=\"{}\"/></feComponentTransfer>",
                    gray,
                    result,
                    number(light[0] - dark[0], precision + 2),
                    number(dark[0], precision + 2),
                    number(light[1] - dark[1], precision + 2),
                    number(dark[1], precision + 2),
                    number(light[2] - dark[2], precision + 2),
                    number(dark[2], precision + 2),
                )?;
            }
            ImageColorEffect::Grayscale => {
                writeln!(
                    output,
                    "      <feColorMatrix in=\"{}\" type=\"matrix\" values=\"0.2126 0.7152 0.0722 0 0 0.2126 0.7152 0.0722 0 0 0.2126 0.7152 0.0722 0 0 0 0 0 1 0\" result=\"{}\"/>",
                    input, result
                )?;
            }
            ImageColorEffect::Luminance {
                brightness,
                contrast,
            } => {
                let contrast = contrast.clamp(-0.999, 0.999);
                let (contrast_slope, contrast_intercept) = if contrast >= 0.0 {
                    let slope = 1.0 / (1.0 - contrast);
                    (slope, 0.5 - slope * 0.5)
                } else {
                    let slope = 1.0 + contrast;
                    (slope, 0.5 - slope * 0.5)
                };
                let brightness = brightness.clamp(-1.0, 1.0);
                let brightness_slope = 1.0 - brightness.abs();
                let slope = contrast_slope * brightness_slope;
                let intercept = contrast_intercept * brightness_slope + brightness.max(0.0);
                writeln!(
                    output,
                    "      <feComponentTransfer in=\"{}\" result=\"{}\"><feFuncR type=\"linear\" slope=\"{}\" intercept=\"{}\"/><feFuncG type=\"linear\" slope=\"{}\" intercept=\"{}\"/><feFuncB type=\"linear\" slope=\"{}\" intercept=\"{}\"/></feComponentTransfer>",
                    input,
                    result,
                    number(slope, precision + 2),
                    number(intercept, precision + 2),
                    number(slope, precision + 2),
                    number(intercept, precision + 2),
                    number(slope, precision + 2),
                    number(intercept, precision + 2),
                )?;
            }
            ImageColorEffect::ColorChange {
                from,
                to,
                to_opacity,
            } => {
                let target = format!("{result}-target");
                let difference = format!("{result}-difference");
                let distance = format!("{result}-distance");
                let mask = format!("{result}-mask");
                let replacement_color = format!("{result}-replacement-color");
                let replacement = format!("{result}-replacement");
                let remainder = format!("{result}-remainder");
                writeln!(
                    output,
                    "      <feFlood flood-color=\"{}\" result=\"{}\"/>",
                    escape_attr(from),
                    target
                )?;
                writeln!(
                    output,
                    "      <feBlend in=\"{}\" in2=\"{}\" mode=\"difference\" result=\"{}\"/>",
                    input, target, difference
                )?;
                writeln!(
                    output,
                    "      <feColorMatrix in=\"{}\" type=\"matrix\" values=\"0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 255 255 255 0 0\" result=\"{}\"/>",
                    difference, distance
                )?;
                writeln!(
                    output,
                    "      <feComponentTransfer in=\"{}\" result=\"{}\"><feFuncA type=\"linear\" slope=\"-1\" intercept=\"1\"/></feComponentTransfer>",
                    distance, mask
                )?;
                writeln!(
                    output,
                    "      <feFlood flood-color=\"{}\" flood-opacity=\"{}\" result=\"{}\"/>",
                    escape_attr(to),
                    number(to_opacity.clamp(0.0, 1.0), precision),
                    replacement_color
                )?;
                writeln!(
                    output,
                    "      <feComposite in=\"{}\" in2=\"{}\" operator=\"in\" result=\"{}\"/>",
                    replacement_color, mask, replacement
                )?;
                writeln!(
                    output,
                    "      <feComposite in=\"{}\" in2=\"{}\" operator=\"out\" result=\"{}\"/>",
                    input, mask, remainder
                )?;
                writeln!(
                    output,
                    "      <feMerge result=\"{}\"><feMergeNode in=\"{}\"/><feMergeNode in=\"{}\"/></feMerge>",
                    result, remainder, replacement
                )?;
            }
        }
        input = result;
    }
    Ok(input)
}

fn svg_hex_rgb(color: &str) -> Option<[f64; 3]> {
    let color = color.trim_start_matches('#');
    if color.len() != 6 {
        return None;
    }
    Some([
        f64::from(u8::from_str_radix(&color[0..2], 16).ok()?) / 255.0,
        f64::from(u8::from_str_radix(&color[2..4], 16).ok()?) / 255.0,
        f64::from(u8::from_str_radix(&color[4..6], 16).ok()?) / 255.0,
    ])
}

fn drawingml_filter_prefix(meta: &crate::ir::SourceMeta) -> Option<&'static str> {
    let shadow = meta.outer_shadow.is_some();
    let glow = meta.glow.is_some();
    let image = !meta.image_effects.is_empty();
    match (shadow, glow, image) {
        (false, false, false) => None,
        (true, false, false) => Some("outer-shadow"),
        (false, true, false) => Some("glow"),
        (false, false, true) => Some("image-effects"),
        _ => Some("drawingml-effects"),
    }
}

fn write_gradient_definitions<W: Write>(
    output: &mut W,
    node: &Node,
    gradient_index: &mut usize,
    precision: usize,
) -> Result<()> {
    match node {
        Node::Path { fill, stroke, .. } => {
            write_paint_gradient(output, fill, gradient_index, precision)?;
            write_paint_gradient(output, &stroke.paint, gradient_index, precision)?;
        }
        Node::Text { runs, .. } => {
            for run in runs {
                write_paint_gradient(output, &run.fill, gradient_index, precision)?;
            }
        }
        Node::Group { nodes, .. } => {
            for child in nodes {
                write_gradient_definitions(output, child, gradient_index, precision)?;
            }
        }
        Node::Image { .. } => {}
    }
    Ok(())
}

fn write_paint_gradient<W: Write>(
    output: &mut W,
    paint: &Paint,
    gradient_index: &mut usize,
    precision: usize,
) -> Result<()> {
    let (stops, closing_tag) = match paint {
        Paint::LinearGradient(gradient) => {
            *gradient_index += 1;
            writeln!(
                output,
                "    <linearGradient id=\"gradient-{}\" gradientUnits=\"userSpaceOnUse\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\">",
                gradient_index,
                number(gradient.x1, precision),
                number(gradient.y1, precision),
                number(gradient.x2, precision),
                number(gradient.y2, precision)
            )?;
            (&gradient.stops, "linearGradient")
        }
        Paint::RadialGradient(gradient) => {
            *gradient_index += 1;
            writeln!(
                output,
                "    <radialGradient id=\"gradient-{}\" gradientUnits=\"userSpaceOnUse\" fx=\"{}\" fy=\"{}\" fr=\"{}\" cx=\"{}\" cy=\"{}\" r=\"{}\" gradientTransform=\"{}\">",
                gradient_index,
                number(gradient.fx, precision),
                number(gradient.fy, precision),
                number(gradient.fr, precision),
                number(gradient.cx, precision),
                number(gradient.cy, precision),
                number(gradient.radius, precision),
                matrix(gradient.transform, precision)
            )?;
            (&gradient.stops, "radialGradient")
        }
        _ => return Ok(()),
    };
    for stop in stops {
        writeln!(
            output,
            "      <stop offset=\"{}\" stop-color=\"{}\" stop-opacity=\"{}\"/>",
            number(stop.offset.clamp(0.0, 1.0), precision),
            escape_attr(&stop.color),
            number(stop.opacity.clamp(0.0, 1.0), precision)
        )?;
    }
    writeln!(output, "    </{closing_tag}>")?;
    Ok(())
}

fn write_node<W: Write>(
    output: &mut W,
    node: &Node,
    depth: usize,
    gradient_index: &mut usize,
    precision: usize,
    clip_parents: &HashMap<String, Option<String>>,
) -> Result<()> {
    let meta = match node {
        Node::Path { meta, .. }
        | Node::Text { meta, .. }
        | Node::Image { meta, .. }
        | Node::Group { meta, .. } => meta,
    };
    let has_effect_wrapper = !meta.mask_id.is_empty()
        || (!meta.blend_mode.is_empty() && meta.blend_mode != "normal")
        || meta.isolation;
    let node_clip_id = match node {
        Node::Path { clip_id, .. }
        | Node::Text { clip_id, .. }
        | Node::Image { clip_id, .. }
        | Node::Group { clip_id, .. } => clip_id.as_deref(),
    };
    let parent_clips = parent_clip_chain(node_clip_id, clip_parents);
    for (index, parent_clip) in parent_clips.iter().enumerate() {
        let indent = "  ".repeat(depth + index);
        writeln!(
            output,
            "{indent}<g clip-path=\"url(#{})\">",
            escape_attr(parent_clip)
        )?;
    }
    let effect_depth = depth + parent_clips.len();
    let wrapper_indent = "  ".repeat(effect_depth);
    if has_effect_wrapper {
        write!(output, "{wrapper_indent}<g")?;
        if !meta.mask_id.is_empty() {
            write!(output, " mask=\"url(#{})\"", escape_attr(&meta.mask_id))?;
        }
        let mut styles = Vec::new();
        if !meta.blend_mode.is_empty() && meta.blend_mode != "normal" {
            styles.push(format!("mix-blend-mode:{}", escape_attr(&meta.blend_mode)));
        }
        if meta.isolation {
            styles.push("isolation:isolate".into());
        }
        if !styles.is_empty() {
            write!(output, " style=\"{}\"", styles.join(";"))?;
        }
        writeln!(output, ">")?;
    }
    let content_depth = effect_depth + usize::from(has_effect_wrapper);
    let indent = "  ".repeat(content_depth);
    match node {
        Node::Path {
            id,
            d,
            fill_rule,
            fill,
            stroke,
            transform,
            clip_id: _,
            meta,
        } => {
            write!(
                output,
                "{indent}<path id=\"{}\" d=\"{}\" fill-rule=\"{}\" transform=\"{}\"",
                escape_attr(id),
                escape_attr(d),
                escape_attr(fill_rule),
                matrix(*transform, precision)
            )?;
            write_paint_attributes(output, fill, gradient_index, precision, "fill")?;
            write_stroke_attributes(output, stroke, gradient_index, precision)?;
            write_common_attributes(output, None, meta, id)?;
            writeln!(output, "/>")?;
        }
        Node::Text {
            id,
            x,
            y,
            runs,
            anchor,
            transform,
            opacity,
            stroke,
            clip_id: _,
            meta,
        } => {
            write!(
                output,
                "{indent}<text id=\"{}\" x=\"{}\" y=\"{}\" text-anchor=\"{}\" transform=\"{}\" opacity=\"{}\"",
                escape_attr(id),
                number(*x, precision),
                number(*y, precision),
                match anchor {
                    TextAnchor::Start => "start",
                    TextAnchor::Middle => "middle",
                    TextAnchor::End => "end",
                },
                matrix(*transform, precision),
                number(opacity.clamp(0.0, 1.0), precision)
            )?;
            write_stroke_attributes(output, stroke, gradient_index, precision)?;
            write_common_attributes(output, None, meta, id)?;
            writeln!(output, ">")?;
            for run in runs {
                write!(
                    output,
                    "{indent}  <tspan xml:space=\"preserve\" font-family=\"{}\" font-size=\"{}\" font-weight=\"{}\" font-style=\"{}\" baseline-shift=\"{}\"",
                    escape_attr(&run.font_family),
                    number(run.font_size, precision),
                    if run.bold { "700" } else { "400" },
                    if run.italic { "italic" } else { "normal" },
                    number(run.baseline_shift, precision)
                )?;
                if let Some(target_advance) = run
                    .target_advance
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .filter(|_| run.glyph_x_offsets.is_empty())
                {
                    write!(
                        output,
                        " textLength=\"{}\" lengthAdjust=\"spacingAndGlyphs\"",
                        number(target_advance, precision)
                    )?;
                }
                write_paint_attributes(output, &run.fill, gradient_index, precision, "fill")?;
                let characters = run.text.chars().collect::<Vec<_>>();
                if !characters.is_empty()
                    && characters.len() == run.glyph_x_offsets.len()
                    && run.glyph_x_offsets.iter().all(|value| value.is_finite())
                {
                    write!(output, ">")?;
                    for (character, x) in characters.iter().zip(&run.glyph_x_offsets) {
                        write!(
                            output,
                            "<tspan x=\"{}\" y=\"0\">{}</tspan>",
                            number(*x, precision),
                            escape_text(&character.to_string())
                        )?;
                    }
                    writeln!(output, "</tspan>")?;
                } else {
                    writeln!(output, ">{}</tspan>", escape_text(&run.text))?;
                }
            }
            writeln!(output, "{indent}</text>")?;
        }
        Node::Image {
            id,
            href,
            x,
            y,
            width,
            height,
            transform,
            opacity,
            clip_id: _,
            meta,
        } => {
            write!(
                output,
                "{indent}<image id=\"{}\" href=\"{}\" xlink:href=\"{}\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" transform=\"{}\" opacity=\"{}\" preserveAspectRatio=\"none\"",
                escape_attr(id),
                escape_attr(href),
                escape_attr(href),
                number(*x, precision),
                number(*y, precision),
                number(*width, precision),
                number(*height, precision),
                matrix(*transform, precision),
                number(opacity.clamp(0.0, 1.0), precision)
            )?;
            write_common_attributes(output, None, meta, id)?;
            writeln!(output, "/>")?;
        }
        Node::Group {
            id,
            nodes,
            transform,
            opacity,
            clip_id: _,
            meta,
        } => {
            write!(
                output,
                "{indent}<g id=\"{}\" transform=\"{}\" opacity=\"{}\"",
                escape_attr(id),
                matrix(*transform, precision),
                number(opacity.clamp(0.0, 1.0), precision)
            )?;
            write_common_attributes(output, None, meta, id)?;
            writeln!(output, ">")?;
            for child in nodes {
                write_node(
                    output,
                    child,
                    content_depth + 1,
                    gradient_index,
                    precision,
                    clip_parents,
                )?;
            }
            writeln!(output, "{indent}</g>")?;
        }
    }
    if has_effect_wrapper {
        writeln!(output, "{wrapper_indent}</g>")?;
    }
    for index in (0..parent_clips.len()).rev() {
        let indent = "  ".repeat(depth + index);
        writeln!(output, "{indent}</g>")?;
    }
    Ok(())
}

fn parent_clip_chain(
    clip_id: Option<&str>,
    clip_parents: &HashMap<String, Option<String>>,
) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = clip_id.map(str::to_owned);
    while let Some(parent) = current {
        if result.contains(&parent) || result.len() >= 256 {
            break;
        }
        current = clip_parents.get(&parent).and_then(Clone::clone);
        result.push(parent);
    }
    result.reverse();
    result
}

fn write_common_attributes<W: Write>(
    output: &mut W,
    clip_id: Option<&str>,
    meta: &crate::ir::SourceMeta,
    node_id: &str,
) -> Result<()> {
    if let Some(clip_id) = clip_id {
        write!(output, " clip-path=\"url(#{})\"", escape_attr(clip_id))?;
    }
    if !meta.kind.is_empty() {
        write!(output, " data-content-kind=\"{}\"", escape_attr(&meta.kind))?;
    }
    if !meta.source_id.is_empty() {
        write!(
            output,
            " data-source-id=\"{}\"",
            escape_attr(&meta.source_id)
        )?;
    }
    if !meta.semantic_role.is_empty() {
        write!(
            output,
            " data-semantic-role=\"{}\"",
            escape_attr(&meta.semantic_role)
        )?;
    }
    if !meta.alt_text.is_empty() {
        write!(output, " aria-label=\"{}\"", escape_attr(&meta.alt_text))?;
    }
    if !meta.image_rendering.is_empty() {
        write!(
            output,
            " image-rendering=\"{}\"",
            escape_attr(&meta.image_rendering)
        )?;
    }
    if !meta.shape_rendering.is_empty() {
        write!(
            output,
            " shape-rendering=\"{}\"",
            escape_attr(&meta.shape_rendering)
        )?;
    }
    if let Some(filter_prefix) = drawingml_filter_prefix(meta) {
        write!(
            output,
            " filter=\"url(#{}-{})\"",
            filter_prefix,
            escape_attr(node_id)
        )?;
    }
    Ok(())
}

fn write_stroke_attributes<W: Write>(
    output: &mut W,
    stroke: &Stroke,
    gradient_index: &mut usize,
    precision: usize,
) -> Result<()> {
    write_paint_attributes(output, &stroke.paint, gradient_index, precision, "stroke")?;
    if !matches!(stroke.paint, Paint::None) {
        write!(
            output,
            " stroke-width=\"{}\" stroke-linecap=\"{}\" stroke-linejoin=\"{}\" stroke-miterlimit=\"{}\"",
            number(stroke.width, precision),
            match stroke.line_cap {
                LineCap::Butt => "butt",
                LineCap::Round => "round",
                LineCap::Square => "square",
            },
            match stroke.line_join {
                LineJoin::Miter => "miter",
                LineJoin::Round => "round",
                LineJoin::Bevel => "bevel",
            },
            number(stroke.miter_limit, precision)
        )?;
        if !stroke.dash_array.is_empty() {
            let values = stroke
                .dash_array
                .iter()
                .map(|value| number(*value, precision))
                .collect::<Vec<_>>()
                .join(" ");
            write!(
                output,
                " stroke-dasharray=\"{values}\" stroke-dashoffset=\"{}\"",
                number(stroke.dash_offset, precision)
            )?;
        }
    }
    Ok(())
}

fn write_paint_attributes<W: Write>(
    output: &mut W,
    paint: &Paint,
    gradient_index: &mut usize,
    precision: usize,
    attribute: &str,
) -> Result<()> {
    match paint {
        Paint::None => write!(output, " {attribute}=\"none\"")?,
        Paint::Solid { color, opacity } => {
            write!(
                output,
                " {attribute}=\"{}\" {attribute}-opacity=\"{}\"",
                escape_attr(color),
                number(opacity.clamp(0.0, 1.0), precision)
            )?;
        }
        Paint::LinearGradient(_) | Paint::RadialGradient(_) => {
            *gradient_index += 1;
            write!(output, " {attribute}=\"url(#gradient-{gradient_index})\"")?;
        }
        Paint::PatternRef { id, opacity } => {
            write!(
                output,
                " {attribute}=\"url(#{})\" {attribute}-opacity=\"{}\"",
                escape_attr(id),
                number(opacity.clamp(0.0, 1.0), precision)
            )?;
        }
    }
    Ok(())
}

fn has_gradients(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| match node {
        Node::Path { fill, stroke, .. } => {
            matches!(fill, Paint::LinearGradient(_) | Paint::RadialGradient(_))
                || matches!(
                    stroke.paint,
                    Paint::LinearGradient(_) | Paint::RadialGradient(_)
                )
        }
        Node::Text { runs, .. } => runs.iter().any(|run| {
            matches!(
                run.fill,
                Paint::LinearGradient(_) | Paint::RadialGradient(_)
            )
        }),
        Node::Group { nodes, .. } => has_gradients(nodes),
        Node::Image { .. } => false,
    })
}

fn has_drawingml_effects(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| match node {
        Node::Path { meta, .. } | Node::Text { meta, .. } | Node::Image { meta, .. } => {
            drawingml_filter_prefix(meta).is_some()
        }
        Node::Group { nodes, meta, .. } => {
            drawingml_filter_prefix(meta).is_some() || has_drawingml_effects(nodes)
        }
    })
}

fn matrix(matrix: Matrix, precision: usize) -> String {
    format!(
        "matrix({})",
        matrix
            .iter()
            .map(|value| number(*value, precision + 3))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn number(value: f64, precision: usize) -> String {
    // SvgOptions is public; bound direct callers as well as the CLI.
    let precision = precision.min(12);
    if !value.is_finite() || value.abs() < 0.5 * 10f64.powi(-(precision as i32)) {
        return "0".into();
    }
    let formatted = format!("{value:.precision$}");
    // With integer precision, trailing zeroes are significant (100 != 1).
    if precision == 0 {
        return formatted;
    }
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn escape_text(value: &str) -> Cow<'_, str> {
    escape_xml(value, false)
}

fn escape_attr(value: &str) -> Cow<'_, str> {
    escape_xml(value, true)
}

fn escape_xml(value: &str, attribute: bool) -> Cow<'_, str> {
    // In particular, embedded image data and path strings usually need no
    // escaping. Borrow them instead of copying multi-megabyte values once
    // for each href/xlink:href attribute.
    let needs_escaping = value.bytes().any(|byte| {
        matches!(byte, b'&' | b'<' | b'>' | 0..=8 | 11 | 12 | 14..=31)
            || (attribute && matches!(byte, b'"' | b'\''))
    });
    if !needs_escaping {
        return Cow::Borrowed(value);
    }
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' if attribute => escaped.push_str("&quot;"),
            '\'' if attribute => escaped.push_str("&apos;"),
            '\t' | '\n' | '\r' => escaped.push(character),
            value if value >= ' ' => escaped.push(value),
            _ => {}
        }
    }
    Cow::Owned(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IDENTITY, Node, Page, SourceMeta, TextRun};

    #[test]
    fn escaping_preserves_xml_rules_and_borrows_plain_values() {
        assert_eq!(escape_text("日本語<&>\"'\0\t"), "日本語&lt;&amp;&gt;\"'\t");
        assert_eq!(
            escape_attr("中文<&>\"'\u{b}\n"),
            "中文&lt;&amp;&gt;&quot;&apos;\n"
        );
        assert!(matches!(
            escape_attr("data:image/png;base64,AAAA"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(escape_text("plain 日本語"), Cow::Borrowed(_)));
    }

    #[test]
    fn integer_precision_preserves_page_dimensions_and_coordinates() {
        let page = Page::new(1, 100.0, 200.0, "test");
        let mut output = Vec::new();
        write_page(
            &page,
            &mut output,
            SvgOptions {
                precision: 0,
                ..SvgOptions::default()
            },
        )
        .unwrap();
        let svg = String::from_utf8(output).unwrap();
        assert!(svg.contains("width=\"100pt\" height=\"200pt\" viewBox=\"0 0 100 200\""));
        assert_eq!(number(-120.0, 0), "-120");
        assert_eq!(number(10.4, 0), "10");
        assert_eq!(number(0.0, 0), "0");
        assert_eq!(number(100.0, usize::MAX), "100");
    }

    #[test]
    fn serializer_escapes_text_and_is_deterministic() {
        let mut page = Page::new(1, 100.0, 50.0, "test");
        page.nodes.push(Node::Text {
            id: "t1".into(),
            x: 4.0,
            y: 12.0,
            runs: vec![TextRun {
                text: "A < B & C".into(),
                ..TextRun::default()
            }],
            anchor: TextAnchor::Start,
            transform: IDENTITY,
            opacity: 1.0,
            stroke: Stroke::default(),
            clip_id: None,
            meta: SourceMeta::default(),
        });
        let mut first = Vec::new();
        let mut second = Vec::new();
        write_page(&page, &mut first, SvgOptions::default()).unwrap();
        write_page(&page, &mut second, SvgOptions::default()).unwrap();
        assert_eq!(first, second);
        let svg = String::from_utf8(first).unwrap();
        assert!(svg.contains("A &lt; B &amp; C"));
    }
}
