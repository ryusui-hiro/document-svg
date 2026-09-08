'use strict'

function assertSvg(svg, depth = 0) {
  if (depth > 4 || (typeof svg === 'string' && svg.length > 64 * 1024 * 1024)) {
    throw new TypeError('SVG preview size or nesting limit exceeded')
  }
  if (typeof svg !== 'string' || !/<svg(?:\s|>)/i.test(svg)) {
    throw new TypeError('svg must be a complete SVG markup string')
  }
  if (
    /<!DOCTYPE|<!ENTITY|<\s*(?:[\w.-]+:)?(?:script|foreignObject|iframe|object|embed|animate|animateMotion|animateTransform|set|discard)\b/i.test(svg) ||
    /\son[a-z]+\s*=/i.test(svg) ||
    /\b(?:href|xlink:href)\s*=\s*['"]\s*(?:https?:|javascript:|file:|blob:)/i.test(svg) ||
    /@import\b|url\(\s*['"]?\s*(?:https?:|javascript:|file:|blob:)/i.test(svg)
  ) {
    throw new TypeError('svg must not contain executable or external content')
  }
  const allowedReference = (value) => {
    if (/^\s*#[a-z0-9_.:-]+\s*$/i.test(value) ||
      /^\s*data:image\/(?:png|jpeg|gif|webp);base64,[a-z0-9+/=\s]*$/i.test(value)) return true
    if (/^\s*data:image\/svg\+xml;base64,[a-z0-9+/=\s]+$/i.test(value)) {
      try {
        const bytes = Uint8Array.from(atob(value.split(',')[1]), (c) => c.charCodeAt(0))
        assertSvg(new TextDecoder('utf-8', { fatal: true }).decode(bytes), depth + 1)
        return true
      } catch { return false }
    }
    return false
  }
  for (const match of svg.matchAll(/\b(?:[\w.-]+:)?href\s*=\s*(['"])(.*?)\1/gis)) {
    if (!allowedReference(match[2])) {
      throw new TypeError('svg must not contain executable or external content')
    }
  }
  for (const match of svg.matchAll(/url\s*\(\s*(['"]?)(.*?)\1\s*\)/gis)) {
    if (!/^#[a-z0-9_.:-]+$/i.test(match[2].trim())) {
      throw new TypeError('svg must not contain executable or external content')
    }
  }
  if (/xml:base\s*=|<\?xml-stylesheet\b/i.test(svg)) {
    throw new TypeError('svg must not contain executable or external content')
  }
  for (const match of svg.matchAll(/\bstyle\s*=\s*(['"])(.*?)\1|<style\b[^>]*>(.*?)<\/style\s*>/gis)) {
    if (/[\\&@]|\/\*/.test(match[2] ?? match[3])) {
      throw new TypeError('svg must not contain executable or external content')
    }
  }
}

function browserGlobal(name) {
  const value = globalThis[name]
  if (value == null) {
    throw new Error(`${name} is not available in this environment`)
  }
  return value
}

/**
 * Create an object URL suitable for an <img> src. Revoke it when the preview
 * is replaced or unmounted.
 */
function createSvgPreviewUrl(svg) {
  assertSvg(svg)
  const BlobConstructor = browserGlobal('Blob')
  const Url = browserGlobal('URL')
  if (typeof Url.createObjectURL !== 'function') {
    throw new Error('URL.createObjectURL is not available in this environment')
  }
  return Url.createObjectURL(
    new BlobConstructor([svg], { type: 'image/svg+xml;charset=utf-8' }),
  )
}

/**
 * Create a self-contained data URL for renderers where blob URLs cannot cross
 * process or Markdown boundaries. The SVG is validated before encoding.
 */
function createSvgPreviewDataUrl(svg) {
  assertSvg(svg)
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
}

/** Revoke an object URL previously returned by createSvgPreviewUrl(). */
function revokeSvgPreviewUrl(url) {
  const Url = browserGlobal('URL')
  if (typeof Url.revokeObjectURL !== 'function') {
    throw new Error('URL.revokeObjectURL is not available in this environment')
  }
  Url.revokeObjectURL(url)
}

function legacyCopyText(text) {
  const documentObject = browserGlobal('document')
  const textarea = documentObject.createElement('textarea')
  textarea.value = text
  textarea.setAttribute('readonly', '')
  textarea.style.position = 'fixed'
  textarea.style.opacity = '0'
  documentObject.body.appendChild(textarea)
  textarea.select()
  try {
    if (!documentObject.execCommand('copy')) {
      throw new Error('the browser rejected the clipboard copy command')
    }
  } finally {
    textarea.remove()
  }
}

/** Copy the exact SVG XML source as plain text. */
async function copySvgSourceToClipboard(svg) {
  assertSvg(svg)
  const navigatorObject = globalThis.navigator
  if (navigatorObject?.clipboard?.writeText) {
    await navigatorObject.clipboard.writeText(svg)
    return
  }
  legacyCopyText(svg)
}

/**
 * Copy SVG using image/svg+xml when the Async Clipboard API explicitly
 * supports it. Otherwise copy the exact SVG XML as text/plain.
 *
 * Must be called from a user gesture such as a button click.
 */
async function copySvgToClipboard(svg) {
  assertSvg(svg)
  const navigatorObject = globalThis.navigator
  const ClipboardItemConstructor = globalThis.ClipboardItem
  const supportsSvg =
    typeof ClipboardItemConstructor === 'function' &&
    typeof ClipboardItemConstructor.supports === 'function' &&
    ClipboardItemConstructor.supports('image/svg+xml')

  if (supportsSvg && navigatorObject?.clipboard?.write) {
    const BlobConstructor = browserGlobal('Blob')
    try {
      await navigatorObject.clipboard.write([
        new ClipboardItemConstructor({
          'image/svg+xml': new BlobConstructor([svg], {
            type: 'image/svg+xml',
          }),
        }),
      ])
      return 'image/svg+xml'
    } catch (error) {
      if (!navigatorObject.clipboard.writeText) {
        throw error
      }
    }
  }

  await copySvgSourceToClipboard(svg)
  return 'text/plain'
}

module.exports = {
  copySvgSourceToClipboard,
  copySvgToClipboard,
  createSvgPreviewDataUrl,
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
}
