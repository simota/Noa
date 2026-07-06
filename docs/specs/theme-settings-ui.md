# Spec: テーマライブプレビュー + 設定変更UI (theme-settings-ui)

## Metadata

- **slug:** theme-settings-ui
- **title:** テーマライブプレビュー + 設定変更UI (Theme Live Preview + Settings UI)
- **status:** **locked**(サインオフ 2026-07-06)
- **owner:** simota
- **build-path:** **apex(一気通貫)** — 詳細は末尾「Build-path decision」
- **recipe:** /nexus spec — FRAME ✓ / EXPAND ✓ / CHALLENGE ✓ / SHAPE ✓ / SPECIFY ✓ / Quality Gate PASS(Judge 再検査済) / LOCK ✓
- **upstream:** `theme-selection.md`(locked — テーマカタログ・起動時適用)、`ghostty-config.md`(locked — config形式・writer方式の準拠先)

## L0 — Vision

- **問題:** noaは574個のGhostty互換テーマ(`noa-theme`)と設定ファイル基盤(`noa-config`, `~/.config/noa/config`)を持つが、設定変更の手段は「外部エディタでファイル編集→再起動」しかない。テーマは`GpuState`初期化時に一度だけ解決され、実行時に切り替える経路が存在しない。
- **提供価値:** アプリ内の汎用設定UI(テーマ・フォント・透過等)を新設。テーマピッカーは一覧+サンプルペイン+実画面への即時反映のライブプレビューを提供。確定時に`~/.config/noa/config`へ書き戻す。
- **対象:** noaユーザー本人(単一ユーザー・ローカルアプリ)。
- **成功definition:** 再起動なしでテーマを閲覧・試用・確定でき、確定値が次回起動でも有効(config書き戻し)。

## FRAME — 再利用資産と制約 (Lensスキャン確定)

### 再利用資産
- `noa-theme`: 574 Ghostty互換テーマのコンパイル時カタログ (`ThemeDef`, `resolve(name)` バイナリサーチ)
- `noa-config`: `~/.config/noa/config` key=value パーサ、`theme`キー対応済、CLI > file > default のマージ、Ghostty configの一回性import
- `noa-app/src/theme.rs`: `resolve_theme_with_overrides(name, &ThemeOverrides)` → `noa_render::Theme`
- `noa-render/src/theme.rs`: `OverlayStyle::from_theme()` — モーダルUI用パレット導出(オンデマンド計算なのでテーマ切替に自動追従)
- OSC 4/10/11 動的カラー経路: `TerminalColors` → `Theme::resolve_with_colors` 毎フレーム解決 — ライブプレビューの相乗り先候補
- UIコンテナ前例: command_palette (ファジーリストカード) / tab_overview・overview (独立ウィンドウ型オーバーレイ)

### 技術制約
1. `gpu.theme` は起動時一度きり代入 (`noa-app/src/app.rs:922-942`) — 更新経路の新設が必要
2. `chrome::ACTIVE_PALETTE` が `OnceLock` 単一書込 (`noa-app/src/chrome.rs:129-147`) — 可変化必須
3. サイドバー/パレットのGPUテクスチャ群がテーマ色焼き込み済み (`app.rs:943-954`) — テーマ切替時に再構築必要
4. configファイル監視は存在しない — 「ライブプレビュー(セッション内)」と「永続化(config書込)」は別メカニズムとして設計する
5. 574テーマカタログは `&'static` — ピッカー一覧には十分、ユーザー定義テーマは別経路(今回out-of-scope)
6. `minimum_contrast`・検索ハイライト・`OverlayStyle` はテーマから導出 — 再解決の一括実行で追従させ、個別パッチ禁止

## ヒアリング確定事項 (FRAME)

- スコープ: **汎用設定UI** (テーマ中心だがフォント・透過等の主要設定も一覧・変更可能)
- テーマ源: Ghostty同梱互換 (= 既存の`noa-theme` 574テーマをそのまま使用)
- プレビューUX: **両方** — 一覧+サンプルペイン+選択中テーマの実画面即時反映 (Esc復帰/Enter確定)
- 永続化: **設定ファイル書き戻し** (`~/.config/noa/config` の該当キー更新、起動時読込)

## Out-of-scope (FRAME確定)

