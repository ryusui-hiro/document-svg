# アーキテクチャ

## 変換フロー

```text
PDF ───────────────┐
PPTX ZIP/XML ──────┤
XLSX ZIP/XML ──────┼─> Page IR ─> streaming SVG writer ─> page-NNNN.svg
DOCX ZIP/XML ──────┤                         └────────────> conversion.json
drawio mxGraphModel┤
XPS/OpenXPS ───────┤
TIFF/BigTIFF ──────┤
CBZ page images ───┤
CAD/CAE ───────────┘

SVGファイル／ディレクトリ ─> bounded SVG reader ─> package / format writer
                             ├─> Office (PPTX, DOCX, XLSX)
                             ├─> drawio（1 SVG / diagram、sourceを持つSVGは元の`<diagram>`を復元）
                             ├─> CAD / CAM (IFC, DXF, G-code, Gerber, HP-GL, Excellon, STEP, IGES)
                             ├─> 3D / CAE (STL, OBJ, PLY, 3MF, SU2, Tecplot, EnSight, PLOT3D, VRML, Gmsh, VTK, OpenDRIVE, OpenSCENARIO)
                             ├─> Data / Math (CSV, Markdown, LaTeX)
                             ├─> Web / UI (HTML viewer, React JSX/TSX, Vue 3, Svelte, SVG path)
                             └─> Raster (PNG, WebP)
```

drawioは`mxfile`の`<diagram>`ごとに1ページを生成します。本文は生XMLか、`encodeURIComponent`＋raw deflate＋base64のいずれかで、後者は`max_zip_entry_bytes`を上限に展開します。モデル座標はCSSピクセルなので、page rootの1 groupがピクセル→ポイント変換とcropのoffsetをまとめて持ち、図形geometry、線幅、font sizeを同じ倍率で保ちます。

全入力形式は`Page`、`Node::{Path,Text,Image,Group}`、`Paint`、`Stroke`、`ClipPath`、`MaskDefinition`からなる共通IRへ正規化します。座標はポイント、変換はSVGと同じ6要素アフィン行列です。`TextRun`はrun全体の期待advanceに加え、PDF由来の証明可能なglyph x originも保持できます。SVGライターは入力順と固定精度で直列化し、属性順も決定的です。

## 言語バインディング

```text
Python ─> PyO3 / GIL detach ─┐
                              ├─> convert ─> SVGページ + report
                              ├─> preview ─> メモリ上SVG文字列 + report
                              ├─> transform ─> 最適化/メタデータ削除済みSVG
Node.js ─> napi AsyncTask ───┼─> convert / preview / transform
                              └─> reverse ─> Office / CAD / Web / ラスター形式

Browser Worker ─> wasm-bindgen ─> convert_bytes ─> page callback ─> SVG + text geometry
```

`bindings/python`と`bindings/node`はパーサーやIRを再実装せず、パス、
`ConvertOptions`、`ConversionReport`、言語固有エラーの変換だけを担当します。
PythonはJSON経由で`dict`へ変換し、Node.jsは型付きNode-API objectへ直接変換します。
Node.jsの`preview`は同じ`convert_path`を専用一時ディレクトリに対して実行し、生成SVGを
1ページ64 MiB・全ページ256 MiBの既定上限内でUTF-8文字列として読み込みます。一時
ディレクトリは成功・失敗のどちらでもRAIIで削除されます。
`document-svg/preview-ui`はネイティブbindingから独立したbrowser-safeなJavaScript
subpathです。Blob URLの生成・破棄とClipboard APIのSVG MIME／text fallbackだけを担当し、
文書parserやファイルシステム処理をrendererへ持ち込みません。
`bindings/wasm`はファイル名と`&[u8]`を受け取るin-memory変換経路を使います。対象は
PDF/DOCX/XLSX/PPTXで、PDFはページ単位、OOXMLは必要partだけをseek可能なmemory cursorから
読み、`PageConsumer`がSVGとテキスト位置をWorkerへ逐次返します。静的デモはWorker内で
同期WASMを実行し、document bytesはWebサーバーへ送りません。検索・選択用のテキスト幾何も
boundedな別レイヤーとして送り、元のページSVGは`<img>` Blob URLとして表示します。
ブラウザ版はSQLite/DICOM/legacy Officeをビルド対象から外し、JPEG 2000は安全な警告付き
SVGフォールバックにします。既定で入力64 MiB、拡張part 32 MiB、全SVG 128 MiB、テキスト位置
50,000 span/8 MiBに制限します。上限を超えたテキスト位置はwarningとして返し、ページ画像自体は
継続して表示します。
ネイティブホストに組み込まれるためrelease profileは`panic = "unwind"`とし、
バインディング境界がエラーへ変換できる設定にします。

PDF効果は、要素自身のtransformと混同しないようidentity座標の外側groupへ適用します。soft mask、blend、isolationは外側group、geometry transformは内側nodeです。複数clipは定義同士を参照せず、親clipから順に外側groupを重ねるため、librsvgやブラウザ間で安定して交差します。

## メモリ設計

