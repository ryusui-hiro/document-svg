//! LaTeX math formula parsing, box model layout, SVG rendering, and reverse extraction.

use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

#[derive(Clone, Debug)]
pub enum MathNode {
    Text(String),
    Superscript(Box<MathNode>),
    Subscript(Box<MathNode>),
    Fraction {
        numerator: Box<MathNode>,
        denominator: Box<MathNode>,
    },
    Sqrt(Box<MathNode>),
    Matrix {
        rows: Vec<Vec<MathNode>>,
        delimiters: Option<(char, char)>,
    },
    Row(Vec<MathNode>),
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "LaTeX math input")?;
    let source = String::from_utf8(bytes)
        .map_err(|e| Error::InvalidInput(format!("math file is not valid UTF-8: {e}")))?;

    if crate::document::latex::looks_like_document(&source) {
        return crate::document::latex::convert_source(path, &source, options, sink);
    }

    let mut clean_math = source
        .trim()
        .trim_start_matches('$')
        .trim_end_matches('$')
        .trim_start_matches("\\[")
        .trim_end_matches("\\]")
        .trim();

    if clean_math.starts_with("\\begin{equation}") && clean_math.ends_with("\\end{equation}") {
        clean_math = clean_math
            .strip_prefix("\\begin{equation}")
            .unwrap()
            .strip_suffix("\\end{equation}")
            .unwrap()
            .trim();
    } else if clean_math.starts_with("\\begin{equation*}")
        && clean_math.ends_with("\\end{equation*}")
    {
        clean_math = clean_math
            .strip_prefix("\\begin{equation*}")
            .unwrap()
            .strip_suffix("\\end{equation*}")
            .unwrap()
            .trim();
    }

    let ast = parse_math_expression(clean_math, 0)?;
    let page = layout_and_render_math(&ast, clean_math, options)?;
    sink.consume(page)?;
    Ok(Vec::new())
}

