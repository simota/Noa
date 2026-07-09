# Spec: テーマ選択機能 (theme-selection)

> **Historical baseline:** L0/FRAME は仕様作成時点の実装前状態を保存している。
> 現在は574テーマのカタログ、設定UI、テーマを含むlive config reloadが実装済み。
> 現行状態は `docs/FEATURES.md` と現在のsymbolを参照し、以下の行番号は設計時点の
> 証跡として扱うこと。

## Metadata

- slug: `theme-selection`
- title: テーマ選択機能 (Theme Selection)
- status: **locked**(サインオフ 2026-07-02)
- owner: simota
- build-path: **orbit loop(engine: codex)** — 詳細は末尾「Build-path decision」
- recipe: /nexus spec — FRAME ✓ / EXPAND ✓ / CHALLENGE ✓ / SHAPE ✓ / SPECIFY ✓ / Quality Gate PASS(Judge 再検査済) / LOCK ✓

## L0 — Vision

1. **対象**: Ghostty からの乗り換えを想定した dotfiles 駆動のターミナルユーザー。noa は現在ハードコード単一テーマのみで配色変更不可。
2. **ジョブ**: Ghostty 構文の config に `theme = <name>`(将来 `light:X,dark:Y`)と書くだけで、Ghostty で使っていた配色が noa でそのまま再現される。
3. **成功条件**: 同名テーマで Ghostty と同一の見た目(bg/fg・カーソル・選択色・ANSI 256 パレット)になり、vim/tmux 等 TUI が意図した色で表示される。
4. **スコープ境界**: GUI テーマエディタ等 Ghostty に無い機能は対象外(フィデリティ原則)。既計画(inc-4 / REQ-THEME-001)の範囲に収める。
5. **制約**: `noa_render::Theme` がシーム(レンダラ変更ゼロ)、OSC 動的色と直交。ランタイムリロード未実装のため v1 は起動時適用(Ghostty はリロード可 → 忠実度ギャップとして明記)。

### Reuse / constraint findings (Lens reuse-scan)

- テーマ選択は**既計画の増分**: README inc-4「~460 themes」、parity-plan Phase 3-4、`REQ-THEME-001` = Partial。
- パラメータ化すべきシームは `noa_render::Theme`(crates/noa-render/src/theme.rs:13-27)— `default_fg/bg`・`cursor`・`selection_*`・`search_*`・`palette:[Rgb;256]`。`Theme::new()` が唯一のハードコードテーマ。
- OSC 4/10/11/12 動的色(`TerminalColors`, crates/noa-grid/src/osc.rs)は実装済みで、`resolve_with_colors` により静的 Theme と**直交合成済み** — 競合解決は不要。
- `noa-config` は ghostty-config 増分で Ghostty 構文パーサーへ置き換わる。`theme` は v1 認識スカラーキーとして継続受理し、未知キーは warn + ignore に変わる。
- CLI は `--cols/--rows/--font-size` のみ。優先順位モデル(CLI > file > default)は確立済み。
- **未実装資産**: テーマカタログ(データ・生成パイプライン無し)、macOS 外観変更フック、ランタイム config リロード(`REQ-CONFIG-002` = Missing)。
- 依存規則: テーマ解決(name → Theme)は純データで noa-app/noa-render より下層に置ける。wgpu/winit 接触不要。

### JTBD (Plea — 全項目 hypothesis)

1. `theme = <name>` で Ghostty の配色がそのまま動く(乗り換え離脱理由 No.1 対策)
2. macOS light/dark 切替への自動追従(`light:X,dark:Y`)— **v1 スコープ外**
3. `noa +list-themes` 相当での一覧確認と手軽な試着 — **v1 DEFER**
4. カーソル色・選択ハイライトまで一貫して変わる(半端な実装への不信回避)
5. ANSI 16 色パレット依存の TUI(vim/tmux/htop)がテーマ変更後も意図通り

## L1 — Requirements

### 機能要件 (Functional)

**設定キー・パース**

