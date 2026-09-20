const assert = require('node:assert/strict')
const { access, mkdir, mkdtemp, readFile, rm, writeFile } = require('node:fs/promises')
const os = require('node:os')
const path = require('node:path')
const test = require('node:test')
const { deflateSync, gzipSync } = require('node:zlib')

const { convert, preview, reverse, transform } = require('../index.js')

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

function minimalTiff() {
  const entries = [
    [256, 3, 2], // ImageWidth
    [257, 3, 1], // ImageLength
    [258, 3, 8], // BitsPerSample
    [259, 3, 1], // Compression: none
    [262, 3, 1], // PhotometricInterpretation: black is zero
    [273, 4, 122], // StripOffsets
    [277, 3, 1], // SamplesPerPixel
    [278, 4, 1], // RowsPerStrip
    [279, 4, 2], // StripByteCounts
  ]
  const bytes = Buffer.alloc(124)
  bytes.write('II', 0, 'ascii')
  bytes.writeUInt16LE(42, 2)
  bytes.writeUInt32LE(8, 4)
  bytes.writeUInt16LE(entries.length, 8)
  for (const [index, [tag, type, value]] of entries.entries()) {
    const offset = 10 + index * 12
    bytes.writeUInt16LE(tag, offset)
    bytes.writeUInt16LE(type, offset + 2)
    bytes.writeUInt32LE(1, offset + 4)
    if (type === 3) bytes.writeUInt16LE(value, offset + 8)
    else bytes.writeUInt32LE(value, offset + 8)
  }
  bytes.writeUInt32LE(0, 10 + entries.length * 12)
  bytes.set([32, 224], 122)
  return bytes
}

function minimalPng(red, green, blue) {
  const chunk = (type, content) => {
    const typeBytes = Buffer.from(type, 'ascii')
    const bytes = Buffer.alloc(12 + content.length)
    bytes.writeUInt32BE(content.length, 0)
    typeBytes.copy(bytes, 4)
    content.copy(bytes, 8)
    bytes.writeUInt32BE(crc32(Buffer.concat([typeBytes, content])), 8 + content.length)
    return bytes
  }
  const header = Buffer.alloc(13)
  header.writeUInt32BE(1, 0)
  header.writeUInt32BE(1, 4)
  header[8] = 8
  header[9] = 2
  const compressedPixel = deflateSync(Buffer.from([0, red, green, blue]))
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', header),
    chunk('IDAT', compressedPixel),
    chunk('IEND', Buffer.alloc(0)),
  ])
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

function minimalOdt() {
  return uncompressedZip([
    ['mimetype', 'application/vnd.oasis.opendocument.text'],
    [
      'content.xml',
      '<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:text><text:h text:outline-level="1">ODT preview</text:h><text:p>OpenDocument Text from Node.</text:p></office:text></office:body></office:document-content>',
    ],
  ])
}

function minimalOds() {
  return uncompressedZip([
    ['mimetype', 'application/vnd.oasis.opendocument.spreadsheet'],
    [
      'content.xml',
      '<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:spreadsheet><table:table table:name="Quarterly"><table:table-row><table:table-cell office:value-type="string"><text:p>Metric</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Value</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell office:value-type="string"><text:p>Requests</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="1200"><text:p>1200</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>',
    ],
  ])
}

function minimalFodp() {
  return '<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0"><office:automatic-styles><style:page-layout style:name="wide"><style:page-layout-properties fo:page-width="20cm" fo:page-height="11.25cm"/></style:page-layout></office:automatic-styles><office:body><office:presentation><draw:page draw:name="Node slide"><draw:rect svg:x="1cm" svg:y="1cm" svg:width="12cm" svg:height="4cm"><text:p>OpenDocument presentation from Node.</text:p></draw:rect></draw:page></office:presentation></office:body></office:document>'
}

function minimalFodg() {
  return '<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:drawing><draw:page draw:name="Node drawing"><draw:rect svg:x="1cm" svg:y="1cm" svg:width="8cm" svg:height="4cm"><text:p>OpenDocument drawing from Node.</text:p></draw:rect></draw:page></office:drawing></office:body></office:document-content>'
}

function minimalVsdx() {
  return uncompressedZip([
    ['_rels/.rels', '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/visio/2010/relationships/visioDocument" Target="visio/document.xml"/></Relationships>'],
    ['visio/document.xml', '<VisioDocument xmlns="urn:visio"/>'],
    ['visio/_rels/document.xml.rels', '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdPages" Type="http://schemas.microsoft.com/visio/2010/relationships/pages" Target="pages/pages.xml"/></Relationships>'],
    ['visio/pages/pages.xml', '<Pages xmlns="urn:visio" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><Page ID="0" NameU="Main" r:id="rId1"/></Pages>'],
    ['visio/pages/_rels/pages.xml.rels', '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/visio/2010/relationships/page" Target="page1.xml"/></Relationships>'],
    ['visio/pages/page1.xml', '<PageContents xmlns="urn:visio"><PageSheet><Cell N="PageWidth" V="5"/><Cell N="PageHeight" V="4"/></PageSheet><Shapes><Shape ID="1" NameU="Process" Type="Shape"><Cell N="PinX" V="2.5"/><Cell N="PinY" V="2"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Cell N="FillForegnd" V="#FF0000"/><Section N="Geometry"><Row T="RelMoveTo"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="1"/></Row></Section><Text>Visio &amp; Node</Text></Shape></Shapes></PageContents>'],
  ])
}

function minimalVdx() {
  return '<?xml version="1.0"?><VisioDocument xmlns="urn:visio"><Pages><Page ID="1" NameU="Legacy"><PageSheet><Cell N="PageWidth" V="4"/><Cell N="PageHeight" V="3"/></PageSheet><Shapes><Shape ID="2" NameU="Process" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" V="1.5"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Cell N="FillForegnd" V="#22AA44"/><Text>Legacy &amp; Node</Text></Shape></Shapes></Page></Pages></VisioDocument>'
}

function minimalRtf() {
  return String.raw`{\rtf1\ansi\ansicpg1252\uc1 R\'e9sum\'e9 \emdash  RTF from Node\par}`
}

function minimalJapaneseRtf() {
  return String.raw`{\rtf1\ansi\ansicpg932\uc1 \'82\'a0\par}`
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

test('previews PDF JPX embedded alpha through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_jpx_alpha.pdf')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'pdf')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.doesNotMatch(report.pages[0].svg, /data:image\/(?:jp2|j2c);base64,/)
})

test('previews PDF JPX images with external grayscale soft masks through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_jpx_soft_mask.pdf')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'pdf')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.doesNotMatch(report.pages[0].svg, /data:image\/(?:jp2|j2c);base64,/)
  assert.equal(report.warnings.length, 0)
})

test('previews PDF JPX SMaskInData=2 images with Matte unblending through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_jpx_smask_in_data_2.pdf')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'pdf')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.doesNotMatch(report.pages[0].svg, /data:image\/(?:jp2|j2c);base64,/)
  assert.equal(report.warnings.length, 0)
})

test('previews a legacy Word Binary document through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_legacy.doc')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'doc')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /document-svg sample/)
  assert.match(report.pages[0].svg, /Supported inputs/)
  assert.ok(report.warnings.some((warning) => warning.includes('layout')))
})

test('previews legacy PowerPoint Binary slide text through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_legacy.ppt')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ppt')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /document-svg/)
  assert.match(report.pages[1].svg, /One page in, one SVG out/)
  assert.ok(report.warnings.some((warning) => warning.includes('geometry')))
})

test('previews KiCad PCB S-expression boards through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.kicad_pcb')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'kicad_pcb')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /PCB DEMO/)
  assert.match(report.pages[0].svg, /R1/)
  assert.ok(report.warnings.some((warning) => warning.includes('2D artwork')))
})

test('previews IFC4 tessellated building geometry through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.ifc')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ifc')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /IFC tessellated building model/)
  assert.match(report.pages[0].svg, /data-semantic-role="obj:mesh"/)
  assert.ok(report.warnings.some((warning) => warning.includes('materials')))
})

test('previews reused IFC mapped geometry through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_mapped.ifc')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ifc')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /IFC tessellated building model/)
  assert.match(report.pages[0].svg, /data-semantic-role="obj:mesh"/)
  assert.ok(!report.warnings.some((warning) => warning.includes('mapped geometry')))
})

test('previews IFC extruded rectangle, circle, and arbitrary profiles through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_extruded.ifc')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ifc')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /IFC tessellated building model/)
  assert.match(report.pages[0].svg, /data-semantic-role="obj:mesh"/)
  assert.ok(report.warnings.some((warning) => warning.includes('materials')))
})

test('previews buildingSMART IFCXML through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_ifcxml.ifcxml')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ifcxml')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="obj:mesh"/)
  assert.ok(report.warnings.some((warning) => warning.includes('materials')))
})

test('previews a bounded IFCZIP archive through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../samples/source/sample.ifczip')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ifczip')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-source-format="ifczip"/)
  assert.equal((report.pages[0].svg.match(/<path id=""/g) || []).length, 24)
})

test('previews an SU2 CFD mesh through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.su2')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'su2')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="simulation:mesh"/)
  assert.match(report.pages[0].svg, /data-semantic-role="simulation:marker-boundary"/)
  assert.ok(report.warnings.some((warning) => warning.includes('marker name')))
})

test('previews an OpenFOAM polyMesh case through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/openfoam_case/sample.foam')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'openfoam')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="simulation:boundary-patch"/)
  assert.ok(report.warnings.some((warning) => warning.includes('patch names')))
})

test('previews gzip-compressed OpenFOAM ASCII polyMesh sidecars', async () => {
  const source = path.resolve(__dirname, '../../../tests/fixtures/openfoam_case')
  const temp = await mkdtemp(path.join(os.tmpdir(), 'docsvg-openfoam-gzip-'))
  const caseDir = path.join(temp, 'case')
  const polyMesh = path.join(caseDir, 'constant', 'polyMesh')
  await mkdir(polyMesh, { recursive: true })
  await writeFile(path.join(caseDir, 'case.foam'), await readFile(path.join(source, 'sample.foam')))
  for (const name of ['points', 'faces', 'owner', 'neighbour', 'boundary']) {
    const bytes = await readFile(path.join(source, 'constant', 'polyMesh', name))
    await writeFile(path.join(polyMesh, `${name}.gz`), gzipSync(bytes))
  }
  try {
    const report = await preview(path.join(caseDir, 'case.foam'))
    assert.equal(report.sourceFormat, 'openfoam')
    assert.equal(report.pageCount, 1)
    assert.match(report.pages[0].svg, /data-semantic-role="simulation:boundary-patch"/)
  } finally {
    await rm(temp, { recursive: true, force: true })
  }
})

test('previews a binary DXF drawing through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/binary_line.dxf')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'dxf')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-source-format="dxf"/)
  assert.match(report.pages[0].svg, /<path/)
})

test('previews a legacy Excel workbook through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/legacy_sample.xls')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'xls')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /sample — rows 1–4, columns A–B/)
  assert.match(report.pages[0].svg, /Alpha/)
})


test('previews an XLSB workbook through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/calamine_any_sheets.xlsb')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'xlsb')
  assert.equal(report.pageCount, 3)
  assert.match(report.pages[0].svg, /Visible — rows 1–5/)
  assert.match(report.pages[0].svg, /data-source-format="xlsb"/)
})

test('previews GeoJSON polygons, lines, and points through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.geojson')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'geojson')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /geojson:polygon/)
  assert.match(report.pages[0].svg, /geojson:line/)
  assert.match(report.pages[0].svg, /geojson:point/)
})

test('previews heterogeneous RFC 8142 GeoJSON Text Sequences through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.geojsons')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'geojsonseq')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /geojsonseq:point/)
  assert.match(report.pages[0].svg, /geojsonseq:line/)
  assert.match(report.pages[0].svg, /geojsonseq:polygon/)
  assert.doesNotMatch(report.pages[0].svg, /Private feature attribute/)
})

test('previews quantized TopoJSON shared arcs through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.topojson')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'topojson')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /topojson:polygon/)
  assert.match(report.pages[0].svg, /topojson:point/)
  assert.doesNotMatch(report.pages[0].svg, /private west|private station/)
})

test('previews generic RFC 7464 JSON Text Sequences through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.jsons')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'jsonseq')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /JSON sequence record 2/)
  assert.match(report.pages[0].svg, /validated/)
  assert.match(report.pages[0].svg, /42/)
})

test('previews ESRI Shapefile polygons and reads a matching projection/index', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_polygon.shp')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'shapefile')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Shapefile Map Preview/)
  assert.match(report.pages[0].svg, /shapefile:polygon/)
  assert.match(report.pages[0].svg, /1 features, 1 geometries, and 15 positions/)
})

test('previews GeoPackage vector layers through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_features.gpkg')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'geopackage')
  assert.equal(report.pageCount, 3)
  assert.match(report.pages[0].svg, /geopackage:polygon/)
  assert.match(report.pages[2].svg, /geopackage:point/)
  assert.ok(report.warnings.some((warning) => warning.includes('EPSG:3857')))
})

test('previews bounded GeoPackage raster tiles through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_tile_mixed.gpkg')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'geopackage')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[1].svg, /geopackage:tile/)
  assert.match(report.pages[1].svg, /data:image\/png;base64,/)
  assert.ok(report.pages[1].warnings.some((warning) => warning.includes('re-encoded as PNG')))
})

test('previews GeoRSS Simple RSS feed geometries through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.georss')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'georss')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /georss:polygon/)
  assert.match(report.pages[0].svg, /georss:line/)
  assert.match(report.pages[0].svg, /georss:point/)
  assert.match(report.pages[0].svg, /6 features, 6 geometries, and 19 positions/)
})

