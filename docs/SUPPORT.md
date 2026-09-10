# 対応範囲

## PDF

| 項目 | 状態 |
|---|---|
| classic xref / xref stream / object stream | `lopdf`経由で対応 |
| page MediaBox / CropBox / Rotate / UserUnit | 対応。`UserUnit`はpage辞書直下だけを読み、物理寸法と全page geometryへ同倍率を適用 |
| path、fill、stroke、dash、line cap/join | 対応 |
| CTM、q/Q、Form XObject、Form BBox | 対応 |
| nested clip intersection | 親clipから順にSVG groupへ適用 |
| DeviceGray/RGB/CMYK、CalGray/CalRGB/Lab | 対応 |
| Indexed、ICCBased、Separation、DeviceNとtint transform | path、shading、画像で対応 |
| 1/2/4/8/16-bit packed image、Decode、ImageMask stencil | row paddingを含め8-bit PNGへ正規化。stencilは現在fill色のRGBAとして対応 |
| JPEG、JPX、Flate画像 | 対応。JPEG圧縮SMaskはbounded grayscale alphaへdecode。JPXのブラウザ表示可否はconsumer依存 |
| `/Interpolate false` | `image-rendering="pixelated"`として対応 |
| unfiltered inline image | 対応 |
| filtered inline/external image（LZW、Flate、CCITT Group3/4等） | bounded native recoveryに対応。external CCITT DecodeParmsと許可されたdamaged Group3 rowも処理 |
| simple font / Type0 / ToUnicode | 対応 |
| `/Widths`、CID `/W`、`/DW`文字送り | 対応。1 code→1 Unicode scalarを証明できるrunはglyphごとのx originをpositioned `tspan`へ保持し、代替fontの累積advance誤差を回避 |
| text fill / stroke / fill+stroke | 対応 |
| text clip（rendering mode 4–7） | outline取得可能fontではglyph union clipとして対応 |
| embedded TrueType/OpenType/raw CFF Type2/Type1 outline | semantic SVG pathとして対応。PDF Encoding、font builtin Encoding、AGL、Form-local font scopeを解決。Arial等はeditable textを維持 |
| explicit embedded-font fidelity mode | `outline_embedded_pdf_text=true`で埋め込みfontをglyph path化。代替font差を回避する代わりに文字編集性を失うため既定off。fontのoutline・埋め込み権利確認warningを必ず出力 |
| Type3 font | CharProcs、FontMatrix、glyph-local resourcesをSVG groupへ展開。rendering mode 0に対応、その他modeはCharProc paint近似＋警告 |
| separable/non-separable blend mode | SVG `mix-blend-mode`として対応 |
| isolated / non-isolated transparency group | SVG isolation有無を保って対応 |
| opaque Normal knockout group | source-overとの同値性を証明して対応 |
| partial-alpha path knockout | 後続shape unionをluminance mask化。非path effectは検出・警告 |
| Alpha/Luminosity soft mask Form、BBox、BC | SVG mask sceneとして対応 |
| soft-mask transfer function `/TR` | Functionを33点sampleし、luminance/alpha別SVG component-transfer filterとして対応 |
| Function Type 0/2/3/4 | sampled multilinear、exponential、stitching、calculator stack machineに対応 |
| Function shading Type 1 | 2入力fieldを適応vector cellへ分割 |
| axial/radial shading | containedはSVG gradient、noncontainedは逆変換scalar fieldの適応vector cell |
| free-form/lattice Gouraud mesh Type 4/5 | shared component Decode rangeを復元し、shadingごと最大3,000・page全体50,000の適応micro-triangle tessellation |
| Coons/tensor patch mesh Type 6/7 | patch reuse、bicubic評価、shadingごと最大3,000・page全体50,000の適応micro-triangle tessellation |
| Shading PatternType 2 | Matrix、BBox、Background、Extend、page alphaを保つgradient paint。fill/stroke/textに対応 |
| Tiling PatternType 1 | colored/uncolored、BBox、XStep/YStep、Matrix、pattern-local resource、CTM variantをSVG patternとして対応 |
| compatibility / inline-image syntax | BX/EX内の未知operatorを仕様どおり無視。BI/IDはliteral/hex/commentを除外したsyntax scanで検出 |
| ExtGStateとgraphics-state stack | `ca`/`CA`/`AIS`/`BM`/`SMask`に加え、`LW`/`LC`/`LJ`/`ML`/`D`を累積更新。`q/Q`でfont、size、spacing、scale、leading、rise、text rendering modeも保存・復元 |
| symbol font fallback | Wingdings由来の標準Unicode丸・菱形・check・arrowをmonochrome sans glyphへ限定正規化し、元文字列はaria labelへ保持 |
| 埋め込み画像の参照 | SVG 2の`href`のみ。`xlink:href`は出力しないため、librsvg 2.46未満やSVG 1.1専用ビューアでは画像が表示されない |
| 暗号化PDF | ユーザーパスワードが必要なものは明示拒否。ユーザーパスワードが空の暗号化（権限フラグのみ）は他のビューア同様に復号して変換する。復号に失敗したページは「drew nothing」警告で報告 |

