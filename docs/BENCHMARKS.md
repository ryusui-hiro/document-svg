# ベンチマーク

2026-08-22、Apple M5（arm64）、macOS 26.5.1、Rust 1.93.0、`cargo build --release`で測定しました。時間は`conversion.json`の変換処理時間、RSSはmacOS `/usr/bin/time -l`のmaximum resident set sizeです。

## ローカル検証用 SVG/OpenXML interoperability regression

2026-08-26、ローカル検証用 backendのSVG fidelity fixtureと実PPTXを使い、librsvg参照画像と
LibreOffice 26.2.2.2経由のOpenXML描画、または元PPTXのLibreOffice描画と生成SVGを
同一寸法RGBで比較しました。`>16`はRGBいずれかのchannel差が16を超えた画素率です。

| 経路・fixture | 修正前 | 修正後 |
|---|---:|---:|
| SVG→PPTX `symbol/use`、`>16` | 24.831% | 1.699% |
| SVG→PPTX multi-layer gradient mask、`>16` | 39.401% | 0.717% |
| ローカル検証用 hybrid PPTX→SVG luminance mask、`>16` | 41.913% | 7.863% |
| ローカル検証用 EMF実slide PPTX→SVG、RGB MAE | 30.701 | 11.207 |
| 同EMF実slide、`>16` | 14.999% | 7.611% |

改善は、`mc:AlternateContent`のChoice/Fallback単一選択、SVG＋PNG compatibility fallback、
EMF/WMFのbounded SVG変換、互換保存用`hiddenFill`／`hiddenLine`の除外、table border色と
cell fill色の分離によるものです。逆変換PPTXはローカル検証用 `validate_pptx_package`にも合格しました。

## pdfsvgpptx PDF fidelity regression

2026-08-26、`pdfsvgpptx`のローカルQA fixtureを再配布せず参照し、Poppler 144dpiと
生成SVGのlibrsvg描画を同一RGB寸法で比較しました。

| fixture / 実page | 修正前MAE | 修正後MAE | `>16` 修正前→修正後 |
|---|---:|---:|---:|
| multi-subpath stroke (`ExtGState /LW=14`) | 7.818 | 0.476 | 5.428% → 1.208% |
| 000623 page 8相当（`q/Q`＋glyph position） | 23.077 | 13.679 | 15.675% → 12.135% |
| handeji page 22相当（text state＋symbol＋glyph position） | 24.452 | 20.038 | 18.340% → 16.430% |

`ExtGState`のline style全体、`q/Q`でのtext parameter保存、Page-local `UserUnit`、
日本語font fallback stack、既知Wingdings symbolのmonochrome Unicode正規化を追加しました。
さらに、代替fontのadvanceへ依存せず、PDFのglyphごとのx originをpositioned SVG tspanへ
保持することで、run境界の重なりと累積文字幅誤差を抑えました。

明示的な`--outline-embedded-pdf-text`では、同じpage 8相当がMAE 6.312・`>16`
8.876%、page 22相当がMAE 12.058・`>16` 13.132%まで改善しました。1-page SVGは
それぞれ959,458 bytes、1,049,959 bytesで、editable modeより大きくなります。
font outlineの利用・再配布条件は入力fontごとに利用者確認が必要なため、このmodeは既定offで
conversion warningを残します。

| 形式 | 入力 | 出力 | jobs | 変換時間 | 最大RSS | 最大Page IR近似 |
|---|---:|---:|---:|---:|---:|---:|
| PDF | 121,437 bytes | 4 pages | 2 | 36 ms | 24.7 MiB | 1,971,461 bytes |
| PPTX | 27,634 bytes | 4 slides | 1 | 4 ms | 3.4 MiB | 77,349 bytes |
| XLSX | 79,605 bytes | 3 sheets | 1 | 25 ms | 6.3 MiB | 855,092 bytes |
| DOCX | 37,566 bytes | 1 page | 1 | 1 ms | 3.4 MiB | 14,672 bytes |

