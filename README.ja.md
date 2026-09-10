# document-svg

PDF・PowerPoint・Excel・Word・draw.ioを、**1ページ＝1枚のSVG**に変換するRustのライブラリとCLIです。
逆に、SVGをPPTX・DOCX・XLSXへ包み直すこともできます。

出来上がったSVGは、対応するブラウザで開けます。画像はファイル内に埋め込みます。埋め込み画像はSVG 2の`href`だけで参照するため、描画側は最近のブラウザ、resvg、またはlibrsvg 2.46以降が必要です。
通常の文字表示は環境のフォントに依存するため、別のPCでは字形や配置が変わる場合があります。
PDFの埋め込みフォントをアウトライン化するオプションもあります。

プレビューガイド: [日本語](bindings/node/docs/preview.ja.md) · [English](bindings/node/docs/preview.en.md) · [简体中文](bindings/node/docs/preview.zh-CN.md)

まず[サンプル](samples/)を見てください。5形式の入力ファイルと、そこから生成されたSVGが置いてあります。

---

## なぜSVGなのか

SVGに変換すると、文書プレビューをブラウザ標準の画像表示で組み込めます。
ページ単位で保存でき、図形や文字などのベクター部分を拡大して表示できます。

SVGはブラウザが最初から読める画像形式です。だから、こうなります。

- **`<img>`タグで表示できる。** ビューアーも実行時のライブラリも要りません
- **文字情報を保持できる。** 通常はSVGの`text`として残します。`<img>`表示では文字選択・検索はできず、アウトライン化した文字もテキストではありません
- **ベクター部分を拡大できる。** 図形や文字は滑らかに表示できます。埋め込んだ写真などの解像度は元画像に依存します
- **画像を埋め込める。** 画像は`data:`URIとして保存します。表示環境のフォント差については元文書と比較してください
- **テキストなので差分が取れる。** 生成物をGitに入れて、レイアウト変更をレビューできます

`page-0001.svg`から`page-0117.svg`までが並ぶので、ページ送りは配列の添字を変えるだけです。

### こんなときに使えます

- Webアプリに文書プレビューと、SVG画像・SVGソースのコピー機能を付けたい
- ElectronやNode.jsのアプリで、Officeを入れずにスライドを表示したい
- 生成AIのパイプラインに文書を流し込みたい。SVGはテキストなので、そのまま渡せます
- CIで文書の見た目が壊れていないか確認したい。SVGを比較すれば差分が出ます

## ソースから試す

```bash
git clone https://github.com/ryusui-hiro/document-svg.git
cd document-svg
cargo build --release

./target/release/docsvg samples/source/sample.pptx --output /tmp/slides
# macOSの場合。その他の環境では生成したSVGを対応ブラウザで開きます。
open /tmp/slides/page-0001.svg
```

自分のファイルでも同じです。拡張子が`.pdf` `.pptx` `.xlsx` `.docx`なら、そのまま渡せます。

```bash
./target/release/docsvg 決算資料.pdf --output out/
```

常用するなら`cargo install --path . --locked`で`docsvg`コマンドとして入ります。

## 何が入って、何が出るか

```text
out/
├── page-0001.svg
├── page-0002.svg
└── conversion.json
```

「1ページ」の意味は形式ごとに違います。

| 入力 | 1ページになるもの |
|---|---|
| PDF | PDFのページ |
| PPTX | スライド1枚 |
| DOCX | ページサイズ・余白・改ページなどを反映して組版したページ |
| XLSX | 印刷したときの1ページ |
| drawio | `<diagram>`要素1つ、つまりエディタのページ1枚 |

XLSXは注意が必要です。180行のシートは1枚のSVGにはなりません。用紙設定に従って分割され、
[サンプル](samples/svg/xlsx/)では4ページになります。Excelと完全同一の組版を保証するものではありません。

### conversion.jsonは飾りではない

変換のたびに書かれるレポートです。ページ数と所要時間のほかに、**warnings**が入ります。

