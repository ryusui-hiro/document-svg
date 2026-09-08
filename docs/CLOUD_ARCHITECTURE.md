# 公式クラウドアイコンによる構成図作成

`authoring/cloud_icons.py` は、生成AIが公式アイコンを検索・参照してSVG構成図を作るための
独立ツールです。Python 3.10以降の標準ライブラリだけで動作し、Rustの変換コア、各parser、
Node/Python bindingには依存しません。生成AIへのAPI呼び出しは実装しておらず、Codex等の
呼び出し元が図の構造をJSONとして作成します。現在はリポジトリのcheckoutから利用します。

業務向けの構成図には **[業務向けアーキテクチャ図ガイド](BUSINESS_ARCHITECTURE.md)** の
`schema_version: 2` と3社のテンプレートを使ってください。入れ子の境界、障害物を避ける
直角配線、通信種別、角丸切り替え、説明欄、出典、幾何検証を追加しています。
以下の `schema_version: 1` は引き続き簡易グリッド図として利用できます。

## 取得済みの素材

2026-09-05に公式配布ページから取得・確認しました。

| 配布元 | 配布内容 | ZIP内SVG数 | 検索ID数 |
|---|---|---:|---:|
| [Azure](https://learn.microsoft.com/en-us/azure/architecture/icons/) | V24 | 714 | 639 |
| [AWS](https://aws.amazon.com/architecture/icons/) | 07312026 | 1,852 | 857 |
| [Google Cloud](https://cloud.google.com/icons) | Core products | 19 | 19 |
| [Google Cloud](https://cloud.google.com/icons) | Product categories | 26 | 26 |
| 合計 | | 2,611 | 1,541 |

同一Azure IDのカテゴリ違いやAWSのサイズ違いをまとめ、AWSは最大サイズの公式SVGを
既定候補とします。全variantの出典・ハッシュ・カテゴリは `variants` に残します。
GCPのカテゴリ共通アイコンは `kind: category` として製品固有の `service` と区別します。
GCPの配布ZIPと[公式解説PDF](https://services.google.com/fh/files/misc/google-cloud-product-icons.pdf)
の名称・収録範囲が必ず一致するとは限りません。カタログの名前は取得ZIPのフォルダ名由来で、
最新のサービス名への自動読み替えや未収録製品のアイコン補作はしません。

## 使い方

以下はリポジトリルートで実行します。

```bash
# 初回取得。2回目以降は同じハッシュのキャッシュを使用
python3 authoring/cloud_icons.py fetch

# 生成AI向けの検索結果はJSON。providerを限定できる
python3 authoring/cloud_icons.py search 'azure functions' --provider azure
python3 authoring/cloud_icons.py search 'lambda' --provider aws
python3 authoring/cloud_icons.py search 'cloud run' --provider gcp

# 完全一致IDから出典とローカルパスを取得。--embedでdata URIも返す
python3 authoring/cloud_icons.py resolve gcp/service/cloud-run --embed

# 3社の構成例からSVGを生成。出力済みファイルは上書きしない
python3 authoring/cloud_icons.py render examples/cloud-architecture.json \
  --output output/cloud/architecture.svg

# 検索・IDコピー・原本SVGダウンロードが可能なHTMLを生成
python3 authoring/cloud_icons.py gallery --output output/cloud/catalog.html
```

HTML一覧は自己完結しており、そのままブラウザで開けます。Copy IDはClipboard APIを使うため、
ブラウザやfile URLの制限で利用できない場合は、表示されたIDを選択してコピーしてください。
一覧の検索は英語名・provider・IDの部分一致、CLIはそれに加えて `aks`、`gce`、`azure functions`、
`ストレージ` など少数の明示した別名を扱います。全サービスの日本語同義語検索ではありません。

保存先は `.cache/cloud-icons/` です。変更するときは各コマンドの**前**に
`--cache /absolute/path` を付けます。カタログを生成する前は `search` 等はエラーになります。

```text
.cache/cloud-icons/
├── archives/<zip-sha256>.zip   原本ZIP（同梱情報も保持）
├── assets/<svg-sha256>.svg     改変しないSVG原本
└── catalog.json               検索ID、種別、出典、variant、hash
```

オフライン取得では、4ファイルを `azure.zip`、`aws.zip`、`gcp-core.zip`、`gcp-category.zip`
として保存したディレクトリを `fetch --archive-dir DIR` に渡します。オンラインと同じハッシュ検証を
行い、任意のZIPを公式素材として登録する経路にはしません。

## 生成AIが作る図のJSON

最小例:

```json
{
  "schema_version": 1,
  "title": "API processing",
  "nodes": [
    {"id": "api", "icon": "aws/service/amazon-api-gateway", "row": 0, "column": 0, "label": "Amazon API Gateway"},
    {"id": "worker", "icon": "aws/service/aws-lambda", "row": 0, "column": 1, "label": "AWS Lambda"}
  ],
  "edges": [{"from": "api", "to": "worker", "label": "HTTPS"}]
}
```

| 項目 | 契約 |
|---|---|
| `schema_version` | 必須、`1` |
| `title` | 必須、1〜100文字 |
| `nodes` | 必須、1〜64件 |
| node `id` | 英字始まりの英数字・`_`・`-`、最大64文字、図内で一意 |
| node `icon` | `search` が返した実在する完全一致ID |
| node `row`, `column` | 必須、整数。rowは0〜15、columnは0〜7。同じcellに2つ置けない |
| node `label` | 任意、既定はアイコン名。1〜100文字、表示幅26単位×2行以内（全角は2単位） |
| `edges` | 任意、最大128件。from/toは既存の異なるnode ID |
| edge `label` | 任意、24文字かつ表示幅12単位以内 |

未知のフィールド、存在しないID、重複ID、重なり、長すぎるラベルは拒否します。
接続は同じ行か同じ列のノード間だけを描画し、他ノードを通過する接続も拒否します。
この簡易v1形式では、自動経路探索や入れ子のVPC/subnet境界を扱いません。
これらを使う場合は業務向けv2形式を使用してください。
複雑な図では `resolve --embed` で素材を得て、別のレイアウト層で配置できます。

呼び出し元のAIは、次の順序で作業します。

1. 利用者の要件から必要な製品と接続を整理する。サービス同士の接続可否、認証、ネットワークは必要に応じて公式資料で確認する。
2. `search` で候補を絞り、provider・kind・製品名・出典を確認してIDを選ぶ。検索結果がなければその不足を明示する。
3. 上記契約のJSONを作成し、`render` を実行する。アイコンのpathを手描き・再着色しない。
4. 出力JSONの `warnings` とSVGの `metadata` を確認する。カテゴリ共通アイコンの警告は、公式の製品カテゴリ対応表で確認する。
5. SVGをPNGやブラウザで描画してラベル・接続・縦横比・欠落を確認する。

`render` は同じJSON・catalog・原本に対して同じSVG文字列を出力します。生成日時は埋め込みません。
製品の接続可否や構成の安全性を検証するエンジンではありません。サンプルも描画機能の例で、
3社それぞれの独立した論理フローを表し、本番構成の認証・冗長化・ネットワークを省略しています。

## SVGの保持精度と既存コアへの接続

公式SVGをbase64の `<image>` として埋め込み、`preserveAspectRatio="xMidYMid meet"` で
縦横比を維持します。アイコン内部の同名ID、gradient、CSSを別ドキュメントとして隔離し、
色の干渉を防ぎます。形・色・viewBox・原本bytesは変更しません。原本のchecksum不一致や
外部参照・実行可能要素・DTD等は拒否します。取込時に拒否されたアイコンはsourceごとの
`rejected` に記録され、存在しないアイコンへ自動代替しません。

生成したSVGは既存の `reverse` へ入力できます。

```bash
cargo run --bin docsvg -- reverse output/cloud/architecture.svg \
  --output output/cloud/architecture.pptx
```

OfficeにはSVG画像とPNG互換プレビューが入ります。ノードや接続線をOfficeの編集可能な図形に
復元する機能ではなく、Officeの意味構造は再構築されません。

## 更新と利用条件

[取得元定義](../authoring/icon-sources.json) は公式ページ、確認日、ZIP URL、SHA-256を固定しています。
更新時は公式ページの配布物・利用条件を確認し、新しいZIPを取得してハッシュ・release・確認日を
更新してから `fetch` を実行します。配布先で内容が差し替わった場合はchecksumエラーで止まります。
更新前のカタログを保存するか、別の `--cache` に取得すれば既存図の再現性を保てます。

アイコンは各社の権利物であり、リポジトリのMITライセンスは適用されません。
[第三者素材の扱い](../THIRD_PARTY_NOTICES.md)と配布元の条件に従って構成図・文書に利用します。
原本と一覧はローカル保存し、パッケージへの自動同梱はしません。

## 不具合修正と検証

SVG writerの `--precision 0` で整数末尾の0も削られ、幅100が1になる不具合を修正しました。
Rust公開APIからの過大な小数精度も12に制限します。既存の変換精度全般が完全になったという意味ではありません。

```bash
cargo test --all-targets
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
python3 -m unittest discover -s tests -p 'test_cloud_icons.py'
```

2026-09-05の実行結果: Rust 110件、Python 13件が成功し、fmt/clippyも通過しました。
取得した検索ID 1,541件はすべて `rsvg-convert` でPNG描画が成功しています。
9ノード・6接続のサンプルは再生成時にSVGが一致し、PPTX内のSVG bytesも入力と一致しました。
サンプルPNGとOffice互換PNGを目視確認しています。HTML一覧はブラウザで3社の検索とIDコピーを
操作確認し、1440・1280・390pxの画面幅で横方向のはみ出しがないことを計測しました。
今回のローカル証跡は `outputs/cloud-architecture-20260905/verification.json` に保存しています。
