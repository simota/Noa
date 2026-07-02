# Spec: Ghostty 設定ファイル読み込み (ghostty-config)

## Metadata

- slug: `ghostty-config`
- title: Ghostty 設定ファイル読み込み (Ghostty Config File)
- status: **locked**(サインオフ 2026-07-03)
- owner: simota
- build-path: **orbit loop(engine: codex)**
- recipe: /nexus spec — FRAME ✓(問題文承認 2026-07-02: 対象=両方 / TOML=置き換え / キー範囲=構文基盤+既存キー相当)/ EXPAND ✓(方向 B 単独採択 + 意味論まで実装)/ CHALLENGE ✓(コンセンサス 10 件 + ⚠A-D)/ SHAPE ✓(Spark)/ SPECIFY ✓(Accord, 現物コード再検証)/ Quality Gate ✓(Run 1 FAIL → 改修 → Run 2 PASS)/ LOCK ✓(2026-07-03: ⚠A-E 確定 + サインオフ + build-path 選択)

## L0 — Vision

1. **対象**: Ghostty からの乗り換えを想定した dotfiles 駆動ユーザー + noa 自体の設定利用者。
2. **ジョブ**: Ghostty ネイティブ構文(行指向 `key = value`)の設定ファイルを noa が読み込み、起動設定として利用できる。Ghostty config 資産(dotfiles)がそのまま流用できる。
3. **成功条件**: noa ネイティブ config(Ghostty 構文)が読める + noa config が無い場合は Ghostty の config パスから読める(「両方」裁定)。未知キーは Ghostty 同様 warn + ignore。
4. **スコープ境界**: v1 は構文基盤 + 既存キー相当 = スカラー4キー(window-width/window-height/font-size + theme〔⚠E 採用 2026-07-03: theme-selection 完走により出荷済みキーに昇格〕)。list 型キー拡張(keybind/font-family/palette…)は後続増分・別 spec。
5. **制約**: 既存 TOML config は**廃止・置き換え**(後方互換負債なしの今が移行適期)。`noa-config` の機構(パス発見・precedence default < file < CLI・検証)は再利用、パーサーのみ差し替え。

### FRAME 裁定 (2026-07-02, ユーザー確認済)

| 論点 | 裁定 |
|------|------|
| 「Ghostty の設定ファイルを読む」の解釈 | **両方** — noa ネイティブ config (Ghostty 構文) を主とし、無い場合は Ghostty の config パスへフォールバック(またはインポート — 方式は EXPAND/CHALLENGE で決定) |
| 既存 TOML config | **置き換え**(TOML 廃止、パーサー一本化。theme-selection ドラフトは新形式前提に要改稿) |
| v1 キー範囲 | **構文基盤 + 既存キー相当**。未知キーは warn + ignore(Ghostty 挙動)。キー拡張は別増分 |

### Reuse / constraint findings (Lens reuse-scan, 2026-07-02)

- **`noa-config` crate 既存**(deps: anyhow/dirs/toml_edit、GUI-free)。パス発見 `default_config_path()`(lib.rs:77-79 → macOS は `~/Library/Application Support/noa/config.toml`)、precedence モデル `load_file_overrides()?.merge(cli).apply_to(default)`(lib.rs:59-65)、検証は再利用可。**TOML パーサー部(lib.rs:92-170)は Ghostty 構文と非互換 → 差し替え**。
- **設定の流路は一本道**: `bin/noa/src/main.rs:21-30`(clap → load_startup_config → AppConfig)→ `noa-app/src/app.rs`(cols/rows → app.rs:214/327-332、font_size → app.rs:222/246/605)。新キーはこの鎖に field 追加で通る。
- **`noa-app` は `noa-config` に依存しない**(バイナリが唯一のブリッジ)— この境界は維持する。
- **未知キーの扱いが真逆**: 現行 `reject_unknown_keys` は hard fail(lib.rs:120-131)、Ghostty は warn + ignore。テスト `unknown_key_is_rejected`(lib.rs:291-296)は書き換え必至(theme-selection ドラフトと同じ指摘)。
- **keybind トリガーパーサー実装済み**(noa-app/src/commands.rs:299-325、`cmd+shift+f` 形式)— 将来の `keybind =` キーの受け皿。
- **String 値キー追加で `Copy` derive 喪失**(StartupConfig/ConfigOverrides)— theme-selection ドラフト BLOCKER-1 と同一の波及。パーサー置き換え時に一緒に処理するのが合理的。
- **関連計画**: parity-plan Phase 3「設定システム拡張」(Ghostty キーセット + live reload Cmd+Shift+,)、Phase 4「config file」。README inc-3/4。**theme-selection.draft.md(SPECIFY 段階)は TOML 前提 — 本 spec の裁定により R-1/AC-1〜3 の改稿が必要**(相互作用として記録)。
- **live reload は未配線**(全消費者が起動時一回)。grid resize 経路(io_thread.rs:125)は存在するが window resize 用。v1 スコープ判断は CHALLENGE で。

## EXPAND — 候補方向 (Riff ‖ Flux ‖ FACT検証, 2026-07-02)

### FACT 検証結果 (Ghostty 1.3.1 実挙動, 出典: ghostty.org docs / ghostty-org/ghostty)

- **VERIFIED — パス解決は「4候補の全読みマージ」**: ①`$XDG_CONFIG_HOME/ghostty/config.ghostty` ②同 `config` ③`~/Library/Application Support/com.mitchellh.ghostty/config.ghostty` ④同 `config` をこの順で全て読み、**後勝ちで上書きマージ**(first-wins ではない)。どれも無ければエラーなしで組み込みデフォルト。`.ghostty` 拡張子は 1.2.3+。noa 既存の `find_first_existing_config_path` は先勝ち単一選択で**セマンティクスが異なる**。
- **VERIFIED — 構文**: `key = value`、`=` 前後の空白無視。`#` コメントは**行頭のみ**(トレーリング不可)。クォートは任意(`?` 接頭辞のリテラル指定等でのみ必須)。**空値 `key =` はデフォルトへのリセット**(リスト型は蓄積クリア)。キーは小文字 kebab-case、大文字小文字区別。
- **PARTIAL — 重複キー**: スカラーは last-wins、`keybind`/`palette`/`font-family`/`config-file` 等リスト型は蓄積(append)。公式の明示的一覧は無し(個別リファレンス+man page から裏付け)。
- **VERIFIED — `config-file` include**: リスト型・相対パスは記述ファイル基準・`?` 接頭辞で optional・**循環はエラー("cycle detected")**・**処理はファイル末尾**(後続キーが include 先を上書きできない遅延評価)。
- **PARTIAL — エラー処理**: 未知キー("unknown field")・不正値("invalid value")は Diagnostic として蓄積され**パース継続**(致命は OOM のみ)。起動時に GUI「Configuration Errors」ダイアログ + stderr ログで表示。hard fail しない。
- **VERIFIED — `window-width`/`window-height`**: グリッドセル数(整数)で**新規ウィンドウの初期サイズのみ**。**両方セットしないと無効**。最小 10×4 クランプ。**`cols`/`rows` キーは Ghostty に存在しない**。`window-save-state` との優先関係は PARTIAL(復元時は復元値優先と推定)。
- **VERIFIED — `font-size`**: float 可(points)。最近傍整数ピクセルに丸め。
- **VERIFIED — CLI 対応**: **全 config キーが CLI フラグ**(`--font-size=14`)。CLI > file。`--config-file` は追加読み(デフォルト探索は止まらない)、`--config-default-files=false` でデフォルト探索スキップ。
- **PARTIAL — reload**: `reload_config` デフォルトは macOS `cmd+shift+,` + `SIGUSR2`。live 適用可否はキー毎(一律ルール無し)。

### 候補方向 (Riff)

**A. Fast Path — 最小パーサー in-place 差し替え + ライブフォールバック** — `noa-config` のパーサー部のみ line-based 最小実装に置換(discovery/precedence/validation 温存)。noa config 無ければ Ghostty パスをその場で読む。繰り返しキー・include・空値リセットは v1 未実装。最小差分だが実 dotfiles は「読めるが意味的に不完全」になり、後続増分で構文機構の手戻りリスク。

**B. Faithful Import — 深い構文パーサー + ワンショットインポート** — 主要意味論(繰り返しキー蓄積・空値リセット・include 再帰/循環検出)を最初から実装。Ghostty config はライブでは読まず、明示的 import(対応キーだけ noa config へ書き出し、未対応キーはコメントアウトで可視化)。構文手戻りゼロ・設定出所が単一収束。v1 3キーには先行投資が重く、import を叩かないと「両方」体験が弱い。

**C. Include-Based Bridge — `config-file` ディレクティブ統合** — フォールバック専用経路を作らず、include 機構を実装して「noa config 不在時は仮想的に `config-file = ~/.config/ghostty/config`(optional)を先頭に置く」一本化。ユーザーも任意に Ghostty 資産を混ぜ込める。ただし「暗黙 include」は Ghostty に無い noa 独自合成挙動で、忠実クローン方針との整合説明が必要。

**D. Clean Room Parser — 純粋関数パーサー分離 + パス解決忠実**(A〜C/E と直交する実装選択) — パーサーを `&str -> Vec<Directive>` の I/O レスな純粋関数として分離し、`noa-config` が消費。unit test がファイル I/O 抜きで書ける。Ghostty の 4 候補パス全読みマージ規則も忠実再現。

**E. Layered Merge — フォールバックではなく常時マージ** — 既存 `ConfigOverrides::merge` に層を1つ足し、`ghostty_file.merge(noa_file).merge(cli)` と常時レイヤー合成。noa 側は差分だけ書けば済む。ただし FRAME 裁定「無い場合はフォールバック」から逸脱し、2ファイル precedence のデバッグ体験が悪化。Ghostty に無い概念。

### Flux による前提チャレンジ(方向横断)