test('previews GML feature geometries through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.gml')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'gml')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /gml:polygon/)
  assert.match(report.pages[0].svg, /gml:line/)
  assert.match(report.pages[0].svg, /gml:point/)
  assert.match(report.pages[0].svg, /3 features, 3 geometries, and 14 positions/)
})

test('previews bounded WKT simple geometries through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.wkt')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'wkt')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /wkt:polygon/)
  assert.match(report.pages[0].svg, /wkt:line/)
  assert.match(report.pages[0].svg, /wkt:point/)
  assert.match(report.pages[0].svg, /1 features, 7 geometries, and 24 positions/)
})

test('previews GPX waypoints, routes, and tracks through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.gpx')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'gpx')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /gpx:point/)
  assert.match(report.pages[0].svg, /gpx:line/)
  assert.match(report.pages[0].svg, /3 features, 5 geometries, and 8 positions/)
})

test('previews KML and KMZ map features through the Node.js API', async () => {
  const directory = path.resolve(__dirname, '../../../tests/fixtures')
  const kml = await preview(path.join(directory, 'sample.kml'))
  assert.equal(kml.sourceFormat, 'kml')
  assert.equal(kml.pageCount, 1)
  assert.match(kml.pages[0].svg, /kml:polygon/)
  assert.match(kml.pages[0].svg, /kml:line/)
  assert.match(kml.pages[0].svg, /kml:point/)

  const kmz = await preview(path.join(directory, 'sample.kmz'))
  assert.equal(kmz.sourceFormat, 'kmz')
  assert.equal(kmz.pageCount, 1)
  assert.match(kmz.pages[0].svg, /kmz:point/)
})

test('previews a TIFF image directory through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'scan.tif')
  await writeFile(input, minimalTiff())

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'tiff')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
})

test('previews DICOM image frames without emitting patient metadata', async () => {
  const fixtureDirectory = path.resolve(__dirname, '../../../tests/fixtures')
  const report = await preview(path.join(fixtureDirectory, 'sample_multiframe.dcm'))

  assert.equal(report.sourceFormat, 'dicom')
  assert.equal(report.pageCount, 2)
  for (const page of report.pages) {
    assert.match(page.svg, /dicom:image-frame/)
    assert.match(page.svg, /data:image\/png;base64,/)
    assert.doesNotMatch(page.svg, /Doe\^DICOM QA/)
  }

  const rgb = await preview(path.join(fixtureDirectory, 'sample_rgb.dcm'))
  assert.equal(rgb.sourceFormat, 'dicom')
  assert.equal(rgb.pageCount, 1)
  assert.doesNotMatch(rgb.pages[0].svg, /Private\^Image/)

  const ct = await preview(path.join(fixtureDirectory, 'sample_ct_16bit.dcm'))
  assert.equal(ct.sourceFormat, 'dicom')
  assert.equal(ct.pageCount, 1)
  assert.match(ct.pages[0].svg, /data:image\/png;base64,/)
  assert.doesNotMatch(ct.pages[0].svg, /Private\^CT/)

  const implicit = await preview(path.join(fixtureDirectory, 'sample_implicit.dcm'))
  assert.equal(implicit.sourceFormat, 'dicom')
  assert.equal(implicit.pageCount, 1)

  const rle = await preview(path.join(fixtureDirectory, 'sample_rle.dcm'))
  assert.equal(rle.sourceFormat, 'dicom')
  assert.equal(rle.pageCount, 1)

  const jpeg2000 = await preview(path.join(fixtureDirectory, 'sample_jpeg2000.dcm'))
  assert.equal(jpeg2000.sourceFormat, 'dicom')
  assert.equal(jpeg2000.pageCount, 1)
  assert.match(jpeg2000.pages[0].svg, /data:image\/png;base64,/)

  const dicomdir = await preview(path.join(fixtureDirectory, 'dicom_set', 'DICOMDIR'))
  assert.equal(dicomdir.sourceFormat, 'dicomdir')
  assert.equal(dicomdir.pageCount, 3)
  assert.match(dicomdir.pages[2].svg, /data-source-format="dicomdir"/)
  assert.doesNotMatch(dicomdir.pages[2].svg, /Private\^DirectoryPatient/)
})

test('previews a DICOM encapsulated PDF without serializing DICOM attributes', async () => {
  const fixtureDirectory = path.resolve(__dirname, '../../../tests/fixtures')
  const report = await preview(path.join(fixtureDirectory, 'sample_encapsulated_pdf.dcm'))

  assert.equal(report.sourceFormat, 'dicom')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /DICOM Encapsulated PDF/)
  assert.match(report.pages[0].svg, /Synthetic radiology report/)
  assert.doesNotMatch(report.pages[0].svg, /Synthetic\^Patient/)
  assert.ok(report.pages[0].warnings.some((warning) => warning.includes('not de-identification')))
})

test('previews each V2000 SDF molecule as a chemical structure page', async () => {
  const fixtureDirectory = path.resolve(__dirname, '../../../tests/fixtures')
  const report = await preview(path.join(fixtureDirectory, 'sample_molecules.sdf'))

  assert.equal(report.sourceFormat, 'sdf')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /Synthetic Aromatic Ion/)
  assert.match(report.pages[0].svg, /chemical:bond/)
  assert.match(report.pages[0].svg, /N\+/)
  assert.match(report.pages[1].svg, /Synthetic Water/)
  assert.ok(report.pages[1].warnings.some((warning) => warning.includes('projected onto the XY plane')))
})

test('previews a V3000 molfile with nonsequential atom identifiers', async () => {
  const fixtureDirectory = path.resolve(__dirname, '../../../tests/fixtures')
  const report = await preview(path.join(fixtureDirectory, 'sample_molecule_v3000.mol'))

  assert.equal(report.sourceFormat, 'mol')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Synthetic V3000 Isotope/)
  assert.match(report.pages[0].svg, /18O-/)
  assert.match(report.pages[0].svg, /chemical:stereo-bond/)
  assert.ok(report.pages[0].warnings.some((warning) => warning.includes('Sgroups')))
})

test('previews a V2000 RXN file as a reactant-to-product diagram', async () => {
  const fixtureDirectory = path.resolve(__dirname, '../../../tests/fixtures')
  const report = await preview(path.join(fixtureDirectory, 'sample_reaction.rxn'))

  assert.equal(report.sourceFormat, 'rxn')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Reactants/)
  assert.match(report.pages[0].svg, /Products/)
  assert.match(report.pages[0].svg, /chemical:reaction-arrow/)
  assert.match(report.pages[0].svg, /Reactant carbonyl/)
  assert.match(report.pages[0].svg, /Product chloromethanol/)
})

test('previews a WebP raster through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.webp')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'raster')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /vectorized_path/)
})

test('previews the first frame of an animated GIF through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.gif')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'raster')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /vectorized_path/)
  assert.ok(report.warnings.some((warning) => warning.includes('first frame')))
})

test('previews CBZ page images through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'comic.cbz')
  await writeFile(input, uncompressedZip([
    ['page10.png', minimalPng(0, 0, 255)],
    ['page2.png', minimalPng(255, 0, 0)],
  ]))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'cbz')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
})

test('previews an Abaqus mesh deck through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'triangle.inp')
  await writeFile(input, [
    '*Heading',
    'Abaqus triangle',
    '*Node',
    '1, 0, 0, 0',
    '2, 1, 0, 0',
    '3, 0, 1, 0',
    '*Element, type=CPS3',
    '1, 1, 2, 3',
    '',
  ].join('\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'abaqus')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="simulation:mesh"/)
})

test('previews a Nastran Bulk Data mesh through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'triangle.bdf')
  await writeFile(input, [
    '$ Nastran triangle',
    'GRID 1 0 0 0 0',
    'GRID 2 0 1 0 0',
    'GRID 3 0 0 1 0',
    'CTRIA3 8 1 1 2 3',
    '',
  ].join('\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'nastran')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="simulation:mesh"/)
})

test('previews an LS-DYNA Keyword mesh through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'panel.k')
  await writeFile(input, [
    '*KEYWORD',
    '*TITLE',
    'LS-DYNA panel',
    '*ELEMENT_SHELL',
    '1,1,1,2,3,4',
    '*NODE',
    '1,0,0,0',
    '2,100,0,0',
    '3,100,50,0',
    '4,0,50,0',
    '*END',
  ].join('\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'lsdyna')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="simulation:mesh"/)
})

test('previews a Jupyter notebook through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'analysis.ipynb')
  await writeFile(input, JSON.stringify({
    nbformat: 4,
    nbformat_minor: 5,
    metadata: {},
    cells: [
      { cell_type: 'markdown', metadata: {}, source: ['# Notebook report\n', 'A short summary.'] },
      {
        cell_type: 'code', execution_count: 1, metadata: {}, source: ['print(2)'],
        outputs: [{ output_type: 'stream', name: 'stdout', text: ['2\n'] }],
      },
    ],
  }))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'jupyter')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Notebook report/)
  assert.match(report.pages[0].svg, /print\(2\)/)
})

test('previews Jupyter Markdown attachment images', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/jupyter_attachment.ipynb')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'jupyter')
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /aria-label="attached chart"/)
  assert.ok(report.warnings.some((warning) => warning.includes('attachment images')))
})

test('previews a bounded JATS article with a local figure', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-jats-'))
  const input = path.join(temporary, 'article.jats')
  const assets = path.join(temporary, 'assets')
  await mkdir(assets)
  await writeFile(input, await readFile(path.resolve(__dirname, '../../../tests/fixtures/sample.jats')))
  await writeFile(path.join(assets, 'red-blue.png'), await readFile(path.resolve(__dirname, '../../../tests/fixtures/assets/red-blue.png')))
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'jats')
  assert.match(report.pages[0].svg, /JATS Article Preview/)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /Ready/)
})

test('previews a bounded DocBook article with a local figure', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-docbook-'))
  const input = path.join(temporary, 'article.dbk')
  const assets = path.join(temporary, 'assets')
  await mkdir(assets)
  await writeFile(input, await readFile(path.resolve(__dirname, '../../../tests/fixtures/sample.docbook')))
  await writeFile(path.join(assets, 'red-blue.png'), await readFile(path.resolve(__dirname, '../../../tests/fixtures/assets/red-blue.png')))
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'docbook')
  assert.match(report.pages[0].svg, /DocBook Preview/)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /Ready/)
})

test('previews a DITA topic and local topic map', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-dita-'))
  await mkdir(path.join(temporary, 'assets'))
  await writeFile(path.join(temporary, 'guide.dita'), await readFile(path.resolve(__dirname, '../../../tests/fixtures/sample.dita')))
  await writeFile(path.join(temporary, 'guide.ditamap'), await readFile(path.resolve(__dirname, '../../../tests/fixtures/sample.ditamap')))
  await writeFile(path.join(temporary, 'second.dita'), await readFile(path.resolve(__dirname, '../../../tests/fixtures/second.dita')))
  await writeFile(path.join(temporary, 'assets', 'red-blue.png'), await readFile(path.resolve(__dirname, '../../../tests/fixtures/assets/red-blue.png')))
  const topic = await preview(path.join(temporary, 'guide.dita'))
  assert.equal(topic.sourceFormat, 'dita')
  assert.match(topic.pages[0].svg, /DITA Preview/)
  assert.match(topic.pages[0].svg, /data:image\/png;base64,/)
  const map = await preview(path.join(temporary, 'guide.ditamap'))
  assert.equal(map.sourceFormat, 'dita')
  assert.match(map.pages[0].svg, /DITA Guide Map/)
  assert.match(map.pages[0].svg, /Second Topic/)
})

test('previews PDB coordinate models as one page per MODEL', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-pdb-'))
  const input = path.join(temporary, 'models.pdb')
  await writeFile(input, await readFile(path.resolve(__dirname, '../../../tests/fixtures/sample.pdb')))
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'pdb')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /SAMPLE PDB PREVIEW/)
  assert.match(report.pages[0].svg, /chemical:bond/)
})

test('previews HWPX sections, tables, and package images', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.hwpx')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'hwpx')
  assert.match(report.pages[0].svg, /HWPX Preview/)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /Ready/)
})

test('previews COLLADA triangle geometry', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/triangle.dae')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'collada')
  assert.match(report.pages[0].svg, /obj:background/)
  assert.ok(report.warnings.some((warning) => warning.includes('materials')))
})

test('previews X3D indexed face geometry', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/triangle.x3d')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'x3d')
  assert.match(report.pages[0].svg, /obj:background/)
})

test('previews an XMind topic outline', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.xmind')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'xmind')
  assert.match(report.pages[0].svg, /XMind Preview/)
  assert.match(report.pages[0].svg, /Child topic/)
})

test('previews XMind content.json topics', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_json.xmind')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'xmind')
  assert.match(report.pages[0].svg, /JSON XMind Preview/)
  assert.match(report.pages[0].svg, /JSON Child/)
})

test('previews NIfTI scalar slices from .nii.gz', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.nii.gz')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'nifti')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /NIfTI slice 1/)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
})

test('previews FITS image planes from .fits.gz', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.fits.gz')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'fits')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /FITS image plane 1/)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
})

test('previews MRC density planes from .mrc.gz', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.mrc.gz')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'mrc')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /MRC plane 1/)
})

test('previews NetCDF classic dimensions and variables', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_netcdf.nc')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'netcdf')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /NetCDF CDF-1/)
  assert.match(report.pages[0].svg, /NetCDF demo/)
  assert.match(report.pages[0].svg, /20/)
})