- 入力全体: `max_input_bytes`で事前制限します。
- OOXML: ZIP中央ディレクトリだけを開き、必要なpartを1件ずつ読みます。展開後の各partは`max_zip_entry_bytes`で制限します。
- TIFF/BigTIFF: 画像directoryごとにdecodeし、decoder buffer、ページ/全体pixel数、PNG data URI、`max_pages`を制限します。planar imageは全planeの出力長を検査します。
- Standalone JPEG 2000: the native-only converter reuses the codestream/JP2 SIZ preflight already used by PDF and DICOM, validates component precision/dimensions/signedness before OpenJPEG allocation, then emits an 8-bit PNG-backed page for grayscale, gray-alpha, RGB or RGBA images.
- CBZ: archive pathを抽出せず、PNG/JPEG memberだけをnatural filename orderで1ページずつ処理します。`max_zip_entry_bytes`と固定上限でentry数、画像名の合計、画像サイズ、ページ/全体pixel数、総data URI bytesを制限します。
- Abaqus INP: flat meshとpart/instance meshをparseし、各instanceのtranslation後にaxis-angle rotationを適用して共通simulation rendererへ渡します。外部include、非Cartesian node system、analysis fields/materialsは読み込まず、対応外要素等はwarning/errorにします。
- Nastran Bulk Data: bounded UTF-8/ASCII parserでGRIDとfree/small/large-field structural cardsを読み、external INCLUDEやsolver dataを解釈せず共通simulation rendererへ渡します。nonzero GRID CPは拒否し、高次要素をcorner meshへ簡略化、3D meshはXY投影します。
- SU2 native mesh: bounded ASCII parserで`NDIME`/`NELEM`/`NPOIN`/`NMARK`のgeometryだけを読み、2D cellまたは3D volume-edge outlineを共通simulation rendererへ渡します。zero-based connectivityを検査し、3DはXY投影、solver boundary conditionsは評価しません。
- Tecplot ASCII: `TITLE`/`VARIABLES`/`ZONE`を上限付きで読み、POINT/BLOCKの線形有限要素zoneとordered I/J gridを共通simulation rendererへ渡します。X/Yと1-based connectivityを検査し、追加の数値変数を不活性scalarとして使います。zone metadataやsolver/macroは評価しません。
- EnSight Gold ASCII: case manifestのgeometry modelをcase root内へcanonicalizeし、`part`/`coordinates`/linear element blockを検証して共通simulation rendererへ渡します。variable file、binary case、外部path、`nsided`/`nfaced`は評価しません。
- PLOT3D formatted ASCII: single/multi-blockの`ni`/`nj`/`nk`とX/Y/Z配列を上限付きで検証し、I-fastestのstructured cellを共有simulation rendererへ渡します。3Dはboundary surfaceだけを描画し、unformatted Fortran、IBLANK、solution `.q`は評価しません。
- VRML97: `Coordinate`のXYZと`IndexedFaceSet`/`IndexedLineSet`の`coordIndex`だけをboundedに抽出し、生成したvalidated meshを陰影付きOBJ rendererへ渡します。TextureCoordinate、transform、appearance、script、route、外部URLは評価しません。
- SYLK/DIF: cell/tuple coordinates and cached values are parsed into the shared paginated table renderer. Formula strings, advisory dimensions, formatting metadata and external content remain inert.
- FASTA/FASTQ: sequence and quality records are bounded before table rendering; FASTQ sequence/quality length is checked, while identifiers are treated as text and no alignment or Phred conversion is performed.
- GFF3/GTF: 9列feature recordsを座標・strand・phase・attributesの検証後に不活性tableへ渡します。directiveと埋め込みFASTAは読み込まず、sequence解析や外部link解決はしません。
- BED/bedGraph: 0-based half-open genomic intervalsを12列まで検証し、block listとfinite signal値を不活性表へ渡します。track/browserやremote genome/trackは解決しません。
- VCF: `#CHROM`の8固定列とFORMAT/sample列を上限付き表へ渡し、POS/QUAL/field長を検証します。reference URL、genotype/phasing、埋め込みFASTAは不活性です。`.vcf`はvCardと内容署名で区別します。
- SAM: 11 mandatory alignment fieldsとoptional TAG:TYPE:VALUEをbounded tableへ渡し、CIGAR/SEQ/QUALとtag重複を検証します。reference genome、projection、genotype/phasing、外部resourceは評価しません。
- WIG/Wiggle: `fixedStep`/`variableStep`を1-based fully-closed interval rowsへboundedに展開し、step/spanとfinite signalを検証します。track/browserと外部trackは評価しません。
- MAF: `a`/`s` alignment blockを座標・strand・size・sequence長検証後に不活性表へ渡し、`i/e/q`とコメントは診断に留めます。reference lookupとcoordinate liftは実行しません。
- Newick: 最初の`;`までの括弧/カンマtopologyをbounded recursive descentで共有`DiagramGraph`へ変換し、quoted/unquoted label、finite branch length、nested bracket commentを検証します。追加tree、taxonomy、sequence lookup、rooting/distance計算は扱いません。
- Stockholm: `# STOCKHOLM 1.x`と`//`区切りのalignment blockを読み、分割sequence rowを連結して共有table rendererへ渡します。`#=GF/GS/GC/GR` annotationは不活性metadataとして扱い、sequence長一致・入力/行/byte上限を検証します。
- CLUSTAL: header後のblockごとのsequence fragmentをname別に連結し、任意のresidue countと`* : . +` consensus lineを不活性として共有table rendererへ渡します。aligned length一致、行長/name/sequence byte上限を検証します。
- NEXUS: `#NEXUS`の`BEGIN TREES`から最初の`TREE/UTREE`文を安全に切り出し、Newick parserと共通`DiagramGraph` rendererを再利用します。TAXA/TRANSLATE/他blockは外部解決せず診断に留めます。
- GenBank: `LOCUS`から`//`までをbounded recordとして読み、固定幅FEATURESを件数化し、ORIGINの配列文字を検証して共通table rendererへ渡します。accession、feature location、外部sequenceは解決しません。
- EMBL-Bank: 2文字tagのID/AC/DE/FT/SQを`//`単位にbounded parseし、FT固定幅featureを件数化、SQ行の累積countと配列文字を検証して共通table rendererへ渡します。DB cross-referenceと外部sequenceは解決しません。
- UniProtKB: `Reviewed;`/`Unreviewed;`を含むID signatureで`.dat`をTecplot/generic textと分離し、ID/AC/DE/GN/OS/FT/SQを`//`単位にbounded parseします。protein metadataとfeature件数を共通tableへ渡し、REST/APIやcross-referenceは呼びません。
- RIS: `TY  -`をrecord開始、`ER  -`を終了とするcanonical tag行を検証し、繰り返し著者/keywordと6-space continuationを共通table rendererへ渡します。DOI/URLや外部citation resourceは解決しません。
- SPICE: title以後のelement cardと`+` continuationを論理行へboundedに結合し、device prefixごとの最小node数、node/value byte上限を検証して不活性tableへ渡します。dot command、include/lib path、model、control sectionは実行・openしません。
- KiCad legacy Eeschema: `$Comp`/`Wire Wire Line`/`Text Label`/`Connection ~`をmilsからpage-fitしたIRへ変換し、symbol libraryやcache.libに依存しない簡略component/wire描画を行います。modern `.kicad_sch`、gEDA、Eagleは別形式として扱います。
- KiCad modern schematic: PCB S-expression arenaを共用し、`lib_id`付きsymbol instance、property、`wire`/`bus`の`xy`、label/text/junctionを抽出してmm座標の簡略IRへ変換します。`lib_symbols`定義、pin connectivity、hierarchy、image/model、ERC/DRCは解析しません。
- LTspice: `Version`/`SHEET`から始まる行指向`.asc`をUTF-8/UTF-16でbounded decodeし、WIRE/FLAG/SYMBOL/SYMATTR/TEXTと基本graphicを簡略IRへ渡します。`.asc`のESRI Grid署名を先に判定し、`.asy`/model/includeとsimulatorは呼びません。
- EAGLE XML: `<eagle>` root署名の`.sch`だけをquick-xmlで解析し、parts/instancesとplain/netのwire・label・textを座標検証後に簡略IRへ渡します。KiCad/gEDA/Eagle以外の`.sch`は誤認せず、外部library/スクリプト/DRCは評価しません。
- LS-DYNA Keyword: bounded UTF-8 parserで`*NODE`とshell/solid/beam element cardsを読み、standard/comma-free rowsを共通simulation rendererへ渡します。long-format、structured decks、外部INCLUDE、solver properties/resultsは解釈せず、high-order geometryをcorner meshへ簡略化します。
- Jupyter `.ipynb`: bounded nbformat-4 JSONをMarkdown/HTML block rendererへ変換します。cell順とhidden metadata、text/HTML/JSON/error/PNG/JPEG outputを扱い、codeは実行しません。interactive/SVG outputsとattachment、外部参照は表示しません。
- Jupyter image output: PNG/JPEG signature/decode validation happens before data-URI embedding, with per-image/aggregate encoded-byte and pixel limits; SVG MIME and remote/attachment images remain unembedded.
- Quarto/R Markdown: bounded Markdown sourceとsimple YAML metadataをHTML block rendererへ渡し、code chunksやinline codeはsource表示のみとします。YAMLのその他設定、filters、cross-references、includes、code executionは行いません。
- Microsoft Project XML: DTDを拒否するbounded `quick-xml` parserでtask date/progress/dependency fieldsだけを抽出し、limits適用後に25-task Gantt pagesをPageConsumerへstreamingします。外部resourceは読まず、calendar/schedule engineは起動しません。
- KiCad PCB: a bounded UTF-8 S-expression arena shares source slices, caps nesting/tokens/text/geometry, and emits common tracks/pads/outlines plus cached zone polygons. External libraries, images and 3D models are never opened.
- OFF: ASCII COFF/NOFF polygon records are validated, converted to bounded OBJ text, and rendered through the existing shaded 3D mesh path. Binary/4D data and optional colors/normals are not imported.
- IFC-SPF/IFCXML/IFCZIP: a bounded ZIP reader selects one root-level `.ifc` model without extracting the package. The Part 21 and bounded buildingSMART IFCXML readers resolve IFC4 tessellated faces and common `IfcExtrudedAreaSolid` profiles (rectangle, 64-segment circle, closed 2D polyline) through product shape representations and local placements. IFCXML internal IDs are indexed in-memory; DTDs and external URI references are never processed. Both readers expand nested `IfcMappedItem`/`IfcRepresentationMap` references with mapping-origin inversion and bounded 3D Cartesian transforms, checking cycles and total expanded mesh size. Convex polygons use fan triangulation; simple concave polygons up to 256 vertices use bounded ear clipping. IFCZIP sidecar resources are ignored.
- dBASE III/III+ `.dbf`: the reader validates bounded fixed-width headers/records, decodes common `.cpg` encodings, skips deleted rows, and renders 100-row/32-column batches. Memo files and unsupported binary field types are not opened.
- TOML: a bounded TOML 1.1 parser renders nested tables, arrays and scalar values as inert key paths. It enforces line, recursion, value, string, path and rendered-text limits; configuration is never evaluated.
- YAML: a bounded event-driven YAML 1.2 parser renders multiple documents and nested values as inert key paths. Aliases are not expanded, custom tags are not applied, complex keys are rejected, and event, depth, document, scalar, path and rendered-text limits are enforced.
- Generic XML: unrecognized XML falls back to a bounded namespace-aware element/attribute/text preview. DTDs are rejected, so external entities are never loaded; recognized Project, draw.io, SVG, HTML and other XML formats keep their specialized converters.
- Java Properties: a bounded ISO-8859-1 line parser decodes Java escapes and continuations, then renders inert key/value rows. Duplicate keys use the last value as `Properties.load` does; variable expansion is not performed.
- BPMN 2.0: the bounded XML tree resolves BPMN model IDs against BPMN Diagram Interchange shapes/edges and their explicit coordinates. It draws common events, activities, gateways and sequence flows without executing process semantics; diagrams without DI geometry are rejected.
- DMN 1.1–1.5: decision tables become bounded inert SVG tables; local requirement references are resolved, but FEEL and hit-policy evaluation never run. External references, DMNDI diagram layout and unsupported decision logic are omitted or warned.
- CMMN 1.1: the bounded XML tree resolves case-plan model IDs to CMMNDI Bounds and waypoints, drawing plan items and connectors while leaving sentries and lifecycle semantics inert.
- ReqIF: the bounded XML tree indexes specification objects, typed values, hierarchy references and local relations into requirement tables. XHTML is flattened, and no external or tool-specific content is processed.
- XMI: model elements are traversed as a bounded containment tree, with qualified xmi:id/xmi:type and attribute references retained as text. External links and model semantics are never resolved or executed.
- CSV/TSV: the parser enforces quote placement, doubled-quote escaping, row/field/cell limits and bounded page count before emitting 100-row/32-column batches. It embeds exact input only for a one-page source of at most 8 MiB; other files keep visual pages without duplicating the source payload.
- ARFF: the bounded Weka dataset parser validates flat `@relation`/`@attribute` headers, dense CSV-like records, and sparse zero-default records before reusing the paginated table renderer. Relation-valued attributes and value evaluation stay inert; line, attribute, record, cell, and value limits are enforced.
- JSON-LD: a bounded JSON tree walker turns `@id`/`@type` nodes, compact predicates, literals, lists, included nodes, and named graphs into inert RDF-style rows. Contexts stay compact and remote `@context`/`@import` references are never resolved; depth, value, statement, term, and rendered-text limits apply.
- OpenAPI/Swagger: JSON uses a bounded tree walk while YAML is validated through the event parser before a conservative indentation scan extracts specification metadata and HTTP operations. `$ref`, externalDocs, servers, callbacks, links, examples, and security schemes remain inert; no reference resolution, network access, or API execution occurs.
- WSDL: a bounded XML tree indexes services, ports, bindings, interfaces/portTypes, operations, messages, imports/includes and embedded schemas into inert rows. Endpoint addresses and external schema/import locations remain unvisited.
- OPML: a bounded XML tree walks the required head/body outline hierarchy and emits inert title, type, depth and feed-outline metadata. Feed URLs, owner addresses and extension values remain unvisited.
- RSS/Atom feeds: a bounded XML tree summarizes channel/feed and item/entry title/date plus link, author, content and enclosure counts. Link targets and payload text remain unvisited.
- Apple Property Lists: XML dictionaries/arrays are flattened into bounded key paths and typed rows; binary `bplist00` trailers, offset tables and object markers are preflighted without dereferencing arbitrary payloads.
- TEI P5: a bounded XML tree extracts `teiHeader` metadata and walks common `text/body` division, heading, paragraph, note, list, figure and table structures. Targets and facsimile resources remain inert.
- ALTO: a bounded XML tree indexes `Layout/Page/TextLine/String` OCR structure and validated `WC` confidence values; source image references and processing metadata remain inert.
- METS: a bounded XML tree summarizes `fileSec`/`fileGrp` files and nested `structMap` divisions, while `fptr`/`mptr`/location references are counted but never dereferenced.
- MARCXML: a bounded XML tree walks record/leader/controlfield/datafield/subfield structure, redacting URL field values and leaving catalog references inert.
- MODS: a bounded XML tree extracts title/name/origin/genre/subject/identifier/location structure; authority and location URIs remain unvisited.
- MARC21 ISO 2709: a bounded binary reader validates leader/directory/field boundaries and emits control/data/subfield rows; `.mrc` stays assigned to microscopy volumes.
- PREMIS: a bounded XML walk counts and summarizes Object, Event, Agent and Rights entities while redacting checksums, URIs and preservation payloads.
- IIIF Presentation: a bounded JSON walk summarizes Manifest/Collection labels, Canvases, painting annotations, ranges and dimensions without dereferencing image services or annotation IDs.
- EAD: a bounded XML walk extracts finding-aid repository/title metadata and nested component levels/titles/dates while leaving digital-object references inert.
- EAC-CPF: a bounded XML walk extracts authority identity names and entity/relation counts while leaving authority URIs, biographies and targets inert.
- Dublin Core XML: a bounded XML walk keeps descriptive DCMI elements as safe value rows while suppressing identifier/relation/rights/description payloads and URLs.
- ISO 19115/19139: a bounded XML walk summarizes identification, dates/topics, hierarchy/language, CRS codes, geographic bounding boxes and quality/distribution counts while suppressing abstract/lineage, contact, identifier and online-resource payloads.
- DICOM Structured Reports: the existing bounded Part 10 parser validates SR SOP classes and walks Content Sequence items recursively, exposing safe value types/concepts/units while keeping referenced objects, UIDs and patient/study attributes inert.
- UBL: a bounded XML walk recognizes namespaced Invoice/Order and related document roots, emits dates/currency and structural counts, and suppresses party, amount, identifier, attachment and URL payloads.
- XBRL: a bounded XML walk indexes instance contexts and units, then emits deterministic top-level fact rows with period/unit/precision metadata while suppressing entity identifiers, taxonomy/linkbase references and URLs.
- FDF: the PDF object parser is fed a normalized bounded FDF signature, then walks `/FDF/Fields` dictionaries recursively with password/value/action limits; form submission, JavaScript and embedded resources remain inert.
- XFDF: a bounded XML walk emits fully-qualified field/value rows, sensitive-field masks and annotation/target counts while keeping PDF references, rich text, actions and URLs inert.
- LandXML: a bounded XML walk summarizes namespaced civil roots, TIN surface counts, alignment segment counts, survey points, parcels and pipe networks while suppressing coordinate and external-reference payloads.
- MathML: a bounded XML walk converts presentation tokens and common layout schemata into deterministic formula text while suppressing annotation-xml, images, scripts and URLs.
- XMP: a bounded RDF/XML walk emits common descriptive properties and packet counts while suppressing identifiers, thumbnails, URLs, private schemas and embedded binary payloads.
- XDP: a bounded XML walk inventories XFA packet and field structure while leaving field values, embedded packets, scripts, events, submit actions and external references inert.
- SpreadsheetML: a bounded XML walk emits Office 2003 XML Spreadsheet worksheet/cell values and formula presence while leaving styles, macros, links and recalculation inert.
- CML: a bounded XML walk summarizes molecule/reaction/spectrum structure and element distributions while suppressing coordinates, dictionaries, URLs and chemistry calculations.
- RDF/XML: a bounded XML walk emits predicate/literal rows and node/IRI counts while suppressing resource URLs, nested XML literals, vocabulary resolution and inference.
- BCFZIP: a bounded ZIP reader validates `bcf.version` and `markup.bcf`, then summarizes project/topic metadata and markup counts. Snapshots, IFC/model payloads, document URLs and collaboration actions are never opened or executed.
- Flat OPC: a bounded XML reader validates the Office XML package namespace and part names, reconstructs XML/base64 parts into an in-memory ZIP, and delegates to the existing DOCX/XLSX/PPTX renderers. The temporary package never touches disk and active/external content remains inert.
- AASX: a bounded OPC relationship walk locates the AAS origin and specification parts, parses only XML/JSON shell metadata, and records supplementary-file/thumbnail counts. CAD, manuals, URLs, signatures and encryption payloads are not opened.
- OpenSCAD: a bounded line scanner strips comments and counts source constructs without invoking the OpenSCAD interpreter. import/use/include paths and surface/file references remain inert.
- AMF: a bounded XML walk converts object-local vertices and volume triangles to an in-memory OBJ stream, then reuses the shaded mesh renderer. Material, texture, lattice, slice and external reference data remain inert.
- PLMXML: a bounded XML walk summarizes product definitions, assembly structures, representations and external-reference counts without resolving IDs, URLs or referenced CAD geometry.
- STEP-XML: a bounded XML walk recognizes ISO 10303-28 roots and summarizes EXPRESS-mapped product, assembly, representation and geometry element counts without schema resolution or external CAD access.
- QIF: a bounded XML walk summarizes product, inspection-plan/result, feature, characteristic, datum and traceability structure while keeping measured values and external inspection files inert.
- B2MML: a bounded XML walk summarizes ISA-95 manufacturing schedules, performance, resources and process segments while keeping operations, values, credentials and external resources inert.
- JDF/JMF: a bounded XML walk summarizes CIP4 job nodes, resources, processes and print-production references while never sending commands or following links.
- S1000D: a bounded XML walk extracts DMC code attributes and common data-module content while leaving DM/ICN references, graphics and publication assembly inert.
- AsyncAPI: JSON uses the same bounded tree walk pattern and YAML is validated before a conservative indentation scan extracts channels, publish/subscribe actions, and operation metadata. AsyncAPI 3.x top-level send/receive operations are supported; references, server URLs, protocol bindings, examples, and security schemes remain inert.
- JSON Schema: a bounded JSON tree walk emits root metadata and nested property/definition paths with type and constraint summaries. YAML is validated by the existing event parser before conservative property extraction; `$ref`, URI values, patterns, examples, and format annotations are never resolved or evaluated.
- ANSYS CDB: the CAE simulation layer reads only fixed-width ASCII `NBLOCK` and `EBLOCK` mesh sections (with basic `N`/`EN` fallback), validates IDs/connectivity and passes `MeshNode`/`MeshCell` data to the shared renderer. APDL commands and include paths stay inert; Z is projected onto XY with a warning.
- HAR: a bounded JSON tree walk extracts request/response metadata and timings while masking secret-like query values and dropping headers, cookies, and bodies. URLs remain inert and no network replay or resource lookup occurs.
- WARC: a bounded record-frame reader validates header delimiters and `Content-Length`, skips payload bytes, and summarizes embedded HTTP status/MIME. Plain and gzip streams share the same size limits; target URIs remain inert and secret-like query values are masked.
- WACZ: the ZIP package layer validates entry counts and reads only `datapackage.json` plus `pages/pages.jsonl`; archive WARC payloads and CDXJ indexes are intentionally not opened. Page metadata uses the same inert URL masking boundary as WARC/HAR.
- Postman Collection: a bounded JSON tree walk recursively flattens folder/request items into an inert request inventory. URL query secrets are masked while auth credentials, variables, scripts, headers, cookies, and bodies are omitted; no collection runner or network client is invoked.
- GraphQL SDL: a bounded line parser tracks definition braces and argument lists, then emits type/interface/input/enum/scalar/union fields and signatures. Descriptions, directives, default values and URLs remain inert text; no GraphQL execution or introspection transport is used.
- Protocol Buffers: a bounded line parser tracks message/enum/service/oneof braces and emits field numbers, types, options and RPC signatures. Imports, options, annotations and defaults remain inert; no `protoc`, external file or RPC call is invoked.
- Kubernetes manifests: JSON uses a bounded object walk for resource identities and `List.items`, while YAML is validated through the event parser before extracting multiple `apiVersion`/`kind`/`metadata` documents. Secret values and spec payloads remain omitted; no kubectl, Helm, cluster request, or external reference is used.
- Docker Compose: JSON uses a bounded object walk for the required `services` map; YAML is validated through the event parser before a conservative indentation scan extracts service image/build and resource-reference counts. Commands, healthchecks, environment/config/secret values, interpolation, include/merge semantics and all Docker runtime operations remain inert.
- GitHub Actions workflows: YAML is validated through the event parser before a bounded indentation scan extracts the workflow name, triggers, job runners, `needs`, steps, action/run counts and strategy presence. Shell commands, expressions, secrets, environments, permissions, reusable workflows and runner/network operations remain inert.
- JUnit-compatible reports: the shared bounded XML tree validates `testsuite`/`testsuites` roots, then suite children are summarized from testcase outcome elements and safe attributes. Failure/error logs, stdout/stderr, properties, attachments and external resources are never rendered or executed.
- SARIF 2.1.0: a bounded JSON walk validates the version and `runs` array, resolves only safe tool/rule identifiers and default levels, and aggregates result severities by tool/rule. Messages, locations, URIs, snippets, fingerprints, fixes and invocation data remain omitted; no upload or code-scanning API is called.
- Terraform JSON plans: a bounded JSON walk validates major format version 1 and extracts only `resource_changes` addresses, types, action sets and action reasons, plus aggregate drift/output/variable counts. `before`/`after`/`sensitive_values`, state, configuration and provider data stay omitted; no Terraform command or provider/network operation is invoked.
- CycloneDX BOMs: bounded JSON validation checks `bomFormat`/`specVersion`, while the shared XML tree checks the CycloneDX BOM namespace. Component type/name/version/scope rows are emitted with aggregate dependency/vulnerability/service counts; serials, refs, hashes, licenses, PURLs, properties, vulnerability details and external references remain inert.
- SPDX 2.x JSON/tag:value: bounded JSON validation checks the `spdxVersion`/`SPDXID` signature; the tag:value scanner bounds lines and extracts only package/file identity fields. Both paths emit relationship, annotation, snippet and external-document counts while checksums, licenses, suppliers, PURLs, download locations, comments and URIs remain omitted.
- JaCoCo/Cobertura coverage XML: the shared bounded XML tree accepts `report` or `coverage` roots, extracts package-level class counts and safe `counter` or rate attributes, and emits covered/missed summaries. Source files, session data, logs, attachments and quality gates remain inert.
- LCOV: a bounded line parser handles tracefile sections and counter records, reduces `SF` paths to basenames, and emits file-level line/function/branch summaries. `DA`/`BRDA` execution records and function names remain omitted; no source or runtime data is opened.
- ASAM OpenDRIVE: a bounded XML tree reads local road `planView` geometry, tessellates line/constant-curvature arc reference lines, normalizes metre coordinates into a planar map, and leaves lanes, profiles, signals, objects, links and simulation behavior inert.
- ASAM OpenCRG: a bounded line scanner reads only clear-text section headers and line counts; road-surface ASCII/binary payloads and file references are not decoded or followed.
- ASAM OpenSCENARIO: a bounded XML tree indexes FileHeader, ScenarioObject entities, Storyboard hierarchy, parameter declarations, catalog references and RoadNetwork presence into inert tables. Catalog paths, controllers, expressions, triggers and simulator semantics are never resolved or executed.
- ASAM OpenLABEL: a bounded JSON walk validates the `openlabel` envelope, counts annotation collection maps and nested object/frame data, and emits collection rows. Sensor payloads, coordinates, values, ontology URLs and resources remain inert.
- CityGML: a bounded XML tree counts thematic city objects, LoD tags, Envelope/CRS and XLinks into inert tables; geometry, textures, attributes and external resources stay omitted.
- CityJSON: a bounded JSON walk validates CityObjects, shared vertices, transform arrays and geometry boundary indices, then emits type/geometry/LoD counts without expanding coordinates or semantic payloads.
- STIX JSON: a bounded JSON walk validates bundle/object envelopes, counts object types, labels and reference arrays, and emits inert type/id/timestamp rows. Indicator patterns, descriptions, hashes, URLs, marking content and custom properties stay omitted; no TAXII/API/network operation runs.
- TAXII JSON: a bounded envelope/resource walk distinguishes object envelopes, manifests, collections, discovery, status and error resources and emits counts. STIX payloads, URLs, authorization metadata and endpoints remain inert; no HTTP/TAXII client is invoked.
- OpenFOAM fields: a standalone bounded lexer validates the ASCII `FoamFile` header, reads scalar/vector uniform or nonuniform internal fields into streaming min/max statistics, and counts boundary patch dictionaries. It does not traverse cases, evaluate dimensions or boundary conditions, or launch a solver.
- JSON Patch: a bounded JSON walk validates RFC 6902 operation requirements, emits ordered op/path/from/value-type rows, and aggregates operation kinds. Values, target documents and pointer evaluation stay inert; no patch mutation occurs.
- JSON Merge Patch: an explicitly routed bounded JSON walk traverses object members into escaped paths and emits set/delete/merge actions with value types and depth. Values and RFC 7396 application semantics remain inert; arrays and scalar roots are summarized as replacement values.
- CloudEvents JSON: a bounded JSON walk accepts one event object or a batch array, validates the four required 1.0 string attributes, and emits type/id/source-host/time/subject/data-size rows. Source query values are masked; payloads, dataschema and extension values are omitted, with no URI resolution, broker handling or network access.
- FHIR JSON: a bounded JSON walk validates a `resourceType` root, treats `Bundle.entry[].resource` objects as a bounded resource inventory, and emits resource type/id/status/profile and structural counts. Narrative, clinical values, identifiers, coded displays, extension values and references are omitted, with no terminology, URL or clinical-operation resolution.
- Avro JSON: a bounded schema walk accepts record/protocol/union roots, emits field paths and type shapes for named, collection and primitive schemas, and summarizes protocol messages. Defaults, documentation, aliases, logical values, imports, field data and RPC/code-generation behavior remain inert.
- OTLP JSON: a bounded signal walk accepts protobuf-JSON `resourceSpans`, `resourceMetrics`, `resourceLogs` and `resourceProfiles` arrays, then emits resource/scope counts and service names. Attribute values, bodies, IDs, exemplars, links and endpoints remain inert; telemetry is never exported or interpreted.
- OCEL 2.0 JSON: a bounded object-centric log walk validates the `eventTypes`, `events`, `objectTypes` and `objects` collections, counts typed attributes and event/object relationships, and emits inert event/object rows. Attribute values and qualifiers stay omitted; process discovery and external resolution never run.
- JSON:API: a bounded document walk validates primary `data` and optional `included` resource objects, counts attribute and relationship members, and rejects the mutually exclusive `data`/`errors` combination. Link URLs, attribute values, meta/error payloads and API operations remain inert.
- GraphML: a bounded quick-xml reader validates the GraphML root, node/edge IDs, `edgedefault`, labels, and basic yFiles shape hints before passing a `DiagramGraph` to the shared layered renderer. DTDs and external resources are rejected; compound nodes, hyperedges, custom keys, and source coordinates are not reconstructed.
- XGMML: a bounded quick-xml reader validates Cytoscape graph/node/edge IDs and labels, reads the graph `directed` flag, and records `att`/graphics presence as explicit warnings before using the shared layered renderer. Attribute columns, coordinates, nested subgraphs, and external resources remain inert.
- Graph Modeling Language: a bounded bracket tokenizer parses `graph [ ... ]` containers, comments, quoted labels, node/edge records, and the optional `directed` flag. The `.gml` route is selected only for the text grammar, so geographic GML XML remains on its map converter; unknown attributes and graphics coordinates are reported but not interpreted.
- ESRI ASCII Grid: the reader streams bounded row-major cell tokens twice (range scan, then PNG encode), caps input/line/dimension/cell/encoded-image sizes, and emits a continuous PNG-backed raster with transparent NoData cells and a legend. CRS reprojection and thematic color-table breaks are not applied.
- E57: the reader bounds physical input, E57 1.0 header/offsets, XML bytes/events/nodes/depth, cloud count, attributes, decoded points and rendered points. CRC-protected pages are checked as XML and point vectors are read. Each cloud's pose is applied and spherical points are converted to Cartesian coordinates; scan images, CRS reprojection and non-coordinate attributes are omitted.
- NetCDF CDF-1/CDF-2: the big-endian classic parser validates dimensions, attribute lists, variable types, offsets, vsize padding, fixed arrays, and interleaved unlimited-record slabs before emitting variable/value tables. CDF-5 and NetCDF-4/HDF5 are rejected rather than misread; fill values are labelled and no scientific transforms are applied.
- XYZ ASCII: a bounded header parser maps common X/Y/Z, intensity, RGB and normal aliases; otherwise homogeneous row field counts select the supported profile. Unknown header columns are skipped, malformed partial color/normal groups are rejected, and quoted CSV fields are unsupported. The reader caps total file bytes, line bytes, line/point count, numeric fields and generated PLY bytes; coordinates are used as stored because the format supplies no guaranteed unit, CRS or pose metadata.
- PPTXのchart/table/SmartArt/OLE補助parserは、namespace prefixに依存しないbyte-level local-element preflightを先に行い、該当要素がない大多数のslideではXML parserを起動しません。
- PDF: `lopdf`がPDFオブジェクト表を保持しますが、ページcontentの展開はページ単位かつ同じ上限で行います。
- 埋め込みfont program: `Arc<[u8]>`でpage/Form/soft-maskのdecoder scope間に共有し、font bytesを複製しません。
- Page IR: `jobs=1`ではrender直後にSVGへconsumeして破棄します。並列PDFも入力順を保つ`jobs`件単位batchだけを保持し、文書全ページのIRは保持しません。
- 出力: `BufWriter`へ直接書き、巨大なSVG文字列を二重保持しません。
- drawio: 図面本文の展開は`max_zip_entry_bytes`で制限します。shape libraryは1ファイル64 MiB、1 shape 200,000命令を上限とし、そのページが実際に使うshapeだけをXMLから抽出して保持します。必要なshapeが全て揃った時点で残りのlibrary fileは読みません（AWS図1枚＋42 MBの全library指定で常駐15 MiB以下）。座標と長さは原点から±1,000,000 pxに収め、ページもその範囲でcropします。
- SVG逆変換: SVG合計を`max_input_bytes`、ページ数を`max_pages`で制限し、OOXMLを一時ZIPへ書いてからrenameします。
- 並列化: `jobs=1`が省メモリ既定です。PDFだけ指定worker数でページを並列化し、同時常駐Page IR上限も`jobs`件です。速度と常駐IR数は明示的なトレードオフです。

