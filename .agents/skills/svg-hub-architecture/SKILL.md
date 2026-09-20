---
name: svg-hub-architecture
description: Architectural positioning and 4-quadrant functional model of SVG as a universal visual hub between arbitrary inputs, intermediate transforms, and target outputs.
---

# SVG Hub Architecture

SVG を「入力の終着点」でも「単なる画像」でもなく、**あらゆるソースとターゲットを媒介する中央ハブ（Universal Visual Hub）** として位置づける設計論。

```text
                             ┌─────────────────────┐
                             │     SOURCE DATA     │
                             └──────────┬──────────┘
                                        │
                     ┌──────────────────┼──────────────────┐
                     │                  │                  │
                Vectorize           Render            Convert
                     │                  │                  │
       Logo / Icon / Line art      Mermaid          AI / EPS
       Signature                    Graphviz          DXF / CAD
       Simple raster                LaTeX             Vector PDF
                                   Chart
                                   QR / Barcode
                                   Map
                     │                  │                  │
                     └──────────────────┼──────────────────┘
                                        ▼
                              ┌─────────────────┐
                              │       SVG       │
                              └────────┬────────┘
                                       │
                ┌──────────────────────┼───────────────────────┐
                │                      │                       │
              Render                 Code                  Fabrication
                │                      │                       │
          PNG / WebP             JSX / TSX                 DXF
          JPEG                   Vue                       G-code
          PDF                    path data                 Cricut
          EPS                    HTML inline               Embroidery
                │
                │
                └───────────────┐
                                ▼
                         SVG Transform
                                │
                    minify / optimize
                    stroke → path
                    text → outline
                    monochrome
                    responsive
                    clean paths
```

## 4大機能群

1. **① TO SVG (Render / Convert)**:
   - DSL・数式・コード・CAD等からSVGを生成する（Mermaid, Graphviz, LaTeX, Chart, QR, DXF）。
2. **② FROM SVG (Render / Code / Fabrication)**:
   - SVGから画像・PDF・UIコード・工作機械データ等へ出力する（PNG, WebP, PDF, JSX, Vue, DXF, G-code）。
3. **③ VECTORIZE**:
   - ラスター画像（ロゴ、線画、署名など）からベクターパスを抽出しSVG化する。
4. **④ SVG TRANSFORM (SVG to SVG)**:
   - SVGを用途別に最適化・変形する（minify, stroke-to-path, text-to-outline, monochrome, responsive, Cricut/プロッター対応）。

## 変換の非対称性と可逆性

- **非対称・非可逆が基本**: ソース（Mermaid, LaTeX等）からSVGへレンダリングすると高次な「意味構造」が失われるため、純粋な幾何からの完全逆復元は原理的に不可。
- **2つの逆復元アプローチ**:
  1. *Metadata-backed*: ソースコードを `<desc>` や `data-*` 属性に安全に埋め込んで100%可逆に復元する。
  2. *Heuristic Structural Reconstruction*: 未埋め込みのSVGであっても、罫線・図形・矢頭・テキストの幾何関係からトポロジーや表構造を推定再構築する。
