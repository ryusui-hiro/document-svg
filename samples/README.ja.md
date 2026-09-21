# サンプル

実際の入力ファイルと、そこから `docsvg` が作ったSVGのページを並べています。インストールする前に、仕上がりを自分の目で確かめられます。
ここにあるファイルはすべてこのリポジトリのスクリプトで作ったもので、自由に使えます。

[English](README.md) · [ギャラリーで見る](https://ryusui-hiro.github.io/document-svg/ja/samples.html)

インストール後に自分で試すには、次のように実行します（出力先は新しいフォルダか空のフォルダにします）。

```bash
docsvg samples/source/sample.pptx --output out/slides
```

出力フォルダには `page-0001.svg`、`page-0002.svg`… と、近似や省略をした箇所を書いた `conversion.json` ができます。

| | 入力 | 出力 |
|---|---|---|
| PDF | [`source/sample.pdf`](source/sample.pdf) | [`svg/pdf/`](svg/pdf/) — 2ページ |
| PPTX | [`source/sample.pptx`](source/sample.pptx) | [`svg/pptx/`](svg/pptx/) — 2ページ |
| XLSX | [`source/sample.xlsx`](source/sample.xlsx) | [`svg/xlsx/`](svg/xlsx/) — 4ページ |
| DOCX | [`source/sample.docx`](source/sample.docx) | [`svg/docx/`](svg/docx/) — 2ページ |
| Microsoft Project XML | [`source/sample.project.xml`](source/sample.project.xml) | [`svg/xml/`](svg/xml/) — 1ページ |
| drawio | [`source/sample.drawio`](source/sample.drawio) | [`svg/drawio/`](svg/drawio/) — 3ページ |
| DXF | [`source/sample.dxf`](source/sample.dxf) | [`svg/dxf/`](svg/dxf/) — 1ページ |
| Gerber | [`source/sample.gbr`](source/sample.gbr) | [`svg/gbr/`](svg/gbr/) — 1ページ |
| HP-GL | [`source/sample.plt`](source/sample.plt) | [`svg/plt/`](svg/plt/) — 1ページ |
| G-code | [`source/sample.nc`](source/sample.nc) | [`svg/nc/`](svg/nc/) — 1ページ |
| Excellon | [`source/sample.drl`](source/sample.drl) | [`svg/drl/`](svg/drl/) — 1ページ |
| STL | [`source/sample.stl`](source/sample.stl) | [`svg/stl/`](svg/stl/) — 1ページ |
| OBJ | [`source/sample.obj`](source/sample.obj) | [`svg/obj/`](svg/obj/) — 1ページ |
| VTK | [`source/sample.vtk`](source/sample.vtk) | [`svg/vtk/`](svg/vtk/) — 1ページ |
| Gmsh MSH | [`source/sample.msh`](source/sample.msh) | [`svg/msh/`](svg/msh/) — 1ページ |
| SU2 CFD mesh | [`source/sample.su2`](source/sample.su2) | [`svg/su2/`](svg/su2/) — 1ページ |
| STEP | [`source/sample.step`](source/sample.step) | [`svg/step/`](svg/step/) — 1ページ |
| IFC4 | [`source/sample.ifc`](source/sample.ifc) | [`svg/ifc/`](svg/ifc/) — 1ページ |
| IFCZIP | [`source/sample.ifczip`](source/sample.ifczip) | [`svg/ifczip/`](svg/ifczip/) — 1ページ |
| Graphviz DOT | [`source/sample.dot`](source/sample.dot) | [`svg/dot/`](svg/dot/) — 1ページ |
| Mermaid | [`source/sample.mmd`](source/sample.mmd) | [`svg/mmd/`](svg/mmd/) — 1ページ |
| LaTeX Math | [`source/sample.tex`](source/sample.tex) | [`svg/tex/`](svg/tex/) — 1ページ |
| Chart JSON | [`source/sample.chart`](source/sample.chart) | [`svg/chart/`](svg/chart/) — 1ページ |
| Markdown Table | [`source/sample.md`](source/sample.md) | [`svg/md/`](svg/md/) — 1ページ |
| QR Code | [`source/sample.qr`](source/sample.qr) | [`svg/qr/`](svg/qr/) — 1ページ |
| Raster Vectorize | [`source/sample.png`](source/sample.png) | [`svg/png/`](svg/png/) — 1ページ |
| ESRI ASCII Grid | [`source/sample-elevation.asc`](source/sample-elevation.asc) | [`svg/asc/`](svg/asc/) — 1ページ |
| dBASE III/III+ table | [`source/sample-parcels.dbf`](source/sample-parcels.dbf) | [`svg/dbf/`](svg/dbf/) — 1ページ |
| TOML configuration | [`source/sample.toml`](source/sample.toml) | [`svg/toml/`](svg/toml/) — 1ページ |
| YAML 1.2 configuration | [`source/sample.yaml`](source/sample.yaml) | [`svg/yaml/`](svg/yaml/) — 2ページ |
| Generic XML configuration | [`source/sample-config.xml`](source/sample-config.xml) | [`svg/generic-xml/`](svg/generic-xml/) — 1ページ |
| Java Properties configuration | [`source/sample.properties`](source/sample.properties) | [`svg/properties/`](svg/properties/) — 1ページ |
| BPMN 2.0 process | [`source/sample.bpmn`](source/sample.bpmn) | [`svg/bpmn/`](svg/bpmn/) — 1ページ |
| DMN 1.5 decision table | [`source/sample.dmn`](source/sample.dmn) | [`svg/dmn/`](svg/dmn/) — 1ページ |
| CMMN 1.1 case plan | [`source/sample.cmmn`](source/sample.cmmn) | [`svg/cmmn/`](svg/cmmn/) — 1ページ |
| ReqIF 1.0.1 requirements | [`source/sample.reqif`](source/sample.reqif) | [`svg/reqif/`](svg/reqif/) — 1ページ |
| XMI 2.1 UML model | [`source/sample.xmi`](source/sample.xmi) | [`svg/xmi/`](svg/xmi/) — 1ページ |
| GeoPackage | [`source/sample.gpkg`](source/sample.gpkg) | [`svg/gpkg/`](svg/gpkg/) — 2ページ |
| GeoJSON Text Sequence | [`source/sample.geojsons`](source/sample.geojsons) | [`svg/geojsons/`](svg/geojsons/) — 1ページ |
| TopoJSON | [`source/sample.topojson`](source/sample.topojson) | [`svg/topojson/`](svg/topojson/) — 1ページ |
| JSON Text Sequence | [`source/sample.jsons`](source/sample.jsons) | [`svg/jsons/`](svg/jsons/) — 1ページ |

`svg/*/page-NNNN.svg` は対応するブラウザで開けます。画像は埋め込み済みなので、
1枚だけ取り出せます。文字の見た目は表示環境のフォントによって変わる場合があります。
同じフォルダの `conversion.json` には、ページ数・所要時間・警告が記録されています。

## それぞれのサンプルが確かめていること

- **PDF** — テキスト、ベジェ曲線の塗り、線のストローク、埋め込みRGB画像、そして
  複数ページ。フォントは Helvetica を `/Widths` なしで指定しており、標準14フォントの
  字幅が正しく適用されるかを見ています。
- **PPTX** — スライドマスター、テーマ色、図形の塗り、箇条書き、16:9のスライドサイズ。
- **XLSX** — 180行のデータ、書式付きの数値、結合セル、`<pageSetup>` による用紙設定。
  1枚の紙に収まらないので、**印刷したときと同じように4ページへ分割**されます。
- **DOCX** — 見出しスタイル、罫線付きの表、明示的な改ページ、ヘッダーとフッター
  （フッターの `PAGE` フィールドはページ番号に解決されます）。
- **Microsoft Project XML** — summary/task bar、progress overlay、milestone、predecessor arrowを表示し、日程は再計算せずStart/FinishをGantt timelineに配置します。
- **drawio** — 3ページ構成。1〜2ページ目は図形と塗り、影、直交・曲線のコネクタと矢尻、エッジのラベル、
  スイムレーンと入れ子の子要素。入力は読みやすいXMLのまま置いていますが、エディタが既定で
  書き出す圧縮された `<diagram>` も同じように変換できます。3ページ目は図形ライブラリから描いた図形の例です（後述）。
- **DXF** — 建築フロアプラン図面。WALLS, DOORS, FURNITURE, TEXT などのレイヤー構造、
  LINE、ARC、CIRCLE、TEXT エンティティ、AutoCAD カラーインデックス (ACI) パレットの描画。
  `docsvg reverse <svg> --output <new.dxf>` による DXF R12 への逆変換往復もサポート。
- **Gerber (RS-274X)** — プリント基板 (PCB) パターンのベクター化。アパーチャ定義 (円形・矩形・楕円)、
  銅箔配線トレース、SMD パッド、スルーホールビア、G36/G37 ポリゴン銅箔ベタ塗り、基板基材プレビュー。
- **HP-GL / HP-GL/2** — プロッター制御言語による設計図面。ペン選択 (SP)、絶対座標プロット (PA, PD, PU)、
  円弧 (AA)、円 (CI)、ペン幅 (PW) と 8 色ペンパレットのベクター描画。
- **G-code** — CNC 加工パス。`docsvg reverse <svg> --output <restored.nc>` で G0/G1/G2 加工パスへ逆変換。
- **Excellon** — PCB ドリル穴データ。`docsvg reverse <svg> --output <restored.drl>` でドリル座標を抽出。
- **STL / OBJ** — ディフューズシェーディング付きの3Dメッシュ・アイソメトリック投影。
  `docsvg reverse <svg> --output <restored.stl>` で2.5D押し出し形状へ逆変換。
- **VTK** — VTK Legacy ASCII のスカラー場。`docsvg reverse <svg> --output <restored.vtk>` で3D PolyData メッシュへ逆変換。
- **Gmsh MSH** — 体積セルのワイヤーフレームと NodeData/ElementData フィールド。
- **SU2 CFD mesh** — zero-based connectivityと4つのnamed boundary markerを持つ2D三角meshです。境界edgeはpreviewでorangeに強調し、marker名は表示しません。
  `docsvg reverse <svg> --output <restored.msh>` で1D境界ワイヤーフレームメッシュへ逆変換。
- **STEP** — CAD Part 21 ASCII 幾何モデル。`docsvg reverse <svg> --output <restored.step>` で曲線集合・多様体サーフェスへ逆変換。
- **IFC4** — 1つのtessellated bodyを共有する2つのbuilding proxyを別々のlocal placementに配置したBIMモデルです。
- **IFCZIP** — 同じIFC4モデルをZIP archive内のroot直下に格納し、展開せずにbounded packageとして読み込みます。
- **Graphviz DOT** — マイクロサービス / クラウドインフラ構成図。DAGトポロジカルソート、階層レイアウト、角丸ノード、ベジェ曲線コネクタ、矢印。
  `docsvg reverse <svg> --output <restored.dot>` で埋め込みDOTの完全復元、未埋め込みSVGからは幾何推定復元。
- **Mermaid** — シーケンス図（OAuth2認証フロー）およびフローチャート。参加者ライフライン、同期・非同期メッセージ矢印、自動ステップ番号。
  `docsvg reverse <svg> --output <restored.mmd>` でMermaid DSLへ逆変換。
- **LaTeX Math** — 確率密度関数・数式表現。分数 (`\frac`)、平方根 (`\sqrt`)、上下添字、ギリシャ文字、総和記号 (`\sum`) の数式ボックスモデル配置。
  `docsvg reverse <svg> --output <restored.tex>` でLaTeX数式コードへ逆変換。
- **Chart (JSON)** — 棒グラフ、折れ線グラフ、円グラフ。軸目盛り、グリッド線、データ系列色分け、凡例。
  `docsvg reverse <svg> --output <restored.csv>` で数値・ラベルをCSV表データへ逆抽出、`--output <chart.png>` でresvg直接ラスタライズ。
- **Markdown Table** — GFM 仕様の表組み（インスタンス仕様・価格表）。ヘッダーセル、アライメント、背景互い違い色、境界線。
  `docsvg reverse <svg> --output <restored.md>` でMarkdown表テキストへ完全復元。
- **QR Code** — ベクター QR マトリックス生成。クリーンな単一パス (`M ... h 12 v 12 ... Z`)。
  `docsvg reverse <svg> --output <QRCode.tsx>` (React TSX)、`--output <QRCode.vue>` (Vue 3 SFC)、`--output <qrcode.datauri>` (Base64 Data URI) へのコード生成。
- **Raster Vectorize** — 手書き風署名・ビットマップ画像 (PNG/JPEG) のエッジ・輝度二値化とランレングス (RLE) ベクターパス化。
- **ESRI ASCII Grid** — NoDataを透明にしたelevation rasterを収録。continuous color ramp、min/max legend、origin/cell-size metadataを確認でき、sample生成scriptから再現できます。
- **dBASE III/III+** — numeric/text/logical/date fieldを含むparcel属性table。Windows-1252 decodeとdeleted rowのskipを確認でき、inputとSVGはsample生成scriptから再現できます。
- **TOML** — nested table、array、array-of-tables、boolean、number、timestampを含むdeployment config。設定値は不活性なdataとして表示し、実行しません。
- **YAML 1.2** — nested mapping/sequence、複数document、alias、custom tagを含むdeployment configuration。aliasはplaceholderのまま、tagは不活性に扱い、includeや設定は実行しません。
- **Generic XML** — namespace、attribute、escaped text、empty elementを含むapplication configuration。DTD/外部resourceを読み込まず、不活性な行として表示します。
- **Java Properties** — ISO-8859-1、Unicode escape、continuation、評価しない`${HOME}` placeholder、duplicate keyのlast valueを確認できます。
- **BPMN 2.0** — start/end event、user/service task、exclusive gateway、分岐ラベル、sequence flowをBPMN DIの座標から描画するorder fulfillment workflow。
- **DMN 1.5** — 2 input、3 rule、hit policy、local requirementを持つloan approval decision table。FEEL式は評価せずtext表示します。
- **CMMN 1.1** — case boundary、plan-item task、CMMNDI waypoint connector、評価しないsentry/lifecycle semanticsを含むclaims case plan。
- **ReqIF 1.0.1** — typed value、階層、XHTMLのtext平坦化、local derive relationを含むvehicle braking requirements exchange。
- **XMI 2.1** — nested package/class、attribute、operation、associationを含むUML model。model ID/typeとreferenceは不活性なtextとして表示します。
- **GeoPackage** — 内側ringを持つ2つのpolygon featureとraster tile layerを含むread-only SQLite database。GeoPackageBinary/WKB、tile-matrix extent、geometry列のみのquery、bounded PNG再エンコード、even-oddによる穴の描画を確認できます。
- **GeoJSON Text Sequence** — RFC 8142のRS delimiterで分割したFeature、LineString、FeatureCollectionを含みます。混在recordと属性省略を処理し、上限付きのmap pageを1枚生成します。
- **TopoJSON** — quantized coordinate、district間で共有して逆向きに参照する境界arc、transform付きPoint、feature property省略を確認できます。
- **JSON Text Sequence** — RFC 7464 Record Separatorで区切ったobject・array・top-level scalarを順序付きの不活性な内容として組版します。
- **SVG Transform** — `docsvg transform <svg> --output <out.svg> --minify --monochrome "#1e293b" --responsive --precision 1` による用途別再仕立て。

往復も試せます。`--embed-drawio-source` を付けて変換したSVGは、
`docsvg reverse <出力ディレクトリ> --output <新しい.drawio>` で元の図面に戻ります
（画像ではなく、編集できる図形として戻ります）。

drawioサンプルの3ページ目はシェイプライブラリの実例です。左の2つは
[`source/sample-stencils.xml`](source/sample-stencils.xml)（このリポジトリで書いた
mxStencil形式のライブラリ）から描いています。3つ目は図面自身が `shape=stencil(...)` で
持っているので、ライブラリの指定は要りません。

```bash
docsvg samples/source/sample.drawio --output out/ --stencils samples/source/sample-stencils.xml
```

drawio公式のライブラリ（AWS・Azure・GCPなど）は同梱しませんが、同じ形式なので
`--stencils` にdrawioの `stencils` ディレクトリを渡せばそのまま描けます。

## 作り直す

サンプルの入力ファイルは、外部から持ち込まずにこのリポジトリのスクリプトで生成しています。
そのためライセンスはリポジトリ本体と同じで、誰でも自由に使えます。

```bash
cargo build --release
python3 scripts/make_samples.py
```

`scripts/make_samples.py` が `source/` の各ファイルを書き直し、`docsvg` に通して
`svg/` を作り直します。レンダラーを変更したあとに実行すると、出力の変化がそのまま
差分として見えます。

> `conversion.json` の `elapsed_ms` は実行ごとに変わります。差分に出ても問題ありません。

`provenance.json`に各入力ファイルのSHA-256を記録しています。公開検査は一致するサンプルだけを許可し、テストでは生成スクリプトから再現できることも確認します。

## Webサイト向けのショーケースサンプル

[GitHub Pages サイト](../site/) では、`source/` 配下にもう一つ別のサンプル群を
手作業で用意して掲載しています。生成スクリプトが作る最小限の回帰テスト用サンプルとは違い、
より実務に近いテーマ性のあるサンプルです。上の生成スクリプトがまだ対応していない形式
（`.adoc`、`.csv`、`.d2`、`.html`、`.puml`、`.ply`）と、既存形式のよりリッチな例
（`sample-architecture.dot`、`sample-math.tex`、`sample-metrics.chart.json`、
`sample-qr.qr`、`sample-sequence.mmd`、`sample-signature.png`、`sample-table.md`）
を含みます。これらは `provenance.json` や `make_samples.py` の対象ではなく、
`docsvg` で直接変換し、その出力を `site/assets/samples/` へコピーしてギャラリーに
使っています。サンプルキーと元ファイルの対応表は
[`../site/assets/data/formats.json`](../site/assets/data/formats.json) を参照してください。