`conversion.json`の`largest_page_ir_bytes`は、ページIRをJSON化したときのサイズを用いる比較可能な近似値です。OS RSSの最大値ではありません。

## 安全性

- drawioのstencilは呼び出し元が指定したファイル／ディレクトリだけを読み、図面やstencilの中身からパスを組み立てることはありません。埋め込みSVG画像はSVG逆変換と同じ検査（script、foreignObject、外部参照、ENTITY宣言の拒否）を通してからでなければ出力に入れません。URL参照の画像は取得しません。
- コアとPythonバインディングは`unsafe_code = "forbid"`、Node.jsバインディングはnapi-rsマクロが生成するFFI glueだけを許可する`unsafe_code = "deny"`です。手書きプロジェクトコードに`unsafe`はありません。
- ユーザーパスワードを要求するPDFは処理しません。ユーザーパスワードが空の暗号化PDFは読み込み時に復号され、通常どおり変換します。lopdfはオブジェクト単位の復号失敗を握りつぶすため、内容を消費しながら1 nodeも描かなかったページは警告として報告します。
- ZIP entry展開量、PDF content展開量、XMLイベント、ページ、描画セルに上限があります。
- PDF画像は宣言dimensionの`width × height × 4`を展開上限と照合してからbufferを確保し、圧縮されたdimension bombを拒否します。
- JPEG soft maskはJPEG metadataのdimension/pixel formatを先に検査し、既存entry上限内でgrayscale alphaへdecodeします。external CCITTはinline画像と共通のbounded hayro decoderを使用します。
- 外部relationshipの画像は取得しません。
- PPTXのvideo/audio本体とOLE payloadは実行・decodeせず、既存poster/previewだけを静的SVGへ保持します。preview欠落時は軽量placeholderと警告を出します。
- 一時出力からのatomic renameで、壊れた完成ページを残しません。
- SVG逆変換は既存OOXMLを上書きせず、active content・外部参照を拒否し、SVG本体とbounded PNG fallbackを同時に格納します。元文書の意味構造を復元したと誤認しないよう常にfidelity警告を返します。
- 未対応PDF演算子、ブレンド、soft mask、shadingなどは`warnings`へ残します。
- knockoutや複雑なpaint serverなど、SVGとPDFの合成モデルが異なる場合は同値性を証明できる条件だけ警告を外します。non-isolated groupはSVGの非isolated group、noncontained radialはbounded field tessellationで表現します。
- mesh shadingはshadingごと最大3,000 micro-triangleへ適応分割し、pattern、soft mask、Type3等のnested interpreterも共有するpage全体50,000 triangle budgetを適用します。`Decode`が座標4値＋1 component pairだけなら、そのrangeを不足color componentへ反復します。component pair自体が欠落・奇数の場合は従来どおり警告してskipします。
- inline imageのBI/IDはPDF literal string、hex string、commentを読み飛ばすsyntax-aware scanで検出し、画像data内のEIだけはdecode検証付きdelimiter scanを使います。
- XLSXの過大な使用範囲は、空セルを無制限に描かず明示エラーにします。
- XLSX formula fallbackは式長1MiB、依存深さ64、演算100,000、range 100,000セルを上限にします。条件集計rangeは数値・文字列・空白の位置対応を維持し、失敗時は警告してcached値契約へ戻します。
- cross-sheet／defined-name式は、未cached式に`!`または実際のdefinedNameが現れるsheetだけresolverを起動します。外部sheet全体を保持せず、必要座標をgroup化してZIP partから選択抽出します。conditional expressionはruleごとに一度評価し、全cell/tileで結果を共有します。
- XLSXのdirect string/inline text/formula captureは所有権をCellへmoveし、capture buffer・raw value・display valueの重複保持を避けます。raw valueは数値・booleanなど評価に必要な型だけ保持します。

