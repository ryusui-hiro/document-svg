const assert = require('node:assert/strict')
const { access, mkdir, mkdtemp, writeFile } = require('node:fs/promises')
const os = require('node:os')
const path = require('node:path')
const test = require('node:test')

const { convert, preview, reverse } = require('../index.js')

function minimalPdf() {
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << >> /Contents 4 0 R >>',
    '<< /Length 0 >>\nstream\n\nendstream',
  ]
  let pdf = '%PDF-1.4\n'
  const offsets = []
  for (const [index, body] of objects.entries()) {
    offsets.push(Buffer.byteLength(pdf))
    pdf += `${index + 1} 0 obj\n${body}\nendobj\n`
  }
  const xrefOffset = Buffer.byteLength(pdf)
  pdf += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`
  for (const offset of offsets) {
    pdf += `${String(offset).padStart(10, '0')} 00000 n \n`
  }
  pdf += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\n`
  pdf += `startxref\n${xrefOffset}\n%%EOF\n`
  return pdf
}

function crc32(buffer) {
  let crc = 0xffffffff
  for (const byte of buffer) {
    crc ^= byte
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0)
    }
  }
  return (crc ^ 0xffffffff) >>> 0
}

function uncompressedZip(entries) {
  const localParts = []
  const centralParts = []
  let offset = 0

  for (const [name, value] of entries) {
    const nameBytes = Buffer.from(name)
    const content = Buffer.from(value)
    const checksum = crc32(content)

    const localHeader = Buffer.alloc(30)
    localHeader.writeUInt32LE(0x04034b50, 0)
    localHeader.writeUInt16LE(20, 4)
    localHeader.writeUInt32LE(checksum, 14)
    localHeader.writeUInt32LE(content.length, 18)
    localHeader.writeUInt32LE(content.length, 22)
    localHeader.writeUInt16LE(nameBytes.length, 26)
    localParts.push(localHeader, nameBytes, content)

    const centralHeader = Buffer.alloc(46)
    centralHeader.writeUInt32LE(0x02014b50, 0)
    centralHeader.writeUInt16LE(20, 4)
    centralHeader.writeUInt16LE(20, 6)
    centralHeader.writeUInt32LE(checksum, 16)
    centralHeader.writeUInt32LE(content.length, 20)
    centralHeader.writeUInt32LE(content.length, 24)
    centralHeader.writeUInt16LE(nameBytes.length, 28)
    centralHeader.writeUInt32LE(offset, 42)
    centralParts.push(centralHeader, nameBytes)

    offset += localHeader.length + nameBytes.length + content.length
  }

  const centralSize = centralParts.reduce((size, part) => size + part.length, 0)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(entries.length, 8)
  end.writeUInt16LE(entries.length, 10)
  end.writeUInt32LE(centralSize, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...localParts, ...centralParts, end])
}

function minimalDocx() {
  return uncompressedZip([
    [
      'word/document.xml',
      '<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Hello DOCX Preview</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>',
    ],
  ])
}

test('converts a PDF and returns the JavaScript report shape', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'input.pdf')
  const output = path.join(temporary, 'out')
  await writeFile(input, minimalPdf())

  const report = await convert(input, output, { jobs: 1 })

  assert.equal(report.sourceFormat, 'pdf')
  assert.equal(report.pageCount, 1)
  await access(path.join(output, report.pages[0].svg))
})

test('previews an Office document as in-memory SVG markup', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'input.docx')
  await writeFile(input, minimalDocx())

  const report = await preview(input, { maxPages: 10 })

  assert.equal(report.sourceFormat, 'docx')
  assert.equal(report.pageCount, 1)
  assert.equal(report.needsReview, false)
  assert.equal('outputDirectory' in report, false)
  assert.match(report.pages[0].svg, /^<\?xml version="1\.0"/)
  assert.match(report.pages[0].svg, /Hello DOCX Preview/)
  assert.equal(report.pages[0].widthPoints, 612)
})

