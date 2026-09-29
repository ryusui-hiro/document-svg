//! CAD / CAM / CAE / 3D / Simulation file format converters and SVG reverse-packaging.
//!
//! This module provides in-crate implementations of CAD format parsers, vector
//! renderers, and reverse converters without linking against copyleft CAD kernels
//! (e.g. GPL LibreDWG or LGPL Open CASCADE).
//!
//! # Supported Formats
//!
//! - **AutoCAD DXF** ([`dxf`]): ASCII and binary DXF (Release 12 through 2018+). Parses layers,
//!   line types, blocks, and entities (LINE, CIRCLE, ARC, LWPOLYLINE with bulge arcs,
//!   TEXT, MTEXT, SOLID, 3DFACE, INSERT). Also provides reverse packaging of SVG
//!   documents back into standard AutoCAD Release 12 DXF entities via [`dxf::writer`].
//! - **Gerber RS-274X** ([`gerber`]): Printed Circuit Board (PCB) CAM data. Renders
//!   apertures (Circle, Rectangle, Obround, Polygon), flashes (D03), linear/circular
//!   interpolations (D01/D02), and polygon copper fills (G36/G37) onto a substrate preview.
//!   Reverse packaging to RS-274X via [`gerber::writer`].
//! - **KiCad PCB** ([`kicad`]): Bounded S-expression board preview (`.kicad_pcb`) for
//!   common traces/arcs, vias, pads, board/footprint graphics, and cached zone polygons.
//!   It does not run DRC or resolve external libraries, images, or 3D models.
//! - **SPICE/ngspice netlists** ([`spice`]): Bounded element-card and node/value
//!   previews (`.cir`, `.sp`, `.spice`, `.ckt`, `.net`); simulation and include paths stay inert.
//! - **KiCad S-expression schematics** ([`kicad_sch_modern`]): KiCad 6+ `.kicad_sch`
//!   symbols, wires, buses, labels, text, and junctions with embedded libraries left inert.
//! - **LTspice schematics** ([`ltspice`]): UTF-8/UTF-16 `.asc` WIRE/FLAG/SYMBOL/TEXT previews;
//! - **Autodesk EAGLE XML schematics** ([`eagle`]): `.sch` parts, instances, wires, labels, and text with external libraries and scripts left inert.
//!   `.asy` libraries and simulation commands are never opened or executed.
//! - **HP-GL / HP-GL/2** ([`hpgl`]): Hewlett-Packard Graphics Language plotter files.
//!   Renders vector motion, pen selection (8-pen palette), arcs, circles, and line widths.
//!   Reverse packaging to HP-GL via [`hpgl::writer`].
//! - **CNC G-code** ([`gcode`]): Machine toolpaths (`.gcode`, `.nc`, `.ngc`, `.tap`).
//!   Renders rapid/feed moves, arcs, spindle and feed modal tracking.
//!   Reverse packaging from SVG vector paths via [`gcode::writer`].
//! - **Excellon Drill** ([`excellon`]): PCB numerical control drilling (`.drl`, `.drd`, `.xln`).
//!   Renders hole diameter tools, drill hits, and FR-4 substrate.
//!   Reverse packaging from SVG circles via [`excellon::writer`].
//! - **3D STL Slicer** ([`stl`]): Stereolithography CAD (`.stl`).
//!   Horizontal cross-section geometric slicing and contours (Slice-to-SVG).
//!   Reverse packaging via 2.5D extrusion triangulation via [`stl::writer`].
//! - **glTF 2.0 / GLB** ([`gltf`]): JSON and binary scene packages (`.gltf`, `.glb`).
//!   Resolves the default node scene and transforms; renders bounded triangle and line primitives.
//! - **COLLADA** ([`collada`]): XML `.dae` triangle and polylist geometry with bounded
//!   position sources, rendered through the shaded OBJ mesh pipeline. Materials, animations,
//!   controllers, external references, and node transforms are reported as omissions.
//! - **VRML97** ([`vrml`]): bounded text `Coordinate`/`IndexedFaceSet`/`IndexedLineSet`
//!   mesh preview. Scene actions, appearances, and external URLs remain inert.
//! - **STEP ISO 10303-21** ([`step`]): CAD standard exchange format (`.step`, `.stp`).
//!   Parses Part 21 ASCII geometric representations, including `CIRCLE`/`ELLIPSE`
//!   axis-placed arcs and `B_SPLINE_CURVE`/`B_SPLINE_CURVE_WITH_KNOTS` NURBS edges
//!   tessellated through [`nurbs`], and renders isometric wireframes.
//!   Reverse packaging to standard ISO 10303-21 Part 21 models via [`step::writer`].
//! - **Industry Foundation Classes** ([`ifc`]): bounded IFC-SPF, buildingSMART IFCXML,
//!   and IFCZIP previews for IFC4 tessellated faces and selected extruded profiles with
//!   product/mapped-instance placements.
//! - **Object File Format** (`off`): ASCII OFF/COFF/NOFF/CNOFF polygon meshes (`.off`).
//!   Reuses the shaded 3D polygon renderer while retaining OFF-specific bounds and validation.
//! - **Wavefront OBJ** ([`obj`]): 3D polygonal models (`.obj`).
//!   Renders shaded vector polygons with Lambertian diffuse illumination.
//!   Reverse packaging from SVG contours to 3D OBJ meshes via [`obj::writer`].
//! - **Point Cloud Data (PCD)** ([`pcd`]): PCL ASCII, binary, and LZF-compressed
//!   point-cloud files (`.pcd`), rendered through the bounded PLY point renderer.
//! - **Leica PTS** ([`pts`]): ASCII unstructured point clouds (`.pts`) with a
//!   declared point count and XYZ/intensity or XYZ/intensity/RGB records.
//! - **Leica PTX** ([`ptx`]): ASCII structured, multi-scan point clouds (`.ptx`).
//!   Applies each cloud transform, retains RGB or maps intensity to grayscale, and
//!   samples grid cells deterministically under a shared point budget.
//! - **XYZ ASCII point clouds** ([`xyz`]): bounded homogeneous XYZ/XYZI/RGB variants
//!   with common named-header mappings and optional normals, rendered through the
//!   shared sampled point-cloud pipeline.
//! - **ASTM E57** ([`e57`]): bounded, CRC-checked binary/XML 3D scan interchange.
//!   Streams point clouds with scan poses, colors or intensity, and optional spherical coordinates.
//! - **LAS/LAZ** ([`las`]): ASPRS LAS point clouds (`.las`) and LAZ-compressed
//!   point clouds (`.laz`) with bounded, sampled XYZ/RGB previews.
//! - **CAE Simulation** ([`simulation`]): Finite Element and scientific field visualization, including ANSYS CDB, Abaqus, LS-DYNA, MEDIT, Nastran, SU2, OpenFOAM, Tecplot, EnSight Gold, PLOT3D, and UNV input decks.
//!   ANSYS CDB blocked `NBLOCK`/`EBLOCK` meshes (`.cdb`); Abaqus flat/part-instance meshes (`.inp`); LS-DYNA Keyword meshes (`.k`, `.key`); MEDIT ASCII/binary meshes (`.mesh`, `.meshb`); Nastran Bulk Data free/small/large-field meshes (`.bdf`, `.nas`); Gmsh ASCII MSH 2.x/4.0/4.1, binary MSH 2.2/4.0/4.1 with NodeData/ElementData; and VTK Legacy ASCII/big-endian binary
//!   (`.vtk`) / XML (`.vtu`, `.vtp`, `.vti`, `.vtr`, `.vts`) meshes with point/cell scalar field colormaps. VTK XML inline ASCII,
//!   base64 inline/appended arrays and zlib compression are supported; raw appended data and
//!   parallel piece files are rejected explicitly.
//!   Standalone OpenFOAM ASCII `volScalarField`/`volVectorField` files (`.foamfield`, `.foam-field`)
//!   render bounded uniform/nonuniform statistics and boundary patch counts without a solver.
//!   Reverse packaging to Gmsh boundary meshes and VTK PolyData via [`simulation::writer`].
//! - **ASAM OpenDRIVE** ([`opendrive`]): bounded `.xodr` road-reference-line previews for
//!   straight and constant-curvature plan-view geometry; lanes, signals, objects, profiles,
//!   links and simulation behavior remain inert.
//! - **ASAM OpenCRG** ([`opencrg`]): bounded `.crg` clear-text header/section previews;
//!   road-surface samples, binary payloads and linked files remain inert.
//! - **ASAM OpenSCENARIO XML** ([`openscenario`]): bounded `.xosc` scenario-structure
//!   previews for FileHeader, entities and storyboard hierarchy; catalogs, controllers,
//!   expressions and simulator behavior remain inert.