- config リロードキーバインド (Ghostty cmd+shift+, 相当)
- `theme = light:X,dark:Y` 自動切替構文
- ユーザー定義テーマファイル (`~/.config/noa/themes/`)
- configファイル監視(外部編集の自動反映)

## EXPAND — 候補方向 (実施済・ユーザー反応済)

- **候補1: OSCオーバーライド相乗りプレビュー** ← ユーザー選択(主軸)。ウィンドウ内オーバーレイ(コマンドパレット様式)。プレビューは既存OSC 4/10/11動的カラー経路に全パレット注入(本文ライブ、クロムは確定時のみ)。設定項目は同オーバーレイ内第2セクション。
- 候補2: 独立設定ウィンドウ+分離プレビュー(overview前例・専用ミニターミナル) — 不採用
- 候補3: 実セッション完全ライブスワップ(ハイライト毎にテクスチャ再構築) — 不採用
- 候補4: A/Bルーレット比較(2分割ペア比較) — 不採用
- Flux提案「おすすめ〜20キュレーション」「永続化=副作用(Enter即書込)」 — どちらも不採用。**フラット574一覧+明示的な保存操作**とする。

## CHALLENGE — 選定 (完了・ユーザー裁定済)

**確定方向: 候補1改 — ウィンドウ内オーバーレイ + `preview_theme` 差替プレビュー + Enter確定書込**

ユーザー裁定 (2026-07-06):
1. **サンプルペイン: 残す** (Magi裁定採用) — 16 ANSI色+truecolor固定見本。空画面でも全色確認可能
2. **設定v1範囲: やや広め** — ライブ適用4行 (font-size / background-opacity / background-blur-radius / cursor-style。opacityとblurは概念上ペア) + 確定時のみ適用の追加行 (font-family, window-padding 等) も一覧に並べる
3. **保存: Enter確定=適用+config書込の単一ジェスチャ**、Escで全取消。Save専用ボタンなし
4. **プレビュー機構: `preview_theme: Option<Theme>` をdraw時に`gpu.theme`の代わりに参照** (TerminalColors注入は不採用) — 選択色・検索色・OverlayStyleまで全プレビュー、ターミナル状態無汚染

Magi裁定 (採用):
- プレビュー中はオーバーレイに「クロム/タブはSaveで更新」バッジを表示 (クロムdimming等のGPU工事はしない)
- クロム更新の再起動送りは否決 — 確定時ワンショット完全スワップ (OnceLock→可変化 + Optionテクスチャ約12個をNoneリセット→lazy-init自動再構築)
- ライブ/確定のみの線引きは「毎フレーム解決可能か」の機構基準: opacity・cursorは即時、font-sizeはdebounce(~150ms)、font-familyは確定のみ

Ripple実現性検証 (要点):
- `preview_theme`差替: `Theme`は毎フレーム`resolve_with_colors`で参照されるため差替は低リスク。焼込クロムのみ非対象
- 確定スワップ爆発半径: ~3ファイル (`chrome.rs`/`app.rs`/`app/state.rs`)、MEDIUM
- **config書込は新規工事**: `noa-config`は現状パース専用。locked spec `ghostty-config.md`に従いGhostty流 line-oriented `key = value` 形式(TOML廃止済)。コメント・未知キー保持のsurgical更新が必要
- font-sizeライブ適用は既存`runtime_font_size`経路 (`app/input_ops.rs:30-70`) 再利用
- **background-opacityの不透明→透明遷移は不可** (winitウィンドウ生成時固定) — SPECIFYで扱い決定要
- CLIフラグ(--font-size等)と書込値の優先関係は製品判断が必要 — SPECIFYで決定要

### Considered but rejected
- 候補2 独立設定ウィンドウ+分離プレビュー — 「実画面即時反映」を満たさない・スワップ経路2系統化
- 候補3 ハイライト毎の完全ライブスワップ — テクスチャ再構築のスクラブ速度性能が未検証でリスクがライブセッション直撃
- 候補4 A/Bペア比較 — レンダ工数大・汎用設定に一般化不可
- TerminalColors注入プレビュー — 選択色・検索色非対象、プログラムOSC状態との衝突ケア必要 (preview_theme差替が優位)
- キュレーション済み〜20テーマ既定表示 — ユーザー不採用 (フラット574一覧+fuzzy検索)
- 永続化=副作用(kitty流) — ユーザー不採用 (Enter明示確定)
- クロム更新の再起動送り — Magi否決 (Save直後の半適用状態は不具合に見える)
- 関連: 既存locked spec `theme-selection.md` (カタログ・起動時適用は実装済の土台)。同specの範囲外だった「一覧確認・試着 (JTBD-3, v1 DEFER)」を本specが引き受ける