入力はローカルQA用で配布物には含めません。PDFはIndexed／ICC画像、SMask、約2,000〜2,700 node/pageを含みます。4入力とも警告0でした。XLSXはローカル検証用参照側の実workbookで、cached valueがないformulaもbounded evaluatorで補完しています。DOCXは日本語FAQ文書です。

## PPTX corpus / native table

2026-08-23、ローカル検証用参照側の20 MiB以下のPPTX 228件をrelease版で監査しました。slideを持つ171件・1,012 slidesは全件変換成功・警告0でした。57件はslideを1枚も持たない`template-shell.pptx`で、入力契約どおり`input contains no renderable pages`として拒否しました。

| corpus | renderable files | slides | warning 0 | warningあり | 内部変換時間合計 |
|---|---:|---:|---:|---:|---:|
| ローカル検証用 PPTX | 171 | 1,012 | 171 | 0 | 801 ms |

同じcorpusで補助parserを無条件起動した時点の992 msに対し、local-element preflight後は801 msで、約19.3%短縮しました。

調整値付き`wedgeRectCallout`を含む4:3実slideでは、bounding box近似からECMA guide/path式へ変更し、1200×900描画のcallout領域MAEを0.12587445から0.09866572、全画面MAEを0.27048417から0.26490474へ改善しました。対象3実PPTXの警告はすべて0です。

42 cellのnative tableを含む29,310-byte実PPTXは、1 slideを3 ms、最大RSS 3.3 MiB、最大Page IR近似74,573 bytes、警告0で変換しました。原本PPTXとSVGを1600×900へ描画したnormalized RGB MAEは次のとおりです。

| 状態 | 全画面MAE | table領域MAE |
|---|---:|---:|
| table未実装（titleのみ） | 0.11845390 | 0.17973711 |
| native table＋word-aware wrap | 0.02944266 | 0.03842762 |

実slide/layout/masterの`outerShdw`を`a14:hiddenEffects`と分離して監査すると、可視effectは7 PPTX・34要素でした。direct shape/picture、shape text、table textから27個のbounded SVG filterを10ページへ生成し、7資料すべて変換成功、対象10ページはlibrsvg `--unlimited`で描画成功しました。内部変換時間合計は591 ms、最大Page IRは24,019,956 bytesです。

2,338,275-byte・8ページのtable text shadow資料は20 ms、最大RSS 8,339,456 bytes、最大Page IR 594,079 bytes、警告0で変換しました。release版2回の全8 SVGはバイト一致し、7個のshadow filterを再現しました。互換保存用hidden shadowだけを14個持つ別の10ページ資料はfilter 0で、変更前SVGとバイト一致しています。

同corpusの可視`glow`は7 PPTX・19要素で、すべてconnector shapeの5/8pt radius、scheme color、alpha指定、うち13要素は`satMod`付きでした。19要素を19個のbounded SVG filterとして7ページへ生成し、全7資料変換成功、対象全ページがlibrsvg `--unlimited`で描画成功しました。内部変換時間合計は235 ms、最大Page IRは6,928,586 bytesです。

1,984,132-byte・2ページ・glow 13個の代表資料は57 ms、最大RSS 39,059,456 bytes、最大Page IR 6,928,586 bytes、警告0でした。release版2回の全SVGはバイト一致し、glow追加後もshadow-onlyの8ページ資料は変更前SVGとバイト一致しました。LibreOfficeは元PPTXのglowを安定して再現しないため、合格判定にはOOXML effect件数、filter構造、SVG描画、決定性を使用しています。

非zero theme `effectRef`は16 PPTX・234参照でした。slide→layout→master→themeを解決すると、148参照が実際にshadow/glowを持つtheme styleへ到達し、148個のtheme由来filterを生成しました。残る参照は参照先effect styleがempty、またはshape-local `effectLst`で明示上書きされています。16資料はすべて変換成功し、effectを含む19ページはすべてlibrsvg `--unlimited`で描画成功しました。内部変換時間合計は1,156 ms、最大Page IRは24,019,956 bytesです。

