//! Bounded ASCII and gzip-compressed ASCII polyMesh preview for a `.foam` marker.

use std::collections::HashSet;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use flate2::read::MultiGzDecoder;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke};

const MAX_OPENFOAM_BYTES: u64 = 256 * 1024 * 1024;
const MAX_OPENFOAM_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_OPENFOAM_POINTS: usize = 1_000_000;
const MAX_OPENFOAM_FACES: usize = 1_000_000;
const MAX_OPENFOAM_FACE_POINTS: usize = 100_000;
const MAX_OPENFOAM_CONNECTIVITY: usize = 8_000_000;
const MAX_OPENFOAM_CELLS: usize = 1_000_000;
const MAX_OPENFOAM_PATCHES: usize = 100_000;
const MAX_OPENFOAM_TOKENS: usize = 24_000_000;
const MAX_OPENFOAM_DEPTH: usize = 64;
const MAX_OPENFOAM_EDGES: usize = 2_000_000;
const MAX_OPENFOAM_OBJ_BYTES: usize = 128 * 1024 * 1024;
const MAX_OPENFOAM_BOUNDARY_OVERLAY_EDGES: usize = 200_000;
const MAX_OPENFOAM_PATCH_TEXT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token<'a> {
    Atom(&'a str),
    Punct(u8),
}

struct Lexer<'a, 'n> {
    text: &'a str,
    bytes: &'a [u8],
    position: usize,
    total_tokens: &'n mut usize,
}

impl<'a, 'n> Lexer<'a, 'n> {
    fn new(text: &'a str, total_tokens: &'n mut usize) -> Self {
        let position = usize::from(text.starts_with('\u{feff}')) * '\u{feff}'.len_utf8();
        Self {
            text,
            bytes: text.as_bytes(),
            position,
            total_tokens,
        }
    }

    fn next(&mut self) -> Result<Option<Token<'a>>> {
        self.skip_space_comments()?;
        if self.position >= self.bytes.len() {
            return Ok(None);
        }
        *self.total_tokens = self
            .total_tokens
            .checked_add(1)
            .ok_or_else(|| Error::LimitExceeded("OpenFOAM token count overflowed".into()))?;
        if *self.total_tokens > MAX_OPENFOAM_TOKENS {
            return Err(Error::LimitExceeded(format!(
                "OpenFOAM mesh tokens exceed {MAX_OPENFOAM_TOKENS}"
            )));
        }
        let byte = self.bytes[self.position];
        if matches!(byte, b'{' | b'}' | b'(' | b')' | b'[' | b']' | b';') {
            self.position += 1;
            return Ok(Some(Token::Punct(byte)));
        }
        if byte == b'\'' || byte == b'"' {
            let quote = byte;
            self.position += 1;
            let start = self.position;
            while self.position < self.bytes.len() {
                let current = self.bytes[self.position];
                if current == b'\\' {
                    self.position = (self.position + 2).min(self.bytes.len());
                } else if current == quote {
                    let end = self.position;
                    self.position += 1;
                    return Ok(Some(Token::Atom(&self.text[start..end])));
                } else {
                    self.position += 1;
                }
            }
            return Err(Error::InvalidInput(
                "unterminated quoted OpenFOAM token".into(),
            ));
        }
        let start = self.position;
        while self.position < self.bytes.len() {
            let current = self.bytes[self.position];
            if current.is_ascii_whitespace()
                || matches!(
                    current,
                    b'{' | b'}' | b'(' | b')' | b'[' | b']' | b';' | b'\'' | b'"'
                )
                || (current == b'/'
                    && matches!(self.bytes.get(self.position + 1), Some(b'/') | Some(b'*')))
            {
                break;
            }
            self.position += 1;
        }
        if start == self.position {
            return Err(Error::InvalidInput("invalid OpenFOAM token".into()));
        }
        Ok(Some(Token::Atom(&self.text[start..self.position])))
    }

    fn skip_space_comments(&mut self) -> Result<()> {
        loop {
            while self
                .bytes
                .get(self.position)
                .is_some_and(u8::is_ascii_whitespace)
            {
                self.position += 1;
            }
            if self.bytes.get(self.position) == Some(&b'/')
                && self.bytes.get(self.position + 1) == Some(&b'/')
            {
                self.position += 2;
                while self.position < self.bytes.len() && self.bytes[self.position] != b'\n' {
                    self.position += 1;
                }
                continue;
            }
            if self.bytes.get(self.position) == Some(&b'/')
                && self.bytes.get(self.position + 1) == Some(&b'*')
            {
                self.position += 2;
                let start = self.position;
                while self.position + 1 < self.bytes.len()
                    && !(self.bytes[self.position] == b'*' && self.bytes[self.position + 1] == b'/')
                {
                    self.position += 1;
                }
                if self.position + 1 >= self.bytes.len() {
                    return Err(Error::InvalidInput(
                        "unterminated OpenFOAM block comment".into(),
                    ));
                }
                if self.position - start > MAX_OPENFOAM_FILE_BYTES as usize {
                    return Err(Error::LimitExceeded(
                        "OpenFOAM block comment exceeds the per-file bound".into(),
                    ));
                }
                self.position += 2;
                continue;
            }
            return Ok(());
        }
    }
}