pub mod opencrg;
pub mod opendrive;
pub mod openscenario;

/// Default number of line segments used to tessellate circles and arcs in CAD
/// output formats.  Callers that need tighter tolerances should increase this.
pub const DEFAULT_CIRCLE_SEGMENTS: usize = 32;

/// Return the next evenly spaced sample index after `selected_count` items
/// have already been selected from a sequence of `total` items.
pub(crate) fn next_sample_index(selected_count: usize, total: usize, quota: usize) -> usize {
    if quota == 0 {
        return total;
    }
    ((selected_count as u128 * total as u128) / quota as u128) as usize
}

/// Allocate a deterministic global point budget proportionally across nonempty scans.
pub(crate) fn allocate_sample_quotas(point_counts: &[usize], maximum: usize) -> Vec<usize> {
    let total = point_counts
        .iter()
        .copied()
        .fold(0usize, usize::saturating_add);
    let active = point_counts.iter().filter(|count| **count > 0).count();
    let target = total.min(maximum);
    let mut quotas = vec![0; point_counts.len()];
    if target == 0 {
        return quotas;
    }
    if target < active {
        let mut remaining = target;
        for (quota, count) in quotas.iter_mut().zip(point_counts) {
            if *count > 0 && remaining > 0 {
                *quota = 1;
                remaining -= 1;
            }
        }
        return quotas;
    }

    for (quota, count) in quotas.iter_mut().zip(point_counts) {
        if *count > 0 {
            *quota = 1;
        }
    }
    let remaining_target = target.saturating_sub(active);
    let remaining_points = total.saturating_sub(active);
    let mut assigned = active;
    if remaining_target > 0 && remaining_points > 0 {
        for (quota, count) in quotas.iter_mut().zip(point_counts) {
            let weight = count.saturating_sub(1);
            let extra = ((remaining_target as u128 * weight as u128) / remaining_points as u128)
                .min(weight as u128) as usize;
            *quota += extra;
            assigned += extra;
        }
    }
    let mut remainder = target.saturating_sub(assigned);
    for (quota, count) in quotas.iter_mut().zip(point_counts) {
        if remainder == 0 {
            break;
        }
        if *quota < *count {
            *quota += 1;
            remainder -= 1;
        }
    }
    quotas
}

