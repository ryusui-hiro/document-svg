# document-svg プレビューガイド

[日本語](preview.ja.md) · [English](preview.en.md) · [简体中文](preview.zh-CN.md)

`document-svg`は、PDF・Office/OpenDocument・Apple Mail EMLX（`.emlx`）・Web/テキスト・reStructuredText（`.rst`、`.rest`）・図面・CAD/CAEなど対応形式を、アプリで安全に表示しやすいページ別SVGへ変換するNode.jsモジュールです。変換はRustのネイティブ処理をNode.jsのworkerで実行するため、イベントループを占有しません。

## できること

- PDF、legacy Word Binary `.doc`/`.dot`、text-only legacy PowerPoint Binary `.ppt`、旧Excel（`.xls` / `.xlsb`）、PPTX、XLSX、DOCX、Flat OPC（`.flatopc` / `.fopc` / `.flatopc.xml`）、AASX（`.aasx`）、OpenSCAD（`.scad`）、AMF（`.amf`）、PLMXML（`.plmxml` / `.plm.xml`）、STEP-XML（`.stepxml` / `.stpx`）、QIF（`.qif` / `.qif.xml`）、B2MML/JDF（`.b2mml` / `.jdf`）、ODT/ODS、Visio Open XML（`.vsdx` / `.vsdm` / `.vstx` / `.vstm`）と旧Visio XML（`.vdx`）、iCalendar（`.ics`）、legacy vCalendar（`.vcs`）、vCard contact（`.vcf` / `.vcard`）、MIMEメール（`.eml`）、Apple Mail EMLX（`.emlx`）、Outlook message（`.msg`）、MBOX archive（`.mbox`）、MHTML web archive（`.mht` / `.mhtml`）、DocBook 4/5（`.dbk` / `.docbook`）、DITA topic/map（`.dita` / `.ditamap`）、PDB coordinate models（`.pdb` / `.ent`）、HWPX（`.hwpx`）、COLLADA（`.dae`）、X3D（`.x3d`）、XMind（`.xmind`）、NIfTI（`.nii` / `.nii.gz`）、FITS（`.fits` / `.fit` / `.fts` / `.fits.gz`）、MRC（`.mrc` / `.map` / `.mrc.gz`）、SQLite（`.sqlite` / `.sqlite3` / `.db`）、mmCIF/PDBx（`.cif` / `.mmcif`）、MOL2（`.mol2`）、RDF Turtle（`.ttl` / `.nt` / `.nq`）、EPS/PostScript（`.eps` / `.ps`）、DICOM medical image（`.dcm` / `.dicom`）、HTML、EPUB、Jupyter notebook（`.ipynb`）、Quarto（`.qmd`）、R Markdown（`.Rmd`）、reStructuredText（`.rst` / `.rest`）、Org-mode（`.org`）、GNU gettext PO/POT翻訳カタログ（`.po` / `.pot`）、BibTeX bibliography（`.bib` / `.bibtex`）、raster PNG/JPEG/BMP/GIF/WebP、CBZコミックarchive、standalone JPEG 2000（`.jp2` / `.j2k` / `.j2c` / `.jpc` / `.jpx`）、複数ページTIFF/BigTIFF、glTF/GLB（`.gltf` / `.glb`）、OFF polygon mesh（`.off`）、IFC4 BIM（`.ifc` / `.ifczip`）、buildingSMART BCFZIP（`.bcfzip`）、KiCad PCB（`.kicad_pcb`）、ASCII XYZ/PCL PCD/ASTM E57/Leica PTS/PTX/ASPRS LAS/LAZ point cloud（`.xyz` / `.pcd` / `.e57` / `.pts` / `.ptx` / `.las` / `.laz`）、Abaqus mesh deck（`.inp`）、LS-DYNA Keyword（`.k` / `.key`）、MEDIT ASCII/binary (`.mesh` / `.meshb`)、Nastran Bulk Data（`.bdf` / `.nas`）、SU2 CFD mesh（`.su2`）、OpenFOAM（`.foam`）、ARFF (`.arff`), JSON-LD 1.1 (`.jsonld`, `.json-ld`), NetCDF classic (`.nc`, `.nc3`, `.cdf`), GraphML (`.graphml`), GEXF (`.gexf`), XGMML (`.xgmml`), Graph Modeling Language (`.gml`), CSV/TSV table、TOML configuration（`.toml`）、YAML 1.2 configuration（`.yaml` / `.yml`）、generic XML（`.xml` fallback）、Java Properties（`.properties`）、BPMN 2.0（`.bpmn` / `.bpmn2`）、CMMN case plan（`.cmmn`）、DMN decision table（`.dmn`）、ReqIF requirements（`.reqif`）、XMI model（`.xmi`）、ESRI ASCII Grid raster（`.asc`）、dBASE III/III+ table（`.dbf`）、GeoJSON/GeoRSS/GML/GPX/KML/KMZ/ESRI Shapefile（`.shp`）/WKT/EWKT、drawio、VTK、CADなどの対応入力を1ページずつSVG文字列へ変換
- 出力ファイルを残さないインメモリプレビュー
- Node.jsサーバー、Electronのmain process、サーバー側TypeScriptでの利用
- `<img>`向けBlob URL／Data URLの生成
- SVG画像またはSVGソースのクリップボードコピー
- 変換警告を`needsReview`で検出
- SVGページをPPTX、DOCX、XLSXへベクター画像として格納

DICOMDIR（`DICOMDIR` / `.dicomdir`）はDirectory Record offsetで階層をたどり、File IDを同じfile set folder内だけで解決します。

