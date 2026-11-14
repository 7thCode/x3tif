---
paths:
  - "core/**/*.rs"
  - "src-tauri/**/*.rs"
---

# テストルール

- テストは各 `.rs` 内の `#[cfg(test)] mod tests` に書く(`core/tests/` は使っていない)。
- 実行は `cargo test`。変更後は `cargo clippy --workspace --all-targets` と `cargo fmt --all` も通す。
- 命名: `対象_条件_期待結果`(例: `decode_truncated_file_returns_error`)。
- 必ず考える範囲: 正常系 / 境界値(空・最小サイズ・端の画素) / 壊れた入力(途切れた X3F、不正なヘッダ) / 対象外機種。
- 実ファイルの X3F はリポジトリに置かない。小さな合成データをテスト内で組み立てる。
- 画質の調整(WB・トーン・鮮鋭化)は単体テストで決めない。数値の根拠は `develop.rs` / `finish.rs` の doc コメントに残し、テストは回帰(既知の入力 → 既知の出力)を守る用途にする。
- テストを無効化・削除して通さない。落ちたら原因を直す。
