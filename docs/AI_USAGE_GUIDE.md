# SVG変換・モジュール利用ガイド

この文書は、人間と生成AIのどちらでも「どのAPIを選び、何を入力し、何が出力され、
結果をどう判定するか」を迷わず実装できるように、`document-svg`の利用契約を明示した
ものです。

## 30秒で分かる変換の仕組み

```text
入力ファイル
  ├─ .pdf  ── PDF parser
  ├─ .pptx ─┐
  ├─ .xlsx ─┼─ OOXML ZIP/XML parser
  └─ .docx ─┘
          ↓
共通 Page IR（Path / Text / Image / Group）
          ↓
SVG writer
          ↓
page-0001.svg、page-0002.svg、...、conversion.json

SVGファイルまたはSVGディレクトリ
          ↓
SVG image partとしてOOXMLへ格納
          ↓
PPTX（1 slide/ SVG）、DOCX（1 page/SVG）、XLSX（1 sheet/SVG）
```

重要な事実は次のとおりです。

- 変換元の形式はファイル拡張子で判定します。対応拡張子は`.pdf`、`.pptx`、
  `.xlsx`、`.docx`です。
- 文書全体を1個のSVGにするのではなく、ページ、スライド、またはシートの出力ページ
  ごとにSVGを1個生成します。
- 通常の利用者は`convert_path`だけを呼びます。PDF/Officeのparserを選ぶ処理は
  `convert_path`が行います。
- `conversion.json`は付属情報ではなく、警告を含む変換結果の一部です。
- SVGの座標と寸法はポイント単位です。ページ左上が原点で、右が+x、下が+yです。

## APIの選び方

Azure・AWS・GCPの公式アイコンを用いる新規アーキテクチャ図は、
[`CLOUD_ARCHITECTURE.md`](CLOUD_ARCHITECTURE.md) の独立した `authoring/cloud_icons.py` を使ってください。
AIがカタログを検索して実在するIDを選び、図のJSONを作り、決定的レンダラーでSVGを生成します。
この経路はPDF/OOXMLの変換parserやRust crateへネットワーク・AI依存を追加しません。
業務資料向けの図では [`BUSINESS_ARCHITECTURE.md`](BUSINESS_ARCHITECTURE.md) を読み、
`templates` → `template` → 編集 → `validate` → `render` の順でv2形式を利用します。

次の規則で選んでください。

1. ファイルをそのまま変換するだけなら、CLIまたは使用言語の高レベル`convert`を使う。
2. Rustアプリから変換するなら、crate rootの`convert_path`を使う。
3. 自作の図形、文字、画像からSVGを組み立てるなら、`ir::Page`を作り、
   `svg::write_page`へ渡す。
4. `pdf`、`ooxml`、`convert`モジュールは内部実装なので、外部crateから直接呼ばない。
5. PDF/PPTXなどを一度`Page`として受け取って独自加工する公開APIは、現時点ではない。
   必要なら内部converterと`PageConsumer`を公開・設計し直す必要がある。
6. SVGをOfficeへ戻すだけなら、CLIの`reverse`または`svg_to_openxml`を使う。この処理は
   見た目をベクター画像として保持するもので、Officeの意味構造を復元しない。

## CLIで変換する

リポジトリのルートでビルドし、入力ファイルと専用の出力ディレクトリを指定します。

```bash
cargo build --release
target/release/docsvg input.pdf --output output/pdf
```

PDFを4ページずつ並列処理する例です。

```bash
target/release/docsvg input.pdf --output output/pdf --jobs 4
```

`--jobs`によるページ並列化が有効なのはPDFです。Office文書はページ単位で逐次処理
されます。既定の`--jobs 1`は常駐メモリを抑える設定です。

コマンドが成功しても警告があり得るため、後述の`conversion.json`も確認してください。
出力先は変換ごとに空の専用ディレクトリを使うことを推奨します。converterは既存の
無関係なファイルや、以前の変換で生成された余分な`page-*.svg`を削除しません。

SVGからOpen XMLへ戻す例:

```bash
target/release/docsvg reverse output/pdf --output repacked.pptx
```

出力拡張子は`.pptx`、`.docx`、`.xlsx`に対応します。既存出力は上書きしません。
返される警告どおり、段落、セル、数式、グラフ等は復元されません。

## Rustから変換する

`Cargo.toml`からこのリポジトリをpath dependencyとして参照する例です。

```toml
[dependencies]
document-svg = { path = "../document-svg" }
```

通常は既定値から必要な項目だけ変更します。

```rust
use document_svg::{ConvertOptions, Error, convert_path};

fn main() -> Result<(), Error> {
    let options = ConvertOptions {
        jobs: 4,
        max_pages: 500,
        ..ConvertOptions::default()
    };

    let report = convert_path("input.pdf", "output/pdf", &options)?;

    println!("{}ページを変換", report.page_count);
    if !report.warnings.is_empty()
        || report.pages.iter().any(|page| !page.warnings.is_empty())
    {
        eprintln!("警告あり: conversion.jsonと各ページのwarningsを要確認");
    }
    Ok(())
}
```

`convert_path`は出力ディレクトリを必要なら作成し、各SVGと`conversion.json`を
書き込み、同じ内容を`ConversionReport`として返します。エラー時には`Error`を返す
ため、`unwrap()`で無視せず、呼び出し元のエラー処理へ伝播させてください。

### `ConvertOptions`の意味

| フィールド | 既定値 | 意味 |
|---|---:|---|
| `max_input_bytes` | 512 MiB | 入力ファイル全体の最大サイズ |
| `max_zip_entry_bytes` | 128 MiB | OOXMLの展開済みpartまたはPDFページstreamの上限 |
| `max_pages` | 10,000 | 出力できる最大ページ数 |
| `max_xml_events` | 5,000,000 | XML解析イベント数の安全上限 |
| `include_metadata` | `true` | SVG内へ由来情報と警告metadataを含めるか |
| `precision` | 5 | SVG数値の小数精度 |
| `jobs` | 1 | PDFページworker数。0は無効 |
| `outline_embedded_pdf_text` | `false` | PDF埋め込みfontをpath化して見た目を優先。文字編集性を失い、権利確認warningが出る |

これらは品質調整だけでなく、信頼できない入力による過大なメモリ・CPU使用を抑える
安全上限です。生成AIは「変換を成功させるため」という理由だけで無制限に増やしては
いけません。

## Pythonから変換する

wheelを作成してインストールした後、`convert`を呼びます。

```python
from document_svg import convert

report = convert(
    "input.pptx",
    "output/pptx",
    jobs=1,
    max_pages=500,
)

needs_review = bool(report["warnings"]) or any(
    page["warnings"] for page in report["pages"]
)
print(report["page_count"], needs_review)
```

Pythonではレポートのキーとオプション名が`snake_case`です。変換中はGILを解放します。
詳細なビルド方法は[`../bindings/python/README.md`](../bindings/python/README.md)を
参照してください。

## Node.jsから変換する

```javascript
const { convert } = require("document-svg")

async function run() {
  const report = await convert("input.docx", "output/docx", {
    jobs: 1,
    maxPages: 500,
  })

  const needsReview =
    report.warnings.length > 0 ||
    report.pages.some((page) => page.warnings.length > 0)

  console.log(report.pageCount, needsReview)
}

run().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
```

Node.jsではフィールドが`camelCase`です。`convert`は`Promise`を返し、変換はlibuvの
worker上で行われます。ブラウザ用APIではなく、ネイティブNode.js用APIです。

### TypeScriptで画面プレビューする

ファイルを保存する通常の`convert()`とは別に、`preview()`は各ページの完全なSVG文字列を
返します。PDFだけでなく、PPTX、XLSX、DOCXにも同じAPIを使用できます。

```typescript
import { preview } from "document-svg"

const report = await preview("input.xlsx", {
  maxPages: 100,
  maxSvgBytes: 64 * 1024 * 1024,
  maxTotalSvgBytes: 256 * 1024 * 1024,
})

for (const page of report.pages) {
  console.log(page.number, page.widthPoints, page.heightPoints)
  // page.svgが完全なSVG XML文字列
}
console.log("review required:", report.needsReview)
```

