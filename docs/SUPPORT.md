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
| 暗号化PDF | 明示拒否 |

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

## SVGからOpen XMLへの逆変換

| 出力 | 対応 | 契約 |
|---|---|---|
| PPTX | 対応 | SVG 1件をスライド1枚のベクター画像として格納し、`mc:AlternateContent`へPNG fallbackを併設 |
| DOCX | 対応 | SVG 1件をページ1枚のベクター画像として格納し、`svgBlip`の基底PNG fallbackを併設 |
| XLSX | 対応 | SVG 1件をシート1枚のベクター画像として格納し、`svgBlip`の基底PNG fallbackを併設 |
| 元Office意味構造の復元 | 非対応 | 段落、表、セル、数式、グラフ、master等は再構築しない |

入力は単一SVGまたはSVGファイルを含むディレクトリです。ディレクトリ内はファイル名順に
処理されます。SVGの`width`／`height`または`viewBox`からページ寸法を決定します。
`script`、`foreignObject`、animation、event属性、外部URL、外部CSS参照、DOCTYPE／ENTITYは
格納前に拒否します。埋め込みraster data URIと、安全検査済みのbase64 SVG data URIは利用できます。

## 品質の読み方

`warning_count == 0`は、実装済み範囲で未対応要素を検出しなかったことを示します。Microsoft Office、Adobe Acrobat、LibreOfficeと画素単位で完全一致する保証ではありません。重要文書では元ファイルと全ページを同一DPIで描画比較してください。
