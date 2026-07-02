# Ghostty パリティ実装計画

作成: 2026-07-02。inc-1 完了・inc-2 ほぼ完了時点の機能棚卸し（全クレート精査、テスト約182本）に基づく、
「Ghostty レベル」到達までのフェーズ計画。README の Roadmap (inc 2–6) を実測ギャップで具体化したもの。

## 現在地（要約）

**堅実に動いている**: CSI 編集系ほぼ全部 / SGR 16・256・truecolor / alt screen / DECSTBM /
DECSC・DECRC / bracketed paste / SGR マウス報告 / ワイド文字 / リサイズ・リフロー（カーソルアンカー保持）/
scrollback 10k 行 + 選択 + 検索エンジン / OSC 0・2・4・10-12・52 / DA・DSR 応答 / クリップボード /
IME preedit / ネイティブメニュー / コード内キーバインドエンジン / headless GPU 回帰テスト。

**主要ギャップ**（監査 + 棚卸しより）:

1. 装飾描画 — 下線が一切描かれない（属性は保持済み）、取消線なし、bold/italic フェイスなし、
   min-contrast 未使用、罫線合成なし、カラー絵文字なし、リガチャなし。
2. プロトコル — DECSCUSR / focus 1004 / sync 2026 / レガシーマウスエンコード / DCS(DECRQSS,XTGETTCAP) /
   OSC 8・7・133 / Kitty keyboard / Kitty graphics / grapheme clustering(2027) が未実装。
3. UX — タブ / 分割 / 検索プロンプト UI / URL クリック / ベル / ウィンドウタイトル反映 /
   実行時フォントサイズ変更 / フルスクリーン / マルチウィンドウ / テーマ選択 / 背景 opacity が未実装。
4. 品質負債 — 監査 P1〜P3（修飾キーエンコード、結合文字破棄、毎フレーム全コピー、release profile 未設定等）。

Sixel は Ghostty 本体も非対応のため **パリティ対象外**（Kitty graphics が画像経路）。

---

## Phase 0 — 監査負債の完済（P1〜P3）

先行理由: 以降の全フェーズが触る箇所（input.rs / snapshot / parser / app.rs）の既知バグ・性能欠陥を
先に潰さないと、上に積む機能が全部再修正になる。`.nexus/loops/noa-critical/backlog.md` の P1〜P3 が単一真実源。

- **P1 correctness (8件)**: 修飾キーエンコード欠落 / 結合文字・ZWJ 破棄 / CUU・CUD margin クランプ /
  UI スレッド blocking write + unbounded channel / overlong UTF-8 / DECSTBM 不正 region /
  scale factor 変更時の再計算 / Surface Lost 後の redraw。
- **P2 performance (5件)**: FrameSnapshot の dirty-row diff 化 / CSI Vec clone 除去 /
  FontRef 再パース除去 / `[profile.release]` (lto, codegen-units=1) / Redraw coalesce。
- **P3 (4件)**: 8-bit ST / PtyEvent::Error 表面化 / 初期化 expect() / JoinHandle 等。

修飾キーエンコード（P1-2）はここで **Ghostty 互換の完全版** にする: Shift/Alt/Ctrl+矢印
(`CSI 1;m A`)、F1–F12、Home/End/PgUp/PgDn/Insert/Delete、Alt-as-Esc prefix、xterm modifyOtherKeys 相当の
既定挙動。Phase 4 の Kitty keyboard の土台になる。

検証: 既存 verify.sh 方式の後続ループ（`noa-p1-fidelity` / `noa-perf`）。各項目回帰テスト必須。

## Phase 1 — VT 忠実度の完成（グリッドまで）

Ghostty との「エスケープシーケンス互換」を宣言できる状態にする。全て noa-vt / noa-grid 中心で
GUI 非依存 → ユニットテストで完結。

- **DECSCUSR** (`CSI Ps SP q`): カーソル形状 6 種を `Terminal` 状態に保持（描画は Phase 2）。
- **SGR 完備**: 21 二重下線 / 4:x 下線スタイル（curly 等）/ 58・59 下線色。`CellAttrs` 拡張。
- **モード**: 1004 focus 報告（winit Focused → CSI I/O）、2026 synchronized output
  （BSU/ESU で snapshot 更新を保留）、2027 grapheme clustering。