## SHAPE — 提案 (承認済 2026-07-06、全件推奨デフォルト採用)

### Proposed solution
- **起動トリガー:** コマンドパレット新規エントリ「テーマ・設定を開く」。既存⌘,(外部エディタでconfig)は変更せず並存。
- **レイアウト:** command_palette様式のウィンドウ内オーバーレイ。第1セクション=テーマピッカー(左: fuzzy検索付き574件フラット一覧 / 右: 16 ANSI+truecolor固定サンプルペイン)。第2セクション=設定rows(同オーバーレイ内下部、スクロール)。
- **キー操作:** ↑↓行選択、文字入力で型フィルタ、Tabでテーマ/設定セクションフォーカス切替、数値行は←→or直接入力。Enter確定、Escで全プレビュー取消・復帰。
- **プレビュー機構:** `preview_theme: Option<Theme>`をdraw時に`gpu.theme`の代わりに参照。本文・選択色・検索色・OverlayStyle全対象。設定行は毎フレーム解決可否で即時/デバウンス/確定のみに分岐。プレビュー中は「クロム/タブはSaveで更新」バッジ表示。
- **Enter確定シーケンス:** ①クロム完全スワップ(`ACTIVE_PALETTE`可変化+焼込テクスチャ約12個Noneリセット→lazy-init再構築) ②config surgical書込(ghostty-config形式、変更キー行のみ置換、コメント・未知キー・他行保持)。単一ジェスチャで両方実行、半端な適用状態を作らない。

### Settings rows (v1, 計7行 — 確定)
| キー | ウィジェット | ライブ | 備考 |
|---|---|---|---|
| font-size | 数値インライン(←→/直接入力) | live debounce ~150ms | 適用は既存`runtime_font_size`経路(app/input_ops.rs:30-70)を呼ぶ。debounce自体は新規の小規模タイマー状態機械(GPU呼出から分離した純ロジック) |
| background-opacity | 数値(0.0–1.0) | live即時(透明起動時のみ) | blur-radiusと対 |
| background-blur-radius | 数値 | live即時(同条件) | opacity=1.0時は無効化表示 |
| cursor-style | サイクル行(block/bar/underline) | live即時 | 既存カーソルモード切替流用 |
| font-family | サイクル行(fuzzy無し — 改訂 2026-07-06) | 確定のみ(永続化のみ・次回起動反映) | フォント再構築コスト高 |
| window-padding-x/y | 数値(2キー1行、両軸同時ステップ — 改訂 2026-07-06) | 確定のみ(同上) | グリッド再計算を伴う |
| macos-titlebar-style | サイクル行 | 確定のみ(同上) | ウィンドウchrome再構築を伴う |

### 確定済み判断 (旧Open questions、全件推奨採用)
1. CLIフラグはセッション限定オーバーライド。設定UI確定時のconfig書込はCLI値を上書き反映しない(既存優先モデル維持)
2. 不透明(opacity=1.0)起動時: opacity/blur行の編集・書込は可、プレビューなし+「再起動後に反映」ノート表示
3. 確定は全ウィンドウへ即時伝播(プロセス単一状態の自然な帰結。SPECIFYで実現性検証)
4. サンプルペインは左リスト+右サンプルの横並び
5. 設定rowsはv1で7行固定。追加要望は別増分
6. surgical書込はghostty-config import writer(`build_import_output`)の「元行テキスト保持+対象キー行のみ置換」方式踏襲、新規パーサー機構なし

### Assumptions
- オーバーレイは単一ウィンドウで開く。確定スワップはプロセス単一状態のため他ウィンドウへ自動伝播
- font-family一覧は既存font-kit discoveryをそのまま使用、テーマ一覧と同じfuzzy検索UX
- opaque→transparentのランタイム遷移は不可(winitウィンドウ生成時固定)という既存制約は変更しない

## L1 — Requirements