- **R-1**: `noa-config` の Ghostty 構文パーサーで文字列キー `theme` を v1 認識スカラーキーとして扱う。値はクォート任意の Ghostty 構文値としてパースし `ConfigOverrides.theme: Option<String>` に保持する。真に未サポートなキーは ghostty-config の diagnostics 経路で warn + ignore される。
- **R-2**: `theme` の値が `light:X,dark:Y` 形式(`light:`/`dark:` プレフィックスを持つペア構文)である場合、パース時に専用 diagnostic を出して値を受理しない。メッセージは未知キー warn や invalid value warn とは異なる文言とし、「ペア構文は未対応、単一名で指定せよ」という原因を明示する。片側だけ読み取る等の中途半端な受理は禁止。
- **R-3**: テーマ名解決時(`noa-app` 層)に、指定名が同梱カタログに存在しない場合は `log::warn!` で警告を出し、デフォルトテーマにフォールバックする。起動を継続する(hard fail しない。Ghostty 実挙動と一致)。

**noa-theme クレート・カタログ**

- **R-4**: 新規 crate `noa-theme` を追加する。依存は `noa-core` のみとし、`wgpu`/`winit`/`noa-render`/`noa-app` への依存を一切持たない純データ crate とする。
- **R-5**: Ghostty が配布する生成済みテーマファイル一式(iTerm2-Color-Schemes 由来 `ghostty-themes.tgz` の展開物、~460 件スナップショット)を `noa-theme` 配下に vendor する。upstream のコミット(またはリリースタグ)を固定し、取得元・固定コミット・取得日・ライセンス所在を記す帰属マニフェストを 1 枚添付する。
- **R-6**: `scripts/gen-themes` を新設し、vendor 済みテーマファイルから `noa-theme` 内の静的 Rust テーブル(生成物)を出力する。生成物はリポジトリにコミットし、`build.rs` は使用しない(確定裁定: コミット済み codegen)。

**テーマ解決**

- **R-7**: 解決済みテーマ名から `noa_render::Theme`(`default_fg`/`default_bg`/`cursor`/`selection_fg`/`selection_bg`/`palette` を含む)を構築できる。`noa-app` はこの `Theme` を起動時に `GpuState.theme` として使用する(全タブ共有、単一配線点)。`Theme` の残り 4 フィールド(`search_fg`/`search_bg`/`active_search_fg`/`active_search_bg`)は Ghostty テーマファイルに対応キーが存在しないため、v1 では現行ハードコード値のまま**テーマ適用外**とする(スコープドリフト防止のため明記)。
- **R-8**: vendor 済みテーマファイルに `selection-background`/`selection-foreground` が存在しない場合、Ghostty のランタイム反転フォールバック(前景色⇔背景色の入れ替え)と同じ規則で導出する。`cursor-color` が存在しない場合も、noa の既存デフォルトテーマが用いる規則(cursor = default_fg)を踏襲する。

**grid ベース色伝播**

- **R-9**: `noa-grid::TerminalColors` に、既存の動的 OSC 上書き(`Option<Rgb>` フィールド群)とは独立した「テーマ由来のベース色」(`default_fg`/`default_bg`/`cursor`/`palette[256]`)を追加する。`Terminal::new(GridSize)` のシグネチャは変更せず、構築後の追加呼び出し(setter)でベース色を注入する(確定裁定: grid 伝播を v1 に含める)。
- **R-10**: OSC 10/11/12 のクエリ応答は、対応する動的上書きが設定されていない場合、ハードコードされた xterm デフォルトではなく、アクティブテーマのベース色を報告する。
- **R-11**: OSC 104(パレットリセット)・110/111/112(fg/bg/cursor リセット)、および RIS(`ESC c`)/`full_reset` は、リセット後の基準をアクティブテーマのベース色にする(動的上書きのみをクリアし、ベース色自体は保持する)。

**優先順位**

- **R-12**: v1 のテーマ選択元は config ファイルの `theme` キーのみとする。CLI フラグ(`--theme`)は追加せず、`ConfigOverrides` のマージ処理はテーマに関して CLI 由来の値を持たない(Out-of-scope: `--theme` CLI フラグ DEFER)。

### 非機能要件 (NFR)