Rust側では専用の一時ディレクトリへ変換してSVGを読み込み、`Promise`の完了前に一時
ファイルを削除します。既定のメモリ上限は1ページ64 MiB、全ページ合計256 MiBです。
`maxSvgBytes`と`maxTotalSvgBytes`でより小さい上限を設定できます。

Reactなどへ渡すときは、browser-safeな`document-svg/preview-ui`を使用できます。この
subpathはネイティブRust bindingを読み込まないため、renderer bundleへ含められます。

```typescript
import {
  copySvgSourceToClipboard,
  copySvgToClipboard,
  createSvgPreviewUrl,
  revokeSvgPreviewUrl,
} from "document-svg/preview-ui"

const svg = report.pages[0].svg
const url = createSvgPreviewUrl(svg)
// JSX: <img src={url} alt="Office document preview" />
// component破棄時: revokeSvgPreviewUrl(url)

// buttonのonClick等から呼ぶ
const copiedMimeType = await copySvgToClipboard(svg)

// 常にXMLソースとしてコピーする場合
await copySvgSourceToClipboard(svg)
```

`copySvgToClipboard()`は`image/svg+xml`のClipboard書き込みにブラウザが明示対応していれば
SVG MIMEとしてコピーし、未対応なら完全なXMLを`text/plain`でコピーします。返り値は
実際にコピーしたMIME typeです。Clipboard APIはsecure contextとユーザー操作を要求する
ため、ページ読み込み時に自動実行せず、必ずコピーボタン等から呼びます。
Blob URLの解放とアクセシブルな結果表示まで含むReact component例は
[`../bindings/node/examples/SvgPreview.tsx`](../bindings/node/examples/SvgPreview.tsx)です。

`preview()`自体が動く場所はNode.js、Electronのmain process、またはサーバーです。
ブラウザUIはIPCまたはHTTP API経由でSVG文字列を受け取ります。現在のAPIはブラウザ内で
直接ネイティブRustを実行するWASM APIではありません。

## Rustモジュールの役割

| モジュール／公開項目 | 公開 | 役割 | 通常利用者が使うか |
|---|:---:|---|:---:|
| crate rootの`convert_path` | はい | 入力判定からSVG・report出力までを一括実行 | はい |
| crate rootの`ConvertOptions` | はい | 安全上限、metadata、精度、並列数の設定 | はい |
| crate rootの`ConversionReport` / `PageReport` | はい | 変換結果と警告の確認 | はい |
| crate rootの`Error` / `Result` | はい | 失敗理由の伝播・分類 | はい |
| `ir` | はい | 出力形式に依存しないページ、図形、文字、画像のモデル | 独自描画時 |
| `svg` | はい | `Page` IRをSVG XMLへ直列化 | 独自描画時 |
| `convert` | いいえ | 形式選択、ページsink、manifest作成 | 直接使わない |
| `pdf` | いいえ | PDFを共通IRへ変換 | 直接使わない |
| `ooxml::pptx/xlsx/docx` | いいえ | Office ZIP/XMLを共通IRへ変換 | 直接使わない |
| `ooxml::package/xml/chart` | いいえ | relationship、XML、chartの内部共通処理 | 直接使わない |

Rustの`mod`が存在することと、外部利用者がそのモジュールを呼べることは別です。
`lib.rs`で`pub mod`または`pub use`されている項目だけが公開APIです。

## `ir`と`svg`で独自SVGを生成する

低レベル利用では、`Page`へ`Node`を描画順に追加し、`write_page`へ渡します。
実行可能な完全例は[`../examples/custom_ir.rs`](../examples/custom_ir.rs)です。

```bash
cargo run --example custom_ir -- output/custom.svg
```

主なIR型は次のとおりです。