```json
{
  "page_count": 2,
  "elapsed_ms": 6,
  "warnings": []
}
```

ここには未対応要素や近似処理、利用時の確認事項が入ります。見た目が同じでも通知が出る場合があります。
たとえば、埋め込みフォントを代替した、対応していない網掛けを飛ばした、といった内容です。

`warnings`が空でない変換を「完全再現」として扱わないでください。人に見せる前にレビューへ回すのか、
そこで失敗させるのか、許容するのか。判断は呼び出し側で決められます。

## 使う

### コマンドライン

```bash
docsvg input.pdf --output out/            # 変換
docsvg input.pdf --output out/ --jobs 4   # PDFを4ページ並列で処理
docsvg input.pdf --output out/ --outline-embedded-pdf-text
```

`--jobs`はPDFだけに効きます。手元のMac（Apple Silicon）で9.7MB・96ページのPDFは
1ワーカー1.8秒、4ワーカー1.0秒。12MB・252ページの画像とType 1フォントが多い資料は
1ワーカー10.8秒、4ワーカー4.3秒でした。Office形式は常に1ページずつ流すので、
`--jobs`を上げてもメモリだけ増えて速くはなりません。

`--outline-embedded-pdf-text`は見た目を優先する指定です。埋め込みフォントの字形を
SVGのパスに変換するため代替フォントとの差が消えますが、その文字はSVGのテキストとして
編集も検索もできなくなります。フォントの埋め込み権利を確認する警告も残ります。

安全のための上限は引数で変えられます。

```bash
--max-input-mib 512   # 入力ファイルの大きさ
--max-entry-mib 128   # ZIPの1エントリ、PDFの1ストリームを展開したときの大きさ
--max-pages 10000     # 出力ページ数
```

### Rust

```rust
use document_svg::{convert_path, ConvertOptions};

let report = convert_path("input.pptx", "out", &ConvertOptions::default())?;
println!("{} pages", report.page_count);
# Ok::<(), document_svg::Error>(())
```

通常の変換は`convert_path`だけで足ります。自前のデータから直接SVGを組み立てたい場合は
`document_svg::ir`と`document_svg::svg::write_page`を使います。例が
[`examples/custom_ir.rs`](examples/custom_ir.rs)にあります。

### Python

```python
from document_svg import convert

report = convert("input.pptx", "out", jobs=4)
print(report["page_count"])
```

変換中はGILを解放するので、他のスレッドは止まりません。ビルドにはmaturinが要ります。

```bash
python3 -m pip wheel --no-deps --wheel-dir dist ./bindings/python
```

詳細は[`bindings/python/README.md`](bindings/python/README.md)へ。

### Node.js

```javascript
const { convert } = require("document-svg")

const report = await convert("input.pptx", "out", { jobs: 4 })
console.log(report.pageCount)
```

`convert()`はlibuvのワーカースレッドで動くので、イベントループを止めません。

画面に出すだけならファイルを書かずに済みます。`preview()`はSVGの文字列を直接返します。

```typescript
import { preview } from "document-svg"

const report = await preview("input.pptx", { maxPages: 100 })
const svg = report.pages[0].svg
console.log(report.needsReview)
```

ブラウザ側には、表示とコピー用のヘルパーが別入口で入っています。

```typescript
import { copySvgToClipboard, createSvgPreviewUrl } from "document-svg/preview-ui"

const url = createSvgPreviewUrl(svg)
await copySvgToClipboard(svg)   // クリック時に呼ぶ
```

`preview()`は一時ファイルを自動で消し、既定では1ページ64MiB・全体256MiBまでをメモリに載せます。
Electronやサーバーでの安全な出し方は、プレビューガイドを読んでください。
[日本語](bindings/node/docs/preview.ja.md) / [English](bindings/node/docs/preview.en.md) /
[简体中文](bindings/node/docs/preview.zh-CN.md)

自分の環境向けにビルドする場合:

```bash
cd bindings/node && npm install && npm run build && npm test
```