struct FoamParser<'a, 'n> {
    lexer: Lexer<'a, 'n>,
}

impl<'a, 'n> FoamParser<'a, 'n> {
    fn new(text: &'a str, expected_class: &str, total_tokens: &'n mut usize) -> Result<Self> {
        let mut parser = Self {
            lexer: Lexer::new(text, total_tokens),
        };
        if parser.next_atom("file header")? != "FoamFile" {
            return Err(Error::InvalidInput(
                "OpenFOAM polyMesh file is missing its FoamFile header".into(),
            ));
        }
        parser.expect_punct(b'{', "FoamFile header")?;
        let mut class = None;
        let mut format = None;
        loop {
            match parser.next_required("FoamFile header field")? {
                Token::Punct(b'}') => break,
                Token::Atom("class") => {
                    class = Some(parser.next_atom("FoamFile class")?.to_owned());
                    parser.expect_punct(b';', "FoamFile class terminator")?;
                }
                Token::Atom("format") => {
                    format = Some(parser.next_atom("FoamFile format")?.to_owned());
                    parser.expect_punct(b';', "FoamFile format terminator")?;
                }
                Token::Atom(_) => parser.skip_statement()?,
                _ => {
                    return Err(Error::InvalidInput(
                        "invalid OpenFOAM FoamFile header field".into(),
                    ));
                }
            }
        }
        if parser.peek_punct(b';')? {
            let _ = parser.lexer.next()?;
        }
        let class = class
            .ok_or_else(|| Error::InvalidInput("OpenFOAM FoamFile header has no class".into()))?;
        if !class.eq_ignore_ascii_case(expected_class) {
            return Err(Error::InvalidInput(format!(
                "OpenFOAM mesh file class '{class}' does not match '{expected_class}'"
            )));
        }
        let format = format
            .ok_or_else(|| Error::InvalidInput("OpenFOAM FoamFile header has no format".into()))?;
        if !format.eq_ignore_ascii_case("ascii") {
            return Err(Error::Unsupported(format!(
                "OpenFOAM '{expected_class}' files with format '{format}' are unsupported; ASCII is required"
            )));
        }
        Ok(parser)
    }