#[cfg(test)]
mod sample_quota_tests {
    use super::allocate_sample_quotas;

    #[test]
    fn quotas_preserve_all_small_scans_and_cap_the_total() {
        let quotas = allocate_sample_quotas(&[10, 90], 50);
        assert_eq!(quotas, [6, 44]);
        assert_eq!(quotas.iter().sum::<usize>(), 50);
    }

    #[test]
    fn empty_scans_receive_no_quota() {
        assert_eq!(allocate_sample_quotas(&[0, 5, 100], 10), [0, 2, 8]);
    }
}

pub(crate) mod amf;
pub(crate) mod collada;
pub mod color;
pub(crate) mod dwg;
pub mod dxf;
pub(crate) mod e57;
pub mod eagle;
pub mod excellon;
pub mod gcode;
pub mod gerber;
pub(crate) mod gltf;
pub mod hpgl;
pub(crate) mod ifc;
pub mod iges;
pub mod kicad;
pub mod kicad_sch;
pub mod kicad_sch_modern;
pub(crate) mod las;
pub mod ltspice;
pub(crate) mod nurbs;
pub mod obj;
pub(crate) mod off;
pub(crate) mod openfoam_field;
pub(crate) mod pcd;
pub mod ply;
pub(crate) mod pts;
pub(crate) mod ptx;
pub(crate) mod sat;
pub mod simulation;
pub mod spice;
pub mod step;
pub mod stl;
pub mod svg_reader;
pub mod threemf;
pub(crate) mod vrml;
pub(crate) mod x3d;
pub(crate) mod xyz;

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};