## PPTX

| 項目 | 状態 |
|---|---|
| slide order / size | 対応 |
| per-master theme color / major-minor font | slide→layout→master→theme relationshipを解決し、theme partをcacheして対応 |
| master → layout → slide装飾・背景layer | 対応 |
| placeholder geometry / level text style / vertical anchor | idxを優先し、unique title/footer/date/slide-number typeへfallback。geometry欠落footer/date/slide番号はbounded既定位置へ配置 |
| rect / roundRect / ellipse / polygon / arrows / chevron / star / seals / braces / brackets / arc / can / cube / moon / donut / wave / flowchart / bentConnector2/3 / straightConnector / wedge rect/round/ellipse callout / borderCallout等 | 対応。主要presetはadjustmentを解決 |
| solid / linear-radial gradient / line / dash / rotation | 対応 |
| nested group transform | off/ext/chOff/chExt、rotation、flipをaffine合成してshape/image/textへ適用 |
| paragraphs / runs / bold / italic / alignment / bullet / wrap | 空白区切りwordを保持し、cell/shape幅を超える長語は文字単位に分割 |
| embedded raster / SVG image / EMF・WMF / crop / color effect | `blip`とOffice SVG extensionの`svgBlip` relationshipを解決してdata URI化。EMF・WMFは12MiB上限内でSVGへ変換し、失敗時だけ警告付きplaceholderへfallback。`srcRect`の正・負cropを画像座標＋回転/group対応clipへ変換。duotone、grayscale、brightness/contrast、exact color-change transparencyをordered SVG filter化し、通常raster画像byteは再encodeしない |
| custom geometry | guide式、move/line/quad/cubic/arc/closeをeditable SVG pathへ変換。未解釈式だけbounding box＋警告 |
| solid/gradient alpha | fill、stroke、text、gradient stopで対応 |
| preset pattern fill | `pct5`、`pct90`、`narHorz`、`narVert`、`wdUpDiag`、`wdDnDiag`、`dkUpDiag`、`openDmnd`を前景/背景色・alpha付きcompact vector tileへ変換 |
| cached bar / line / pie chart | native SVGとして対応 |
| native table | column/row実寸、cell fill、run書式、alignment、clip、gridSpan/rowSpan、header/band fallbackをeditable SVGへ変換。row/column metricが0ならframe extentと有効metric平均から有限復元 |
| outer shadow / glow / effectRef | shape、picture、shape text、table textのdirect `outerShdw`/`glow`と、1-based theme `effectRef`を色・color transform・alpha・radius/distance/direction付きbounded SVG filterへ変換。idx 0とempty/direct clearを保持し、併用時は1 filterへ合成。`a14:hiddenEffects`は可視化しない |
| SmartArt cached diagramDrawing | frame/data/drawing relationship、shape/text/image、txXfrm rotation、theme color transformを解決して対応 |
| embedded video / audio | poster画像を静的SVG imageとして保持。poster欠落時はplaceholderを描画し、再生不可を警告 |
| OLE embedded object | 保存済みpreviewを保持。preview欠落時はframe placeholderを描画し、activation不可を警告 |
| SmartArt再layout | cached diagramDrawingがない場合は未対応 |
| inner shadow、reflection、soft edge、3D | 未対応 |