    fn next_required(&mut self, context: &str) -> Result<Token<'a>> {
        self.lexer
            .next()?
            .ok_or_else(|| Error::InvalidInput(format!("OpenFOAM {context} is missing")))
    }

    fn next_atom(&mut self, context: &str) -> Result<&'a str> {
        match self.next_required(context)? {
            Token::Atom(value) => Ok(value),
            _ => Err(Error::InvalidInput(format!(
                "OpenFOAM {context} must be a word or value"
            ))),
        }
    }

    fn next_usize(&mut self, context: &str) -> Result<usize> {
        let value = self.next_atom(context)?;
        value
            .parse::<usize>()
            .map_err(|_| Error::InvalidInput(format!("invalid OpenFOAM {context} value '{value}'")))
    }

    fn next_f64(&mut self, context: &str) -> Result<f64> {
        let value = self.next_atom(context)?;
        let number = value.parse::<f64>().map_err(|_| {
            Error::InvalidInput(format!("invalid OpenFOAM {context} value '{value}'"))
        })?;
        if !number.is_finite() || number.abs() > 1e12 {
            return Err(Error::InvalidInput(format!(
                "OpenFOAM {context} is non-finite or outside ±1e12"
            )));
        }
        Ok(number)
    }

    fn expect_punct(&mut self, expected: u8, context: &str) -> Result<()> {
        match self.next_required(context)? {
            Token::Punct(actual) if actual == expected => Ok(()),
            _ => Err(Error::InvalidInput(format!(
                "OpenFOAM {context} has an unexpected token"
            ))),
        }
    }

    fn peek_punct(&mut self, expected: u8) -> Result<bool> {
        let position = self.lexer.position;
        let total_tokens = *self.lexer.total_tokens;
        let result = matches!(self.lexer.next()?, Some(Token::Punct(actual)) if actual == expected);
        self.lexer.position = position;
        *self.lexer.total_tokens = total_tokens;
        Ok(result)
    }

    fn skip_statement(&mut self) -> Result<()> {
        let mut parens = 0usize;
        let mut brackets = 0usize;
        let mut braces = 0usize;
        loop {
            match self.next_required("dictionary statement")? {
                Token::Punct(b'(') => {
                    parens += 1;
                    if parens > MAX_OPENFOAM_DEPTH {
                        return Err(Error::LimitExceeded(format!(
                            "OpenFOAM dictionary nesting exceeds {MAX_OPENFOAM_DEPTH}"
                        )));
                    }
                }
                Token::Punct(b')') => {
                    parens = parens
                        .checked_sub(1)
                        .ok_or_else(|| Error::InvalidInput("unbalanced OpenFOAM ')'".into()))?;
                }
                Token::Punct(b'[') => {
                    brackets += 1;
                    if brackets > MAX_OPENFOAM_DEPTH {
                        return Err(Error::LimitExceeded(format!(
                            "OpenFOAM dictionary nesting exceeds {MAX_OPENFOAM_DEPTH}"
                        )));
                    }
                }
                Token::Punct(b']') => {
                    brackets = brackets
                        .checked_sub(1)
                        .ok_or_else(|| Error::InvalidInput("unbalanced OpenFOAM ']'".into()))?;
                }
                Token::Punct(b'{') => {
                    braces += 1;
                    if braces > MAX_OPENFOAM_DEPTH {
                        return Err(Error::LimitExceeded(format!(
                            "OpenFOAM dictionary nesting exceeds {MAX_OPENFOAM_DEPTH}"
                        )));
                    }
                }
                Token::Punct(b'}') if braces > 0 => braces -= 1,
                Token::Punct(b'}') => {
                    return Err(Error::InvalidInput(
                        "OpenFOAM dictionary statement ended before ';'".into(),
                    ));
                }
                Token::Punct(b';') if parens == 0 && brackets == 0 && braces == 0 => return Ok(()),
                _ => {}
            }
        }
    }

    fn finish(&mut self) -> Result<()> {
        if self.lexer.next()?.is_some() {
            return Err(Error::InvalidInput(
                "unexpected values after OpenFOAM mesh list".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<'a, 'n>(
        text: &'a str,
        class: &str,
        total_tokens: &'n mut usize,
    ) -> FoamParser<'a, 'n> {
        FoamParser::new(text, class, total_tokens).unwrap()
    }

    #[test]
    fn parses_ascii_poly_mesh_lists_and_patch_ranges() {
        let points = "FoamFile { version 2.0; format ascii; class vectorField; object points; } 4((0 0 0)(1 0 0)(1 1 0)(0 1 0))";
        let faces = "FoamFile { format ascii; class faceList; } 2(4(0 1 2 3)4(0 3 2 1))";
        let owner = "FoamFile { format ascii; class labelList; } 2(0 0)";
        let neighbour = "FoamFile { format ascii; class labelList; } 0()";
        let boundary = "FoamFile { format ascii; class polyBoundaryMesh; } 1(walls { type wall; nFaces 2; startFace 0; inGroups 1(wall); })";
        let mut tokens = 0;
        assert_eq!(
            parse(points, "vectorField", &mut tokens)
                .read_points()
                .unwrap()
                .len(),
            4
        );
        let (faces, total_indices) = parse(faces, "faceList", &mut tokens).read_faces().unwrap();
        assert_eq!(faces.len(), 2);
        assert_eq!(total_indices, 8);
        assert_eq!(
            parse(owner, "labelList", &mut tokens)
                .read_labels("owner")
                .unwrap(),
            vec![0, 0]
        );
        assert!(
            parse(neighbour, "labelList", &mut tokens)
                .read_labels("neighbour")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            parse(boundary, "polyBoundaryMesh", &mut tokens)
                .read_patches()
                .unwrap(),
            vec![PatchRange {
                start_face: 0,
                face_count: 2
            }]
        );
    }

    #[test]
    fn rejects_binary_mesh_lists_and_uncovered_patch_faces() {
        let binary = "FoamFile { format binary; class vectorField; } 0()";
        let mut tokens = 0;
        assert!(FoamParser::new(binary, "vectorField", &mut tokens).is_err());
        assert!(
            validate_patches(
                &[PatchRange {
                    start_face: 0,
                    face_count: 1
                }],
                2,
                0
            )
            .is_err()
        );
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PatchRange {
    start_face: usize,
    face_count: usize,
}

struct OpenFoamPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    points: &'a [(f64, f64, f64)],
    boundary_edges: &'a [(usize, usize)],
}

impl PageConsumer for OpenFoamPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "openfoam".into();
        page.title = "OpenFOAM polyMesh".into();
        retag_wireframe(&mut page.nodes);
        if !self.boundary_edges.is_empty() {
            let cos30 = (30.0_f64.to_radians()).cos();
            let sin30 = (30.0_f64.to_radians()).sin();
            let project = |point: (f64, f64, f64)| {
                (
                    (point.0 - point.1) * cos30,
                    (point.0 + point.1) * sin30 - point.2,
                )
            };
            let min_x = self
                .points
                .iter()
                .map(|point| project(*point).0)
                .fold(f64::INFINITY, f64::min);
            let min_y = self
                .points
                .iter()
                .map(|point| project(*point).1)
                .fold(f64::INFINITY, f64::min);
            let max_x = self
                .points
                .iter()
                .map(|point| project(*point).0)
                .fold(f64::NEG_INFINITY, f64::max);
            let max_y = self
                .points
                .iter()
                .map(|point| project(*point).1)
                .fold(f64::NEG_INFINITY, f64::max);
            let (min_x, min_y, max_x, max_y) = if min_x >= max_x || min_y >= max_y {
                (0.0, 0.0, 100.0, 100.0)
            } else {
                (min_x, min_y, max_x, max_y)
            };
            let raw_w = (max_x - min_x).max(1e-9);
            let raw_h = (max_y - min_y).max(1e-9);
            let margin = (raw_w.max(raw_h) * 0.1).max(0.01);
            let content_w = raw_w + 2.0 * margin;
            let content_h = raw_h + 2.0 * margin;
            let scale = (1200.0 / content_w.max(content_h)).clamp(0.01, 10_000.0);
            let map = |point: (f64, f64, f64)| {
                let (x, y) = project(point);
                ((x - min_x + margin) * scale, (y - min_y + margin) * scale)
            };
            let mut path = String::new();
            for (a, b) in self.boundary_edges {
                if let (Some(point_a), Some(point_b)) = (self.points.get(*a), self.points.get(*b)) {
                    let a = map(*point_a);
                    let b = map(*point_b);
                    let _ = write!(path, "M {:.3} {:.3} L {:.3} {:.3} ", a.0, a.1, b.0, b.1);
                }
            }
            if !path.is_empty() {
                page.nodes.push(Node::Path {
                    id: "openfoam-boundary-patches".into(),
                    d: path,
                    fill_rule: "nonzero".into(),
                    fill: Paint::None,
                    stroke: Stroke {
                        paint: Paint::solid("#f97316"),
                        width: 2.0,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        ..Default::default()
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "openfoam:boundary-patches".into(),
                        semantic_role: "simulation:boundary-patch".into(),
                        alt_text: "OpenFOAM boundary patch edges".into(),
                        ..Default::default()
                    },
                });
            }
        }
        self.inner.consume(page)
    }
}

fn retag_wireframe(nodes: &mut [Node]) {
    for node in nodes {
        if let Node::Group {
            nodes: children,
            meta,
            ..
        } = node
        {
            if meta.semantic_role == "obj:wireframe" {
                meta.kind = "openfoam:poly-mesh".into();
                meta.semantic_role = "simulation:mesh".into();
                meta.alt_text = "OpenFOAM polyMesh wireframe".into();
            }
            retag_wireframe(children);
        }
    }
}

pub(crate) fn convert(
    input: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let input_limit = options.max_input_bytes.min(MAX_OPENFOAM_BYTES);
    let case_root = input
        .parent()
        .ok_or_else(|| Error::InvalidInput("OpenFOAM marker has no case directory".into()))?
        .canonicalize()?;
    let canonical_marker = input.canonicalize()?;
    if !canonical_marker.starts_with(&case_root) {
        return Err(Error::Unsupported(
            "OpenFOAM marker symlink escapes its case directory".into(),
        ));
    }
    let marker_size = fs::metadata(&canonical_marker)?.len();
    if marker_size > MAX_OPENFOAM_FILE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OpenFOAM .foam marker exceeds {MAX_OPENFOAM_FILE_BYTES} bytes"
        )));
    }
    let mut total_bytes = marker_size;
    let mut total_expanded_bytes = marker_size;
    let mesh_dir = case_root.join("constant").join("polyMesh");
    let mut tokens = 0usize;
    let points = {
        let text = read_mesh_file(
            &mesh_dir.join("points"),
            &case_root,
            input_limit,
            &mut total_bytes,
            &mut total_expanded_bytes,
        )?;
        FoamParser::new(&text, "vectorField", &mut tokens)?.read_points()?
    };
    let (faces, face_index_count) = {
        let text = read_mesh_file(
            &mesh_dir.join("faces"),
            &case_root,
            input_limit,
            &mut total_bytes,
            &mut total_expanded_bytes,
        )?;
        FoamParser::new(&text, "faceList", &mut tokens)?.read_faces()?
    };
    let owner = {
        let text = read_mesh_file(
            &mesh_dir.join("owner"),
            &case_root,
            input_limit,
            &mut total_bytes,
            &mut total_expanded_bytes,
        )?;
        FoamParser::new(&text, "labelList", &mut tokens)?.read_labels("owner")?
    };
    let neighbour = {
        let text = read_mesh_file(
            &mesh_dir.join("neighbour"),
            &case_root,
            input_limit,
            &mut total_bytes,
            &mut total_expanded_bytes,
        )?;
        FoamParser::new(&text, "labelList", &mut tokens)?.read_labels("neighbour")?
    };
    let patches = {
        let text = read_mesh_file(
            &mesh_dir.join("boundary"),
            &case_root,
            input_limit,
            &mut total_bytes,
            &mut total_expanded_bytes,
        )?;
        FoamParser::new(&text, "polyBoundaryMesh", &mut tokens)?.read_patches()?
    };
    if points.is_empty() {
        return Err(Error::Unsupported("OpenFOAM polyMesh has no points".into()));
    }
    if faces.is_empty() {
        return Err(Error::Unsupported("OpenFOAM polyMesh has no faces".into()));
    }
    if owner.len() != faces.len() || neighbour.len() > faces.len() {
        return Err(Error::InvalidInput(
            "OpenFOAM owner/neighbour counts do not match the face list".into(),
        ));
    }
    if owner
        .iter()
        .chain(neighbour.iter())
        .any(|cell| *cell >= MAX_OPENFOAM_CELLS)
    {
        return Err(Error::LimitExceeded(format!(
            "OpenFOAM cell label exceeds {MAX_OPENFOAM_CELLS}"
        )));
    }
    if face_index_count > MAX_OPENFOAM_CONNECTIVITY {
        return Err(Error::LimitExceeded(format!(
            "OpenFOAM face connectivity exceeds {MAX_OPENFOAM_CONNECTIVITY}"
        )));
    }
    for face in &faces {
        if face.iter().any(|point| *point >= points.len()) {
            return Err(Error::InvalidInput(
                "OpenFOAM face references a point outside the points list".into(),
            ));
        }
    }
    validate_patches(&patches, faces.len(), neighbour.len())?;

    let mut all_edges = HashSet::new();
    for face in &faces {
        append_face_edges(face, &mut all_edges)?;
    }
    let mut boundary_edges = HashSet::new();
    for patch in &patches {
        let end = patch
            .start_face
            .checked_add(patch.face_count)
            .ok_or_else(|| {
                Error::LimitExceeded("OpenFOAM boundary face range overflowed".into())
            })?;
        for face in &faces[patch.start_face..end] {
            append_face_edges(face, &mut boundary_edges)?;
        }
    }
    let mut ordered_edges = all_edges.into_iter().collect::<Vec<_>>();
    ordered_edges.sort_unstable();
    let mut obj_text = String::new();
    for (x, y, z) in &points {
        writeln!(&mut obj_text, "v {x} {y} {z}")
            .map_err(|_| Error::InvalidInput("could not write OpenFOAM mesh vertex".into()))?;
        check_obj_size(&obj_text, options)?;
    }
    for (a, b) in ordered_edges {
        writeln!(&mut obj_text, "l {} {}", a + 1, b + 1)
            .map_err(|_| Error::InvalidInput("could not write OpenFOAM mesh edge".into()))?;
        check_obj_size(&obj_text, options)?;
    }
    if obj_text.is_empty() {
        return Err(Error::Unsupported(
            "OpenFOAM polyMesh has no renderable edges".into(),
        ));
    }
    let mut boundary_edges = boundary_edges.into_iter().collect::<Vec<_>>();
    boundary_edges.sort_unstable();
    let mut warnings = vec![
        "OpenFOAM polyMesh faces render as an isometric wireframe; patch names, cell values and solver settings are not displayed".into(),
    ];
    if boundary_edges.len() > MAX_OPENFOAM_BOUNDARY_OVERLAY_EDGES {
        warnings.push(format!(
            "OpenFOAM boundary overlay exceeds {MAX_OPENFOAM_BOUNDARY_OVERLAY_EDGES} edges and was omitted"
        ));
        boundary_edges.clear();
    }
    let mut page_sink = OpenFoamPageSink {
        inner: sink,
        points: &points,
        boundary_edges: &boundary_edges,
    };
    warnings.extend(crate::cad::obj::convert(
        Cursor::new(obj_text),
        options,
        &mut page_sink,
    )?);
    Ok(deduplicate_warnings(warnings))
}