PythonとNode.jsのAPIは、Rustの`ConvertOptions`と同じ上限を公開しています。
レポートのキーだけ違います。Pythonは`snake_case`、Node.jsは`camelCase`。
ブラウザ向けのWASMは、ファイルシステムAPIを作り直す必要があるため含めていません。

## SVGからOffice文書へ戻す

1枚のSVGでも、`page-NNNN.svg`が入ったディレクトリでも渡せます。出力の拡張子で形式が決まります。

```bash
docsvg reverse page.svg --output page.pptx
docsvg reverse out/ --output pages.docx
```

SVGはベクター画像のまま格納され、SVGを読めないソフト向けにPNGも同梱されます
（PowerPointが自分で書き出すのと同じ形です）。1枚のSVGがPPTXでは1スライド、
DOCXでは1ページ、XLSXでは1シートになります。

**元の文書構造は戻りません。** 段落も表もセルも数式も、画像の中の線と文字になります。
「Wordで編集し直せるファイルに変換する機能」ではない、と理解して使ってください。

drawioだけは例外があります。出力先を`.drawio`にすると、SVGは`<diagram>`1枚になります。

```bash
docsvg reverse out/ --output diagram.drawio
```

このとき、**元の図面のsourceを持っているSVGは、画像ではなく編集可能な図形として復元します。**
対象は2種類です。drawioのSVG書き出しで「Include a copy of my diagram」を付けたもの（rootの
`content`属性に`mxfile`が入っています）と、`--embed-drawio-source`を付けて変換した本ツールの
SVGです。

```bash
docsvg diagram.drawio --output out/ --embed-drawio-source
docsvg reverse out/ --output diagram.drawio   # 元の図面に戻る
```

sourceを持たないSVG（PDFやOfficeから変換したものなど）は、これまでどおり画像として1ページに
貼り付けます。図形には戻りません。
スクリプトや外部参照を含むSVGは受け付けません。既存の出力ファイルも上書きしません。

## 対応している範囲

現物で確かめるのが早いので、まず[サンプル](samples/)と[対応表](docs/SUPPORT.md)を見てください。
形式ごとの詳細は長いので畳んであります。

<details>
<summary>形式別の対応内容</summary>

**PDF** — MediaBox/CropBox/Rotate/UserUnit、パス、字形単位で配置されたテキスト、画像、Form
XObject、ExtGStateの線・文字状態、入れ子のクリップ、透明グループ・ソフトマスク・ノックアウト、
特殊な色空間、JPEGのSMaskと外部CCITT、関数タイプ0/2/3/4、共有Decodeとページ単位の上限を持つ
シェーディング（タイプ1〜7）、シェーディング／タイリングパターン、埋め込みTrueType・CFF・Type1の
字形、文字体系を見た代替フォント。埋め込みのない標準14フォント（Helvetica、Times、Courier、
Symbol、ZapfDingbats）は、`/Widths`がなくても正しい字幅で配置します。

**PPTX** — マスターとレイアウトの継承、種別ごとのフォールバック、フッターとスライド番号の
既定位置、マスターごとのテーマとeffectRef、入れ子のグループ、調整可能なプリセット図形と
カスタム図形（brace/arc/コネクタ/フローチャート/吹き出し/円柱/立方体/月/ドーナツ/括弧/矢印ほか）、
グラデーションと不透明度、プリセットの網掛け、範囲を限った外側の影とグロー、srcRectによる画像の
切り抜き、デュオトーン・グレースケール・輝度・色変換、SmartArtのキャッシュ描画、行が空のときの
復旧を含むネイティブの表、回転、単語単位で折り返す段落、埋め込みラスターとSVG、上限つきの
EMF/WMF→SVG、キャッシュされたグラフ、動画・音声のポスター、OLEのプレビューと代替表示