- **NFR-1(忠実度)**: 解決された `Theme` の色値(`default_fg`/`default_bg`/`cursor`/`selection_*`/`palette[]`)は、対応する vendor 済み Ghostty テーマファイルに記載された16進値とバイト単位(文字列比較)で一致すること。近似・視覚比較は不可。
- **NFR-2(起動コスト)**: テーマ名解決は静的テーブルへのルックアップのみで完結し、実行時のファイル I/O・ネットワークアクセスを行わない。計算量は O(log n)(ソート済み静的配列への二分探索)を上限とし、起動シーケンスに知覚可能な遅延を追加しない。
- **NFR-3(依存衛生)**: `noa-theme` および `noa-config` の依存グラフに `wgpu`/`winit` が含まれないこと(`cargo tree` で検証可能)。
- **NFR-4(後方互換)**: `Terminal::new(size: GridSize) -> Self` のシグネチャ、および既存 21 箇所の呼び出しサイト(本番コードは app.rs:332 の 1 箇所、残り 20 はテストフィクスチャ)は無変更のまま残る。
- **NFR-5(品質ゲート)**: 本変更後も `cargo test --workspace` と `cargo clippy --workspace` がクリーンであること。新規追加の `#[allow(...)]` によるもみ消しは禁止。
- **NFR-6(帰属管理)**: vendor コーパスの帰属マニフェストは upstream リポジトリ名・固定コミット SHA・取得日・ライセンス所在を記載し、`scripts/gen-themes` はマニフェスト欠如時に非ゼロ終了する。
- **NFR-7(オフライン生成)**: `scripts/gen-themes` はネットワークアクセスを行わない(vendor ファイルの取得・更新は別工程)。`cargo build --workspace --offline` は生成物の再実行なしに成功する(CLAUDE.md のサンドボックス制約に整合)。

## L2 — Detail

per-crate のシームのみを定義する(コードは書かない)。

### noa-theme(新規 crate)

- 配置: `crates/noa-theme/`。ワークスペースメンバーに追加。`Cargo.toml` の依存は `noa-core` のみ。
- 公開型: `ThemeDef`(`name: &'static str`、`default_fg`/`default_bg`/`cursor`/`selection_fg`/`selection_bg`: `Rgb`、`palette: [Rgb; 256]`)。selection/cursor の反転導出(R-8)は **codegen 時点で確定値化**する — `scripts/gen-themes` が導出ロジックを一箇所で実行し、`ThemeDef` は常に具体値のみを持つ(`Option` を持たない)純データにする。
- 公開関数: `pub fn resolve(name: &str) -> Option<&'static ThemeDef>`。実装はソート済み静的配列 `&'static [(&'static str, ThemeDef)]` への `binary_search_by` とし、新規クレート依存(`phf` 等)は追加しない。
- 生成物: `crates/noa-theme/src/generated.rs`(コミット対象、`scripts/gen-themes` が上書き生成)。`src/lib.rs` は `ThemeDef` 型定義と `resolve` 実装のみを持ち、`mod generated;` で取り込む。
- vendor 配置: `crates/noa-theme/vendor/themes/*.conf`(Ghostty ネイティブ構文、ファイル名 = テーマ名)+ `crates/noa-theme/vendor/ATTRIBUTION.md`(upstream リポジトリ・固定コミット SHA・取得日・ライセンス所在)。
- 未対応キー(`cell-foreground`/`cell-background` 等 1.2.0+ 特殊値を含む)は codegen 時に無視する(forward-compat skip、生成失敗にしない)。対応要否自体は Open Questions のまま未確定(本 L2 は「無視して壊れない」ことだけを定める)。

### noa-config

- `ConfigOverrides`/`StartupConfig` の `pub theme: Option<String>` は維持し、Ghostty 構文パーサーの `theme` 分岐で設定する(CLI 側は R-12 によりテーマ値を持たないため、`merge` の CLI 引数側は常に `theme: None`)。
- `theme` のパースは ghostty-config 増分の `parser.rs` に統合する:
  - `theme = 3024 Day` と `theme = "3024 Day"` を等価に受理する。
  - 値が `light:` または `dark:` で始まる場合、R-2 専用の diagnostic を返し、`ConfigOverrides.theme` は `None` のままにする。
  - **カタログに対する名前存在チェックはここでは行わない**(`noa-config` は `noa-theme` に依存しない設計を維持する)。存在チェック + フォールバックは `noa-app` 層の責務。
- 既存 TOML 前提の `SUPPORTED_KEYS`/`reject_unknown_keys`/`toml_edit`/`parse_theme(path, document)` は ghostty-config 増分で削除済み前提とする。
- テスト: `theme_key_is_accepted`(クォートあり/なしの有効テーマ名を受理)、`light_dark_syntax_is_rejected`(R-2 の diagnostic 検証)、未知キーは hard error ではなく warn+継続であることを検証する。

