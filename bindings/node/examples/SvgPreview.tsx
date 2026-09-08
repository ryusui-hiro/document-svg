import { useEffect, useState } from "react"

import {
  copySvgToClipboard,
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
} from "document-svg/preview-ui"

export interface SvgPreviewProps {
  svg: string
  alt?: string
}

export function SvgPreview({
  svg,
  alt = "Office document preview",
}: SvgPreviewProps) {
  const [previewUrl, setPreviewUrl] = useState<string>()
  const [copyStatus, setCopyStatus] = useState("")

  useEffect(() => {
    const url = createSvgPreviewUrl(svg)
    setPreviewUrl(url)
    setCopyStatus("")
    return () => revokeSvgPreviewUrl(url)
  }, [svg])

  async function copySvg() {
    try {
      const format = await copySvgToClipboard(svg)
      setCopyStatus(
        format === "image/svg+xml"
          ? "SVG画像をコピーしました"
          : "SVGソースをコピーしました",
      )
    } catch (error) {
      setCopyStatus(
        error instanceof Error
          ? `コピーできませんでした: ${error.message}`
          : "コピーできませんでした",
      )
    }
  }

  return (
    <figure>
      {previewUrl && <img src={previewUrl} alt={alt} />}
      <figcaption>
        <button type="button" onClick={copySvg}>
          SVGをコピー
        </button>
        <span role="status" aria-live="polite">
          {copyStatus}
        </span>
      </figcaption>
    </figure>
  )
}