**XLSX** — セル、スタイル、結合、非表示の行と列、Excelの数値・日付書式、シート間参照や
定義名を含むキャッシュ欠落数式の上限つき評価（数値・文字列・空白）、式による条件付き書式、
図形と画像とテキスト、キャッシュされたグラフ、印刷範囲・改ページ・印刷タイトル・用紙設定、
巨大なシートの自動分割

**DOCX** — 用紙と余白、basedOnによるスタイル継承、文字体系を見たフォント選択、単語単位の
折り返し、上付き・下付き、OMMLの分数・根号・添字、多階層のリスト、脚注・文末脚注・コメント、
変更履歴の最終版表示、DrawingMLとVMLのテキストボックス、段落途中の改ページ、表、画像、
セクションごとの先頭・偶数・既定のヘッダーとフッター、PAGEフィールド

**drawio** — 複数ページ（`<diagram>`）、圧縮されたページ本体（URIエンコード＋raw
deflate＋base64）の展開、基本図形とフローチャート図形、グループの入れ子と座標、塗り・線・
破線・グラデーション・影・不透明度、`direction`と`rotation`と反転、HTMLラベル（`<br>`・
太字・斜体・`<font>`）と折り返し、直交・直線・曲線のコネクタ、固定接続点（`exitX`/`entryX`）と
経由点、mxGraph準拠の矢尻、スイムレーン、背景色

</details>

### やらないこと

- **パスワードは破りません。** ユーザーパスワードが必要なPDFとOffice文書は、その旨を返して止まります。一方、ユーザーパスワードが空の暗号化PDF（印刷禁止などの権限フラグだけを持ついわゆるオーナーパスワード形式）は、他のビューアと同じように開きます。権限フラグは宣言であってアクセス制御ではなく、`docsvg`はこれを強制しません。この扱いで良いかは変換前に判断してください
- **Officeの意味構造は復元しません。** 逆変換はSVGを画像として包み直すだけ
- **drawioのシェイプライブラリは、ステンシルを渡せば描きます。** `--stencils`にdrawioのステンシルファイル（またはそのディレクトリ）を渡すと、AWS・Azure・GCP・フロアプランなどのアイコンを本物どおり描きます。渡さない場合はラベル付きの矩形に置き換えて警告に出します。drawioがJavaScriptで実装しているシェイプは、ステンシルを渡しても描けないものがあります。ベンダーアイコン自体はライセンスと容量の都合で同梱しません
- 経由点のないコネクタの経路は、drawio独自のルーターの近似です
- **変換コアはローカルで動きます。** 入力文書を外部サービスへ送信しません。公式アイコン取得やライセンス監査は、必要な公開情報を取得する別ツールです

## 安全のための境界

信頼できない文書を受け取る前提で書かれています。

- 入力サイズ、ZIP展開後のサイズ、XMLイベント数、ページ数、PDFの展開ストリームに上限があります
- ZIPのパス、埋め込みリソースの参照先を検査します
- 逆変換は、スクリプト・`file://`・外部URL・`javascript:`・イベントハンドラ属性・`foreignObject`・
  外部実体参照を含むSVGを拒否します
- 各ページは一時ファイルに書いてから名前を変えます。途中で失敗したページが、完成したファイルとして残りません
- 同じ入力からは常に同じバイト列が出ます。並列度を変えても変わりません

## 構成図を描く（変換とは別の機能）

Azure・AWS・Google Cloudの公式アイコンを使って、構成図をSVGで組み立てるツールも入っています。
変換コアとは独立していて、Rustの依存も生成AIのAPIキーも要りません。

```bash
python3 authoring/cloud_icons.py fetch                    # 公式アイコンを取得
python3 authoring/cloud_icons.py search "app service"     # IDを調べる
python3 authoring/cloud_icons.py build-examples --output out/arch
```

使い方は[クラウド構成図ガイド](docs/CLOUD_ARCHITECTURE.md)、業務資料向けのテンプレートと
直角配線は[3社の構成図テンプレート](docs/BUSINESS_ARCHITECTURE.md)にまとめています。
PNGを書き出すには`rsvg-convert`が要ります。

