# Phase 1.9 — コメント焼き込みエクスポートのテストリスト

ASS 生成のコアは `src-tauri/src/downloader/comment_ass.rs`、ffmpeg 連携は
`src-tauri/src/downloader/ffmpeg.rs`、コマンドは `src-tauri/src/commands.rs`
(`export_video_with_comments`)、UI は `src/routes/library/[id]/+page.svelte`。

## 背景

DL 済み動画 + コメントスナップショットから、コメントを字幕として **映像へ
焼き込んだ MP4** を書き出す。プレイヤーが `@xpadev-net/niconicomments` で
Canvas にリアルタイム描画しているものを、ffmpeg の `ass` フィルタで合成できる
静的 ASS へ落とし込む方式。

設計方針 (README「Rust コア重視」) に従い、レイアウト計算は I/O を持たない純
関数 `generate_ass` に集約し、単体テストで全分岐を固定する。重い再エンコードは
ffmpeg に委譲し、`-progress pipe:1` を解析して `burnin:progress` イベントで
進捗を流す。

### 本家 niconicomments の忠実移植 (再現度改善)

旧実装は「レーングリッド + 単純な時間衝突」という近似で、本家との配置ズレが
大きかった。現行 `comment_ass.rs` は `@xpadev-net/niconicomments` (v0.2.78) の
**HTML5 描画アルゴリズムをそのまま Rust へ移植**している:

- 全計算を本家と同じ **1920×1080 の内部座標系**で行い、ASS の PlayResX/Y も
  1920×1080 に固定。libass が出力解像度へスケールするので、本家の
  `setScale(width/1920, height/1080)` と等価な見た目になる (解像度非依存)。
- フォントサイズは `getCharSize` / `getLineHeight` / `getFontSizeAndScale` を
  忠実移植。1080 換算で char 箱 = small 65 / medium 95.5 / big 138px、字面 =
  50.6 / 75.9 / 109.6px。**改行リサイズ**(行数閾値超で縮小)・**横幅オーバー
  フローリサイズ**(固定コメントが描画幅を超えたら縮小)も再現。
- 流れる (naka) コメントは本家の速度式
  `(commentDrawRange + width*0.95)/(long+100)` と `getPosX` をそのまま使い、
  ASS の `\move` で線形補間 (**幅依存の速度・滞留時間を完全再現**。旧実装の
  「一律 4 秒」は誤り)。
- 当たり判定は本家の `processMovableComment` / `getMovablePosY` /
  `processFixedComment` / `getFixedPosY` / `getPosY` を移植。vpos スロット毎の
  衝突配列・**投稿者/視聴者レイヤー分離**・画面溢れ時の配置まで再現。
- 各コメントは **行ごとに 1 Dialogue** (`\an4`= 左中央基準) を出力し、行高を
  本家の lineHeight に合わせて配置 → 単行・複数行・AA を字面メトリクス非依存で
  正確に積む。
- 色 (通常 + プレミアム 2 系列 + `#hex`)、縁取り (本家 `getStrokeColor`: 黒 40%、
  純黒文字は白縁)、`nico:stroke` / `nico:opacity` / `_live`、`@N` 長さ、`ender`、
  `full`、`invisible` に対応。

実 libass で代表的なコメント群 (流れる/固定/複数行/高密度弾幕/色/縁取り) を
焼き込み、本家同等の配置・字面・レーン積みになることを目視確認済み。

## 単体テスト（`comment_ass.rs` 内 `#[cfg(test)] mod tests`）

### 色・縁取り

- [x] 名前付き色 (通常 + プレミアム 2 系列 + 別名 marinblue/marineblue) を 0xRRGGBB へ
- [x] `#RRGGBB` / `#RGB` の hex 色をパース、`#` 無しは拒否
- [x] 0xRRGGBB → ASS の `&HBBGGRR&` (BGR 順) 変換
- [x] 不透明度 → アルファバイト (1.0→00 / 0.0→FF / 0.5→80)
- [x] 縁取りは既定黒、純黒文字は白縁 (本家 getStrokeColor)

### コマンド解釈 (本家準拠の「最初勝ち」)

- [x] 既定は naka / medium / 白 / defont
- [x] 位置 (ue/shita/naka)・サイズ (big/small/medium)・色・フォントを解釈
- [x] 色は**最初勝ち**、hex (プレミアム想定) も反映
- [x] 大文字小文字を無視
- [x] `invisible` フラグ・本文 `/` 始まり
- [x] `@N` で長さ指定
- [x] `fixed_duration_sec` を変えても naka の速度(long)は不変

### フォントメトリクス (本家 niconico.ts 移植)

- [x] char 箱サイズが small 65 / medium 95.5 / big 138 (1080 換算) と一致
- [x] 行高が medium 81.3 / big 127.2 と一致
- [x] medium 字面 ≈ 76px、単行箱高 ≈ 95.5px
- [x] 全角は 1em、半角 (i/W) は幅が異なる
- [x] 改行リサイズで縦長コメントが縮む、`ender` は抑止
- [x] 固定コメントは描画幅超過で縮む、naka は縮まない

### 速度・配置

- [x] naka 速度は幅依存 (広いほど速い)
- [x] 時間経過で x が減る (右→左)
- [x] naka 開始 x は右端外 (≳1920)、vpos=0 は t=0 へクランプされ流入済み

### エスケープ・時刻