BCFZIP issue package（`.bcfzip`）はbuildingSMARTのproject/topic metadata、topic title/status/priority、markup countをbounded表示します。snapshot画像、IFC/model payload、document URL、協調actionは不活性で、entryの実行・外部参照は行いません。

Flat OPC package（`.flatopc` / `.fopc` / `.flatopc.xml`）は検証後にメモリ内で再構成し、既存のDOCX/XLSX/PPTX rendererへ渡します。macro、外部relationship、URL、active content、filesystem extractionは不活性です。

AASX package（`.aasx`）はAsset Administration Shellのrelationshipとspecification metadataをbounded表示します。supplementary CAD/manual file、identifier、value、URL、signature、encryption materialは不活性です。

Jupyter notebookは保存済みのcodeとoutputを表示するだけで、kernelを起動してcodeを実行することはありません。
Quarto/R Markdownのcode chunkとYAML execution optionも評価しません。

純粋なブラウザだけで文書を変換するWASMモジュールではありません。文書変換はNode.js側で実行し、表示ヘルパー`document-svg/preview-ui`だけをrenderer／ブラウザ側で使います。

## インストール

公開後:

```bash
npm install document-svg
```

このリポジトリから試す場合:

```bash
cd bindings/node
npm install
npm run build
npm test
```

Node.js 18以上が必要です。配布版では実行OS／CPUに合うネイティブパッケージも同時にインストールされます。

## 最小例: 文書をSVG文字列へ変換

```js
const { preview } = require('document-svg')

async function main() {
  const report = await preview('slides.pptx', { maxPages: 100 })
  console.log(`${report.pageCount} pages`)
  console.log(`review required: ${report.needsReview}`)
  const firstPageSvg = report.pages[0]?.svg
  console.log(firstPageSvg)
}
main().catch(console.error)
```

`report.pages`にはページ番号、SVG、pt単位の幅と高さ、警告、推定IRサイズが入ります。一時出力はPromiseの完了前に削除されます。

## `<img>`で表示する

```js
import {
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
} from 'document-svg/preview-ui'

const url = createSvgPreviewUrl(svgMarkup)
imageElement.src = url

// ページを差し替える時や画面を破棄する時に必ず解放する
revokeSvgPreviewUrl(url)
```

プロセス境界、Markdown、Blob URLを共有できないrendererにはData URLを使えます。

```js
import { createSvgPreviewDataUrl } from 'document-svg/preview-ui'
imageElement.src = createSvgPreviewDataUrl(svgMarkup)
```

Data URLは文字列が大きくなるため、同一画面内ではBlob URLを推奨します。

## React例

Blob URLの作成・解放、コピー操作、`aria-live`の状態表示まで含む実装は[`../examples/SvgPreview.tsx`](../examples/SvgPreview.tsx)にあります。

```tsx
const report = await preview(filePath)
return <SvgPreview svg={report.pages[currentPage].svg} />
```

## 実行できるHTML生成例

同梱の例は、文書の全ページを1つのHTMLプレビューへ書き出します。

```bash
npm run example:preview -- ./slides.pptx ./preview.html
```

実装は[`../examples/preview-to-html.cjs`](../examples/preview-to-html.cjs)を参照してください。変換警告がある場合はHTML内に表示し、終了コードを`2`にします。

## クリップボードへコピー

```js
import { copySvgToClipboard } from 'document-svg/preview-ui'

button.addEventListener('click', async () => {
  const copiedType = await copySvgToClipboard(svgMarkup)
  console.log(copiedType) // image/svg+xml または text/plain
})
```

クリップボードAPIはHTTPS／localhostなどのsecure contextと、クリック等のユーザー操作を要求する場合があります。ブラウザがSVG MIMEを扱えない場合は、正確なSVGソースをテキストとしてコピーします。

## 警告と安全上限

```js
const report = await preview('report.docx', {
  maxInputBytes: 512 * 1024 * 1024,
  maxZipEntryBytes: 128 * 1024 * 1024,
  maxPages: 100,
  maxXmlEvents: 20_000_000,
  maxSvgBytes: 64 * 1024 * 1024,
  maxTotalSvgBytes: 256 * 1024 * 1024,
  jobs: 1,
})

if (report.needsReview) {
  console.warn(report.warnings)
  console.warn(report.pages.flatMap((page) => page.warnings))
}
```

`preview-ui`は`script`、イベント属性、外部URL、DOCTYPE、animation等を含むSVGを拒否します。表示には`innerHTML`ではなく`<img>`を使ってください。重要文書は元文書との目視比較を行い、警告ゼロを画素単位の完全一致と解釈しないでください。

## APIの使い分け

| API | 用途 | ファイルを残す |
|---|---|---|
| `preview(input, options)` | UI向けSVG文字列 | いいえ |
| `convert(input, output, options)` | バッチ変換、成果物保存 | はい |
| `reverse(input, output, options)` | SVGをOffice形式へ格納 | はい |
| `createSvgPreviewUrl(svg)` | 同一rendererの`<img>`表示 | Blob URLのみ |
| `createSvgPreviewDataUrl(svg)` | プロセス境界を越える表示 | いいえ |
| `copySvgToClipboard(svg)` | 画像／ソースをコピー | いいえ |

変換後のSVGは編集可能な見た目を優先しますが、Officeの段落・セル・数式等の意味構造を保持するものではありません。

## ライセンス

`MIT OR Apache-2.0`。いずれかを選択できます。再配布時は、選択したライセンスに従い著作権表示・ライセンス文を必ず保持してください。[全文](../LICENSE)。