4,348,562-byte・10ページ・theme shadow 6個の代表資料は43 ms、最大RSS 15,269,888 bytes、最大Page IR 3,131,476 bytes、警告0でした。release版2回の全10 SVGはバイト一致しました。2 slideが別master/themeを使い、idx 0、idx 1/2、direct empty overrideを含む専用fixtureでもtheme別shadow/glow解決を検証しています。

PPTX `srcRect`は175資料・844要素で使われ、うち146資料・608要素がnonzero cropでした。全要素中215個はgroup内、142個は回転picture、42属性値は負cropです。nonzero crop 146資料・1,349ページは全件変換成功し、593個のcrop clipを出力しました。内部変換時間合計は4,503 ms、最大Page IRは28,323,725 bytesです。

1,327,543-byte・6ページの実資料page 2を1560×1080で比較すると、全画面normalized RGB MAEは0.21788353から0.19344309へ改善しました。2つの主要crop領域は0.45911464→0.36319640、0.24888556→0.01676657です。crop対応後は24〜27 ms、3 crop clipで全6 SVGが2回のrelease変換でバイト一致し、対象page 2/6をlibrsvgで描画できました。

4,348,562-byte・10ページ・crop 11個の警告0資料は55 ms、最大RSS 14,958,592 bytes、最大Page IR 3,133,030 bytesでした。この資料もrelease版2回の全SVGがバイト一致しています。

PPTX `pattFill`は16資料・55要素で、`narHorz/narVert`各11、`pct5` 8、`wdUpDiag` 8、`openDmnd` 7、`pct90` 6、`wdDnDiag` 3、`dkUpDiag` 1でした。16要素はgroup内、4要素は回転shapeです。全16資料・149ページが変換成功し、55個すべてをcompact vector SVG patternとして18ページへ出力しました。対象18ページはすべてlibrsvg `--unlimited`で描画成功し、内部変換時間合計は488 ms、最大Page IRは5,034,052 bytesです。

2,010,200-byte・11ページ・pattern 6個の警告0資料は33 ms、最大RSS 9,240,576 bytes、最大Page IR 1,063,028 bytesでした。release版2回の全11 SVGはバイト一致し、横・縦hatchの線密度はLibreOffice描画へ合わせて2.0/1.5pt周期、0.4/0.3pt線へ調整しています。

PPTX image color effectは15資料・45要素で、duotone 23、exact `clrChange` 19、grayscale 2、brightness 1でした。全15資料・141ページが変換成功し、45 source effectを45個のordered SVG filter stageへ変換しました。effectを含む19ページはすべてlibrsvg `--unlimited`で描画成功し、内部変換時間合計は362 ms、最大Page IRは4,657,610 bytesです。

4,511,916-byte・17ページ・duotone 8個の実資料では、1600×1200全画面MAEがpage 3で0.10908561→0.09340865、page 4で0.13061325→0.12133024へ改善しました。

495,895-byte・7ページ・exact color-change 2個の警告0資料は9 ms、最大RSS 6,651,904 bytes、最大Page IR 410,238 bytesでした。page 3 MAEは0.04728162→0.04684454へ改善し、release版2回の全7 SVGはバイト一致しました。

20 MiB以下の重複除去PPTX 215件で警告を分類すると、rightBrace 213、arc 78、bentConnector3 40、leftBrace 38、flowChartManualInput 37、can 32、flowChartExtract 28、flowChartDecision 18、bentConnector2 13が上位でした。adjustment対応native path追加後も215/215件が変換成功し、警告資料は143→84、distinct warningは822→325へ減少しました。合計497警告を除去し、内部変換時間合計は4,575 ms、最大Page IRは70,927,302 bytesです。

1,327,543-byte・6ページのarc/can実資料は警告14→3となり、残る3件は未対応donut/rightBracketとinvalid tableだけです。