fn check_obj_size(obj_text: &str, options: &ConvertOptions) -> Result<()> {
    let limit = options.max_input_bytes.min(MAX_OPENFOAM_OBJ_BYTES as u64) as usize;
    if obj_text.len() > limit {
        return Err(Error::LimitExceeded(format!(
            "OpenFOAM wireframe intermediate exceeds {limit} bytes"
        )));
    }
    Ok(())
}

fn deduplicate_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}

fn read_mesh_file(
    path: &Path,
    case_root: &Path,
    input_limit: u64,
    total_bytes: &mut u64,
    total_expanded_bytes: &mut u64,
) -> Result<String> {
    let mut compressed_name = path.as_os_str().to_owned();
    compressed_name.push(".gz");
    let compressed_path = PathBuf::from(compressed_name);
    let selected_path = if path.exists() {
        path
    } else if compressed_path.exists() {
        &compressed_path
    } else {
        path
    };
    let compressed = selected_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".gz"));
    let canonical = selected_path.canonicalize().map_err(|_| {
        Error::InvalidInput(format!(
            "OpenFOAM polyMesh file '{}' is missing",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
        ))
    })?;
    if !canonical.starts_with(case_root) {
        return Err(Error::Unsupported(
            "OpenFOAM polyMesh file escapes the case directory".into(),
        ));
    }
    let mut file = fs::File::open(&canonical)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::InvalidInput(
            "OpenFOAM polyMesh sidecar is not a regular file".into(),
        ));
    }
    let size = metadata.len();
    if size > MAX_OPENFOAM_FILE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OpenFOAM file '{}' exceeds {MAX_OPENFOAM_FILE_BYTES} bytes",
            canonical
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
        )));
    }
    let mut bytes = Vec::with_capacity(size.min(MAX_OPENFOAM_FILE_BYTES) as usize);
    Read::take(&mut file, MAX_OPENFOAM_FILE_BYTES.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_OPENFOAM_FILE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OpenFOAM file '{}' exceeds {MAX_OPENFOAM_FILE_BYTES} bytes",
            canonical
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?")
        )));
    }
    *total_bytes = total_bytes
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| Error::LimitExceeded("OpenFOAM total file size overflowed".into()))?;
    if *total_bytes > input_limit {
        return Err(Error::LimitExceeded(format!(
            "OpenFOAM case files exceed {input_limit} bytes"
        )));
    }
    let decoded = if compressed {
        let remaining = input_limit
            .saturating_sub(*total_expanded_bytes)
            .min(MAX_OPENFOAM_FILE_BYTES);
        let mut expanded = Vec::with_capacity(bytes.len().min(remaining as usize));
        Read::take(
            MultiGzDecoder::new(Cursor::new(bytes)),
            remaining.saturating_add(1),
        )
        .read_to_end(&mut expanded)?;
        if expanded.len() as u64 > remaining {
            return Err(Error::LimitExceeded(format!(
                "decompressed OpenFOAM file exceeds the remaining {remaining} byte budget"
            )));
        }
        *total_expanded_bytes = total_expanded_bytes
            .checked_add(expanded.len() as u64)
            .ok_or_else(|| Error::LimitExceeded("OpenFOAM expanded size overflowed".into()))?;
        expanded
    } else {
        *total_expanded_bytes = total_expanded_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| Error::LimitExceeded("OpenFOAM expanded size overflowed".into()))?;
        if *total_expanded_bytes > input_limit {
            return Err(Error::LimitExceeded(format!(
                "OpenFOAM expanded mesh files exceed {input_limit} bytes"
            )));
        }
        bytes
    };
    String::from_utf8(decoded).map_err(|error| {
        Error::Unsupported(format!("OpenFOAM mesh files must use ASCII/UTF-8: {error}"))
    })
}

