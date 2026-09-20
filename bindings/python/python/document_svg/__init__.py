"""Python API for the Rust-powered document-svg converter."""

from __future__ import annotations

import json
from os import PathLike
from typing import TypedDict

from ._native import _convert_json, _preview_json, _reverse_json, _transform_svg


class PageReport(TypedDict):
    number: int
    svg: str
    width_points: float
    height_points: float
    node_count: int
    warning_count: int
    warnings: list[str]
    estimated_ir_bytes: int


class ConversionReport(TypedDict):
    converter: str
    version: str
    source: str
    source_format: str
    output_directory: str
    elapsed_ms: int
    input_bytes: int
    page_count: int
    largest_page_ir_bytes: int
    pages: list[PageReport]
    warnings: list[str]


class ReverseReport(TypedDict):
    converter: str
    version: str
    source: str
    output: str
    output_format: str
    page_count: int
    input_bytes: int
    warnings: list[str]


class PreviewPage(TypedDict):
    number: int
    svg: str
    width_points: float
    height_points: float
    node_count: int
    warning_count: int
    warnings: list[str]
    estimated_ir_bytes: int


class PreviewReport(TypedDict):
    converter: str
    version: str
    source: str
    source_format: str
    elapsed_ms: int
    input_bytes: int
    page_count: int
    largest_page_ir_bytes: int
    pages: list[PreviewPage]
    warnings: list[str]
    needs_review: bool


def convert(
    input_path: str | PathLike[str],
    output_directory: str | PathLike[str],
    *,
    max_input_bytes: int | None = None,
    max_zip_entry_bytes: int | None = None,
    max_pages: int | None = None,
    max_xml_events: int | None = None,
    include_metadata: bool | None = None,
    precision: int | None = None,
    jobs: int | None = None,
    outline_embedded_pdf_text: bool | None = None,
    embed_drawio_source: bool | None = None,
    stencil_paths: list[str] | None = None,
) -> ConversionReport:
    """Convert documents and technical formats into SVG pages written to disk.

    The conversion runs outside the Python GIL. Files are written to
    ``output_directory`` and the conversion manifest is returned as a dict.
    """

    report = _convert_json(
        input_path,
        output_directory,
        max_input_bytes=max_input_bytes,
        max_zip_entry_bytes=max_zip_entry_bytes,
        max_pages=max_pages,
        max_xml_events=max_xml_events,
        include_metadata=include_metadata,
        precision=precision,
        jobs=jobs,
        outline_embedded_pdf_text=outline_embedded_pdf_text,
        embed_drawio_source=embed_drawio_source,
        stencil_paths=stencil_paths,
    )
    return json.loads(report)


def reverse(
    input_path: str | PathLike[str],
    output_path: str | PathLike[str],
    *,
    max_input_bytes: int | None = None,
    max_pages: int | None = None,
) -> ReverseReport:
    """Package one SVG or a directory of SVG pages as Office, draw.io, CAD/CAM/3D, code, or image formats.

    SVG pages remain vector drawings. Original application semantic structures are not reconstructed.
    """

    report = _reverse_json(
        input_path,
        output_path,
        max_input_bytes=max_input_bytes,
        max_pages=max_pages,
    )
    return json.loads(report)


def preview(
    input_path: str | PathLike[str],
    *,
    max_svg_bytes: int | None = None,
    max_total_svg_bytes: int | None = None,
    max_input_bytes: int | None = None,
    max_zip_entry_bytes: int | None = None,
    max_pages: int | None = None,
    max_xml_events: int | None = None,
    include_metadata: bool | None = None,
    precision: int | None = None,
    jobs: int | None = None,
    outline_embedded_pdf_text: bool | None = None,
    embed_drawio_source: bool | None = None,
    stencil_paths: list[str] | None = None,
) -> PreviewReport:
    """Convert a supported document and return complete SVG page markup in memory for UI preview.

    Does not leave files on disk. Suitable for server-side rendering, API responses, and memory-constrained previews.
    """

    report = _preview_json(
        input_path,
        max_svg_bytes=max_svg_bytes,
        max_total_svg_bytes=max_total_svg_bytes,
        max_input_bytes=max_input_bytes,
        max_zip_entry_bytes=max_zip_entry_bytes,
        max_pages=max_pages,
        max_xml_events=max_xml_events,
        include_metadata=include_metadata,
        precision=precision,
        jobs=jobs,
        outline_embedded_pdf_text=outline_embedded_pdf_text,
        embed_drawio_source=embed_drawio_source,
        stencil_paths=stencil_paths,
    )
    return json.loads(report)


def transform(
    svg: str | bytes,
    *,
    minify: bool = False,
    monochrome: str | None = None,
    responsive: bool = False,
    precision: int | None = None,
    remove_metadata: bool = False,
    clean_paths: bool = False,
    strip_empty_groups: bool = False,
) -> str | bytes:
    """Transform, minify, recolor, scale, or sanitize an SVG document.

    If input is ``str``, returns transformed SVG as ``str``.
    If input is ``bytes``, returns transformed SVG as ``bytes``.
    """

    is_str = isinstance(svg, str)
    raw_bytes = svg.encode("utf-8") if is_str else bytes(svg)
    transformed_bytes = _transform_svg(
        raw_bytes,
        minify=minify,
        monochrome=monochrome,
        responsive=responsive,
        precision=precision,
        remove_metadata=remove_metadata,
        clean_paths=clean_paths,
        strip_empty_groups=strip_empty_groups,
    )
    if is_str:
        return bytes(transformed_bytes).decode("utf-8")
    return bytes(transformed_bytes)


__all__ = [
    "ConversionReport",
    "PageReport",
    "PreviewPage",
    "PreviewReport",
    "ReverseReport",
    "convert",
    "preview",
    "reverse",
    "transform",
]