## XLSX

| 項目 | 状態 |
|---|---|
| workbook / sheet order | 対応 |
| shared string / inline string / cached formula value | 対応 |
| row height / column width / hidden rows-columns | 対応 |
| merged cells | 対応 |
| font / fill / borders / alignment | 対応 |
| general numeric alignment / number format | percent、桁区切り、通貨、科学表記、日付・時刻・経過時間、1900/1904 date systemに対応 |
| cell text clipping | 対応 |
| formula value | 保存済みcached valueを優先。欠落時はNumber/Text/Blank、文字列比較、current/cross-sheet参照、単一cell definedName、算術・範囲、SUM/AVERAGE/MIN/MAX/COUNT/COUNTA、COUNTIF(S)、SUMIF(S)、AVERAGEIF(S)、exact VLOOKUP、IF/ROUND/ABS等をbounded fallback評価 |
| conditional formatting | DXF font/fill、cellIs、expression（AND/OR/range比較/COUNTIF/CONCAT等）、containsText、contains/notContainsBlanks、2/3色scale、data barに対応。expression結果はsheet単位cache。standard fallbackがあるx14 extLstは重複解析しない |
| Drawing oneCell/twoCell/absolute image | hidden互換objectを除外し、anchor直下extだけを解決して対応 |
| Drawing basic shape / text box / horizontal-vertical line | markerが逆転/同一点ならshape-local xfrm off/extへfallback。0寸法auto-fit text boxはscript-aware自然寸法で復元 |
| cached bar / line / pie chart | native SVGとして対応 |
| print area / manual page breaks | `_xlnm.Print_Area`とrow/column breakをregion分割し、1 sheetから複数SVG pageを生成 |
| pageSetup / fit-to-page | Letter/Legal/A3/A4/A5等、portrait/landscape、margin、scale、fitToWidth/Height、中央配置に対応 |
| print titles | `_xlnm.Print_Titles`の反復行・反復列をmanual break後の各SVG pageへ再配置 |
| large sheet auto tile | print area/pageSetupがないsheetを16,384ptまたは2,000 grid-cell単位へ自動分割。prefix座標を共有しPage IRをbounded化 |

## DOCX

| 項目 | 状態 |
|---|---|
| multi-section page size / orientation / margins | sectPr block境界でpageを確定し、sectionごとのPageSetupへ切替 |
| paragraph / run / basic style | docDefaults→bounded basedOn chain→paragraph direct rPr→run direct rPrの順で継承 |
| mixed Japanese/Latin wrapping | 空白区切りwordを保持し、幅を超える長語だけ文字分割。ascii/hAnsiを先頭、eastAsiaをfallbackにしたfont stackを使用 |
| superscript / subscript | font縮小とbaseline shiftとして対応 |
| explicit and automatic page breaks | `pageBreakBefore`、段落途中の`w:br type="page"`、flow overflowに対応。break後のPAGE fieldも新page番号で評価 |
| table grid / cell fill / cell text | 対応 |
| inline image | 対応 |
| header / footer | section別default/first/even storyを選択・継承し、paragraph、image、tableを各pageへ繰返し対応 |
| PAGE field | 生成ページ番号をmaterialize |
| floating image（page/margin/paragraph offset） | 対応 |
| numbering.xml list | multi-level decimal、letter、Roman、bullet、lvlText、indentに対応 |
| DrawingML / VML text box | anchorはpage/margin/paragraph offset、inlineはflow heightと改ページへ反映。fill/stroke、clipped text、box内imageに対応 |
| arbitrary floating shape / wrap polygon | 未対応 |
| footnote / endnote | text referenceをsuperscript化し、本文領域を予約してseparator付きページ下部noteとして対応。note内Drawingは未対応 |
| comment | commentReferenceをsuperscript化し、comments.xml textをページnoteとして対応 |
| tracked change | final viewとして`ins`を採用し、`del/delText`を除外 |
| OMML math | fraction、sub/sup/subSup、radical、delimiterをsemantic linear runsへ変換。未知のmatrix/n-ary等だけ警告 |
| Word互換フォント組版と完全同一pagination | 未対応。決定的な近似layout |