### noa-grid

- `TerminalColors`(osc.rs)に、既存の動的上書き用 `Option<Rgb>` フィールド群とは別に、テーマ由来のベース色フィールドを追加する(`base_fg: Rgb`、`base_bg: Rgb`、`base_cursor: Rgb`、`base_palette: [Rgb; 256]`)。`Default` 実装は現行の `DEFAULT_FG`/`DEFAULT_BG`/`DEFAULT_CURSOR`/`xterm_palette()` を初期値にし、テーマ未注入時の既存挙動を完全維持する(NFR-4)。
- 追加 setter: `TerminalColors::set_base_colors(fg, bg, cursor, palette)`(加算的・非破壊)。`Terminal` 側にも同名の薄いパススルー `Terminal::set_base_colors(..)` を追加し、`Terminal::new(GridSize)` 自体のシグネチャには触れない。
- `query_default_fg`/`query_default_bg`/`query_cursor`/`query_palette`(osc.rs:97-112)のフォールバック参照先を、ハードコードされた `DEFAULT_FG`/`DEFAULT_BG`/`DEFAULT_CURSOR`/`xterm_palette_color(index)` から `self.base_fg`/`self.base_bg`/`self.base_cursor`/`self.base_palette[index]` に差し替える(R-10)。OSC 104/110/111/112 のリセットハンドラ自体は変更不要 — `Option` を `None` に戻すだけで、フォールバック先の変更により自動的にテーマ相対になる。
- `full_reset`(terminal.rs:354-364, RIS 経路)は現状 `self.colors = TerminalColors::default()` によりベース色ごと初期化してしまう。ベース色を保持したまま動的上書き層のみをクリアする再構築(例: `TerminalColors::with_base(fg, bg, cursor, palette)` のような「ベース色保持リセット」コンストラクタ/メソッド)に置き換える(R-11)。

### noa-app

- `AppConfig`(app.rs:37)に `pub theme: Option<String>` を追加。
- `bin/noa/src/main.rs`: `noa_config::load_startup_config` から得た `theme` を `noa_app::AppConfig` へ橋渡しする(CLI フラグは追加しない、R-12)。
- `crates/noa-app/src/theme.rs` の `default_theme()` を `resolve_theme(name: Option<&str>) -> Theme` へ拡張する:
  - `name` が `None` → 現行 `Theme::default()`。
  - `name` が `Some` かつ `noa_theme::resolve` がヒット → `ThemeDef` から `noa_render::Theme` を構築。
  - `name` が `Some` かつ未ヒット → `log::warn!`(R-3)+ `Theme::default()` へフォールバック。
- `GpuState.theme` の構築箇所(app.rs:283)を `crate::theme::default_theme()` から `crate::theme::resolve_theme(config.theme.as_deref())` に差し替える。
- 各タブ/ウィンドウの `Terminal` 生成箇所で、生成直後に `terminal.set_base_colors(theme.default_fg, theme.default_bg, theme.cursor, theme.palette)` を呼び出し、grid のベース色をシードする(noa-grid 側 setter の呼び出し元)。
- `crates/noa-app/Cargo.toml` に `noa-theme` を新規依存として追加する(DAG 上は `noa-app` が最上流であり、`noa-theme` 自体に GUI 依存は増えないため既存の依存規則に抵触しない)。

### scripts/gen-themes

- `scripts/gen-icon.sh` と同型の Bash スクリプト(`set -euo pipefail`、再実行安全、破壊的操作なし)。
- 入力: `crates/noa-theme/vendor/themes/*.conf` + `crates/noa-theme/vendor/ATTRIBUTION.md`。
- 処理: 各テーマファイルを Ghostty ネイティブ構文でパースし(認識キー: `background`/`foreground`/`cursor-color`/`cursor-text`/`selection-background`/`selection-foreground`/`palette = N=#rrggbb`。未知キーは無視)、R-8 の反転導出を適用し、名前でソート済みの静的配列としてシリアライズする。
- 出力: `crates/noa-theme/src/generated.rs`(コミット対象)。
- ネットワークアクセスなし(vendor ファイルの取得・更新は別途の手動/一回限りの vendor 更新手順とし、本スクリプトの実行時要件に含めない、NFR-7)。
- `ATTRIBUTION.md` 欠如時は非ゼロ終了する(NFR-6)。

## L3 — Acceptance Criteria