## エディタとAIエージェントから使う

Codex、Claude Code、GitHub Copilot、GitHub CLI、GitHub Actionsから呼べます。配線の全体は
[GitHub連携ガイド](docs/GITHUB_INTEGRATION.md)、導入条件と依頼例は
[導入ガイド](docs/PLUGIN_INSTALLATION.md)にまとめました。

| 使う場所 | やること |
|---|---|
| Codex | `./scripts/install-codex-plugin.sh` を実行して `$document-svg` を呼ぶ |
| Claude Code | `/plugin marketplace add ryusui-hiro/document-svg` のあと `/plugin install document-svg@document-svg` |
| GitHub Copilot | `.github/copilot-instructions.md` を読みます。別のリポジトリなら `copilot-setup-steps.yml` に下のsetupアクションを足すとCLIが使えます |
| GitHub CLI | `./scripts/install-gh-extension.sh` を実行して `gh docsvg convert report.pdf --output preview/report` |
| GitHub Actions | `uses: ryusui-hiro/document-svg/.github/actions/setup-docsvg@main` |

```bash
./scripts/install-codex-plugin.sh    # Codex
./scripts/install-claude-plugin.sh   # Claude Code（再起動が要ります）
./scripts/install-gh-extension.sh    # gh docsvg
```

インストーラはcargoがあればこのチェックアウトからビルドし、なければ配布済みのリリースを
SHA256で照合してから置きます。Rustツールチェーンは要りません。

`gh extension install ryusui-hiro/document-svg` は動きません。GitHub CLIは拡張のリポジトリ名が
`gh-` で始まることを求めるので、リモートから一発で入れるには別リポジトリが要ります。上の
インストーラがこのリポジトリでの入口です。

導入後は、たとえばこう頼めます。

```text
$document-svg を使って report.docx をページ別SVGへ変換し、警告も確認して
$document-svg を使って page-*.svg を slides.pptx に戻して
```

Claude Codeにはコマンドも入ります。`/document-svg:convert`、`/document-svg:reverse`、
`/document-svg:preview`、`/document-svg:setup` の4つ。

プラグインはSVGへのリンクに加えて、librsvgで描いたPNGも出します。エディタやOSでSVGの
扱いが違うため、画像なら確実に見えます。

```bash
plugins/document-svg/skills/document-svg/scripts/render-preview.sh \
  out/slides out/slides-preview 1400
```

リポジトリを開いただけでは何もインストールしません。利用者が頼んだときだけ上のスクリプトが走ります。

### プルリクエストで文書の差分を見る

PDFやOffice文書を置いているリポジトリに、これを足します。

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

変更された文書がSVGページに変換され、実行の成果物として添付され、ページ数と警告数の表が
プルリクエストにコメントされます。警告が1件でも出たら、その変換は元の見た目を保証しません。

## 開発

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release

(cd bindings/node && npm run build && npm test)
python3 -m pip wheel --no-deps --wheel-dir dist ./bindings/python
```

テストは5形式の最小ファイルと機能別ファイルをテスト内で生成し、公開APIからSVGまで通します。
サンプルを作り直すときは`python3 scripts/make_samples.py`を実行してください。

設計は[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)、対応範囲は[docs/SUPPORT.md](docs/SUPPORT.md)、
実測値は[docs/BENCHMARKS.md](docs/BENCHMARKS.md)にあります。

## ライセンス

`MIT OR Apache-2.0`。どちらかを選んで使えます。再配布するときは、選んだライセンスの
著作権表示とライセンス文を残してください（[LICENSE](LICENSE)）。
依存パッケージは直接・推移とも許容的なライセンスだけを選び、監査手順を
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)に記録しています。
現在の確認結果は[依存ライセンス監査](docs/LICENSE_AUDIT.md)、同梱する通知全文は
[THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt)を参照してください。

`samples/`のファイルは`scripts/make_samples.py`が生成したもので、このリポジトリと同じライセンスです。