2,254,542-byte・8ページ・rightBrace 4個の代表資料は29 ms、最大RSS 11,780,096 bytes、最大Page IR 1,769,476 bytes、警告0でした。release版2回の全8 SVGはバイト一致し、対象page 6をlibrsvgで描画できました。

第2 preset群としてmoon、bent/bent-up/uturn/curved-left/left-right arrow、delay/connector/collate/process flowchart、cube、left/right/bracketPair、donut、snip2SameRect、homePlate、corner、wave、actionButtonForwardNext、mathPlusを追加しました。215件は引き続き215/215変換成功し、警告資料は84→62、distinct warningは325→185へ減少しました。第1弾前からの累計は143→62、822→185で、637警告を除去しています。内部変換時間合計は4,616 ms、最大Page IRは70,927,302 bytesです。

996,669-byte・8ページのmoon/collate資料は警告16→8となり、preset geometry警告は0です。残る8件はtransformを持たないplaceholder shapeです。

1,178,906-byte・8ページ・第2 preset 20個の代表資料は40 ms、最大RSS 37,126,144 bytes、最大Page IR 4,465,225 bytes、警告0でした。release版2回の全8 SVGはバイト一致し、全8ページをlibrsvgで描画できました。

残るtransform警告をXML監査すると、zero-size Line/Rectangleの空paragraph＋hiddenFill/hiddenLine互換artifactと、実fieldを持つfooter/slide-number placeholderが混在していました。空artifactだけを無警告除外し、placeholderをidx→unique type fallback→bounded既定位置の順で回復した結果、215件・1,910ページは全件成功のまま、警告資料は62→21、distinct warningは185→36へ減少しました。preset改善前からの累計は143→21、822→36で786警告を除去しています。内部変換時間合計は4,698 ms、最大Page IRは70,927,302 bytesです。

996,669-byte・8ページのmoon/collate＋zero-size互換shape資料は警告16→0となりました。

footer/slide-numberを含む実資料は12ページ・23 ms・最大RSS 8,617,984 bytes・最大Page IR 703,764 bytes・警告0で、全12ページをlibrsvg描画できました。release版2回の全SVGはバイト一致しました。

残るtable警告13件はすべてframe幅・高さとcolumn幅が有効で、row `h=0`だけが欠落したOffice auto-row tableでした。zero metricをframe extentから復元し、wedgeRoundRect/wedgeEllipse/border callout、rare arrow/sealを追加した結果、215件・1,910ページは215/215成功のまま、警告資料は21→3、distinct warningは36→4となりました。残る4件はすべてOLEを静的previewとして保持したことを知らせる意図的通知です。preset改善前からの累計は143→3、822→4で818警告を除去しました。内部変換時間合計は4,663 ms、最大Page IRは70,927,302 bytesです。

1,327,543-byte・6ページのtable/callout資料は警告14→0となり、20 ms、最大RSS 16,416,768 bytes、最大Page IR 2,466,422 bytesでした。release版2回の全6 SVGはバイト一致し、全6ページをlibrsvg描画できました。

## XLSX corpus / bounded auto tile

2026-08-23、ローカル検証用参照側の20 MiB以下のXLSX 184件をSHA-256で重複除去し、139 unique workbookをrelease版で監査しました。変更前は138件成功・1件canvas上限失敗・警告10件・599 SVG pages・最大Page IR 65,102,928 bytesでした。auto tile、formula resolver、Drawing fallback後は139件すべて成功・警告0、860 SVG pages、最大Page IR 17,872,542 bytesです。内部変換時間合計は10.085秒でした。

| 状態 | 成功/unique | 警告workbook | SVG pages | 最大Page IR |
|---|---:|---:|---:|---:|
| 変更前 | 138/139 | 10 | 599 | 65,102,928 bytes |
| bounded auto tile／formula／Drawing fallback後 | 139/139 | 0 | 860 | 17,872,542 bytes |