/// Converts a CAD file at `path` to SVG pages through the provided `sink`.
pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let extension = path
        .extension()
        .and_then(|v| v.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    match extension.as_str() {
        "dae" => return collada::convert(path, options, sink),
        "x3d" => return x3d::convert(path, options, sink),
        "dxf" => {
            let mut probe = File::open(path)?;
            let mut signature = [0u8; 22];
            if probe.read_exact(&mut signature).is_ok()
                && signature == *dxf::binary::BINARY_DXF_SENTINEL
            {
                return dxf::binary::convert(File::open(path)?, options, sink);
            }
        }
        "gltf" | "glb" => return gltf::convert(path, options, sink),
        "3mf" => return threemf::convert(path, options, sink),
        "iges" | "igs" => return iges::convert(path, options, sink),
        "ptx" => return ptx::convert(File::open(path)?, options, sink),
        "pts" => return pts::convert(File::open(path)?, options, sink),
        "xyz" => return xyz::convert(File::open(path)?, options, sink),
        "e57" => return e57::convert(path, options, sink),
        "las" | "laz" => return las::convert(path, options, sink),
        _ => {}
    }

    let file = File::open(path)?;
    let reader = BufReader::new(file);

    match extension.as_str() {
        "dxf" => dxf::convert(reader, options, sink),
        "gbr" | "gerber" | "gtl" | "gbl" | "gts" | "gbs" | "gto" | "gbo" | "gko" | "gm1"
        | "gm2" | "art" | "pho" | "cmp" | "sol" | "stc" | "sts" => {
            gerber::convert(reader, options, sink)
        }
        "plt" | "hpgl" | "hpg" | "gl2" => hpgl::convert(reader, options, sink),
        "gcode" | "nc" | "ngc" | "tap" | "gco" | "cnc" => gcode::convert(reader, options, sink),
        "drl" | "drd" | "xln" | "exc" => excellon::convert(reader, options, sink),
        "stl" => stl::convert(reader, options, sink),
        "pcd" => pcd::convert(reader, options, sink),
        "ply" => ply::convert(reader, options, sink),
        "step" | "stp" | "p21" => step::convert(reader, options, sink),
        "obj" => obj::convert(reader, options, sink),
        "meshb" => simulation::convert_medit(reader, options, sink),
        "msh" | "vtk" | "vtu" | "vtp" | "vti" | "vtr" | "vts" => {
            simulation::convert(reader, options, sink)
        }
        "unv" => simulation::convert_unv(reader, options, sink),
        _ => {
            // Content sniffing fallback for CAD files
            use std::io::Read;
            let mut check_file = File::open(path)?;
            let mut buf = vec![0u8; 1024];
            let n = check_file.read(&mut buf).unwrap_or(0);
            buf.truncate(n);

            if buf.starts_with(dxf::binary::BINARY_DXF_SENTINEL) {
                dxf::binary::convert(File::open(path)?, options, sink)
            } else if buf.starts_with(b"LASF") {
                las::convert(path, options, sink)
            } else if buf.starts_with(b"ISO-10303-21;")
                || buf.windows(13).any(|w| w == b"ISO-10303-21;")
            {
                step::convert(reader, options, sink)
            } else if buf.starts_with(b"0\nSECTION")
                || buf.starts_with(b"  0\r\nSECTION")
                || buf.starts_with(b"  0\nSECTION")
                || buf.starts_with(b"0\r\nSECTION")
            {
                dxf::convert(reader, options, sink)
            } else if buf.starts_with(b"%FSLA")
                || buf.starts_with(b"%MO")
                || buf.starts_with(b"G04")
            {
                gerber::convert(reader, options, sink)
            } else if buf.starts_with(b"IN;")
                || buf.starts_with(b"SP")
                || buf.starts_with(b"PU")
                || buf.starts_with(b"PD")
            {
                hpgl::convert(reader, options, sink)
            } else if buf.starts_with(b"M48")
                || buf.starts_with(b"T01")
                || buf.starts_with(b"FMAT,2")
            {
                excellon::convert(reader, options, sink)
            } else if buf.starts_with(b"G0 ")
                || buf.starts_with(b"G1 ")
                || buf.starts_with(b"G21")
                || buf.starts_with(b"G90")
                || buf.starts_with(b"(Generated")
            {
                gcode::convert(reader, options, sink)
            } else if buf.starts_with(b"solid ") {
                stl::convert(reader, options, sink)
            } else if collada::looks_like_prefix(&buf) {
                collada::convert(path, options, sink)
            } else if x3d::looks_like_prefix(&buf) {
                x3d::convert(path, options, sink)
            } else if buf.starts_with(b"ply\n") || buf.starts_with(b"ply\r\n") {
                ply::convert(reader, options, sink)
            } else if buf.starts_with(b"$MeshFormat")
                || buf.starts_with(b"# vtk DataFile Version")
                || buf.starts_with(b"<VTKFile")
            {
                simulation::convert(reader, options, sink)
            } else if simulation::looks_like_unv_prefix(&buf) {
                simulation::convert_unv(reader, options, sink)
            } else {
                Err(Error::Unsupported(format!(
                    "unsupported CAD format extension '.{extension}'"
                )))
            }
        }
    }
}