### drawioモジュールの構成

`src/drawio/`は仕事ごとに分かれています。`mod.rs`が`mxfile`の展開・モデル解析・Scene（親子座標）・
ページ組み立てを持ち、`geometry.rs`が矩形とpath文字列と変換、`shapes.rs`がshape名の解決と幾何、
`edge.rs`が経路・矢尻・エッジラベル、`label.rs`がHTMLラベルと折り返し、`bpmn.rs`がBPMNの3層、
`stencil.rs`がshape libraryの読み込みと解釈を担当します。

### drawioのshape解決

1. `shape=stencil(...)`（図面が自前で持つmxStencil）
2. 呼び出し元が渡したstencil library（`stencil_paths`／`--stencils`）の完全一致
3. 本modのnative shape（基本図形、フローチャート、BPMN、UML、floorplan、AWS 3Dの箱など）
4. label付きplaceholder矩形＋shape名を含む警告

2が3より優先します。libraryはエディタが実際に描く定義そのものなので、同名のnative近似より忠実だからです。mxStencilは`path`（move/line/quad/curve/arc/close）、`rect`／`roundrect`／`ellipse`、`fill`／`stroke`／`fillstroke`、`save`／`restore`、色・線幅・破線・alpha・cap/join/miterを解釈し、`aspect="fixed"`は等倍センタリング、`strokewidth="inherit"`はcell側の線幅を継承します。ベンダーアイコンは同梱せず、利用者が用意したファイルだけを読み、ネットワークは使いません。