- **結合文字・ZWJ**: セルを grapheme cluster 単位に（P1-3 の本修正）。絵文字 ZWJ 列・VS16 の幅判定を
  Ghostty の `grapheme.zig` 挙動に合わせる。
- **マウス**: X10/UTF-8(1005)/urxvt(1015) レガシーエンコード追加（現状 SGR のみ）。
- **DCS 基盤 + DECRQSS / XTGETTCAP**: parser の DcsPassthrough を Handler 経路に昇格。
- **BEL**: Handler に bell イベント（鳴らすのは Phase 3 の UX 側）。
- **OSC 8 / 7 / 133**: グリッド側の状態保持（ハイパーリンク ID をセル属性に、cwd・プロンプトマークを
  Terminal に）。UI 反映は Phase 3。
- **パリティハーネス新設**: esctest2 / vttest を CI 外部オラクルとして流す薄いランナー +
  「同一バイト列 → Ghostty と noa のスクリーンダンプ比較」fixture 形式を `tests/parity/` に確立。
  以降のフェーズの受け入れ基準を「ハーネス緑」に統一する。

## Phase 2 — 描画を Ghostty 品質に

「見た目が Ghostty」の核。noa-font / noa-render 中心。

- **下線ジオメトリ**: single/double/curly/dotted/dashed + 下線色。Ghostty 同様シェーダ/専用 quad で合成
  （フォントメトリクスの underline position/thickness 使用）。取消線・overline も同時に。
- **カーソル形状描画**: DECSCUSR の block/bar/underline + blink。非フォーカス時は中抜き枠（Ghostty 挙動）。
- **bold / italic**: font-kit で weight/style 別フェイスを解決、無ければ合成（embolden / oblique）。
  `GlyphKey` に style 軸を追加。
- **罫線・ブロック要素の手続き合成**: U+2500–259F, U+E0B0– (Powerline) をフォントを介さず描画
  （Ghostty の `sprite/` 相当）。Nerd Font アイコンのセル内センタリング調整もここ。
- **カラー絵文字**: sbix/CBDT を swash でラスタ → RGBA アトラス（既存 R8 と 2 枚持ち）。
- **シェーピング/リガチャ**: swash shaper で行単位シェーピング（`=>` 等の合字、既定 ON・設定で OFF）。
  セル→グリフのマッピングを 1:1 から m:n に一般化する、このフェーズ最大の構造変更。
- **minimum-contrast**: 既存 uniform を実配線し設定公開。
- **背景 opacity**: サーフェス α + clear color α、`background-opacity` 設定。
- 検証: pipeline.rs 拡張 + スナップショット画像比較（wgpu offscreen readback）を導入し、
  Ghostty と同一コマンドのスクリーンショット目視パリティをチェックリスト化。

## Phase 3 — スクロールバック基盤・リンク・検索 UI・設定（≒ inc 3）

- **ページ化 scrollback**: `VecDeque<Row>` 行クローン方式 → ページ（固定サイズブロック）+
  **インターン化スタイル**（page-local style table、Ghostty の PageList/style set 相当）。
  上限を行数でなくバイト量で設定（`scrollback-limit`）。Phase 2 の描画高速化と併せ長大出力を実用域に。
- **OSC 8 ハイパーリンク UI + URL 自動検出**: hover 下線、Cmd+クリックで `open`。
  正規表現 URL matcher は Ghostty の `link` 設定互換の形に。
- **検索 UI**: エンジンは実装済 → オーバーレイ入力プロンプト、マッチ件数、n/N 移動、
  Cmd+F バインド。
- **設定システム拡張**: 現行 3 キー (cols/rows/font_size) → Ghostty の config 体系に寄せる:
  `font-family` / `font-size` / `theme` / `background-opacity` / `cursor-style` / `keybind` /
  `scrollback-limit` / `copy-on-select` / `mouse-hide-while-typing` 等。実装済みのキーバインドエンジンを
  config から公開。リロード（Cmd+Shift+,）対応。
- **UX 小物の完済**: ウィンドウタイトル OSC 反映 / ベル（audio + Dock attention）/ copy-on-select /
  ホイールでのローカル scrollback スクロール / 実行時フォントサイズ変更 (Cmd+±/0) / フルスクリーン。

## Phase 4 — タブ・分割・テーマ（≒ inc 4）