### オーバーレイ起動・構成
- **R-1**: コマンドパレットに新規エントリ「テーマ・設定を開く」を追加し、単一ウィンドウ内オーバーレイを開く。パレットのエントリ選択はパレットを同期的に閉じてからコマンドをディスパッチする(既存パレット挙動を踏襲)ため、R-3の相互排他と起動導線は矛盾しない。既存⌘,(外部エディタ起動)は変更せず並存する。
- **R-2**: オーバーレイは command_palette 様式(独立ウィンドウではない)。第1セクション=テーマピッカー(左:574件フラット一覧+fuzzy検索/右:サンプルペイン)、第2セクション=設定rows(スクロール)。Tab で2セクション間のフォーカスを切替える。キー操作モデルは全行で統一: ↑↓=行選択(値調整には使わない)、←→=フォーカス行の値調整(数値行のステップ増減・サイクル行の巡回)、数値行は直接入力も可。
- **R-3**: 他のオーバーレイ(コマンドパレット・検索)が開いている間はテーマ・設定オーバーレイを開始できない(相互排他)。逆に本オーバーレイが開いている間は他オーバーレイの起動ショートカットを無視する。

### テーマピッカー・プレビュー
- **R-4**: 574件フラット一覧を表示し、文字入力でリアルタイム fuzzy フィルタする(キュレーション表示なし)。
- **R-5**: サンプルペインは16 ANSI色+truecolor固定見本を常時表示する(空画面でも全色確認可能)。
- **R-6**: 一覧でのハイライト変更を `preview_theme: Option<Theme>` に反映し、本文・選択色・検索色・`OverlayStyle` の全てが次フレームまでに切り替わる。ターミナルの実状態(`TerminalColors`)は無汚染のまま維持する。
- **R-7**: プレビュー中はオーバーレイ内に「クロム/タブはSaveで更新」バッジを表示する(クロム自体の外観は変更しない)。

### 設定rows(v1, 7行固定)
- **R-8**: font-size / background-opacity / background-blur-radius / cursor-style の4行はライブプレビュー対象。font-family / window-padding-x,y / macos-titlebar-style の3行は**確定時にconfigへ永続化のみ行い、反映は次回起動時**とする(改訂 2026-07-06、ユーザー裁定 — ランタイム適用経路が存在しないため)。この3行がtouchedの場合、R-11と同じ「再起動後に反映」ノートを行に表示する。各行の分類はウィジェット単位で固定し、実行時に切替不可とする。
- **R-9**: font-size 行は ~150ms のデバウンスを経て既存 `runtime_font_size` 経路(`noa-app/src/app/input_ops.rs:30-70`)を呼び出して適用する。デバウンスは新規の小規模タイマー状態機械として実装し(既存コードにデバウンス機構は存在しない)、GPU呼出から分離した純ロジックとして単体テスト可能にする。
- **R-10**: background-opacity/background-blur-radius は即時反映(opaque起動時を除く)。cursor-style は即時反映。
- **R-11**: 不透明(opacity=1.0)で起動した場合、opacity/blur行は編集・確定時の書込みは可能だがプレビューは行わず、「再起動後に反映」の注記を行に表示する(opaque→transparent のランタイム遷移不可という既存制約はそのまま)。

### 確定シーケンス(commit)
- **R-12**: Enter は「config書込み」→「クロム完全スワップ」の順で単一ジェスチャとして実行する。書込み(失敗しうる唯一の段)が先: 書込みが失敗した場合はクロムスワップを行わず、プレビュー状態を維持したままオーバーレイ内にエラーを表示する。書込み成功後のクロムスワップはインメモリ操作のみで実質失敗しないため、「片方のみ適用された中間状態」は構造的に発生しない。
- **R-13**: クロムスワップは `chrome::ACTIVE_PALETTE` を可変化した上で新テーマ値へ差し替え、焼込済みGPUテクスチャ群(`app.rs:943-954`、約12個)を `None` にリセットして次回描画時の lazy-init で再構築させる。
- **R-14**: config書込みは `~/.config/noa/config`(ghostty-config形式)に対し、変更のあった行のみをテキスト置換し、コメント・未知キー・他キーの行および元の行順を保持する(surgical update)。書込みはテンポラリファイル+rename により原子的に行う。
- **R-15**: config ファイルが存在しない場合、新規作成して確定した設定値のみを書き込む(既存キーが無いため全行が新規追加行になる)。
- **R-16**: Esc は `preview_theme` を `None` に戻し、設定rowsの編集下書き(draft値)を破棄して、オーバーレイを開く直前の状態に完全復帰する。config書込みは一切行わない。

