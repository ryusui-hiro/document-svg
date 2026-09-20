//! Code generation from SVG: React JSX/TSX, Vue 3 components, and CSS Data URIs.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::error::{Error, Result};

/// Converts SVG bytes to a React JSX or TSX component.
pub fn svg_to_jsx(svg_bytes: &[u8], component_name: &str, is_typescript: bool) -> Result<String> {
    let svg_str = std::str::from_utf8(svg_bytes)
        .map_err(|e| Error::InvalidInput(format!("SVG is not valid UTF-8: {e}")))?;

    let stripped = strip_xml_comments(svg_str);
    let clean_svg = sanitize_for_jsx(&stripped);
    let mut out = String::new();

    if is_typescript {
        out.push_str("import React from 'react';\n\n");
        out.push_str(&format!("export const {component_name}: React.FC<React.SVGProps<SVGSVGElement>> = (props) => (\n"));
    } else {
        out.push_str("import React from 'react';\n\n");
        out.push_str(&format!("export const {component_name} = (props) => (\n"));
    }

    // Filter out XML declaration and DOCTYPE which are invalid in JSX
    let filtered_lines: Vec<&str> = clean_svg
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("<?xml") && !trimmed.starts_with("<!DOCTYPE")
        })
        .collect();

    let mut svg_started = false;
    for line in filtered_lines {
        let trimmed = line.trim_start();
        if !svg_started && trimmed.starts_with("<svg") {
            svg_started = true;
            let replaced = line.replace("<svg", "<svg {...props}");
            out.push_str(&format!("  {replaced}\n"));
        } else {
            out.push_str(&format!("  {line}\n"));
        }
    }

    out.push_str(");\n\nexport default ");
    out.push_str(component_name);
    out.push_str(";\n");

    Ok(out)
}

/// Converts SVG bytes to a Vue 3 SFC component.
pub fn svg_to_vue(svg_bytes: &[u8]) -> Result<String> {
    let svg_str = std::str::from_utf8(svg_bytes)
        .map_err(|e| Error::InvalidInput(format!("SVG is not valid UTF-8: {e}")))?;

    let stripped = strip_xml_comments(svg_str);
    let mut out = String::new();
    out.push_str("<template>\n");
    for line in stripped.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<?xml") || trimmed.starts_with("<!DOCTYPE") {
            continue;
        }
        out.push_str(&format!("  {line}\n"));
    }
    out.push_str("</template>\n\n<script setup>\n// Vue 3 SVG Icon component\n</script>\n");
    Ok(out)
}

/// Converts SVG bytes to a base64 Data URI string.
pub fn svg_to_data_uri(svg_bytes: &[u8]) -> Result<String> {
    let encoded = BASE64.encode(svg_bytes);
    Ok(format!("data:image/svg+xml;base64,{encoded}"))
}

/// Converts SVG bytes to a Svelte 3/4/5 icon component.
pub fn svg_to_svelte(svg_bytes: &[u8]) -> Result<String> {
    let svg_str = std::str::from_utf8(svg_bytes)
        .map_err(|e| Error::InvalidInput(format!("SVG is not valid UTF-8: {e}")))?;

    let stripped = strip_xml_comments(svg_str);
    let mut out = String::from("<script>\n  // Svelte SVG Icon component\n</script>\n\n");
    let filtered_lines: Vec<&str> = stripped
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("<?xml") && !trimmed.starts_with("<!DOCTYPE")
        })
        .collect();

    let mut svg_started = false;
    for line in filtered_lines {
        let trimmed = line.trim_start();
        if !svg_started && trimmed.starts_with("<svg") {
            svg_started = true;
            let replaced = line.replace("<svg", "<svg {...$$restProps}");
            out.push_str(&format!("{replaced}\n"));
        } else {
            out.push_str(&format!("{line}\n"));
        }
    }
    Ok(out)
}

