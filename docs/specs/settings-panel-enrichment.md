# Spec: Settingsパネル リッチ化・最適化 (settings-panel-enrichment)

## Metadata

- **slug:** settings-panel-enrichment
- **title:** Settingsパネル リッチ化・最適化 (Settings Panel Enrichment & Optimization)
- **status:** **draft**(magi評決スコープB+確定 2026-07-11、本spec本体は未サインオフ)
- **owner:** simota
- **scope mode:** **Standard**(要件10件[must 8 + nice 2] + NFR 3件、中複雑度 — Accordテンプレート選定基準に合致)
- **upstream:** `theme-settings-ui.md`(locked — オーバーレイの起動導線・プレビュー機構・commitシーケンス・config書込みの土台。本specはその上に「情報設計の質」を追加する増分で、upstreamの決定を再審議しない)
- **traceability:** 10 R + 3 NFR = 13項目、各項目に1〜3 ACを付与、全項目カバー(100%、Standard最低ライン85%を超過)

## L0 — Vision

`noa`のSettingsオーバーレイ(`crates/noa-app/src/theme_settings/`)は`theme-settings-ui.md`により実装済みで、16設定行のライブ/次回起動プレビュー・574テーマのfuzzy検索・surgicalなconfig書込みまで機能する。しかし「リッチ化」とはビジュアル装飾ではなく **情報設計の質** — ユーザーが今どの行が即時反映されどれが次回起動待ちなのかを常に正しく知り(嘘表示ゼロ)、16行の中から目的の設定を素早く見つけ、その意味を確認し、誤って変えた値を安全に既定へ戻せること — と定義する。同時に、既存実装には「毎フレームのフルクローン」という具体的なパフォーマンス債務(`theme_settings/state.rs:68-79`のdoc commentが明示)が残っており、これを解消する。

magi評決(2026-07-11、再審議禁止)により採用スコープは **B+**: must-have 8件 + nice-to-have 2件(must全緑後のみ着手)。本specはそのmagi評決を、実装コードの直接監査によって具体化・裏取りしたものである。監査の結果、magi評決が「未実装」と想定していた項目のうち複数が**部分的に、あるいは異なる形で既に実装済み**であることが判明した(下記「コード監査の結果」参照)。本specの各要件は、この監査結果を反映して磨き直したものであり、magiのCritical#1/#2の意図を変更するものではない。

### コード監査の結果(magi評決との差分)

| magi項目 | 想定 | 監査結果 | specでの扱い |
|---|---|---|---|
| Critical#1(opaque restart note) | 未実装 | **部分実装済み**: `ThemeSettings::restart_note()`(`theme_settings/state.rs:306-332`)がopaque-at-startup時のopacity/blur行と、touched済みcommit-only行の両方を`bool`で検出している。ただし表示文言は両ケースとも同一の`"(restart to apply)"`(`app/sidebar/palette.rs:923-924`, `macos_overlay/imp/appkit.rs:1099-1100`)で、理由の区別がない | R-1: 検出ロジックは流用し、理由提示(reason)を追加する差分に絞る |
| Critical#2(メニュー項目追加) | 未実装 | **部分実装済み**: `AppCommand::OpenSettings`は既に存在し`open_theme_settings(Settings mode)`に配線済み(`app/commands.rs:146`)だが、`menu_id()`が空文字列(`commands/command.rs:217`)でネイティブメニューに未登録。一方、既存の"Settings..."(⌘,)メニュー項目は`AppCommand::Preferences`で外部エディタ起動(`open_config_file()`, `app/commands.rs:66`)に配線されており、これは`theme-settings-ui.md` R-1の意図的な決定("既存⌘,は変更せず並存")だった | R-2: 新規メニュー項目を追加して`OpenSettings`を配線する。既存⌘,には触れない(locked upstream決定の維持) |
| Must#3(全行バッジ) | 未実装 | **未実装(想定通り)**: `restart_note()`は`touched`済みでない限りcommit-only行に何も表示しない — 未編集時、commit-only行はlive行と見分けがつかない | R-3として新規実装 |
| Must#4(per-frameクローン) | 未実装 | **想定通り未解消**: `app/render.rs:48`で`session.state.clone()`が確認された。`state.rs:68-79`のdoc commentが「filteredフィールド(最大574件)込みの許容済み債務」と明記 | R-4として新規実装(`Arc`+`make_mut`方式) |
| Must#5(fuzzy検索) | 未実装 | **未実装(想定通り)**: Settingsモードの`push_text`は`FontSize`/`BackgroundImage`行の直接入力のみ処理し、行一覧のフィルタは存在しない | R-5として新規実装 |
| Must#6(説明文) | 未実装 | **未実装(想定通り)**: `SettingsRowKind`に説明文の関数がない | R-6として新規実装 |
| Must#7(Reset) | 未実装 | **未実装(想定通り)**: リセット操作は存在しない | R-7として新規実装 |
| Must#8(既存回帰ゼロ) | ハードゲート | 既存テスト`theme_settings/tests.rs`(981行) + `app/input_ops/theme_settings.rs`内テストの現状を確認済み | R-8としてハードゲート化 |
| Nice#9(C安全4キー) | 未実装 | `scrollback_limit`/`cursor_style_blink`/`minimum_contrast`/`macos_option_as_alt`は全て`StartupConfig`の実在フィールド(`noa-config/src/lib.rs:611-625`) | R-9として新規実装(must全緑後) |
| Nice#10(コントラストトークン化) | 想定: ハードコード色の移行 | **監査の結果、移行対象がほぼ存在しない**: AppKit側の選択行背景は既に`colors.selected_bg`(`OverlayColors`由来、`macos_overlay/imp/appkit.rs:941,1114`)を使用、wgpu側の選択行前景も`accent: Rgb`引数(呼び出し元でテーマ由来に解決済み)を使用。ハードコードRGBリテラルは発見されなかった | R-10として範囲を再定義: 既存トークン経路をリグレッション防止する検証テストの追加に絞る |

## Out-of-scope(スコープ境界、magi確定・再審議禁止)

