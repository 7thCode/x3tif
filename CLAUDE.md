# x3tif

Sigma Foveon の X3F を 16bit TIFF に変換する Tauri デスクトップアプリ。仕様・使い方・対応機種は `README.md` を参照(ここには重複して書かない)。

## 構成

- `core/` — 外部依存なしの Rust ライブラリ `x3tif_core`。アプリの中身はほぼここ。
  - `x3f.rs`(コンテナ解析・デコード)、`camf.rs`(校正データ)、`develop.rs`(「見たまま」現像。最大のファイル)、
    `finish.rs`(Quattro の色ノイズ抑制・鮮鋭化)、`preview.rs`(縮小プレビュー)、`tiff.rs` / `png.rs` / `icc.rs` / `exif.rs`(書き出し)、`convert.rs`(変換の入口)
  - `examples/x3tif.rs` — 検証用 CLI(下記)
- `src-tauri/` — Tauri 本体(`src-tauri/src/lib.rs`: ドロップ → プレビュー → 調整 → 保存)
- `src/index.html` — UI はこの 1 ファイルだけ(フレームワークなし)

## コマンド

```bash
cargo test                                   # core のテスト(テストは各 .rs 内)
cargo clippy --workspace --all-targets
cargo fmt --all
npm ci && npm run dev                        # アプリを開発起動
npm run tauri -- build                       # インストーラ作成
cargo run --release -p x3tif-core --example x3tif -- in.x3f out.tif developed      # 現像出力(raw / developed [ev])
cargo run --release -p x3tif-core --example x3tif -- in.x3f out.png preview [ev [max_side]]
```

release プロファイルは `opt-level = "z"` + LTO で**ビルドが遅い**。画像処理の速度を測るときや大きな画像で試すときは、この設定の影響を考える。

## 色・画質の作業ルール(重要)

- 較正(WB 倍率・色マトリクス・トーン・鮮鋭化など)は、**仕上がった「見たまま」出力を Photo Pro の TIFF と直接比べて**行う。生データ出力から求めたモデルは増幅誤差で外れる(v0.1.4 の失敗)。参照は `~/Desktop/sample/<機種>/`(dp0 quattro / dp1 merrill / sd quattro。X3F と、Photo Pro の TIFF(`*_quattro.tif` / `*_merrill.tif` / `*_sd.tif`)の組)。比較は `core/examples/tiffcmp.rs`。
- 指標は R−G・B−G のずれ。自動露出の差が混ざるので、チャンネルの絶対値の平均は使わない。
- 倍率への感度が高く(青 1% で約 2.4/255)、1 枚の結論で決めない。複数枚で評価し、機種(CAMERAID)ごとに分けて考える。
- 試行は一時パッチ(環境変数で上書き → 測定 → `git checkout` で戻す)で行い、コミットに残さない。
- 根拠になった測定値は `develop.rs` / `finish.rs` の doc コメントに残す(既存の書き方に合わせる)。

## 守ること

- 対象外の機種(SD9〜SD15、SD1)は変換せずエラーを返す。ここを緩めない。
- 生データ出力は無損失(ビット深度分の左シフトのみ)。現像処理を混ぜない。
- 既存の `.tif` は上書きしない(`-1`, `-2` を付ける)。
- `core` に外部クレートを足さない方針。足す場合は理由を先に説明する。
- `Cargo.lock` を意図せず更新しない(#23 で zerofrom が誤って上がった)。依存を変えていないのに差分が出たら戻す。

## 開発の流れ

- 必ずフィーチャーブランチで作業し、PR 経由で `main` にマージする(直接 push しない)。
- コミットメッセージ・コードのコメントは日本語。識別子は英語。
- リリース: バージョンを上げる PR(`package.json` / `core/Cargo.toml` / `src-tauri/Cargo.toml` / `src-tauri/tauri.conf.json` の 4 か所。`Cargo.lock` は該当 2 クレートの行だけ変わるはず)をマージ → `git tag vX.Y.Z && git push origin vX.Y.Z`。CI(`.github/workflows/build.yml`)が 3 OS のインストーラを Release に付ける。
- ドキュメントを変えたら `README.md` と `docs/index.html` の両方を揃える。