test('bounds SVG retained by the preview API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'input.docx')
  await writeFile(input, minimalDocx())

  await assert.rejects(preview(input, { maxSvgBytes: 10 }), /preview page 1/)
})

test('rejects missing input asynchronously', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))

  await assert.rejects(
    convert(path.join(temporary, 'missing.pdf'), path.join(temporary, 'out')),
    /I\/O error/,
  )
})

test('validates numeric options before scheduling conversion', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'input.pdf')
  await writeFile(input, '%PDF-invalid')

  assert.throws(
    () => convert(input, path.join(temporary, 'out'), { jobs: 1.5 }),
    /jobs must be a non-negative safe integer/,
  )
})

test('packages an SVG as PPTX', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'page.svg')
  const output = path.join(temporary, 'page.pptx')
  await writeFile(
    input,
    '<svg xmlns="http://www.w3.org/2000/svg" width="200pt" height="100pt"><text x="10" y="20">Hello</text></svg>',
  )

  const report = await reverse(input, output)

  assert.equal(report.outputFormat, 'pptx')
  assert.equal(report.pageCount, 1)
  await access(output)
})

const DRAWIO_DIAGRAM =
  '<mxfile><diagram id="p1" name="Flow"><mxGraphModel><root>' +
  '<mxCell id="0"/><mxCell id="1" parent="0"/>' +
  '<mxCell id="a" value="Start" style="ellipse;whiteSpace=wrap;html=1;" vertex="1" parent="1">' +
  '<mxGeometry x="20" y="20" width="120" height="60" as="geometry"/></mxCell>' +
  '</root></mxGraphModel></diagram></mxfile>'

test('converts drawio and keeps the diagram source when asked', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-drawio-'))
  const source = path.join(directory, 'diagram.drawio')
  await writeFile(source, DRAWIO_DIAGRAM, 'utf8')

  const plain = await convert(source, path.join(directory, 'plain'))
  assert.equal(plain.sourceFormat, 'drawio')
  assert.equal(plain.pageCount, 1)

  const embedded = await convert(source, path.join(directory, 'embedded'), {
    embedDrawioSource: true,
  })
  const pages = await preview(source, { embedDrawioSource: true })
  assert.match(pages.pages[0].svg, /content="&lt;mxfile/)
  assert.equal(embedded.pageCount, 1)

  // The embedded copy is what lets the round trip give back a diagram.
  const restored = path.join(directory, 'restored.drawio')
  const report = await reverse(path.join(directory, 'embedded'), restored)
  assert.equal(report.outputFormat, 'drawio')
  assert.match(report.warnings[0], /restored/)
})

test('draws a shape from a stencil library the caller supplies', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-stencil-'))
  const stencils = path.join(directory, 'stencils')
  await mkdir(stencils)
  await writeFile(
    path.join(stencils, 'demo.xml'),
    '<shapes name="mxgraph.demo"><shape name="Badge" h="10" w="10" aspect="fixed">' +
      '<connections/><background><ellipse x="0" y="0" w="10" h="10"/></background>' +
      '<foreground><fillstroke/></foreground></shape></shapes>',
    'utf8',
  )
  const source = path.join(directory, 'badge.drawio')
  await writeFile(
    source,
    '<mxfile><diagram name="Badge"><mxGraphModel><root>' +
      '<mxCell id="0"/><mxCell id="1" parent="0"/>' +
      '<mxCell id="b" style="shape=mxgraph.demo.badge;html=1;" vertex="1" parent="1">' +
      '<mxGeometry x="0" y="0" width="40" height="40" as="geometry"/></mxCell>' +
      '</root></mxGraphModel></diagram></mxfile>',
    'utf8',
  )

  const without = await convert(source, path.join(directory, 'without'))
  assert.ok(without.warnings.some((warning) => warning.includes('mxgraph.demo.badge')))

  const withLibrary = await convert(source, path.join(directory, 'with'), {
    stencilPaths: [stencils],
  })
  assert.deepEqual(withLibrary.warnings, [])
})
