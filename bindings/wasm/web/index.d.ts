export interface BrowserConvertOptions {
  /** Maximum compressed source size. Defaults to 64 MiB; hard maximum 256 MiB. */
  maxInputBytes?: number
  /** Maximum expanded PDF stream or ZIP part size. Defaults to 32 MiB. */
  maxZipEntryBytes?: number
  /** Maximum number of output pages. Defaults to 1,000. */
  maxPages?: number
  /** Maximum XML parser events. Defaults to 2,000,000. */
  maxXmlEvents?: number
  includeMetadata?: boolean
  /** SVG coordinate precision from 1 to 12. Defaults to 4. */
  precision?: number
  /** PDF font outlines can improve appearance while removing searchable text. */
  outlineEmbeddedPdfText?: boolean
  /** Maximum SVG bytes for one page. Defaults to 16 MiB. */
  maxPageSvgBytes?: number
  /** Maximum combined SVG bytes. Defaults to 128 MiB. */
  maxTotalSvgBytes?: number
  /** Maximum searchable text spans per page. Defaults to 5,000. */
  maxPageTextSpans?: number
  /** Maximum searchable text spans across the document. Defaults to 50,000. */
  maxTotalTextSpans?: number
  /** Maximum searchable text bytes across the document. Defaults to 8 MiB. */
  maxTotalTextBytes?: number
}

export interface BrowserSvgPage {
  number: number
  svg: string
  widthPoints: number
  heightPoints: number
  nodeCount: number
  warningCount: number
  warnings: string[]
  estimatedIrBytes: number
  textSpans: BrowserTextSpan[]
}

export interface BrowserTextSpan {
  text: string
  x: number
  y: number
  width: number
  height: number
  fontSize: number
  /** SVG affine matrix [a, b, c, d, e, f]. */
  transform: [number, number, number, number, number, number]
}

export interface BrowserConversionReport {
  converter: string
  version: string
  source: string
  sourceFormat: 'pdf' | 'docx' | 'xlsx' | 'pptx'
  elapsedMs: number
  inputBytes: number
  pageCount: number
  largestPageIrBytes: number
  warnings: string[]
}

export type PageCallback = (page: BrowserSvgPage) => void

/**
 * Synchronous conversion entry point exported by the generated WASM module.
 * Always call from a Worker; PDF and Office parsing are CPU intensive.
 */
export declare function convert_document(
  fileName: string,
  bytes: Uint8Array,
  options: BrowserConvertOptions,
  onPage: PageCallback,
): BrowserConversionReport