159,835-byteの密集workbookは4 pages・最大Page IR 56,121,280 bytes・最大RSS 156.9 MiBから、14 pages・6,769,476 bytes・37.5 MiBへ改善しました。総SVG内容は保持したままページ単位streamingの常駐量を抑えています。

従来canvas高220,503ptで失敗した1,904,989-byte／13-sheet workbookは、53 pages・1.399 s・最大Page IR 7,920,142 bytes・最大RSS 124.8 MiB・警告0で完走しました。

955,150-byteの実workbookでは、37個の未cached式、cross-sheet参照、単一cell definedName、exact VLOOKUP、文字列IF、conditional expression、hidden互換drawingを順に解決し、警告8件から0件へ改善しました。最終releaseは6 pages・47 ms・最大Page IR 611,872 bytes・最大RSS 33.3 MiBです。

## DOCX corpus / LibreOffice comparison

2026-08-23、ローカル実DOCX 307件をSHA-256で重複除去し、54 unique documentをrelease版で監査しました。全54件・93 SVG pagesが変換成功・警告0で、最大Page IRは1,711,222 bytesです。

5,953,676-byteの科学文書は、word-aware wrap、script font fallback、style chain、super/subscript対応前の24 SVG pagesから25 pagesへ変化し、LibreOfficeの28 pagesへ近づきました。先頭ページを1414×2000へ描画したnormalized RGB MAEは0.09218703から0.07480915へ改善しました。最終releaseでは25 pages・49 ms・最大Page IR 1,711,222 bytes・最大RSS 31.8 MiB・警告0です。

警告対象だった2件の数式文書では、OMML fraction 43、super/subscript run 284、radical 6をsemantic linear SVGへ変換しました。このうち同科学文書の数式ページでは、1414×2000描画MAEを0.08664780から0.07063865へ改善しました。

| 実文書 | 変更前Rust | 変更後Rust | LibreOffice |
|---|---:|---:|---:|
| low-energy diamond MEMS | 24 | 25 | 28 |
| diamond MOSFET | 5 | 5 | 4 |
| Young's modulus | 3 | 3 | 2 |
| silicon nanostructure | 1 | 1 | 1 |
| silicon metasurface | 2 | 2 | 2 |

測定コマンド例:

```bash
cargo build --release
/usr/bin/time -l target/release/docsvg input.pdf --output .tmp/bench/pdf --jobs 2
```

`jobs`を増やすとPDFページのスループットは上がりますが、同時に保持するPage IRと画像bufferも増えます。常駐メモリ優先なら既定の`jobs=1`を使用してください。

111ページ・embedded Type1 outlineを含むPDFで、全ページIRを保持しないbounded streamingを確認しました。

| jobs | 変換時間 | 最大RSS | 同時Page IR上限 | warning |
|---:|---:|---:|---:|---:|
| 1 | 3.972 s | 21.2 MiB | 1 | 0 |
| 4 | 1.737 s | 30.8 MiB | 4 | 0 |

並列時も`jobs`件ずつ入力順にconsumeするため、111ページ分のPage IRを一括保持しません。

## PDF feature fidelity

`pdfsvgpptx`のローカルQA fixtureをPDF原画とSVGへそれぞれ144dpiで描画し、同一RGB寸法の平均絶対誤差を測定しました。

