# アーキテクチャ

## 変換フロー

```text
PDF ───────────────┐
PPTX ZIP/XML ──────┤
XLSX ZIP/XML ──────┼─> Page IR ─> streaming SVG writer ─> page-NNNN.svg
DOCX ZIP/XML ──────┘                         └────────────> conversion.json

SVGファイル／ディレクトリ ─> bounded SVG reader ─> OOXML package writer
                                                   ├─> PPTX（1 SVG / slide）
                                                   ├─> DOCX（1 SVG / page）
                                                   └─> XLSX（1 SVG / sheet）
```

全入力形式は`Page`、`Node::{Path,Text,Image,Group}`、`Paint`、`Stroke`、`ClipPath`、`MaskDefinition`からなる共通IRへ正規化します。座標はポイント、変換はSVGと同じ6要素アフィン行列です。`TextRun`はrun全体の期待advanceに加え、PDF由来の証明可能なglyph x originも保持できます。SVGライターは入力順と固定精度で直列化し、属性順も決定的です。

## 言語バインディング

```text
Python ─> PyO3 / GIL detach ─┐
                              ├─> convert_path ─> SVGページ + report
Node.js ─> napi AsyncTask ───┘
                    └─ preview ─> 一時SVGをbounded read ─> SVG文字列 + report
Python / Node.js ─> svg_to_openxml ─> PPTX / DOCX / XLSX + reverse report
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
ネイティブホストに組み込まれるためrelease profileは`panic = "unwind"`とし、
バインディング境界がエラーへ変換できる設定にします。

PDF効果は、要素自身のtransformと混同しないようidentity座標の外側groupへ適用します。soft mask、blend、isolationは外側group、geometry transformは内側nodeです。複数clipは定義同士を参照せず、親clipから順に外側groupを重ねるため、librsvgやブラウザ間で安定して交差します。

## メモリ設計

- 入力全体: `max_input_bytes`で事前制限します。
- OOXML: ZIP中央ディレクトリだけを開き、必要なpartを1件ずつ読みます。展開後の各partは`max_zip_entry_bytes`で制限します。
- PPTXのchart/table/SmartArt/OLE補助parserは、namespace prefixに依存しないbyte-level local-element preflightを先に行い、該当要素がない大多数のslideではXML parserを起動しません。
- PDF: `lopdf`がPDFオブジェクト表を保持しますが、ページcontentの展開はページ単位かつ同じ上限で行います。
- 埋め込みfont program: `Arc<[u8]>`でpage/Form/soft-maskのdecoder scope間に共有し、font bytesを複製しません。
- Page IR: `jobs=1`ではrender直後にSVGへconsumeして破棄します。並列PDFも入力順を保つ`jobs`件単位batchだけを保持し、文書全ページのIRは保持しません。
- 出力: `BufWriter`へ直接書き、巨大なSVG文字列を二重保持しません。
- SVG逆変換: SVG合計を`max_input_bytes`、ページ数を`max_pages`で制限し、OOXMLを一時ZIPへ書いてからrenameします。
- 並列化: `jobs=1`が省メモリ既定です。PDFだけ指定worker数でページを並列化し、同時常駐Page IR上限も`jobs`件です。速度と常駐IR数は明示的なトレードオフです。

`conversion.json`の`largest_page_ir_bytes`は、ページIRをJSON化したときのサイズを用いる比較可能な近似値です。OS RSSの最大値ではありません。

## 安全性

- コアとPythonバインディングは`unsafe_code = "forbid"`、Node.jsバインディングはnapi-rsマクロが生成するFFI glueだけを許可する`unsafe_code = "deny"`です。手書きプロジェクトコードに`unsafe`はありません。
- 暗号化PDFは処理しません。
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

## 精度方針

1. 画像化より、編集可能なSVG primitiveを優先します。
2. 意味を黙って失うより、警告または明示エラーを優先します。
3. PDF文字はToUnicodeとフォント幅を使い、文字列と配置の両方を維持します。
4. Office入力はテーマ、style、relationshipを解決し、名前空間prefixには依存しません。
5. SVGへ`data-source-id`、`data-content-kind`、`data-semantic-role`を付与します。

## 拡張ポイント

- 新しい入力形式は`PageConsumer`へページを渡すconverterとして追加します。
- SVG以外の出力は共通IRから別writerを実装できます。
- PPTXとXLSXのchartは共通`ChartData` parser／rendererを利用し、Office cached seriesをbar・line・pie pathへ変換します。
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
- QAは元文書とSVGを同じDPIへ描画し、画素MAE、構造件数、文字欠落、警告件数を比較できます。