アーキテクチャ影響が最大のフェーズ。`Arc<Mutex<Terminal>>` 1 個 + io thread 1 本の現行構造を
**Surface 多重化**（Ghostty の Surface/apprt 分離相当）に再編する。

- **Surface 抽象**: {Terminal, Pty, io thread, renderer state} を Surface としてカプセル化し、
  1 ウィンドウ N Surface に。フォーカス管理・タイトル・通知を Surface 単位に。
- **タブ**: macOS ネイティブタブ（winit の tabbing identifier）優先で Ghostty と同じ操作感
  (Cmd+T/W, Cmd+1..9, Cmd+Shift+[])。
- **分割**: split tree（Ghostty の SplitTree 相当の再帰レイアウト）、Cmd+D / Cmd+Shift+D、
  フォーカス移動 (Cmd+Opt+矢印)、リサイズ、zoom (Cmd+Shift+Enter)。renderer は viewport 分割描画に対応。
- **マルチウィンドウ**: winit 複数 Window + per-window surface tree。
- **テーマ集**: Ghostty が同梱する iTerm2-Color-Schemes 由来 ~460 テーマをビルド時取り込み、
  `theme = <name>` で選択 + ライト/ダーク自動切替。
- **フォント設定**: `font-family` フォールバックチェーン設定化、`font-feature`、`font-style` 上書き。

## Phase 5 — モダンプロトコル（≒ inc 5）

- **Kitty keyboard protocol**: progressive enhancement 全フラグ（disambiguate / report events /
  alternate keys / report all keys as escapes / associated text)。Phase 0 の xterm 完全版が土台。
- **Kitty graphics protocol**: APC 経路新設、画像デコード（png）、GPU テクスチャ管理、
  placement/削除/z-index、Unicode placeholder。renderer に画像レイヤ追加。
- **シェル統合**: OSC 133 (prompt mark → prompt jump (Cmd+↑/↓)、コマンド終了通知)、OSC 7 (cwd →
  新タブ/分割の cwd 継承、タイトル表示)。zsh/bash/fish 用統合スクリプト同梱 + 自動注入
  (Ghostty の shell-integration 相当)。
- **DCS 続き**: XTVERSION、DECRQM 応答完備。
- 検証: kitty 公式テストスイート・notcurses デモ・`kitten icat` 実写確認をパリティチェックに追加。

## Phase 6 — macOS ネイティブ磨き（≒ inc 6）

- クイックターミナル（グローバルホットキー + 上端スライドイン、NSPanel 相当）。
- コマンドパレット (Cmd+Shift+P、実装済みコマンド機構を列挙 UI 化)。
- 背景ブラー（private API `CGSSetWindowBackgroundBlurRadius` — Ghostty 同等）。
- セッション復元（ウィンドウ/タブ/分割トポロジ + cwd を再現。Ghostty 同様コンテンツは復元しない）。
- Secure Keyboard Entry、タイトルバースタイル (`macos-titlebar-style`)、Option-as-Alt 設定、
  通知 (OSC 9 / 777)。
- `noa +list-themes` / `+list-keybinds` 等の CLI アクション。

---

## 横断事項

- **パリティハーネス**（Phase 1 で新設）を全フェーズの受け入れゲートに。5 次元 Parity Map の
  「Behavioral / Feature」は CI 化、「Visual」はスクリーンショット比較チェックリスト。
- **CI**: GitHub Actions で `build / clippy -D warnings / test --workspace` + macOS ランナー。
  現状 CI が無いので Phase 0 と同時に導入。
- **ループ運用**: 各フェーズを `.nexus/loops/noa-<phase>/` の後続ループに分割
  （goal.md に AC、backlog.md にタスク、verify.sh に機械ゲート — noa-critical と同型）。
- **推定規模**: Phase 0: 小〜中 / 1: 中 / 2: 大（シェーピング m:n 化が山） / 3: 中〜大（ページ化） /
  4: 大（Surface 再編）/ 5: 大（Kitty graphics）/ 6: 中。
- **順序の根拠**: 負債→意味論→見た目→基盤→UX 大物→プロトコル→磨き。各フェーズは前フェーズの
  構造変更（grapheme セル、m:n グリフ、Surface 抽象）に依存するため入れ替え不可が基本。
  例外として Phase 3 の UX 小物と Phase 2 は並行可。