pub fn parse_math_expression(input: &str, depth: usize) -> Result<MathNode> {
    if depth > 256 {
        return Err(Error::LimitExceeded(
            "math recursion depth limit exceeded".into(),
        ));
    }
    let mut nodes = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '\\' {
            chars.next();
            if let Some(&next_ch) = chars.peek()
                && !next_ch.is_alphabetic()
            {
                chars.next();
                match next_ch {
                    '{' => nodes.push(MathNode::Text("{".to_string())),
                    '}' => nodes.push(MathNode::Text("}".to_string())),
                    '\\' => nodes.push(MathNode::Text(" ".to_string())),
                    ',' | ';' | ':' | ' ' => nodes.push(MathNode::Text(" ".to_string())),
                    '!' => {} // negative space
                    '%' => nodes.push(MathNode::Text("%".to_string())),
                    '&' => nodes.push(MathNode::Text("&".to_string())),
                    '_' => nodes.push(MathNode::Text("_".to_string())),
                    '$' => nodes.push(MathNode::Text("$".to_string())),
                    _ => nodes.push(MathNode::Text(next_ch.to_string())),
                }
                continue;
            }
            let mut command = String::new();
            while let Some(&cmd_ch) = chars.peek() {
                if cmd_ch.is_alphabetic() {
                    command.push(cmd_ch);
                    chars.next();
                } else {
                    break;
                }
            }
            match command.as_str() {
                "frac" => {
                    let num = parse_group(&mut chars);
                    let den = parse_group(&mut chars);
                    nodes.push(MathNode::Fraction {
                        numerator: Box::new(parse_math_expression(&num, depth + 1)?),
                        denominator: Box::new(parse_math_expression(&den, depth + 1)?),
                    });
                }
                "sqrt" => {
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if chars.peek() == Some(&'[') {
                        chars.next();
                        for c in chars.by_ref() {
                            if c == ']' {
                                break;
                            }
                        }
                    }
                    let inner = parse_group(&mut chars);
                    nodes.push(MathNode::Sqrt(Box::new(parse_math_expression(
                        &inner,
                        depth + 1,
                    )?)));
                }
                "vec" => {
                    let inner = parse_group(&mut chars);
                    nodes.push(MathNode::Text(format!("{inner}\u{20d7}")));
                }
                "hat" => {
                    let inner = parse_group(&mut chars);
                    nodes.push(MathNode::Text(format!("{inner}\u{0302}")));
                }
                "bar" | "overline" => {
                    let inner = parse_group(&mut chars);
                    nodes.push(MathNode::Text(format!("{inner}\u{0304}")));
                }
                "tilde" => {
                    let inner = parse_group(&mut chars);
                    nodes.push(MathNode::Text(format!("{inner}\u{0303}")));
                }
                "dot" => {
                    let inner = parse_group(&mut chars);
                    nodes.push(MathNode::Text(format!("{inner}\u{0307}")));
                }
                "ddot" => {
                    let inner = parse_group(&mut chars);
                    nodes.push(MathNode::Text(format!("{inner}\u{0308}")));
                }
                "mathbb" => {
                    let inner = parse_group(&mut chars);
                    let bb = match inner.as_str() {
                        "R" => "ℝ",
                        "C" => "ℂ",
                        "N" => "ℕ",
                        "Z" => "ℤ",
                        "Q" => "ℚ",
                        "P" => "ℙ",
                        "H" => "ℍ",
                        _ => &inner,
                    };
                    nodes.push(MathNode::Text(bb.to_string()));
                }
                "text" | "mathrm" | "mathbf" | "mathit" | "operatorname" | "bm" => {
                    let inner = parse_group(&mut chars);
                    nodes.push(MathNode::Text(inner));
                }
                "mathcal" | "mathscr" => {
                    let inner = parse_group(&mut chars);
                    let mapped: String = inner
                        .chars()
                        .map(|ch| match ch {
                            'A' => '𝒜',
                            'B' => 'ℬ',
                            'C' => '𝒞',
                            'D' => '𝒟',
                            'E' => 'ℰ',
                            'F' => 'ℱ',
                            'G' => '𝒢',
                            'H' => 'ℋ',
                            'I' => 'ℐ',
                            'J' => '𝒥',
                            'K' => '𝒦',
                            'L' => 'ℒ',
                            'M' => 'ℳ',
                            'N' => '𝒩',
                            'O' => '𝒪',
                            'P' => '𝒫',
                            'Q' => '𝒬',
                            'R' => 'ℛ',
                            'S' => '𝒮',
                            'T' => '𝒯',
                            'U' => '𝒰',
                            'V' => '𝒱',
                            'W' => '𝒲',
                            'X' => '𝒳',
                            'Y' => '𝒴',
                            'Z' => '𝒵',
                            'a' => '𝒶',
                            'b' => '𝒷',
                            'c' => '𝒸',
                            'd' => '𝒹',
                            'e' => 'ℯ',
                            'f' => '𝒻',
                            'g' => 'ℊ',
                            'h' => '𝒽',
                            'i' => '𝒾',
                            'j' => '𝒿',
                            'k' => '𝓀',
                            'l' => '𝓁',
                            'm' => '𝓂',
                            'n' => '𝓃',
                            'o' => 'ℴ',
                            'p' => '𝓅',
                            'q' => '𝓆',
                            'r' => '𝓇',
                            's' => '𝓈',
                            't' => '𝓉',
                            'u' => '𝓊',
                            'v' => '𝓋',
                            'w' => '𝓌',
                            'x' => '𝓍',
                            'y' => '𝓎',
                            'z' => '𝓏',
                            other => other,
                        })
                        .collect();
                    nodes.push(MathNode::Text(mapped));
                }
                "mathsf" | "mathtt" | "mathfrak" | "frak" => {
                    let inner = parse_group(&mut chars);
                    nodes.push(MathNode::Text(inner));
                }
                "left" | "right" => {
                    while let Some(&nc) = chars.peek() {
                        if nc.is_whitespace() {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if chars.peek() == Some(&'.') {
                        chars.next(); // consume invisible delimiter
                    }
                }
                "sum" => nodes.push(MathNode::Text("∑".to_string())),
                "int" => nodes.push(MathNode::Text("∫".to_string())),
                "times" => nodes.push(MathNode::Text("×".to_string())),
                "div" => nodes.push(MathNode::Text("÷".to_string())),
                "pm" => nodes.push(MathNode::Text("±".to_string())),
                "mp" => nodes.push(MathNode::Text("∓".to_string())),
                "alpha" => nodes.push(MathNode::Text("α".to_string())),
                "beta" => nodes.push(MathNode::Text("β".to_string())),
                "gamma" => nodes.push(MathNode::Text("γ".to_string())),
                "Gamma" => nodes.push(MathNode::Text("Γ".to_string())),
                "delta" => nodes.push(MathNode::Text("δ".to_string())),
                "Delta" => nodes.push(MathNode::Text("Δ".to_string())),
                "epsilon" | "varepsilon" => nodes.push(MathNode::Text("ε".to_string())),
                "zeta" => nodes.push(MathNode::Text("ζ".to_string())),
                "eta" => nodes.push(MathNode::Text("η".to_string())),
                "theta" => nodes.push(MathNode::Text("θ".to_string())),
                "Theta" => nodes.push(MathNode::Text("Θ".to_string())),
                "iota" => nodes.push(MathNode::Text("ι".to_string())),
                "kappa" => nodes.push(MathNode::Text("κ".to_string())),
                "lambda" => nodes.push(MathNode::Text("λ".to_string())),
                "Lambda" => nodes.push(MathNode::Text("Λ".to_string())),
                "mu" => nodes.push(MathNode::Text("μ".to_string())),
                "nu" => nodes.push(MathNode::Text("ν".to_string())),
                "xi" => nodes.push(MathNode::Text("ξ".to_string())),
                "Xi" => nodes.push(MathNode::Text("Ξ".to_string())),
                "pi" => nodes.push(MathNode::Text("π".to_string())),
                "Pi" => nodes.push(MathNode::Text("Π".to_string())),
                "rho" | "varrho" => nodes.push(MathNode::Text("ρ".to_string())),
                "sigma" => nodes.push(MathNode::Text("σ".to_string())),
                "Sigma" => nodes.push(MathNode::Text("Σ".to_string())),
                "tau" => nodes.push(MathNode::Text("τ".to_string())),
                "upsilon" => nodes.push(MathNode::Text("υ".to_string())),
                "Upsilon" => nodes.push(MathNode::Text("Υ".to_string())),
                "phi" | "varphi" => nodes.push(MathNode::Text("φ".to_string())),
                "Phi" => nodes.push(MathNode::Text("Φ".to_string())),
                "chi" => nodes.push(MathNode::Text("χ".to_string())),
                "psi" => nodes.push(MathNode::Text("ψ".to_string())),
                "Psi" => nodes.push(MathNode::Text("Ψ".to_string())),
                "omega" => nodes.push(MathNode::Text("ω".to_string())),
                "Omega" => nodes.push(MathNode::Text("Ω".to_string())),
                "partial" => nodes.push(MathNode::Text("∂".to_string())),
                "nabla" => nodes.push(MathNode::Text("∇".to_string())),
                "approx" => nodes.push(MathNode::Text("≈".to_string())),
                "neq" | "ne" => nodes.push(MathNode::Text("≠".to_string())),
                "leq" | "le" => nodes.push(MathNode::Text("≤".to_string())),
                "geq" | "ge" => nodes.push(MathNode::Text("≥".to_string())),
                "cdot" => nodes.push(MathNode::Text("·".to_string())),
                "infty" => nodes.push(MathNode::Text("∞".to_string())),
                "in" => nodes.push(MathNode::Text("∈".to_string())),
                "notin" => nodes.push(MathNode::Text("∉".to_string())),
                "subset" => nodes.push(MathNode::Text("⊂".to_string())),
                "subseteq" => nodes.push(MathNode::Text("⊆".to_string())),
                "supset" => nodes.push(MathNode::Text("⊃".to_string())),
                "supseteq" => nodes.push(MathNode::Text("⊇".to_string())),
                "equiv" => nodes.push(MathNode::Text("≡".to_string())),
                "sim" => nodes.push(MathNode::Text("∼".to_string())),
                "propto" => nodes.push(MathNode::Text("∝".to_string())),
                "land" | "wedge" => nodes.push(MathNode::Text("∧".to_string())),
                "lor" | "vee" => nodes.push(MathNode::Text("∨".to_string())),
                "oplus" => nodes.push(MathNode::Text("⊕".to_string())),
                "otimes" => nodes.push(MathNode::Text("⊗".to_string())),
                "odot" => nodes.push(MathNode::Text("⊙".to_string())),
                "circ" => nodes.push(MathNode::Text("∘".to_string())),
                "bullet" => nodes.push(MathNode::Text("•".to_string())),
                "dots" | "ldots" | "cdots" => nodes.push(MathNode::Text("…".to_string())),
                "degree" => nodes.push(MathNode::Text("°".to_string())),
                "cap" => nodes.push(MathNode::Text("∩".to_string())),
                "cup" => nodes.push(MathNode::Text("∪".to_string())),
                "forall" => nodes.push(MathNode::Text("∀".to_string())),
                "exists" => nodes.push(MathNode::Text("∃".to_string())),
                "neg" => nodes.push(MathNode::Text("¬".to_string())),
                "to" | "rightarrow" => nodes.push(MathNode::Text("→".to_string())),
                "leftarrow" | "gets" => nodes.push(MathNode::Text("←".to_string())),
                "leftrightarrow" => nodes.push(MathNode::Text("↔".to_string())),
                "uparrow" => nodes.push(MathNode::Text("↑".to_string())),
                "downarrow" => nodes.push(MathNode::Text("↓".to_string())),
                "parallel" => nodes.push(MathNode::Text("∥".to_string())),
                "perp" => nodes.push(MathNode::Text("⊥".to_string())),
                "quad" => nodes.push(MathNode::Text("  ".to_string())),
                "qquad" => nodes.push(MathNode::Text("    ".to_string())),
                "Rightarrow" | "implies" => nodes.push(MathNode::Text("⇒".to_string())),
                "Leftarrow" | "impliedby" => nodes.push(MathNode::Text("⇐".to_string())),
                "iff" | "Leftrightarrow" => nodes.push(MathNode::Text("⇔".to_string())),
                "emptyset" | "varnothing" => nodes.push(MathNode::Text("∅".to_string())),
                "nexists" => nodes.push(MathNode::Text("∄".to_string())),
                "ni" | "owns" => nodes.push(MathNode::Text("∋".to_string())),
                "top" => nodes.push(MathNode::Text("⊤".to_string())),
                "bot" => nodes.push(MathNode::Text("⊥".to_string())),
                "ominus" => nodes.push(MathNode::Text("⊖".to_string())),
                "oslash" => nodes.push(MathNode::Text("⊘".to_string())),
                "setminus" => nodes.push(MathNode::Text("∖".to_string())),
                "mid" => nodes.push(MathNode::Text("∣".to_string())),
                "nmid" => nodes.push(MathNode::Text("∤".to_string())),
                "simeq" => nodes.push(MathNode::Text("≃".to_string())),
                "cong" => nodes.push(MathNode::Text("≅".to_string())),
                "asymp" => nodes.push(MathNode::Text("≍".to_string())),
                "doteq" => nodes.push(MathNode::Text("≐".to_string())),
                "aleph" => nodes.push(MathNode::Text("ℵ".to_string())),
                "wp" => nodes.push(MathNode::Text("℘".to_string())),
                "prod" => nodes.push(MathNode::Text("∏".to_string())),
                "coprod" => nodes.push(MathNode::Text("∐".to_string())),
                "oint" => nodes.push(MathNode::Text("∮".to_string())),
                "iint" => nodes.push(MathNode::Text("∬".to_string())),
                "iiint" => nodes.push(MathNode::Text("∭".to_string())),
                "hbar" => nodes.push(MathNode::Text("ħ".to_string())),
                "ell" => nodes.push(MathNode::Text("ℓ".to_string())),
                "Re" => nodes.push(MathNode::Text("ℜ".to_string())),
                "Im" => nodes.push(MathNode::Text("ℑ".to_string())),
                "prime" => nodes.push(MathNode::Text("′".to_string())),
                "dagger" => nodes.push(MathNode::Text("†".to_string())),
                "ddagger" => nodes.push(MathNode::Text("‡".to_string())),
                "angle" => nodes.push(MathNode::Text("∠".to_string())),
                "longrightarrow" => nodes.push(MathNode::Text("⟶".to_string())),
                "longleftarrow" => nodes.push(MathNode::Text("⟵".to_string())),
                "Longrightarrow" => nodes.push(MathNode::Text("⟹".to_string())),
                "Longleftrightarrow" => nodes.push(MathNode::Text("⟺".to_string())),
                "sin" | "cos" | "tan" | "log" | "ln" | "exp" | "lim" | "max" | "min" | "det"
                | "arcsin" | "arccos" | "arctan" | "sinh" | "cosh" | "tanh" | "dim" | "ker"
                | "gcd" | "deg" | "sup" | "inf" | "sec" | "csc" | "cot" | "hom" | "arg" => {
                    nodes.push(MathNode::Text(command));
                }
                "begin" => {
                    let env_name = parse_group(&mut chars);
                    if env_name == "array" {
                        let _ = parse_group(&mut chars);
                    }
                    let body = parse_environment_body(&mut chars, &env_name);
                    let delimiters = match env_name.as_str() {
                        "pmatrix" => Some(('(', ')')),
                        "bmatrix" => Some(('[', ']')),
                        "Bmatrix" => Some(('{', '}')),
                        "vmatrix" => Some(('|', '|')),
                        "Vmatrix" => Some(('‖', '‖')),
                        "cases" => Some(('{', '.')),
                        "aligned" | "align" | "align*" | "split" | "gather" | "gather*" => {
                            Some(('.', '.'))
                        }
                        _ => None,
                    };
                    let matrix_rows = parse_matrix_body(&body, depth + 1)?;
                    nodes.push(MathNode::Matrix {
                        rows: matrix_rows,
                        delimiters,
                    });
                }
                "end" => {
                    let _ = parse_group(&mut chars);
                }
                "binom" => {
                    let top = parse_group(&mut chars);
                    let bot = parse_group(&mut chars);
                    let r1 = vec![parse_math_expression(&top, depth + 1)?];
                    let r2 = vec![parse_math_expression(&bot, depth + 1)?];
                    nodes.push(MathNode::Matrix {
                        rows: vec![r1, r2],
                        delimiters: Some(('(', ')')),
                    });
                }
                "limits" | "nolimits" | "displaystyle" | "textstyle" | "notag" | "hline"
                | "nonumber" => {}
                "label" => {
                    let _ = parse_group(&mut chars);
                }
                "vdots" => nodes.push(MathNode::Text("⋮".to_string())),
                "ddots" => nodes.push(MathNode::Text("⋱".to_string())),
                "hdots" => nodes.push(MathNode::Text("…".to_string())),
                _ => nodes.push(MathNode::Text(command)),
            }
        } else if c == '{' {
            let inner = parse_group(&mut chars);
            nodes.push(parse_math_expression(&inner, depth + 1)?);
        } else if c == '}' {
            chars.next();
        } else if c == '^' {
            chars.next();
            let sup_text = parse_single_or_group(&mut chars);
            nodes.push(MathNode::Superscript(Box::new(parse_math_expression(
                &sup_text,
                depth + 1,
            )?)));
        } else if c == '_' {
            chars.next();
            let sub_text = parse_single_or_group(&mut chars);
            nodes.push(MathNode::Subscript(Box::new(parse_math_expression(
                &sub_text,
                depth + 1,
            )?)));
        } else {
            nodes.push(MathNode::Text(c.to_string()));
            chars.next();
        }
    }

    if nodes.len() == 1 {
        Ok(nodes.remove(0))
    } else {
        Ok(MathNode::Row(nodes))
    }
}

fn parse_group(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
    if chars.peek() == Some(&'{') {
        chars.next();
        let mut depth = 1;
        let mut content = String::new();
        for c in chars.by_ref() {
            if c == '{' {
                depth += 1;
                content.push(c);
            } else if c == '}' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                content.push(c);
            } else {
                content.push(c);
            }
        }
        content
    } else if let Some(c) = chars.next() {
        c.to_string()
    } else {
        String::new()
    }
}