fn validate_patches(
    patches: &[PatchRange],
    face_count: usize,
    internal_face_count: usize,
) -> Result<()> {
    if internal_face_count > face_count {
        return Err(Error::InvalidInput(
            "OpenFOAM neighbour count exceeds the number of faces".into(),
        ));
    }
    let boundary_count = face_count - internal_face_count;
    let mut assigned = vec![false; boundary_count];
    for patch in patches {
        let end = patch
            .start_face
            .checked_add(patch.face_count)
            .ok_or_else(|| Error::LimitExceeded("OpenFOAM patch range overflowed".into()))?;
        if patch.start_face < internal_face_count || end > face_count {
            return Err(Error::InvalidInput(
                "OpenFOAM patch face range is outside the boundary face list".into(),
            ));
        }
        for face in patch.start_face..end {
            let local = face - internal_face_count;
            if std::mem::replace(&mut assigned[local], true) {
                return Err(Error::InvalidInput(
                    "OpenFOAM boundary patches overlap".into(),
                ));
            }
        }
    }
    if assigned.iter().any(|assigned| !assigned) {
        return Err(Error::InvalidInput(
            "OpenFOAM boundary patches do not cover all boundary faces".into(),
        ));
    }
    Ok(())
}

fn append_face_edges(face: &[usize], edges: &mut HashSet<(usize, usize)>) -> Result<()> {
    if face.len() < 3 || face.len() > MAX_OPENFOAM_FACE_POINTS {
        return Err(Error::InvalidInput(format!(
            "OpenFOAM face must have 3..={MAX_OPENFOAM_FACE_POINTS} vertices"
        )));
    }
    for index in 0..face.len() {
        let a = face[index];
        let b = face[(index + 1) % face.len()];
        if a != b {
            edges.insert((a.min(b), a.max(b)));
            if edges.len() > MAX_OPENFOAM_EDGES {
                return Err(Error::LimitExceeded(format!(
                    "OpenFOAM rendered edges exceed {MAX_OPENFOAM_EDGES}"
                )));
            }
        }
    }
    Ok(())
}

