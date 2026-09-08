const assert = require('node:assert/strict')
const test = require('node:test')

const {
  copySvgSourceToClipboard,
  copySvgToClipboard,
  createSvgPreviewDataUrl,
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
} = require('../preview-ui.js')

const SVG = '<svg xmlns="http://www.w3.org/2000/svg"><rect width="1" height="1"/></svg>'

function replaceGlobal(name, value) {
  const previous = Object.getOwnPropertyDescriptor(globalThis, name)
  Object.defineProperty(globalThis, name, {
    configurable: true,
    value,
    writable: true,
  })
  return () => {
    if (previous) {
      Object.defineProperty(globalThis, name, previous)
    } else {
      delete globalThis[name]
    }
  }
}

test('creates and revokes an SVG preview URL', () => {
  const url = createSvgPreviewUrl(SVG)
  assert.match(url, /^blob:/)
  revokeSvgPreviewUrl(url)
})

test('creates an exact self-contained SVG preview data URL', () => {
  const url = createSvgPreviewDataUrl(SVG)
  assert.match(url, /^data:image\/svg\+xml;charset=utf-8,/)
  assert.equal(decodeURIComponent(url.split(',', 2)[1]), SVG)
})

test('allows self-contained raster image data in SVG previews', () => {
  const svg = '<svg xmlns="http://www.w3.org/2000/svg"><image href="data:image/png;base64,AA=="/></svg>'
  assert.match(createSvgPreviewDataUrl(svg), /^data:image\/svg\+xml/)
})

test('rejects executable and external SVG preview content', () => {
  assert.throws(
    () => createSvgPreviewUrl('<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>'),
    /executable or external/,
  )
  assert.throws(
    () => createSvgPreviewDataUrl('<svg xmlns="http://www.w3.org/2000/svg"><image href="https://example.com/a.png"/></svg>'),
    /executable or external/,
  )
})

test('copies exact SVG source through writeText', async () => {
  let copied
  const restoreNavigator = replaceGlobal('navigator', {
    clipboard: {
      async writeText(value) {
        copied = value
      },
    },
  })
  try {
    await copySvgSourceToClipboard(SVG)
    assert.equal(copied, SVG)
  } finally {
    restoreNavigator()
  }
})

test('rejects relative, protocol-relative, encoded and active nested SVG references', () => {
  const nested = Buffer.from('<svg><script/></svg>').toString('base64')
  for (const href of ['//example.com/x', '../secret', 'https&#58;//example.com/x', `data:image/svg+xml;base64,${nested}`]) {
    assert.throws(() => createSvgPreviewDataUrl(`<svg><image href="${href}"/></svg>`), /executable or external/)
  }
  assert.throws(() => createSvgPreviewDataUrl('<svg><x:script xmlns:x="http://www.w3.org/2000/svg"/></svg>'), /executable or external/)
})

test('accepts passive nested SVG and rejects escaped CSS', () => {
  const nested = Buffer.from('<svg><rect width="1"/></svg>').toString('base64')
  assert.match(createSvgPreviewDataUrl(`<svg><image href="data:image/svg+xml;base64,${nested}"/></svg>`), /^data:/)
  assert.throws(() => createSvgPreviewDataUrl('<svg><rect style="fill:u\\72l(https://example.com/a)"/></svg>'), /executable or external/)
})

test('copies SVG MIME data when supported by the browser', async () => {
  let clipboardItems
  class FakeClipboardItem {
    static supports(type) {
      return type === 'image/svg+xml'
    }

    constructor(items) {
      this.items = items
    }
  }
  const restoreClipboardItem = replaceGlobal('ClipboardItem', FakeClipboardItem)
  const restoreNavigator = replaceGlobal('navigator', {
    clipboard: {
      async write(items) {
        clipboardItems = items
      },
    },
  })
  try {
    const format = await copySvgToClipboard(SVG)
    assert.equal(format, 'image/svg+xml')
    assert.equal(clipboardItems.length, 1)
    assert.equal(clipboardItems[0].items['image/svg+xml'].type, 'image/svg+xml')
  } finally {
    restoreNavigator()
    restoreClipboardItem()
  }
})

test('falls back to text when SVG clipboard MIME is unsupported', async () => {
  let copied
  class FakeClipboardItem {
    static supports() {
      return false
    }
  }
  const restoreClipboardItem = replaceGlobal('ClipboardItem', FakeClipboardItem)
  const restoreNavigator = replaceGlobal('navigator', {
    clipboard: {
      async writeText(value) {
        copied = value
      },
    },
  })
  try {
    const format = await copySvgToClipboard(SVG)
    assert.equal(format, 'text/plain')
    assert.equal(copied, SVG)
  } finally {
    restoreNavigator()
    restoreClipboardItem()
  }
})

test('rejects non-SVG input before accessing the clipboard', async () => {
  await assert.rejects(copySvgToClipboard('<div>not svg</div>'), /complete SVG/)
})