各 AC は対応する `R-*`/`NFR-*` を明記する(`AC-n → R-m` 形式)。Ripple 必須テスト3件は AC-1+AC-2(①)・AC-14(②)・AC-17(③)で満たす。実装完了後の Attest フェーズでの独立検証を推奨する。

### 設定パース

- **AC-1 → R-1**: Given config に `theme = 3024 Day` または `theme = "3024 Day"` のみが書かれている。When `noa_config::parse_overrides` を実行する。Then diagnostics が空で `ConfigOverrides.theme == Some("3024 Day".to_string())` を返す。
- **AC-2 → R-1(Ripple 必須テスト①)**: Given config に真に未サポートなキー(例: `bogus-key = x`)と後続の有効キーが書かれている。When `parse_overrides` を実行する。Then error にならず、diagnostics にファイルパス・`bogus-key` を含む warn が1件入り、後続キーのパースは継続する。
- **AC-3 → R-2**: Given config に `theme = light:Foo,dark:Bar` が書かれている。When `parse_overrides` を実行する。Then error にならず、`light:`/`dark:` ペア構文が未対応である旨の専用 diagnostic が1件生成され、`ConfigOverrides.theme == None` になる(汎用 unknown-key/invalid-value diagnostic とは異なる文言であることを確認できる)。
- **AC-4 → R-3**: Given カタログに存在しないテーマ名。When `resolve_theme(Some("NoSuchTheme"))` を呼ぶ(単体テスト、GPU/ウィンドウ文脈不要)。Then エラーにならず `Theme::default()` と等価の `Theme` を返す。加えて、フォールバック経路上に `log::warn!` 呼び出しが存在することをコード検査(grep)で確認する(ログ捕捉ハーネスは導入しない)。

### noa-theme クレート・カタログ

- **AC-5 → R-4**: Given `crates/noa-theme` がワークスペースメンバーである。When `cargo tree -p noa-theme --offline` を実行する。Then 依存グラフに `noa-core` 以外の noa クレート・`wgpu`・`winit` が含まれない。
- **AC-6 → R-5, NFR-6**: Given `crates/noa-theme/vendor/ATTRIBUTION.md` が存在する。When 内容を検査する。Then upstream リポジトリ名・固定コミット SHA・取得日・ライセンス所在の4項目が記載されている。
- **AC-7 → R-6, NFR-7**: Given `crates/noa-theme/src/generated.rs` がリポジトリにコミットされ、`build.rs` が存在しない。When `cargo build --workspace --offline` を実行する。Then ビルドが成功し、`generated.rs` は再生成されず、ネットワークアクセスも発生しない。

### テーマ解決・忠実度

- **AC-8 → R-7**: Given `theme = "<known-name>"` が設定されている。When `resolve_theme` が `noa_render::Theme` を構築する。Then `default_fg`/`default_bg`/`cursor`/`selection_fg`/`selection_bg`/`palette` の 6 フィールドすべてが、対応する `ThemeDef` のフィールド値と厳密に一致する(偶然デフォルト値と一致するフィールドがあっても偽陰性にならない比較にする)。
- **AC-9 → R-7, NFR-1(スポットチェック、必須)**: Given vendor 済みテーマから任意に選んだ3件以上の既知テーマ(実際に vendor される正式名称に読み替える)。When 各テーマ名を `resolve_theme` に渡し、結果の `default_fg`/`default_bg`/`cursor`/`palette[]` を対応する vendor 済み Ghostty テーマファイル中の16進値と突合する。Then 全フィールドがバイト単位(文字列比較)で完全一致する。
- **AC-10 → R-8**: Given `selection-background`/`selection-foreground` を含まない vendor 済みテーマファイル。When `scripts/gen-themes` が `ThemeDef` を生成する。Then `selection_bg == default_fg` かつ `selection_fg == default_bg`(反転フォールバック)。
- **AC-11 → R-8**: Given `cursor-color` を含まない vendor 済みテーマファイル。When `scripts/gen-themes` が `ThemeDef` を生成する。Then `cursor == default_fg`。

### grid ベース色伝播

