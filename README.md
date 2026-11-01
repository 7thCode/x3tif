# x3tif

Sigma Foveon の RAW(`.x3f`)を、**現像せずに 16bit TIFF へ変換する**小さなデスクトップアプリです。
ウィンドウに X3F ファイルをドロップすると、同じフォルダに `.tif` を書き出します。

## 対応機種

- dp Merrill(dp1/dp2/dp3 Merrill)
- dp Quattro(dp0/dp1/dp2/dp3 Quattro)
- sd Quattro / sd Quattro H

SD9〜SD15、SD1 など旧世代の X3F は対象外です(変換せず、エラーを表示します)。

## 出力

- 16bit・3 チャンネル(R, G, B = 下層・中層・上層)のリニア RGB、非圧縮 TIFF。
- **現像処理は行いません**(黒レベル減算、ホワイトバランス、色変換、ガンマなし)。センサー値をそのまま保存します。
  必要な係数は XMP に入っているので、Lightroom / Capture One / darktable 等で現像できます。
- メタデータ:
  - 標準タグ: 機種名、撮影日時、向き(Orientation)
  - EXIF: 絞り、露出時間、ISO、露出補正、焦点距離(取得できたもの)
  - XMP(`x3f:` 名前空間): X3F の PROP と CAMF(WB ゲイン、色マトリクス、有効領域、RawOffset など)
    ※ バッドピクセルマップなど大きなテーブルは含みません。

### Quattro について

Quattro 世代は、上層がフル解像度で、中層・下層は縦横 1/2 の解像度です。出力は上層の解像度に合わせ、
中層・下層を**双一次補間で 2 倍に拡大**したものです。補間で元の情報が増えるわけではありません。
また、出力は下層が覆う範囲(KeepImageArea)に切り出します。上層の右端にある余分な列は含まれません。

## 使い方

[Releases](https://github.com/7thCode/x3tif/releases) からインストーラをダウンロードして起動し、
X3F ファイルをウィンドウにドロップします。既存の `.tif` は上書きせず、`-1`, `-2`… を付けて保存します。

## ビルド

Rust(stable)と Node.js が必要です。

```bash
npm ci
npm run tauri -- build      # 開発時は npm run dev
cargo test                  # コアライブラリのテスト
```

構成:

- `core/` — X3F の解析・デコード、TIFF 書き出し(外部依存なしの Rust)
- `src-tauri/` — Tauri アプリ本体(ドロップ → 変換)
- `src/` — 最小の UI(`index.html` のみ)

## 検証

デコード結果は、参照実装 [x3f_tools](https://github.com/Kalpanika/x3f) の `x3f_extract -unprocessed` と比較しています。
dp Merrill は全ピクセル一致、dp Quattro は最上層が全ピクセル一致、下 2 層は独立に計算した双一次補間と一致しました。
sd Quattro は参照実装が読み込めないため、画像として破綻していないことを目視で確認しています。

## ライセンス

MIT License([LICENSE](LICENSE))。

## 謝辞

X3F の構造と圧縮方式は、Roland Karlsson 氏らの [x3f_tools](https://github.com/Kalpanika/x3f)(BSD スタイルライセンス)
のソースコードを参照して実装しました。ライセンス表記は [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) を参照してください。

## 開発の進め方

必ずフィーチャーブランチで作業し、プルリクエスト経由で `main` にマージします(直接 push しません)。
リリースは `git tag vX.Y.Z && git push origin vX.Y.Z` で行います(macOS / Windows / Linux のインストーラが GitHub Release に作成されます)。