1. [FACT→VERIFIED] 「Ghostty のパス」は単数ではない — 4候補全読み後勝ちマージ。フォールバックの対象パス集合とマージ順を spec で明示必須。
2. [FACT→VERIFIED] `cols`/`rows` ≒ `window-width`/`window-height` は表面一致 — Ghostty 側は「両方必須・window-save-state と結合・10×4 クランプ」。noa にウィンドウ状態復元は無い → fidelity gap として明記要否を決定。
3. [DESIGN] 「warn+ignore」は一枚岩でない — 未知キー warn / 型不正 warn / theme ファイル内禁止キー silent の少なくとも3段階。v1 でどのエラークラスまで非致命化するか決めないと「未知キーは warn、型不正は起動中断」の中途半端な忠実度になる。
4. [DESIGN] 生の Ghostty config をライブで読むと、noa 未対応キー数十個が**起動のたびに warn 洪水**になる。ライブフォールバック採用なら Ghostty ファイル由来の未対応キー警告の抑制方針が必要。
5. [DESIGN] 「他アプリの config を読む」は Ghostty の観測可能挙動ではない — **noa 独自 convenience 拡張**。theme spec の `light:dark` 先例に倣い「fidelity ではなく noa 拡張」と明記する場所を決める。
6. [DESIGN] TOML 廃止の移行 UX 未定義 — 既存 config.toml が無言で無視されると「起動サイズが勝手に変わった」リグレッション報告を招く。検出 warn / 自動変換 / 何もしない、の明示的決定が必要。
7. [FACT→VERIFIED] Ghostty は全 config キーを CLI フラグとして受ける。noa 既存 `--cols`/`--rows` は Ghostty に対応物が無い → 改名 or noa 独自名維持 or 併存を決定しないと config/CLI で同一概念に別名が生じる。
8. [FACT→VERIFIED] `config-file` は「ファイル末尾で処理」の非直感的規則。v1 で未実装でも、遭遇時の挙動(無視/warn/エラー)を今決めないと、後続増分で有効化した瞬間に沈黙的動作変化が起きる。
9. [DESIGN] scalar(last-wins)/list(append+空値リセット)の 2 種セマンティクス。v1 の 3 キーは全て scalar だが、「構文基盤」を謳うならパーサーが list 構文・空値リセットに遭遇したときの挙動を今決めておかないと次増分でパーサー再設計になる。

### EXPAND チェックポイント結果 (2026-07-02, ユーザー裁定)

- **方向 B(Faithful Import)を単独採択** — A/C/E は CHALLENGE に持ち込まず却下(Considered but rejected へ)。D(純粋関数パーサー分離)は直交軸として CHALLENGE で扱う。
- **構文忠実度: 意味論まで v1 実装** — 空値リセット・scalar last-wins・list 蓄積構造・行頭コメント規則を正しく実装。`config-file` は「認識して warn(未実装を明示)」。

## CHALLENGE — 評決と裁定 (Magi + Void + Ripple, 2026-07-02)

### コンセンサス裁定(3 エージェント一致 — ADOPTED)

1. **パーサー配置 = 純粋関数分離(D 軸採用)**: パーサー本体を `&str -> Result<Vec<Directive>, ...>`(I/O レス)として分離し `noa-config` が消費。既存 `parse_overrides(path, source)` の分離パターンと noa-vt の Handler/Stream 規範の踏襲。[Magi 3-0, conf 90]
2. **noa ネイティブパスは単一パス維持**: Ghostty の 4 候補マージは bundle-id 変更・`.ghostty` 拡張子移行という Ghostty 固有の歴史的遺産の解決機構であり、noa 自身に持ち込むのは過剰。既存 `default_config_path()` 系を温存、**ファイル名は `config.toml` と衝突しない新名(`config`)に変更**(TOML 廃止後の「静かにデフォルトへ戻る」regression を構造的に回避)。[Magi 3-0 conf 85 / Void CUT conf 90]
3. **Ghostty 側(読み元)の探索は 4 候補全読み・後勝ちマージを忠実実装**(import/参照が生きる場合)— ここは「Ghostty の観測可能挙動」の再現そのもの。[Magi 3-0, conf 85]
4. **未知キー = warn + 継続**: コピーされた実 dotfiles には未対応キーが確実に含まれるため、これがないと JTBD が入口で全滅(1 個の未知キーで起動不能)。生存条件。[Void conf 95]
5. **汎用 `--<key>=<value>` CLI は CUT**: 3 キーのための speculative generalization。`--cols`/`--rows` は改名せず維持(Ghostty に対応物の無い noa 独自キーを Ghostty 名に改名するのは誤誘導)。**`--font-size` は clap の kebab 化により既に Ghostty 名と一致**(Ripple 発見 — リネーム論点は実質消滅)。[Magi 3-0 conf 78 / Void conf 90]
6. **TOML 移行 = 検出 warn 一度だけ + 自動変換なし**: 旧 `config.toml` の存在を検出したら起動時に一度 warn。自動変換は「パーサー一本化」の L0 制約と矛盾するため実装しない。[Magi 3-0 conf 80 / Void 7a KEEP 75, 7b CUT 90]
7. **10×4 最小クランプは採用**(reject → clamp への意味論変更)、`window-save-state` 相互作用は対応不要(noa に該当機能なし)。[Magi 3-0 conf 80 / Void 6b CUT 90]
8. **`config-file` は認識して専用 warn**(汎用 unknown-key warn に埋没させない — typo と「認識済み未実装」をユーザーが区別可能に)。cycle detection・実読込は次増分。[Void KEEP 75]
9. **Diagnostic 蓄積は構造体の外に出す**: `StartupConfig`/`ConfigOverrides` に Vec を足さず `load_startup_config() -> Result<(StartupConfig, Vec<Diagnostic>)>` 形式で分離 → **`Copy` derive は v1 で維持**され、theme-selection の BLOCKER-1(Copy 喪失)は theme キー追加時まで発生しない依存順に保てる。Ghostty 風 Diagnostic 型・severity taxonomy の模倣は CUT(消費者たる GUI ダイアログが noa に無い)。[Ripple mitigation 2 / Void 4c CUT 80]
10. **list 蓄積の汎用データ構造は DEFER**(EXPAND 裁定の内側の絞り込み): v1 に list 型キーの消費者がゼロのため、list 型キー遭遇時は `config-file` 同様「認識して warn」で受け流し、実ストレージは最初の list 型キー実装増分で作る。scalar last-wins + 空値リセット + 行頭コメント規則は v1 完全実装(EXPAND 裁定通り)。[Void 8a CUT/DEFER 80 — ユーザー黙認で確定]

### 暫定裁定 ⚠ → **全件ユーザー確定(2026-07-03)**: ⚠A/⚠B/⚠C 下記の通り採択、⚠D は事象確定(theme-selection DONE → 本 spec が次増分 + theme spec 改稿)、⚠E(theme キー v1 認識)採択。

*(以下は暫定裁定時の記録 — 2026-07-02 チェックポイント提示時ユーザー離席)*

