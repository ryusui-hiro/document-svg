# document-svg

PDF・PowerPoint・Excel・Wordを、**1ページ＝1枚のSVG**に変換するRustのライブラリとCLIです。
逆に、SVGをPPTX・DOCX・XLSXへ包み直すこともできます。

出来上がったSVGは、対応するブラウザで開けます。画像はファイル内に埋め込みます。
通常の文字表示は環境のフォントに依存するため、別のPCでは字形や配置が変わる場合があります。
PDFの埋め込みフォントをアウトライン化するオプションもあります。

プレビューガイド: [日本語](bindings/node/docs/preview.ja.md) · [English](bindings/node/docs/preview.en.md) · [简体中文](bindings/node/docs/preview.zh-CN.md)

まず[サンプル](samples/)を見てください。4形式の入力ファイルと、そこから生成されたSVGが置いてあります。

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

`--jobs`はPDFだけに効きます。手元のMacで5.1MB・117ページのPDFを測ると、
1ワーカーで9.9秒、4ワーカーで3.9秒でした。Office形式は常に1ページずつ流すので、
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

</details>

### やらないこと

- **暗号化された文書は開きません。** パスワード付きのPDFとOffice文書は、その旨を返して止まります。アクセス制限を迂回する処理は入っていません
- **Officeの意味構造は復元しません。** 逆変換はSVGを画像として包み直すだけ
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

Codex、Claude Code、GitHub CLIから呼べます。導入条件と依頼例は
[導入ガイド](docs/PLUGIN_INSTALLATION.md)にまとめました。

```bash
./scripts/install-codex-plugin.sh    # Codex
./scripts/install-claude-plugin.sh   # Claude Code（再起動が要ります）
./scripts/install-gh-extension.sh    # gh docsvg
```

導入後は、たとえばこう頼めます。

```text
$document-svg を使って report.docx をページ別SVGへ変換し、警告も確認して
$document-svg を使って page-*.svg を slides.pptx に戻して
```

プラグインはSVGへのリンクに加えて、librsvgで描いたPNGも出します。エディタやOSでSVGの
扱いが違うため、画像なら確実に見えます。

```bash
plugins/document-svg/skills/document-svg/scripts/render-preview.sh \
  out/slides out/slides-preview 1400
```

リポジトリを開いただけでは何もインストールしません。利用者が頼んだときだけ上のスクリプトが走ります。

## 開発

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release

(cd bindings/node && npm run build && npm test)
python3 -m pip wheel --no-deps --wheel-dir dist ./bindings/python
```

テストは4形式の最小ファイルと機能別ファイルをテスト内で生成し、公開APIからSVGまで通します。
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
