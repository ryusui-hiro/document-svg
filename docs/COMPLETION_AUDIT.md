# Goal completion audit

監査日: 2026-08-26

## 要求と証拠

| 要求 | current stateの証拠 | 判定 |
|---|---|---|
| Rust製module | `document-svg` libraryと`docsvg` binary。edition 2024、unsafe禁止、MIT | 達成 |
| PDF→SVG | path/text/image/font outline、clip、transparency、pattern、Function、Type 1–7 shading、CCITT/JPEG、page streaming/parallelを実装 | 達成 |
| PPTX→SVG | master/layout/theme、shape/custom geometry、table、chart、SmartArt cache、image crop/effect、pattern、shadow/glow、OLE/media静的表現を実装 | 達成 |
| XLSX→SVG | cell/style/merge、formula fallback、conditional format、Drawing/chart、print area/break/title、bounded auto tileを実装 | 達成 |
| DOCX→SVG | section/page、style chain、word wrap、table/image/textbox、header/footer、notes/comments、OMML、fieldを実装 | 達成 |
| SVG→Open XML | 単一SVG／SVG directoryからPPTX 1 slide、DOCX 1 page、XLSX 1 sheet単位のvector image packageを生成。active contentを拒否し、PNG compatibility fallbackを併設。Rust/CLI/Python/Node.jsを公開 | 達成 |
| memory-oriented | page単位`PageConsumer`、ZIP part単位read、PDF jobs上限、input/entry/page/XML上限、mesh page budgetを実装 | 達成 |
| high speed | release LTO、PDF bounded parallelism、OOXML streaming、formula cache、XLSX prefix grid、theme cacheを実装 | 達成 |
| deterministic SVG | page番号・resource ID・float precisionを決定化。4形式の実入力を2回変換し全SVG hash一致 | 達成 |
| 参照実装を踏まえた独立再実装 | `slidekit`のSVG契約と`pdfsvgpptx`のscene/QA方針を比較し、code/assetsをvendorせず独立実装。READMEとthird-party noticeに記録 | 達成 |
| 精度改善の実測 | crop、table、callout、DOCX layout/OMML、PDF mesh/soft mask等のMAE・warning・IR比較を`BENCHMARKS.md`へ記録 | 達成 |

## 最終current-release smoke

| 形式 | 実入力bytes | pages | elapsed | 最大Page IR | 最大RSS | warning | 全SVG 2回一致 |
|---|---:|---:|---:|---:|---:|---:|---:|
| PDF | 19,362,031 | 33 | 1,519 ms | 51,766,293 | 261,275,648 | 0 | yes |
| PPTX | 4,348,562 | 10 | 38 ms | 3,133,030 | 14,794,752 | 0 | yes |
| XLSX | 955,150 | 6 | 24 ms | 609,419 | 33,423,360 | 0 | yes |
| DOCX | 5,953,676 | 25 | 42 ms | 1,711,222 | 31,621,120 | 0 | yes |

全代表SVGをlibrsvgで描画確認しました。

## Corpus evidence

- PPTX: 20 MiB以下の重複除去215件・1,910 pagesが215/215成功。残る4 warningは3資料のOLE静的preview通知だけです。
- XLSX: 139 unique workbook・860 pagesが139/139成功、warning file 0です。
- DOCX: 54 unique document・93 pagesが54/54成功、warning file 0です。
- Large PDF: 30件・1,660 pagesの初期6 failureを修正し30/30成功。Honda 33-page mesh資料とTI 276-page CCITT資料も完走しています。

## Quality gates

- `cargo fmt --all -- --check`
- unit tests 24/24
- forward integration tests 77/77、reverse integration tests 8/8
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo build --release`
- runtime/transitive dependency SPDX audit: mandatory copyleft・missing license 0

## 明示した境界

暗号化PDFは回避せず拒否します。cached drawingのないSmartArt再layout、Officeと画素単位で完全同一のWord pagination、動画/OLEの実行機能など、SVG静的変換の契約外または近似領域は`SUPPORT.md`へ明示しています。これらは入力を黙って別物へ変換せず、警告または明示拒否する運用契約です。
