'use strict'

const { writeFile } = require('node:fs/promises')
const { resolve } = require('node:path')

const { preview } = require('document-svg')
const { createSvgPreviewDataUrl } = require('document-svg/preview-ui')

async function main() {
  const input = process.argv[2]
  const output = resolve(process.argv[3] || 'document-preview.html')
  if (!input) {
    throw new Error(
      'Usage: node examples/preview-to-html.cjs INPUT.pdf|pptx|xlsx|docx [OUTPUT.html]',
    )
  }

  const report = await preview(resolve(input), {
    maxPages: 100,
    maxSvgBytes: 64 * 1024 * 1024,
    maxTotalSvgBytes: 256 * 1024 * 1024,
  })

  const pages = report.pages
    .map((page) => {
      const url = createSvgPreviewDataUrl(page.svg)
      return `<figure><img src="${url}" alt="Page ${page.number}"><figcaption>Page ${page.number}</figcaption></figure>`
    })
    .join('\n')

  const warnings = report.warnings.length
    ? `<pre>${escapeHtml(report.warnings.join('\n'))}</pre>`
    : '<p>No conversion warnings.</p>'

  const html = `<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Document SVG preview</title>
<style>
  body { margin: 0 auto; max-width: 72rem; padding: 2rem; background: #eee; font: 16px system-ui; }
  header, figure { background: white; padding: 1rem; border-radius: .5rem; }
  figure { margin: 1rem 0; box-shadow: 0 2px 12px #0002; }
  img { display: block; width: 100%; height: auto; }
  figcaption { margin-top: .5rem; color: #555; }
</style>
<header><h1>${escapeHtml(report.sourceFormat.toUpperCase())} preview</h1>${warnings}</header>
<main>${pages}</main>
</html>`

  await writeFile(output, html, { encoding: 'utf8', flag: 'wx' })
  console.log(`Wrote ${report.pageCount} page(s) to ${output}`)
  if (report.needsReview) process.exitCode = 2
}

function escapeHtml(value) {
  return value.replace(/[&<>"']/g, (character) => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  })[character])
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error)
  process.exitCode = 1
})