- テーマのlight/darkペア対応(`theme = light:X,dark:Y`)
- Settingsオーバーレイ内のセクション見出し追加
- マウス操作対応(クリック選択・スクロールドラッグ等)
- VoiceOver/アクセシビリティツリー対応
- 透過方式の変更(`background_opacity`のwinit生成時固定制約、ALPHA_REPLACE経路 — マゼンタ縞RCA[`noa-nativetab-magenta`メモリ参照]の再燃を避けるため一切変更しない)
- 既存⌘,("Settings...")メニュー項目の配線変更
- `theme-settings-ui.md`のプレビュー機構・commitシーケンス・config書込みフォーマットの変更

### 失敗条件(magi確定、ハードゲート)

- F1: 既存16行のいずれかの挙動が退行する
- F2: 透過方式・マゼンタ縞系の不具合を再誘発する
- F3: 上記Out-of-scope項目のいずれかに着手する
- F4: 検索(R-5)・Reset(R-7)・説明文(R-6)のいずれかが部分実装のまま完了扱いになる
- F5: must-have(R-1〜R-8)が全緑になる前にnice-to-have(R-9, R-10)に着手する

## L1 — Requirements

### Must-have(magi Critical/必須、8件)

- **R-1(理由提示付きrestartノート)**: `ThemeSettings::restart_note(kind) -> bool`(`theme_settings/state.rs:306-332`)を、理由を区別する型(例: `RestartReason::None | RestartReason::OpaqueStartup | RestartReason::CommitOnly`)に置き換える。opaque起動時のopacity/blur行と、touched済みcommit-only行(font-family等)は異なる文言で表示する。透過方式そのものは変更しない(Out-of-scope)。
- **R-2(ネイティブメニュー項目)**: `AppCommand::OpenSettings`(既存、`app/commands.rs:146`で`open_theme_settings(Settings mode)`に配線済み)に非空の`menu_id()`を与え、`macos_menu.rs`のネイティブメニューに新規項目として追加する。既存の⌘,/"Settings..."(`AppCommand::Preferences`→`open_config_file()`)は一切変更しない。
- **R-3(全行常時バッジ)**: `SettingsRowKind::is_live()`(既存、静的分類)から導出する「Live」/「Next launch」バッジを、`touched`状態に関わらず全16(nice#9採用後20)行に常時表示する。R-1の理由付きノートとは独立した信号として両立させる(嘘表示ゼロ — 未編集のcommit-only行が一見live行と区別つかない現状を解消)。
- **R-4(per-frameクローン解消)**: `app/render.rs:48`の`session.state.clone()`(`ThemeSettings`のフルクローン、`filtered: Vec<ThemeMatch>`最大574件込み)を除去する。`ThemeSettingsSession.state`を`Arc<ThemeSettings>`化し、描画パスは`Arc::clone`(参照カウント増分のみ)、変更系メソッドは`Arc::make_mut`経由で呼び出す。
- **R-5(設定行fuzzy検索)**: `command_palette::fuzzy_match`を転用し、Settingsモードの16(20)行をラベルでfuzzy検索できるようにする。空クエリ=全行表示、非マッチ=0件表示。
- **R-6(選択行の説明文)**: `SettingsRowKind`の全kindに静的な1行説明文を追加し、現在選択中の行についてビュー上に表示する(AppKitカード・wgpuテキストカード両方)。
- **R-7(Reset to Default)**: 選択中の行を`noa_config::StartupConfig::default()`由来の既定値へ、行単位でリセットする操作を追加する。
- **R-8(既存回帰ゼロ、ハードゲート)**: 既存16行の値・キー操作・commit/revert・restart-note検出ロジックの外部観測可能な挙動を一切変えない。`theme_settings/tests.rs`(981行)と`app/input_ops/theme_settings.rs`内の既存テストは全て無改変のまま緑を維持する。

### Nice-to-have(must全緑後のみ、2件)

- **R-9(C安全4キー解放)**: `scrollback-limit` / `cursor-style-blink` / `minimum-contrast` / `macos-option-as-alt`(全て`StartupConfig`の実在フィールド)を新規行として追加する。各行はR-2〜R-6の「5点セット」(`RowDraft`変体・`is_live()`分類・`commit_updates()`マッピング・ラベル・説明文)を備える。`SettingsRowKind::COUNT`は16→20。
- **R-10(選択行コントラストの回帰防止)**: コード監査により、選択行の背景色(`OverlayColors::selected_bg`)・前景色(`accent: Rgb`)は両レンダーパス(AppKit/wgpu)で既に既存UIトークン経由であることを確認した。移行対象コードが存在しないため、本要件は「既存トークン経路が将来ハードコード値に退行しないことを保証するコントラスト比検証テストの追加」に再定義する。

### 非機能要件(NFR)

- **NFR-1(アロケーション)**: 入力なし(選択・編集が発生しない)フレーム間の連続redrawで、`ThemeSettings`の深いクローンが発生しないこと(R-4に直結)。
- **NFR-2(60fps非劣化)**: 設定行fuzzy検索(R-5、最大20行)は既存の574テーマfuzzy検索と同一の`fuzzy_match`を使い、テキスト入力イベント発生時のみ再計算する(既存の`refilter_and_mark`と同じトリガ規律)。アイドル時の毎フレーム再計算を発生させない。
- **NFR-3(config writer非拡張)**: R-1/R-3/R-6/R-7/R-9のいずれも、`noa_config::write_config_updates`(既存、`theme-settings-ui.md` R-14で導入済み)以外の新規config書込み経路を作らない。

## L2 — Detail

### R-1: 理由提示付きrestartノート

- 対象ファイル: `crates/noa-app/src/theme_settings/state.rs`(`restart_note`メソッド)、`crates/noa-app/src/macos_overlay/model.rs`(`ThemeSettingsViewModel::rows`タプル)、`crates/noa-app/src/app/sidebar/palette.rs:923-924`、`crates/noa-app/src/macos_overlay/imp/appkit.rs:1099-1100`。
- `restart_note(kind: SettingsRowKind) -> bool`を`restart_reason(kind: SettingsRowKind) -> RestartReason`に置き換える。既存の判定条件(opaque_at_startup && Opacity/BlurRadius、または非live行のtouched)はロジックとしてそのまま流用し、戻り値の型だけを`bool`から3値enumへ広げる。
- `ThemeSettingsViewModel::rows`タプルの3要素目(現状`bool`)を`RestartReason`(または表示済み文言`Option<&'static str>`)に変更する。両レンダーパスの文言分岐先(`app/sidebar/palette.rs:923-924`、`appkit.rs:1099-1100`)を、`RestartReason::OpaqueStartup`用と`RestartReason::CommitOnly`用の2文言に分岐させる。
- 文言例(実装時に確定): `CommitOnly` → `"(restart to apply)"`(既存文言を維持)、`OpaqueStartup` → `"(opaque at launch — restart to preview)"`のような、なぜ次回起動待ちなのかを言い分ける文言。
- 透過方式・`opaque_at_startup`判定・`background_opacity >= 1.0`の閾値そのものは一切変更しない。

### R-2: ネイティブメニュー項目

- 対象ファイル: `crates/noa-app/src/commands/command.rs`(`menu_id()`関数、`OPEN_SETTINGS_MENU_ID`定数追加、`from_menu_id`への追加)、`crates/noa-app/src/macos_menu.rs`(`MacosMenu::install`)。
- `AppCommand::OpenSettings`に`OPEN_SETTINGS_MENU_ID`定数(他の`*_MENU_ID`定数と同じ命名規則)を割り当て、`menu_id()`の`AppCommand::OpenSettings => ""`を実IDに差し替える。`from_menu_id`の逆引きにも追加する。
- 新規`MenuItem`を構築し、`view_menu`(`macos_menu.rs`の"Command Palette"/"Session Overview"項目が並ぶブロック)に追加する。ラベルは既存のコマンドパレット表記("Open Settings…")と揃える。既存"Settings..."(⌘,)との混同を避けるため、アクセラレータは割り当てない(コマンドパレットからの既存導線と同じく無キーバインド)。
- `AppCommand::Preferences`のmenu_id・ラベル・アクセラレータ(⌘,)・`open_config_file()`への配線は一切変更しない — `theme-settings-ui.md` R-1の"既存⌘,は変更せず並存"という決定の維持。
- テストパターンは既存の`preferences_menu_item_spec`/`fullscreen_menu_item_spec`(`macos_menu.rs`末尾の`#[cfg(test)] mod tests`)を踏襲する — ウィンドウ/GPU不要のプレーンな関数として`open_settings_menu_item_spec()`相当を切り出し、単体テスト可能にする。

### R-3: 全行常時バッジ

- 対象ファイル: `theme_settings/state.rs`、`macos_overlay/model.rs`(`ThemeSettingsViewModel`)。
- `SettingsRowKind::is_live()`(既存、静的)をそのまま参照元として使う — 新しい分類ロジックは作らない。
- `ThemeSettingsViewModel::rows`タプルに、R-1の`RestartReason`とは別に、`live: bool`(= `kind.is_live()`)フィールドを追加する。両者は独立して描画する: `live`は行を選択していなくても常時見えるバッジ(例: 行末に小さく"Live"/"Restart"のラベル)、`RestartReason`はR-1の「(restart to apply)」系の補足文言(touched後にのみ意味を持つ)。
- 「嘘表示ゼロ」の要件は、`live`バッジが`touched`の値に一切依存しないことで満たす — オーバーレイを開いた直後、まだ何も編集していない状態でも全20行の分類が見える。

### R-4: per-frameクローン解消

- 対象ファイル: `crates/noa-app/src/app/state.rs`(`ThemeSettingsSession`定義、`app/state.rs:537`)、`crates/noa-app/src/app/render.rs:44-48`、`crates/noa-app/src/app/input_ops/theme_settings.rs`(全ての`session.state.xxx_mut`相当の変更系呼び出し)、`crates/noa-app/src/app/sidebar/palette.rs`(`draw_theme_settings_card`/`theme_settings_overlay_text`の`state: &ThemeSettings`引数)、`crates/noa-app/src/macos_overlay/mod.rs`(`sync_theme_settings`の`&ThemeSettings`引数)。
- `ThemeSettingsSession.state: ThemeSettings`を`state: Arc<ThemeSettings>`に変更する。
- `render.rs:48`の`session.state.clone()`(深いクローン)を`Arc::clone(&session.state)`(参照カウント増分のみ)に置き換える。`sidebar::draw_theme_settings_card`・`macos_overlay::sync_theme_settings`は既に`&ThemeSettings`を受け取るシグネチャなので、`Arc<ThemeSettings>`から`&ThemeSettings`への参照解決(Derefまたは明示的`&*`)以外の変更は不要。
- 変更系メソッド(`move_up`/`move_down`/`adjust`/`push_text`/`backspace`/`commit`/`revert`等、`input_ops/theme_settings.rs`から呼ばれる箇所)の呼び出し元を`Arc::make_mut(&mut session.state)`経由に変える。single-threadなwinitイベントループ上では、`redraw()`が取得した`Arc`クローンは同フレーム内で破棄され、次のキー入力までに参照カウントは1に戻るため、`make_mut`の実クローン分岐はほぼ発火しない(redraw自身は`session.state`を変更しないため、レンダリングとミューテーションが同一Arcを同時に握ることが構造的にない)。
- なぜ`Arc<ThemeSettings>`全体の共有か(view-modelへの分離ではなく): wgpu側の`theme_picker_overlay_text`/`settings_rows_overlay_text`(`app/sidebar/palette.rs`)は、ペイン実サイズに応じた可変windowing(`THEME_SETTINGS_COLS`/`ROWS`をペインの実colsRowsでクランプ)を行うため、AppKit側の固定8行windowing用`ThemeSettingsViewModel`(`macos_overlay/model.rs`)をそのまま両パスの共通スナップショット型として転用できない(windowingの粒度が異なる)。したがって"軽量view-model型への分離"ではなく"Arc共有+make_mut"を採用する(タスク提示の2候補のうち後者)。

### R-5: 設定行fuzzy検索

- 対象ファイル: `theme_settings/state.rs`(`ThemeSettings`構造体・`push_text`/`backspace`)、`app/input_ops/theme_settings.rs`(Tabキーの扱い)。
- `Section::SettingsRows`専用の新規フィールド`settings_search_active: bool`と`settings_filter: String`、`settings_filtered: Vec<usize>`(`SettingsRowKind::ALL`へのインデックス、`ThemeMatch`と同型のスコア付けで並べる)を追加する。
- **検索の起動/終了ジェスチャー**: 現在`Section::SettingsRows`では`Tab`キーが`toggle_section()`(DEC-2により空実装の死んだコード経路、`state.rs:240-244`のdoc comment参照)を呼ぶだけで何もしない。この死んだフックを再利用し、`ThemeSettingsMode::Settings`セッション限定で`Tab`を「検索モードのトグル」に転用する(Themeモードの`Tab`は変更しない — Themeモードには元々セクション切替の意味しかなく、本specの対象外)。
- **検索有効時の入力ルーティング**: `push_text`/`backspace`は、`settings_search_active == true`の間、現在選択中の行の種類(`FontSize`の桁入力、`BackgroundImage`のパス入力)に関わらず`settings_filter`への追記/削除を優先する。検索終了(再度Tab、またはEnter)で通常の行編集入力ルーティングに戻る。
- 空クエリは`SettingsRowKind::ALL`を元の表示順で全件表示(`fuzzy_match`の空クエリ挙動、`command_palette_matches`/`filter_themes`と同じ扱いを踏襲)。非マッチは0件表示とし、`ThemePicker`の`filtered.is_empty()`ガード(`move_up`/`move_down`, `state.rs:347-379`)と同じパターンで、空リスト時の`move_up`/`move_down`をno-opにする。
- 検索を抜けた時点の選択行は、その時点でハイライトされていたフィルタ結果の実行を選択する(コマンドパレットの確定操作と同じ考え方)。

### R-6: 選択行の説明文

- 対象ファイル: `theme_settings/rows.rs`(`SettingsRowKind`)、`macos_overlay/model.rs`(`ThemeSettingsViewModel`)、`app/sidebar/palette.rs`(wgpuテキストカード)、`macos_overlay/imp/appkit.rs`(AppKitカード)。
- `SettingsRowKind::description(self) -> &'static str`を`label()`(`rows.rs:98-118`)と同じ形の静的match関数として追加する。全20 kind(R-9採用後)に1行の英語説明文を与える。
- `ThemeSettingsViewModel`に`selected_description: &'static str`フィールドを追加し、`theme_settings_view_model()`内で`SettingsRowKind::ALL[state.selected_row()].description()`から導出する。
- 両レンダーパスで、選択行の直下(または footer 領域の上)に1行追加する。カードの縦幅が最小化(既存メモリ「theme-settings-ui spec」の"カード縦幅縮退min3"制約)に触れる場合は、既存の`overlay_scroll_window`の表示行数と共存できるよう、説明文表示を優先してテーマリストや行リストの表示件数を1行分減らす(既存の`THEME_SETTINGS_ROWS`/`THEME_LIST_ROWS`定数調整で吸収する)。

### R-7: Reset to Default

- 対象ファイル: `theme_settings/state.rs`(`ThemeSettings::reset_selected_row`新設)、`theme_settings/rows.rs`(`RowDraft::default_for(kind) -> RowDraft`新設)、`app/input_ops/theme_settings.rs`(Deleteキーのハンドリング)。
- 既定値ソースは`noa_config::StartupConfig::default()`(`noa-config/src/lib.rs:590-649`、実装確認済み)。`ThemeSettingsInit`が`StartupConfig`の各フィールドから初期値を組み立てているのと対称的に、`RowDraft::default_for(kind: SettingsRowKind) -> RowDraft`は`StartupConfig::default()`の対応フィールドから`RowDraft`を組み立てる純粋関数として`rows.rs`に追加する(`ThemeSettingsInit`組み立てロジックと二重管理にならないよう、`open_theme_settings`側の初期値マッピングと`default_for`は共通の変換関数を参照する設計にする — 実装時に`app/input_ops/theme_settings.rs:open_theme_settings`のフィールドマッピングと突き合わせて整合させる)。
- `ThemeSettings::reset_selected_row(&mut self, now: Instant) -> RowEffect`: 選択中の行の`draft`を`RowDraft::default_for(kind)`に置き換え、`touched = true`にする(既定値がsnapshotと異なる場合のみcommit時に書き込まれる、既存の`commit_updates()`のtouchedゲートをそのまま利用)。live行(`FontSize`/`BackgroundOpacity`/`BackgroundBlurRadius`/`CursorStyle`/`SidebarPreviewLines`)は`adjust()`と同じ`RowEffect`を返し、`app/input_ops/theme_settings.rs`の`adjust_theme_settings_row`と同じ適用パスに合流させる。
- **キーバインディング**: `Backspace`は既に`FontSize`桁入力/`BackgroundImage`パス入力の削除に使われているため衝突を避け、`NamedKey::Delete`(フォワードデリート、`Backspace`とは別の物理キー、`winit::keyboard::NamedKey`に実在)を新規に割り当てる(`handle_theme_settings_key`の先頭match、`Escape`/`Enter`/`Tab`と同列に追加)。誤操作時はEsc(既存のR-16セッション全体revert)で復帰できるため、追加の確認ダイアログは設けない(可逆な操作、Ambiguous+reversibleの原則で確認なしをデフォルトとする)。
- FontSizeのデバウンス中(R-9既存のデバウンサ)にResetが押された場合は、`set_font_size`と同じデバウンス提出経路を通す(直接値を書くのではなくデバウンサへ提出し、既存のR-9(旧spec)のfont-size取り扱いと一貫させる)。

### R-8: 既存回帰ゼロ(ハードゲート)

- 新規コードの追加は既存の公開メソッドのシグネチャ・戻り値の意味を変えない形で行う(R-1のみ`restart_note`の戻り値型を`bool`→enumに変更するため、この1メソッドの型変更は許容された変更として明記し、呼び出し元3箇所[`state.rs`内部, `model.rs`, 両レンダーパス]を全て追従させる)。
- `theme_settings/tests.rs`(981行)と`app/input_ops/theme_settings.rs`内の`#[cfg(test)] mod commit_theme_settings_tests`の既存テスト関数は、アサーション本文を一切変更せずに緑を維持する。新規テストは追加のみ。
- `cargo test -p noa-app`をゲートとする。

### R-9: C安全4キー解放(must全緑後)

- 対象4キー: `scrollback-limit`(`StartupConfig::scrollback_limit`, `DEFAULT_SCROLLBACK_LIMIT`)、`cursor-style-blink`(`cursor_style_blink: Option<bool>`)、`minimum-contrast`(`minimum_contrast`, `DEFAULT_MINIMUM_CONTRAST`)、`macos-option-as-alt`(`macos_option_as_alt: MacosOptionAsAlt`)。全て`noa-config/src/lib.rs:590-649`の`StartupConfig::default()`に実在するフィールドであることを確認済み。
- 各行は`RowDraft`新variant・`SettingsRowKind`新variant・`label()`/`description()`(R-6)エントリ・`is_live() == false`(「C安全」= ランタイム適用経路が存在しないため、既存の`FontFamily`/`WindowPadding`/`MacosTitlebarStyle`と同じ"persist-only、次回起動反映"パターンを踏襲、`commit_theme_settings`のコメントが明記する既存の設計判断と一貫)・`commit_updates()`での対応config keyへのマッピングを備える。
- `SettingsRowKind::COUNT`が16→20になることに伴い、`ALL`配列・`rows`配列・全ての`SettingsRowKind::ALL[idx]`前提コードを機械的に追従させる(既存の16行分の実装パターンの反復であり、新規の設計判断は伴わない)。
- ランタイム適用(live化)は本specのスコープ外 — 将来増分候補として明示する(下記Open Questions)。

### R-10: 選択行コントラストの回帰防止

- 対象ファイル: `macos_overlay/model.rs`(`OverlayColors`)。
- WCAG相対輝度式(外部crate不要、`[f32;4]`のRGBA成分に対する純粋な算術)を使い、既存テスト内に`OverlayColors::selected_bg` vs `surface_fg`、および`accent` vs `surface_bg`のコントラスト比が最低ライン(例: 3.0:1、UI装飾要素向けのWCAG AA Large Text相当)を満たすことを検証するunit testを追加する。
- 検証対象のテーマ/カラーフィクスチャは、既存テストが既に使っているもの(例: `theme_settings/tests.rs`の`"3024 Day"`)を再利用し、light/darkペアの網羅的なマトリクス検証は行わない(Out-of-scope)。
- 本要件は新規のトークン化作業ではなく、既存のトークン経由経路(`colors.selected_bg`, `accent: Rgb`)が将来ハードコード値へ退行しないことを保証するリグレッションガードとして位置づける。

## エッジケース・未検証事項(明示)

- **検索とselected_row/font_size_digits/background_image_textの相互作用**: R-5で検索モードに入った際、`FontSize`行が編集途中(`font_size_digits: Some(..)`)だった場合の扱い(検索開始時に未確定の桁入力を確定させるか破棄するか)は未確定。実装時に`clear_row_input_state()`(既存、`state.rs:439-442`)の呼び出しタイミングと合わせて決定する。
- **検索終了時のindex安定性**: `settings_filtered`が絞られた状態から全件表示に戻したとき、`selected_row`(`SettingsRowKind::ALL`への生インデックス)が検索前の行を指し続けるか、検索結果内での相対位置を保つかは未確定。コマンドパレットの確定挙動との整合を実装時に確認する。
- **R-9の4行のランタイム適用**: 本specでは全てpersist-only(次回起動反映)とするが、`cursor-style-blink`は既存のlive`CursorStyle`行と密接に関連するため、将来`apply_live_cursor_style`の`blinking`引数(現状`initial_cursor_style`から静的導出、`app/input_ops/theme_settings.rs:518-528`)と統合してlive化できる可能性がある — 本spec範囲外の将来増分候補として記録する。
- **R-1文言の最終テキスト**: 実装時のコピーレビュー対象とし、本specでは意味(理由の区別)のみを契約する。

## L3 — Acceptance Criteria

検証手段の凡例: [unit]=GPU不要のユニットテスト / [integration]=結合テスト / [code-review]=実装検査 / [GUI目視]=手動確認。

### R-1(理由提示付きrestartノート)
- **AC-1** [unit]: Given opaque起動(`opaque_at_startup=true`)で`BackgroundOpacity`行が未編集。When `restart_reason(BackgroundOpacity)`を呼ぶ。Then `RestartReason::OpaqueStartup`を返す。
- **AC-2** [unit]: Given `FontFamily`行(commit-only)がtouched済み。When `restart_reason(FontFamily)`を呼ぶ。Then `RestartReason::CommitOnly`を返し、AC-1のケースとは異なるvariantである。
- **AC-3** [GUI目視]: opaque起動セッションのopacity/blur行が、他のcommit-only行の"(restart to apply)"とは異なる文言を表示する。

### R-2(ネイティブメニュー項目)
- **AC-4** [unit]: `AppCommand::OpenSettings.menu_id()`が非空文字列を返し、`AppCommand::from_menu_id`がそれを`OpenSettings`へ逆変換する(既存`preferences_menu_item_spec`テストと同型のウィンドウ不要テスト)。
- **AC-5** [code-review]: `AppCommand::Preferences`のmenu_id・ラベル・アクセラレータ・`open_config_file()`への配線に差分がないことを diff で確認する。
- **AC-6** [GUI目視]: 新規メニュー項目がSettingsオーバーレイ(Settingsモード)を開き、外部エディタは起動しない。

### R-3(全行常時バッジ)
- **AC-7** [unit]: Given オーバーレイを開いた直後(全行`touched=false`)。When view modelを構築する。Then 全20行の`live`フィールドが`SettingsRowKind::is_live()`の値と1件ずつ一致する。
- **AC-8** [unit]: Given live行を編集する。When view modelを再構築する。Then その行の`live`フィールドは`true`のまま変化しない(R-1の`RestartReason`とは独立)。

### R-4(per-frameクローン解消)
- **AC-9** [unit]: Given セッションが開いていて、2回連続で`redraw`相当の読み取り専用スナップショット取得(`Arc::clone`)を行い、その間に変更系メソッドを呼ばない。When 2つの`Arc`ポインタを`Arc::ptr_eq`で比較する。Then 一致する(深いクローンが発生していない)。
- **AC-10** [unit]: Given 直前のスナップショット`Arc`がスコープを抜けて破棄された後。When `move_down`等の変更系メソッドを呼ぶ。Then 既存の(R-8で凍結された)全ての振る舞いテストが変更なしで緑のまま通る(`Arc::make_mut`経由でも意味論が変わらないことの回帰証明)。
- **AC-11** [code-review]: `app/render.rs`の`theme_settings_card`構築コードに`.clone()`(`ThemeSettings`本体に対する)が存在しないことを確認する。

### R-5(設定行fuzzy検索)
- **AC-12** [unit]: Given Settingsモードで`Tab`を押す。When 状態を検査する。Then `settings_search_active`が`true`になる。
- **AC-13** [unit]: Given 検索有効状態で"curs"を入力する。When `settings_filtered`を検査する。Then ラベルに"curs"がfuzzyマッチする行のみが、スコア降順で並ぶ。
- **AC-14** [unit]: Given 検索クエリが0件マッチになる。When 一覧を確認する。Then 一覧は空だが、`move_up`/`move_down`はpanicしない(no-op)。
- **AC-15** [unit]: Given 空クエリ。When `settings_filtered`を検査する。Then `SettingsRowKind::ALL`と同じ順序で全20件を含む。

### R-6(選択行の説明文)
- **AC-16** [unit]: `SettingsRowKind::ALL`の全kindについて、`description()`が空文字列でなく、`label()`とも異なる文字列を返す。
- **AC-17** [unit]: `theme_settings_view_model()`が返す`selected_description`が、常に`SettingsRowKind::ALL[state.selected_row()].description()`と一致する。

### R-7(Reset to Default)
- **AC-18** [unit]: Given `FontSize`行(live)を既定値から変更済み。When `Delete`キー相当の操作(`reset_selected_row`)を実行する。Then 行の`draft`が`StartupConfig::default().font_size`相当の値に戻り、`touched=true`になり、対応する`RowEffect`がライブ適用のために返る。
- **AC-19** [unit]: Given `FontFamily`行(commit-only)が未編集。When Resetを実行する。Then `draft`が既定値になり`touched=true`になる(値が既存のsnapshotと同じ場合でも`touched`は立つ — 明示的なリセット操作の意図を保持するため)。

### R-8(既存回帰ゼロ、ハードゲート)
- **AC-20** [unit/integration]: `cargo test -p noa-app`実行時、`theme_settings::tests`および`app::input_ops::theme_settings::commit_theme_settings_tests`配下の既存テスト関数(981行分)が、アサーション本文の変更なしに全て通過する。

### R-9(C安全4キー解放)
- **AC-21** [unit]: 新規4行それぞれについて、`RowDraft`variant・`is_live() == false`・`commit_updates()`が対応するconfig key(`scrollback-limit`/`cursor-style-blink`/`minimum-contrast`/`macos-option-as-alt`)へマッピングされることを確認する。
- **AC-22** [unit]: `SettingsRowKind::COUNT == 20`であり、既存の`SettingsRowKind::ALL[i]`不変条件(`rows[i]`のdraft variantと一致)が20行全てで成り立つ。

### R-10(選択行コントラストの回帰防止)
- **AC-23** [unit]: 既存テストフィクスチャのテーマ(例: "3024 Day")から導出した`OverlayColors`について、`selected_bg` vs `surface_fg`、`accent` vs `surface_bg`のWCAG相対輝度コントラスト比が、いずれも定めた最低ライン以上であることを検証する。

### NFR
- **AC-24(NFR-1)** [unit]: AC-9と同一(アロケーション非発生の直接証明)。
- **AC-25(NFR-2)** [code-review]: 設定行fuzzy検索の再計算が、テキスト入力イベント発生時にのみトリガされ(既存`refilter_and_mark`と同じ規律)、アイドル時の毎フレーム再計算経路を持たないことをコードレビューで確認する。
- **AC-26(NFR-3)** [code-review]: R-1/R-3/R-6/R-7/R-9のいずれのdiffにも、`noa_config::write_config_updates`以外の新規config書込み関数が追加されていないことを確認する。

### トレーサビリティ

| Requirement | AC |
|---|---|
| R-1 | AC-1, AC-2, AC-3 |
| R-2 | AC-4, AC-5, AC-6 |
| R-3 | AC-7, AC-8 |
| R-4 | AC-9, AC-10, AC-11 |
| R-5 | AC-12, AC-13, AC-14, AC-15 |
| R-6 | AC-16, AC-17 |
| R-7 | AC-18, AC-19 |
| R-8 | AC-20 |
| R-9 | AC-21, AC-22 |
| R-10 | AC-23 |
| NFR-1 | AC-24 |
| NFR-2 | AC-25 |
| NFR-3 | AC-26 |

全 10 R + 3 NFR = 13項目、各項目に最低1 ACが対応し、計26 AC(AC-1〜26)。トレーサビリティ完全性 **100%**(Standardスコープ最低ライン85%を超過)。[GUI目視]はAC-3/AC-6のみ、他24件はunit/integration/code-reviewで自動的または実装検査で検証可能。

## L4 — Reversibility / Learning / Disqualification

```yaml
L4:
  reversibility:
    classification: HIGH
    # 全10要件は単一クレート(noa-app)内の追加的変更で、config形式・DB・公開APIの変更を伴わない。
    # R-4(Arc化)のみ内部型シグネチャ変更を伴うが、`ThemeSettingsSession`はnoa-app内部プライベート型で外部境界を跨がない。
    revert_procedure: "該当コミット群をrevertするか、featureブランチを一括破棄する。config書式・既存の16行の意味論には触れないため、ユーザーのconfigファイルへの影響はゼロ。"
    revert_time_estimate: "minutes(git revert 1コマンド相当)"
    revert_blast_radius: "Settingsオーバーレイのみ。ターミナル本体機能・他オーバーレイ(コマンドパレット・overview等)には波及しない(R-3非機能要件の相互排他ガードは既存のまま)。"

  learning:
    hypothesis: "Settingsオーバーレイの各行にlive/next-launch分類バッジと説明文を常時表示し、fuzzy検索とReset操作を追加することで、誤操作からの回復と目的の設定への到達が容易になる。"
    success_threshold:
      metric: "既存テストスイート(theme_settings::tests, 981行)の無改変通過率"
      value: 100
      window: "実装完了時点(CI一回)"
    fail_threshold:
      metric: "既存テストの改変または失敗件数"
      value: 1
      window: "実装完了時点(CI一回)"
    learning_capture_plan:
      win_capture: "実装完了後、cargo test -p noa-app の結果とGUI目視スポットチェック(AC-3, AC-6)をコミットメッセージ/PR説明に記録する。"
      loss_capture: "F1〜F5いずれかの失敗条件に抵触した場合、該当要件を該当PRから切り離し、原因をこのspecのOpen Questionsへ追記する。"
      decision_horizon: "実装ループ完了時(本specはapex/featureいずれかのbuild-pathで単一ランを想定)"

  disqualification:
    conditions:
      - id: DISQ-001
        description: "既存16行の値・キー操作・commit/revert挙動のいずれかが変わる(F1)"
        check: "AC-20(cargo test -p noa-app 既存テスト無改変通過)"
        on_trigger: REJECT
      - id: DISQ-002
        description: "透過方式(ALPHA_REPLACE経路)またはopaque判定ロジックに変更が入る(F2)"
        check: "code-review — R-1のdiffが`restart_reason`の戻り値型のみに限定されていることを確認"
        on_trigger: REJECT
      - id: DISQ-003
        description: "Out-of-scope項目(light/darkペア・セクション見出し・マウス操作・VoiceOver・既存⌘,配線)のいずれかに着手する(F3)"
        check: "code-review — diffの対象ファイル一覧を本specのL2記載ファイルと突き合わせる"
        on_trigger: REJECT
      - id: DISQ-004
        description: "R-5/R-6/R-7が部分実装のまま完了扱いになる(F4)"
        check: "AC-12〜15(R-5), AC-16〜17(R-6), AC-18〜19(R-7) 全緑"
        on_trigger: REJECT
      - id: DISQ-005
        description: "R-1〜R-8のいずれかが未緑のままR-9/R-10に着手する(F5)"
        check: "実装順序のcode-review — コミット履歴でR-9/R-10着手コミットがR-1〜R-8全緑コミットより後であることを確認"
        on_trigger: REJECT
```

## Meta

- **status:** draft(spec本体はサインオフ待ち。magi評決スコープは確定・再審議禁止)
- **version:** 1.0
- **authored by:** Accord agent(コード監査込み、2026-07-11)。L3は磨き直しのみで正式なThree Amigosレビュー(product/dev/QA)は未実施 — 実装着手前に人間レビューを推奨する。
- **reviews:** 未実施
- **upstream lock:** `theme-settings-ui.md`(locked) — 本specはその上に増分し、upstreamのR/AC/L2決定を変更しない。
- **next:** atlas + vision による並行設計(magi評決の"Next"指示どおり)。両agentは本specのL2(特にR-4のArc設計、R-5の検索状態機械)を実装設計の起点として参照すること。

## Open Questions / Deferred Decisions

- R-9の4行のうちどれか(特に`cursor-style-blink`)を将来live化する場合の設計(既存`apply_live_cursor_style`の`blinking`引数との統合方法)。
- R-5の検索終了時のindex安定性(検索前の行 vs フィルタ結果内の相対位置)。
- R-1の最終表示文言(意味の契約のみ本specで固定、コピーは実装時レビュー)。
- R-6の説明文が既存の"カード縦幅縮退min3"制約(`theme-settings-ui spec`メモリ参照)とどう共存するか、`THEME_SETTINGS_ROWS`/`THEME_LIST_ROWS`の調整幅。

---

## Addendum A — Tech design (Atlas ADR-0001, binding)

- **R-4 (per-frame clone)**: `Arc<ThemeSettings>` + `Arc::make_mut` CONFIRMED — strictly better than status quo.
  - `render.rs:48` `session.state.clone()` → `Arc::clone`. wgpu path unchanged (deref coercion); macOS sync path: `(ts.as_ref(), r)` one-token change.
  - 9 mutation sites in `app/input_ops/theme_settings.rs` (move_up/down, backspace, push_text, adjust, toggle_section, revert, commit, +poll path) go through `Arc::make_mut`.
  - CoW fires effectively never: render's Arc clone is frame-local (dropped at frame end); mutations see refcount==1.
  - **New invariant (code-review gate)**: the render path must NEVER store its Arc clone back into `self` across event-loop turns — that would silently re-enable deep copies via make_mut forks. AC-9 (ptr_eq) covers only the happy path.
  - Rejected: per-field Arc (whack-a-mole, spine still cloned), thin snapshot type (wgpu windowing needs full state — `render.rs:439`).
- **R-5 (search)**: modal sub-state — `settings_search_active: bool` + `settings_filter: String` + `settings_highlight` (symmetric with ThemePicker's filtered/highlighted). In-progress FontSize digits / BackgroundImage text buffers are DISCARDED on search enter+exit via `clear_row_input_state()` (safe: drafts already committed per keystroke). Exit leaves selection on the highlighted row. Empty query = all rows in ALL order; 0 matches = ↑↓ no-op (same guard pattern as `state.rs:367`). Tab toggles search only when `section == SettingsRows`.
- **R-9 (4 new keys)**: use a **6-point set** per key: ALL entry / label / is_live / RowDraft variant / RowEffect+apply path / restart_reason classification. New 4 keys have no runtime-apply → `RestartReason::CommitOnly`; cursor-style-blink is persist-only. COUNT is type-enforced (`[SettingsRow; COUNT]` + open()'s array literal); zero existing tests assume 16.

## Addendum B — UX design (Vision, binding)

- **Search row**: below the section header (`settings_top` slot), hidden entirely when inactive; active row = mono 12pt muted `/{query}` (byte-identical convention to Theme filter). `needed()` unconditionally adds `description_h(19)`, plus `16` when search active.
- **Badges**: label column slack — label w 220→170; badge x=196 w=44 right-aligned 9.5pt semibold. `LIVE` (accent) for `is_live()==true` rows; `ON LAUNCH` (muted) for the rest. Never depends on `touched`. Do NOT use "●" (semantic collision with section-focus glyph).
- **RestartReason display**: `None` → nothing; `CommitOnly` → `(restart to apply)` (existing string); `OpaqueStartup` → `(opaque window — restart to preview)`.
- **Descriptions**: fixed one-line slot directly above the footer (12pt regular muted); never positioned under the selected row. 16 static strings per Vision's table (SettingsRowKind::description()).
- **Keys**: `Tab` toggles search (SettingsRows only; Theme mode keeps no-op). `Enter` in search = confirm highlighted row + exit search; `Tab` again = exit restoring pre-search selection; `Esc` unchanged (whole-overlay cancel, never search-only). Reset = bare `Delete` (NamedKey::Delete; rejected: `r` collides with text entry, Backspace-hold needs a new primitive, cmd+Delete breaks bare-key vocabulary).
- **Footer hint**: `↑↓ navigate   ←→ adjust   Tab search   Delete reset   Esc cancel   Enter save`.
- **Empty state**: `No settings match "{query}"` centered in the list area (muted 12.5pt).
- **wgpu fallback row format**: `{badge:<10}{label:<22}{value}{reason}` — badge words identical to AppKit. Search/description/footer lines mirror AppKit content.
- Implementation-time recommendation (non-mandatory): brief highlight feedback on Reset (reuse tint_layer pattern).

## Addendum C — Risk-Gate conditions (binding, incorporated before implementation)

From Ripple (Conditional-Go conditions):
- **C-1**: R-4's file list additionally includes `app/timers.rs:490-501` (`tick_theme_settings_debounce` calls `poll_font_size` — the 9th mutation site; 8 are in `app/input_ops/theme_settings.rs`). `macos_overlay/sync.rs` needs no change (deref).
- **C-2**: R-1/R-8 amendment — `restart_note(&self, row) -> bool` is KEPT as a thin compatibility wrapper (`self.restart_reason(row) != RestartReason::None`); the 28 existing test call sites in `tests.rs` stay untouched. Only new code (model.rs, palette.rs, appkit.rs) calls the new `restart_reason(&self, row) -> RestartReason`.

From Echo (adopted design refinements):
- **C-3** (MAJOR-1): while search is active, the footer hint switches to a search-specific string (e.g. `Enter confirm row   Tab exit search   Esc cancel`) — Enter's row-confirm (vs save) meaning must be visible in the moment.
- **C-4** (MAJOR-2): Reset accepts BOTH `NamedKey::Delete` (forward delete) and `Cmd+Backspace` (laptop-reachable alias; bare Backspace stays text-delete). Footer text stays `Delete reset`.
- **C-5** (MAJOR-3): the brief Reset highlight feedback (tint_layer pattern) is MANDATORY, not optional — it is the only misfire detection cue.
- **C-6** (MODERATE-4): the badge derives from *effective* liveness: a live-class row downgraded by `RestartReason::OpaqueStartup` shows `ON LAUNCH` (muted) for that session, not `LIVE`. Zero-lie display is per-session truth.
- **C-7** (MODERATE-5, accepted deviation): Settings search row stays hidden when inactive (differs from Theme's always-visible filter row) — accepted for vertical-budget reasons; revisit only if user feedback contradicts.
- **C-8** (MINOR-6): visual-review checklist item: with a selected row, at most badge + reason + description + footer are simultaneously relevant — reviewer confirms this reads as layered, not cluttered.

## Addendum D — Gate aggregate: FM-01 spec correction + orbit contract clauses (binding)

**Risk Gate verdict: Conditional-Go (omen PASS-w/-conditions, ripple Conditional-Go, echo PASS). Conditions below are part of the implementation contract.**

### D-1. FM-01 spec correction (supersedes R-9's classification, RPN 567)
Code-verified: `app/config_reload.rs` (ConfigWatcher, 500ms poll) live-applies `scrollback_limit` (`apply_reloaded_terminal_policies`, :382), `cursor_style_blink` (:174-176), and `minimum_contrast` (`theme_inputs_changed`, :446) after any config-file write — including the Settings panel's own Enter commit. Only `macos-option-as-alt` is genuinely persist-only (read at pty spawn).
- Badge classes become THREE: `LIVE` (accent; applies as you adjust), `ON SAVE` (muted; applies within ~a moment of saving — the 3 reload-applied keys), `ON LAUNCH` (muted; needs restart).
- `RestartReason` for the 3 reload-applied keys = `None` (no "(restart to apply)" text). `macos-option-as-alt` = `CommitOnly`.
- AC-21 is amended accordingly; add a test asserting the 3 keys ARE picked up by the reload diff functions and that `macos-option-as-alt` is absent from them.
- Do NOT suppress or special-case the ConfigWatcher for app-originated writes (it serves external editors; out of scope).

### D-2. Authoritative badge geometry (FM-05 — absolute pt, resolves Addendum B ambiguity)
label x=20 w=170 · badge x=196 w=44 (right edge 240, right-aligned) · value column x=250 (pad+230, UNCHANGED). 10pt gutter, zero overlap. These absolute numbers win over any other reading.

### D-3. Orbit-loop contract clauses (from omen mitigations; all mandatory)
1. (FM-02) Search-mode key routing lives at the ROUTER: `handle_theme_settings_key`'s Enter/Tab/Backspace arms must check search-active state BEFORE falling through to `commit_theme_settings()`/legacy paths. ↑↓ during search navigate a `settings_highlight` over `settings_filtered` — a separate index space from `selected_row`; both exist without cross-contamination. Integration test: Enter mid-search must NOT commit/close.
2. (FM-06) `reset_selected_row` calls `clear_row_input_state()` (mirroring move_up/move_down). Compound test: digits → reset → one more digit derives from post-reset draft.
3. (FM-04) The description line (19pt) and search line (16pt when active) are added into `settings_top`/the `needed()` baseline so the min-3 shrink loop re-solves correctly; on genuinely too-small panes, drop description/search lines before violating the row floor. Unit test `needed(3) <= avail` at the smallest supported pane.
4. (FM-09) wgpu path: implement extra vertical budget as mode-specific offsets inside `settings_rows_overlay_text` only; do NOT touch shared `THEME_SETTINGS_ROWS`/clamp math used by Theme mode.
5. (FM-07) Hard commit boundary: R-1..R-8 (must-have) committed with `cargo test -p noa-app` green BEFORE any R-9/R-10 diff is authored.
6. (FM-08) `RestartReason` derives `Clone, Copy, Debug, PartialEq, Eq, Hash` (view-model cache dedup).
7. (Atlas invariant) Render path never stores its `Arc<ThemeSettings>` clone back into `self` across turns — code-review gate.
8. (Ripple C-1) `app/timers.rs:490-501` is the 9th `Arc::make_mut` site; `macos_overlay/sync.rs` unchanged. (ime.rs:92 also touches the session — audit it during implementation.)