test('previews standalone JPEG 2000 JP2 and raw codestreams', async () => {
  const jp2 = path.resolve(__dirname, '../../../tests/fixtures/sample_jpeg2000.jp2')
  const j2k = path.resolve(__dirname, '../../../tests/fixtures/sample_jpeg2000.j2k')
  const rgba = path.resolve(__dirname, '../../../tests/fixtures/sample_jpeg2000_rgba.jp2')
  for (const input of [jp2, j2k, rgba]) {
    const report = await preview(input)
    assert.equal(report.sourceFormat, 'jpeg2000')
    assert.equal(report.pageCount, 1)
    assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  }
})

test('previews XGMML nodes and edges', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.xgmml')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'xgmml')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Start order/)
  assert.match(report.pages[0].svg, /approve/)
})

test('previews SQLite user tables read-only', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.sqlite')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'sqlite')
  assert.match(report.pages[0].svg, /people/)
  assert.match(report.pages[0].svg, /Alice/)
})

test('previews CIF atom_site models', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.cif')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'cif')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /chemical:atom/)
})

test('previews Tripos MOL2 structure', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.mol2')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'mol2')
  assert.match(report.pages[0].svg, /chemical:bond/)
})

test('previews RDF Turtle statements', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.ttl')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'turtle')
  assert.match(report.pages[0].svg, /RDF statements/)
  assert.match(report.pages[0].svg, /Alice/)
})

test('previews bounded EPS vector paths', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/triangle.eps')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'eps')
  assert.match(report.pages[0].svg, /eps:path/)
})

test('previews Quarto Markdown without executing its code chunks', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'report.qmd')
  await writeFile(input, [
    '---',
    'title: "Quarto report"',
    'format: html',
    '---',
    '',
    '# Findings',
    '',
    '```{python}',
    'print(1 + 1)',
    '```',
  ].join('\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'quarto')
  assert.equal(report.pageCount, 1)
  assert.ok(report.needsReview)
  assert.match(report.pages[0].svg, /Quarto report/)
  assert.match(report.pages[0].svg, /print\(1 \+ 1\)/)
})

test('previews Quarto local and reference-style images safely', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-quarto-image-'))
  const input = path.join(temporary, 'report.qmd')
  const assets = path.join(temporary, 'assets')
  await mkdir(assets)
  await writeFile(input, await readFile(path.resolve(__dirname, '../../../tests/fixtures/quarto_image.qmd')))
  await writeFile( path.join(assets, 'red-blue.png'), await readFile(path.resolve(__dirname, '../../../tests/fixtures/assets/red-blue.png')))
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'quarto')
  assert.match(report.pages[0].svg, /aria-label="Local Quarto image"/)
  assert.match(report.pages[0].svg, /aria-label="Reference Quarto image"/)
  assert.doesNotMatch(report.pages[0].svg, /example\.invalid/)
})

test('previews an ASCII MEDIT mesh through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'surface.mesh')
  await writeFile(input, [
    'MeshVersionFormatted 2',
    'Dimension 2',
    'Vertices 3',
    '0 0 0',
    '100 0 0',
    '0 100 0',
    'Triangles 1',
    '1 2 3 1',
    'End',
  ].join('\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'medit')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="simulation:mesh"/)
})

test('previews a binary MEDIT meshb file through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/medit_binary_v2.meshb')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'medit')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /MEDIT binary finite-element mesh/)
  assert.match(report.pages[0].svg, /data-semantic-role="simulation:mesh"/)
})

test('previews an ASCII OFF polygon mesh through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'surface.off')
  await writeFile(input, [
    'OFF',
    '4 1 4',
    '0 0 0',
    '100 0 0',
    '100 100 0',
    '0 100 0',
    '4 0 1 2 3',
  ].join('\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'off')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="obj:mesh"/)
})

test('previews a vertex-only PLY point cloud through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'scan.ply')
  await writeFile(input, [
    'ply',
    'format ascii 1.0',
    'element vertex 4',
    'property float x',
    'property float y',
    'property float z',
    'end_header',
    '-10 0 0',
    '10 0 0',
    '0 -10 0',
    '0 10 0',
  ].join('\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ply')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="ply:point-cloud"/)
})

test('previews a PCD point cloud through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/point_cloud_ascii.pcd')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'pcd')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /PCD Point Cloud/)
  assert.match(report.pages[0].svg, /data-semantic-role="pcd:point-cloud"/)
  assert.match(report.pages[0].svg, /fill="#FF0000"/)
  assert.match(report.pages[0].svg, /fill="#00FF00"/)
})

test('previews an LAS point cloud through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_rgb.las')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'las')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /LAS\/LAZ Point Cloud/)
  assert.match(report.pages[0].svg, /data-semantic-role="las:point-cloud"/)
  assert.match(report.pages[0].svg, /fill="#FF0000"/)
})

test('previews multi-scan Leica PTX point clouds through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.ptx')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ptx')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /PTX scan 1/)
  assert.match(report.pages[0].svg, /data-semantic-role="ptx:point-cloud"/)
  assert.match(report.pages[0].svg, /fill="#2563EB"/)
  assert.match(report.pages[1].svg, /PTX scan 2/)
  assert.ok(report.pages[0].warnings.some((warning) => warning.includes('mark no color')))
  assert.ok(report.pages[1].warnings.every((warning) => !warning.includes('scan 1')))
  assert.ok(report.warnings.some((warning) => warning.includes('no-return')))
  assert.ok(report.warnings.some((warning) => warning.includes('mark no color')))
})

test('previews Leica PTS point clouds through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.pts')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'pts')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="pts:point-cloud"/)
  assert.match(report.pages[0].svg, /fill="#FF0000"/)
  assert.match(report.pages[0].svg, /fill="#2563EB"/)
  assert.ok(report.warnings.some((warning) => warning.includes('mark no color')))
})

test('previews a public ASTM E57 sample through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_e57_bunny.e57')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'e57')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="e57:point-cloud"/)
  assert.ok(report.pages[0].nodeCount > 0)
  assert.ok(report.warnings.some((warning) => warning.includes('image blobs')))
  assert.ok(report.pages[0].warnings.every((warning) => !warning.includes('invalid E57 colors')))
})

test('previews whitespace XYZ point clouds through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.xyz')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'xyz')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="xyz:point-cloud"/)
  assert.match(report.pages[0].svg, /fill="#000000"/)
  assert.match(report.pages[0].svg, /fill="#FF0000"/)

  const headerInput = path.resolve(__dirname, '../../../tests/fixtures/sample_xyz_header.xyz')
  const headerReport = await preview(headerInput)
  assert.equal(headerReport.sourceFormat, 'xyz')
  assert.match(headerReport.pages[0].svg, /fill="#00FF00"/)
  assert.ok(headerReport.warnings.some((warning) => warning.includes('unrecognized XYZ header')))
})

test('previews an ESRI ASCII Grid raster through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_elevation.asc')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'esri_ascii_grid')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="esri:ascii-grid-raster"/)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.ok(report.warnings.some((warning) => warning.includes('NODATA_VALUE')))
})

test('previews RFC 4180 quoted CSV cells through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-csv-'))
  const input = path.join(temporary, 'quoted.csv')
  await writeFile(input, 'Name,Note\r\n"Acme, Inc.","She said ""hello"""\r\n')

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'csv')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Acme, Inc\./)
  assert.match(report.pages[0].svg, /She said "hello"/)
})

test('previews Weka ARFF datasets through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.arff')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'arff')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /temperature \(numeric\)/)
  assert.match(report.pages[0].svg, /sunny/)
  assert.match(report.pages[0].svg, /18/)
})

test('previews JSON-LD nodes and named graphs through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.jsonld')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'json-ld')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('@context')))
  assert.match(report.pages[0].svg, /urn:book:1/)
  assert.match(report.pages[0].svg, /urn:graph:1/)
})

test('previews GraphML nodes and edges through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.graphml')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'graphml')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Start order/)
  assert.match(report.pages[0].svg, /approve/)
})

test('previews a dBASE III attribute table through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_attributes.dbf')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'dbf')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('deleted dBASE record')))
  assert.match(report.pages[0].svg, /Café Moreno/)
  assert.match(report.pages[0].svg, /AREA_HA/)
  assert.doesNotMatch(report.pages[0].svg, /Willow Farm/)
})

test('previews TOML configuration data without executing it through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_config.toml')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'toml')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /\$\.service\.name \(string\)/)
  assert.match(report.pages[0].svg, /\$\.targets\[1\]\.name/)
  assert.match(report.pages[0].svg, /ap-northeast-1/)
  assert.ok(report.warnings.some((warning) => warning.includes('float value(s) were normalized')))
})

test('previews YAML documents without expanding aliases or tags through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_config.yaml')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'yaml')
  assert.equal(report.pageCount, 2)
  const svg = report.pages.map((page) => page.svg).join('\n')
  assert.match(svg, /\$\.service\.name \(scalar\)/)
  assert.match(svg, /\$\.targets\[1\]\.name/)
  assert.match(svg, /Document 2/)
  assert.match(svg, /not expanded/)
  assert.ok(report.warnings.some((warning) => warning.includes('custom-tagged')))
})

test('previews generic XML configuration as inert path rows through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_config.xml')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'xml')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /namespace ns1 = "urn:example:application"/)
  assert.match(report.pages[0].svg, /\/ns1:application \(element\)/)
  assert.match(report.pages[0].svg, /catalog-api/)
  assert.match(report.pages[0].svg, /full-text &amp; faceted/)
})

test('previews Java Properties without evaluating placeholders through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_properties.properties')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'properties')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('duplicate')))
  assert.match(report.pages[0].svg, /Catalog API/)
  assert.match(report.pages[0].svg, /Café/)
  assert.match(report.pages[0].svg, /\$\{HOME\} is displayed as text/)
  assert.match(report.pages[0].svg, /release\.channel.*stable/)
  assert.doesNotMatch(report.pages[0].svg, /release\.channel.*old/)
})

test('previews BPMN 2.0 Diagram Interchange as a workflow through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_bpmn.bpmn')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'bpmn')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Order fulfillment/)
  assert.match(report.pages[0].svg, /Validate order/)
  assert.match(report.pages[0].svg, /bpmn:sequence-flow-arrow/)
  assert.deepEqual(report.warnings, [])
})

test('previews DMN decision tables without executing FEEL through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_dmn.dmn')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'dmn')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Loan approval/)
  assert.match(report.pages[0].svg, /Applicant age/)
  assert.match(report.pages[0].svg, /manual review/)
  assert.match(report.pages[0].svg, /FEEL is/)
  assert.deepEqual(report.warnings, [])
})

test('previews CMMN 1.1 case plans through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_cmmn.cmmn')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'cmmn')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Claims file/)
  assert.match(report.pages[0].svg, /Review documents/)
  assert.match(report.pages[0].svg, /cmmn:connector/)
  assert.ok(report.warnings.some((warning) => warning.includes('not evaluated')))
})

test('previews ReqIF requirements and relations through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_requirements.reqif')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'reqif')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Vehicle braking requirements/)
  assert.match(report.pages[0].svg, /Requirement details/)
  assert.match(report.pages[0].svg, /Stopping distance/)
  assert.match(report.pages[0].svg, /Derives/)
  assert.ok(report.warnings.some((warning) => warning.includes('XHTML')))
})

test('previews XMI model elements without dereferencing external links through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_model.xmi')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'xmi')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /BrakeController/)
  assert.match(report.pages[0].svg, /WheelSensor/)
  assert.match(report.pages[0].svg, /type=double/)
})

test('previews RFC 4180 quoted CSV cells through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-csv-'))
  const input = path.join(temporary, 'quoted.csv')
  await writeFile(input, 'Name,Note\r\n"Acme, Inc.","She said ""hello"""\r\n')

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'csv')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Acme, Inc\./)
  assert.match(report.pages[0].svg, /She said "hello"/)
})

test('previews a Visio Open XML page through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'flow.vsdx')
  await writeFile(input, minimalVsdx())

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'visio')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="office:visio-shape"/)
  assert.match(report.pages[0].svg, /Visio &amp; Node/)
})

test('previews a legacy Visio XML drawing through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'legacy.vdx')
  await writeFile(input, minimalVdx())

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'visio')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Legacy &amp; Node/)
})

test('previews an EML message through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'message.eml')
  await writeFile(input, [
    'From: Alice <alice@example.test>',
    'Subject: EML preview',
    'Content-Type: text/plain; charset=utf-8',
    '',
    'A readable message body.',
  ].join('\r\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'eml')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /EML preview/)
  assert.match(report.pages[0].svg, /A readable message body\./)
})

test('unwraps RFC 3676 flowed text in a plain-text message', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/flowed_message.eml')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'eml')
  assert.match(report.pages[0].svg, /A flowed line with a preserved word\./)
  assert.match(report.pages[0].svg, /From this sender\./)
  assert.match(report.pages[0].svg, /&gt; quoted words continue\./)
  assert.match(report.pages[0].svg, /Signature line\./)
})

test('previews an Apple Mail EMLX message within its declared byte count', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.emlx')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'emlx')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Apple Mail message/)
  assert.match(report.pages[0].svg, /日本語の本文です。/)
  assert.doesNotMatch(report.pages[0].svg, /plist|flags/)
  assert.ok(report.warnings.some((warning) => warning.includes('property-list metadata was ignored')))
})

test('previews CID images in a safe EML HTML alternative', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/eml_cid_image.eml')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'eml')
  assert.equal(report.pageCount, 1)
  assert.equal(report.pages[0].svg.match(/data:image\/png;base64,/g)?.length, 2)
  assert.match(report.pages[0].svg, /brand logo/)
  assert.match(report.pages[0].svg, /location logo/)
  assert.doesNotMatch(report.pages[0].svg, /example\.invalid|must not render/)
})

