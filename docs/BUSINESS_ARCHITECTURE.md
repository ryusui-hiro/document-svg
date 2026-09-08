# 業務向けアーキテクチャ図

公式アイコンと、参照資料に基づく構成例を使い、提案・設計レビュー・説明資料へ配置できる
SVGを生成します。変換コアとは独立した `authoring/architecture.py` が描画と幾何検証を担い、
従来の `authoring/cloud_icons.py` から呼び出せます。AIがサービス構成と配置をJSONで作り、
レンダラーが接続を直角に配線します。サービスの自動選定やクラウド構築は行いません。

## すぐ使う

リポジトリルートで実行します。Python 3.10以降、既存のアイコンカタログが必要です。

```bash
# カタログがない場合のみ取得
python3 authoring/cloud_icons.py fetch

# 3社のSVG・2400px PNG・JSON・検証結果・確認画面をまとめて作成
python3 authoring/cloud_icons.py build-examples --output output/business-architecture

# テンプレートを選んで変更する
python3 authoring/cloud_icons.py templates
python3 authoring/cloud_icons.py template azure-private-web --output output/my-architecture.json

# JSON編集後に検証し、SVGとレポートを生成
python3 authoring/cloud_icons.py validate output/my-architecture.json
python3 authoring/cloud_icons.py render output/my-architecture.json \
  --output output/my-architecture.svg --report output/my-architecture.report.json
```

`build-examples` のPNG生成には `rsvg-convert` を利用します。`--png-width 3200` などで
1200〜4800pxの出力幅を選べます。SVGのみの `render` は外部描画CLIを要求しません。
既存の出力ファイル・bundleディレクトリは上書きしません。

生成する `index.html` はローカルで開けます。Azure / AWS / Google Cloudのタブと
「直角（標準） / 小さな角丸」の切り替えがあり、選択した図柄に対応するSVG・PNG・JSON・
レポートをダウンロードできます。図と必要な素材は自己完結し、外部サービスへ構成内容を送信しません。

## 参照資料とテンプレート

確認日: 2026-09-06。配布元の図の構成・境界・通信の考え方を参照し、独自のレイアウトで
再構成しています。参照図の画像を完成物へ貼り付ける方式ではありません。