- **AC-12 → R-9(回帰ガード)**: Given `TerminalColors::default()`(ベース色未注入)。When `query_default_fg`/`query_default_bg`/`query_cursor`/`query_palette` を呼ぶ。Then 変更前と同じ値(`DEFAULT_FG`/`DEFAULT_BG`/`DEFAULT_CURSOR`/`xterm_palette_color`)を返す。
- **AC-13 → R-9**: Given `Terminal::set_base_colors(fg, bg, cursor, palette)` が呼ばれた `Terminal`。When 動的 OSC 上書きが一切行われていない状態で内部の `TerminalColors` を検査する。Then ベース色フィールドが注入した値と一致する。
- **AC-14 → R-10(Ripple 必須テスト②)**: Given アクティブテーマの `default_bg` が noa のハードコードデフォルトと異なる色であり、OSC 11 の動的上書きが行われていない。When pty からのバイト列として OSC 11 クエリ(`\x1b]11;?\x1b\\`)を処理する。Then `take_pending_writes()` が返す応答がアクティブテーマの `default_bg` を報告する(ハードコード xterm デフォルトではない)。
- **AC-15 → R-10**: Given 同条件で OSC 10(default_fg)・OSC 12(cursor)についても動的上書きなし。When それぞれのクエリを処理する。Then 応答がアクティブテーマの `default_fg`/`cursor` を報告する。
- **AC-16 → R-11**: Given OSC 11 で `default_bg` を一時的に上書きした後、OSC 111(リセット)を送る。When 続けて OSC 11 クエリを送る。Then 応答はハードコード xterm デフォルトではなく、アクティブテーマの `default_bg` を報告する。
- **AC-17 → R-11(Ripple 必須テスト③)**: Given OSC 4 でパレットの一部を上書きし、OSC 11 で `default_bg` を上書きした状態。When RIS(`ESC c`)/`full_reset` を実行する。Then 以降の OSC 4/10/11/12 クエリはすべてアクティブテーマのベース色(パレット含む)を報告し、ハードコード xterm デフォルトには戻らない。

### 優先順位・依存・品質

- **AC-18 → R-12**: Given `bin/noa/src/main.rs` の `Args`(clap 定義)。When `noa --help` を実行する、またはソースを検査する。Then `--theme` フラグが存在せず、テーマの入力元は config ファイルの `theme` キーのみである。
- **AC-19 → NFR-2**: Given `noa_theme::resolve` の実装。When 実装を検査する。Then 実行時のファイル I/O・ネットワークアクセスが一切ない(静的配列への `binary_search_by` のみ)ことをコードレビューで確認する。参考値として、ルックアップ1回のコストが起動シーケンス全体に対して無視できる水準(目安 <1ms)であることをマイクロベンチマークで健全性チェックする(必須合否基準ではなく参考指標)。
- **AC-20 → NFR-3**: Given `crates/noa-theme` と `crates/noa-config` の `Cargo.toml`。When `cargo tree -p noa-theme --offline` と `cargo tree -p noa-config --offline` を実行する。Then いずれの依存グラフにも `wgpu`/`winit` が現れない。
- **AC-21 → NFR-4**: Given 変更後の `noa-grid::Terminal`。When `fn new(size: GridSize) -> Self` のシグネチャと既存 21 箇所の呼び出しサイト(git diff)を確認する。Then シグネチャ・呼び出しサイトのいずれにも差分がない(ベース色注入は `Terminal::new` 後の追加呼び出しとして実装されている)。
- **AC-22 → NFR-5(必須)**: Given 本変更を適用したワークスペース。When `cargo test --workspace --offline` と `cargo clippy --workspace --offline` を実行する。Then 両方とも exit code 0 で完了し、変更によって新規追加された `#[allow(...)]` が存在しない。
- **AC-23 → NFR-6**: Given `crates/noa-theme/vendor/ATTRIBUTION.md` が存在しない状態。When `scripts/gen-themes` を実行する。Then 非ゼロ終了コードで失敗し、マニフェスト欠如を示すメッセージを出す。
- **AC-24 → NFR-7**: Given `scripts/gen-themes`。When (a) ネットワーク遮断サンドボックス内でスクリプトを実行し、(b) スクリプト本文を `curl`/`wget`/`git fetch`/`git clone`/`nc`/`scp`/`ssh` について grep する。Then (a) が正常終了し、(b) でネットワークアクセスを行う命令が一切検出されない(deny-list grep 単独ではなくサンドボックス実行を正とする)。

### トレーサビリティ・サマリ