- [x] `{` `}` `\` をエスケープ (override 注入を防ぐ)、空白は `\h`
- [x] 秒 → `H:MM:SS.cs` タイムコード

### 生成全体 (`generate_ass`)

- [x] ヘッダの PlayRes は 1920×1080 固定 + Style 行
- [x] コメント 0 件 → Dialogue 行なし
- [x] naka は `\move(...)` + `\an4`
- [x] ue は `\pos` + `\an4`・上半分・既定 3 秒表示
- [x] shita は画面下半分に配置 (下から積む)
- [x] `invisible` / 空白本文はスキップ
- [x] 動画長を超えて出現するコメントはスキップ
- [x] 色コマンドが `\c` タグになる
- [x] **当たり判定**: 同時刻の流れるコメントは別レーン (別 y)
- [x] 時間差があれば最上段レーンを再利用できる
- [x] **投稿者/視聴者は別レイヤー**で同レーンに同居できる
- [x] 同時刻の ue は下へ積み上がる
- [x] 複数行コメントは行ごとに 1 Dialogue (y が行高ぶん下がる)
- [x] 入力が vpos 逆順でも vpos 昇順で処理
- [x] 不透明度がアルファ (`\1a`) に焼き込まれる
- [x] フォント名が Style と `\fn` に反映
- [x] 本文の `{ }` はタグではなくエスケープされる

## 単体テスト（`ffmpeg.rs` 内 `#[cfg(test)] mod tests`）

### プローブ (`parse_probe` / `find_dimensions` / `parse_timecode`)

- [x] stderr から解像度と長さを抽出
- [x] 縦長動画も解像度を取れる
- [x] 映像ストリームが無ければ None
- [x] `N/A` のタイムコードは None
- [x] DAR 表記の `16x9` ではなく実解像度を拾う

### 進捗パース (`parse_progress_line`)

- [x] `out_time_us` / `out_time=HH:MM:SS.cs` を秒に
- [x] それ以外の行は None

## 手動 / E2E 検証（要・本物の ffmpeg + libass）

ローカルで使い捨て統合テストを通し、以下を確認済み（CI は ffmpeg を空スタブに
するため恒久テストには含めない）:

- [x] `generate_ass` の出力を libass が構文エラーなく受理する
- [x] `burn_in_comments` の ffmpeg コマンド (filter_complex `[0:v]ass=...[v]` +
      libx264 再エンコード + faststart) が成功する
- [x] 進捗コールバックが ~1.0 まで到達する
- [x] 出力が入力と同じ解像度・長さの再生可能な MP4 になる (probe 往復)

## コマンド層（`export_video_with_comments`）

- [x] video_id を検証し、未 DL ならエラー
- [x] snapshot_id 省略時は最新スナップショットを使用
- [x] コメント 0 件ならエラー（焼き込む対象なし）
- [x] 解像度・長さは DB 優先、欠落時は ffmpeg プローブで補完
- [x] 出力は `exports/` 配下（`cleanup_storage` が消さない場所）
- [x] 一時 ASS は成否に関わらず削除
- [x] ffmpeg 失敗時は stderr 抜粋を返し、壊れた出力を削除
- [x] 進捗を `burnin:progress` (`{ videoId, percent }`) で 1% 刻みに間引いて通知

## UI（ライブラリ動画ページ）

- [x] スナップショットセクションに焼き込みエクスポートのトグル
- [x] フォント倍率・不透明度のスライダ
- [x] 進捗バー（`burnin:progress` 購読）
- [x] 完了後に出力パス表示 + フォルダを開くボタン
- [x] 実行中は二重起動を抑止、離脱時にリスナ解除

## 実装済み（本家再現の一環）

- AA（複数行アスキーアート）のレイアウト再現 — 行ごとに本家 lineHeight で配置
- 改行リサイズ（行数に応じた縦縮小、niconicomments の resizedY 相当）
- 横幅オーバーフローリサイズ（固定コメントの resizedX 相当）
- 幅依存のスクロール速度・滞留時間、本家の当たり判定（投稿者/視聴者レイヤー分離）

## 数値クロス検証（本家 niconicomments との突き合わせ）

`niconicomments` (v0.2.78) を node-canvas + Noto Sans CJK JP(weight 600) で
headless 実行し、各コメントの width/height/posY を抽出して Rust 移植版と比較。
恒久テスト `cross_validates_against_niconicomments_reference` で固定:

- [x] char 箱高 (small/medium/big・複数行・改行リサイズ) が本家と厳密一致 (±0.1px)
- [x] 固定 (ue/shita) コメントの積み上げ posY が本家と厳密一致
- [x] naka の当たり判定積み上げ posY が本家系列 (0/95.53/191.06/…) と一致
- [x] CJK 幅が厳密一致、ASCII 幅は実測テーブルで ±3px (整数 em の丸めのみ)
- [x] ASCII 送り幅テーブルは Noto Sans CJK JP(600) の実測値で libass 描画と整合

## 範囲外（Phase 2.0 以降）

- 保存先をユーザが選ぶ保存ダイアログ（`dialog:allow-save` 追加が必要）
- Flash 版コメントの文字単位フォント変化（gulim/simsun の置換規則）。
  プレイヤーが `mode: 'html5'` 固定のため、焼き込みも HTML5 描画に統一している。
- **縦長 (portrait) 動画の特別扱い**。プレイヤーは portrait 時に 16:9 バンド +
  `scale:0.85` でコメントを表示するが、焼き込みは 1920×1080 レイアウトを libass が
  フレームへスケールするため、縦長動画ではコメントが縦伸びする。16:9 (大多数) は
  画素単位で一致しており、portrait は既知の制約として保留 (本家も非 16:9 では
  setScale で歪むため、回帰ではない)。
- スナップショット間の diff 表示
- フレーム単位レンダリング（niconicomments-convert 方式）による完全ピクセル一致。
  現状は静的 ASS + libass で配置・字面・動きを高精度再現する方式。