drawioがJavaScriptで実装するshapeは、幾何が確定しているものをnative実装として移植しています（BPMNのoutline/background/symbol、floorplanのwall系、UMLのcomponent/folder、AWS 3Dの箱と地上コネクタなど）。AWS 3Dのサービス図形は箱と陰影だけを描き、上に載るグリフが無いことを専用の警告で明示します。

## 精度方針

1. 画像化より、編集可能なSVG primitiveを優先します。
2. 意味を黙って失うより、警告または明示エラーを優先します。
3. PDF文字はToUnicodeとフォント幅を使い、文字列と配置の両方を維持します。
4. Office入力はテーマ、style、relationshipを解決し、名前空間prefixには依存しません。
5. SVGへ`data-source-id`、`data-content-kind`、`data-semantic-role`を付与します。

Visio VSDXはOPC packageとして読み、internal relationshipからdocument、ページ順、各page XMLを解決します。旧VDXは単一XML内のPageを順に抽出し、同じShapeSheet rendererへ渡します。ShapeSheetのlocal Cartesian座標はshapeのLocPinを引き、FlipX/FlipY、反時計回り回転、Pin位置の順に変換し、pageからSVGへ渡す境界でY軸を一度だけ反転します。1次元shapeの端点、直接のMoveTo/LineTo geometry、基本的な名前付きfallback形状、直接style、textに対応します。master継承、nested group、connector曲線routing、foreign dataは明示的に省略し、external relationshipを取得したりmacroを実行したりしません。