test('previews an Outlook MSG message through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/outlook_message.msg')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'msg')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Outlook preview/)
  assert.match(report.pages[0].svg, /Rendered/)
  assert.match(report.pages[0].svg, /日本語表示/)
  assert.match(report.pages[0].svg, /Attachments: 1 omitted/)
  assert.doesNotMatch(report.pages[0].svg, /window\.alert|example\.invalid/)
  assert.ok(report.warnings.some((warning) => warning.includes('attachment')))
})

test('previews each mbox message as a sequential SVG page', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'archive.mbox')
  await writeFile(input, [
    'From alice@example.test Sat Sep 14 10:00:00 2024',
    'From: Alice <alice@example.test>',
    'Subject: First archived message',
    'Content-Type: text/plain; charset=utf-8',
    '',
    'First mailbox body.',
    '',
    'From bob@example.test Sun Sep 15 11:30:00 2024',
    'From: Bob <bob@example.test>',
    'Subject: Second archived message',
    'Content-Type: text/plain; charset=utf-8',
    '',
    'Second mailbox body.',
  ].join('\r\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'mbox')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /First archived message/)
  assert.match(report.pages[1].svg, /Second archived message/)
})

test('previews an MHTML saved web page through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'saved.mhtml')
  await writeFile(input, [
    'MIME-Version: 1.0',
    'Content-Type: multipart/related; boundary=page; type="text/html"',
    '',
    '--page',
    'Content-Type: text/html; charset=utf-8',
    '',
    '<html><body><h1>Saved MHTML</h1><p>Archived page from Node.</p></body></html>',
    '--page--',
    '',
  ].join('\r\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'mhtml')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Saved MHTML/)
  assert.match(report.pages[0].svg, /Archived page from Node/)
})

test('previews local HTML images without resolving remote paths', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/local_image.html')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'html')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /aria-label="red-blue HTML image"/)
  assert.doesNotMatch(report.pages[0].svg, /example\.invalid/)
  assert.ok(report.warnings.some((warning) => warning.includes('external image resources')))
  assert.ok(report.warnings.some((warning) => warning.includes('srcset uses its first candidate')))
})

test('previews bounded local Markdown image blocks', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/markdown_image.md')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'markdown')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /aria-label="red-blue Markdown image"/)
  assert.match(report.pages[0].svg, /aria-label="red-blue inline image"/)
  assert.match(report.pages[0].svg, /aria-label="red-blue reference image"/)
  assert.doesNotMatch(report.pages[0].svg, /example\.invalid/)
})

test('previews AsciiDoc block images from a local imagesdir', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/asciidoc_image.adoc')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'asciidoc')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /aria-label="Red and blue test image"/)
  assert.doesNotMatch(report.pages[0].svg, /example\.invalid/)
  assert.ok(report.warnings.some((warning) => warning.includes('not a validated local PNG/JPEG')))
})

test('previews a complete LaTeX document without executing TeX commands', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample_latex.tex')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'tex')
  assert.equal(report.pageCount, 1)
  const svg = report.pages[0].svg
  assert.match(svg, /LaTeX Document Preview/)
  assert.match(svg, /data:image\/png;base64,/)
  assert.match(svg, /Local red and blue figure/)
  assert.ok(report.warnings.some((warning) => warning.includes('input\/include\/shell')))
  assert.doesNotMatch(svg, /not-loaded\.tex/)
})

test('previews FictionBook 2 XML with bounded embedded images', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.fb2')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'fb2')
  assert.equal(report.pageCount, 1)
  const svg = report.pages[0].svg
  assert.match(svg, /FictionBook Preview/)
  assert.match(svg, /Chapter One/)
  assert.match(svg, /data:image\/png;base64,/)
  assert.doesNotMatch(svg, /Notes are not part of the main flow/)
})

test('previews a ZIP-wrapped FictionBook 2 document', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-fb2-zip-'))
  const input = path.join(directory, 'book.fb2.zip')
  const fixture = await readFile(path.resolve(__dirname, '../../../tests/fixtures/sample.fb2'))
  await writeFile(input, uncompressedZip([['books/book.fb2', fixture]]))
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'fb2')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /FictionBook Preview/)
})

test('previews a bounded PalmDOC/MOBI text book', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.mobi')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'mobi')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /MOBI Preview/)
  assert.match(report.pages[0].svg, /PalmDOC text from a bounded record/)
})

test('previews reStructuredText documents without evaluating file directives', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.rst')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'rst')
  assert.ok(report.pageCount >= 1)
  const svg = report.pages.map((page) => page.svg).join('\n')
  assert.match(svg, /Document Conversion Notes/)
  assert.match(svg, /named target/)
  assert.doesNotMatch(svg, /:ref:/)
  assert.match(svg, /data:image\/png;base64,/)
  assert.match(svg, /RST red and blue image/)
  assert.match(svg, /Figure caption is retained/)
  assert.ok(report.warnings.some((warning) => warning.includes('include directive was not evaluated')))
  assert.ok(report.warnings.some((warning) => warning.includes('raw directive was not evaluated')))
  assert.ok(report.warnings.some((warning) => warning.includes('external image resources')))
  assert.doesNotMatch(svg, /example\.invalid/)
  assert.doesNotMatch(svg, /must-not-be-read/)
  assert.doesNotMatch(svg, /must-not-run/)
})

test('previews Org-mode source without evaluating Babel or include directives', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.org')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'org')
  assert.ok(report.pageCount >= 1)
  const svg = report.pages.map((page) => page.svg).join('\n')
  assert.match(svg, /Org-mode Architecture Notes/)
  assert.match(svg, /delete-file/)
  assert.match(svg, /data:image\/png;base64,/)
  assert.match(svg, /width="96" height="48"/)
  assert.match(svg, /Local Org image caption/)
  assert.ok(report.warnings.some((warning) => warning.includes('#+INCLUDE was not evaluated')))
  assert.ok(report.warnings.some((warning) => warning.includes('Babel calls were not executed')))
  assert.doesNotMatch(svg, /example\.invalid|must-not-render|\/etc\/passwd/)
})

test('previews GNU gettext PO catalogs with contexts and plural forms', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.po')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'po')
  const svg = report.pages.map((page) => page.svg).join('\n')
  assert.match(svg, /Language fr/)
  assert.match(svg, /main-menu/)
  assert.match(svg, /Bienvenue/)
  assert.match(svg, /\[0\] Un fichier/)
  assert.match(svg, /\[1\] %d fichiers/)
  assert.ok(report.warnings.some((warning) => warning.includes('fuzzy gettext translations')))
  assert.doesNotMatch(svg, /Obsolete source text/)
})

test('previews BibTeX bibliography entries without evaluating style or macros', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.bib')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'bib')
  const svg = report.pages.map((page) => page.svg).join('\n')
  assert.match(svg, /smith2024/)
  assert.match(svg, /Nested Unicode Study/)
  assert.match(svg, /A value, with punctuation\./)
  assert.match(svg, /joc Review/)
  assert.ok(report.warnings.some((warning) => warning.includes('@string macros are not expanded')))
})

test('previews iCalendar events through the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'calendar.ics')
  await writeFile(input, [
    'BEGIN:VCALENDAR',
    'VERSION:2.0',
    'BEGIN:VEVENT',
    'UID:event-1',
    'DTSTART:20241012T090000Z',
    'SUMMARY:Calendar API event',
    'END:VEVENT',
    'END:VCALENDAR',
    '',
  ].join('\r\n'))

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ical')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Calendar API event/)
})

test('previews legacy vCalendar 1.0 events through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/meeting.vcs')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'vcalendar')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Legacy vCalendar meeting/)
  assert.match(report.pages[0].svg, /Conference room/)
})

test('previews vCard contacts through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/contact.vcf')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'vcard')
  assert.equal(report.pageCount, 2)
  assert.match(report.pages[0].svg, /山田花子/)
  assert.match(report.pages[1].svg, /Ren Tanaka/)
  assert.doesNotMatch(report.pages[0].svg, /example\.invalid\/avatar/)
})

test('previews vCard 2.1 quoted-printable contacts through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/legacy_contact_21.vcf')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'vcard')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Hanako 山田花子/)
  assert.match(report.pages[0].svg, /hana@example\.test/)
  assert.doesNotMatch(report.pages[0].svg, /AA==/)
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

test('previews Strict OOXML Word, Excel, and PowerPoint packages', async () => {
  const fixtureDirectory = path.resolve(__dirname, '../../../tests/fixtures')
  for (const [filename, sourceFormat, pageCount, text] of [
    ['sample_strict.docx', 'docx', 2, 'document-svg sample'],
    ['sample_strict.xlsx', 'xlsx', 4, 'Quarterly sales by region'],
    ['sample_strict.pptx', 'pptx', 2, 'PDF and Office documents as self-contained SVG pages'],
  ]) {
    const report = await preview(path.join(fixtureDirectory, filename))
    assert.equal(report.sourceFormat, sourceFormat, filename)
    assert.equal(report.pageCount, pageCount, filename)
    assert.deepEqual(report.warnings, [], filename)
    assert.match(report.pages[0].svg, new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }
})

test('previews Microsoft Project XML schedules as Gantt timelines', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/sample.project.xml')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'projectxml')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Website Refresh Plan/)
  assert.match(report.pages[0].svg, /office:project-task-bar/)
  assert.match(report.pages[0].svg, /office:project-dependency-arrow/)
  assert.match(report.pages[0].svg, /office:project-milestone/)
  assert.doesNotMatch(report.pages[0].svg, /Private project resource|Notes are data/)
})

test('previews OpenDocument Text from the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'notes.odt')
  await writeFile(input, minimalOdt())

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'odt')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /ODT preview/)
  assert.match(report.pages[0].svg, /OpenDocument Text from Node/)
})

test('previews bounded package-linked OpenDocument Text images', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/odt_image.odt')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'odt')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /aria-label="red-blue sample"/)
})

test('previews flat OpenDocument Text inline binary PNG images', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-fodt-inline-'))
  const source = path.join(directory, 'inline.fodt')
  const xml = '<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:text><text:p>Before<draw:frame draw:name="inline"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame>After</text:p></office:text></office:body></office:document>'
  await writeFile(source, xml, 'utf8')
  const report = await preview(source)
  assert.equal(report.sourceFormat, 'odt')
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.doesNotMatch(report.pages[0].svg, /binary-data images are omitted/)
})

test('previews bounded named and inline OpenDocument Text styles', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/odt_styles.odt')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'odt')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /Styled ODT heading/)
  assert.match(report.pages[0].svg, /font-weight="700"/)
  assert.match(report.pages[0].svg, /font-style="italic"/)
  assert.match(report.pages[0].svg, /#1D4ED8/)
  assert.match(report.pages[0].svg, /#DC2626/)
  assert.match(report.pages[0].svg, /#16A34A/)
})

test('previews package-linked EPUB images without fetching remote resources', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/epub_image.epub')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'epub')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /aria-label="red-blue EPUB image"/)
  assert.doesNotMatch(report.pages[0].svg, /example\.invalid/)
  assert.ok(report.warnings.some((warning) => warning.includes('remote EPUB image')))
  assert.ok(report.warnings.some((warning) => warning.includes('srcset uses its first candidate')))
})

test('previews OpenDocument Spreadsheet from the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'metrics.ods')
  await writeFile(input, minimalOds())

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ods')
  assert.equal(report.pageCount, 1)
  assert.equal(report.needsReview, true)
  assert.match(report.pages[0].svg, /Quarterly/)
  assert.match(report.pages[0].svg, /Requests/)
})

test('previews package-linked ODS images with safe resource handling', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/ods_image.ods')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'ods')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /aria-label="red-blue sheet image"/)
  assert.match(report.pages[0].svg, /aria-label="cell-anchored image"/)
  assert.doesNotMatch(report.pages[0].svg, /example\.invalid/)
  assert.ok(report.warnings.some((warning) => warning.includes('external ODS image')))
})

test('previews a GLB scene with bounded mesh geometry', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/triangle.glb')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'gltf')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-source-format="gltf"/)
  assert.ok(report.warnings.some((warning) => warning.includes('materials, textures')))
})

test('previews JSON glTF with a confined local buffer', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/triangle.gltf')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'gltf')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data-semantic-role="gltf:mesh"/)
})

test('previews OpenDocument Presentation from the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'slides.fodp')
  await writeFile(input, minimalFodp())

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'odp')
  assert.equal(report.pageCount, 1)
  assert.equal(report.needsReview, true)
  assert.match(report.pages[0].svg, /OpenDocument presentation from Node/)
})

test('previews package-linked OpenDocument presentation images through the Node.js API', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/odp_image.odp')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'odp')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.doesNotMatch(report.pages[0].svg, /ODP embedded images/)
})

test('previews OpenDocument Drawing from the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'diagram.fodg')
  await writeFile(input, minimalFodg())

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'odg')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /OpenDocument drawing from Node/)
})

test('previews flat OpenDocument Drawing inline binary PNG images', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-fodg-inline-'))
  const input = path.join(temporary, 'inline.fodg')
  const xml = '<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:drawing><draw:page draw:name="Inline"><draw:frame draw:name="image" svg:x="1cm" svg:y="1cm" svg:width="4cm" svg:height="2cm"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame></draw:page></office:drawing></office:body></office:document-content>'
  await writeFile(input, xml, 'utf8')
  const report = await preview(input)
  assert.equal(report.sourceFormat, 'odg')
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
})

