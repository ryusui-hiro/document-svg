"""Python API for the Rust-powered document-svg converter."""

from __future__ import annotations

import json
from os import PathLike
from typing import TypedDict

from ._native import _convert_json, _reverse_json


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
) -> ConversionReport:
    """Convert a PDF/PPTX/XLSX/DOCX file into SVG pages.

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
    )
    return json.loads(report)


def reverse(
    input_path: str | PathLike[str],
    output_path: str | PathLike[str],
    *,
    max_input_bytes: int | None = None,
    max_pages: int | None = None,
) -> ReverseReport:
    """Package one SVG or a directory of SVG pages as PPTX, DOCX, or XLSX.

    SVG pages remain vector images. Original Office paragraphs, cells, formulas,
    charts, and other semantic structures are not reconstructed.
    """

    report = _reverse_json(
        input_path,
        output_path,
        max_input_bytes=max_input_bytes,
        max_pages=max_pages,
    )
    return json.loads(report)


__all__ = ["ConversionReport", "PageReport", "ReverseReport", "convert", "reverse"]