/// Wraps SVG bytes in a standalone, responsive HTML5 viewer document.
pub fn svg_to_html(svg_bytes: &[u8], title: &str) -> Result<String> {
    let svg_str = std::str::from_utf8(svg_bytes)
        .map_err(|e| Error::InvalidInput(format!("SVG is not valid UTF-8: {e}")))?;

    let stripped = strip_xml_comments(svg_str);
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("  <meta charset=\"utf-8\">\n");
    out.push_str("  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!("  <title>{}</title>\n", html_escape(title)));
    out.push_str("  <style>\n");
    out.push_str("    * { box-sizing: border-box; margin: 0; padding: 0; }\n");
    out.push_str("    body {\n");
    out.push_str("      display: flex;\n");
    out.push_str("      justify-content: center;\n");
    out.push_str("      align-items: center;\n");
    out.push_str("      min-height: 100vh;\n");
    out.push_str("      background-color: #f8fafc;\n");
    out.push_str("      padding: 1.5rem;\n");
    out.push_str("    }\n");
    out.push_str("    @media (prefers-color-scheme: dark) {\n");
    out.push_str("      body { background-color: #0f172a; }\n");
    out.push_str("    }\n");
    out.push_str("    .svg-container {\n");
    out.push_str("      max-width: 100%;\n");
    out.push_str("      max-height: 100vh;\n");
    out.push_str("      display: flex;\n");
    out.push_str("      justify-content: center;\n");
    out.push_str("      align-items: center;\n");
    out.push_str("    }\n");
    out.push_str("    svg {\n");
    out.push_str("      max-width: 100%;\n");
    out.push_str("      height: auto;\n");
    out.push_str(
        "      box-shadow: 0 10px 15px -3px rgb(0 0 0 / 0.1), 0 4px 6px -4px rgb(0 0 0 / 0.1);\n",
    );
    out.push_str("      border-radius: 4px;\n");
    out.push_str("    }\n");
    out.push_str("  </style>\n</head>\n<body>\n  <div class=\"svg-container\">\n");

    for line in stripped.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("<?xml") || trimmed.starts_with("<!DOCTYPE") {
            continue;
        }
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }

    out.push_str("  </div>\n</body>\n</html>\n");
    Ok(out)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Extracts raw SVG path definition data (`d="..."`) from SVG bytes.
pub fn svg_to_path_data(svg_bytes: &[u8]) -> Result<String> {
    let mut reader = quick_xml::Reader::from_reader(svg_bytes);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut paths = Vec::new();

    while let Ok(event) = reader.read_event_into(&mut buffer) {
        match event {
            quick_xml::events::Event::Start(e) | quick_xml::events::Event::Empty(e) => {
                let name = e.name();
                if name.as_ref() == b"path" {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"d" {
                            let val = String::from_utf8_lossy(&attr.value).into_owned();
                            if !val.trim().is_empty() {
                                paths.push(val);
                            }
                        }
                    }
                }
            }
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    Ok(paths.join("\n"))
}

fn strip_xml_comments(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut remaining = s;
    while let Some(start) = remaining.find("<!--") {
        result.push_str(&remaining[..start]);
        if let Some(end) = remaining[start + 4..].find("-->") {
            remaining = &remaining[start + 4 + end + 3..];
        } else {
            remaining = "";
            break;
        }
    }
    result.push_str(remaining);
    result
}

fn sanitize_for_jsx(svg: &str) -> String {
    // Transform common kebab-case SVG attributes to React camelCase
    let mut s = svg.to_string();
    let mappings = [
        ("class=", "className="),
        ("clip-path=", "clipPath="),
        ("clip-rule=", "clipRule="),
        ("fill-opacity=", "fillOpacity="),
        ("fill-rule=", "fillRule="),
        ("stroke-dasharray=", "strokeDasharray="),
        ("stroke-dashoffset=", "strokeDashoffset="),
        ("stroke-linecap=", "strokeLinecap="),
        ("stroke-linejoin=", "strokeLinejoin="),
        ("stroke-miterlimit=", "strokeMiterlimit="),
        ("stroke-opacity=", "strokeOpacity="),
        ("stroke-width=", "strokeWidth="),
        ("font-family=", "fontFamily="),
        ("font-size=", "fontSize="),
        ("font-weight=", "fontWeight="),
        ("font-style=", "fontStyle="),
        ("text-anchor=", "textAnchor="),
        ("dominant-baseline=", "dominantBaseline="),
        ("stop-color=", "stopColor="),
        ("stop-opacity=", "stopOpacity="),
        ("xmlns:xlink=", "xmlnsXlink="),
        ("xlink:href=", "xlinkHref="),
        ("xml:space=", "xmlSpace="),
        ("color-interpolation-filters=", "colorInterpolationFilters="),
        ("flood-color=", "floodColor="),
        ("flood-opacity=", "floodOpacity="),
        ("lighting-color=", "lightingColor="),
        ("pointer-events=", "pointerEvents="),
        ("tabindex=", "tabIndex="),
    ];

    for (kebab, camel) in mappings {
        s = s.replace(kebab, camel);
    }
    transform_inline_styles_for_jsx(&s)
}

fn kebab_to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut capitalize_next = false;
    for ch in s.chars() {
        if ch == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            out.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn convert_inline_style_to_jsx(style_str: &str) -> String {
    let mut entries = Vec::new();
    for item in style_str.split(';') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if let Some((prop, val)) = item.split_once(':') {
            let prop_trimmed = prop.trim();
            let val_trimmed = val.trim();
            let camel_prop = kebab_to_camel(prop_trimmed);
            let escaped_val = val_trimmed.replace('\'', "\\'");
            entries.push(format!("{camel_prop}: '{escaped_val}'"));
        }
    }
    if entries.is_empty() {
        "style={{}}".to_string()
    } else {
        format!("style={{{{ {} }}}}", entries.join(", "))
    }
}

fn transform_inline_styles_for_jsx(svg: &str) -> String {
    let mut out = String::with_capacity(svg.len());
    let mut remaining = svg;

    while let Some(pos) = remaining.find("style=") {
        out.push_str(&remaining[..pos]);
        let after_style = &remaining[pos + 6..];
        if let Some(quote) = after_style.chars().next()
            && (quote == '"' || quote == '\'')
        {
            let after_quote = &after_style[1..];
            if let Some(end_quote) = after_quote.find(quote) {
                let style_val = &after_quote[..end_quote];
                let jsx_style = convert_inline_style_to_jsx(style_val);
                out.push_str(&jsx_style);
                remaining = &after_quote[end_quote + 1..];
                continue;
            }
        }
        out.push_str("style=");
        remaining = after_style;
    }
    out.push_str(remaining);
    out
}