test('previews RTF text from the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'memo.rtf')
  await writeFile(input, minimalRtf())

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'rtf')
  assert.equal(report.pageCount, 1)
  assert.equal(report.needsReview, true)
  assert.match(report.pages[0].svg, /Résumé — RTF from Node/)
})

test('previews Japanese ANSI-codepage RTF text from the Node.js API', async () => {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'document-svg-node-'))
  const input = path.join(temporary, 'japanese.rtf')
  await writeFile(input, minimalJapaneseRtf())

  const report = await preview(input)

  assert.equal(report.sourceFormat, 'rtf')
  assert.match(report.pages[0].svg, /あ/)
  assert.equal(report.warnings.some((warning) => warning.includes('code page 932')), false)
})

test('previews bounded PNG pictures embedded in RTF', async () => {
  const input = path.resolve(__dirname, '../../../tests/fixtures/rtf_image.rtf')
  const report = await preview(input)

  assert.equal(report.sourceFormat, 'rtf')
  assert.equal(report.pageCount, 1)
  assert.match(report.pages[0].svg, /data:image\/png;base64,/)
  assert.match(report.pages[0].svg, /aria-label="Embedded RTF picture"/)
  assert.ok(report.warnings.some((warning) => warning.includes('picture anchors')))
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

test('converts CAD DXF file to SVG and previews it', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-cad-'))
  const dxfContent = `0
SECTION
2
ENTITIES
0
LINE
8
0
10
0.0
20
0.0
11
100.0
21
50.0
0
ENDSEC
0
EOF
`
  const source = path.join(directory, 'model.dxf')
  await writeFile(source, dxfContent, 'utf8')

  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'dxf')
  assert.equal(report.pageCount, 1)

  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'dxf')
  assert.match(previewReport.pages[0].svg, /<path/)
})

test('converts and previews SubRip and WebVTT subtitle cues', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-subtitle-'))
  const fixtures = [
    {
      name: 'captions.srt',
      sourceFormat: 'srt',
      content: '1\n00:00:01,000 --> 00:00:02,500\nHello <b>world</b>.\n',
      expected: ['00:00:01.000', 'Hello world.'],
    },
    {
      name: 'captions.vtt',
      sourceFormat: 'vtt',
      content: 'WEBVTT\n\n00:01.000 --> 00:02.000\n<v Speaker>Welcome aboard.</v>\n',
      expected: ['00:00:01.000', 'Speaker: Welcome aboard.'],
    },
  ]

  for (const fixture of fixtures) {
    const source = path.join(directory, fixture.name)
    await writeFile(source, fixture.content, 'utf8')
    const report = await convert(source, path.join(directory, `${fixture.name}-out`))
    assert.equal(report.sourceFormat, fixture.sourceFormat)
    assert.equal(report.pageCount, 1)
    const previewReport = await preview(source)
    assert.equal(previewReport.sourceFormat, fixture.sourceFormat)
    for (const text of fixture.expected) {
      assert.ok(previewReport.pages[0].svg.includes(text), `missing ${text}`)
    }
  }
})

test('converts TTML text cues without loading styles or active content', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-ttml-'))
  const source = path.join(directory, 'captions.ttml')
  await writeFile(
    source,
    '<tt xmlns="http://www.w3.org/ns/ttml"><head><layout><region xml:id="r"/></layout></head>' +
      '<body><div><p xml:id="line-1" begin="00:00:01.000" dur="2s">' +
      'Safe &lt;script&gt;text&lt;/script&gt;.</p></div></body></tt>',
    'utf8',
  )
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'ttml')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('styles, regions')))
  const previewReport = await preview(source)
  assert.ok(previewReport.pages[0].svg.includes('00:00:01.000–00:00:03.000 [line-1]'))
  assert.ok(previewReport.pages[0].svg.includes('&lt;script&gt;text&lt;/script&gt;.'))
  assert.ok(!previewReport.pages[0].svg.includes('<script>'))
})

test('converts XLIFF source and target segments without activating markup', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-xliff-'))
  const source = path.join(directory, 'messages.xliff')
  await writeFile(
    source,
    '<xliff xmlns="urn:oasis:names:tc:xliff:document:2.0" version="2.0" srcLang="en" trgLang="fr">' +
      '<file original="menu.xml"><unit id="open"><segment state="translated">' +
      '<source>Open <ph id="1"/></source><target>Ouvrir <ph id="1"/></target>' +
      '</segment></unit></file></xliff>',
    'utf8',
  )
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'xliff')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  const svg = previewReport.pages[0].svg
  assert.ok(svg.includes('Languages: en → fr'))
  assert.ok(svg.includes('Source (en): Open [ph:1]'))
  assert.ok(svg.includes('Segment 1 [translated]'))
  assert.ok(svg.includes('Target (fr): Ouvrir [ph:1]'))
})

test('converts an I-DEAS UNV FEA mesh through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-unv-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.unv')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'unv')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('nonzero Z')))
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'unv')
  assert.match(previewReport.pages[0].svg, /UNV Universal FEA mesh/)
})

test('converts a Tecplot ASCII FEA mesh through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-tecplot-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_tecplot.dat')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'tecplot')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'tecplot')
  assert.match(previewReport.pages[0].svg, /Tecplot finite-element sample/)
})

test('converts an EnSight Gold ASCII case through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-ensight-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_ensight.case')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'ensight')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'ensight')
  assert.match(previewReport.pages[0].svg, /EnSight: EnSight Gold ASCII sample/)
})

test('converts a PLOT3D ASCII structured grid through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-plot3d-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_plot3d.p3d')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'plot3d')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'plot3d')
  assert.match(previewReport.pages[0].svg, /PLOT3D structured grid/)
})

test('converts a VRML97 indexed mesh through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-vrml-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.wrl')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'vrml')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'vrml')
  assert.match(previewReport.pages[0].svg, /Wavefront OBJ 3D Model/)
})

test('converts a Netpbm PPM raster through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-netpbm-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_rgb.ppm')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'raster')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'raster')
  assert.match(previewReport.pages[0].svg, /vectorized_path/)
})

test('converts a SYLK spreadsheet through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-sylk-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.slk')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'sylk')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'sylk')
  assert.match(previewReport.pages[0].svg, /SYLK spreadsheet/)
})

test('converts a DIF spreadsheet through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-dif-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.dif')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'dif')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'dif')
  assert.match(previewReport.pages[0].svg, /DIF spreadsheet/)
})

test('converts FASTA and FASTQ sequence records through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-fastx-'))
  for (const [filename, format] of [['sample.fasta', 'fasta'], ['sample.fastq', 'fastq']]) {
    const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', filename)
    const report = await convert(source, path.join(directory, format))
    assert.equal(report.sourceFormat, format)
    assert.equal(report.pageCount, 1)
    const previewReport = await preview(source)
    assert.equal(previewReport.sourceFormat, format)
    assert.match(previewReport.pages[0].svg, new RegExp(`${format.toUpperCase()} sequence records`))
  }
})

test('converts GFF3 and GTF annotations through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-gff-'))
  for (const [filename, format] of [['sample.gff3', 'gff3'], ['sample.gtf', 'gtf']]) {
    const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', filename)
    const report = await convert(source, path.join(directory, format))
    assert.equal(report.sourceFormat, format)
    assert.equal(report.pageCount, 1)
    const previewReport = await preview(source)
    assert.equal(previewReport.sourceFormat, format)
    assert.match(previewReport.pages[0].svg, new RegExp(`${format.toUpperCase()} feature annotations`))
  }
})

test('converts BED and bedGraph intervals through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-bed-'))
  for (const [filename, format] of [['sample.bed', 'bed'], ['sample.bedgraph', 'bedgraph']]) {
    const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', filename)
    const report = await convert(source, path.join(directory, format))
    assert.equal(report.sourceFormat, format)
    assert.equal(report.pageCount, 1)
    const previewReport = await preview(source)
    assert.equal(previewReport.sourceFormat, format)
    assert.match(previewReport.pages[0].svg, new RegExp(`${format.toUpperCase()} interval annotations`))
  }
})

test('converts VCF variants through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-vcf-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_variants.vcf')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'vcf')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'vcf')
  assert.match(previewReport.pages[0].svg, /VCF variant annotations/)
})

test('converts SAM alignment records through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-sam-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.sam')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'sam')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'sam')
  assert.match(previewReport.pages[0].svg, /SAM alignment records/)
})

test('converts WIG fixedStep and variableStep signals through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-wig-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.wig')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'wig')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'wig')
  assert.match(previewReport.pages[0].svg, /WIG continuous signal/)
})

test('converts MAF multiple alignment blocks through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-maf-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.maf')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'maf')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'maf')
  assert.match(previewReport.pages[0].svg, /MAF multiple alignments/)
})

test('converts Newick phylogenetic trees through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-newick-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.nwk')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'newick')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'newick')
  assert.match(previewReport.pages[0].svg, /Newick phylogenetic tree/)
  assert.match(previewReport.pages[0].svg, /Homo_sapiens/)
})

test('converts Stockholm multiple alignments through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-stockholm-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.sto')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'stockholm')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'stockholm')
  assert.match(previewReport.pages[0].svg, /Stockholm multiple alignments/)
  assert.match(previewReport.pages[0].svg, /AC-GTT/)
})

test('converts CLUSTAL block alignments through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-clustal-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.aln')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'clustal')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'clustal')
  assert.match(previewReport.pages[0].svg, /CLUSTAL multiple alignments/)
  assert.match(previewReport.pages[0].svg, /AC-GTT/)
})

test('converts NEXUS tree blocks through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-nexus-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.nex')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'nexus')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'nexus')
  assert.match(previewReport.pages[0].svg, /NEXUS phylogenetic tree/)
  assert.match(previewReport.pages[0].svg, /Mammals/)
})

test('converts GenBank records through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-genbank-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.gb')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'genbank')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'genbank')
  assert.match(previewReport.pages[0].svg, /GenBank records/)
  assert.match(previewReport.pages[0].svg, /DEMO0001/)
})

test('converts EMBL-Bank records through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-embl-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.embl')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'embl')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'embl')
  assert.match(previewReport.pages[0].svg, /EMBL-Bank records/)
  assert.match(previewReport.pages[0].svg, /DEMO0001/)
})

test('converts UniProt flat records through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-uniprot-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_uniprot.dat')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'uniprot')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'uniprot')
  assert.match(previewReport.pages[0].svg, /UniProtKB protein records/)
  assert.match(previewReport.pages[0].svg, /DEMO_HUMAN/)
})

test('converts RIS bibliography records through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-ris-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.ris')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'ris')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'ris')
  assert.match(previewReport.pages[0].svg, /RIS bibliography/)
  assert.match(previewReport.pages[0].svg, /Safe document conversion/)
})

test('converts SPICE netlists through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-spice-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cir')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'spice')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'spice')
  assert.match(previewReport.pages[0].svg, /SPICE netlist/)
  assert.match(previewReport.pages[0].svg, /R1/)
})

test('converts legacy KiCad schematics through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-kicad-sch-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_legacy.sch')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'kicad_sch_legacy')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'kicad_sch_legacy')
  assert.match(previewReport.pages[0].svg, /KiCad legacy schematic/)
  assert.match(previewReport.pages[0].svg, /FILTERED_OUT/)
})

test('converts modern KiCad S-expression schematics through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-kicad-modern-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.kicad_sch')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'kicad_sch')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'kicad_sch')
  assert.match(previewReport.pages[0].svg, /KiCad schematic/)
  assert.match(previewReport.pages[0].svg, /FILTERED_OUT/)
})

test('converts LTspice ASCII schematics through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-ltspice-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_ltspice.asc')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'ltspice_asc')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'ltspice_asc')
  assert.match(previewReport.pages[0].svg, /LTspice schematic/)
  assert.match(previewReport.pages[0].svg, /R1/)
})

test('converts EAGLE XML schematics through the Node.js API', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-eagle-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_eagle.sch')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'eagle_sch')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  assert.equal(previewReport.sourceFormat, 'eagle_sch')
  assert.match(previewReport.pages[0].svg, /EAGLE schematic/)
  assert.match(previewReport.pages[0].svg, /R1/)
})

test('previews general JSON settings as safe path/value rows', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-json-'))
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.json')
  const report = await convert(source, path.join(directory, 'out'))
  assert.equal(report.sourceFormat, 'json')
  assert.equal(report.pageCount, 1)
  const previewReport = await preview(source)
  const svg = previewReport.pages[0].svg
  assert.ok(svg.includes('$.service.enabled (boolean): true'))
  assert.ok(svg.includes('$.service.ports[1] (number): 8081'))
  assert.ok(svg.includes('&lt;script&gt;alert(1)&lt;/script&gt;'))
  assert.ok(!svg.includes('<script>'))
})