impl<'a, 'n> FoamParser<'a, 'n> {
    fn read_points(mut self) -> Result<Vec<(f64, f64, f64)>> {
        let count = self.next_usize("point count")?;
        if count > MAX_OPENFOAM_POINTS {
            return Err(Error::LimitExceeded(format!(
                "OpenFOAM points exceed {MAX_OPENFOAM_POINTS}"
            )));
        }
        self.expect_punct(b'(', "points list")?;
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            self.expect_punct(b'(', "point vector")?;
            let x = self.next_f64("point x")?;
            let y = self.next_f64("point y")?;
            let z = self.next_f64("point z")?;
            self.expect_punct(b')', "point vector end")?;
            points.push((x, y, z));
        }
        self.expect_punct(b')', "points list end")?;
        self.finish()?;
        Ok(points)
    }

    fn read_faces(mut self) -> Result<(Vec<Vec<usize>>, usize)> {
        let count = self.next_usize("face count")?;
        if count > MAX_OPENFOAM_FACES {
            return Err(Error::LimitExceeded(format!(
                "OpenFOAM faces exceed {MAX_OPENFOAM_FACES}"
            )));
        }
        self.expect_punct(b'(', "faces list")?;
        let mut faces = Vec::with_capacity(count);
        let mut total_indices = 0usize;
        for _ in 0..count {
            let point_count = self.next_usize("face vertex count")?;
            if !(3..=MAX_OPENFOAM_FACE_POINTS).contains(&point_count) {
                return Err(Error::InvalidInput(format!(
                    "OpenFOAM face vertex count must be 3..={MAX_OPENFOAM_FACE_POINTS}"
                )));
            }
            self.expect_punct(b'(', "face vertex list")?;
            total_indices = total_indices
                .checked_add(point_count)
                .ok_or_else(|| Error::LimitExceeded("OpenFOAM connectivity overflowed".into()))?;
            if total_indices > MAX_OPENFOAM_CONNECTIVITY {
                return Err(Error::LimitExceeded(format!(
                    "OpenFOAM face connectivity exceeds {MAX_OPENFOAM_CONNECTIVITY}"
                )));
            }
            let mut face = Vec::with_capacity(point_count);
            for _ in 0..point_count {
                face.push(self.next_usize("face point index")?);
            }
            self.expect_punct(b')', "face vertex list end")?;
            faces.push(face);
        }
        self.expect_punct(b')', "faces list end")?;
        self.finish()?;
        Ok((faces, total_indices))
    }

    fn read_labels(mut self, context: &str) -> Result<Vec<usize>> {
        let count = self.next_usize(&format!("{context} count"))?;
        if count > MAX_OPENFOAM_FACES {
            return Err(Error::LimitExceeded(format!(
                "OpenFOAM {context} count exceeds {MAX_OPENFOAM_FACES}"
            )));
        }
        self.expect_punct(b'(', &format!("{context} list"))?;
        let mut labels = Vec::with_capacity(count);
        for _ in 0..count {
            labels.push(self.next_usize(context)?);
        }
        self.expect_punct(b')', &format!("{context} list end"))?;
        self.finish()?;
        Ok(labels)
    }

    fn read_patches(mut self) -> Result<Vec<PatchRange>> {
        let count = self.next_usize("boundary patch count")?;
        if count > MAX_OPENFOAM_PATCHES {
            return Err(Error::LimitExceeded(format!(
                "OpenFOAM boundary patches exceed {MAX_OPENFOAM_PATCHES}"
            )));
        }
        self.expect_punct(b'(', "boundary list")?;
        let mut patches = Vec::with_capacity(count);
        let mut name_bytes = 0usize;
        for _ in 0..count {
            let name = self.next_atom("boundary patch name")?;
            if name.is_empty() || name.len() > 1024 {
                return Err(Error::InvalidInput(
                    "OpenFOAM patch name must be 1..=1024 bytes".into(),
                ));
            }
            name_bytes = name_bytes.checked_add(name.len()).ok_or_else(|| {
                Error::LimitExceeded("OpenFOAM patch name size overflowed".into())
            })?;
            if name_bytes > MAX_OPENFOAM_PATCH_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "OpenFOAM patch names exceed {MAX_OPENFOAM_PATCH_TEXT_BYTES} bytes"
                )));
            }
            self.expect_punct(b'{', "boundary patch dictionary")?;
            let mut start_face = None;
            let mut face_count = None;
            loop {
                let token = self.next_required("boundary patch entry")?;
                if token == Token::Punct(b'}') {
                    break;
                }
                let Token::Atom(key) = token else {
                    return Err(Error::InvalidInput(
                        "OpenFOAM boundary patch key must be a word".into(),
                    ));
                };
                match key {
                    "startFace" => {
                        start_face = Some(self.next_usize("patch startFace")?);
                        self.expect_punct(b';', "patch startFace terminator")?;
                    }
                    "nFaces" => {
                        face_count = Some(self.next_usize("patch nFaces")?);
                        self.expect_punct(b';', "patch nFaces terminator")?;
                    }
                    _ => self.skip_statement()?,
                }
            }
            let start_face = start_face.ok_or_else(|| {
                Error::InvalidInput(format!("OpenFOAM patch '{name}' is missing startFace"))
            })?;
            let face_count = face_count.ok_or_else(|| {
                Error::InvalidInput(format!("OpenFOAM patch '{name}' is missing nFaces"))
            })?;
            patches.push(PatchRange {
                start_face,
                face_count,
            });
        }
        self.expect_punct(b')', "boundary list end")?;
        self.finish()?;
        Ok(patches)
    }
}