| feature | normalized RGB MAE | warning |
|---|---:|---|
| direct Multiply blend | 0.00000000 | 0 |
| nested clip intersection | 0.00034807 | 0 |
| isolated group＋axial gradient | 0.00035639 | 0 |
| axial gradient | 0.00051176 | 0 |
| image＋vector luminosity mask、Interpolate=false | 0.00102436 | 0 |
| radial gradient（contained） | 0.00133301 | 0 |
| path＋vector luminosity mask | 0.00142816 | 0 |
| embedded Type1 fill＋stroke text | 0.00019430 | 0 |
| Type3 CharProc glyph text | 0.00041496 | 0 |
| Type1 rendering mode 7 glyph-union clip | 0.00016022 | 0 |
| CCITT Group4 inline image | 0.00115741 | 0 |
| CCITT Group3 1D inline image | 0.00115741 | 0 |
| CCITT Group3 2D inline image | 0.00348428 | 0 |
| LZW inline image | 0.00454265 | 0 |
| damaged-row CCITT Group3 2D（EOL再同期） | 0.00019290 | 0 |
| Function shading Type 1 / Function Type 4 | 0.00486632（塗り内部0.00148511） | 0 |
| noncontained radial（extended） | 0.00273752 | 0 |
| noncontained radial（unextended） | 0.00447967 | 0 |
| noncontained radial in soft mask | 0.00060045 | 0 |
| Type 4 Gouraud mesh（adaptive max 64 / 3,000 triangle budget） | 0.00663824 | 0 |
| Type 5 lattice mesh（adaptive max 64 / 3,000 triangle budget） | 0.00742310 | 0 |
| Type 6 Coons patch mesh（3,000 triangle budget） | 0.00732474 | 0 |
| Type 7 tensor patch mesh（3,000 triangle budget） | 0.00729982 | 0 |
| Separation/DeviceN/Lab/ICC path＋image | 0.00257190 | 0 |
| Shading Pattern fill/stroke/text/BBox | 0.00049804〜0.00391155 | 0 |
| Tiling Pattern colored/uncolored＋affine CTM | 0.01537951〜0.02345359 | 0 |
| soft-mask Function `/TR`（x²） | 0.00000000 | 0 |
| PPTX custom geometry＋solid alpha | 0.00025219 | 0 |
| PPTX SmartArt cached drawing（theme transform＋shape image） | 0.03515396 | 0 |

MAEはページ境界やrasterizerのcoverage差も含む全画面値です。Function shadingは境界を除いた塗り内部値も併記しました。

重複を除いた小型PDF 186件・394ページのrelease回帰では184件を8.135秒で変換しました。183件は警告0、1件は`/Shading`がMask辞書を指す意図的な不正resourceを警告しました。残る2件の失敗は参照側に意図的に残されたinvalid trailer PDFです。

## Large real-PDF corpus / mesh memory

2026-08-23、2–20 MiBの実PDFからページ数を層化して30件・1,660 pagesを抽出しました。初回は24件成功、JPEG SMask・external CCITT・inline-image token誤検出で6件失敗しました。JPEG SMask decoder、external CCITT DecodeParms、syntax-aware BI/ID scan追加後は30件すべて変換成功しました。

19,362,031-byte／33-page実PDFでは、同一pageの複数mesh shadingが1,261,568 triangleを生成していました。per-shading 3,000 triangle budget後は最大Page IR 836,631,120→53,659,918 bytes、変換時間5.696→1.367秒、最大RSS 258.9 MiBへ改善しましたが、DeviceRGB meshが座標range＋共有`[0,1]`だけの6-value `/Decode`を使うため警告1件でskipされていました。

共有component rangeをRGBへ展開するだけでは最大Page IR 164,325,554 bytes・最大RSS 526,712,832 bytesへ増えたため採用せず、nested pattern/maskを含むpage全体50,000 triangle budgetを追加しました。最終releaseは1.519秒、最大Page IR 51,766,293 bytes、page 26 SVG 35,551,145 bytes、最大RSS 261,275,648 bytes、警告0です。960×540 page 26 MAEはbudget前後とも0.23553602で、Type4–7単独fixtureの144dpi MAE 0.00664–0.00742も維持します。release版2回の全33 SVGはバイト一致しました。

5,692,146-byte／276-page TI datasheetはexternal CCITT stencilを含みます。最終releaseでは1.089秒、最大Page IR 3,153,912 bytes、最大RSS 119.4 MiB、警告0で完走し、該当249ページもlibrsvgで描画確認しました。
