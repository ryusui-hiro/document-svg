//! SVG and CSS color parsing: `#RGB`, `#RRGGBB`, `rgb()`, `rgba()`, and CSS named colors.

/// Parsed CSS style declarations relevant for SVG vector shapes and text.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ParsedStyle {
    pub stroke: Option<Option<u32>>,
    pub fill: Option<Option<u32>>,
    pub stroke_width: Option<f64>,
    pub font_size: Option<f64>,
}

/// Parses an inline CSS `style` attribute string (e.g. `stroke: #ff0000; stroke-width: 2px`).
pub fn parse_style(s: &str) -> ParsedStyle {
    let mut res = ParsedStyle::default();
    for item in s.split(';') {
        let mut parts = item.splitn(2, ':');
        if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            match key.as_str() {
                "stroke" => {
                    if val.eq_ignore_ascii_case("none") {
                        res.stroke = Some(None);
                    } else if let Some(c) = parse_color(val) {
                        res.stroke = Some(Some(c));
                    }
                }
                "fill" => {
                    if val.eq_ignore_ascii_case("none") {
                        res.fill = Some(None);
                    } else if let Some(c) = parse_color(val) {
                        res.fill = Some(Some(c));
                    }
                }
                "stroke-width" => {
                    let stripped = val.trim_end_matches("px").trim_end_matches("pt");
                    if let Ok(w) = stripped.parse::<f64>() {
                        res.stroke_width = Some(w);
                    }
                }
                "font-size" => {
                    let stripped = val.trim_end_matches("px").trim_end_matches("pt");
                    if let Ok(sz) = stripped.parse::<f64>() {
                        res.font_size = Some(sz);
                    }
                }
                _ => {}
            }
        }
    }
    res
}

/// Parses a color string into `0x00RRGGBB` integer.
///
/// Supports `#RRGGBB`, `#RGB`, `rgb(...)`, `rgba(...)`, and standard CSS named colors.
pub fn parse_color(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if s.starts_with('#') {
        return parse_color_hex(s);
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("rgb(") || lower.starts_with("rgba(") {
        let open = lower.find('(')?;
        let close = lower.find(')')?;
        if close <= open {
            return None;
        }
        let inner = &lower[open + 1..close];
        let parts: Vec<f64> = inner
            .split([' ', ','])
            .filter_map(|p| p.trim().trim_end_matches('%').parse().ok())
            .collect();
        if parts.len() >= 3 {
            let r = (parts[0].round() as u32).min(255);
            let g = (parts[1].round() as u32).min(255);
            let b = (parts[2].round() as u32).min(255);
            return Some((r << 16) | (g << 8) | b);
        }
    }

    match lower.as_str() {
        "black" => Some(0x000000),
        "white" => Some(0xffffff),
        "red" => Some(0xff0000),
        "green" => Some(0x008000),
        "lime" => Some(0x00ff00),
        "blue" => Some(0x0000ff),
        "yellow" => Some(0xffff00),
        "cyan" | "aqua" => Some(0x00ffff),
        "magenta" | "fuchsia" => Some(0xff00ff),
        "gray" | "grey" => Some(0x808080),
        "silver" => Some(0xc0c0c0),
        "maroon" => Some(0x800000),
        "olive" => Some(0x808000),
        "navy" => Some(0x000080),
        "purple" => Some(0x800080),
        "teal" => Some(0x008080),
        "orange" => Some(0xffa500),
        "brown" => Some(0xa52a2a),
        "gold" => Some(0xffd700),
        "pink" => Some(0xffc0cb),
        _ => None,
    }
}

/// Parses a color hex string (`#RRGGBB` or `#RGB`) into `0x00RRGGBB` integer.
pub fn parse_color_hex(s: &str) -> Option<u32> {
    let s = s.trim();
    if !s.starts_with('#') {
        return None;
    }
    let hex = &s[1..];
    if !hex.is_ascii() {
        return None;
    }
    if hex.len() == 6 {
        u32::from_str_radix(hex, 16).ok()
    } else if hex.len() == 3 {
        let r = u32::from_str_radix(&hex[0..1], 16).ok()?;
        let g = u32::from_str_radix(&hex[1..2], 16).ok()?;
        let b = u32::from_str_radix(&hex[2..3], 16).ok()?;
        Some((r * 17) << 16 | (g * 17) << 8 | (b * 17))
    } else {
        None
    }
}