### 優先順位・伝播
- **R-17**: CLIフラグ(`--font-size` 等)によるセッション限定オーバーライドは、確定時の config 書込み値には反映しない(書込み値は常にオーバーレイ操作前の config 値+今回の変更分のみ)。既存の CLI > file > default モデルは変更しない。
- **R-18**: 確定(commit)はプロセス内の全ウィンドウ状態を走査して各ウィンドウに再描画(`request_redraw`)を要求し、各ウィンドウの次回描画フレームで新テーマ・新設定が反映される(バックグラウンドウィンドウが古いクロムのまま放置されない)。

### 非機能要件
- **NFR-1(プレビュー遅延)**: ハイライト変更(テーマ一覧・cursor-style・opacity/blur)からの反映は次フレーム描画までに完了する。プレビュー解決+再描画要求の処理は1フレームバジェット(60Hz基準で約16ms)以内に収まること。
- **NFR-2(スクラブ性能)**: 一覧ハイライトのスクラブ中および font-size デバウンス待機中に、GPUテクスチャの再構築(atlas/焼込テクスチャの再生成)を発生させない。テクスチャ再構築はEnter確定時の一度のみ。
- **NFR-3(書込み原子性)**: config書込みはテンポラリファイル+rename方式とし、書込み途中でのプロセス終了・クラッシュがconfigファイルを破損させない。
- **NFR-4(config欠如時の継続性)**: config ファイルが存在しない状態からの確定は失敗せず、新規ファイルを作成する。
- **NFR-5(surgical性)**: 書込み後、変更対象キー以外の行(コメント・未知キー・キー順)は書込み前後でバイト単位一致する。
- **NFR-6(CLI非汚染)**: CLIオーバーライドが有効なセッションでオーバーレイを確定しても、config書込み値にCLI由来の値が混入しない。

## L2 — Detail

### noa-app(主変更点)
- 新規モジュール(例 `noa-app/src/theme_settings.rs` 相当)にオーバーレイ状態を保持: `preview_theme: Option<Theme>`、設定rows の draft 値(font_size/opacity/blur/cursor_style は即時反映用、font_family/padding/titlebar は確定のみのdraft保持用)。
- draw経路: 現在 `GpuState.theme`(`app.rs:922-942`で起動時一度だけ代入)を直接参照している箇所を、`preview_theme.as_ref().unwrap_or(&gpu.theme)` を経由する解決関数に差し替える。`Theme::resolve_with_colors` 呼び出し(OSC動的色との合成、毎フレーム実行)はこの解決後の `Theme` を受け取るだけで変更不要。
- `chrome::ACTIVE_PALETTE`(`chrome.rs:129-147`、現状 `OnceLock` 単一書込)を可変な保持(`RwLock`/`Mutex` 等)に変更し、確定時に差し替え可能にする。差し替えロジックは `chrome.rs` 単体で `GpuState` 不要のユニットテスト対象(AC-9)。
- 焼込テクスチャ約12個(`app.rs:943-954` の `Option` フィールド群)は、リセット可能なサブ構造体(例 `ChromeTextures`)に集約し `reset()` メソッドを与える。`Option` を `None` に戻す操作は純粋なため、この構造はGPUデバイスなしでユニットテスト可能(AC-20)。lazy-init 再構築は既存の draw 経路がそのまま担う。
- サブ構造体には debug ビルド限定の再構築カウンタ(`AtomicUsize` 等)を持たせ、lazy-init 実行回数を計測可能にする(NFR-2/AC-18 の検証手段。既存コードに計測フックが無いことをGateで確認済みのため、これは実装の納品物)。
- 確定時の順序(R-12): (1) `noa-config` 新設 writer による config書込み — 失敗時はここで中断しエラー表示、(2) 書込み成功後に `ACTIVE_PALETTE` 差し替え + `ChromeTextures::reset()` + `gpu.theme` 更新、(3) 全ウィンドウ走査で `request_redraw`(R-18)。単一関数内で同期実行する。
- font-size のデバウンスは新規の小規模タイマー状態機械(入力: タイムスタンプ付き値変更列 → 出力: 発火すべき最終値)として GPU 呼出から分離したモジュールに実装し、発火時に既存 `runtime_font_size` 経路(`app/input_ops.rs:30-70`)を呼び出す。
- 各タブ/ウィンドウの `Terminal` 生成箇所には手を入れない(`preview_theme` は grid の `TerminalColors` に注入しない — 確定裁定4)。

