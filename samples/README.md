# サンプル

`docsvg` が何を入力に取り、何を出力するのかを、実際のファイルで確認できます。

| | 入力 | 出力 |
|---|---|---|
| PDF | [`source/sample.pdf`](source/sample.pdf) | [`svg/pdf/`](svg/pdf/) — 2ページ |
| PPTX | [`source/sample.pptx`](source/sample.pptx) | [`svg/pptx/`](svg/pptx/) — 2ページ |
| XLSX | [`source/sample.xlsx`](source/sample.xlsx) | [`svg/xlsx/`](svg/xlsx/) — 4ページ |
| DOCX | [`source/sample.docx`](source/sample.docx) | [`svg/docx/`](svg/docx/) — 2ページ |

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