## drawio

| 項目 | 状態 |
|---|---|
| `mxfile` / 複数`<diagram>` | `<diagram>`1件を1ページとして対応 |
| 圧縮されたdiagram本体 | `encodeURIComponent`＋raw deflate＋base64を展開。展開後サイズは`max_zip_entry_bytes`で制限 |
| 素の`mxGraphModel`文書 | 対応。`mxGraphModel`を含まないXMLは入力エラーとして拒否 |
| `object` / `UserObject`ラッパー | id、labelを内側の`mxCell`へ適用 |
| mxStyle文字列 | `;`区切りのkey=valueと先頭のshape名を解析。keyは大文字小文字を無視 |
| 基本図形・フローチャート図形 | rectangle（rounded含む）、ellipse、doubleEllipse、rhombus、triangle、hexagon、parallelogram、trapezoid、step、process、cylinder、cloud、document、multiDocument、note、card、internalStorage、cube、tape、actor、or、xor、dataStorage、delay、display、manualInput、offPageConnector、loopLimit、collate、extract、merge、cross、singleArrow、doubleArrow、swimlane、text、line、callout、message、umlLifeline、umlFrameに対応 |
| コンテナ系 | `group`と`waypoint`は子要素の位置だけを決め、自身は描画しません。`partialRectangle`は塗りと、`top`/`right`/`bottom`/`left`で有効な辺だけを描きます。`table`／`tableRow`は矩形として描きます |
| `=`を含まないスタイル語 | mxGraphと同じく、名前付きスタイルとして解決できるものだけをshapeとして扱い、解決できない語は無視します（`shape=`で明示された未対応shapeだけ警告します） |
| `shape=mxgraph.flowchart.*` / `mxgraph.basic.*` | 末尾名が上記に対応するものへマップ |
| shape library（`mxgraph.aws4.*`等） | `stencil_paths`（CLIは`--stencils`）にdrawioのstencil XMLファイルまたはそのディレクトリを渡すと、mxStencilを解釈して本物の図形を描画します。path/rect/roundrect/ellipse、fill/stroke/fillstroke、save/restore、色・線幅・破線・alpha、`aspect="fixed"`の等倍センタリングに対応 |
| インラインstencil（`shape=stencil(...)`） | 対応。図面が自前で持つ図形なので、外部ファイルは不要 |
| `resIcon`／`grIcon` | 対応。タイルの色はスタイルから、内側のアイコンは指定されたstencilから描きます |
| drawioがJavaScriptで実装するshape | 主要なものに対応します。BPMN（event／gateway2／shape／taskのoutline・background・symbol 17種）、floorplanのwall/wallCorner/wallU/window/door系/stairs、UMLのcomponent・folder（package）・startState・endState、table／tableRow、partialRectangle、waypoint、group、`mxgraph.gcp2.doubleRect`、AWSのresourceIcon/productIcon/group、AWS 3Dの箱と地上コネクタ（arrowNE/SE/SW/NW、arrowlessNE、flatDoubleEdge、dashedArrowlessEdge）、mockupのsearchBox/comboBox/iconGrid/simpleIcon、infographicのribbonSimple/cylinder/banner/bannerSingleFold/barCallout/shadedTriangle/shadedPyramid/pyramidStep、lean_mappingのoutside_sources/inventory_box/manufacturing_process/schedule/data_box/push_arrow/physical_pull、basicのpartConcEllipse/arc/pie/rectCallout/roundRectCallout、floorplanのroom/stairsRest/doorBypass、sysmlのactFinal/flowFinal/isControl/objFlowL/objFlowR/itemFlowLeft/itemFlowRight/paramDgm/port1、ios7uiのhorLines、rackGeneralのcontainer、bootstrapのrrect/horLines/checkbox/radioButton、各ライブラリ共通のtop/bottom/left/rightButton・rrect・marginRect・uRect・anchor（anchorは非描画）、ios7uiのphone/appBar/pageControl/downloadBar/slider/onOffButton/iconGrid、lean_mappingのtimeline2/fifo_lane/truck_shipment、mockupのmarkup.line/buttons.button/forms.checkbox、basicのdrop/obtuse_triangle/polygon（polyCoords・polyCurves・polylineに対応）、接頭辞なしのisoRectangle/isoCube2/curlyBracket。それ以外はlabel付きplaceholder矩形として描画し、shape名を警告に出力します |
| AWS 3Dのサービス図形 | 箱と陰影は忠実に描きますが、上に載る白いグリフはdrawio側のコードにあるため描けません。その旨を警告に明示します |
| 矢尻 | classic／block／open／oval／diamond（thin変種含む）、async／openAsync、box、dash、cross、circle、circlePlus、halfCircle、ER記法6種（ERone／ERmandOne／ERmany／ERoneToMany／ERzeroToOne／ERzeroToMany）に対応。ER記法の距離は`size + strokeWidth + 1`基準でmxMarkerと同じです |
| ベンダーアイコン本体 | 同梱しません。ライセンスと容量の都合で、利用者が用意したstencilファイルを読みます |
| `mxgraph.world.*`（国・地域の地図） | 非対応。drawioのオープンソース版にはこのstencilもsidebarも含まれず、オンライン版限定のため、利用者が`--stencils`で補うこともできません |
| `perimeter=` | `ellipse`／`rhombus`／`triangle`／`hexagon`／`step`／`parallelogram`／`trapezoid`／`center`／`lifeline`／`backbone`に対応。指定がない場合は描画中のshapeの輪郭に合わせます |
| `overflow=hidden` | ラベルを図形の枠でクリップします（ページ寸法も広げません）。`fill`／`width`は図形の幅で折り返します |
| `sketch=1`／`comic=1` | 未対応。手描き風の揺らぎは付かず、通常の直線・曲線で描きます |
| fill / stroke / dashed / dashPattern / opacity | 対応。`fillOpacity`、`strokeOpacity`、`strokeWidth`を含む |
| `gradientColor` / `gradientDirection` | 2 stopのlinear gradientとして対応 |
| `shadow` | mxGraphと同じ、(2, 3)ずらしの灰色コピーとして対応 |
| `direction` / `rotation` / `flipH` / `flipV` | 図形中心まわりの回転・反転として対応。`direction`のnorth/southはwidth/heightを入れ替え |
| グループ入れ子の座標 | 親vertexのoriginを最大64段まで累積。`visible="0"`は自身と子孫を非表示 |
| HTMLラベル | `<br>`、`<div>`/`<p>`、`<b>`/`<i>`、`<font>`のcolor/face/size、`style`のcolor/font-size/font-weight/font-style、文字実体参照に対応。それ以外のタグは除去してテキストを残す |
| `whiteSpace=wrap`と`align`/`verticalAlign`/`spacing*` | 文字体系を見た幅推定による単語単位の折り返しとして対応 |
| `labelPosition` / `verticalLabelPosition` | ラベル枠を図形1つ分ずらす（アイコン下のキャプション）形で対応 |
| `labelBackgroundColor` / `labelBorderColor` | 対応 |
| コネクタ（直線・`curved=1`・`orthogonalEdgeStyle`） | 対応。`elbowEdgeStyle`と`entityRelationEdgeStyle`は直交ルートとして扱います |
| 固定接続点（`exitX`/`exitY`/`exitDx`、`entryX`/`entryY`/`entryDx`） | 対応。辺上の点は進入方向を決める側面として解釈 |
| 経由点（`Array as="points"`）と`sourcePoint`/`targetPoint` | 対応 |
| 直交ルーターの一致 | 近似。`mxEdgeStyle.orthBuffer`の10 px分だけ図形から離れてから曲がり、経由点がない場合は辺の中央から出て図形間の中点で折れます。mxGraphのroute pattern表そのものではありません |
| エッジラベルの位置（`relative`な`x`とoffset） | 経路長に沿った位置として対応 |
| swimlane | title barとlane本体の分割線、`swimlaneFillColor`、縦向きtitleに対応。lane内の折り畳みは未対応 |
| 背景色（`mxGraphModel background`） | 対応 |
| ページ寸法 | `pageWidth`/`pageHeight`ではなく描画内容のbounding box＋10 pxの余白でcrop。モデル1 px = 0.75 pt |
| 座標・長さの上限 | モデルが宣言する座標と長さは原点から±1,000,000 pxに、ページも同じ範囲に収めます。超える場合はcropした旨を警告に出します |
| mxlibrary（シェイプライブラリ）／その他のXML | 変換対象外。何のファイルかを名指ししてエラーにします |
| 埋め込み画像（`image=data:`） | PNG/JPEG/GIF/WebPのdata URIに対応。`shape=image`はセル全体、それ以外は`imageWidth`/`imageHeight`/`imageAlign`/`imageVerticalAlign`に従うアイコンとして配置。`;base64`が省略されたdrawio形式も正規化 |
| URL参照の画像 | 未対応。ローカル変換で外部取得は行わず、警告を出して図形だけ描画 |
| SVGのdata URI画像 | 未対応。出力SVGへ検査していない別文書を埋め込まないため、警告を出して図形だけ描画 |
| `.drawio.png` / `.drawio.svg`の埋め込みメタデータ | 未対応 |