### noa-config(新規: writer)
- 新規モジュール `src/writer.rs`(仮)。ghostty-config 増分の `parser.rs`(`parse_directives` → 行番号付き `Directive`)を再利用し、`build_import_output`(`import.rs`)と同型の「純粋関数(文字列処理)+ I/Oの薄いラッパー」構成を踏襲するが、機能は逆方向: 既存configの生テキストを読み、変更対象キーに対応する `Directive.line` の行だけを新しい `key = value` 行に置換し、他の全行(コメント・未知キー・list型キー行含む)を元のテキストのまま保持する。対象キーが元テキストに存在しない場合は末尾に新規行として追記する。
- I/O部はテンポラリファイル書込み+`rename`で原子性を担保する(NFR-3)。書込み先は `default_config_path()`(ghostty-config スペック定義済み、`<config_dir>/noa/config`)。
- config ファイル自体が存在しない場合は空文字列を入力として同じ置換ロジックを通す(全行が新規追記になる、NFR-4)。
- CLI由来の値は本 writer の入力に含めない(呼び出し元の `noa-app` がオーバーレイのdraft値のみを渡す、R-17/NFR-6)。

### noa-render
- 変更なし。`OverlayStyle::from_theme()` はオンデマンド計算のため、`noa-app` 側が渡す `Theme` が差し替われば自動追従する(既存資産の再利用のみ)。

### エッジケース
- **fuzzy検索が空一致**: 一覧を空表示にし、サンプルペインは直前にハイライトしていたテーマ(またはオーバーレイを開いた時点のアクティブテーマ)を保持したままにする。`preview_theme` を勝手に `None` へ戻さない。
- **config に存在しないテーマ名で起動している状態でオーバーレイを開く**: 既存の起動時フォールバック(デフォルトテーマ+warn、`theme-selection.md` R-3)で解決済みのテーマがアクティブテーマとして表示される。オーバーレイ内では不正名を再現しない。
- **config に CLI オーバーライドが効いている状態で開く**: ピッカー/設定rowsの初期選択はセッション中のアクティブ値(CLI由来含む)を表示するが、確定時のconfig書込みはCLI由来の値を書かない(R-17)。
- **font-size のデバウンス中に Esc**: デバウンスタイマーを破棄し(未発火の値変更は捨てる)、既に発火済みの変更があれば `runtime_font_size` をオーバーレイを開いた時点の値へ戻す。
- **他オーバーレイが開いている間の起動要求**: R-3により起動要求自体を無視する(エラー表示なし、無音で無視)。

## L3 — Acceptance Criteria

検証手段の凡例: [unit]=GPU不要のユニットテスト / [integration]=tempdir等を使う結合テスト / [code-review]=実装検査 / [GUI目視]=手動確認。

### 起動・レイアウト
- **AC-21 (R-1)** [unit]: Given コマンドパレットで「テーマ・設定を開く」を選択する。When コマンドディスパッチ後の状態を検査する。Then パレットは閉じており、テーマ・設定オーバーレイが開いている。
- **AC-22 (R-2)** [unit]: Given オーバーレイが開いている。When Tab を押す。Then フォーカスセクションがテーマピッカー⇄設定rowsで切り替わる。また設定rowsフォーカス中、↑↓は行選択を移動し、←→はフォーカス行の値を変更する(キールーティングの状態機械テスト)。
- **AC-17 (R-3)** [unit]: Given コマンドパレットが開いている。When テーマ・設定オーバーレイの起動を要求する。Then 何も起きない(オーバーレイが開かない)。