| テンプレート | 内容 | 参照した公式資料 |
|---|---|---|
| `azure-private-web` | WAF付きApplication Gateway → Private Endpoint → App Service。送信はVNet統合経由でSQL・Key Vaultへ | [App Service baseline](https://learn.microsoft.com/en-us/azure/architecture/web-apps/app-service/architectures/baseline-zone-redundant) |
| `aws-multi-az-web` | CloudFront・WAF、2AZの単一ALB・EC2、RDS primary / standby、DNSと監視 | [Web application architecture](https://d1.awsstatic.com/architecture-diagrams/ArchitectureDiagrams/web-application-architecture-on-aws-ra.pdf)、[VPC private subnets](https://docs.aws.amazon.com/vpc/latest/userguide/vpc-example-private-subnets-nat.html)、[RDS Multi-AZ](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/Concepts.MultiAZSingleStandby.html) |
| `gcp-serverless-web` | Global HTTPS LB・Cloud Armor → Serverless NEG → Cloud Run、Direct VPC egress → private IPのCloud SQL HA | [Global serverless LB](https://docs.cloud.google.com/load-balancing/docs/https/setup-global-ext-https-serverless)、[Cloud Run → Cloud SQL](https://docs.cloud.google.com/sql/docs/postgres/connect-run)、[Cloud SQL HA](https://docs.cloud.google.com/sql/docs/postgres/high-availability)、[Cloud Run ingress](https://docs.cloud.google.com/run/docs/securing/ingress) |

AzureのApp Serviceをサブネット内のVMとして描かず、Private Endpointと送信のVNet統合を
別々に表します。AWSの2つのALB endpointは**同じALBのAZ別配置**です。DB standbyには
通常の読取通信を送りません。GCPはグローバルVPCとリージョン内サブネットを区別し、Cloud Runの
実行環境とVPC送信接続を分けます。Cloud SQLはサービス側ネットワークに置いています。

Azure例はベースラインを簡略化し、DNS・NSG等の一部を説明欄へ移しています。AWS例では
NAT等の送信経路を省略しています。いずれも図内に省略範囲と設定時の確認事項を記載しています。
使用するSKU、リージョン対応、可用性、バックアップ、IAM、認証、経路、容量は実案件でレビューします。

GCPのLB・NEG・Cloud Armor・監視には、現行の公式**カテゴリ共通アイコン**を使用します。
製品名を明記し、レポートにもカテゴリ確認の警告を残します。これらを製品固有アイコンと主張しません。

## 図の構造: schema_version 2

`authoring/templates/*.json` が完全な編集例です。既存のv1形式と互換に共存します。

| 要素 | 主なフィールドと制約 |
|---|---|
| document | `schema_version: 2`、`provider: azure / aws / gcp / neutral`、`title`、`groups`、`nodes`、`edges` は必須。`subtitle`、`revision`、`notes`、`sources`、`edge_style` は任意 |
| canvas | `width` 1200〜3200、`height` 800〜2400。既定1760×1120。上部タイトル、右側説明欄、下部凡例の領域を予約 |
| group | `id`, `label`, `kind`, `x`, `y`, `width`, `height`。任意の `parent` で入れ子。最大40件 |
| group kind | `cloud / region / vpc / subnet / zone / service / logical`。色・実線/破線・背景で境界を区別 |
| node | `id`, `label`, `x`, `y`。公式 `icon` か汎用 `symbol` のいずれか。`group`, `detail`, `badge`, `width`, `height` は任意。最大64件 |
| node size | 既定208×112。幅160〜360、高さ100〜190。ラベルは最大3行、detailは1行。配置間隔は16px以上 |
| symbol | `user / external / interface`。利用者や一般的な接続点であり、クラウド製品アイコンの代用品ではない |
| edge | `from`, `to`, `kind` 必須。`id`, `label`, `from_port`, `to_port`, `from_offset`, `to_offset`, `via` 任意。最大96件 |
| port | `n / e / s / w`。offsetは辺中央からの比率 −0.35〜0.35。辺に対して垂直に出入りする |
| via | 必要な場合だけ `[[x,y], ...]` を指定。最大12点。障害物上や180度の折り返しはエラー |
| notes | `title`, `body` の配列。最大5件。説明欄に収まらない文章はエラー |
| sources | `title`, `url` の配列。最大5件、URLはHTTPS。SVG metadataとレポートに保持し、確認画面から参照可能 |

groupの座標もnodeの座標も、**ページ全体の絶対座標**です。親を指定しても座標を加算しません。
描画領域は `x=40..(canvas.width−370)`、`y=160..(canvas.height−150)`。
親group内では左右12px、上42px、下12pxを予約します。nodeを親内に収め、関係のないgroupへ
重ねないでください。サービス配置を自動計算する機能ではないため、既存テンプレートを編集すると扱いやすくなります。

### 通信種別

| kind | 表示 | 意味 |
|---|---|---|
| `request` | 青・実線 | 利用者リクエストやサービス呼出し |
| `private` | 青緑・実線 | プライベート接続 |
| `replication` | 紫・破線 | 複製・同期 |
| `telemetry` | 灰・点線 | 監視・ログ |
| `control` | 黄土・破線 | DNS、ポリシー、設定など |

色だけに依存せず、線種・矢印・ラベル・凡例で意味を示します。描画上の線の種類は実際の
プロトコルを保証するものではなく、JSON作成者が参照資料に基づいて指定します。

## draw.io / drawiomoを参考にした配線

[draw.ioのコネクタスタイル](https://www.drawio.com/docs/manual/styles/connector-styles/) と
[style reference](https://www.drawio.com/docs/reference/diagram-generation/style-reference/) を確認しました。
指定されたローカル `drawiomo` では、以下の実装・fixtureを読んで配線の振る舞いを参照しています。
別リポジトリの変更や依存追加は行っていません。

- `src/canvas-graph/edgeStyle.ts`
- `src/canvas-graph/routeAesthetics.ts`
- `src/canvas-graph/orthogonalPathGeometry.ts`
- `src/canvas-graph/connectorPathGeometry.ts`
- `qa-external-edge-route-aliases.drawio`

このツールでは次の設定を使えます。

```json
"edge_style": {
  "corner": "sharp",
  "radius": 8,
  "width": 1.65,
  "jetty": 32
}
```

- 標準は `sharp` の直角配線。`rounded` は直線を保ったまま曲がり角だけ小さく丸めます。
- radiusは0〜12px、線幅は1〜3px、jettyは12〜64px。値の意味はこのツールの契約であり、draw.ioのstyle文字列互換ではありません。
- ノード・境界名を障害物とする直角グラフ上でA*探索を行い、折れ曲がりと既存配線との交差を減らします。
- 重複点と同じ方向の一直線上の点を省きます。180度の折り返しを消してごまかさず、必要なら配置・viaの修正を求めます。
- 向かい合う近接ポートは、利用できる距離に合わせてjettyを縮めます。端点余白が互いに重なって小さなループになることを防ぎます。
- 角丸でノードや見出しの領域へ食い込む場合は角丸を縮め、必要ならその角だけ直角を維持します。
- 矢印は線幅に依存しない9pxの大きさとし、先端をノードの接続位置に合わせます。
- 配線同士が交差した場合は白い下地で区別し、警告を出します。接続点のない交差は論理的な接続ではありません。

draw.ioファイルのimport/exportや、対話的なノード移動は今回の機能には含めません。

## 検証と成果物の扱い

`validate` と `render --report` は同じレンダラーを通り、次を確認します。

- 未知のフィールド、非有限数値、重複ID、不正な親、親子の循環。
- ノードと境界のはみ出し、ノードの重なり、他ノードを貫く線、境界名を貫く線。
- ラベルの長さと位置、配線や他ラベルとの衝突。
- 公式アイコンの存在・ハッシュ・provider整合性。
- 曲がり角数、経路長、遠回り比率、短い端点、配線同士の交差。

幾何条件に違反するとエラーになり、存在しないアイコンへ自動代替しません。
交差・カテゴリアイコン・参照資料の不足は `warnings` に残ります。
`quality` のゼロ値は**上記幾何条件**の検査結果です。アーキテクチャ全体が本番環境として
正しい、可用性やセキュリティが保証される、という意味ではありません。

```bash
python3 -m unittest discover -s tests -p 'test_*.py'
```

SVGは公式アイコンを原本のまま埋め込み、文字と接続はベクターで保持します。
同じ構成・カタログ・スタイルから同じSVGを出力します。PNGの文字描画はフォント環境に依存します。
ブラウザやPNGで全体を確認してから資料へ掲載してください。

既存の `docsvg reverse` にも入力できます。OfficeではSVG画像とPNG互換プレビューとして保持され、
ノード・接続をOfficeのネイティブ図形として再構築するものではありません。
各社アイコンの権利と利用条件は [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) を参照してください。
