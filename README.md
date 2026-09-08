# document-svg

プレビュー機能のガイド: [日本語](bindings/node/docs/preview.ja.md) · [English](bindings/node/docs/preview.en.md) · [简体中文](bindings/node/docs/preview.zh-CN.md)

PDF、PPTX、XLSX、DOCXを自己完結したSVGページへ変換し、SVGページをPPTX、
DOCX、XLSXへベクター画像として戻せるRustライブラリ／CLIです。

公開仕様をもとに実装した文書変換ライブラリです。ローカル検証用の文書や生成物は配布に含めません。

## まず何を使うか

通常の文書変換では、入力形式ごとの内部モジュールを直接呼び出す必要はありません。
拡張子が`.pdf`、`.pptx`、`.xlsx`、`.docx`のいずれかであるファイルを、次の
入口へ渡してください。

| 利用環境 | 入口 | 選ぶ場面 |
|---|---|---|
| コマンドライン | `docsvg` | 手作業、バッチ、動作確認 |
| Rust | `document_svg::convert_path` | Rustアプリへの組み込み |
| Python | `document_svg.convert` | Pythonの処理・AIパイプライン |
| Node.js | `document-svg`の`convert` | サーバー、Electron、Node.js |
| Rustで独自描画 | `document_svg::ir` + `document_svg::svg::write_page` | 自作データからSVGを生成 |
| SVGからOOXML | `document_svg::svg_to_openxml` | SVGをPPTX/DOCX/XLSXへ再パッケージ化 |

変換処理は、入力を形式別に解析し、共通の`Page` IRへ正規化してから、1ページずつ
SVGへ書き出します。たとえば10ページのPDFは1個のSVGではなく、
`page-0001.svg`から`page-0010.svg`までの10ファイルになります。

生成AIや実装担当者向けの詳しい手順、モジュール一覧、判断規則、完全なコード例は
**[SVG変換・モジュール利用ガイド](docs/AI_USAGE_GUIDE.md)** を参照してください。

Azure・AWS・GCPの公式アイコンを使った構成図作成には、変換コアから独立した
**[クラウド構成図作成ガイド](docs/CLOUD_ARCHITECTURE.md)** を用意しています。
`python3 authoring/cloud_icons.py fetch` で公式素材を取得し、検索・ID参照・JSONからのSVG生成・
検索可能なHTML一覧を利用できます。追加のRust依存や生成AIのAPIキーは不要です。

業務資料向けには **[3社の構成図テンプレートと直角配線](docs/BUSINESS_ARCHITECTURE.md)** を
利用できます。`python3 authoring/cloud_icons.py build-examples --output output/business-architecture`
でAzure・AWS・GCPのSVG、高解像度PNG、構成JSON、検証結果、角のスタイルを切り替えられる
確認画面を生成します（PNG描画には `rsvg-convert` が必要です）。

## 特徴

- PDF: MediaBox/CropBox/Rotate/UserUnit、path/positioned-glyph text/image/Form、ExtGState line/text state、nested clip、透明group/soft mask/knockout、特殊色空間、JPEG SMask・external CCITT、Function 0/2/3/4、shared-Decode＋page budget付きbounded shading Type 1–7、Shading/Tiling Pattern、embedded TrueType/CFF/Type1 outline、script-aware font/symbol fallback
- PPTX: master/layout継承・type fallback・footer/slide番号既定配置、master別theme・effectRef、nested group、adjustable preset/custom geometry（brace/arc/connector/flowchart/callout/can/cube/moon/donut/bracket/arrow等）、gradient/alpha、preset pattern fill、bounded outer shadow/glow、srcRect画像crop、duotone/grayscale/luminance/color-change、SmartArt cached drawing、zero-row recovery付きnative table、回転、word-aware段落、埋め込みraster/SVG、bounded EMF/WMF→SVG、cached chart、video/audio poster、OLE preview/placeholder
- XLSX: セル・style・結合・非表示行列、Excel number/date format、cross-sheet/defined-nameを含むcached欠落formulaのNumber/Text/Blank bounded評価、expression conditional formatting、Drawing画像・shape・text、cached chart、print area/break/title/page setup、巨大sheetのbounded auto tile
- DOCX: 用紙・余白、basedOn style chain、script font fallback、word-aware段落、super/subscript、OMML fraction/radical/script、multi-level list、footnote/endnote/comment、tracked final view、DrawingML/VML textbox、段落途中の改ページ、表、画像、section別first/even/default header/footer、PAGE field
- ページごとの決定的な`page-NNNN.svg`と、警告・時間・推定IRサイズを含む`conversion.json`
- 入力、ZIP展開、XMLイベント、ページ数、PDF展開ストリームに対する安全上限
- PDFだけ任意のページ並列化。逐次時は即時streaming、並列時も同時Page IRを`jobs`件に制限。既定は省メモリ優先の1 worker
- 暗号化PDFを明示拒否し、アクセス制限を回避しない