### プレビュー
- **AC-1 (R-6)** [unit]: Given `preview_theme` に別テーマを設定する。When 解決関数(`preview_theme.as_ref().unwrap_or(&gpu.theme)` 相当)の出力を `resolve_with_colors` に通して検査する。Then (a)本文デフォルトfg/bg (b)選択色 (c)検索ハイライト色 (d)`OverlayStyle::from_theme` 出力、の4点がそれぞれ新テーマの値に一致する(描画ループ不要、リゾルバ直接呼び出し)。
- **AC-2 (R-6)** [unit]: Given `preview_theme` が `Some` の状態。When `TerminalColors` の内部状態を検査する。Then プレビュー前と一致し変更されていない(無汚染性)。
- **AC-3 (R-5)** [unit]: サンプルペインの表示データが16 ANSI色全て+truecolor見本を含むことをデータ構造レベルで検証する。実際のグリフ描画は確定前後のGUI目視スポットチェックで補完(任意)。
- **AC-4 (R-7)**: (a) [unit] `preview_theme.is_some()` のときバッジ表示フラグが立ち、`None` で消えること。(b) [GUI目視] プレビュー中にクロム自体の外観が確定まで変化しないこと。

### 設定rows
- **AC-5 (R-8, R-10)** [unit]: Given cursor-style行の値を変更する。When オーバーレイ状態を検査する。Then カーソル描画モード(enum)が即時に新値へ切り替わっている。
- **AC-6 (R-9)** [unit]: Given デバウンスタイマー状態機械にタイムスタンプ付きの値変更列(間隔<150ms)を入力する。When 150ms経過をシミュレートする。Then 発火は1回のみで、最後の値が出力される(GPU呼出から分離した純ロジックのテスト)。
- **AC-7 (R-11)**: (a) [unit] opacity=1.0起動状態でopacity/blur行を編集したとき、プレビュー適用フラグが立たず「再起動後に反映」注記フラグが立つこと。(b) [GUI目視] 実画面でプレビューが起きないこと。書込み自体はAC-11系で検証。

### 確定(commit)・Esc
- **AC-8 (R-16)** [unit]: Given `preview_theme=Some` かつ設定rows draft が変更済み。When Esc を押す。Then `preview_theme` が `None` に戻り、draft値が破棄され、config書込み(注入したモックwriter)の呼び出し回数が0である。
- **AC-9 (R-13)** [unit]: `chrome.rs` 単体で、可変化した `ACTIVE_PALETTE` を新パレットに差し替え、読み出しが新値を返すこと(`GpuState` 不要)。
- **AC-20 (R-13)** [unit]: `ChromeTextures::reset()` を呼ぶと全 `Option` フィールドが `None` になること(GPUデバイス不要の純粋操作)。
- **AC-23 (R-12)** [unit]: Given 注入したモックwriterが書込み失敗を返す。When Enter確定を実行する。Then クロムスワップ・`gpu.theme` 更新は実行されず、`preview_theme` は維持され、エラー表示フラグが立つ。
- **AC-10 (R-12)** [code-review]: commit関数がconfig書込み成功→クロムスワップ→再描画要求を単一関数内で同期実行し、途中でフレーム描画が割り込める構造になっていないことを実装検査で確認する(+任意のGUI目視スポットチェック)。

### config書込み
- **AC-11 (R-14, NFR-5)** [unit]: Given コメント・未知キー・複数の既存キーを含むconfigテキスト。When 1キーだけを変更してwriterを通す。Then 出力は変更キーの行以外が入力とバイト単位で完全一致する(純関数のラウンドトリップテスト、`build_import_output` テスト群と同型)。
- **AC-12 (R-15, NFR-4)** [integration]: Given tempdir上にconfigファイルが存在しない状態。When 設定を1件変更して確定する。Then 新規ファイルが作成され、変更した値が書き込まれ、処理が失敗しない。
- **AC-13 (NFR-3)** [code-review]: テンポラリファイル書込み後に `rename` で置換しており、書込み途中状態が外部から観測できない設計であることを確認する(前例: `noa-app/src/session.rs:347-354`)。
- **AC-14 (R-17, NFR-6)** [integration]: Given `--font-size` CLIフラグ有効セッション相当の状態で別の値を設定rowで確定する。When 出力configを検査する。Then CLI由来の値ではなく、設定rowで確定した値が書かれている。