`.eml`はRFC 5322/MIME parserでboundedに読み、headerとdecoded text bodyを共有HTML typesetterへ渡します。HTML alternativeもテキストとして扱い、attachment/inline imageは省略します。HTML bodyをブラウザとして実行したり、外部画像・linkを取得したりしません。

`.mbox`はRFC 4155のfull `From_` separatorを検査してメッセージ範囲を分割し、各messageを同じMIME text rendererへstreamします。separator判定を限定して曖昧な本文行を境界と誤認しにくくし、メール件数・行数・入力byteで制限します。

`.mht`/`.mhtml`はRFC 2557 MIME containerからHTML bodyを取り出し、既存HTML parserへ渡します。Content-IDで参照されたPNG/JPEGだけをサイズ・pixel上限内で埋め込み、Content-Location/remote resourceは取得しません。safe HTML subsetにないscript/styleは省略します。

`.ics`はRFC 5545 content lineをbyte単位でunfoldしてからUTF-8として読むので、foldがmultibyte character内部に入った記録も復元できます。VEVENT/VTODOなどを独立ページへ変換し、recurrenceやVTIMEZONEは勝手に評価せず、元のルール・TZIDを表示して警告します。

`.vcs`はVCALENDAR VERSION 1.0のVEVENT/VTODOを同じbounded calendar parser/typesetterへ渡します。互換性を明示するため、ページは`vcalendar` source formatとし、未対応alarm/propertyをwarningで知らせます。