## CLI

公開ソースから導入する場合:

```bash
git clone https://github.com/ryusui-hiro/document-svg.git
cd document-svg
```

```bash
# `docsvg`を現在のユーザー用コマンドとしてインストール
cargo install --path . --locked

docsvg input.pdf --output output/pdf
docsvg slides.pptx --output output/pptx
docsvg book.xlsx --output output/xlsx
docsvg report.docx --output output/docx

# 埋め込みPDF fontをoutline化して見た目を優先
docsvg input.pdf --output output/pdf-fidelity --outline-embedded-pdf-text
```

`--outline-embedded-pdf-text`は明示的な高精度modeです。PDFに埋め込まれたfontのglyphを
SVG pathへ変換するため代替font差を減らせますが、その文字は通常のSVG textとしては
編集できません。fontのoutline化・埋め込み権利確認を求めるwarningを残し、既定では
editable textとglyph位置を維持します。

開発中のビルドを直接使う場合は、`cargo build --release`の後に
`target/release/docsvg`を同じ引数で実行できます。

PDFを4ページ並列で処理する例:

```bash
docsvg input.pdf --output output/pdf --jobs 4
```

主要な安全上限:

```text
--max-input-mib 512
--max-entry-mib 128
--max-pages 10000
```

### SVGからOpen XMLへの逆変換

単一SVG、または`page-NNNN.svg`を含むディレクトリを指定します。出力拡張子で
PPTX、DOCX、XLSXを選びます。

```bash
docsvg reverse page.svg --output page.pptx
docsvg reverse output/pdf --output pages.docx
docsvg reverse output/slides --output slides.xlsx
```

- PPTX: 1 SVGを1スライドへ格納
- DOCX: 1 SVGを1ページへ格納
- XLSX: 1 SVGを1シートへ格納

SVGは画像パートとしてベクターのまま保持され、SVG非対応consumer向けPNG fallbackも
同梱されます。active contentや外部参照を含むSVGは拒否します。段落、表、セル、数式、グラフ、
スライドマスターなど、元Office文書の意味構造を復元する変換ではありません。また、
既存の出力ファイルは上書きしません。

## Codexプラグイン／スキル

Codex・Claude Code共通の導入条件と日英中の依頼例は[導入ガイド](docs/PLUGIN_INSTALLATION.md)を参照してください。公開前の保護設定は[リポジトリのセキュリティ](docs/REPOSITORY_SECURITY.md)にまとめています。

このリポジトリには、そのまま配布できるCodexマーケットプレイス、`document-svg`
プラグイン、同名スキルが含まれます。リポジトリをcloneした別の利用者は、次の1コマンドで
CLIのビルドとプラグイン登録を完了できます。

```bash
./scripts/install-codex-plugin.sh
```

スクリプトを使わない場合は、次の3コマンドでも導入できます。

```bash
cargo install --path . --locked
codex plugin marketplace add .
codex plugin add document-svg@document-svg
```

プラグイン内のスキルは、PDF・PPTX・XLSX・DOCXのSVG化、`conversion.json`の
警告確認、代表ページの目視確認、SVGからOOXMLへの逆変換までを案内します。Codex内では
SVGそのものへのリンクに加え、librsvgで固定描画したPNGを表示できるため、Markdownや
Quick LookのSVG対応差に影響されにくくなっています。

