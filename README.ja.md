# document-svg

**PDF、Word、Excel、PowerPoint、図、CAD図面の全ページを、作ったアプリがなくてもSVG画像で見られます。変換は手元のマシンで行います。**

[English](README.md) · [简体中文](README.zh-CN.md) ·
[プロジェクトサイト](https://ryusui-hiro.github.io/document-svg/ja/) ·
[サンプル](https://ryusui-hiro.github.io/document-svg/ja/samples.html) ·
[リリース](https://github.com/ryusui-hiro/document-svg/releases)

[![npm](https://img.shields.io/npm/v/document-svg?label=npm)](https://www.npmjs.com/package/document-svg)
[![PyPI](https://img.shields.io/pypi/v/document-svg?label=PyPI)](https://pypi.org/project/document-svg/)
[![crates.io](https://img.shields.io/crates/v/document-svg?label=crates.io)](https://crates.io/crates/document-svg)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#ライセンス)

document-svg は、文書や図、技術図面を、1ページ1枚のふつうのSVG画像に変換します。
SVGはどのブラウザでも表示でき、拡大してもぼやけず、ほかの画像と同じようにWebページに貼れます。
ページと一緒に短いレポートも出力し、そのまま再現できなかった部分を隠さず書き出します。

単体で開くアプリではなく、Webサイト、社内ツール、レビューの流れ、AIアシスタントなどに組み込んで使う変換部品です。
Node.js、Python、Rust のライブラリと、コマンド（`docsvg`）として使えます。

| PowerPoint | Excel | draw.io | CAD図面（DXF） |
|---|---|---|---|
| <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/pptx/page-0001.svg" alt="SVGに変換したPowerPointのスライド" width="200"> | <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/xlsx/page-0001.svg" alt="SVGに変換したExcelのシート" width="200"> | <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/drawio/page-0001.svg" alt="SVGに変換したdraw.ioの図" width="200"> | <img src="https://ryusui-hiro.github.io/document-svg/assets/samples/dxf/page-0001.svg" alt="SVGに変換したDXF図面" width="200"> |

上の画像はどれも、このリポジトリのサンプルファイルを変換したままのもので、手直しはしていません。

## 使う理由

- **ファイルは手元から出ません。** 変換は、あなたのPCか自社サーバーで終わります。アップロードもアカウント登録も利用状況の送信もないので、社外に出せない契約書や設計図でもプレビューを作れます。
- **誰でも開けます。** 確認する人に Office も AutoCAD も専用ビューアも要りません。ブラウザがあれば十分。
- **できなかったことを正直に書きます。** 何でも完璧に再現できる変換ツールはありません。document-svg は、グラフやフォントを黙って落とすかわりに、省略や近似をした箇所をページごとに記録します。共有してよい出来かどうかは、それを読んで決められます。
- **開いても、中身は実行しません。** マクロ、スクリプト、フォームの送信、外部へのリンクは、表示するか読み飛ばすだけです。サイズとページ数に上限があるので、巨大なファイルや壊れたファイルがメモリを食いつぶすこともありません。
- **ツールがひとつで済みます。** Office文書、PDF、図、CADや3Dモデル、研究データや業務データまで、同じ呼び出し方で変換し、同じ形で出力します。
- **元の形式にも戻せます。** SVGのページを PowerPoint、Word、Excel、draw.io、CAD のファイルにまとめ直せます。

## こんな使い方をしています

- **コードレビューで文書の変更を見る。** プルリクエストで変わったスライドや表計算、PDFを GitHub Action が変換します。「バイナリファイルが変更されました」だけで終わりません。
- **アップロードされたファイルを自分のアプリで見せる。** サーバーに Office を入れたり、有料のビューアを契約したりする必要はありません。
- **図をコードと一緒に最新に保つ。** draw.io、Mermaid、PlantUML、D2、Graphviz のファイルを、ドキュメントのビルドのたびに画像にします。
- **CADがなくても図面や3Dモデルを確認する。** 間取り図、プリント基板、加工パス、3DモデルをWebページで見られます。
- **AIアシスタントに文書を読ませる。** スクリーンショットのかわりに、確実な手順をひとつ渡せます。

それぞれの詳しい説明は[プロジェクトサイト](https://ryusui-hiro.github.io/document-svg/ja/use-cases.html)にあります。

## インストール

どこから呼び出すかで、ひとつ選んでください。全部入れる必要はありません。

| やりたいこと | インストール |
|---|---|
| Node.js や Electron のアプリから変換する（Node.js 18 以降） | `npm install document-svg` |
| Python から変換する（Python 3.10 以降） | `python -m pip install document-svg` |
| `docsvg` コマンドや Rust のライブラリを使う | `cargo install document-svg --locked` / `cargo add document-svg` |

npm と pip のパッケージは Windows、macOS、Linux 向けにビルド済みなので、Rust は要りません。
Rust を入れずにコマンドだけ使うこともできます。[リリース](https://github.com/ryusui-hiro/document-svg/releases)から自分のOS用のファイルをダウンロードし、`docsvg` を `PATH` の通った場所に置いてください。

### 入手先とインストール先

| パッケージ | ページ | インストールされる場所 |
|---|---|---|
| npm の `document-svg` | [npmjs.com/package/document-svg](https://www.npmjs.com/package/document-svg) | プロジェクトの `node_modules/document-svg`。OSに合ったビルド済みのパッケージ（`node_modules/document-svg-darwin-arm64` など）も1つ入ります |
| PyPI の `document-svg` | [pypi.org/project/document-svg](https://pypi.org/project/document-svg/) | 使っている環境の `site-packages/document_svg` |
| crates.io の `document-svg` | [crates.io/crates/document-svg](https://crates.io/crates/document-svg) | `cargo install` なら `docsvg` が `~/.cargo/bin`（Windowsでは `%USERPROFILE%\.cargo\bin`）に入ります。`cargo add` ならライブラリとしてプロジェクトに加わります |
| GitHub のリリース | [リリース](https://github.com/ryusui-hiro/document-svg/releases) | 展開した場所。`docsvg` を `PATH` の通った場所に置きます |

入っているかどうかは、次のコマンドで確かめられます。`npm ls document-svg`、`python -m pip show document-svg`、`docsvg --version`。
Rust のAPIの説明は [docs.rs](https://docs.rs/document-svg) にあります。4つの使い方すべての関数とオプション、既定値は[開発者リファレンス](docs/API.ja.md)（[サイト版](https://ryusui-hiro.github.io/document-svg/ja/reference.html)）にまとめています。GitHub Packages の認証付きnpmミラーについては [docs/PUBLISHING.md](docs/PUBLISHING.md#installing-the-github-packages-mirror) を見てください。

## 試してみる

```sh
docsvg slides.pptx --output out/slides
```

出力先には、新しいフォルダか空のフォルダを指定します。ページごとのSVGと、変換の記録が1つできます。

```text
out/slides/
├── page-0001.svg
├── page-0002.svg
└── conversion.json
```

ページはどのブラウザでも開けます。記録には **警告（warnings）** が入ります。フォントを代わりのものにした、対応していない塗りを飛ばした、といった近似や省略の知らせです。
エラーなく終わっても、完璧とは限りません。警告があれば、共有する前に人の目でページを確かめてください。

「1ページ」は、ファイルの種類ごとに自然な単位です。PDFならページ、PowerPointならスライド1枚、Wordなら組版した1ページ、Excelなら印刷したときの1ページ、draw.io なら図の1ページになります。

### Node.js / TypeScript から

```js
const { convert, preview } = require('document-svg')

async function main() {
  // SVGファイルをフォルダに書き出す
  const report = await convert('slides.pptx', 'out/slides')
  console.log(report.pageCount, report.warnings)

  // ファイルに書かずにメモリ上で受け取る（ブラウザへ送るときなど）
  const result = await preview('slides.pptx')
  const firstPageSvg = result.pages[0].svg
  console.log('要確認:', result.needsReview)
}

main().catch(console.error)
```

変換は Node.js（サーバーや Electron のメインプロセス）で、メインスレッドを止めずに動きます。
ブラウザ側では、ネイティブコードを読み込まない小さな `document-svg/preview-ui` を使って、各ページを画像として表示します。
詳しくは [Node.js ガイド](bindings/node/README.md)と[プレビューガイド](bindings/node/docs/preview.ja.md)を見てください。

### Python から

```python
from document_svg import convert, preview

report = convert("report.docx", "out/report")
print(report["page_count"], report["warnings"])

result = preview("report.docx")          # ファイルに書かず、メモリ上で受け取る
first_page_svg = result["pages"][0]["svg"]
```

詳しくは [Python ガイド](bindings/python/README.md)を見てください。

### Rust から

```rust
use document_svg::{convert_path, ConvertOptions};

fn main() -> Result<(), document_svg::Error> {
    let report = convert_path("report.pdf", "out/report", &ConvertOptions::default())?;
    println!("{} pages, {} warnings", report.page_count, report.warnings.len());
    Ok(())
}
```

APIの説明は [docs.rs](https://docs.rs/document-svg) にあります。

### ブラウザだけで動かす

PDF、Word、Excel、PowerPoint に限り、ブラウザの中だけで変換する WebAssembly 版があります。サーバーもアップロードも要りません。
パッケージとしては配布していないので、[`bindings/wasm`](bindings/wasm/README.md) から自分でビルドしてください。ビルドする前に[ブラウザで試せます](https://ryusui-hiro.github.io/document-svg/viewer/)。

## アプリに表示する

各ページは画像として表示してください（`<img>` 要素など）。SVGの中身をページのHTMLに直接貼り込むのは避けてください。
付属の表示用ヘルパーは簡単な確認をしますが、どんなSVGでも無害にするフィルターではありません。
画像として表示した文字は、選択や検索ができません。

文字は、SVGを表示する環境にあるフォントで描かれます。別のPCでは少し違って見えることがあります。
PDFでは、文字を選べることより見た目を優先したいときに、埋め込みフォントの文字を図形（アウトライン）にできます。

## SVGを Office や CAD のファイルに戻す

```sh
docsvg reverse out/slides --output slides.pptx
docsvg reverse out/report --output report.docx
docsvg reverse out/drawing --output drawing.dxf
```

出力ファイルの拡張子で形式が決まります。PowerPoint、Word、Excel、PDF、draw.io、DXF、G-code、STL などに対応しています。
戻るのはページの見た目です。段落、セル、数式、グラフまでは組み立て直しません。Wordで編集し直せるファイルに変換する機能ではない、と考えてください。

draw.io の図だけは例外です。`--embed-drawio-source` を付けて変換しておくと、`reverse` で元の編集できる図に戻ります。

## 対応形式

| 分野 | 例 |
|---|---|
| オフィス文書 | PDF、Word、Excel、PowerPoint（新旧どちらの形式も）、OpenDocument、RTF、メール、カレンダー、電子書籍、Markdown、HTML |
| 図 | draw.io、Visio、Mermaid、PlantUML、D2、Graphviz、BPMN |
| CAD・CAM・3D | DXF、Gerber、G-code、HP-GL、STL、OBJ、STEP、IGES、glTF、IFC |
| シミュレーション | Gmsh、VTK、OpenFOAM、Abaqus、Nastran などのメッシュ |
| データ・地図 | CSV、JSON、YAML、グラフ、LaTeX の数式、GeoJSON、KML、GeoPackage |
| 画像 | PNG、JPEG、TIFF、WebP、DICOM、SVG |

図全体を描ける形式もあれば、DWG や Access のデータベースのように、中身の概要を表示する形式もあります。
各形式には、A（ファイルの中身をそのまま描く）、B（ピクセルから輪郭をなぞる）、C（表などの構造を図から推測する）の評価を付けています。

- [対応形式の一覧（検索できます）](https://ryusui-hiro.github.io/document-svg/ja/formats.html) — お使いのファイルが使えるか調べる
- [対応形式の詳細](docs/FORMATS.ja.md) — 拡張子ごとの技術的な注意
- [対応機能と制限](docs/SUPPORT.md)

## AIアシスタントや GitHub から使う

| 使う場所 | やること |
|---|---|
| Claude Code | `/plugin marketplace add ryusui-hiro/document-svg` のあと `/plugin install document-svg@document-svg` |
| Codex | リポジトリを手元に置いて `./scripts/install-codex-plugin.sh` を実行し、新しいタスクを始める |
| GitHub Copilot | Copilot のセットアップ手順に、下のセットアップ用アクションを足す |
| GitHub CLI | `./scripts/install-gh-extension.sh` を実行し、`gh docsvg convert report.pdf --output preview/report` |
| GitHub Actions | `uses: ryusui-hiro/document-svg/.github/actions/setup-docsvg@main` |

インストーラーは、Rust がなければSHA256で照合したリリース版を入れます。
プルリクエストで変わった文書を確かめるには、次のワークフローをリポジトリに足します。

```yaml
name: Document preview
on:
  pull_request:
    paths: ['**.pdf', '**.pptx', '**.xlsx', '**.docx']
jobs:
  preview:
    uses: ryusui-hiro/document-svg/.github/workflows/document-preview.yml@main
    permissions:
      contents: read
      pull-requests: write
```

変わった文書がページごとの画像になり、実行結果からダウンロードできます。文書ごとのページ数と警告の数は、コメントで知らせます。
詳しくは [GitHub 連携ガイド](docs/GITHUB_INTEGRATION.md)、[プラグイン導入ガイド](docs/PLUGIN_INSTALLATION.md)、[AIエージェント向けガイド](docs/AI_USAGE_GUIDE.md)を見てください。

## 安全性と限界

- ファイルはどこにも送りません。マクロ、スクリプト、外部リンクを実行したり取得したりもしません。
- 開くのにパスワードが要るPDFは、変換を断ります。パスワードを破ろうとはしません。「印刷禁止」などの権限パスワードだけが付いたPDFは、ほかのビューアと同じように開きます。その制限を守らせる仕組みはありません。
- 入力の大きさ、圧縮ファイルの中身、ページ数に上限があります。難しいファイルを通すためだけに引き上げないでください。
- 既存のファイルを上書きしません。
- 誰でもファイルを上げられるサービスでは、メモリと時間に上限をかけた別のプロセスで変換してください。
- 警告は「確認してください」という合図で、正確さや安全性の証明ではありません。医療画像やレポートの匿名化もしません。

公開サービスで使う前に、[安全性と限界](https://ryusui-hiro.github.io/document-svg/ja/safety.html)と[セキュリティポリシー](SECURITY.md)を読んでください。

### 対応環境とトラブルシューティング

| 環境（x64・ARM64） | npm パッケージ | Python パッケージ | コマンド |
|---|---|---|---|
| Windows | ビルド済み | ビルド済み | ビルド済み |
| macOS | ビルド済み | ビルド済み | ビルド済み |
| Linux（Ubuntu、Debian など glibc 系） | ビルド済み | ビルド済み | ビルド済み |
| Linux（Alpine など musl 系） | ビルド済み | ソースからビルド | ビルド済み |

- pip がソースからのビルドを始めたら、その Python と環境向けのビルド済みパッケージがありません。まず pip を更新してください。ビルドには Rust とCのリンカーが要ります。
- npm で入れたのに読み込めないときは、オプションの依存パッケージが入っているか、Node.js とマシンのCPUの種類が合っているかを確かめてください。`node_modules` を別のOSにコピーしても動きません。
- 変換したSVGは、最近のブラウザ、resvg、librsvg 2.46 以降で表示できます。

## クラウド構成図を描く

変換とは別に、Azure、AWS、Google Cloud の公式アイコンで構成図を組み立てるツールも入っています。
使い方は[クラウド構成図ガイド](docs/CLOUD_ARCHITECTURE.md)と[業務資料向けテンプレート](docs/BUSINESS_ARCHITECTURE.md)を見てください。

## 開発に参加する

```sh
cargo test --workspace --locked
```

確認手順の全体は [CONTRIBUTING.md](CONTRIBUTING.md)、変換の仕組みは [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) にあります。
リリース手順は [docs/PUBLISHING.md](docs/PUBLISHING.md)、変更履歴は [CHANGELOG.md](CHANGELOG.md) を見てください。

## ライセンス

**MIT OR Apache-2.0** です。どちらかを選んで使えます。再配布するときは、選んだライセンスが求める著作権表示とライセンス文を残してください。
[LICENSE](LICENSE)、[LICENSE-MIT](LICENSE-MIT)、[LICENSE-APACHE](LICENSE-APACHE)、同梱の[第三者ライセンス](THIRD_PARTY_LICENSES.txt)を見てください。