### 伝播・エッジケース
- **AC-24 (R-18)** [unit]: Given 複数ウィンドウ状態(モック)。When commitを実行する。Then 全ウィンドウに対して再描画要求が発行されている(呼び出し記録の検証)。
- **AC-15 (R-18)** [GUI目視]: 複数ウィンドウを開いた状態で1ウィンドウのオーバーレイから確定し、全ウィンドウのクロム/本文が新テーマ・新設定に切り替わることを最終確認する。
- **AC-16 (R-4)** [unit]: Given fuzzy検索で一致0件になる文字列を入力する。When 一覧を確認する。Then 一覧は空だが `preview_theme` は直前の値を維持している。

### 性能
- **AC-18 (NFR-2)** [unit]: Given debug再構築カウンタ(L2で規定した納品物)を持つ `ChromeTextures`。When テーマ一覧の連続ハイライト10件以上をシミュレートする。Then カウンタは0のまま、Enter確定後の次回描画で初めて一括再構築される(カウンタ増分が確定1回分のみ)。
- **AC-19 (NFR-1)**: (a) [unit] プレビュー解決+再描画要求の処理が1フレームバジェット(約16ms@60Hz)以内で完了する計時テスト。(b) [GUI目視] 体感遅延がないことの確認(補助)。

### トレーサビリティ

| Requirement | AC |
|---|---|
| R-1 | AC-21 |
| R-2 | AC-22 |
| R-3 | AC-17 |
| R-4 | AC-16 |
| R-5 | AC-3 |
| R-6 | AC-1, AC-2 |
| R-7 | AC-4 |
| R-8 | AC-5 |
| R-9 | AC-6 |
| R-10 | AC-5 |
| R-11 | AC-7 |
| R-12 | AC-10, AC-23 |
| R-13 | AC-9, AC-20 |
| R-14 | AC-11 |
| R-15 | AC-12 |
| R-16 | AC-8 |
| R-17 | AC-14 |
| R-18 | AC-15, AC-24 |
| NFR-1 | AC-19 |
| NFR-2 | AC-18 |
| NFR-3 | AC-13 |
| NFR-4 | AC-12 |
| NFR-5 | AC-11 |
| NFR-6 | AC-14 |

全 18 R + 6 NFR に各 ≥1 AC、計24 AC(AC-1〜24)。[GUI目視] を主検証とするのは AC-15 のみ、補助目視は AC-3/4b/7b/10/19b。

## 改訂履歴

- **2026-07-06 (実装時改訂、ユーザーサインオフ済)**: (1) R-8 — 確定のみ3行は「確定時適用」から「永続化のみ・次回起動反映+再起動ノート表示」へ改訂(ランタイム適用経路が未実装のため)。(2) font-family行のfuzzyサブフィルタを削除(単純サイクル)。(3) window-padding x/y独立編集を両軸同時ステップに簡略化。(2)(3)の元仕様は将来増分候補としてOpen Questionsに記載。

## Open Questions / Deferred Decisions

- **将来増分候補 (2026-07-06 実装時に簡略化)**: font-family行のfuzzy検索サブフィルタ / window-padding x/yの行内独立編集 / 確定のみ3行のランタイム適用(font-family=FontGrid再構築, padding=合成resize, titlebar=AppKit呼出)。

- **Ghostty忠実度ポジショニング**: 本機能はGhosttyに存在しないnoa独自拡張であり、`theme-selection.md` L0の忠実度原則(「GUIテーマエディタ等Ghosttyに無い機能は対象外」)の明示的な例外。UI表層はGhostty観測挙動の複製対象(Parity Map)に含めない。
- **AC-19(a) 計時テストのCI耐性**: 16ms@60Hzはローカル実行基準。CI環境でflakyな場合は閾値緩和またはskipを許容する(実装時判断)。
- **将来増分候補(out-of-scope確定分)**: configリロードキーバインド(cmd+shift+,相当)/ `theme = light:X,dark:Y` 自動切替 / ユーザー定義テーマ(`~/.config/noa/themes/`)/ configファイル監視。設定rowsの追加要望も別増分でレビュー(確定裁定5)。

## Build-path decision

**apex(一気通貫)** — サインオフ時選択(2026-07-06)。`/nexus apex` に本specを入力として渡し、設計→リスクゲート→実装ループ→L3 AC検証→出荷を同席の単一ランで実行する。L3のAC-1〜24(トレーサビリティ表付き)が検証契約。フォールバック: `/nexus feature`(監督付き単一ビルド)。