| 要件 | AC | 要件 | AC | 要件 | AC |
|---|---|---|---|---|---|
| R-1 | AC-1, AC-2 | R-8 | AC-10, AC-11 | NFR-2 | AC-19 |
| R-2 | AC-3 | R-9 | AC-12, AC-13 | NFR-3 | AC-20 |
| R-3 | AC-4 | R-10 | AC-14, AC-15 | NFR-4 | AC-21 |
| R-4 | AC-5 | R-11 | AC-16, AC-17 | NFR-5 | AC-22 |
| R-5 | AC-6 | R-12 | AC-18 | NFR-6 | AC-6, AC-23 |
| R-6 | AC-7 | NFR-1 | AC-9 | NFR-7 | AC-7, AC-24 |
| R-7 | AC-8, AC-9 | | | | |

対象要件 19件(R-1〜R-12, NFR-1〜NFR-7)すべてに ≥1 件の AC が対応(トレーサビリティ完全性 19/19 = 100%)。

## Scope

### 問題

noa は現在ハードコード単一テーマのみで配色変更ができない。Ghostty から乗り換える dotfiles 駆動ユーザーは、config に `theme = <name>` と書くだけで手元の Ghostty 配色(bg/fg・カーソル・選択色・ANSI 256 パレット)がそのまま再現され、vim/tmux 等 TUI が意図した色で表示されることを期待している。この期待が満たせないと乗り換え離脱の主因になる。

### 提案する解決策

Ghostty が生成配布するテーマファイル(iTerm2-Color-Schemes 由来 `ghostty-themes.tgz`)を vendor し、コミット済み codegen(`scripts/gen-themes` → 新 crate `noa-theme` 内の静的テーブル)としてリポジトリに固定する。`noa-config` に `theme = <name>` キーを追加し、起動時に名前解決 → `noa_render::Theme` を構築(レンダラのシームはゼロ)。さらに解決済みテーマの base 色(bg/fg 等)を `noa-grid` の `TerminalColors` に伝播し、OSC 10/11/12 のクエリ応答と OSC 104/110-112 のリセットをアクティブテーマ相対にする。未知のテーマ名は warn を出しデフォルトへ fallback する(hard fail しない)。テーマファイルに selection/cursor 色が無い場合は Ghostty 同様の反転導出で埋める(codegen 時に確定値化)。

### In-scope

- `noa-config`: Ghostty 構文で `theme = <name>` キーを v1 認識スカラーキーとして継続受理する
- 新規 crate `noa-theme`(`noa-core` のみ依存、純データ)
- `scripts/gen-themes`: vendor 済みテーマファイル → `noa-theme` 内静的 Rust テーブルを生成し、生成物をコミット(build.rs なし)
- vendor 対象: Ghostty 配布テーマファイル一式(~460 件スナップショット)+ upstream コミット固定 + 帰属マニフェスト 1 枚
- アプリ配線: 起動時に `theme` を解決し `noa_render::Theme` を構築(`GpuState.theme` 経由、全タブ共有)
- 未知テーマ名時の warn + デフォルト fallback
- selection/cursor 未設定テーマに対する反転色フォールバック導出
- grid base 色シード: `TerminalColors` に base 色フィールドを追加し、`Terminal::new` を非破壊で拡張(既存 21 呼び出し箇所に影響なし)
- Ripple 必須テスト 3 件: ① `theme` キー受理 / 未知キー拒否、② OSC 11 クエリがアクティブテーマの bg を報告、③ RIS/`full_reset` がテーマ相対に復元
- `noa-config` の未知キー検証を、ghostty-config の warn+継続 semantics と整合させる

### Out-of-scope

- `light:X,dark:Y` 構文および macOS 外観自動切替 — 「構文だけ入れて切替なし」は禁止(silent fidelity divergence)。**明示的スコープ外**とし、当該構文が渡された場合は明確なエラーで拒否する(半端な受理は不可)。忠実度ギャップとして文書化。
- ランタイム config リロード(テーマ変更は起動時適用のみ)
- `+list-themes` 相当の CLI サブコマンド
- `--theme` CLI フラグ
- config dir `themes/` ユーザーファイルのルックアップ(将来増分で追加可能な構造は維持するが v1 実装はしない)
- GUI テーマエディタ・カラーピッカー

### 前提(Assumptions)