## SVGからOpen XMLへの逆変換

| 出力 | 対応 | 契約 |
|---|---|---|
| PPTX | 対応 | SVG 1件をスライド1枚のベクター画像として格納し、`mc:AlternateContent`へPNG fallbackを併設 |
| DOCX | 対応 | SVG 1件をページ1枚のベクター画像として格納し、`svgBlip`の基底PNG fallbackを併設 |
| XLSX | 対応 | SVG 1件をシート1枚のベクター画像として格納し、`svgBlip`の基底PNG fallbackを併設 |
| drawio | 対応 | 図面のsourceを持つSVGは`<diagram>`をそのまま復元し、編集可能な図形に戻す。持たないSVGはpage 1枚＝`shape=image`のSVG data URI 1件として格納 |
| 元Office意味構造の復元 | 非対応 | 段落、表、セル、数式、グラフ、master等は再構築しない |
| 元drawio図形の復元（sourceなし） | 非対応 | SVGを画像として持つだけで、shapeやedgeには戻らない |

入力は単一SVGまたはSVGファイルを含むディレクトリです。ディレクトリ内はファイル名順に
処理されます。SVGの`width`／`height`または`viewBox`からページ寸法を決定します。
`script`、`foreignObject`、animation、event属性、外部URL、外部CSS参照、ENTITY宣言は
格納前に拒否します。実際のdrawioのSVG exportが必ず持つ標準SVG 1.1 DOCTYPE（内部subsetと
ENTITY宣言のないもの）は受け付けます。埋め込みraster data URIと、安全検査済みのbase64 SVG
data URIは利用できます。

drawio出力だけは例外があります。drawioの「Include a copy of my diagram」で書き出されたSVG
（rootの`content`属性に`mxfile`を持つもの。2018年より前のリリースが書くURIエンコード形式も
含みます）と、`--embed-drawio-source`を付けて変換した本コンバータのSVGは、画像ではなく元の
`<diagram>`をそのまま復元します。`content`はroot要素のものだけを読みます。この場合そのページのSVG
本体は出力に入らないため、HTMLラベルの`foreignObject`を含む実exportもそのまま扱えます。
復元するsourceは、格納前に`mxfile`として構文解析し、DOCTYPE宣言があれば拒否します。

## 品質の読み方

`warning_count == 0`は、実装済み範囲で未対応要素を検出しなかったことを示します。Microsoft Office、Adobe Acrobat、LibreOfficeと画素単位で完全一致する保証ではありません。重要文書では元ファイルと全ページを同一DPIで描画比較してください。
