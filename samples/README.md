# サンプル

`docsvg` が何を入力に取り、何を出力するのかを、実際のファイルで確認できます。

| | 入力 | 出力 |
|---|---|---|
| PDF | [`source/sample.pdf`](source/sample.pdf) | [`svg/pdf/`](svg/pdf/) — 2ページ |
| PPTX | [`source/sample.pptx`](source/sample.pptx) | [`svg/pptx/`](svg/pptx/) — 2ページ |
| XLSX | [`source/sample.xlsx`](source/sample.xlsx) | [`svg/xlsx/`](svg/xlsx/) — 4ページ |
| DOCX | [`source/sample.docx`](source/sample.docx) | [`svg/docx/`](svg/docx/) — 2ページ |
| drawio | [`source/sample.drawio`](source/sample.drawio) | [`svg/drawio/`](svg/drawio/) — 3ページ |

`svg/*/page-NNNN.svg` は対応するブラウザで開けます。画像は埋め込み済みなので、
1枚だけ取り出せます。文字の見た目は表示環境のフォントによって変わる場合があります。
同じフォルダの `conversion.json` には、ページ数・所要時間・警告が記録されています。

## それぞれのサンプルが確かめていること

- **PDF** — テキスト、ベジェ曲線の塗り、線のストローク、埋め込みRGB画像、そして
  複数ページ。フォントは Helvetica を `/Widths` なしで指定しており、標準14フォントの
  字幅が正しく適用されるかを見ています。
- **PPTX** — スライドマスター、テーマ色、図形の塗り、箇条書き、16:9のスライドサイズ。
- **XLSX** — 180行のデータ、書式付きの数値、結合セル、`<pageSetup>` による用紙設定。
  1枚の紙に収まらないので、**印刷したときと同じように4ページへ分割**されます。
- **DOCX** — 見出しスタイル、罫線付きの表、明示的な改ページ、ヘッダーとフッター
  （フッターの `PAGE` フィールドはページ番号に解決されます）。
- **drawio** — 2ページ構成、図形と塗り、影、直交・曲線のコネクタと矢尻、エッジのラベル、
  スイムレーンと入れ子の子要素。入力は読みやすいXMLのまま置いていますが、エディタが既定で
  書き出す圧縮された `<diagram>` も同じように変換できます。

  往復も試せます。`--embed-drawio-source` を付けて変換したSVGは、
  `docsvg reverse <出力ディレクトリ> --output <新しい.drawio>` で元の図面に戻ります
  （画像ではなく、編集できる図形として戻ります）。

  3ページ目はシェイプライブラリの実例です。左の2つは
  [`source/sample-stencils.xml`](source/sample-stencils.xml)（このリポジトリで書いた
  mxStencil形式のライブラリ）から描いています。3つ目は図面自身が `shape=stencil(...)` で
  持っているので、ライブラリの指定は要りません。

  ```bash
  docsvg samples/source/sample.drawio --output out/ --stencils samples/source/sample-stencils.xml
  ```

  drawio公式のライブラリ（AWS・Azure・GCPなど）は同梱しませんが、同じ形式なので
  `--stencils` にdrawioの `stencils` ディレクトリを渡せばそのまま描けます。

## 作り直す

サンプルの入力ファイルは、外部から持ち込まずにこのリポジトリのスクリプトで生成しています。
そのためライセンスはリポジトリ本体と同じで、誰でも自由に使えます。

```bash
cargo build --release
python3 scripts/make_samples.py
```

`scripts/make_samples.py` が `source/` の4ファイルを書き直し、`docsvg` に通して
`svg/` を作り直します。レンダラーを変更したあとに実行すると、出力の変化がそのまま
差分として見えます。

> `conversion.json` の `elapsed_ms` は実行ごとに変わります。差分に出ても問題ありません。

`provenance.json`に入力4ファイルのSHA-256を記録しています。公開検査は一致するサンプルだけを許可し、テストでは生成スクリプトから再現できることも確認します。