fn parse_single_or_group(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
    if chars.peek() == Some(&'{') {
        parse_group(chars)
    } else if chars.peek() == Some(&'\\') {
        chars.next();
        let mut cmd = String::from('\\');
        while let Some(&nc) = chars.peek() {
            if nc.is_alphabetic() {
                cmd.push(nc);
                chars.next();
            } else {
                break;
            }
        }
        cmd
    } else if let Some(c) = chars.next() {
        c.to_string()
    } else {
        String::new()
    }
}

fn parse_environment_body(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    env_name: &str,
) -> String {
    let mut body = String::new();
    let mut depth = 1;

    while let Some(c) = chars.next() {
        if c == '\\' {
            let mut cmd = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_alphabetic() {
                    cmd.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            if cmd == "begin" {
                let name = parse_group(chars);
                if name == env_name {
                    depth += 1;
                }
                body.push_str(&format!("\\begin{{{name}}}"));
            } else if cmd == "end" {
                let name = parse_group(chars);
                if name == env_name {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                body.push_str(&format!("\\end{{{name}}}"));
            } else {
                body.push('\\');
                body.push_str(&cmd);
            }
        } else {
            body.push(c);
        }
    }
    body
}

fn parse_matrix_body(body: &str, depth: usize) -> Result<Vec<Vec<MathNode>>> {
    let mut rows = Vec::new();
    let mut current_row_str = String::new();
    let mut chars = body.chars().peekable();

    let mut row_strings = Vec::new();
    let mut brace_depth: usize = 0;
    let mut env_depth: usize = 0;

    while let Some(c) = chars.next() {
        if c == '{' {
            brace_depth += 1;
            current_row_str.push(c);
        } else if c == '}' {
            brace_depth = brace_depth.saturating_sub(1);
            current_row_str.push(c);
        } else if c == '\\' {
            if brace_depth == 0 && env_depth == 0 && chars.peek() == Some(&'\\') {
                chars.next(); // consume second '\'
                row_strings.push(std::mem::take(&mut current_row_str));
                continue;
            }
            current_row_str.push(c);
            let mut cmd = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_alphabetic() || nc == '*' {
                    cmd.push(nc);
                    current_row_str.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            if cmd == "begin" {
                env_depth += 1;
            } else if cmd == "end" {
                env_depth = env_depth.saturating_sub(1);
            }
        } else {
            current_row_str.push(c);
        }
    }
    if !current_row_str.trim().is_empty() || row_strings.is_empty() {
        row_strings.push(current_row_str);
    }

    for r_str in row_strings {
        let trimmed_r = r_str.trim();
        if trimmed_r.is_empty() {
            continue;
        }
        let mut cells = Vec::new();
        let mut cell_str = String::new();
        let mut b_depth: usize = 0;
        let mut e_depth: usize = 0;
        let mut chars_r = trimmed_r.chars().peekable();
        while let Some(c) = chars_r.next() {
            if c == '{' {
                b_depth += 1;
                cell_str.push(c);
            } else if c == '}' {
                b_depth = b_depth.saturating_sub(1);
                cell_str.push(c);
            } else if c == '\\' {
                cell_str.push(c);
                let mut cmd = String::new();
                while let Some(&nc) = chars_r.peek() {
                    if nc.is_alphabetic() || nc == '*' {
                        cmd.push(nc);
                        cell_str.push(nc);
                        chars_r.next();
                    } else {
                        break;
                    }
                }
                if cmd == "begin" {
                    e_depth += 1;
                } else if cmd == "end" {
                    e_depth = e_depth.saturating_sub(1);
                }
            } else if c == '&' && b_depth == 0 && e_depth == 0 {
                let parsed = parse_math_expression(cell_str.trim(), depth)?;
                cells.push(parsed);
                cell_str.clear();
            } else {
                cell_str.push(c);
            }
        }
        let parsed = parse_math_expression(cell_str.trim(), depth)?;
        cells.push(parsed);
        rows.push(cells);
    }

    Ok(rows)
}

struct LayoutBox {
    width: f64,
    height_above: f64,
    height_below: f64,
}

pub fn layout_and_render_math(
    ast: &MathNode,
    raw_tex: &str,
    _options: &ConvertOptions,
) -> Result<Page> {
    let mut page_nodes = Vec::new();
    let base_font_size = 22.0;

    let base_y = 60.0;
    let base_x = 40.0;

    let box_dims = render_node_recursive(ast, base_x, base_y, base_font_size, &mut page_nodes);

    let page_width = (base_x + box_dims.width + 40.0).max(160.0);
    let page_height = (box_dims.height_above + box_dims.height_below + 80.0).max(100.0);

    let mut page = Page::new(1, page_width, page_height, "latex_math");
    page.embedded_source = Some(raw_tex.to_string());
    page.nodes = page_nodes;

    Ok(page)
}

fn render_node_recursive(
    node: &MathNode,
    x: f64,
    y: f64,
    font_size: f64,
    out_nodes: &mut Vec<Node>,
) -> LayoutBox {
    match node {
        MathNode::Text(txt) => {
            let width = txt.chars().count() as f64 * (font_size * 0.60);
            let is_fn_name = matches!(
                txt.as_str(),
                "sin"
                    | "cos"
                    | "tan"
                    | "log"
                    | "ln"
                    | "exp"
                    | "lim"
                    | "max"
                    | "min"
                    | "det"
                    | "arcsin"
                    | "arccos"
                    | "arctan"
                    | "sinh"
                    | "cosh"
                    | "tanh"
                    | "dim"
                    | "ker"
                    | "gcd"
                    | "deg"
            );
            let is_var = !is_fn_name && txt.chars().all(|c| c.is_ascii_alphabetic());
            let font_family = if is_var {
                "Cambria Math, 'Times New Roman', serif"
            } else {
                "Helvetica, Arial, sans-serif"
            };
            out_nodes.push(Node::Text {
                id: String::new(),
                x,
                y,
                runs: vec![TextRun {
                    text: txt.clone(),
                    font_size,
                    font_family: font_family.to_string(),
                    italic: is_var,
                    fill: Paint::solid("#0f172a"),
                    ..Default::default()
                }],
                anchor: TextAnchor::Start,
                transform: IDENTITY,
                opacity: 1.0,
                stroke: Stroke::default(),
                clip_id: None,
                meta: SourceMeta::default(),
            });
            LayoutBox {
                width,
                height_above: font_size * 0.8,
                height_below: font_size * 0.2,
            }
        }
        MathNode::Fraction {
            numerator,
            denominator,
        } => {
            let num_fs = font_size * 0.85;
            let den_fs = font_size * 0.85;

            let num_box = measure_box(numerator, num_fs);
            let den_box = measure_box(denominator, den_fs);

            let frac_width = num_box.width.max(den_box.width) + 12.0;
            let bar_y = y - font_size * 0.28;

            let num_x = x + (frac_width - num_box.width) / 2.0;
            let num_y = bar_y - num_box.height_below - 4.0;
            render_node_recursive(numerator, num_x, num_y, num_fs, out_nodes);

            let den_x = x + (frac_width - den_box.width) / 2.0;
            let den_y = bar_y + den_box.height_above + 4.0;
            render_node_recursive(denominator, den_x, den_y, den_fs, out_nodes);

            // Fraction bar
            let bar_d = format!(
                "M {:.2},{:.2} L {:.2},{:.2}",
                x + 2.0,
                bar_y,
                x + frac_width - 2.0,
                bar_y
            );
            out_nodes.push(Node::Path {
                id: String::new(),
                d: bar_d,
                fill_rule: String::new(),
                fill: Paint::None,
                stroke: Stroke {
                    paint: Paint::solid("#0f172a"),
                    width: 1.5,
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    ..Default::default()
                },
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });

            LayoutBox {
                width: frac_width,
                height_above: (bar_y - (num_y - num_box.height_above)).abs() + 6.0,
                height_below: (den_y + den_box.height_below - bar_y).abs() + 6.0,
            }
        }
        MathNode::Superscript(inner) => {
            let sup_fs = font_size * 0.70;
            let sup_y = y - font_size * 0.45;
            let b = render_node_recursive(inner, x, sup_y, sup_fs, out_nodes);
            LayoutBox {
                width: b.width,
                height_above: b.height_above + font_size * 0.45,
                height_below: 0.0,
            }
        }
        MathNode::Subscript(inner) => {
            let sub_fs = font_size * 0.70;
            let sub_y = y + font_size * 0.30;
            let b = render_node_recursive(inner, x, sub_y, sub_fs, out_nodes);
            LayoutBox {
                width: b.width,
                height_above: 0.0,
                height_below: b.height_below + font_size * 0.30,
            }
        }
        MathNode::Sqrt(inner) => {
            let inner_box = measure_box(inner, font_size);
            let sqrt_w = inner_box.width + 16.0;

            let tick_x = x;
            let tick_y = y - font_size * 0.1;
            let low_y = y + font_size * 0.35;
            let top_y = y - inner_box.height_above - 4.0;

            let sqrt_d = format!(
                "M {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2}",
                tick_x,
                tick_y,
                tick_x + 3.0,
                tick_y - 2.0,
                tick_x + 6.0,
                low_y,
                tick_x + 10.0,
                top_y,
                tick_x + sqrt_w,
                top_y
            );
            out_nodes.push(Node::Path {
                id: String::new(),
                d: sqrt_d,
                fill_rule: String::new(),
                fill: Paint::None,
                stroke: Stroke {
                    paint: Paint::solid("#0f172a"),
                    width: 1.5,
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    ..Default::default()
                },
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });

            render_node_recursive(inner, x + 12.0, y, font_size, out_nodes);

            LayoutBox {
                width: sqrt_w,
                height_above: inner_box.height_above + 6.0,
                height_below: inner_box.height_below,
            }
        }
        MathNode::Matrix { rows, delimiters } => {
            let row_count = rows.len();
            let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
            if row_count == 0 || col_count == 0 {
                return LayoutBox {
                    width: 0.0,
                    height_above: font_size * 0.8,
                    height_below: font_size * 0.2,
                };
            }

            let cell_font_size = font_size * 0.90;
            let col_gap = 16.0;
            let row_gap = 8.0;

            // Measure each cell
            let mut cell_boxes: Vec<Vec<LayoutBox>> = Vec::with_capacity(row_count);
            for r in rows {
                let mut r_boxes = Vec::with_capacity(r.len());
                for cell in r {
                    r_boxes.push(measure_box(cell, cell_font_size));
                }
                cell_boxes.push(r_boxes);
            }

            // Calculate max width for each column
            let mut col_widths = vec![0.0f64; col_count];
            for r_boxes in &cell_boxes {
                for (c_idx, b) in r_boxes.iter().enumerate() {
                    if b.width > col_widths[c_idx] {
                        col_widths[c_idx] = b.width;
                    }
                }
            }

            // Calculate height for each row
            let mut row_aboves = vec![cell_font_size * 0.8; row_count];
            let mut row_belows = vec![cell_font_size * 0.2; row_count];
            for (r_idx, r_boxes) in cell_boxes.iter().enumerate() {
                for b in r_boxes {
                    if b.height_above > row_aboves[r_idx] {
                        row_aboves[r_idx] = b.height_above;
                    }
                    if b.height_below > row_belows[r_idx] {
                        row_belows[r_idx] = b.height_below;
                    }
                }
            }

            let mut row_heights = Vec::with_capacity(row_count);
            for i in 0..row_count {
                row_heights.push(row_aboves[i] + row_belows[i] + row_gap);
            }
            let total_matrix_height: f64 = row_heights.iter().sum::<f64>() - row_gap;
            let inner_width: f64 = col_widths.iter().sum::<f64>()
                + if col_count > 1 {
                    (col_count - 1) as f64 * col_gap
                } else {
                    0.0
                };

            let has_visible_delims = matches!(*delimiters, Some((l, r)) if l != '.' || r != '.');
            let delim_pad = if has_visible_delims { 12.0 } else { 0.0 };
            let total_width = inner_width + delim_pad * 2.0;

            // Matrix vertical center aligned with baseline y
            let matrix_top = y - total_matrix_height / 2.0;
            let matrix_bottom = matrix_top + total_matrix_height;

            // Render delimiters
            if let Some((ld, rd)) = delimiters {
                let left_x = x + 3.0;
                let right_x = x + total_width - 3.0;
                match ld {
                    '(' => {
                        let d = format!(
                            "M {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2}",
                            left_x + 5.0,
                            matrix_top - 2.0,
                            left_x - 3.0,
                            y - total_matrix_height * 0.2,
                            left_x - 3.0,
                            y + total_matrix_height * 0.2,
                            left_x + 5.0,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                line_join: LineJoin::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    '[' => {
                        let d = format!(
                            "M {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2}",
                            left_x + 6.0,
                            matrix_top - 2.0,
                            left_x,
                            matrix_top - 2.0,
                            left_x,
                            matrix_bottom + 2.0,
                            left_x + 6.0,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                line_join: LineJoin::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    '|' => {
                        let d = format!(
                            "M {:.2},{:.2} L {:.2},{:.2}",
                            left_x + 2.0,
                            matrix_top - 2.0,
                            left_x + 2.0,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    '‖' => {
                        let d = format!(
                            "M {:.2},{:.2} L {:.2},{:.2} M {:.2},{:.2} L {:.2},{:.2}",
                            left_x + 1.0,
                            matrix_top - 2.0,
                            left_x + 1.0,
                            matrix_bottom + 2.0,
                            left_x + 4.5,
                            matrix_top - 2.0,
                            left_x + 4.5,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    '{' => {
                        let mid_y = y;
                        let d = format!(
                            "M {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2}",
                            left_x + 6.0,
                            matrix_top - 2.0,
                            left_x + 1.0,
                            matrix_top - 2.0,
                            left_x + 1.0,
                            matrix_top + 10.0,
                            left_x + 1.0,
                            mid_y - 2.0,
                            left_x - 4.0,
                            mid_y,
                            left_x + 1.0,
                            mid_y + 2.0,
                            left_x + 1.0,
                            matrix_bottom - 10.0,
                            left_x + 1.0,
                            matrix_bottom + 2.0,
                            left_x + 6.0,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                line_join: LineJoin::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    _ => {}
                }

                match rd {
                    ')' => {
                        let d = format!(
                            "M {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2}",
                            right_x - 5.0,
                            matrix_top - 2.0,
                            right_x + 3.0,
                            y - total_matrix_height * 0.2,
                            right_x + 3.0,
                            y + total_matrix_height * 0.2,
                            right_x - 5.0,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                line_join: LineJoin::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    ']' => {
                        let d = format!(
                            "M {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2}",
                            right_x - 6.0,
                            matrix_top - 2.0,
                            right_x,
                            matrix_top - 2.0,
                            right_x,
                            matrix_bottom + 2.0,
                            right_x - 6.0,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                line_join: LineJoin::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    '|' => {
                        let d = format!(
                            "M {:.2},{:.2} L {:.2},{:.2}",
                            right_x - 2.0,
                            matrix_top - 2.0,
                            right_x - 2.0,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    '‖' => {
                        let d = format!(
                            "M {:.2},{:.2} L {:.2},{:.2} M {:.2},{:.2} L {:.2},{:.2}",
                            right_x - 4.5,
                            matrix_top - 2.0,
                            right_x - 4.5,
                            matrix_bottom + 2.0,
                            right_x - 1.0,
                            matrix_top - 2.0,
                            right_x - 1.0,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    '}' => {
                        let mid_y = y;
                        let d = format!(
                            "M {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2}",
                            right_x - 6.0,
                            matrix_top - 2.0,
                            right_x - 1.0,
                            matrix_top - 2.0,
                            right_x - 1.0,
                            matrix_top + 10.0,
                            right_x - 1.0,
                            mid_y - 2.0,
                            right_x + 4.0,
                            mid_y,
                            right_x - 1.0,
                            mid_y + 2.0,
                            right_x - 1.0,
                            matrix_bottom - 10.0,
                            right_x - 1.0,
                            matrix_bottom + 2.0,
                            right_x - 6.0,
                            matrix_bottom + 2.0
                        );
                        out_nodes.push(Node::Path {
                            id: String::new(),
                            d,
                            fill_rule: String::new(),
                            fill: Paint::None,
                            stroke: Stroke {
                                paint: Paint::solid("#0f172a"),
                                width: 1.5,
                                line_cap: LineCap::Round,
                                line_join: LineJoin::Round,
                                ..Default::default()
                            },
                            transform: IDENTITY,
                            clip_id: None,
                            meta: SourceMeta::default(),
                        });
                    }
                    _ => {}
                }
            }

            // Render cells
            let mut cur_y = matrix_top;
            for (r_idx, row) in rows.iter().enumerate() {
                let row_baseline = cur_y + row_aboves[r_idx];
                let mut cur_x = x + delim_pad;
                for (c_idx, cell) in row.iter().enumerate() {
                    let cell_w = cell_boxes[r_idx].get(c_idx).map(|b| b.width).unwrap_or(0.0);
                    let target_col_w = col_widths.get(c_idx).copied().unwrap_or(cell_w);
                    let is_left_aligned =
                        matches!(*delimiters, Some(('{', '.')) | Some(('.', '.')));
                    let cell_x = if is_left_aligned {
                        cur_x
                    } else {
                        cur_x + (target_col_w - cell_w) / 2.0
                    };
                    render_node_recursive(cell, cell_x, row_baseline, cell_font_size, out_nodes);
                    cur_x += target_col_w + col_gap;
                }
                cur_y += row_aboves[r_idx] + row_belows[r_idx] + row_gap;
            }

            LayoutBox {
                width: total_width,
                height_above: (y - matrix_top).max(cell_font_size * 0.8) + 4.0,
                height_below: (matrix_bottom - y).max(cell_font_size * 0.2) + 4.0,
            }
        }
        MathNode::Row(children) => {
            let mut cur_x = x;
            let mut max_above = font_size * 0.8;
            let mut max_below = font_size * 0.2;

            for child in children {
                let b = render_node_recursive(child, cur_x, y, font_size, out_nodes);
                cur_x += b.width + 2.0;
                if b.height_above > max_above {
                    max_above = b.height_above;
                }
                if b.height_below > max_below {
                    max_below = b.height_below;
                }
            }

            LayoutBox {
                width: cur_x - x,
                height_above: max_above,
                height_below: max_below,
            }
        }
    }
}

fn measure_box(node: &MathNode, font_size: f64) -> LayoutBox {
    let mut dummy = Vec::new();
    render_node_recursive(node, 0.0, 0.0, font_size, &mut dummy)
}

/// Reverse extraction: restores LaTeX formula from SVG.
pub fn extract_latex_from_svg(svg_bytes: &[u8]) -> Result<String> {
    if let Some(embedded) = crate::cad::svg_reader::extract_embedded_source(svg_bytes) {
        return Ok(embedded);
    }

    let svg_text = std::str::from_utf8(svg_bytes)
        .map_err(|e| Error::InvalidInput(format!("SVG is not valid UTF-8: {e}")))?;

    let mut reader = Reader::from_str(svg_text);
    reader.config_mut().trim_text(true);

    let mut texts = Vec::new();
    let mut in_text = false;
    let mut current_text = String::new();

    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) if e.name().as_ref() == b"text" => {
                in_text = true;
                current_text.clear();
            }
            Event::Text(e) if in_text => {
                let bytes = e.as_ref();
                if let Ok(s) = std::str::from_utf8(bytes) {
                    current_text.push_str(s);
                }
            }
            Event::End(e) if e.name().as_ref() == b"text" => {
                in_text = false;
                let trimmed = current_text.trim();
                if !trimmed.is_empty() {
                    texts.push(trimmed.to_string());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(texts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_renders_matrices_and_environments() {
        let pmatrix = r"\begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}";
        let ast = parse_math_expression(pmatrix, 0).expect("parse pmatrix");
        match &ast {
            MathNode::Matrix { rows, delimiters } => {
                assert_eq!(*delimiters, Some(('(', ')')));
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].len(), 2);
                assert_eq!(rows[1].len(), 2);
            }
            _ => panic!("expected Matrix"),
        }

        let opts = ConvertOptions::default();
        let page = layout_and_render_math(&ast, pmatrix, &opts).expect("render pmatrix");
        assert!(page.width > 0.0);
        assert!(page.height > 0.0);
        assert!(!page.nodes.is_empty());
    }

    #[test]
    fn parses_nested_matrices_without_leaking_separators() {
        let nested = r"\begin{pmatrix} \begin{matrix} a & b \\ c & d \end{matrix} & 0 \\ 0 & 1 \end{pmatrix}";
        let ast = parse_math_expression(nested, 0).expect("parse nested");
        match &ast {
            MathNode::Matrix { rows, delimiters } => {
                assert_eq!(*delimiters, Some(('(', ')')));
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].len(), 2);
                // First cell is inner matrix
                match &rows[0][0] {
                    MathNode::Matrix {
                        rows: inner_rows,
                        delimiters: inner_delims,
                    } => {
                        assert_eq!(*inner_delims, None);
                        assert_eq!(inner_rows.len(), 2);
                    }
                    _ => panic!("expected inner Matrix"),
                }
            }
            _ => panic!("expected Matrix"),
        }
    }

    #[test]
    fn parses_cases_and_aligned() {
        let cases_tex = r"\begin{cases} x + 1 & x \ge 0 \\ -x & x < 0 \end{cases}";
        let ast = parse_math_expression(cases_tex, 0).expect("parse cases");
        match &ast {
            MathNode::Matrix { rows, delimiters } => {
                assert_eq!(*delimiters, Some(('{', '.')));
                assert_eq!(rows.len(), 2);
            }
            _ => panic!("expected Matrix"),
        }

        let aligned_tex = r"\begin{aligned} a &= b + c \\ d &= e \end{aligned}";
        let ast_aligned = parse_math_expression(aligned_tex, 0).expect("parse aligned");
        match &ast_aligned {
            MathNode::Matrix { rows, delimiters } => {
                assert_eq!(*delimiters, Some(('.', '.')));
                assert_eq!(rows.len(), 2);
            }
            _ => panic!("expected Matrix"),
        }
    }

    #[test]
    fn parses_array_with_column_spec() {
        let array_tex = r"\begin{array}{lcr} 1 & 2 & 3 \\ 4 & 5 & 6 \end{array}";
        let ast = parse_math_expression(array_tex, 0).expect("parse array");
        match &ast {
            MathNode::Matrix { rows, delimiters } => {
                assert_eq!(*delimiters, None);
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].len(), 3);
            }
            _ => panic!("expected Matrix"),
        }
    }

    #[test]
    fn parses_sqrt_with_optional_index() {
        let sqrt_tex = r"\sqrt[3]{x + 1}";
        let ast = parse_math_expression(sqrt_tex, 0).expect("parse sqrt");
        match &ast {
            MathNode::Sqrt(_) => {}
            _ => panic!("expected Sqrt"),
        }
    }
}