- `Page`: 1枚のSVGページ。幅、高さ、node、clip、mask、pattern、警告を保持する。
- `Node::Path`: SVG path相当。図形や線を表す。
- `Node::Text`: 編集可能な文字列と複数の`TextRun`を表す。
- `Node::Image`: 通常はdata URLを`href`に保持する画像を表す。
- `Node::Group`: 複数nodeへ共通の変換、透明度、clipを適用する。
- `Paint`: なし、単色、線形・放射gradient、pattern参照を表す。
- `Stroke`: 線色、太さ、端、結合、dashを表す。
- `SourceMeta`: 元要素ID、意味役割、代替テキストなどの由来情報を表す。
- `ClipPath` / `MaskDefinition` / `TilingPatternDefinition`: SVGの`defs`に相当する。

`Page.nodes`の順序が描画順です。後から追加したnodeほど手前に描かれます。

### 座標変換

`Matrix`はSVGと同じ`[a, b, c, d, e, f]`で、点`(x, y)`を次のように変換します。

```text
x' = a*x + c*y + e
y' = b*x + d*y + f
```

変換なしには`ir::IDENTITY`を使います。複数の行列を合成するときは`ir::compose`、
点だけを変換するときは`ir::transform_point`を使います。

`svg::write_page`は任意の`std::io::Write`へ書けるため、`File`、`BufWriter`、
`Vec<u8>`などを渡せます。大量出力では`BufWriter`を推奨します。

## 出力と成功判定

代表的な出力は次の構成です。

```text
output/pdf/
├── page-0001.svg
├── page-0002.svg
└── conversion.json
```

成功判定を「関数がエラーを返さなかった」だけにしないでください。推奨判定は次です。

```text
関数がErrorを返した             → failed
文書またはページにwarningがある → needs_review
warningがない                   → converted
```

`ConversionReport.warnings`は文書全体、`PageReport.warnings`はページ固有の警告です。
未対応機能を安全に近似・省略した場合があるため、警告ありの結果を「完全再現」とは
扱わないでください。`svg`は出力ディレクトリからの相対ファイル名です。

## 生成AIが実装するときの契約

生成AIへの依頼やagent実装では、次の情報を明示すると安定します。

```text
目的: PDF/PPTX/XLSX/DOCXをページ単位のSVGへ変換する
推奨入口: document_svg::convert_path（言語bindingではconvert）
入力判定: 拡張子
出力: page-NNNN.svg + conversion.json
品質判定: document warningとpage warningの両方を見る
禁止: 内部pdf/ooxml parserの直接呼び出し、警告の黙殺、安全上限の無制限化
運用: 変換ごとに専用の空ディレクトリを使う
```

実装手順は次の順序に固定できます。

1. 入力拡張子が対応形式か確認する。
2. 入力ごとに専用出力ディレクトリを決める。
3. 既定の安全上限を基準に`ConvertOptions`を作る。
4. 高レベル変換APIを1回呼ぶ。
5. エラーなら処理を失敗として返す。
6. 文書警告と全ページ警告を集約する。
7. 警告があれば`needs_review`、なければ`converted`とする。
8. 利用者へSVG一覧と`conversion.json`の場所を返す。

## よくある誤解

- 「PPTXをSVGへ変換」は、通常、スライドごとにSVGを出すという意味です。
- `include_metadata = false`は図形を消す設定ではありません。SVG内の由来metadataを
  省略する設定です。
- `precision`を上げても、入力に存在しない情報や未対応機能は復元できません。
- `jobs`を増やせば必ず速くなるわけではなく、PDF処理のメモリ使用量も増えます。
- `conversion.json`だけを読んでもSVG本体ではありません。逆にSVGだけを見ても警告を
  見落とします。両方を一組として扱ってください。
- このネイティブ配布はブラウザWASMではありません。ブラウザから直接ファイルを
  変換するには、Node.js等のサーバー側APIを用意する必要があります。

内部設計や新しい入力形式・writerの追加方法は
[`ARCHITECTURE.md`](ARCHITECTURE.md)、形式ごとの対応範囲は
[`SUPPORT.md`](SUPPORT.md)を参照してください。
