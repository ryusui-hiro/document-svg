# AstraAnnotation Web文書テストと document-svg 側への確認

実施日: 2026-09-13  
テスト対象: AstraAnnotation と、SVG RUST リポジトリの現在の作業ツリー  

## 結論

PDF、DOCX、PPTX、XLSX の実文書で、Rust の `docsvg` CLI と AstraAnnotation の変換APIを確認した。今回の資料では document-svg の変換不具合は再現しなかったため、document-svg のコード変更・GitHub Issue は不要と判断した。SVG RUST では `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings` がすべて成功した。

テストで別途見つかったAstraAnnotation側のExcelJS制約は修正済み。MarkItDownの画像入りXLSXをプレビューできてもワークブックとして開けない問題があり、default namespace形式のSpreadsheetDrawingをExcelJSが解析できないことが原因だった。読み込み時だけ`xdr:`接頭辞に正規化し、編集・新規XLSX書き出し後にも埋め込み画像が残る回帰テストを追加した。再試験ではworkbook APIと新しいXLSXの出力が成功した。

## 入力資料と変換結果

| 資料 | 公開元と用途 | 入力サイズ | `docsvg` 出力 |
|---|---|---:|---|
| UKEF Performance Highlights 2024/25 PDF | [GOV.UKの年次報告ページ](https://www.gov.uk/government/publications/uk-export-finance-annual-report-and-accounts-2024-to-2025)。PDFの表示ページ数と実ページ数の確認 | 769,198 B | 4ページ、警告なし、SVG内にテキストと画像 |
| W3C WAI Headers and Footers PDF | [W3C WAIサンプル](https://www.w3.org/WAI/WCAG22/working-examples/pdf-headers-footers/headers-footers-oo.pdf)。ヘッダー／フッター、文書構造、複数ページの確認 | 83,394 B | 5ページ、警告なし、SVG内にテキスト |
| MarkItDown [`pdf_multipage.pdf`](https://raw.githubusercontent.com/microsoft/markitdown/main/packages/markitdown-ocr/tests/ocr_test_data/pdf_multipage.pdf) | Microsoft MarkItDown OCRテスト資料。末尾が不完全なPDFのフォールバック確認 | 14,418 B | 3ページ、警告なし |
| MarkItDown [`docx_multipage.docx`](https://raw.githubusercontent.com/microsoft/markitdown/main/packages/markitdown-ocr/tests/ocr_test_data/docx_multipage.docx) | 同リポジトリのWord画像配置・複数ページテスト | 48,583 B | 3ページ、画像3点をSVGに保持、警告なし |
| MarkItDown [`pptx_complex_layout.pptx`](https://raw.githubusercontent.com/microsoft/markitdown/main/packages/markitdown-ocr/tests/ocr_test_data/pptx_complex_layout.pptx) | 同リポジトリの画像入りスライドテスト | 32,130 B | 1スライド、画像1点をSVGに保持、警告なし |
| MarkItDown [`xlsx_complex_layout.xlsx`](https://raw.githubusercontent.com/microsoft/markitdown/main/packages/markitdown-ocr/tests/ocr_test_data/xlsx_complex_layout.xlsx) | 同リポジトリの複数シート・画像入りワークブックテスト | 22,744 B | 3ページ、画像4点をSVGに保持、警告なし |
| Microsoft Data Validation Examples XLSX | [Microsoft Download Center](https://www.microsoft.com/en-us/download/details.aspx?id=53669)。入力規則、数式、14シートの確認 | 398,855 B | 31ビジュアルページ、14シート、画像30点。E7とE20に数式キャッシュがないため警告1件 |

MarkItDown OCRパッケージの[ライセンス](https://github.com/microsoft/markitdown/blob/main/packages/markitdown-ocr/LICENSE)はMITだが、個々のテストファイルには別のライセンス表示を確認できなかった。これらのファイルはローカルテストのみに使い、リポジトリへは追加していない。W3Cの資料は[公式利用条件](https://www.w3.org/WAI/about/using-wai-material/)に従い、変更・再配布していない。Microsoftのブックもテスト用に取得しただけで再配布していない。GOV.UK資料は公開ページの利用条件に従う。

## 実行した確認

- AstraAnnotation APIで7ファイルすべての`POST /api/convert`がHTTP 200。PDF/DOCX/PPTXと両XLSXのページプレビューSVGもHTTP 200。
- Microsoft XLSXのworkbook APIは14シートを返した。MarkItDown XLSXは今回の修正前はExcelJSの`TypeError: Cannot read properties of undefined (reading 'anchors')`でHTTP 415だった。修正後は3シートを返し、ネイティブXLSX出力は有効なZIPとして開き、元の埋め込み画像4点を保持した。
- SVG RUST の `docsvg` CLI は7資料をすべて変換した。ページ数は上表の通り。
- SVG RUST標準確認: `cargo fmt --all -- --check`成功、`cargo test --workspace --locked`は630件すべて成功、`cargo clippy --workspace --all-targets --locked -- -D warnings`成功。
- AstraAnnotation: `npm test` 94件成功、`npm run build`成功、`npm audit --audit-level=high`で脆弱性0件。修正後、画像入りMarkItDown XLSXのworkbook APIがHTTP 200となり、新規XLSXの再読込と画像4点の保持を確認した。
- APIテストではAIプロバイダーを呼び出していない。PDF/Officeの変換、SVG応答、workbook読取・書出しのみを検証した。

## document-svgへの依頼事項

このサンプル一式ではmodule側の再現可能な不具合は見つからなかったため、コード変更やIssueは起こさない。特に、末尾が不完全なMarkItDown PDFも3ページへ変換され、画像入りOfficeファイルもそれぞれの画像をSVGに保持した。Microsoft XLSXの警告は元ファイルに数式のキャッシュ値がないことを示しており、入力資料に起因する期待された診断だった。

ExcelJSのdefault-namespace解析問題はAstraAnnotation側の適合処理と回帰テストで直した。これはdocument-svgが正常に生成したSVGプレビューと別のワークブック編集処理の問題であり、module側へ依頼しない。

ダウンロードした原ファイルはAstraAnnotationリポジトリ内のGit ignore対象`output/web-samples/`に置いてあり、SVG RUSTの作業ブランチには追加していない。SVG RUSTの既存変更ファイルは編集・stage・commitしていない。このレポートだけを新規作成した。