- **⚠ A = (b) フラグ + 初回ヒント**: `--import-ghostty-config` フラグで明示実行(非対応キーはコメントアウト書き出し)+ noa config 不在かつ Ghostty config 検知時に使い方ヒント 1 行表示。自動書き込みなし。[Magi #1 3-0 conf 88 + Ripple mitigation 5 を採択。Magi #2 多数派の自動 import と Void の全 CUT は対立評決として記録]
- **⚠ B = warn + 継続**: 型不正値は該当キーのみデフォルトにして起動継続(Ghostty 実挙動と一致、エラーモデルが一枚岩になる)。[Void 90 + Magi Logos 少数意見を採択。Magi 多数派 (fail-fast) は対立評決として記録]
- **⚠ C = config 層のみ「両方必須」採用**: config キー `window-width`/`window-height` は片方のみ指定で無効 + warn(Ghostty 意味論)。CLI `--cols`/`--rows` は noa 独自キーとして独立指定可を維持。[Magi 案と Void 案の統合]
- **⚠ D = ghostty-config 先行**: 本 spec 実装 → theme-selection.md の該当 7 節改稿(locked → 再ロック)→ theme の orbit loop 起動、の順。[Ripple mitigation 1]
  - **【2026-07-03 前提崩れ — SPECIFY 中に発見】** theme-selection の orbit loop は**既に起動・実装進行中**(noa-config に `theme` フィールド・`parse_theme`・テスト2件、bin/noa に転送ロジック、noa-app に `AppConfig.theme` が追加済み。`Copy` derive も既に喪失)。実質順序は **(b) theme-selection 先行**に逆転している。L1/L2 は現物コード基準で再整合済み(R-8)。
  - **【2026-07-03 追記】theme-selection loop は DONE(15/15 verified、.agents/PROJECT.md Orbit 行)**。theme 機能は TOML `theme = "name"` で**出荷済み**。→ 新論点 **⚠E**: 本 spec の R-8 のまま(theme = 未知キー扱い)だと、ghostty-config 実装〜theme 再改稿増分の間、**出荷済み theme 機能が動作しない期間**が生じる。`ConfigOverrides.theme` と下流配線は既存のため、Ghostty 構文パーサーで `theme` を v1 認識スカラーキー(文字列パススルー + `light:`/`dark:` ペア構文は専用 warn で不受理)にする追加コストはほぼゼロ。**推奨: theme を v1 認識キーに含め、機能リグレッションを回避**(採用時は R-8 を修正し、theme-selection.md の R-1/R-2/AC-1〜3 を Ghostty 構文前提に改稿)。LOCK 時に要ユーザー裁定。

### 対立評決の原案(記録)

- **⚠ A. import 機構の要否と形態**(FRAME「両方」裁定の実現方式):
  - (a) **初回起動時 自動 import**(非破壊・一度きり・noa config 不在時のみ)[Magi 2-1, conf 62 — Pathos/Sophia 多数派]
  - (b) **`--import-ghostty-config` フラグ + 初回起動ヒント表示**(自動書き込みなし)[Magi #1 3-0 conf 88 + Ripple mitigation 5]
  - (c) **import 自体を CUT** — 構文が同一なので `cp` + ドキュメント 1 行で代替可能 [Void conf 75-80]
  - 留意: (c) は FRAME「両方」裁定の実質縮小。また丸コピー運用は未対応キーの **warn 洪水**(Flux #4)を招くため、warn の集約表示(「N 個の未対応キーを無視(詳細は log)」)が対になる。
- **⚠ B. 型不正値(invalid value)の扱い**:
  - (a) v1 は fail-fast 維持(gap 明記)[Magi 2-1, conf 58 — log 埋没で実ミスに気づけない懸念]
  - (b) **warn + 継続(該当キーのみデフォルト)**[Void conf 90 + Magi Logos 少数意見 + FACT(Ghostty は継続)] — 「未知キーは warn だが型 typo は起動中断」の中途半端な忠実度(Flux #3 名指し)を避ける
- **⚠ C. `window-width`/`window-height`「両方セット必須」**:
  - (a) 不採用 — 独立設定可のまま(fidelity gap 明記)[Magi 3-0, conf 80]
  - (b) 採用 — VERIFIED の観測可能挙動・実装数行 [Void KEEP 80]
  - 統合案: **config キー層のみ Ghostty 意味論(両方必須)を採用し、CLI `--cols`/`--rows`(noa 独自)は独立のまま** — 忠実性と regression 回避の両立。
- **⚠ D. theme-selection.md(locked)との実行順序**(Ripple 最大リスク): theme spec の R-1/R-2・L2 noa-config 節・AC-1〜3・BLOCKER-1 は本 spec が削除する `toml_edit`/`SUPPORTED_KEYS` 機構前提。orbit loop 起動前に順序を確定しないと二重手戻り。
  - (a) **ghostty-config 実装先行** → theme-selection の該当 7 節を改稿(locked → 再ロック)してから orbit 起動
  - (b) theme-selection 実装先行(TOML のまま)→ 本 spec が後から theme キーごと移行

### Ripple 影響分析(要点)

- Risk 5.5/10 = **MEDIUM (Conditional Go)**。直接改修 2 ファイル(`noa-config/src/lib.rs` 全面書き換え 342 行 + `bin/noa/src/main.rs` 部分)+ workspace Cargo.toml(`toml_edit` 依存は noa-config が唯一の直接利用者で削除可能。ただし Cargo.lock からは muda 系ビルド依存経由で消えない — 誤解注意)。
- **noa-app は無変更**(AppConfig フィールドは cols/rows/font_size のまま、キー名マッピングは main.rs の 1 箇所で吸収)。
- 既存テスト 10 件中 **約 7 件が挙動レベル書き換え**(`unknown_key_is_rejected` は期待が真逆に反転、`invalid_file_value_*` はクランプ/非致命化裁定に依存)。3 件(defaults/merge/font_size NaN)は温存。
- 推定 400-600 行規模 → PR 分割推奨(パーサー部 / バリデーション部 / import 部)。
- 実装は本 spec の L1 確定後に着手(裁定 A〜D が影響範囲を上下させる)。

### CHALLENGE への持ち込み論点

1. **import の起動方式**: noa にサブコマンド基盤なし(bin は flags-only — theme spec で `+list-themes` DEFER の根拠になった同じ制約)。`--import-ghostty-config` フラグ? 初回起動時の自動 import? サブコマンド基盤新設?
2. **「両方」の体験設計**: import を叩かないと Ghostty 資産が使われない(B の弱点)。noa config 不在 + Ghostty config 存在時に自動 import + 通知?
3. **noa ネイティブパス方式**: Ghostty の4候補全読みマージを noa 版でも忠実再現(`~/.config/noa/config[.noa]` + App Support)? それとも単一パス?
4. **診断モデル**: Diagnostic 蓄積 + パース継続をどこまで再現するか。noa に GUI エラーダイアログ基盤なし → v1 は stderr/log のみ(fidelity gap 明記)?型不正値も非致命化するか。
5. **CLI フラグ**: `--cols`/`--rows` の扱い(改名 `--window-width` 等 / 独自名維持+内部マッピング / 汎用 `--<key>=<value>` 実装)。
6. **window-width/height 意味論**: 「両方指定必須・10×4 クランプ」を忠実採用するか(現行 noa は cols 単独指定可 — 挙動変更になる)。
7. **TOML 移行 UX**: 既存 config.toml 検出時に warn? import が TOML→新形式変換も担う? 何もしない?
8. **パーサー配置**(D 軸): noa-config 内モジュール vs 純粋関数分離。

## L1 — Requirements

*(SPECIFY — Accord, 2026-07-03。現物コード検証済み — theme-selection 増分の並行実装を反映)*

### 機能要件 (Functional)

**構文パーサー基盤**

- **R-1**: Ghostty 構文の行指向パーサーを、ファイル I/O を含まない純粋関数 `parse_directives(source: &str) -> Vec<Directive>` として実装する。v1 で確定する構文規則は以下の通り。
  - `key = value` の `=` 前後の空白は無視する。
  - `#` コメントは**行頭(先頭の空白は許容)のみ**有効。値の途中に現れる `#` はコメント開始とみなさず、そのまま値の一部として保持する(トレーリングコメント不可、FACT VERIFIED)。
  - `=` を含まない非空行は `Directive` を生成せず黙って読み飛ばす(v1 では診断も生成しない)。他行のパースは継続する。
  - クォートは任意。値の前後を `"..."` で正しく囲んでいる場合のみ剥がして中身を採用する。片側のみの `"`(閉じられていない)はクォートとして扱わず、`"` を含む文字列としてそのままリテラルに保持する(後段の数値パースで型不正となり R-7 の経路に入る)。値の内部に非エスケープの `"` を含む場合(例 `key = "ab"cd"`)も「正しく閉じたクォート」とみなさず、同様にリテラル保持する。
  - 行分割は `str::lines()` を基盤とし、CRLF 行末の `\r` は値の末尾から除去する。ファイル先頭の UTF-8 BOM は除去してからパースする。非 UTF-8 ファイルは Diagnostic ではなく I/O レーンのエラー(L2「2レーンのエラーモデル」参照)。
- **R-2**: スカラーキーの重複出現は **last-wins**。同一キーが複数回現れた場合、ソース上で最後に出現した値を採用する。
- **R-3**: `key =` のように `=` の後がホワイトスペースのみ(空文字含む)の場合、そのキーは「未指定」としてデフォルトへリセットする。一方、明示的にクォートされた空文字列 `key = ""` はリセットではなく、リテラルな空文字列値として扱う(v1 スカラーキーはすべて数値のため、この場合は型不正 → R-7 経路に入る)。

**キー分類・警告(未知キー / list 型 / config-file)**

- **R-4**: v1 の認識済みキー集合(スカラー4キー = `window-width`/`window-height`/`font-size`/`theme`〔R-8 ⚠E〕、下記 R-5 の list 型3キー、R-6 の `config-file`)のいずれにも該当しないキーは、**未知キー warn 診断を1件生成して継続**する。パースは中断しない。
- **R-5**: v1 で「list 型キーとして認識するが値は保持しない」対象を **`keybind` / `palette` / `font-family`** の3つに限定して明示する。これらは R-4 とは異なる専用文言の warn 診断を1件生成し、値を読み飛ばす(蓄積ストレージは実装しない — EXPAND/CHALLENGE 裁定 DEFER)。
- **R-6**: `config-file` は上記 list 型キー集合ともさらに区別し、R-4/R-5 いずれとも異なる専用文言の warn 診断を生成する(CHALLENGE 裁定8: typo と「認識済み未実装」の区別)。実ファイルの読込・再帰 include・循環検出は行わない。
- **R-7 ⚠B**: v1 スカラーキー(`window-width`/`window-height`/`font-size`)の値が、数値としてパース不能、または数値としては妥当でも意味的範囲外(`font-size` が非正・非有限など)である場合、**warn 診断を1件生成し、そのキーのみデフォルトへフォールバックしてパースを継続**する。この経路は**ファイル起源の値にのみ**適用され、CLI 起源の最終値に対する `validate_startup_config` の hard-fail 経路とは独立に維持する(既存の CLI 向けテスト2件は変更しない)。
- **R-8 ⚠E(2026-07-03 ユーザー裁定で改訂 — theme キーの v1 認識)**: theme-selection 増分は**完走・出荷済み**(`ConfigOverrides.theme`/`StartupConfig.theme`/`AppConfig.theme` と転送ロジック、TOML `theme = "name"` 受理)。機能リグレッションを避けるため、`theme` を **v1 認識スカラーキー(文字列)**として Ghostty 構文パーサーで受理する: `theme = <name>` → `ConfigOverrides.theme = Some(name)`(クォート任意、R-1 規則に従う。空値 `theme =` は R-3 のリセット)。値が `light:`/`dark:` プレフィックスを持つペア構文の場合は**専用文言の warn 診断を1件生成し、値を受理しない**(`theme == None` — 片側だけ読む等の部分受理は禁止。theme-selection spec の Flux 裁定「silent fidelity divergence 禁止」を warn+継続モデルへ移植)。TOML 専用の `parse_theme` 実装は削除し、TOML 前提テスト `theme_key_is_accepted`/`light_dark_syntax_is_rejected` は Ghostty 構文前提に書き換える。theme-selection.md の R-1/R-2/L2 noa-config 節/AC-1〜3 は本 spec 実装時に Ghostty 構文前提へ改稿する(⚠D 確定)。

**Diagnostics 集約**

- **R-9**: `Diagnostic` は `message: String` を持つ軽量な型とし(Ghostty 風 severity taxonomy は模倣しない — CHALLENGE 裁定9)、`StartupConfig`/`ConfigOverrides` の**外側**で `Vec<Diagnostic>` として蓄積・返却する。`load_startup_config` のシグネチャを `pub fn load_startup_config(cli: ConfigOverrides) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)>` に変更する。**(現物コードによる訂正)**: CHALLENGE 裁定9の「Copy derive 温存」前提は、theme-selection 増分の `theme: Option<String>` 先行追加により既に崩れている(両構造体とも `Clone` のみ)。本要件の目的は「Diagnostics を構造体に混入させてこれ以上複雑化させない」ことに読み替える。

**noa ネイティブパス**

- **R-10**: noa ネイティブ config は**単一パス方式を維持**し、ファイル名のみ `config.toml` から `config` に変更する(`default_config_path()` は `<config_dir>/noa/config` を返す)。パス発見・precedence(default < file < CLI)の機構自体は変更しない。

**window-width / window-height / font-size マッピング**

- **R-11**: `font-size` config キーは内部 `font_size: f32` フィールドへ1:1でマッピングする(既存の内部フィールド名 `cols`/`rows`/`font_size` は変更しない)。
- **R-12 ⚠C**: `window-width`/`window-height` は **config ファイル層でのみ** Ghostty 意味論(両方指定必須)を採用する。ファイル内で片方のみ指定された場合、両方とも未指定(`None`)として扱い、warn 診断を1件生成する。空値リセット(R-3)により片方が未指定化された場合も「片方のみ指定」と同一に扱う(「リセット済み」と「未記載」を区別する tri-state は導入しない)。
- **R-13 ⚠C**: `window-width`/`window-height` が両方とも有効な数値の場合、`window-width` は10、`window-height` は4を下限としてクランプする。クランプ自体は診断を伴わない(Ghostty の観測可能挙動に倣う — FACT VERIFIED)。
- **R-14**: CLI `--cols`/`--rows`(noa 独自キー)は R-12/R-13 の対象外とし、従来どおり独立に指定可能なまま維持する。改名は行わない。

**CLI**

- **R-15**: `Args` の既存3フィールド(`cols`/`rows`/`font_size`)は名称・型とも変更しない。汎用 `--<key>=<value>` フラグは実装しない(speculative generalization として CUT 済み)。`--font-size` は clap の kebab-case 化により既に Ghostty のキー名と一致しているため変更不要。

**TOML 移行**

- **R-16**: 旧 `config.toml`(`<config_dir>/noa/config.toml`)の存在を検出した場合、**起動処理1回につき warn 診断を1件**生成する(プロセス単位の一度きり、永続的な「既に表示済み」フラグは持たない)。判定は旧ファイルの存在のみで行い、新 `config` の有無には依存しない(移行完了後も旧ファイルが残置されている限り warn を出し、削除を促す)。自動変換・自動読込は行わない。

**Ghostty インポート・初回ヒント**

- **R-17 ⚠A**: `--import-ghostty-config` フラグ。実行時に Ghostty 側4候補パス — ①`$XDG_CONFIG_HOME`(未設定時は `~/.config`)配下の `ghostty/config.ghostty` ②同 `ghostty/config` ③`~/Library/Application Support/com.mitchellh.ghostty/config.ghostty` ④同 `config` — のうち存在するものをこの優先順で全読みし、**後勝ちマージ**で解決する。v1 認識スカラーキー(`window-width`/`window-height`/`font-size`/`theme` の4キー)は元の行テキストのまま noa の `config` へ書き出し、それ以外の全キー(list 型・`config-file`・真の未知キーを含む)は元の行テキストを保持したまま `# ` を前置してコメントアウトする。書き込み先(`default_config_path()`)に既にファイルが存在する場合は**書き込みを拒否**する(非破壊、NFR-6)。4候補のいずれも存在しない場合は失敗として扱う。
- **R-18 ⚠A**: 初回起動ヒント。noa 側の `config` が存在せず、かつ R-17 の4候補のうち少なくとも1つが存在する場合、通常起動時(import フラグなし)に `--import-ghostty-config` の利用を促す**1行**ヒントを表示する。自動書き込みは行わない。

### 非機能要件 (NFR)

- **NFR-1(依存衛生)**: パーサー置き換えに伴い `noa-config` から `toml_edit` 依存を除去する。新規の外部クレート依存を追加しない(標準ライブラリの文字列処理のみで実装する)。禁止対象は `[dependencies]` であり、`[dev-dependencies]` は本 NFR の対象外(ただし v1 のテストは `std::env::temp_dir()` ベースで足りる見込みで、新規 dev-dep も不要と想定)。ワークスペースルート `Cargo.toml` の `[workspace.dependencies]` からも `toml_edit` エントリを削除する。
- **NFR-2(品質ゲート)**: 変更後も `cargo clippy --workspace` および `cargo test --workspace` がクリーンであること。もみ消し目的の新規 `#[allow(...)]` を追加しない。
- **NFR-3(依存境界)**: `noa-config` の依存グラフに `wgpu`/`winit` が含まれないこと(既存の回帰保証を継続)。
- **NFR-4(オフラインビルド)**: `cargo build --workspace --offline` が成功すること(ネットワークアクセスなし、追加の生成物再生成が不要)。
- **NFR-5(決定性)**: パーサー(`parse_directives`/構成後の折り畳みロジック)は純粋関数である — 同一入力に対し常に同一出力を返し、ファイル I/O・環境変数・グローバル可変状態に触れない。property-based テストが書ける形を維持する(`proptest` 等の新規テスト依存の追加までは求めない)。純粋性は「`src/parser.rs` に `std::fs`/`std::env` の使用が存在しない」ことの機械検査(grep ベースのテスト、AC-45)で担保する。
- **NFR-6(非破壊性)**: `--import-ghostty-config` は非破壊である — 既存の noa `config` ファイルを上書き・変更することは決してない。

## L2 — Detail

per-crate のシームのみを定義する(コードは書かない)。

### noa-config

現行 `crates/noa-config/src/lib.rs` は **396行**(本ドラフト前半の「342行」は theme-selection 増分の先行変更を反映する前の stale 値 — 本 L2 の行参照は 2026-07-03 時点の現物コードで検証済み)。

**モジュール構成**

- `src/lib.rs` — `StartupConfig`/`ConfigOverrides`/`DEFAULT_*`/`load_startup_config`/`load_file_overrides`/`load_overrides_from_path`/`default_config_path`/`validate_startup_config`/`validate_grid_dimension` を保持。TOML 検出診断の付加はここで行う。**2レーンのエラーモデル(意図的な設計判断)**: パース内容の問題(未知キー・型不正・ペア欠落等)はすべて Diagnostic(warn + 継続)、ファイル読み取り自体の失敗(非 UTF-8・権限エラー等の真の I/O エラー)は従来通り `anyhow::Result::Err`(致命)。前者は「ユーザーの設定ミスで起動を殺さない」、後者は「環境の破損を黙って握りつぶさない」ための区別である。
- `src/parser.rs`(新規) — `Directive`/`Diagnostic` 型、純粋関数 `parse_directives`、キー分類テーブル、折り畳みロジック `build_overrides`、既存名を維持する薄いラッパー `parse_overrides` を配置する(noa-vt の Handler/Stream 分離規範を踏襲 — CHALLENGE 裁定1)。
  - `pub struct Directive { pub line: usize, pub key: String, pub value: Option<String> }`(`value: None` = 空値リセット)
  - `pub struct Diagnostic { pub message: String }`(severity 階層なし — CHALLENGE 裁定9)
  - `pub fn parse_directives(source: &str) -> Vec<Directive>`(I/O レス、R-1〜R-3 の構文規則を実装)
  - `fn build_overrides(path: &Path, directives: &[Directive]) -> (ConfigOverrides, Vec<Diagnostic>)`(R-4〜R-8・R-11〜R-13 のキー分類・警告・window ペア検証・クランプを実装)
  - `pub fn parse_overrides(path: &Path, source: &str) -> (ConfigOverrides, Vec<Diagnostic>)` — 既存の関数名を維持しつつシグネチャ変更(`anyhow::Result` を廃し非致命化 — Ghostty 同様「致命は OOM のみ」)。
- `src/ghostty.rs`(新規) — **hermetic テスト可能性のため「純粋関数 + 薄い環境ラッパー」に分割**(Attest 指摘 / CHALLENGE 裁定1 の横展開): `pub fn ghostty_config_candidates_from(xdg_config_home: Option<&Path>, home_dir: &Path) -> [PathBuf; 4]`(純粋関数、存在有無に関わらず常に4パスを返す)+ 実環境を読む薄いラッパー `pub fn ghostty_config_candidates() -> [PathBuf; 4]`(R-17/R-18 で共有)。`$XDG_CONFIG_HOME` は `dirs::config_dir()` では代替できない(macOS では App Support を返す)ため、ラッパーが環境変数を直接読み `~/.config` にフォールバックする。
- `src/import.rs`(新規) — 同じく分割: 純粋部 `pub fn build_import_output(source_texts: &[String]) -> (String, ImportStats)`(連結・分類・コメントアウトを文字列のみで実施、unit test 可)+ I/O 部 `pub fn import_ghostty_config_at(candidates: &[PathBuf], target: &Path) -> anyhow::Result<ImportOutcome>`(候補読込・非破壊チェック・書込)+ 実環境を配線する薄いラッパー `pub fn import_ghostty_config() -> anyhow::Result<ImportOutcome>`(詳細は「Import writer」節)。

**既存要素の置き換え**

| 現行(lib.rs) | 置き換え後 |
|---|---|
| `SUPPORTED_KEYS`(13行目) | `parser.rs` 内のキー分類テーブル(v1 スカラー3キー / list 型3キー / `config-file` / それ以外=未知)。allowlist ではなく分類のみ、hard-fail 用途では使わない |
| `reject_unknown_keys`(126〜137行目、hard fail) | `build_overrides` 内のインライン分類 + Diagnostic 生成(継続、致命化しない) |
| `parse_u16`(139〜158行目、toml_edit `Item` ベース) | `Directive.value: Option<String>` を受け取る文字列ベースの数値パース。失敗時は `bail!` ではなく Diagnostic 生成 + `None`(R-7) |
| `parse_font_size`(160〜176行目) | 同上パターンの文字列ベース版(正/有限チェックも同じ経路で non-fatal 化) |
| `parse_theme`(178〜191行目) | **置き換え**(R-8 ⚠E: `build_overrides` 内の theme 分岐 — 文字列パススルー + `light:`/`dark:` ペア構文の専用 warn。TOML 専用実装は削除) |
| `invalid_type`(203〜209行目、`Item::type_name()` ベース) | 型情報を持たない汎用「invalid value」Diagnostic コンストラクタ(path・key・生テキストのみ埋め込み) |
| `find_first_existing_config_path`(86〜95行目) | **削除**(呼び出し元なし — noa ネイティブパスは単一固定/裁定2、Ghostty 側4候補は「全読み後勝ちマージ」で first-wins とは意味論が異なり転用不可) |
| `default_config_path`(82〜84行目) | 純粋関数 `default_config_path_in(config_dir: &Path) -> PathBuf`(ファイル名 `"config"`)+ 薄いラッパー `default_config_path()` に分割。旧ファイル検出(R-16)用に同型の `legacy_toml_config_path_in`/`legacy_toml_config_path`(ファイル名 `"config.toml"`)を追加 |

**変更なしで生存する既存要素**

- `ConfigOverrides::merge`(45〜52行目)・`ConfigOverrides::apply_to`(54〜61行目) — フィールド単位の `.or()`/`.unwrap_or()` ロジックは無変更。
- `impl Default for StartupConfig`(24〜33行目) — `DEFAULT_COLS`/`DEFAULT_ROWS`/`DEFAULT_FONT_SIZE` の値も無変更。
- `validate_grid_dimension`(193〜201行目) — ロジック無変更(呼び出し文脈のみ変化)。
- `validate_startup_config`(117〜124行目) — **役割を CLI 起源値専用の最終防波堤に純化**しつつロジック無変更で存続。ファイル起源の無効値は R-7 で事前に無害化(None 化)されるため、ここに到達する無効値はほぼ CLI 起源のみ(既存テスト `validates_cli_grid_values_after_merge`/`validates_cli_font_size_after_merge` は変更不要)。
- `load_overrides_from_path`(97〜101行目) — 役割は維持、戻り値型のみ `anyhow::Result<(ConfigOverrides, Vec<Diagnostic>)>` に変更。

**`load_startup_config` の変更**

実ロジックをパス注入可能な `load_startup_config_from(config_path: &Path, legacy_path: &Path, cli: ConfigOverrides) -> anyhow::Result<(StartupConfig, Vec<Diagnostic>)>` として実装し、公開 API `load_startup_config(cli)`(64〜70行目)はこれに実パスを配線する薄いラッパーへ変更する(hermetic テスト可能性 — Attest 指摘)。内部で `load_overrides_from_path` からの Diagnostic に加え、R-16(TOML 検出)の Diagnostic を追加してから返す。`validate_startup_config` の呼び出し(hard-fail 経路)は維持。

**`Cargo.toml` 変更**

- `crates/noa-config/Cargo.toml`: `[dependencies]` から `toml_edit.workspace = true` を削除(残るのは `anyhow`/`dirs` のみ)。
- ルート `Cargo.toml`: `[workspace.dependencies]` の `toml_edit` エントリを削除(唯一の直接利用者が `noa-config`。ただし `Cargo.lock` には muda 系ビルド依存経由で残存しうるため、回帰チェックは `cargo tree -p noa-config` にスコープする)。

### bin/noa

**留意**: `bin/noa/src/main.rs` は theme-selection 増分の並行実装により 32行 → 66行 へ変化中(`app_config_from_startup` ヘルパー + テスト2件追加済み)。本節は絶対行番号ではなく**関数名・構造**基準で記述する。

- `Args`(既存 `cols`/`rows`/`font_size` 無変更)に `#[arg(long)] import_ghostty_config: bool` を追加(`--import-ghostty-config`)。
- `main()` 冒頭、`Args::parse()` 直後に **import フラグの早期分岐**: 真なら `noa_config::import_ghostty_config()` を呼び、成功時はサマリ(書き込み先パス・supported/commented-out 件数)を stdout、失敗時はエラーを stderr に出し、**GUI(`noa_app::run`)を起動せず終了**する。
- 通常経路: `load_startup_config(...)` の戻り値を `let (config, diagnostics) = ...?;` に変更し、`diagnostics` を1件ずつ **`eprintln!` で stderr へ直接出力**する(TOML 検出・未知キー・list 型・config-file・invalid value のすべてがこの単一ループで均一に出力される)。**`log::warn!` は使用しない(Quality Gate F1)**: 現行 `env_logger::init()` は `RUST_LOG` 未設定時にフィルタが `LevelFilter::Off` となり(vendored `env_filter-2.0.0/src/filter.rs:226` で確認済み)、`log` 経由ではデフォルト環境で診断が一切見えなくなる。ユーザー向け設定診断の可視性を `RUST_LOG` に依存させてはならない。
- 診断出力の後、**初回起動ヒント判定**: 判定ロジックは純粋関数 `fn import_hint(config_exists: bool, any_candidate_exists: bool) -> Option<&'static str>` として切り出し(unit test 可)、`main()` は `default_config_path()` の存在と `ghostty_config_candidates()` の存在判定を渡して、`Some` なら **`eprintln!` で stderr へ**1行出力する(→ Open Questions の「診断の出力先」は解決: 出力 sink は stderr、GUI ダイアログ・新規ログファイル基盤は追加しない)。
- **キー名マッピングに main.rs 側の変更は不要**: `window-width`/`window-height`/`font-size` → 内部 `cols`/`rows`/`font_size` の解決は `noa-config` の `build_overrides` 内で完結。既存の `app_config_from_startup(config) -> AppConfig` はフィールド構成が変わらないため無変更で存続。
- CLI 側 `ConfigOverrides { cols: args.cols, rows: args.rows, font_size: args.font_size, theme: None }` の構築は無変更。

### noa-app

**本 spec による変更なし。** `AppConfig`(`crates/noa-app/src/app.rs:37-42`)の `theme` フィールドは theme-selection 増分によるもので、本 spec は関与しない。ghostty-config が課す唯一の制約は「`cols`/`rows`/`font_size` が引き続き無変更で流れること」。`noa-app` が `noa-config` に依存しない DAG 境界(バイナリのみが橋渡し)も維持。

### Import writer(`noa-config::import`)

- **API 分割(hermetic テスト可能性)**: 純粋部 `build_import_output(source_texts: &[String]) -> (String, ImportStats)` が連結・分類・コメントアウトのすべてを文字列上で行い(unit test は文字列のみで可)、I/O 部 `import_ghostty_config_at(candidates: &[PathBuf], target: &Path)` が候補読込・非破壊チェック・書込を担う。引数なしの `import_ghostty_config()` は実環境(`ghostty_config_candidates()`・`default_config_path()`)を配線する薄いラッパー。
- **出力形式**: プレーンテキスト、Ghostty 構文の行単位。整形・再シリアライズは行わず**元の行テキストをそのまま**出力(クォート・空白・数値表記を保持)。
- **マージ入力**: R-17 の4候補のうち存在するものを優先順に読み、生テキストを改行区切りで連結してから同一の `parse_directives` を通す。「後勝ちマージ」は連結順序 + 既存の scalar last-wins 折り畳みで実現(専用マージアルゴリズムは新設しない)。
- **コメントアウト規則**: 連結済みソースの各行のキーを分類し、supported(v1 認識スカラー4キー)な行は無変更で書き出し、unsupported な行は先頭に `# ` を付与。空行・既存コメント行は透過。
- **`config-file` の扱い**: import 時も再帰追跡しない。`config-file = ...` 行はコメントアウト対象になるのみ。
- **書き込み先・非破壊性**: 常に `default_config_path()`(新名 `config`)。既存ファイルがあれば**一切書き込まず** `Err`(NFR-6)。親ディレクトリが無ければ作成。
- **候補ゼロ時**: `Err`(4パス列挙メッセージ)を返し、何も書き込まない。
- **Open Question(未裁定)**: 出所ヘッダーコメント(インポート元・日時)は未裁定のまま。実装はヘッダーなしの最小形で進めてよいが、後日追加が非破壊追記で可能な設計(行単位書き出し)を維持する。

### テスト計画差分

現行 `noa-config` の `#[cfg(test)] mod tests`(211〜396行目)には **12件**のテストが存在(Ripple 分析当時の 10 件 + theme-selection 増分の先行追加 2 件)。

| # | テスト名 | 扱い |
|---|---|---|
| 1 | `defaults_match_existing_startup_behavior` | **維持** |
| 2 | `parses_supported_config_keys` | **書き換え**(TOML → Ghostty 構文 `window-width=`/`window-height=`/`font-size=`、戻り値タプル追随、diagnostics 空も検証) |
| 3 | `cli_overrides_config_file_values` | **維持**(`merge`/`apply_to` 直接呼びでパーサー非依存) |
| 4 | `theme_key_is_accepted` | **書き換え**(R-8 ⚠E: TOML 構文 → Ghostty 構文 `theme = 3024 Day` での受理検証) |
| 5 | `finds_first_existing_config_candidate` | **削除**(関数ごと削除) |
| 6 | `invalid_file_value_includes_path_and_key` | **書き換え**(`cols = 0` は「ペア片方欠落」に意味が変わるため、非数値 `window-width = abc` 等の真の型不正 + warn+continue 検証へ) |
| 7 | `invalid_type_includes_path_and_key` | **書き換え**(`font-size = large` の非数値、hard fail → warn+デフォルトフォールバック検証へ) |
| 8 | `unknown_key_is_rejected` | **書き換え**(期待反転: hard fail → warn+continue。`bogus-key = "x"` + 他キー継続の検証) |
| 9 | `light_dark_syntax_is_rejected` | **書き換え**(R-8 ⚠E: hard error → 専用 warn 診断 + `theme == None` の非致命化検証、Ghostty 構文前提) |
| 10 | `invalid_file_values_are_rejected` | **書き換え**(`rows = 0` はペア欠落ケースへ分離、`font_size = -1.0`/`inf` は warn+フォールバックへ) |
| 11 | `validates_cli_grid_values_after_merge` | **維持** |
| 12 | `validates_cli_font_size_after_merge` | **維持** |

**新規追加テスト(代表、L3 の AC と対応)**: 行頭コメント/値中 `#` 非コメント、`=` 無し行読み飛ばし、クォート剥がし/片側クォート保持、last-wins、空値リセット/クォート空文字列非リセット、list 型3キーの専用診断、`config-file` 専用診断、3診断文言の相互相違、`theme` の汎用未知キー経路、window ペア片方欠落、9×4 境界クランプ、CLI `--cols` 単独、TOML 検出 warn、import(候補ゼロ/単一/複数後勝ち/既存拒否/`config-file` 非追跡)、初回ヒント3条件、`cargo tree -p noa-config` の `toml_edit` 不在。

**bin/noa 側**: theme-selection 増分の既存テスト2件は本 spec スコープ外で変更不要。import 早期分岐・診断出力ループ・初回ヒント判定の新規テストを追加。

## L3 — Acceptance Criteria

各 AC は対応する `R-*`/`NFR-*` を明記する(`AC-n → R-m` 形式)。⚠A/⚠B/⚠C に依存する AC には同マークを付す。

### 構文パーサー — 基本規則

- **AC-1 → R-1**: Given `window-width   =   120`(`=` 前後に余分な空白)。When `parse_directives` を実行する。Then `Directive{ key: "window-width", value: Some("120") }`(空白除去済み)。
- **AC-2 → R-1**: Given 先頭に空白を伴うコメント行 `  # a comment`。When `parse_directives` を実行する。Then 対応する `Directive` は生成されない。
- **AC-3 → R-1**: Given `font-size = 14 # not a comment`(値の途中に `#`)。When `parse_directives` を実行する。Then `Directive.value == Some("14 # not a comment")`(`#` は行頭以外ではコメント開始にならない)。
- **AC-4 → R-1**: Given `=` を含まない非空行 `not-a-directive` と、後続の `font-size = 15`。When `parse_directives` を実行する。Then 前者の `Directive` は生成されず、後者は正しく生成される(パース継続)。
- **AC-5 → R-1**: Given `window-width = "120"`(正しく閉じたクォート)。When `parse_directives` を実行する。Then `Directive.value == Some("120")`(クォート剥がし)。
- **AC-6 → R-1**: Given `window-width = "120`(片側のみの未閉クォート)。When `parse_directives` を実行する。Then `Directive.value == Some("\"120")`(リテラル保持、後段で AC-14 経路)。
- **AC-7 → R-2**: Given `font-size = 14` の後に `font-size = 16`。When ソース全体をパースする。Then `ConfigOverrides.font_size == Some(16.0)`(last-wins)。
- **AC-8 → R-3**: Given `font-size = 14` の後に空値の `font-size =`。When ソース全体をパースする。Then `font_size == None`(リセット。`apply_to` 後は `DEFAULT_FONT_SIZE`)。
- **AC-9a → R-3**: Given `window-width = ""`(クォートされた空文字列)。When パースする。Then `Directive.value == Some("")`(空値リセット `None` ではなく、リテラル空文字列として保持される)。
- **AC-9b → R-3, R-7 ⚠B**: Given 同上。When `build_overrides` まで通す。Then 型不正 diagnostic 1件が生成され、該当キーはデフォルトへフォールバックする(AC-14 と同じ経路)。
- **AC-49 → R-1**: Given `key = "ab"cd"`(値内部に非エスケープの `"`)。When `parse_directives` を実行する。Then クォート剥がしは行われず `value == Some("\"ab\"cd\"")`(リテラル保持)。
- **AC-50 → R-1**: Given CRLF 改行のファイル(`font-size = 15\r\n`)と先頭 UTF-8 BOM 付きのファイル(`\u{FEFF}font-size = 15`)。When `parse_directives` を実行する。Then いずれも `Directive{ key: "font-size", value: Some("15") }` が生成される(`\r` 除去・BOM 除去)。

### キー分類・警告

- **AC-10 → R-4**: Given `bogus-key = "x"` と `font-size = 15` を含むファイル。When `parse_overrides` を実行する。Then エラーにならず、diagnostics に `bogus-key` とファイルパスを含むメッセージが1件、かつ `font_size == Some(15.0)`(継続)。
- **AC-11 → R-4, R-5, R-6**: Given 未知キー診断(AC-10)・list 型診断(AC-12)・`config-file` 診断(AC-13)。When 3件の文言を比較する。Then 3件とも完全に異なる文言で、3カテゴリを判別できる。
- **AC-12 → R-5**: Given `keybind = "cmd+shift+f=..."`・`palette = "0=#000000"`・`font-family = "Fira Code"` を各々単独で含むファイル。When 各々をパースする。Then それぞれ diagnostic 1件、値は保持されない。
- **AC-13 → R-6**: Given `config-file = "~/.config/ghostty/extra"`。When パースする。Then diagnostic 1件(AC-10/12 と異なる文言)が生成される。(「当該パスへのファイルアクセスが発生しない」ことはパーサーの純粋性検査 AC-45 で構造的に担保する)
- **AC-14 → R-7 ⚠B**: Given `font-size = not-a-number`。When パースする。Then エラーにならず `font_size == None`、diagnostics にパス・`font-size`・`not-a-number` を含むメッセージ1件。
- **AC-15 → R-8 ⚠E**: Given `theme = 3024 Day`(クォートなし・空白含む)と `theme = "3024 Day"`(クォートあり)の各ファイル。When `parse_overrides` を実行する。Then いずれも diagnostic ゼロで `ConfigOverrides.theme == Some("3024 Day")`(認識スカラーキーとして受理、クォート有無で等価)。
- **AC-51 → R-8 ⚠E**: Given `theme = light:Foo,dark:Bar`。When `parse_overrides` を実行する。Then エラーにならず、**未知キー warn とも型不正 warn とも異なる専用文言**の diagnostic が1件生成され、`ConfigOverrides.theme == None`(ペア構文の部分受理なし)。

### Diagnostics 集約

- **AC-16 → R-9**: Given 未知キー1件・型不正1件を含む config ファイルを tempdir に配置。When `load_startup_config_from(その config パス, 不在の legacy パス, ConfigOverrides::default())` を呼ぶ。Then 戻り値型が `anyhow::Result<(StartupConfig, Vec<Diagnostic>)>` で、`Vec` にファイル出現順の2件が含まれる(hermetic — 実ホームディレクトリ非依存)。
- **AC-17 → R-9**: Given 変更後の `StartupConfig`/`ConfigOverrides` 定義。When 両構造体を `..` なしで完全分解する unit test をコンパイルする。Then どちらにも `Diagnostic` 系フィールドは存在しない(フィールド追加はコンパイルエラーとして検出される)。
- **AC-18 → R-10**: Given 任意のベースパス。When `default_config_path_in(base)` を呼ぶ。Then `base/noa/config` を返す(拡張子なし、`config.toml` ではない)。純粋関数のため環境非依存で検証可能。
- **AC-19 → R-10**: Given tempdir 内の `config` に `font-size = 16`、CLI 相当の `ConfigOverrides { font_size: Some(18.0), .. }`。When `load_startup_config_from` を実行する。Then 最終値 `18.0`(CLI > file 維持)。
- **AC-20 → R-10**: Given tempdir 内に `config`・旧 `config.toml` とも不在、CLI オーバーライドなし。When `load_startup_config_from` を実行する。Then `Ok((StartupConfig::default(), vec![]))`(エラーなし・diagnostics 空)。

### window sizing

- **AC-21 → R-11**: Given `font-size = 15.5` のみ。When パースする。Then `font_size == Some(15.5)`(ペアリング・クランプ対象外)。
- **AC-22 → R-12 ⚠C**: Given `window-width = 120` のみ(`window-height` 未設定)。When パースする。Then `cols == None` かつ `rows == None`(両方破棄)、diagnostic 1件。
- **AC-23 → R-12 ⚠C**: Given 対称ケース(`window-height` のみ)。When パースする。Then 同様に両方 `None` + diagnostic 1件。
- **AC-24 → R-13 ⚠C**: Given `window-width = 9` かつ `window-height = 4`。When パースする。Then `cols == Some(10)`(クランプ)、`rows == Some(4)`(下限ちょうどは無変更)。
- **AC-25 → R-13 ⚠C**: Given `window-width = 120` かつ `window-height = 30`。When パースする。Then `cols == Some(120)`・`rows == Some(30)`(無変更)。
- **AC-43 → R-7 ⚠B**: Given `window-width = abc` かつ `window-height = 30`(width が非数値)。When パースする。Then エラーにならず型不正 diagnostic 1件、width は `None` 化され、結果としてペア欠落(R-12)の扱いに従う。`window-height = abc` の対称ケースも同様。
- **AC-44 → R-13 ⚠C**: Given `window-width = 120` かつ `window-height = 2`(height が下限4未満)。When パースする。Then `cols == Some(120)`・`rows == Some(4)`(height 側クランプ)。
- **AC-46 → R-3, R-12 ⚠C**: Given `window-width = 120` と `window-height = 30` の後に空値の `window-height =`(明示リセット)。When ソース全体をパースする。Then 「片方のみ指定」と同一に扱われ、両方 `None` + diagnostic 1件(リセットと未記載を区別する tri-state が存在しない)。
- **AC-26 → R-14**: Given CLI 相当の `ConfigOverrides { cols: Some(50), rows: None, .. }`、tempdir に config なし。When `load_startup_config_from` を実行する。Then エラーにならず `cols == 50`・`rows == DEFAULT_ROWS`(CLI は両方必須ルール対象外)。

### CLI

- **AC-27 → R-15**: Given `Args` 定義。When ソース検査または `noa --help`。Then `--cols`/`--rows`/`--font-size`(無変更)と新規 `--import-ghostty-config` のみが存在し、汎用 `--<key>=<value>` 機構は存在しない。

### TOML 移行

- **AC-28 → R-16**: Given tempdir 内に旧 `config.toml`(中身は任意の旧 TOML)が存在し、新 `config` は不在。When `load_startup_config_from` を1回実行する。Then 旧ファイル検出メッセージが diagnostics に**ちょうど1件**含まれ、旧ファイルの内容はパース・適用されない。
- **AC-47 → R-16**: Given tempdir 内に旧 `config.toml` と新 `config` の**両方**が存在する。When `load_startup_config_from` を実行する。Then 新 `config` の内容が適用され、かつ旧ファイル検出メッセージも1件含まれる(新 config の有無に依存しない判定)。

### Ghostty インポート

- **AC-29 → R-17 ⚠A**: Given tempdir 上の4候補パスのいずれも不在。When `import_ghostty_config_at(candidates, target)` を実行する。Then `Err`(4候補パスを列挙したメッセージ)、`target` へのファイル書き込みなし。(`noa --import-ghostty-config` の終了コード配線は AC-27 の flag 存在 + one-shot レビューで確認)
- **AC-30 → R-17 ⚠A ⚠E**: Given tempdir 上の候補の1つに `window-width = 100`・`theme = "Foo"`・`keybind = "cmd+n=new_tab"`・`window-decoration = false`、`target` 不在。When `import_ghostty_config_at` 実行。Then `target` に `window-width = 100` と `theme = "Foo"` は無変更で出力(認識スカラーキー)、`keybind`/`window-decoration` 行は元テキスト保持のまま `# ` 前置でコメントアウト、`Ok`。
- **AC-31 → R-17 ⚠A**: Given tempdir 上で優先順位の低い候補が `font-size = 12`、高い候補(App Support 相当スロット)が `font-size = 14`、両方存在。When `import_ghostty_config_at` 実行 → 出力 `target` を `parse_overrides` で読み直す。Then `font_size == Some(14.0)`(後勝ちマージ)。
- **AC-32 → R-17, NFR-6 ⚠A**: Given `target` に既存ファイルが存在。When `import_ghostty_config_at` を実行する。Then `Err`(「上書き拒否」メッセージ)、既存ファイルはバイト単位で無変化。
- **AC-33 → R-17 ⚠A**: Given 候補に `config-file = "<tempdir 内の実在ファイル>"` 行。When `import_ghostty_config_at` 実行。Then その行はコメントアウトされるのみで、指定先ファイルの内容は出力に一切現れない(再帰追跡なし — 内容非混入で機械検証)。

### 初回ヒント

- **AC-34 → R-18 ⚠A**: Given `config_exists == false` かつ `any_candidate_exists == true`。When 純粋関数 `import_hint(config_exists, any_candidate_exists)` を呼ぶ。Then `Some(...)` を返し、文言に `--import-ghostty-config` を含む。(main.rs での stderr `eprintln!` 配線と「書き込みが発生しない」ことは one-shot コードレビューで確認 — 通常起動経路は GUI 必須のため headless プロセステスト不可: CLAUDE.md 既知制約)
- **AC-35 → R-18 ⚠A**: Given `config_exists == false` かつ `any_candidate_exists == false`。When `import_hint` を呼ぶ。Then `None`。
- **AC-36 → R-18 ⚠A**: Given `config_exists == true`(候補の有無を問わず両ケース)。When `import_hint` を呼ぶ。Then `None`。

### 依存・品質

- **AC-37 → NFR-1**: Given 変更後の `crates/noa-config/Cargo.toml`。When 検査する。Then `[dependencies]` は `anyhow`・`dirs` のみ、ルート `Cargo.toml` の `[workspace.dependencies]` にも `toml_edit` エントリなし。
- **AC-38 → NFR-1**: Given 変更後のワークスペース。When `cargo tree -p noa-config --offline` を実行する。Then 出力に `toml_edit` が現れない(`Cargo.lock` 全体 grep は muda 系残存で偽陽性のため不採用、`-p noa-config` スコープを正とする)。
- **AC-39 → NFR-2**: Given 完成した変更。When `cargo test --workspace --offline` と `cargo clippy --workspace --offline` を実行する。Then 両方 exit code 0、新規 `#[allow(...)]` なし。
- **AC-40 → NFR-3**: Given `noa-config` の依存グラフ。When `cargo tree -p noa-config --offline`。Then `wgpu`/`winit` は含まれない。
- **AC-41 → NFR-4**: Given クリーンな target。When `cargo build --workspace --offline`。Then ネットワークなしで成功、生成物再生成不要。
- **AC-42 → NFR-5**: Given `parse_directives` と `build_overrides`、代表的な入力群(正常・診断あり・空・境界ケース数種)。When 同一入力をそれぞれ2回渡す。Then 2回とも等しい結果(`PartialEq`)を返す(機械検証)。
- **AC-45 → NFR-5, R-6**: Given `crates/noa-config/src/parser.rs` のソース。When `std::fs`/`std::env`(および `dirs::`)の使用を grep 検査する(unit test 内の `include_str!` grep または CI ステップ)。Then 一切出現しない(パーサーの I/O レス性の構造的担保 — AC-13/AC-33 の「アクセス不発生」クレームの検証手段)。
- **AC-48 → R-4, R-16, R-18(可視性)**: Given `bin/noa/src/main.rs` の診断出力・ヒント出力の実装。When ソースを検査する。Then 出力が `eprintln!` 直接呼びであり、`log::warn!`/`log::info!` 経由の経路が存在しない(`RUST_LOG` 未設定のデフォルト環境で `env_logger` フィルタが `Off` になっても診断が可視であることの構造的担保 — Quality Gate F1)。

### トレーサビリティ・サマリ

| 要件 | AC | 要件 | AC | 要件 | AC |
|---|---|---|---|---|---|
| R-1 | AC-1〜6, AC-49, AC-50 | R-11 | AC-21 | NFR-1 | AC-37, AC-38 |
| R-2 | AC-7 | R-12 | AC-22, AC-23, AC-46 | NFR-2 | AC-39 |
| R-3 | AC-8, AC-9a, AC-9b, AC-46 | R-13 | AC-24, AC-25, AC-44 | NFR-3 | AC-40 |
| R-4 | AC-10, AC-11, AC-48 | R-14 | AC-26 | NFR-4 | AC-41 |
| R-5 | AC-11, AC-12 | R-15 | AC-27 | NFR-5 | AC-42, AC-45 |
| R-6 | AC-11, AC-13, AC-45 | R-16 | AC-28, AC-47, AC-48 | NFR-6 | AC-32 |
| R-7 | AC-9b, AC-14, AC-43 | R-17 | AC-29〜33 | | |
| R-8 | AC-15, AC-30, AC-51 | R-18 | AC-34〜36, AC-48 | | |
| R-9 | AC-16, AC-17 | | | | |
| R-10 | AC-18〜20 | | | | |

対象要件 24件(R-1〜R-18, NFR-1〜NFR-6)すべてに ≥1 件の AC が対応。R-7 は数値スカラーキー全種の型不正(AC-9b/14/43)、R-13 は両軸のクランプ(AC-24/44)、R-8 は theme の受理・ペア構文不受理・import パススルー(AC-15/51/30)を内容レベルでカバー(Quality Gate F4/F11 + ⚠E 対応済み)。AC 総数 52 件。

## Scope

*(SHAPE — Spark, 2026-07-02)*

### 1. 問題

noa は現在 `noa-config` の TOML パーサー(allowlist 制・未知キー hard fail)のみを読み、Ghostty ネイティブ構文(行指向 `key = value`)の config を一切解釈できない。Ghostty から乗り換える dotfiles 駆動ユーザーは、手元の Ghostty config 資産(繰り返しキーによる list 蓄積・空値リセット・行頭コメントなどの意味論を含む)をそのまま流用したいが、現行の TOML 専用パーサーと「未知キー即死」の挙動では、実際の dotfiles をコピーした瞬間に 1 個の未対応キーで起動不能になり JTBD が入口で成立しない。加えて theme-selection spec はこの TOML 前提の機構(`SUPPORTED_KEYS`・`toml_edit`)に依存しており、config 基盤を先に構文レベルで置き換えないと後続のキー拡張増分すべてに手戻りが波及する。

### 2. 提案する解決策

`noa-config` のパーサー部を、Ghostty 構文を解釈する I/O レスな純粋関数(`&str -> Result<Vec<Directive>, ...>`)として分離し、既存の `parse_overrides(path, source)` 分離パターンおよび noa-vt の Handler/Stream 規範を踏襲する。意味論は scalar last-wins・空値によるデフォルトリセット・行頭のみのコメント規則までを v1 で正しく実装し、list 型キー(`keybind`/`palette`/`font-family` 等)と `config-file` ディレクティブは「認識して専用 warn」で受け流す(汎用 unknown-key warn とは別文言にし、typo と「認識済み未実装」をユーザーが区別できるようにする。実ストレージ・include 実読込は次増分)。noa ネイティブ config は単一パス方式を維持しつつファイル名を `config.toml` と衝突しない新名 `config` に変更し、TOML 廃止後に設定が無言でデフォルトへ落ちる regression を構造的に回避する。未知キー・型不正値はいずれも warn + 継続とし、`load_startup_config() -> Result<(StartupConfig, Vec<Diagnostic>)>` の形で Diagnostic を構造体の外に蓄積し、設定構造体をこれ以上複雑化させない(注: theme-selection 増分の `theme: Option<String>` 先行追加により `Copy` derive は既に喪失済み — R-9 の訂正参照)。`window-width`/`window-height` は config 層でのみ Ghostty 意味論(両方指定必須・未達は warn + 無視、10×4 最小クランプ)を採用し、CLI `--cols`/`--rows`(noa 独自キー)は従来どおり独立指定を維持する。Ghostty 資産の取り込みは、ライブフォールバックではなく **`--import-ghostty-config` フラグによる明示実行**(noa 独自の convenience 拡張、Ghostty に対応機能なし)とし、実行時のみ Ghostty 側の実パス解決規則(4 候補全読み・後勝ちマージ)を忠実実装して読み込み、対応キーは noa 形式で書き出し、非対応キーはコメントアウトして可視化する。noa config が無くかつ Ghostty config が検出された場合は、起動時に **1 行の初回ヒント**(同じく noa 独自拡張)でこのフラグの存在を案内するのみに留め、自動書き込みは行わない。旧 `config.toml` が存在する場合は起動時に一度だけ検出 warn を出し、自動変換は L0 の「パーサー一本化」制約と矛盾するため実装しない。

### 3. In-scope

- Ghostty 構文パーサーを I/O レスな純粋関数として `noa-config` 内に分離(`&str -> Vec<Directive>` 相当)、unit test はファイル I/O 抜きで記述可能にする
- v1 で実装する構文意味論: `key = value`(`=` 前後空白無視)、行頭のみの `#` コメント、scalar は last-wins、空値 `key =` はデフォルトへのリセット
- `config-file` ディレクティブ: 認識して専用 warn(汎用 unknown-key warn とは別文言)。cycle detection・実読込は次増分
- list 型キー構文(繰り返しキー蓄積): 認識して専用 warn で受け流す。汎用 list 蓄積データ構造の実装は次増分に DEFER(v1 に消費者ゼロのため)
- noa ネイティブ config: 既存のパス発見機構(`default_config_path()` 系、単一パス、precedence: default < file < CLI)は温存し、ファイル名のみ `config.toml` から `config` に変更
- 未知キー: warn + 継続(hard fail から挙動反転)。既存テスト `unknown_key_is_rejected` を「未対応キーは warn して起動継続する」内容に書き換え
- 型不正値: warn + 継続(該当キーのみデフォルトへフォールバックし起動は続行)⚠B
- Diagnostic 蓄積を `StartupConfig`/`ConfigOverrides` の外に出し、`load_startup_config() -> Result<(StartupConfig, Vec<Diagnostic>)>` 形式で返す(設定構造体への Vec 混入を避ける — `Copy` は theme-selection 増分で既に喪失済みのため「温存」ではなく「これ以上複雑化させない」が目的。R-9)
- `window-width`/`window-height`(config キー): 両方指定必須(片方のみは無効 + warn)、10×4 最小クランプを採用 ⚠C。CLI `--cols`/`--rows` は noa 独自キーとして従来どおり独立指定可能なまま維持し、改名は行わない
- `--font-size` CLI フラグ: 変更不要(clap の kebab 化により既に Ghostty 名 `font-size` と一致済み)
- 旧 `config.toml` 検出時、起動時に一度だけ warn(自動変換なし)
- `theme` config キー(⚠E 採用 2026-07-03): 出荷済み theme 機能を Ghostty 構文で継続受理(文字列パススルー。`light:`/`dark:` ペア構文は専用 warn で不受理 — 部分受理禁止)。theme-selection.md 該当節の Ghostty 構文前提への改稿を実装増分に含める
- **`--import-ghostty-config` フラグ(noa 拡張 — fidelity ではない)⚠A**: 実行時に Ghostty 側の実パス解決規則(4 候補: `$XDG_CONFIG_HOME/ghostty/{config.ghostty,config}` および `~/Library/Application Support/com.mitchellh.ghostty/{config.ghostty,config}`、後勝ちマージ)を忠実実装して全読みし、対応キーを noa `config` 形式で書き出す。非対応キーはコメントアウトして出力に残し、ユーザーが手動で確認・移行できるようにする
- **初回起動ヒント(noa 拡張 — fidelity ではない)⚠A**: noa config が存在せず、かつ上記 4 候補パス探索で Ghostty config が検出された場合、`--import-ghostty-config` の使い方を案内する 1 行ヒントを表示する。自動書き込みは行わない

### 4. Out-of-scope

- **TOML 自動変換** — 「パーサー一本化」の L0 制約と直接矛盾(noa 独自スコープ判断、fidelity gap ではない)
- **初回起動時の自動 import(フラグなし)** — ⚠A 裁定でフラグ方式を採用、無操作でのファイル書き込み副作用を回避(fidelity gap ではない — Ghostty に import 機能自体が無い)
- **汎用 `--<key>=<value>` CLI** — 3 キーのための speculative generalization として CUT(Ghostty は全 config キーを CLI フラグとして受けるため **fidelity gap として文書化**)
- **`config-file` の実読み込み(再帰 include・循環検出・末尾遅延処理)** — 認識+warn に留める(**fidelity gap として文書化**)
- **list 値の蓄積ストレージ実装** — 認識+warn に留める(**fidelity gap として文書化**)
- **ライブ config reload(`cmd+shift+,` / SIGUSR2)** — 起動時一回読みの現行アーキテクチャを維持(**fidelity gap として文書化**、parity-plan Phase 3 の既計画)
- **GUI「Configuration Errors」ダイアログ** — noa に GUI ダイアログ基盤なし、diagnostics は stderr/log のみ(**fidelity gap として文書化**)
- **サブコマンド基盤の新設** — import はフラグで実現、bin は flags-only 維持(theme spec の `+list-themes` DEFER と同じ制約判断)
- **`window-save-state` 相互作用** — noa に該当機能なし(機能非存在として **fidelity gap 文書化**)
- **`keybind`/`font-family`/`palette` 等の list 型キー意味論拡張** — 別 spec・別増分(`theme` は ⚠E 採用〔2026-07-03〕により v1 認識スカラーキーへ昇格 — R-8)

### 5. 前提(Assumptions)

- v1 の対象キーは構文基盤 + 認識スカラー4キー(`window-width`/`window-height`/`font-size`/`theme`〔⚠E〕)。`keybind`/`font-family` 等の list 型キー拡張は別 spec・別増分
- noa ネイティブ config のパス発見機構(単一パス、precedence モデル)は変更しない。変更はファイル名(`config.toml` → `config`)とパーサー部のみ
- Ghostty 側 4 候補パスの全読みマージは「`--import-ghostty-config` 実行時」と「初回ヒント表示のための検出判定時」にのみ使用し、通常起動時に noa が Ghostty config をライブで読むことはない(方向 B の帰結)
- `noa-app` は `noa-config` に依存しない既存 DAG 境界を維持し、キー名マッピングは `bin/noa/src/main.rs` の一箇所で吸収。`AppConfig`(cols/rows/font_size)のフィールドは無変更(Ripple 前提)
- 実装順序は ⚠D 裁定(**ただし 2026-07-03 に前提崩れを確認 — CHALLENGE 節の追記参照**): 当初裁定は「本 spec 実装 → theme-selection 改稿 → theme orbit 起動」だったが、theme-selection の orbit loop が既に実装進行中のため実質 (b) theme-selection 先行に逆転。L1/L2 は現物コード(theme フィールド追加済み)基準で再整合済み。最終順序は LOCK 時にユーザー確認

## Considered but rejected

EXPAND チェックポイント(2026-07-02、ユーザー裁定): **方向 B(Faithful Import)を単独採択**。

- **A. Fast Path(最小パーサー + ライブフォールバック)** — 却下: 実 dotfiles が「読めるが意味的に不完全」になり、後続増分で構文機構の手戻り。ライブフォールバックは未対応キーの warn 洪水(Flux #4)も内包。
- **C. Include Bridge(暗黙 `config-file` 注入)** — 却下: 「暗黙 include」は Ghostty に無い noa 独自合成挙動で、忠実クローン方針と緊張。
- **E. Layered Merge(常時 2 ファイルマージ)** — 却下: FRAME 裁定「無い場合のみ」から逸脱、2 ファイル precedence のデバッグ体験悪化、Ghostty に無い概念。
- **初回起動時の完全自動 import** — 却下(⚠A 暫定): 無操作でのファイル書き込み副作用。Magi 2-1 多数派案だったが Ripple mitigation 5 のフラグ + ヒント案を採択。対立評決として記録。
- **import 機構の全 CUT(cp + docs のみ)** — 却下(⚠A 暫定): Void conf 75-80。FRAME「両方」裁定の実質縮小になるため不採択。対立評決として記録。
- **型不正値の fail-fast 維持** — 却下(⚠B 暫定): Magi 2-1 多数派案。「未知キーは warn、型 typo は起動中断」の中途半端な忠実度(Flux #3)を避けるため warn+継続を採択。対立評決として記録。
- **noa 自身のパスへの 4 候補マージ移植** — 却下: Ghostty 固有の歴史的遺産(bundle-id 変更・拡張子移行)の解決機構であり noa に該当遺産なし。[Void CUT 90]
- **Ghostty 風 Diagnostic 型・severity taxonomy の模倣** — 却下: 消費者たる GUI ダイアログが noa に存在しない。[Void 4c CUT 80]
- **TOML→新形式の自動変換** — 却下: 削除対象の旧パーサーを生かし続けることになり「パーサー一本化」と矛盾。[Magi 3-0 / Void 90]

## Open Questions / Deferred Decisions

**裁定確定記録(2026-07-03 ユーザー承認)**

- **⚠A 確定**: `--import-ghostty-config` フラグ + 初回起動ヒント(自動書き込みなし)。対立評決(自動 import / 全 CUT)は Considered but rejected に記録済み。
- **⚠B 確定**: 型不正値は warn + 継続(R-7)。
- **⚠C 確定**: 両方必須は config 層のみ、CLI `--cols`/`--rows` は独立のまま(R-12/R-14)。
- **⚠D 確定**: theme-selection loop DONE を受け、本 spec が次の実装増分。theme-selection.md 該当節(R-1/R-2/L2 noa-config/AC-1〜3)の Ghostty 構文前提への改稿を実装増分に含める。
- **⚠E 確定**: `theme` を v1 認識スカラーキーに含める(R-8 改訂・AC-15/51 反映済み)。

**その他の未解決事項**

- import で書き出す noa `config` に出所ヘッダーコメント(インポート元・日時)を残すか — トレーサビリティ論点、未裁定
- `config-file`/list 型キーを「認識+warn」から実読込へ移行する次増分の着手条件 — 未定
- noa ネイティブ config ファイル名 `config` に拡張子を持たせるか(Ghostty は 1.2.3+ で `.ghostty` 拡張子を追加した前例)— 本 increment は拡張子なし前提で進める
- 未対応キー warn の表示形式 — キー毎個別行 vs 集約表示(「N 個の未対応キーを無視」)— Flux #4、未裁定
- 起動時 diagnostics の具体的出力先(stderr のみ / ログファイル併用)— GUI ダイアログ除外は確定、代替出力の形式は SPECIFY で確定

## Spec Quality Gate 記録

- **Run 1(2026-07-03, Judge + Attest)**: **GATE FAIL**。
  - Judge blocking: F1(診断出力を `log::warn!` 前提にすると `env_logger` デフォルトフィルタ `Off` により不可視 — 実コード検証済み)/ F2(Scope の「Copy 温存」記述が R-9 訂正と矛盾)/ F3(⚠D 前提崩れが Assumptions に未反映)/ F4(R-7 の window 系型不正 AC 欠落)。非 blocking: F5〜F12。
  - Attest: LOCK-ready NO — 13/51 AC が引数なし API 形状(`default_config_path()`/`ghostty_config_candidates()`/`import_ghostty_config()`)起因で非 hermetic、ヒント出力 sink 未定義。
- **改修(2026-07-03)**: 全 blocking + 主要 non-blocking を反映 — 出力 sink を stderr `eprintln!` に確定(F1, AC-48)/ Scope 2 箇所の Copy 記述訂正(F2)/ Assumptions に ⚠D 前提崩れ注記(F3)/ AC-43・44 追加(F4, F11)/ R-12 に リセット=未記載 同一視を明記 + AC-46(F5)/ 埋め込みクォート規則 + AC-49(F6)/ R-16 トリガー条件明確化 + AC-47(F7)/ AC-42 を決定性のみに純化し純粋性は AC-45 grep 検査へ(F8, Attest#7)/ 2レーンエラーモデル明記(F9)/ CRLF・BOM 規則 + AC-50(F10)/ Out-of-scope にキー拡張を明記(F12)/ L2 API を「純粋関数 + 薄いラッパー」に分割(`default_config_path_in`・`ghostty_config_candidates_from`・`build_import_output`・`import_ghostty_config_at`・`load_startup_config_from`・`import_hint`)し AC-16/19/20/26/28〜36 を hermetic 形式に書き換え(Attest #1〜6)/ NFR-1 の dev-dependency 適用範囲明記(Attest #9)。
- **Run 2(2026-07-03, 独立検証)**: **GATE PASS**。Judge blocking 4件 + non-blocking 8件 + Attest blocker 全件の FIXED を実文確認。トレーサビリティ表(AC 51件)の幽霊参照・欠落なし。指摘は AC-43/44/46/49/50 の見出し配置のみ(→ 整形済み: 各 thematic 節へ移設)。
- **LOCK 前提条件充足(2026-07-03)**: ⚠A〜E ユーザー確定 + サインオフ取得。⚠E 採用に伴う R-8/AC-15/AC-30/AC-51/テスト計画/Scope の整合更新済み(AC 総数 52)。

## Build-path decision

**orbit loop(engine: codex)** — 2026-07-03 LOCK 時にユーザー選択。

- 本 spec の L3 AC 52 件を `nexus-autoloop` の完了契約(machine-checkable DONE ゲート)とするランナーを orbit が生成する。theme-selection と同じ build-path(orbit/codex)。
- Codex 実行の前提条件(`~/.codex` の `multi_agent = true` + `[agents] max_depth >= 2`、`-o` アーティファクト捕捉)は起動前に確認すること。
- 実装増分に含める付帯作業: theme-selection.md 該当節(R-1/R-2/L2 noa-config 節/AC-1〜3/Copy-derive 段落/Open Questions のパス記述)の Ghostty 構文前提への改稿(⚠D 確定)。