test('reverses SVG to CAD formats (DXF, G-code, Gerber, HP-GL)', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'document-svg-rev-'))
  const svgSource = path.join(directory, 'test.svg')
  const svgContent = '<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">' +
    '<line x1="10" y1="10" x2="90" y2="90" stroke="black"/>' +
    '<circle cx="50" cy="50" r="20" fill="none" stroke="red"/>' +
    '</svg>'
  await writeFile(svgSource, svgContent, 'utf8')

  // Reverse to DXF
  const dxfOutput = path.join(directory, 'output.dxf')
  const dxfReport = await reverse(svgSource, dxfOutput)
  assert.equal(dxfReport.outputFormat, 'dxf')
  await access(dxfOutput)

  // Reverse to G-code
  const gcodeOutput = path.join(directory, 'output.gcode')
  const gcodeReport = await reverse(svgSource, gcodeOutput)
  assert.equal(gcodeReport.outputFormat, 'gcode')
  await access(gcodeOutput)

  // Reverse to Gerber
  const gbrOutput = path.join(directory, 'output.gbr')
  const gbrReport = await reverse(svgSource, gbrOutput)
  assert.equal(gbrReport.outputFormat, 'gerber')
  await access(gbrOutput)

  // Reverse to HP-GL
  const pltOutput = path.join(directory, 'output.plt')
  const pltReport = await reverse(svgSource, pltOutput)
  assert.equal(pltReport.outputFormat, 'hpgl')
  await access(pltOutput)

  // Reverse to Excellon Drill
  const drlOutput = path.join(directory, 'output.drl')
  const drlReport = await reverse(svgSource, drlOutput)
  assert.equal(drlReport.outputFormat, 'excellon')
  await access(drlOutput)

  // Reverse to 3D STL
  const stlOutput = path.join(directory, 'output.stl')
  const stlReport = await reverse(svgSource, stlOutput)
  assert.equal(stlReport.outputFormat, 'stl')
  await access(stlOutput)

  // Reverse to 3D Wavefront OBJ
  const objOutput = path.join(directory, 'output.obj')
  const objReport = await reverse(svgSource, objOutput)
  assert.equal(objReport.outputFormat, 'obj')
  await access(objOutput)

  // Reverse to STEP ISO 10303-21
  const stepOutput = path.join(directory, 'output.step')
  const stepReport = await reverse(svgSource, stepOutput)
  assert.equal(stepReport.outputFormat, 'step')
  await access(stepOutput)

  // Reverse to Gmsh FEA Mesh
  const mshOutput = path.join(directory, 'output.msh')
  const mshReport = await reverse(svgSource, mshOutput)
  assert.equal(mshReport.outputFormat, 'gmsh')
  await access(mshOutput)

  // Reverse to VTK PolyData
  const vtkOutput = path.join(directory, 'output.vtk')
  const vtkReport = await reverse(svgSource, vtkOutput)
  assert.equal(vtkReport.outputFormat, 'vtk')
  await access(vtkOutput)

  // Reverse to HTML
  const htmlOutput = path.join(directory, 'output.html')
  const htmlReport = await reverse(svgSource, htmlOutput)
  assert.equal(htmlReport.outputFormat, 'html')
  await access(htmlOutput)

  // Reverse to WebP
  const webpOutput = path.join(directory, 'output.webp')
  const webpReport = await reverse(svgSource, webpOutput)
  assert.equal(webpReport.outputFormat, 'webp')
  await access(webpOutput)
})

test('transforms and optimizes SVG via Node.js API', async () => {
  const svg = `<!-- Comment to remove -->
<svg width="100px" height="80px" viewBox="0 0 100 80" data-source="unit-test">
  <metadata><author>Bob</author></metadata>
  <desc>A descriptive note</desc>
  <rect x="0" y="0" width="50.456" height="40.789" fill="#ff0000" stroke="#00ff00" data-elem="r1" />
</svg>`

  // 1. Minify + responsive
  const res1 = transform(svg, { minify: true, responsive: true })
  assert.ok(!res1.includes('<!-- Comment to remove -->'))
  assert.ok(!res1.includes('width="100px"'))
  assert.ok(res1.includes('viewBox="0 0 100 80"'))

  // 2. Monochrome + precision + removeMetadata
  const res2 = transform(svg, {
    monochrome: '#111111',
    precision: 1,
    removeMetadata: true,
  })
  assert.ok(res2.includes('fill="#111111"'))
  assert.ok(res2.includes('stroke="#111111"'))
  assert.ok(res2.includes('width="50.5"'))
  assert.ok(!res2.includes('data-source'))
  assert.ok(!res2.includes('data-elem'))
  assert.ok(!res2.includes('Bob'))
  assert.ok(!res2.includes('metadata'))
  assert.ok(!res2.includes('desc'))

  const cleaned = transform(`<svg viewBox="0 0 20 20">
  <g id="empty-group"><g id="nested-empty"></g></g>
  <g id="content-group"><path d="M 0 0 L 10 10 L 10 10 Z Z" /></g>
</svg>`, { cleanPaths: true, stripEmptyGroups: true })
  assert.ok(!cleaned.includes('empty-group'))
  assert.ok(!cleaned.includes('nested-empty'))
  assert.ok(cleaned.includes('content-group'))
  assert.ok(cleaned.includes('d="M 0 0 L 10 10 Z"'))
})

