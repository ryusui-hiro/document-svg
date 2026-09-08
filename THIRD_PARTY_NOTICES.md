# Third-party notices

## Optional cloud architecture assets

`authoring/cloud_icons.py` は、Azure・AWS・Google Cloudの公式アイコンを利用者のローカル
キャッシュへ取得する任意ツールです。素材とキャッシュ、デモ出力は配布に含めません。原本ZIPには配布元の同梱情報も保持します。

- Microsoft Azure: https://learn.microsoft.com/en-us/azure/architecture/icons/ 。構成図・研修資料・文書への利用が認められています。原形を維持し、製品名を添え、回転・反転・切り抜き・変形を行いません。
- AWS: https://aws.amazon.com/architecture/icons/ 。公式の構成図用素材を取得します。製品・リソース・カテゴリ・グループを区別します。
- Google Cloud: https://cloud.google.com/icons 。現行のcore product/category素材を取得します。個別製品と共通カテゴリの使い分けは公式Product icons overviewに従います。

取得元URL・確認日・リリース表記・ZIPのSHA-256は `authoring/icon-sources.json`、各SVGの
出典・原本内パス・SHA-256は生成する `catalog.json` に記録します。

## Runtime dependencies

このプロジェクトは、コア実行時依存をMIT、Apache-2.0、BSD-3-Clause、Zlib、IJGなどの許容的ライセンスへ限定します。GPL、AGPL、SSPL、非商用限定、ソース公開を要求する依存は採用していません。

直接依存:

| crate | resolved version | license |
|---|---:|---|
| anyhow | 1.0.104 | MIT OR Apache-2.0 |
| base64 | 0.23.1 | MIT OR Apache-2.0 |
| clap | 4.6.6 | MIT OR Apache-2.0 |
| emf-core | 0.1.0 | MIT |
| hayro-ccitt | 0.3.0 | Apache-2.0 OR MIT |
| jpeg-decoder | 0.3.2 | MIT OR Apache-2.0 |
| lopdf | 0.44.0 | MIT |
| png | 0.18.1 | MIT OR Apache-2.0 |
| quick-xml | 0.41.0 | MIT |
| rayon | 1.12.0 | MIT OR Apache-2.0 |
| resvg | 0.48.1 | Apache-2.0 OR MIT |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.151 | MIT OR Apache-2.0 |
| stet-fonts | 0.4.1 | Apache-2.0 OR MIT |
| ttf-parser | 0.25.1 | MIT OR Apache-2.0 |
| zip | 8.6.0 | MIT |

言語バインディング専用の直接依存:

| crate | resolved version | license |
|---|---:|---|
| napi | 3.12.2 | MIT |
| napi-derive | 3.6.3 | MIT |
| napi-build | 2.4.1 | MIT |
| pyo3 | 0.29.2 | MIT OR Apache-2.0 |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |

実行時依存`jpeg-encoder 0.6.1`は、PDF内のCMYK/YCCK JPEGをブラウザ互換RGB JPEGへ正規化するために使用し、ライセンスは(MIT OR Apache-2.0) AND IJGです。`tempfile`はコアのテストに加えてNode.js preview APIの一時出力管理にも使用します。

2026-08-23に次のコマンドで全推移依存のSPDX表現を確認し、必須のコピーレフト依存がないことを確認しました。複数ライセンスを`OR`で提示するcrateは、MIT、Apache-2.0または他の許容的選択肢を選択します。target固有推移依存`r-efi`の`MIT OR Apache-2.0 OR LGPL-2.1-or-later`からもMITを選択します。

```bash
cargo metadata --format-version 1 \
  | jq -r '.packages[] | [.name,.version,(.license // "MISSING")] | @tsv' \
  | sort -u
```

リリース前には`Cargo.lock`を基準に同じ監査を再実行し、各crateの配布物に含まれるLICENSE／NOTICEも保持してください。QAだけに使う`pdftoppm`、`rsvg-convert`、LibreOfficeは外部CLIであり、このcrateへリンク・同梱しません。

参照元`pdfsvgpptx`はproprietary licenseです。そのコードをcopy/vendorせず、公開仕様と観察した入出力契約をもとに独立実装しています。参照QA文書、SVG、PPTX、画像はこの配布物へ含めません。