- vendor するテーマファイルは Ghostty config と同一構文のみをパース対象とする(config 全文法のパーサーは実装しない)
- OSC 104 リセットの復元先がテーマ相対であることは PARTIAL 検証(推定)— 実装前提として採用(Open Questions 参照)
- 「~460 テーマ」は取込時点のスナップショット数であり、upstream 更新により変動しうる(コミット固定で件数を凍結)
- selection/cursor 色の反転導出は Ghostty のドキュメント記載挙動("If this is not set, then the selection color is inverted")を踏襲し、codegen 時に確定値化する

## Considered but rejected

ユーザー選択(EXPAND checkpoint, 2026-07-02): **方向 B(フルカタログ取込)を採択**(取込機構は CHALLENGE でコミット済み codegen に裁定)。

- **A. 最小静的スライス(手動移植 20-30 テーマ)** — 却下: README inc-4「~460 themes」公約に未達。手動移植はカタログ増分時に捨てコストになる。
- **C. デュアルルックアップ(`themes/` ユーザーファイル)** — 却下(v1): Ghostty の実機構ではあるが、v1 は同梱カタログで JTBD①を満たす。後増分で追加可能な構造は維持する。
- **D. light/dark 自動切替中心** — 却下(v1): macOS 外観フック + 実行時再テーマが v1 必須依存になる。「構文だけ入れる」は禁止 → `light:X,dark:Y` は明示的スコープ外として文書化。
- **build.rs codegen(取込機構の対立評決, Magi conf 85)** — 却下: リポジトリに build.rs 前例ゼロ、オフラインサンドボックスでのネットワーク footgun、既存 `scripts/gen-icon.sh` と同型のコミット済み codegen を採択(Ripple 案)。
- **grid 伝播の DEFER(対立評決, Void conf 75)** — 却下: OSC 11 クエリによる vim/tmux の背景自動検出(JTBD⑤)が誤動作するため v1 に含める。修正は安価・非破壊。

## 確定裁定(LOCK 時にユーザー承認)

1. 取込機構 = **コミット済み codegen**(`scripts/gen-themes` 生成物をコミット、build.rs なし)
2. grid へのテーマ伝播 = **v1 に含める**(R-9〜R-11)
3. `+list-themes` = **DEFER**(次増分候補)
4. `--theme` CLI フラグ = **DEFER**(次増分候補)

## Open Questions / Deferred Decisions

- 帰属表記の具体的要件(ライセンス文言・配置場所)— vendor 取込時に iTerm2-Color-Schemes の LICENSE を確認して確定(NFR-6 の 4 項目は最低要件)
- 同梱テーマの正確な件数 — 取込時に確定し ATTRIBUTION.md に記録
- OSC 104/110-112 のテーマ相対復元の意味論を Ghostty ソースで断定検証(現状 PARTIAL — discussions/12708 の override/default 二層構造が傍証)。実装時に乖離が判明した場合は R-11 を Ghostty 実挙動に合わせて修正
- `cell-foreground`/`cell-background`(Ghostty 1.2.0+ 特殊値)の v1 サポート要否 — codegen は無視(壊れない)ことのみ確定済み
- config ファイルパスは ghostty-config 増分で `<config_dir>/noa/config`(拡張子なし)に変更済み。旧 `config.toml` は検出 warn のみで読み込まない。
- 将来増分(deferred): `light:X,dark:Y` + macOS 外観フック / ランタイムリロード / `themes/` ユーザーファイルルックアップ / `+list-themes` / `--theme`

## Build-path decision

**orbit loop(engine: codex)** — サインオフ時選択(2026-07-02)。

- 本スペックの AC-1〜AC-24 を loop の完了契約(machine-checkable DONE ゲート)とする。AC-22(`cargo test`/`clippy` green)が各イテレーションの検証コマンド、AC-9(バイト一致スポットチェック)が忠実度ゲート。
- 実行エンジン: **Codex CLI**(各イテレーションを codex が実行)。前提: `~/.codex/config.toml` で `multi_agent = true` + `[agents] max_depth >= 2`。既存の `.nexus/loops/*`(`exec-codex.sh`)と同型の運用。
- vendor 取込(ネットワークを伴う一回限りの工程)は loop 外の手動/事前ステップとして分離すること(NFR-7)。
- ハンドオフ先: `orbit` エージェント(`~/.claude/skills/orbit/SKILL.md`、engine 詳細は `orbit/reference/executor-engines.md`)。**本 spec はコードを書かない — loop の生成・起動は別途指示で実行。**