test('converts OpenAPI JSON and YAML through Node.js API', async () => {
  const json = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.openapi.json')
  const jsonReport = await convert(json, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-openapi-')), 'json-out'))
  assert.equal(jsonReport.sourceFormat, 'openapi')
  assert.equal(jsonReport.pageCount, 1)
  const yaml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.openapi.yaml')
  const yamlReport = await convert(yaml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-openapi-')), 'yaml-out'))
  assert.equal(yamlReport.sourceFormat, 'openapi')
  assert.equal(yamlReport.pageCount, 1)
})

test('converts AsyncAPI JSON and YAML through Node.js API', async () => {
  const json = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.asyncapi.json')
  const jsonReport = await convert(json, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-asyncapi-')), 'json-out'))
  assert.equal(jsonReport.sourceFormat, 'asyncapi')
  assert.equal(jsonReport.pageCount, 1)
  const yaml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.asyncapi.yaml')
  const yamlReport = await convert(yaml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-asyncapi-')), 'yaml-out'))
  assert.equal(yamlReport.sourceFormat, 'asyncapi')
  assert.equal(yamlReport.pageCount, 1)
})

test('converts JSON Schema JSON and YAML through Node.js API', async () => {
  const json = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.schema.json')
  const jsonReport = await convert(json, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-jsonschema-')), 'json-out'))
  assert.equal(jsonReport.sourceFormat, 'jsonschema')
  assert.equal(jsonReport.pageCount, 1)
  const yaml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.schema.yaml')
  const yamlReport = await convert(yaml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-jsonschema-')), 'yaml-out'))
  assert.equal(yamlReport.sourceFormat, 'jsonschema')
  assert.equal(yamlReport.pageCount, 1)
})

test('converts ANSYS CDB mesh through Node.js API', async () => {
  const cdb = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cdb')
  const report = await convert(cdb, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-cdb-')), 'out'))
  assert.equal(report.sourceFormat, 'cdb')
  assert.equal(report.pageCount, 1)
})

test('converts HAR network logs with safe redaction through Node.js API', async () => {
  const har = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.har')
  const report = await convert(har, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-har-')), 'out'))
  assert.equal(report.sourceFormat, 'har')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('never fetched')))
})

test('converts WARC records and gzip archives through Node.js API', async () => {
  const warc = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.warc')
  const report = await convert(warc, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-warc-')), 'out'))
  assert.equal(report.sourceFormat, 'warc')
  assert.equal(report.pageCount, 1)
  const gzip = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.warc.gz')
  const gzipReport = await convert(gzip, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-warc-')), 'gzip-out'))
  assert.equal(gzipReport.sourceFormat, 'warc')
  assert.equal(gzipReport.pageCount, 1)
})

test('converts WACZ manifest and page metadata through Node.js API', async () => {
  const wacz = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.wacz')
  const report = await convert(wacz, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-wacz-')), 'out'))
  assert.equal(report.sourceFormat, 'wacz')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('payloads')))
})

test('converts Postman Collection JSON with safe redaction through Node.js API', async () => {
  const collection = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.postman_collection.json')
  const report = await convert(collection, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-postman-')), 'out'))
  assert.equal(report.sourceFormat, 'postman')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('never') && warning.includes('executed')))
})

test('converts GraphQL SDL through Node.js API', async () => {
  const schema = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.graphql')
  const report = await convert(schema, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-graphql-')), 'out'))
  assert.equal(report.sourceFormat, 'graphql')
  assert.equal(report.pageCount, 1)
})

test('converts Protocol Buffers schema through Node.js API', async () => {
  const schema = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.proto')
  const report = await convert(schema, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-protobuf-')), 'out'))
  assert.equal(report.sourceFormat, 'protobuf')
  assert.equal(report.pageCount, 1)
})

test('converts Kubernetes YAML and JSON manifests safely through Node.js API', async () => {
  const yaml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.k8s.yaml')
  const yamlReport = await convert(yaml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-k8s-')), 'yaml-out'))
  assert.equal(yamlReport.sourceFormat, 'kubernetes')
  assert.equal(yamlReport.pageCount, 1)
  const json = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.k8s.json')
  const jsonReport = await convert(json, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-k8s-')), 'json-out'))
  assert.equal(jsonReport.sourceFormat, 'kubernetes')
  assert.equal(jsonReport.pageCount, 1)
})

test('converts Docker Compose YAML and JSON safely through Node.js API', async () => {
  const yaml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.compose.yaml')
  const yamlReport = await convert(yaml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-compose-')), 'yaml-out'))
  assert.equal(yamlReport.sourceFormat, 'compose')
  assert.equal(yamlReport.pageCount, 1)
  const json = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.compose.json')
  const jsonReport = await convert(json, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-compose-')), 'json-out'))
  assert.equal(jsonReport.sourceFormat, 'compose')
  assert.equal(jsonReport.pageCount, 1)
  assert.ok(yamlReport.warnings.some((warning) => warning.includes('daemon')))
})

test('converts GitHub Actions workflow YAML safely through Node.js API', async () => {
  const workflow = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.github.workflow.yml')
  const report = await convert(workflow, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-workflow-')), 'out'))
  assert.equal(report.sourceFormat, 'github-actions')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('never')))
})

test('converts JUnit XML reports safely through Node.js API', async () => {
  const reportFile = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.junit.xml')
  const report = await convert(reportFile, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-junit-')), 'out'))
  assert.equal(report.sourceFormat, 'junit')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('never')))
})

test('converts SARIF static analysis results safely through Node.js API', async () => {
  const sarif = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.sarif')
  const report = await convert(sarif, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-sarif-')), 'out'))
  assert.equal(report.sourceFormat, 'sarif')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('never')))
})

test('converts Terraform plan JSON safely through Node.js API', async () => {
  const plan = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.tfplan.json')
  const report = await convert(plan, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-terraform-')), 'out'))
  assert.equal(report.sourceFormat, 'terraform-plan')
  assert.equal(report.pageCount, 1)
  assert.ok(report.warnings.some((warning) => warning.includes('never')))
})

test('converts CycloneDX JSON and XML BOMs safely through Node.js API', async () => {
  const json = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cdx.json')
  const jsonReport = await convert(json, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-cdx-')), 'json-out'))
  assert.equal(jsonReport.sourceFormat, 'cyclonedx')
  assert.equal(jsonReport.pageCount, 1)
  const xml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cdx.xml')
  const xmlReport = await convert(xml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-cdx-')), 'xml-out'))
  assert.equal(xmlReport.sourceFormat, 'cyclonedx')
  assert.equal(xmlReport.pageCount, 1)
})

test('converts SPDX JSON SBOMs safely through Node.js API', async () => {
  const spdx = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.spdx.json')
  const report = await convert(spdx, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-spdx-')), 'out'))
  assert.equal(report.sourceFormat, 'spdx')
  assert.equal(report.pageCount, 1)
  const tag = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.spdx')
  const tagReport = await convert(tag, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-spdx-')), 'tag-out'))
  assert.equal(tagReport.sourceFormat, 'spdx')
  assert.equal(tagReport.pageCount, 1)
})

test('converts JaCoCo and Cobertura coverage XML safely through Node.js API', async () => {
  const jacoco = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.jacoco.xml')
  const report = await convert(jacoco, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-coverage-')), 'jacoco-out'))
  assert.equal(report.sourceFormat, 'coverage')
  assert.equal(report.pageCount, 1)
  const cobertura = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cobertura.xml')
  const coberturaReport = await convert(cobertura, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-coverage-')), 'cobertura-out'))
  assert.equal(coberturaReport.sourceFormat, 'coverage')
})

test('converts LCOV tracefiles safely through Node.js API', async () => {
  const lcov = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.lcov.info')
  const report = await convert(lcov, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-lcov-')), 'out'))
  assert.equal(report.sourceFormat, 'lcov')
  assert.equal(report.pageCount, 1)
})

test('converts JSON Patch documents safely through Node.js API', async () => {
  const patch = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.jsonpatch')
  const report = await convert(patch, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-jsonpatch-')), 'out'))
  assert.equal(report.sourceFormat, 'jsonpatch')
  assert.equal(report.pageCount, 1)
})

test('converts JSON Merge Patch documents safely through Node.js API', async () => {
  const patch = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.mergepatch')
  const report = await convert(patch, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-mergepatch-')), 'out'))
  assert.equal(report.sourceFormat, 'jsonmergepatch')
  assert.equal(report.pageCount, 1)
})

test('converts OpenFOAM field files safely through Node.js API', async () => {
  const field = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.foamfield')
  const report = await convert(field, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-foam-field-')), 'out'))
  assert.equal(report.sourceFormat, 'openfoam-field')
  assert.equal(report.pageCount, 1)
})

test('converts CSL-JSON bibliography data safely through Node.js API', async () => {
  const csl = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.csl.json')
  const report = await convert(csl, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-csl-')), 'out'))
  assert.equal(report.sourceFormat, 'csl-json')
  assert.equal(report.pageCount, 1)
})

test('converts JSON Feed documents safely through Node.js API', async () => {
  const feed = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.jsonfeed')
  const report = await convert(feed, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-jsonfeed-')), 'out'))
  assert.equal(report.sourceFormat, 'jsonfeed')
  assert.equal(report.pageCount, 1)
})

test('converts CloudEvents JSON safely through Node.js API', async () => {
  const event = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cloudevent.json')
  const report = await convert(event, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-cloudevents-')), 'out'))
  assert.equal(report.sourceFormat, 'cloudevents')
  assert.equal(report.pageCount, 1)
})

test('converts FHIR JSON bundles safely through Node.js API', async () => {
  const bundle = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.fhir.json')
  const report = await convert(bundle, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-fhir-')), 'out'))
  assert.equal(report.sourceFormat, 'fhir-json')
  assert.equal(report.pageCount, 1)
})

test('converts Avro JSON schemas safely through Node.js API', async () => {
  const schema = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.avsc')
  const report = await convert(schema, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-avro-')), 'out'))
  assert.equal(report.sourceFormat, 'avro')
  assert.equal(report.pageCount, 1)
})

test('converts OTLP JSON telemetry safely through Node.js API', async () => {
  const telemetry = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.otlp.json')
  const report = await convert(telemetry, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-otlp-')), 'out'))
  assert.equal(report.sourceFormat, 'otlp-json')
  assert.equal(report.pageCount, 1)
})

test('converts OCEL JSON logs safely through Node.js API', async () => {
  const log = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.jsonocel')
  const report = await convert(log, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-ocel-')), 'out'))
  assert.equal(report.sourceFormat, 'ocel-json')
  assert.equal(report.pageCount, 1)
})

test('converts JSON:API documents safely through Node.js API', async () => {
  const api = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.jsonapi')
  const report = await convert(api, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-jsonapi-')), 'out'))
  assert.equal(report.sourceFormat, 'json-api')
  assert.equal(report.pageCount, 1)
})

test('converts ASAM OpenDRIVE road geometry safely through Node.js API', async () => {
  const road = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xodr')
  const report = await convert(road, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-opendrive-')), 'out'))
  assert.equal(report.sourceFormat, 'opendrive')
  assert.equal(report.pageCount, 1)
})

test('converts ASAM OpenSCENARIO structure safely through Node.js API', async () => {
  const scenario = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xosc')
  const report = await convert(scenario, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-openscenario-')), 'out'))
  assert.equal(report.sourceFormat, 'openscenario')
  assert.equal(report.pageCount, 1)
})

test('converts ASAM OpenLABEL annotations safely through Node.js API', async () => {
  const labels = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.openlabel.json')
  const report = await convert(labels, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-openlabel-')), 'out'))
  assert.equal(report.sourceFormat, 'openlabel')
  assert.equal(report.pageCount, 1)
})

test('converts CityGML metadata safely through Node.js API', async () => {
  const city = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.citygml')
  const report = await convert(city, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-citygml-')), 'out'))
  assert.equal(report.sourceFormat, 'citygml')
  assert.equal(report.pageCount, 1)
})

test('converts CityJSON metadata safely through Node.js API', async () => {
  const city = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cityjson')
  const report = await convert(city, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-cityjson-')), 'out'))
  assert.equal(report.sourceFormat, 'cityjson')
  assert.equal(report.pageCount, 1)
})

test('converts STIX JSON threat bundles safely through Node.js API', async () => {
  const stix = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.stix.json')
  const report = await convert(stix, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-stix-')), 'out'))
  assert.equal(report.sourceFormat, 'stix-json')
  assert.equal(report.pageCount, 1)
})

test('converts ASAM OpenCRG headers safely through Node.js API', async () => {
  const crg = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.crg')
  const report = await convert(crg, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-opencrg-')), 'out'))
  assert.equal(report.sourceFormat, 'opencrg')
  assert.equal(report.pageCount, 1)
})

test('converts TAXII JSON manifests safely through Node.js API', async () => {
  const taxii = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.taxii.json')
  const report = await convert(taxii, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-taxii-')), 'out'))
  assert.equal(report.sourceFormat, 'taxii-json')
  assert.equal(report.pageCount, 1)
})

test('converts WSDL service descriptions safely through Node.js API', async () => {
  const wsdl = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.wsdl')
  const report = await convert(wsdl, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-wsdl-')), 'out'))
  assert.equal(report.sourceFormat, 'wsdl')
  assert.equal(report.pageCount, 1)
})

test('converts OPML outlines without fetching feed URLs through Node.js API', async () => {
  const opml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.opml')
  const report = await convert(opml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-opml-')), 'out'))
  assert.equal(report.sourceFormat, 'opml')
  assert.equal(report.pageCount, 1)
})

test('converts RSS feeds without fetching links through Node.js API', async () => {
  const rss = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.rss')
  const report = await convert(rss, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-rss-')), 'out'))
  assert.equal(report.sourceFormat, 'feed')
  assert.equal(report.pageCount, 1)
})

test('converts XML property lists safely through Node.js API', async () => {
  const plist = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.plist')
  const report = await convert(plist, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-plist-')), 'out'))
  assert.equal(report.sourceFormat, 'plist')
  assert.equal(report.pageCount, 1)
})

test('converts TEI scholarly text safely through Node.js API', async () => {
  const tei = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.tei')
  const report = await convert(tei, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-tei-')), 'out'))
  assert.equal(report.sourceFormat, 'tei')
  assert.equal(report.pageCount, 1)
})

test('converts ALTO OCR layout safely through Node.js API', async () => {
  const alto = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.alto')
  const report = await convert(alto, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-alto-')), 'out'))
  assert.equal(report.sourceFormat, 'alto')
  assert.equal(report.pageCount, 1)
})

test('converts METS archive structure safely through Node.js API', async () => {
  const mets = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.mets')
  const report = await convert(mets, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-mets-')), 'out'))
  assert.equal(report.sourceFormat, 'mets')
  assert.equal(report.pageCount, 1)
})

test('converts MARCXML records safely through Node.js API', async () => {
  const marcxml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.marcxml')
  const report = await convert(marcxml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-marcxml-')), 'out'))
  assert.equal(report.sourceFormat, 'marcxml')
  assert.equal(report.pageCount, 1)
})

test('converts MODS collections safely through Node.js API', async () => {
  const mods = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.mods')
  const report = await convert(mods, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-mods-')), 'out'))
  assert.equal(report.sourceFormat, 'mods')
  assert.equal(report.pageCount, 1)
})

test('converts PREMIS preservation metadata safely through Node.js API', async () => {
  const premis = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.premis')
  const report = await convert(premis, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-premis-')), 'out'))
  assert.equal(report.sourceFormat, 'premis')
  assert.equal(report.pageCount, 1)
})

test('converts IIIF Presentation manifests safely through Node.js API', async () => {
  const iiif = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.iiif.json')
  const report = await convert(iiif, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-iiif-')), 'out'))
  assert.equal(report.sourceFormat, 'iiif')
  assert.equal(report.pageCount, 1)
})

test('converts EAD finding aids safely through Node.js API', async () => {
  const ead = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.ead')
  const report = await convert(ead, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-ead-')), 'out'))
  assert.equal(report.sourceFormat, 'ead')
  assert.equal(report.pageCount, 1)
})

test('converts EAC-CPF authority records safely through Node.js API', async () => {
  const eac = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.eac-cpf')
  const report = await convert(eac, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-eac-')), 'out'))
  assert.equal(report.sourceFormat, 'eac-cpf')
  assert.equal(report.pageCount, 1)
})

test('converts Dublin Core XML safely through Node.js API', async () => {
  const dc = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.dc.xml')
  const report = await convert(dc, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-dc-')), 'out'))
  assert.equal(report.sourceFormat, 'dublin-core')
  assert.equal(report.pageCount, 1)
})

test('converts S1000D data modules safely through Node.js API', async () => {
  const s1000d = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.s1000d')
  const report = await convert(s1000d, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-s1000d-')), 'out'))
  assert.equal(report.sourceFormat, 's1000d')
  assert.equal(report.pageCount, 1)
})

test('converts DICOM Structured Reports safely through Node.js API', async () => {
  const sr = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample_sr.dcm')
  const report = await convert(sr, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-dicom-sr-')), 'out'))
  assert.equal(report.sourceFormat, 'dicom-sr')
  assert.equal(report.pageCount, 1)
})

test('converts SpreadsheetML workbooks safely through Node.js API', async () => {
  const xmlss = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.spreadsheetml')
  const report = await convert(xmlss, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xmlss-')), 'out'))
  assert.equal(report.sourceFormat, 'spreadsheetml')
  assert.equal(report.pageCount, 1)
})

test('converts RDF/XML graphs safely through Node.js API', async () => {
  const rdfxml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.rdf')
  const report = await convert(rdfxml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-rdfxml-')), 'out'))
  assert.equal(report.sourceFormat, 'rdf-xml')
  assert.equal(report.pageCount, 1)
})

test('converts BCFZIP issue packages safely through Node.js API', async () => {
  const bcf = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.bcfzip')
  const report = await convert(bcf, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-bcf-')), 'out'))
  assert.equal(report.sourceFormat, 'bcfzip')
  assert.equal(report.pageCount, 1)
})

test('converts Flat OPC Word packages safely through Node.js API', async () => {
  const flatOpc = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.flatopc')
  const report = await convert(flatOpc, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-flatopc-')), 'out'))
  assert.equal(report.sourceFormat, 'flat-opc')
  assert.equal(report.pageCount, 1)
})

test('converts AASX asset packages safely through Node.js API', async () => {
  const aasx = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.aasx')
  const report = await convert(aasx, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-aasx-')), 'out'))
  assert.equal(report.sourceFormat, 'aasx')
  assert.equal(report.pageCount, 1)
})

test('previews OpenSCAD source without executing CAD code through Node.js API', async () => {
  const scad = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.scad')
  const report = await convert(scad, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-openscad-')), 'out'))
  assert.equal(report.sourceFormat, 'openscad')
  assert.equal(report.pageCount, 1)
})

test('converts AMF additive-manufacturing meshes safely through Node.js API', async () => {
  const amf = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.amf')
  const report = await convert(amf, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-amf-')), 'out'))
  assert.equal(report.sourceFormat, 'amf')
  assert.equal(report.pageCount, 1)
})

test('converts PLMXML product structures safely through Node.js API', async () => {
  const plmxml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.plmxml')
  const report = await convert(plmxml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-plmxml-')), 'out'))
  assert.equal(report.sourceFormat, 'plmxml')
  assert.equal(report.pageCount, 1)
})

test('converts STEP-XML product data safely through Node.js API', async () => {
  const stepxml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.stepxml')
  const report = await convert(stepxml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-stepxml-')), 'out'))
  assert.equal(report.sourceFormat, 'stepxml')
  assert.equal(report.pageCount, 1)
})

test('converts QIF inspection data safely through Node.js API', async () => {
  const qif = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.qif')
  const report = await convert(qif, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-qif-')), 'out'))
  assert.equal(report.sourceFormat, 'qif')
  assert.equal(report.pageCount, 1)
})

test('converts B2MML manufacturing data safely through Node.js API', async () => {
  const b2mml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.b2mml')
  const report = await convert(b2mml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-b2mml-')), 'out'))
  assert.equal(report.sourceFormat, 'b2mml')
  assert.equal(report.pageCount, 1)
})

test('converts JDF job tickets safely through Node.js API', async () => {
  const jdf = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.jdf')
  const report = await convert(jdf, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-jdf-')), 'out'))
  assert.equal(report.sourceFormat, 'jdf')
  assert.equal(report.pageCount, 1)
})

test('converts XJDF job tickets safely through Node.js API', async () => {
  const xjdf = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xjdf')
  const report = await convert(xjdf, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xjdf-')), 'out'))
  assert.equal(report.sourceFormat, 'xjdf')
  assert.equal(report.pageCount, 1)
})

test('converts CML chemical documents safely through Node.js API', async () => {
  const cml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cml')
  const report = await convert(cml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-cml-')), 'out'))
  assert.equal(report.sourceFormat, 'cml')
  assert.equal(report.pageCount, 1)
})

test('converts XDP packages safely through Node.js API', async () => {
  const xdp = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xdp')
  const report = await convert(xdp, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xdp-')), 'out'))
  assert.equal(report.sourceFormat, 'xdp')
  assert.equal(report.pageCount, 1)
})

test('converts XMP metadata safely through Node.js API', async () => {
  const xmp = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xmp')
  const report = await convert(xmp, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xmp-')), 'out'))
  assert.equal(report.sourceFormat, 'xmp')
  assert.equal(report.pageCount, 1)
})

test('converts MathML formulas safely through Node.js API', async () => {
  const mathml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.mathml')
  const report = await convert(mathml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-mathml-')), 'out'))
  assert.equal(report.sourceFormat, 'mathml')
  assert.equal(report.pageCount, 1)
})

test('converts LandXML civil models safely through Node.js API', async () => {
  const landxml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.landxml')
  const report = await convert(landxml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-landxml-')), 'out'))
  assert.equal(report.sourceFormat, 'landxml')
  assert.equal(report.pageCount, 1)
})

test('converts XFDF form data safely through Node.js API', async () => {
  const xfdf = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xfdf')
  const report = await convert(xfdf, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xfdf-')), 'out'))
  assert.equal(report.sourceFormat, 'xfdf')
  assert.equal(report.pageCount, 1)
})

test('converts FDF form data safely through Node.js API', async () => {
  const fdf = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.fdf')
  const report = await convert(fdf, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-fdf-')), 'out'))
  assert.equal(report.sourceFormat, 'fdf')
  assert.equal(report.pageCount, 1)
})

test('converts XBRL instances safely through Node.js API', async () => {
  const xbrl = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xbrl.xml')
  const report = await convert(xbrl, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xbrl-')), 'out'))
  assert.equal(report.sourceFormat, 'xbrl')
  assert.equal(report.pageCount, 1)
})

test('converts UBL business documents safely through Node.js API', async () => {
  const ubl = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.ubl.xml')
  const report = await convert(ubl, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-ubl-')), 'out'))
  assert.equal(report.sourceFormat, 'ubl')
  assert.equal(report.pageCount, 1)
})

test('converts ISO 19115 metadata safely through Node.js API', async () => {
  const iso19115 = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.iso19115.xml')
  const report = await convert(iso19115, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-iso19115-')), 'out'))
  assert.equal(report.sourceFormat, 'iso19115')
  assert.equal(report.pageCount, 1)
})

test('converts MARC21 ISO 2709 records safely through Node.js API', async () => {
  const marc = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.marc')
  const report = await convert(marc, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-marc-')), 'out'))
  assert.equal(report.sourceFormat, 'marc21')
  assert.equal(report.pageCount, 1)
})

test('converts TMX translation memories safely through Node.js API', async () => {
  const tmx = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.tmx')
  const report = await convert(tmx, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-tmx-')), 'out'))
  assert.equal(report.sourceFormat, 'tmx')
  assert.equal(report.pageCount, 1)
})

test('converts TBX terminology bases safely through Node.js API', async () => {
  const tbx = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.tbx')
  const report = await convert(tbx, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-tbx-')), 'out'))
  assert.equal(report.sourceFormat, 'tbx')
  assert.equal(report.pageCount, 1)
})

test('converts gbXML building models safely through Node.js API', async () => {
  const gbxml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.gbxml')
  const report = await convert(gbxml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-gbxml-')), 'out'))
  assert.equal(report.sourceFormat, 'gbxml')
  assert.equal(report.pageCount, 1)
})

test('converts FHIR XML resources safely through Node.js API', async () => {
  const fhirXml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.fhir.xml')
  const report = await convert(fhirXml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-fhir-xml-')), 'out'))
  assert.equal(report.sourceFormat, 'fhir-xml')
  assert.equal(report.pageCount, 1)
})

test('converts IDML packages safely through Node.js API', async () => {
  const idml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.idml')
  const report = await convert(idml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-idml-')), 'out'))
  assert.equal(report.sourceFormat, 'idml')
  assert.equal(report.pageCount, 1)
})

test('converts XPDL workflow definitions safely through Node.js API', async () => {
  const xpdl = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xpdl')
  const report = await convert(xpdl, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xpdl-')), 'out'))
  assert.equal(report.sourceFormat, 'xpdl')
  assert.equal(report.pageCount, 1)
})

test('converts ONIX book metadata safely through Node.js API', async () => {
  const onix = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.onix')
  const report = await convert(onix, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-onix-')), 'out'))
  assert.equal(report.sourceFormat, 'onix')
  assert.equal(report.pageCount, 1)
})

test('converts OAI-PMH responses safely through Node.js API', async () => {
  const oaipmh = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.oaipmh')
  const report = await convert(oaipmh, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-oaipmh-')), 'out'))
  assert.equal(report.sourceFormat, 'oaipmh')
  assert.equal(report.pageCount, 1)
})

test('converts CDA clinical documents safely through Node.js API', async () => {
  const cda = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cda')
  const report = await convert(cda, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-cda-')), 'out'))
  assert.equal(report.sourceFormat, 'cda')
  assert.equal(report.pageCount, 1)
})

test('converts ISO 20022 messages safely through Node.js API', async () => {
  const iso20022 = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.iso20022.xml')
  const report = await convert(iso20022, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-iso20022-')), 'out'))
  assert.equal(report.sourceFormat, 'iso20022')
  assert.equal(report.pageCount, 1)
})

test('converts SBML models safely through Node.js API', async () => {
  const sbml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.sbml')
  const report = await convert(sbml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-sbml-')), 'out'))
  assert.equal(report.sourceFormat, 'sbml')
  assert.equal(report.pageCount, 1)
})

test('converts CellML models safely through Node.js API', async () => {
  const cellml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.cellml')
  const report = await convert(cellml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-cellml-')), 'out'))
  assert.equal(report.sourceFormat, 'cellml')
  assert.equal(report.pageCount, 1)
})

test('converts OCEL XML event logs safely through Node.js API', async () => {
  const ocelXml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xmlocel')
  const report = await convert(ocelXml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-ocel-xml-')), 'out'))
  assert.equal(report.sourceFormat, 'ocel-xml')
  assert.equal(report.pageCount, 1)
})

test('converts EnergyPlus IDF inputs safely through Node.js API', async () => {
  const idf = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.idf')
  const report = await convert(idf, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-idf-')), 'out'))
  assert.equal(report.sourceFormat, 'energyplus-idf')
  assert.equal(report.pageCount, 1)
})

test('converts EnergyPlus EPW weather safely through Node.js API', async () => {
  const epw = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.epw')
  const report = await convert(epw, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-epw-')), 'out'))
  assert.equal(report.sourceFormat, 'energyplus-epw')
  assert.equal(report.pageCount, 1)
})

test('converts RINEX GNSS data safely through Node.js API', async () => {
  const rinex = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.rnx')
  const report = await convert(rinex, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-rinex-')), 'out'))
  assert.equal(report.sourceFormat, 'rinex')
  assert.equal(report.pageCount, 1)
})

test('converts ACIS SAT models safely through Node.js API', async () => {
  const sat = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.sat')
  const report = await convert(sat, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-sat-')), 'out'))
  assert.equal(report.sourceFormat, 'sat')
  assert.equal(report.pageCount, 1)
})

test('converts SED-ML experiments safely through Node.js API', async () => {
  const sedml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.sedml')
  const report = await convert(sedml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-sedml-')), 'out'))
  assert.equal(report.sourceFormat, 'sedml')
  assert.equal(report.pageCount, 1)
})

test('converts SBGN-ML maps safely through Node.js API', async () => {
  const sbgnml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.sbgnml')
  const report = await convert(sbgnml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-sbgnml-')), 'out'))
  assert.equal(report.sourceFormat, 'sbgnml')
  assert.equal(report.pageCount, 1)
})

test('converts COMBINE/OMEX archives safely through Node.js API', async () => {
  const omex = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.omex')
  const report = await convert(omex, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-omex-')), 'out'))
  assert.equal(report.sourceFormat, 'omex')
  assert.equal(report.pageCount, 1)
})

test('converts XDMF mesh metadata safely through Node.js API', async () => {
  const xdmf = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xdmf')
  const report = await convert(xdmf, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xdmf-')), 'out'))
  assert.equal(report.sourceFormat, 'xdmf')
  assert.equal(report.pageCount, 1)
})

test('converts VTK PVD collections safely through Node.js API', async () => {
  const pvd = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.pvd')
  const report = await convert(pvd, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-pvd-')), 'out'))
  assert.equal(report.sourceFormat, 'pvd')
  assert.equal(report.pageCount, 1)
})

test('converts FDS input decks safely through Node.js API', async () => {
  const fds = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.fds')
  const report = await convert(fds, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-fds-')), 'out'))
  assert.equal(report.sourceFormat, 'fds')
  assert.equal(report.pageCount, 1)
})

test('converts AbiWord documents safely through Node.js API', async () => {
  const abw = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.abw')
  const report = await convert(abw, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-abw-')), 'out'))
  assert.equal(report.sourceFormat, 'abiword')
  assert.equal(report.pageCount, 1)
})

test('converts NeuroML models safely through Node.js API', async () => {
  const neuroml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.nml')
  const report = await convert(neuroml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-neuroml-')), 'out'))
  assert.equal(report.sourceFormat, 'neuroml')
  assert.equal(report.pageCount, 1)
})

test('converts BioPAX pathway RDF/XML safely through Node.js API', async () => {
  const biopax = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.biopax.xml')
  const report = await convert(biopax, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-biopax-')), 'out'))
  assert.equal(report.sourceFormat, 'biopax')
  assert.equal(report.pageCount, 1)
})

test('converts XSD schemas safely through Node.js API', async () => {
  const xsd = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xsd')
  const report = await convert(xsd, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xsd-')), 'out'))
  assert.equal(report.sourceFormat, 'xsd')
  assert.equal(report.pageCount, 1)
})

test('converts XSLT stylesheets safely through Node.js API', async () => {
  const xslt = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xsl')
  const report = await convert(xslt, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xslt-')), 'out'))
  assert.equal(report.sourceFormat, 'xslt')
  assert.equal(report.pageCount, 1)
})

test('converts XSL-FO layouts safely through Node.js API', async () => {
  const fo = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.fo')
  const report = await convert(fo, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xslfo-')), 'out'))
  assert.equal(report.sourceFormat, 'xsl-fo')
  assert.equal(report.pageCount, 1)
})

test('converts XProc pipelines safely through Node.js API', async () => {
  const xproc = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xproc')
  const report = await convert(xproc, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xproc-')), 'out'))
  assert.equal(report.sourceFormat, 'xproc')
  assert.equal(report.pageCount, 1)
})

test('converts WADL descriptions safely through Node.js API', async () => {
  const wadl = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.wadl')
  const report = await convert(wadl, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-wadl-')), 'out'))
  assert.equal(report.sourceFormat, 'wadl')
  assert.equal(report.pageCount, 1)
})

test('converts OpenSearch descriptions safely through Node.js API', async () => {
  const osdd = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.osdd')
  const report = await convert(osdd, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-osdd-')), 'out'))
  assert.equal(report.sourceFormat, 'opensearch')
  assert.equal(report.pageCount, 1)
})

test('converts SAML metadata safely through Node.js API', async () => {
  const saml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.saml.xml')
  const report = await convert(saml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-saml-')), 'out'))
  assert.equal(report.sourceFormat, 'saml')
  assert.equal(report.pageCount, 1)
})

test('converts XACML policies safely through Node.js API', async () => {
  const xacml = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.xacml')
  const report = await convert(xacml, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-xacml-')), 'out'))
  assert.equal(report.sourceFormat, 'xacml')
  assert.equal(report.pageCount, 1)
})

test('converts legacy OpenOffice SXW/SXC/SXI packages through Node.js API', async () => {
  const root = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures')
  for (const extension of ['sxw', 'sxc', 'sxi']) {
    const source = path.join(root, `sample.${extension}`)
    const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), `docsvg-${extension}-`)), 'out'))
    assert.equal(report.pageCount >= 1, true)
  }
})

test('converts legacy Visio CFB documents safely through Node.js API', async () => {
  const vsd = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.vsd')
  const report = await convert(vsd, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-vsd-')), 'out'))
  assert.equal(report.sourceFormat, 'vsd')
  assert.equal(report.pageCount, 1)
})

test('converts HDF5 and CGNS superblocks safely through Node.js API', async () => {
  const root = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures')
  for (const extension of ['h5', 'cgns', 'exo']) {
    const source = path.join(root, `sample.${extension}`)
    const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), `docsvg-${extension}-`)), 'out'))
    assert.equal(report.sourceFormat, extension === 'h5' ? 'hdf5' : extension === 'cgns' ? 'cgns' : 'exodus')
    assert.equal(report.pageCount, 1)
  }
})

test('converts Apple iWork packages safely through Node.js API', async () => {
  const root = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures')
  for (const extension of ['pages', 'numbers', 'key']) {
    const source = path.join(root, `sample.${extension}`)
    const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), `docsvg-${extension}-`)), 'out'))
    assert.equal(report.sourceFormat, 'iwork')
    assert.equal(report.pageCount, 1)
  }
})

test('converts DWG header metadata safely through Node.js API', async () => {
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.dwg')
  const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-dwg-')), 'out'))
  assert.equal(report.sourceFormat, 'dwg')
  assert.equal(report.pageCount, 1)
})

test('converts Rhino 3DM marker metadata safely through Node.js API', async () => {
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.3dm')
  const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-3dm-')), 'out'))
  assert.equal(report.sourceFormat, '3dm')
  assert.equal(report.pageCount, 1)
})

test('converts Access ACE and Jet headers safely through Node.js API', async () => {
  const root = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures')
  for (const extension of ['accdb', 'mdb']) {
    const source = path.join(root, `sample.${extension}`)
    const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), `docsvg-${extension}-`)), 'out'))
    assert.equal(report.sourceFormat, 'access')
    assert.equal(report.pageCount, 1)
  }
})

test('converts Dassault 3DXML product structures safely through Node.js API', async () => {
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.3dxml')
  const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-3dxml-')), 'out'))
  assert.equal(report.sourceFormat, '3dxml')
  assert.equal(report.pageCount, 1)
})

test('converts DWFx fixed-page alias safely through Node.js API', async () => {
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.dwfx')
  const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-dwfx-')), 'out'))
  assert.equal(report.sourceFormat, 'xps')
  assert.equal(report.pageCount, 1)
})

test('preflights Nastran OP2 records safely through Node.js API', async () => {
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.op2')
  const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-op2-')), 'out'))
  assert.equal(report.sourceFormat, 'op2')
  assert.equal(report.pageCount, 1)
})

test('previews IPC-2581 PCB exchange structure safely through Node.js API', async () => {
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.ipc2581')
  const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-ipc2581-')), 'out'))
  assert.equal(report.sourceFormat, 'ipc2581')
  assert.equal(report.pageCount, 1)
})

test('previews Siemens JT header metadata safely through Node.js API', async () => {
  const source = path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'sample.jt')
  const report = await convert(source, path.join(await mkdtemp(path.join(os.tmpdir(), 'docsvg-jt-')), 'out'))
  assert.equal(report.sourceFormat, 'jt')
  assert.equal(report.pageCount, 1)
})