```bash
plugins/document-svg/skills/document-svg/scripts/render-preview.sh \
  output/slides output/slides-preview 1400
```

`output/slides-preview/index.html`、`preview.json`、ページ別PNGが生成されます。実行可能要素を
含むSVGと外部参照は拒否し、既存の非空プレビューディレクトリは上書きしません。
インストール後は新しいCodexタスクを開始し、たとえば次のように依頼してください。

```text
$document-svg を使って report.docx をページ別SVGへ変換し、警告も確認して
$document-svg を使って page-*.svg を slides.pptx に戻して
```

Codexがこのリポジトリを直接開いた場合は、ルートの`AGENTS.md`からプラグインの場所と
導入方法を認識できます。リポジトリを読むだけで勝手にインストールせず、利用者が導入を
依頼したときに上記スクリプトを実行する設計です。

## GitHub CLI拡張

ローカルcheckoutのディレクトリ名に関係なく、次のスクリプトで`gh docsvg`を導入できます。

```bash
./scripts/install-gh-extension.sh

gh docsvg input.pdf --output output/pdf
gh docsvg convert slides.pptx --output output/slides
gh docsvg reverse output/slides --output repacked.pptx
```

GitHub CLI向けの独立リポジトリ`gh-docsvg`は未公開です。現在は上記のローカル導入を利用してください。
将来リモート拡張として配布する場合は、GitHub CLIの規約に従い、別リポジトリへ切り出します。
[GitHub公式の拡張仕様](https://docs.github.com/en/github-cli/github-cli/creating-github-cli-extensions)
に準拠しています。

## Claude Codeプラグイン

Claude Code用のマーケットプレイスとプラグインも同梱しています。

```bash
./scripts/install-claude-plugin.sh
```

手動導入する場合:

```bash
claude plugin marketplace add .
claude plugin install document-svg@document-svg
```

再起動後、`/document-svg:document-svg`で文書のSVG化とSVGからOOXMLへの逆変換を
利用できます。マーケットプレイスは[`.claude-plugin/marketplace.json`](.claude-plugin/marketplace.json)、
プラグイン定義は[`plugins/document-svg/.claude-plugin/plugin.json`](plugins/document-svg/.claude-plugin/plugin.json)です。
[Claude Code公式プラグイン仕様](https://code.claude.com/docs/en/plugins-reference)に従い、
`claude plugin validate --strict .`で検証できます。Claude Codeの`settings.json`はコメントを
含まない有効なJSONである必要があります。

## ライブラリ

```rust
use document_svg::{convert_path, ConvertOptions};

let report = convert_path(
    "input.pptx",
    "output/pptx",
    &ConvertOptions::default(),
)?;
println!("{} pages", report.page_count);
# Ok::<(), document_svg::Error>(())
```

SVGからOOXMLへ戻す場合:

```rust
use document_svg::{ReverseOptions, svg_to_openxml};

let report = svg_to_openxml(
    "output/pptx",
    "repacked.pptx",
    &ReverseOptions::default(),
)?;
println!("{} SVG pages", report.page_count);
# Ok::<(), document_svg::Error>(())
```

`convert_path`が通常変換の推奨APIです。低レベルAPIを使って独自の`Page` IRを
SVGにする例は[`examples/custom_ir.rs`](examples/custom_ir.rs)にあります。

## Python

`bindings/python`はPyO3／maturin製の薄いネイティブラッパーです。変換中は
PythonのGILを解放し、結果を`dict`として返します。

```python
from document_svg import convert

report = convert("input.pptx", "output/pptx", jobs=4)
print(report["page_count"])

from document_svg import reverse
reverse_report = reverse("output/pptx", "repacked.pptx")
```

ローカルwheelの作成:

```bash
python3 -m pip wheel --no-deps --wheel-dir dist ./bindings/python
```

PyPI公開時は、対応するOS／CPUごとにwheelを作成してから公開してください。
詳細は[`bindings/python/README.md`](bindings/python/README.md)を参照してください。

## Node.js／npm

`bindings/node`はnapi-rs製のNode-APIラッパーです。`convert()`はlibuvの
worker上で変換し、`Promise`を返すためJavaScriptイベントループをブロックしません。

```javascript
const { convert } = require("document-svg")

const report = await convert("input.pptx", "output/pptx", { jobs: 4 })
console.log(report.pageCount)

const reverseReport = await require("document-svg").reverse(
  "output/pptx",
  "repacked.pptx",
)
```

TypeScript／Electron／Node.jsサーバーで画面プレビューする場合は、出力先を必要とせず
SVG文字列を直接返す`preview()`を使えます。

```typescript
import { preview } from "document-svg"

const report = await preview("input.pptx", { maxPages: 100 })
const svgMarkup = report.pages[0].svg
console.log(report.needsReview)
```

renderer／ブラウザ側では、別entrypointのコピー・表示ヘルパーを利用できます。

```typescript
import {
  copySvgToClipboard,
  createSvgPreviewUrl,
} from "document-svg/preview-ui"

const imageUrl = createSvgPreviewUrl(svgMarkup)
// button click内で実行
await copySvgToClipboard(svgMarkup)
```

`preview()`は一時ファイルを自動削除し、既定で1ページ64 MiB、全ページ256 MiBまでの
SVGをNode.jsメモリへ保持します。詳しい安全な表示例は
[`bindings/node/README.md`](bindings/node/README.md)と、
[日本語](bindings/node/docs/preview.ja.md)・[English](bindings/node/docs/preview.en.md)・
[简体中文](bindings/node/docs/preview.zh-CN.md)のプレビューガイドを参照してください。

現在の環境向けネイティブパッケージの作成:

```bash
cd bindings/node
npm install
npm run build
npm test
```

npm公開時は`npm run create:npm-dirs`とnapi-rsのリリース処理を使い、ルート
パッケージとOS／CPU別パッケージを同じバージョンで公開します。詳細は
[`bindings/node/README.md`](bindings/node/README.md)を参照してください。

PythonとNode.jsのAPIはいずれもRustの`ConvertOptions`と同じ安全上限を公開します。
Pythonのレポートキーは`snake_case`、Node.jsでは`camelCase`です。ブラウザWASMは
ファイルシステムAPIの再設計が必要なため、このネイティブ配布には含めていません。

## 出力契約

```text
output/
├── page-0001.svg
├── page-0002.svg
└── conversion.json
```

SVGは白いページ背景、ポイント単位の`viewBox`、編集可能な`path`／`text`／`image`、入力要素を追跡する`data-*`属性を持ちます。各ページは一時ファイルへ書いてからrenameするため、途中失敗したページを完成ファイルとして残しません。

`conversion.json`の警告は品質契約の一部です。警告がある変換を「完全再現」として扱わず、呼び出し側で`needs_review`、失敗、許容のいずれかを選択してください。

## 検証

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release

(cd bindings/node && npm run build && npm test)
python3 -m pip wheel --no-deps --wheel-dir dist ./bindings/python
```

4形式の最小・機能fixtureをテスト内で生成し、公開APIからSVGまで通す統合テストを含みます。ローカルQAでは参照PDF、参照PPTX、実XLSX、実DOCXを変換し、`rsvg-convert`で代表ページを同一DPI比較しています。

詳しい設計は [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)、対応範囲は [docs/SUPPORT.md](docs/SUPPORT.md)、実測値は [docs/BENCHMARKS.md](docs/BENCHMARKS.md)、goal全体の要求別証拠は [docs/COMPLETION_AUDIT.md](docs/COMPLETION_AUDIT.md) を参照してください。

## ライセンス

このリポジトリのコードは `MIT OR Apache-2.0` です。利用者はいずれかを選択できます。再配布時は、選択したライセンスに従って著作権表示・ライセンス文を必ず保持してください。詳細は[ライセンス](LICENSE)を参照してください。直接・推移依存は許容的ライセンスだけを選び、監査方法を [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) に記録しています。