`.vcf`はRFC 6350 vCard 4.0とRFC 2426 vCard 3.0のUTF-8 content lineをunfoldし、escape済みtextと複数contactを共通typesetterへ渡します。写真や鍵などのmediaは展開せず、URLなどの外部resourceも取得しません。

`.msg`はCFBをbounded readerで開いてOutlookの代表的なMAPI root property streamだけを読み、attachment storageやbinary payloadは展開しません。Subject、送受信者、HTML/plain-text bodyを共通typesetterへ渡し、`PidTagInternetCodepage`/`PidTagMessageCodepage`に従ってANSI形式をdecodeします。HTMLはsafe subsetで処理し、script/styleは省略、画像・linkは取得しません。

`.ppt`は`CurrentUserAtom`から最新`UserEditAtom`を読み、過去のedit chainをたどってpersist directoryを古い順に合成します。最新の`DocumentContainer`とpresentation slide listの順序からlive `SlideContainer`だけを解決し、OfficeArt `ClientTextbox`の直接文字とoutline referenceをA4 text flowへ渡します。PPTXの図形レンダラーとは独立したtext-only fallbackで、画像、master/layout、geometry、style、animationや埋め込みpayloadは描画しません。暗号化tokenとencrypt-session referenceを拒否し、外部resourceやmacroは開きません。

## 拡張ポイント

- 新しい入力形式は`PageConsumer`へページを渡すconverterとして追加します。
- SVG以外の出力は共通IRから別writerを実装できます。
- PPTXとXLSXのchartは共通`ChartData` parser／rendererを利用し、Office cached seriesをbar・line・pie・doughnut・area・XY scatter previewへ変換します。cacheの`pt idx`を保持して欠損値の位置を保ち、scatterは数値x/yを軸上へ配置、負値はzero axisをまたいで表示します。areaはbaselineまたはstacked regionを塗り、doughnutのhole sizeと、存在するseries/category legendをbounded SVG nodesで描画します。bubble、他の未知chart type、複合chartはplaceholderを出し、Office chart style/axis/data labelの再現は対象外です。
- PPTX native tableはgraphicFrameのgridをpoint座標へscaleし、cell pathとclip付きtextへ正規化します。gridSpan/rowSpan continuationは重複描画せず、空白区切りwordを保つscript-aware wrapをshape textと共有します。
- Table row/column metricが0でもgraphicFrame extentが有効なら、全zeroは均等配分、部分zeroは正metric平均へ置換してからframe寸法へscaleします。frameとgrid双方が無効な場合だけreview警告を残します。
- DrawingML custom geometryはguide式からpathへ正規化します。未対応preset／guide演算とWord互換組版は既存IRを拡張して追加できます。
- DrawingML preset adjustmentは`prstGeom/avLst/gd`から取得します。wedgeRectCalloutはECMA presetのdx/dy、side-selection、16点path式を評価し、tipがshape bbox外でも座標を保持します。shape direct paintと`style/fillRef/lnRef`のcolor transformは混在させません。
- 高頻度presetはbounding boxへ落とさずnative pathを生成します。right/left braceはadj1のshoulderとadj2のcenter、arcは60000分の1度のstart/end angle、bentConnector2/3はbounded bend guide、canはadjによる楕円高を評価します。manual-input/extract/decision flowchartは回転・group affine適用前のlocal geometryとして正規化します。
- 第2 preset群はmoonのinner arc、cubeのdepth、snip/cornerのarm、waveのamplitudeをadjustmentから評価します。donutは向きが逆のinner subpathでnonzero holeを保持し、left/right/bracketPairはopen path、bent/uturn/curved/left-right arrow、delay/connector/collate/process flowchart、homePlate、action button、mathPlusもnative compound pathへ正規化します。
- 残るwedgeRoundRect/wedgeEllipse/border callout、curved up/down/circular/up-down arrow、irregular sealもnative path化します。round calloutはtip方向に応じた辺へwedgeを挿入し、adj3 radiusをbounded評価します。
- DrawingML direct `outerShdw`と`glow`はshape/picture/text/table textのSourceMetaへ保持し、SVG writerがSourceAlpha blur、offset、flood、composite、SourceGraphic mergeからなるfilterを生成します。両effectがあるnodeはshadow、glow、元graphicを1 filterで順序合成します。blur/glow radiusは512pt、shadow distanceは4,096ptで上限化し、filter regionもpage寸法から有限化します。互換保存用`a14:hiddenEffects`はfilter化しません。
- PPTX themeはpresentation既定だけに固定せず、各slideのlayout→master relationshipからtheme partを解決し、part名ごとに一度だけstream parseしてcacheします。`effectRef`はidx 0を無effect、idx 1以上を`effectStyleLst`の1-based参照として解決します。shape-local `effectLst`が存在する場合は、empty listも含めてtheme effectを上書きします。
- Placeholder geometryはidx完全一致を優先し、title/ctrTitle/subTitle/footer/date/slide-numberだけtype aliasへfallbackします。layout/masterにもgeometryがない可視footer/date/slide-numberはpage下端の有限既定boxへ配置します。zero-sizeかつ空paragraphのhiddenFill/hiddenLine互換artifactは可視nodeも警告も生成せず、可視textを持つ未知body placeholderだけreview警告を維持します。
- PPTX `srcRect`はsource画像をdecode/crop/re-encodeせず、残存source比率からimageのx/y/width/heightを逆算し、元frame geometryを回転・group affine付きclipとして適用します。負cropは画像外を透明marginとして保持します。crop値は±1,000%、可視幅・高さは0.01%以上、生成座標は絶対値1e9以下に制限し、不正時は警告付きfull-imageへ戻します。cropとshadow/glowが併存する場合はclip済みimageをgroup化してからeffectを適用します。
- DrawingML `pattFill`はbitmap化せず、前景/背景color transformとalphaを解決して`TilingPatternDefinition`へ正規化します。density、horizontal/vertical hatch、up/down diagonal、open diamondを1.5〜12ptの有限tileと2〜3個のeditable pathだけで表し、shape回転とgroup affineは参照path側で保持します。未知presetは警告して背景色へ戻します。
- PPTX image color effectは入力画像をdecode/re-encodeせず、XML順の`ImageColorEffect`列としてSourceMetaへ保持します。duotoneはluminance matrix＋channel linear map、grayscaleはsRGB luminance matrix、brightness/contrastはbounded component transfer、`clrChange`はdifference・1/255 exact-match mask・replacement mergeへ変換します。cropがある場合はclip済みsourceをgroup化して色効果を適用し、その結果alphaからshadow/glowを生成します。
- XLSXのprint area/manual break/print titleはbodyと反復titleを局所座標groupへ分け、不要な中間セルを複製せず1 sheetから複数Page IRを生成します。
- 明示print area/pageSetupがない巨大XLSXは16,384pt／2,000 grid-cellを上限にauto tileします。row/column prefix座標はsheetごとに一度だけ構築して全tileで共有し、各Page IRはconsume後に破棄します。
- XLSX Drawingはcell markerの位置・寸法とshape-local `a:xfrm`を別々に保持します。from/toが逆転または同一点ならlocal off/extへfallbackし、auto-fit text boxの0寸法はfont/run内容からbounded自然寸法を算出します。hidden互換shapeは描画前に除外します。
- DOCXのsection rangeは用紙設定とheader/footer relationship setを保持し、first/even/default storyをpage生成時に選びます。段落途中のpage breakはrun markerで分割し、break後のfieldを新page番号でmaterializeします。
- DOCX styleはdocDefaultsから最大32段の`basedOn` chainを基底順にmergeし、paragraph direct property、run direct propertyで上書きします。run fontはascii/hAnsiをLatin先頭family、eastAsiaをfallback familyとして保持します。
- DOCX/PPTXのword-aware wrapは空白区切りtokenを行末で分断せず、token自体がavailable widthを超える場合だけscript-aware文字幅で分割します。
- DOCX OMMLはdocument streamを追加DOM化せず、fraction numerator/denominatorへ`(num)/(den)` marker、radicalへ`√(expr)` marker、sub/supへbaseline-shift runを挿入します。matrix、n-ary、limit等の未知構造だけreview警告を残します。
- CAD (DXF): 外部GPL/Cライブラリを一切使わず、Pure Rustかつ安全なストリーミングASCII DXFパーサーを自作実装しています。Windows-1252/ANSIフォールバック付きデコードにより、実世界の角度記号（°）や欧州特殊文字を含む図面も安全にパースします。AutoCAD Color Index (ACI 1〜255) および TrueColor、レイヤー（非表示/フリーズ）、線種（LTYPE: 点線/破線）、ブロック定義と再帰参照（INSERT: 深度上限16）、軽量ポリライン（LWPOLYLINE: バルジ円弧計算）、円弧・楕円・スプライン・文字（TEXT/MTEXT: 制御文字整形）に対応し、図面のバウンディングボックスから用紙へのアスペクト比維持フィットスケーリングとY軸反転正規化を行ってPage IRへマッピングします。
- CAD (Gerber RS-274X): プリント基板（PCB）製造CAMデータに対応。アパーチャ定義（円形、矩形、長円/楕円、多角形）、フラッシュ（D03）、補間（D01/D02）、およびG36/G37ポリゴン輪郭塗りつぶしをパースし、基板外形と銅箔配線（Copper Layer）のPage IRへ変換します。
- CAD (HP-GL / HP-GL/2): プロッター言語に対応。ペン選択（SP）、絶対/相対座標プロット（PA, PR, PD, PU）、円弧（AA）、円（CI）、ペン幅設定（PW）を8色ペンパレットでベクター描画します。
- CAD逆変換（SVG -> DXF）: SVGのベクター図形（`<path>`, `<line>`, `<circle>`, `<rect>`）およびテキスト・レイヤーグループ構造から、AutoCAD Release 12（AC1009）標準のASCII DXFエンティティ（LINE, CIRCLE, ARC, TEXT）を再構築します。
- CADのメモリ・セキュリティ設計: DXFパースは最大行数（5,000,000行）、最大エンティティ数（500,000件）、ポリライン頂点数（100,000頂点）の安全上限を設け、ブロック展開の循環参照はスタック深度（最大16）で防御します。外部ライブラリ依存ゼロ（`unsafe_code = "forbid"`）を維持し、GPL/LGPLなどのコピーレフト感染を完全に排除しています。
