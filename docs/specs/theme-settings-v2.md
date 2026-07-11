# Spec: テーマ設定パネル v2 — リッチ化+最適化増分 (theme-settings-v2)

## Metadata

- **slug:** theme-settings-v2
- **title:** テーマ設定パネル v2 — リッチ化+最適化増分 (Theme Settings Panel v2 — Richness + Optimization Increment)
- **status:** draft(magi 3-0全会一致「B−」パッケージが確定スコープ。サインオフ未実施)
- **owner:** simota
- **scope:** **Standard**(traceability ≥85%目標)。理由: 新規要求クラスタ数は16件(R-19〜R-34)で本来Full圏(12+)の目安に達するが、magi裁定で「単一機能の性能是正+リッチ化増分」という一枚岩の作業単位に確定しており、L2をチーム別(Biz/Dev/Design)3分割するほどの多team調整は発生しない(実装者=noa-app/noa-config内のみ、外部stakeholderなし)。Standardの構造(involved L2、主要L3)を採る一方、上位タスク指示に従いL3は全R/NFRを1AC以上でカバーする(「main scenarios」への間引きはしない)。
- **upstream:** `theme-settings-ui.md`(locked, v1 — 用語・R-1〜R-18/NFR-1〜NFR-6/AC-1〜24を前提とする。本specはその増分のみを記述し、再掲しない)
- **magi裁定:** 2026-07-11、3-0全会一致、パッケージ「B−」(性能是正+リッチ化A全体+B低リスク群、データ安全1件、条件付きstretch2件)。本specはその確定スコープ箇条書きをR-19〜R-33/NFR-7〜NFR-9/AC-25以降として形式化したものであり、magiワークシート上のインフォーマルなAC番号(AC-P1〜P3, AC-1〜15, AC-C1/C2)をそのまま転記してはいない — 本specのID体系(下記)への対応表はL1の各項に出典として付す。AC-C1/AC-C2の2ラベルのみ、magi裁定書と同一名称をそのまま踏襲する(スコープ表内で名指しされているため)。
- **ID体系の逸脱(意図的):** Accordの既定ID体系(`REQ-*`/`CFR-*`/`AC-{FEATURE}-{NNN}`)ではなく、v1から続く`R-*`/`NFR-*`/`AC-*`の通し番号(R-19〜, NFR-7〜, AC-25〜)を採用する。理由: v1のR-9/AC-6/AC-8/AC-23等のIDは実装コードのdocコメントに直接引用済み(`state.rs`/`debounce.rs`/`writer.rs`)であり、実装が実在するコードベースとの照合可能性を保つことがAccordの新ID体系への切替より優先度が高いと判断した(CLAUDE.mdの自律規則: 可逆な曖昧決定は既定案を明記して進める)。

## L0 — Vision

- **問題:** `theme-settings-ui`(v1, 2026-07-06サインオフ)は実装完了しmainにマージ済み(2026-07-11時点、テーマピッカーとSettings行は`ThemeSettingsMode::Theme`/`Settings`の別セッションに分割され、両方ともAppKitネイティブカードで描画される — v1の「単一オーバーレイ+Tab切替」設計は実装時に分割へ改訂されており、`toggle_section`は空実装として残存する)。しかし実装完了後の実査で3つの性能負債が判明した: (1) 毎フレームの再描画経路(`render.rs:44-48`)が`ThemeSettings`状態全体を無条件`clone()`している、(2) ネイティブオーバーレイの冪等sync(`macos_overlay/sync.rs:61-83`)がハッシュ比較の**前**に毎回`ThemeSettings`ViewModelを構築しており、変更が無いフレームでも構築コストが発生する、(3) 574テーマのfuzzy検索がキー入力毎に無条件で全件再走査される(debounceなし)。加えてリッチ化面では、Cmd+,がGUIパネルではなく従来通り外部エディタを開く、`OpenThemePicker`/`OpenSettings`コマンドはコマンドパレットからのみ到達可能でメニューバー項目も既定キーバインドも持たない、Tab超えでの逆モード再オープンが未実装、一致件数・コントラスト比・お気に入り・明暗フィルタ・確定後Undo・ホイールスクロールが未実装、といったギャップがある。さらにデータ安全面で、`theme = light:X,dark:Y`のペア設定下でパネルから単一テーマをcommitすると、`noa-config`のsurgical writer(`apply_updates`)がペア構文を認識せず該当行を単一テーマ名で無条件上書きし、設定の片側appearanceが黙示的に失われる実装済みのバグが確認された。
- **提供価値:** 性能是正(F1-F3、非交渉コア)でパネル操作中のフレーム時間を安定させ、リッチ化(magiパッケージB−のA全体+B低リスク群)で発見性・使い勝手を高め、AC-13データ安全対応でペア設定の黙示破壊を構造的に不可能にする。
- **対象:** v1と同じくnoaユーザー本人(単一ユーザー・ローカルアプリ)。
- **成功definition:** 性能是正3件がゼロ回帰(既存テスト: `theme_settings/tests.rs`34件 + `app/input_ops/theme_settings.rs`6件 + `noa-config/src/writer.rs`11件、計51件)で着地し、リッチ化各項目がR-12のcommit順序不変条件(config書込→クロムスワップ)とtouched行境界を破らずに実装され、`theme = light:X,dark:Y`環境下でのパネルcommitがペア構文を破壊しないことがユニット/結合テストで保証される。

## 保全制約(v1からの不変条件 — 本増分が破ってはならないもの)

1. **R-12順序**: commitは「config書込み(失敗しうる唯一の段)」→「クロムスワップ」の順を維持する。書込み失敗時はクロムスワップを行わず`preview_theme`を維持する(`ThemeSettings::commit`, `theme_settings/state.rs:894-910`)。
2. **touched境界**: `SettingsRow.touched`は実際の編集でのみtrueになり、ナビゲーション/再描画では絶対に変化しない(`rows.rs:205-207`のpre-mortem RPN 252コメント)。新規行(お気に入り等の新規UI状態)を追加する場合もこの境界を踏襲する。
3. **プレビュー無汚染**: `preview_theme`は`TerminalColors`に注入しない(AC-2)。新規のプレビュー拡張(サンプル複数行化)もこの経路を再利用し、別経路を新設しない。
4. **既存テスト51件を破壊しない**: `theme_settings/tests.rs`(34)、`app/input_ops/theme_settings.rs`(6)、`noa-config/src/writer.rs`(11)。
5. **3描画同期点の単一情報源**: 下記L2参照。`RowDraft::display_value`/`settings_row_display_value`を各描画経路がフォークしない。
6. **CLI非汚染(NFR-6)**: 新規行・新規機能もCLIオーバーライド値をconfig書込みに混入させない。

## FRAME補訂 — v1からの実装ドリフト

- v1 SHAPE案の「単一オーバーレイ+Tab切替」は実装時に「モード別セッション」(`ThemeSettingsMode::Theme`/`Settings`、`open_theme_settings(mode)`が都度新規セッションを開く)へ改訂済み。`toggle_section`(`state.rs:244`)は「セッションの`Section`は`ThemeSettingsMode`で生涯固定」というdocコメント付きの意図的な空実装であり、バグではない。本specのR-24(Tab逆モード再オープン)はこの新アーキテクチャを前提に設計する(v1のTab仕様をそのまま復元するのではない)。
- v1 L2で言及されていた「ChromeTextures再構築カウンタ」パターン(debug限定`AtomicUsize`)は実装済み(`gpu.chrome_textures.record_rebuild()`が`app/sidebar/palette.rs`の描画ヘルパー内で呼ばれている)。本specのNFR-7計測はこの既存パターンを流用する。
- ネイティブAppKitカード化(`macos_overlay/`)はv1には無かった追加実装であり、F1/F2の性能負債はこのネイティブ化に伴って新たに生じたものである。

## L1 — Requirements

### 性能是正(非交渉コア)

- **R-19(F1・出典: magiパッケージ性能是正1件目)**: `App::redraw`(`app/render.rs:44-48`)の`theme_settings_card`構築は現在`session.state.clone()`で`ThemeSettings`全体(574件走査済みの`filtered: Vec<ThemeMatch>`を含む)を毎フレーム複製している。これを描画専用の薄いスナップショット型に置換し、フレームごとの複製コストを`filtered`の全複製から解放する。
- **R-20(F2・出典: magiパッケージ性能是正2件目)**: `macos_overlay::sync_theme_settings`(`macos_overlay/sync.rs:61-83`)は現在ハッシュ比較の**前**に`theme_settings_view_model(state)`を無条件構築し(69行目)、ハッシュが変化していれば同じ構築をもう一度行う(80行目)。ViewModel構築前に軽量な同一性キー(filter文字列・highlighted/selected_rowインデックス・catalogのepoch値・rect・colorsのハッシュ等、ViewModelを組み立てずに導出可能な値のみ)を先に比較し、冪等な(前フレームと状態が変わっていない)syncでは`theme_settings_view_model`の呼び出し回数を0にする。
- **R-21(F3・出典: magiパッケージ性能是正3件目)**: テーマピッカーの`push_text`/`backspace`(`theme_settings/state.rs:384-437`)は文字入力毎に`recompute_filtered`→`filter_themes`(574件全走査、`fuzzy_match`を全件に適用)を無条件実行する。既存`debounce.rs`の`Debouncer<T>`パターン(F1-F3と同じモジュール、`ThemeSettings::font_size_debounce`が既に使用中)を再利用し、(a)高速連続入力をdebounceでまとめて末尾値のみ発火、(b)新フィルタ文字列が直前のフィルタ文字列の拡張(prefix継続)である場合は前回`filtered`結果集合内でのみ再走査する差分絞込みを行う。フィルタ文字列が短縮(Backspaceでprefix関係が崩れる)された場合は全574件の再走査にフォールバックする。

### メニュー・キーバインド

- **R-22(出典: magiパッケージ「Cmd+,をGUIオーバーレイ起動に変更」)**: `AppCommand::Preferences`の識別子・`PREFERENCES_MENU_ID`・Cmd+,アクセラレータ(`macos_menu.rs:551-558`)はそのまま維持し、ディスパッチ本体(`app/commands.rs:66`、現状`AppCommand::Preferences => crate::app_actions::open_config_file()`)のみを`self.open_theme_settings(ThemeSettingsMode::Settings)`へ差し替える。既存の`preferences_menu_item_is_enabled_and_routes_to_preferences`テスト(`macos_menu.rs:729`)はメニュー項目のidentity/ルーティングのみを検証しておりディスパッチ先の実装詳細を検査しないため、この変更で失敗しない(要着手時再確認)。
- **R-23(出典: magiパッケージ「configファイルを開く従来動作を別コマンドとして温存」)**: 従来の外部エディタ起動(`open_config_file()`)を新規`AppCommand::EditConfigFile`として独立させ、既存のPreferencesメニュー項目の近くに新規メニュー項目(ラベル例: "Edit Config File...")を追加する。コマンドパレットにも新規エントリを追加する(`command_palette.rs`の`AppCommand::Preferences => "Open Preferences"`と同型の1行追加)。既定キーバインドは割り当てない(Cmd+,の意味変更で操作導線が変わるため、新規チャタリング防止として無割当のまま、config keybind経由でのみユーザーが任意のchordを割り当てる)。
- **R-24(出典: magiパッケージ「OpenThemePicker/OpenSettingsのメニュー掲載+既定キーバインド」)**: `AppCommand::OpenThemePicker`にメニュー項目(ラベル例: "Open Theme...")と既定キーバインド`cmd+shift+,`を追加する(`KeybindEngine::default()`の`specs`配列に追加。`cmd+shift+,`は現行既定バインド一覧に未使用であることを確認済み)。`AppCommand::OpenSettings`はR-22によりCmd+,(Preferences経由)から同じ`ThemeSettingsMode::Settings`へ到達可能になるため、`OpenSettings`変数自体への重複デフォルトチョード付与は行わない(コマンドパレット/config keybindアクション名`settings.open`/`open_settings`からの直接到達性は変更なしで維持)。この判断は「曖昧・可逆」区分の既定選択であり、Open Questionsに理由を記録する。

### セッション・UX(リッチ化)

- **R-25(出典: magiパッケージ「Tab逆モード再オープン、フィルタ/スクロール引継」)**: Tabキーは現状no-op(`toggle_section`空実装)。これを「現在のセッションを`ThemeSettingsMode`の逆モードで再オープンし、フィルタ文字列(Theme→Settings→Theme時)またはスクロール位置(`selected_row`)を引き継ぐ」動作に変更する。この再オープンはEsc(revert)ともEnter(commit)とも異なる第三の遷移であり、`gpu.preview_theme`・ライブ適用済みの行(font-size/opacity/blur/cursor-style/sidebar-preview-lines)の実行時状態を一切変更しない(config書込みも行わない)。
- **R-26(出典: magiパッケージ「一致件数ライブ表示」)**: Theme モードの表示に `highlighted位置+1 / filtered_len()` 形式(例: `12 / 574`)の一致件数を追加する。キーヒントフッター(`ThemeSettingsViewModel::footer`)は既に実装済みであり、本要求は既存フッターへ件数表示を付加する差分のみとする。
- **R-27(出典: magiパッケージ「コントラスト比表示+低コントラスト警告」)**: `noa_render::theme::contrast_ratio(a: Rgb, b: Rgb) -> f32`(既存公開関数、`noa-render/src/theme.rs:177`)を再利用し、ハイライト/プレビュー中テーマの`default_fg`/`default_bg`間コントラスト比をTheme モードの表示に追加する。WCAG AA相当(4.5:1、`noa-render`の`DEFAULT_MINIMUM_CONTRAST`と同値)を下回る場合に警告表示(色/アイコンいずれか、ネイティブ・wgpu両経路で表現可能な手段)を出す。新規のコントラスト計算ロジックを実装しない(既存関数の呼び出しのみ)。
- **R-28(出典: magiパッケージ「font-family fuzzy検索」)**: `SettingsRowKind::FontFamily`行(現状`cycle_font_family`による←→巡回のみ、`state.rs:719-731`)をfuzzy検索可能にする。`command_palette::fuzzy_match`(既存、テーマピッカーと同一マッチャー)を再利用し、第二のマッチャーを実装しない。確定のみ行(`is_live() == false`)という既存分類は変更しない。
- **R-29(出典: magiパッケージ「お気に入り: 別状態ファイル永続」)**: テーマの「お気に入り」トグルを追加する。永続化先は`~/.config/noa/config`ではない別状態ファイル(config writerのsurgical更新契約・R-12/R-14に一切関与しない)とし、Theme モードの追加フィルタ(「お気に入りのみ表示」トグル)としてのみ機能する。commit経路(`commit_updates`/`write`)には一切触れない。
- **R-30(出典: magiパッケージ「明暗属性フィルタ: fg/bg輝度からオンザフライ導出」)**: 各テーマの`default_fg`/`default_bg`から相対輝度を算出し(既存`noa_render::theme`の輝度計算 — `relative_luminance`相当のロジックを再利用、`ThemeDef`スキーマは変更しない)、「Light/Dark」属性フィルタをTheme モードに追加する。事前計算キャッシュ(574件×輝度)を持つか都度計算するかは実装時判断(NFR-8のスクラブ性能要件を満たす限り自由)。
- **R-31(出典: magiパッケージ「コミット後Undoトースト」)**: Enter確定(commit)成功直後に、直前のcommit前スナップショット(`RevertValues`)を保持したUndoトーストを表示する。Undo操作は既存commit経路(`ThemeSettings::commit`と同じ書込み関数)を使って直前スナップショットの値を再commitする(「commit経路不変」— 新しい書込み/適用機構を作らない)。トーストの表示・描画は既存の汎用トーストカード機構(`draw_toast_card`/`macos_overlay::sync_toast`、現状は`WindowState.resize_overlay: Option<(String, Instant)>`のリサイズ専用フィールドが唯一の呼び出し元)を一般化して再利用する。resize toastとUndo toastが時間的に重なるケースの優先順位(新しい方が古い方を置き換える)を実装時に定義する。
- **R-32(出典: magiパッケージ「スクロールホイール対応」)**: `App::on_mouse_wheel`(`app/event_loop.rs:1124`)は現状、サイドバー帯(`handle_sidebar_wheel`)を除きテーマ設定オーバーレイの開閉状態を一切考慮せず、パネルが開いていてもホイールイベントがそのままペインのターミナルスクロールへ流れてしまう。`handle_sidebar_wheel`と同じ「(bool)消費済みなら true を返す」契約で、`self.active_overlay(window_id) == ActiveOverlay::ThemeSettings`のとき新規`handle_theme_settings_wheel`へルーティングする早期分岐を追加し、ホイールデルタを既存↑↓ナビゲーション(Theme モードのハイライト移動 / Settings モードの行選択)へマッピングする。クリックは対象外(magiスコープ境界)。ホイール蓄積ロジックは`app/overview/interaction.rs::apply_overview_wheel`の`WHEEL_PAGE_THRESHOLD`蓄積パターンを参考実装として踏襲する。
- **R-33(出典: magiパッケージ「プレビュー: 実色を使った代表サンプル複数行」)**: 現状の`sample_swatches`(`theme_settings/sample.rs`)は16 ANSI色+fg/bg/cursor/selection+truecolor1件の色パッチのみを提供している。これを「実際のfg/bg/選択色を使ったテキストサンプル行」(例: 通常テキスト行・強調テキスト行・選択ハイライト行など複数行)として、既存の3描画同期点(下記L2)の両方(wgpu `theme_picker_overlay_text`、ネイティブ`theme_settings_view_model`)に同一フレーム内で反映する。`sample_swatches`が返す色データ自体は再利用し、新しい色導出ロジックを追加しない。

### データ安全(非交渉コア)

- **R-34(AC-13・出典: magiパッケージ「theme = light:X,dark:Y下での黙示上書き禁止」)**: `open_theme_settings`(`app/input_ops/theme_settings.rs:32-96`)は現状`self.config.theme`(`Option<String>`、pairはappearanceで既に単一名へ解決済み)のみを`ThemeSettingsInit.current_theme`へ渡し、`self.config.theme_appearance: Option<noa_config::ThemeAppearancePair>`(`app/config.rs:23`、pair情報そのものを保持)を一切参照していない。このため`ThemeSettings::commit_updates`(`state.rs:806-812`)が`updates.push(("theme".to_string(), name))`を生成すると、`noa_config::apply_updates`(`writer.rs:27-78`)は`theme`キーの最終行(pairの`light:X,dark:Y`行そのもの)を単一テーマ名で無条件置換し、pair構文を破壊する。本要求は、パネルからの単一テーマcommit時に元configがpair設定であった場合、**現在アクティブなappearance側のみを書き換え、pair構文を維持したまま他方のappearance値を保持する**ことを義務付ける(magi裁定の2択「提示+明示confirm」「現在外観側のみ書換」のうち後者を既定実装として採用 — 理由はL2記載)。

### 非機能要件

- **NFR-7(性能, F1/F2)**: パネルが開いた状態で状態変化のない定常フレーム(冪等sync)は、`ThemeSettings`の`clone()`呼び出し回数0、`theme_settings_view_model`呼び出し回数0で完了する。既存の`ChromeTextures`再構築カウンタ(debug限定`AtomicUsize`)パターンを踏襲した計測手段を新設する。
- **NFR-8(性能, F3)**: 574件カタログに対する連続キー入力バーストは、debounceウィンドウあたり1回のフル/差分fuzzy走査に収まる。新フィルタ文字列が直前フィルタのprefix拡張である限り、走査対象は前回`filtered`結果集合(574件全体ではない)に限定される。
- **NFR-9(データ安全, R-34)**: `theme = light:X,dark:Y`を含むconfigに対しパネルから単一テーマをcommitした後も、`light:`/`dark:`両トークンを含む有効なpair構文が`noa_config::parser::values::parse_theme_pair`でパース可能な状態で残る。書込み後、変更されたappearance側以外の値は書込み前とバイト単位で一致する(既存NFR-5の精神をpairケースへ拡張)。

## L2 — Detail

### 3つの描画同期点(F2/R-33が特に触れる箇所)

theme-settingsは3つの独立した描画経路を持ち、これらは共通の純粋関数で値を一致させる契約になっている。本増分がこの契約を保つことは保全制約5の具体化である。

1. **wgpuフォールバックテキスト経路** — `app/sidebar/palette.rs`の`theme_settings_overlay_text`→`theme_picker_overlay_text`/`settings_rows_overlay_text`。ANSI端末セルとして描画されるテキスト表現。
2. **共有値フォーマッタ** — `theme_settings/rows.rs`の`RowDraft::display_value`/`settings_row_display_value`。上記(1)と下記(3)の**両方**から呼ばれる単一の値整形関数。R-26(件数)・R-27(コントラスト)・R-29(お気に入りマーク)等の新規表示要素は、この関数(または同格の新規共有関数)を経由して両経路に反映させ、どちらか一方だけをフォークしてはならない。
3. **ネイティブViewModelビルダー** — `macos_overlay/model.rs`の`theme_settings_view_model`。AppKitカードが実際に描画する構造化データ。R-20(F2)のハッシュ前置最適化はこの関数の呼び出しコストそのものを対象とする。

### noa-app: 状態機械・セッション

- `ThemeSettings`(`theme_settings/state.rs`)に以下を追加する:
  - R-25用: `carryover()`(現在の`filter`/`highlighted`または`selected_row`を取り出す)と、`ThemeSettingsInit`への`Option<ThemeSettingsCarryover>`フィールド追加(`ThemeSettings::open`が非`None`のとき、デフォルトのfilter/highlighted初期化をcarryover値で上書きする)。
  - R-28用: `FontFamily`行の`draft`を文字列のまま維持しつつ、`cycle_font_family`と並行して`filter_font_families(query) -> Vec<FontMatch>`相当のfuzzy一覧関数を追加(`filter_themes`と同型のパターン)。
  - R-29/R-30用: `favorites: &FavoritesStore`(または同等の参照)と`attribute_filter: Option<Light|Dark>`をセッション状態に追加。commitの`commit_updates()`には一切影響させない(お気に入り・属性フィルタはフィルタ条件としてのみ`filter_themes`の絞込みに合流する)。
  - R-31用: `commit`成功時に返す`Vec<(String, String)>`とは別に、commit直前の`self.snapshot: RevertValues`のクローンを`App`側へ返す(Undoトーストが再commitする対象)。
- `App::open_theme_settings`(`app/input_ops/theme_settings.rs:32`)に以下を追加する:
  - R-34用: `ThemeSettingsInit`に`theme_appearance: Option<noa_config::ThemeAppearancePair>`フィールドを追加し、`self.config.theme_appearance.clone()`を渡す(現状は`self.config.theme`しか渡していない)。
  - どのappearance側が「現在アクティブ」かは既存`effective_theme_name`/`app/config.rs:367`と同型のappearance解決ロジック(winitの`Theme::Light`/`Theme::Dark`)を再利用して判定する。新しい判定ロジックを作らない。
- Tab(R-25)の新ハンドラは`close_theme_settings`(Esc相当)にも`commit_theme_settings`(Enter相当)にも分岐せず、第三の遷移として`ThemeSettingsSession`を新mode向けに再構築する。`gpu.preview_theme`とライブ適用済み実行時状態(runtime_font_size等)は一切触れない。

### noa-app: 入力

- `on_mouse_wheel`(`app/event_loop.rs:1124`)に`handle_sidebar_wheel`と同じ位置(パネルスクロール判定より前、pane scroll routingより前)で`ActiveOverlay::ThemeSettings`分岐を追加する(R-32)。

### noa-app: メニュー・キーバインド

- `macos_menu.rs`: 新規メニュー項目2件(`EditConfigFile`、`OpenThemePicker`)。`preferences_menu_item_spec()`と同型の`_menu_item_spec()`関数を追加する。
- `commands/command.rs`: 新規`AppCommand::EditConfigFile`バリアント追加(`menu_id()`/`action_name()`/パレットタイトルの登録)。`OpenThemePicker.menu_id()`を`""`から実IDへ変更。
- `commands/keybind.rs`: `KeybindEngine::default()`の`specs`配列に`("cmd+shift+,", AppCommand::OpenThemePicker)`を追加。

### noa-config: writer / pair安全性(R-34)

- `noa-config/src/writer.rs`の`apply_updates`自体は「keyの最終行を置換する」という現行契約のまま変更しない(他の全キー・NFR-5のバイト精度保証に影響させない)。
- 呼び出し元(`noa-app`)に新しい前処理層を追加する: commit直前に、対象configの`theme`ディレクティブがpair構文かどうかを判定し(`ThemeSettingsInit.theme_appearance.is_some()`で判定可能、パース済み情報を再利用でき、生テキストの再パースは不要)、pairだった場合は`updates`に`("theme", name)`を積む代わりに`("theme", "light:<new-or-kept>,dark:<new-or-kept>")`という**pair構文そのものの文字列**をvalueとして積む。アクティブでない側の値は`ThemeSettingsInit.theme_appearance`から取得した元の値をそのまま使う(変更しない)。この方式なら`apply_updates`のロジック自体には一切手を入れず、渡す`value`をpair文字列に変えるだけで済む — 最小差分でNFR-9を満たす。
- 「提示+明示confirm」側(magi裁定のもう一方の選択肢)を採らない理由: (a) 新しいモーダル種別の追加はmagiスコープの他項目(「フルマウスクリックはスコープ外」等、対話コスト最小化の方針)と整合しない、(b) 現在アクティブ側のみ書換は常にEsc/Undoトースト(R-31)で可逆、(c) 既存のtouched行モデル(保全制約2)に自然に収まる(pair書換の判断はtouchedフラグの副作用としてL2内で完結し、新規UI状態を要さない)。AC-C2(条件付きstretch、下記)はこの上に「パネル内でpairそのものを編集するUI」を追加するものであり、本要求の「アクティブ側のみ書換」を代替するものではなく拡張するものである。

### noa-render

変更なし(v1同様、`OverlayStyle::from_theme()`はオンデマンド計算のため`noa-app`側の`Theme`差替に自動追従する)。`contrast_ratio`(R-27)は既存公開関数の呼び出しのみ。

### エッジケース

- **carryover中にfilteredが空になる(R-25)**: Theme→Settings→Theme往復でフィルタ文字列を引き継いだ結果、574件カタログの状態が変わっていなくても一致0件になることは理論上ない(カタログは静的)が、お気に入り/属性フィルタ(R-29/R-30)がONの状態でcarryoverすると一致0件になり得る。AC-16と同じ「一覧は空、直前のpreview_themeは維持」の挙動を踏襲する。
- **お気に入り状態ファイルが読めない(R-29)**: 起動時読込失敗は空のお気に入り集合にフォールバックし、パネル自体の起動をブロックしない(config読込エラーと同じ「best-effort、warnログのみ」方針)。
- **pairの一方の名前が574カタログに存在しない(R-34)**: `theme_appearance`のvalidationは`noa-config`側で既に行われている(`theme_pair_diagnostic`)ため、パネルが開いた時点で渡される`theme_appearance`は必ず両側とも文字列として存在する(名前解決の成否はconfig層の責務のままとし、本増分では再検証しない)。
- **resize toastとUndo toastの同時発生(R-31)**: 新しい方が古い方を即座に置き換える(単一の`WindowState`トースト表示スロットのまま、種別のみ`enum ToastKind { Resize, Undo }`等でタグ付けする)。

## L3 — Acceptance Criteria

検証手段の凡例はv1を踏襲: [unit]=GPU不要のユニットテスト / [integration]=tempdir等を使う結合テスト / [code-review]=実装検査 / [計測]=debugカウンタ等による定量計測 / [GUI目視]=手動確認。

### 性能是正

- **AC-25(R-19)** [計測]: Given パネルが開いた状態で10フレーム連続描画する。When 各フレームの`ThemeSettings`複製経路を計測する。Then 新しい描画専用スナップショット型への変換のみが発生し、`filtered: Vec<ThemeMatch>`を含む全体複製が発生しない(複製対象データ量が旧実装比で大幅減、具体的な計測手段はNFR-7と共用のdebugカウンタ)。
- **AC-26(R-20, NFR-7)** [unit]: Given 前フレームと状態が完全に同一な`ThemeSettings`で`sync_theme_settings`相当のロジックを2回連続呼ぶ。When 2回目の呼び出しを計測する。Then `theme_settings_view_model`(またはその等価構築関数)の呼び出し回数が0である。
- **AC-27(R-20)** [unit]: Given 状態が実際に変化した(例: highlighted移動)`ThemeSettings`で同ロジックを呼ぶ。When 呼び出し結果を検査する。Then 軽量キーの比較で変化が検出され、ViewModelが再構築される(誤って変化を見逃さないことの検証)。
- **AC-28(R-21, NFR-8)** [unit]: Given フィルタ文字列に対し高速連続のprefix拡張キー入力列(例: "3"→"30"→"302"→"3024"、間隔<debounceウィンドウ)を投入する。When debounceウィンドウ経過をシミュレートする。Then 全574件に対するフル走査は1回のみ発生し、中間状態では前回`filtered`結果集合内のみが走査される(走査件数の計測、または呼び出し回数の記録によるテスト)。
- **AC-29(R-21)** [unit]: Given prefix拡張ではない変更(Backspaceでprefix関係が崩れる、または全く異なる文字列に置換)を投入する。When 再フィルタを実行する。Then 全574件に対するフル走査にフォールバックする(差分絞込みの誤検出防止)。

### メニュー・キーバインド

- **AC-30(R-22)** [unit]: Given `AppCommand::Preferences`をディスパッチする。When ディスパッチ結果を検査する。Then `open_config_file()`は呼ばれず、`ThemeSettingsMode::Settings`でオーバーレイが開く。
- **AC-31(R-22)** [code-review]: `macos_menu.rs:729`の`preferences_menu_item_is_enabled_and_routes_to_preferences`テストがR-22実装後も変更なしでパスすることを確認する(メニュー項目のidentity/アクセラレータ自体は不変であることの裏付け)。
- **AC-32(R-23)** [unit]: Given `AppCommand::EditConfigFile`をディスパッチする。When ディスパッチ結果を検査する。Then 既存`open_config_file()`と同じ副作用(外部エディタ起動)が発生する。
- **AC-33(R-24)** [unit]: Given `KeybindEngine::default()`を構築する。When `cmd+shift+,`に対応するコマンドを解決する。Then `AppCommand::OpenThemePicker`が返る。

### セッション・UX

- **AC-34(R-25)** [unit]: Given Theme モードでフィルタ文字列"abc"を入力した状態でTabを押す。When 新セッションの状態を検査する。Then `mode == Settings`であり、直前にThemeモードで入力していたフィルタ文字列はSettingsモードには適用されない(Settingsモードにフィルタ概念がないため無視される)が、続けてもう一度Tabを押してTheme モードへ戻ったとき、フィルタ文字列"abc"が復元されている。
- **AC-35(R-25)** [unit]: Given Settings モードで`selected_row`が5の状態でTabを押しTheme モードへ、さらにもう一度TabでSettings モードへ戻る。When 復帰後の`selected_row`を検査する。Then 5が維持されている。
- **AC-36(R-25)** [unit]: Given Tab遷移の前後で`gpu.preview_theme`相当の状態とライブ適用済みfont-size等の実行時値を用意する。When Tabで往復する。Then これらの値がTab遷移の前後で一切変化しない(revert/commitのいずれのコードパスも通っていないことの検証)。
- **AC-37(R-26)** [unit]: Given フィルタ結果が574件中12件でhighlightedが3番目(0-index 2)の状態。When 件数表示用のデータを取得する。Then "3 / 12"相当の値が得られる。
- **AC-38(R-27)** [unit]: Given 既知のfg/bg色ペア(コントラスト比が計算済みの固定値)を持つテーマをハイライトする。When コントラスト比表示ロジックを呼ぶ。Then `noa_render::theme::contrast_ratio`の返り値と一致し、4.5未満のとき警告フラグが立つ。
- **AC-39(R-28)** [unit]: Given `available_font_families`に対しfuzzy検索クエリを入力する。When 結果を検査する。Then `command_palette::fuzzy_match`と同一のスコアリング/ハイライト位置が得られる(第二のマッチャーが実装されていないことの検証)。
- **AC-40(R-29)** [unit]: Given テーマAを「お気に入り」に追加し、フィルタを「お気に入りのみ」に切り替える。When 一覧を検査する。Then テーマAのみ(または「お気に入り」集合とfuzzy一致条件の積集合)が表示され、`commit_updates()`の出力にお気に入り関連のキーが一切含まれない。
- **AC-41(R-29)** [integration]: Given tempdir上のお気に入り状態ファイルが存在しない状態でお気に入りを1件追加する。When 状態ファイルを検査する。Then 新規ファイルが作成され、config書込み(`write_config_updates`)は一切呼ばれない。
- **AC-42(R-30)** [unit]: Given 既知のfg/bg輝度を持つ2テーマ(片方は明らかにlight、片方はdark)を用意する。When 属性フィルタを"Light"に設定する。Then dark判定のテーマが一覧から除外される。
- **AC-43(R-31)** [unit]: Given commit成功直後の状態。When Undoトーストのトリガ条件を検査する。Then commit前の`RevertValues`スナップショットを保持したトースト表示フラグが立つ。
- **AC-44(R-31)** [unit]: Given Undoトーストが表示された状態でUndo操作を実行する。When 書込み関数の呼び出しを検査する。Then commitと同じ書込み関数(`write_config_updates`)がcommit前スナップショットの値で呼ばれ、新しい書込み経路は使われない。
- **AC-45(R-32)** [unit]: Given `ActiveOverlay::ThemeSettings`が開いている状態でホイールイベントを送る。When `on_mouse_wheel`相当のロジックを呼ぶ。Then イベントが消費され(戻り値`true`相当)、ペインのターミナルスクロールへは伝播しない。
- **AC-46(R-32)** [unit]: Given Theme モードでホイールイベントを送る。When 蓄積ロジックを検査する。Then `apply_overview_wheel`と同型の閾値蓄積を経てhighlighted移動が発生する(1ノッチ=1件送りの単純マッピングではなく、既存パターンとの一貫性)。
- **AC-47(R-33)** [unit]: Given `sample_swatches`が返すfg/bg/選択色データ。When 複数行サンプルの生成ロジックを呼ぶ。Then 生成された各行が実テーマのfg/bg/選択色のいずれかを実際に使用している(ハードコードされたプレースホルダ色が含まれないことの検証)。
- **AC-48(R-33)** [code-review]: `theme_picker_overlay_text`(wgpu経路)と`theme_settings_view_model`(ネイティブ経路)の両方が同一の複数行サンプル生成関数を呼んでいることを実装検査で確認する(3描画同期点の契約が新規機能でも保たれていることの検証)。

### データ安全

- **AC-49(R-34, NFR-9)** [unit]: Given `theme_appearance = Some(ThemeAppearancePair { light: "A", dark: "B" })`で開いたセッションが、現在appearance=Lightの状態でテーマ"C"をハイライトしてcommitする。When `commit_updates()`の出力を検査する。Then `("theme", "light:C,dark:B")`(dark側"B"が保持される)が含まれ、`("theme", "C")`という単純上書き値は含まれない。
- **AC-50(R-34, NFR-9)** [integration]: Given tempdir上に`theme = light:A,dark:B`を含むconfigファイルが存在する状態で、AC-49と同じcommitをファイルへ実際に書き込む。When 書込み後のファイルを`parse_theme_pair`でパースする。Then 有効なpairとしてパースでき、`light`が新値、`dark`が旧値"B"のまま一致する。
- **AC-51(R-34)** [unit]: Given `theme_appearance = None`(pairでない通常のconfig)で開いたセッションでテーマをcommitする。When `commit_updates()`の出力を検査する。Then 従来通り`("theme", "<name>")`という単純な値が出力される(pairでない場合の既存挙動に回帰がないことの検証)。

### 条件付きstretch(実装ループが安価と判断した場合のみ)

- **AC-C1**(プレビューの実セルレンダラー化): [code-review]+[GUI目視]。`noa-render/tests/pipeline.rs`のグリーン維持が実装条件。維持できない場合はこの1件のみ見送り、他のACには影響しない。
- **AC-C2**(パネル内pair編集UI): [GUI目視]。AC-49/AC-50(R-34)の「アクティブ側のみ書換」実装後、pairの両側を明示編集できるUIをほぼ無償で追加できると実装時に判断した場合のみ着手する。着手しない場合もAC-49/AC-50は独立して満たされる。

## トレーサビリティ表

| Requirement | AC |
|---|---|
| R-19 | AC-25 |
| R-20 | AC-26, AC-27 |
| R-21 | AC-28, AC-29 |
| R-22 | AC-30, AC-31 |
| R-23 | AC-32 |
| R-24 | AC-33 |
| R-25 | AC-34, AC-35, AC-36 |
| R-26 | AC-37 |
| R-27 | AC-38 |
| R-28 | AC-39 |
| R-29 | AC-40, AC-41 |
| R-30 | AC-42 |
| R-31 | AC-43, AC-44 |
| R-32 | AC-45, AC-46 |
| R-33 | AC-47, AC-48 |
| R-34 | AC-49, AC-50, AC-51 |
| NFR-7 | AC-26 |
| NFR-8 | AC-28 |
| NFR-9 | AC-49, AC-50 |

全 16 R + 3 NFR に各 ≥1 AC、計27 AC(AC-25〜51)+ 条件付き2 AC(AC-C1/C2)。要求16件(R-19〜R-34)中16件がAC紐付け済み = **traceability 100%**(Standardの目標値85%を上回る。理由: 上位タスク指示で全R/NFRのAC具体化が明示的に求められたため、Standardの「主要L3のみ」という通常運用より広めにカバーした)。[GUI目視]を主検証とするのはAC-C1/AC-C2の2件のみ、他は[unit]/[integration]/[code-review]/[計測]で自動検証可能。

## Open Questions / Deferred Decisions

- **R-24のOpenSettings既定キーバインド非付与**: `AppCommand::OpenSettings`自体には新規デフォルトchordを与えず、Cmd+,(Preferences経由)での到達のみとした。ユーザー実機フィードバックで「OpenSettings単体の直接chordが欲しい」と判明した場合、空いているchord(例: `cmd+alt+,`)を追加する増分は本specの範囲内で吸収可能(構造変更不要)。
- **R-31のトースト表示秒数**: 具体的なミリ秒値(resize toastとの整合、既存`resize_overlay`のタイムアウト値を踏襲するかUndo専用に長め設定するか)は実装時判断とする。
- **AC-C2着手可否**: R-34実装完了後の実装ループでの見積り次第。着手しない場合の代替: pair設定ユーザーはCmd+,の外(温存された`AppCommand::EditConfigFile`、R-23)から手動でpairの他方を編集する。
- **お気に入り状態ファイルのパス/形式**: `~/.config/noa/theme-favorites`相当の想定だが、noa全体の他の状態ファイル(セッション保存等)の既存パス規約(`noa-app/src/session.rs`)との整合は実装時に確認する。

## Build-path decision

未確定(magi裁定時点でbuild-path選択は未実施)。次工程はAccordのAUTORUN契約に従い**atlas(Tech design)**への引き継ぎを推奨する — F1/F2/F3の性能是正3件は既存アーキテクチャ内の局所最適化でatlas設計コストが小さい一方、R-34(データ安全)はconfig writerの呼び出し契約に新しい前処理層を追加するため、着手前に設計判断(前処理層の配置場所、`ThemeSettingsInit`拡張の型設計)をatlasで固めることを推奨する。

---

## Amendments (Phase 5 Risk Gate 反映, 2026-07-11)

- **AC-52 (新規, must)**: フィルタ状態変更(⌃D/⌃⇧F/Tab carryover)によるfiltered再計算では、(a)ハイライト中テーマが残存するならハイライトが追跡し preview 不変、(b)除外時は preview_theme 不変+ハイライト先頭移動+`highlight_moved`リセット(明示的な↑↓までpreview非発火)、(c)0件時はAC-16準拠。検証: unit test(フィルタトグル前後のpreview_theme/highlighted assert)。詳細: theme-settings-v2.ux.md Addendum A-2。
- **AC-53 (新規, must)**: お気に入りチップに局所キャプション`⌃⇧F`を表示(⌃D cycleと対称)。検証: 両描画経路の文字列生成関数のunit test。詳細: ux.md Addendum A-1。
- **AC-28 実装形調整 (ADR-3準拠)**: タイマーdebounceではなくprefix差分絞込みで「キー入力毎の574件全fuzzy再走査禁止」を満たす。検証: 前進入力時の走査スコープが前回filtered集合内であることのassert。

## Amendments 2 (omen FMEA反映, 2026-07-11) — R-34グループ追加AC

- **AC-54 (must)**: `theme_appearance = Some(light:A,dark:B)`・システム外観Light下でSettingsモードを開くと `current_theme == "A"`(Darkなら`"B"`)。空文字列にならない。検証: unit。
- **AC-55 (must)**: 同前提でtheme picker非接触・非theme行のみtouchしてcommitしたとき、`commit_updates()` 出力に `"theme"` キーが含まれない。既存 `settings_mode_commit_updates_never_includes_a_theme_change` をpair解決経路のfixtureで複製した新テストで検証。
- **AC-56 (must)**: Settingsモードでは `highlight_moved` が常にfalseである不変条件テスト(FM-01二重防御)。
- **AC-57 (must)**: pair×carryover×favoritesトグル×commit の統合シナリオテスト1件以上(FM-03)。
- **AC-58 (must)**: `rebuild_theme_settings` にdebug専用rebuildカウンタ(ChromeTextures.record_rebuild()パターン)を追加し、sync経由の真の状態変化1回につきrebuild=1をテストで固定(FM-05)。release buildでのフィルタ打鍵レイテンシの手動計測ノート1回をPR/ジャーナルに記録。
- **AC-59 (must)**: 複数回Tab往復後のEscが**最初の**open時点まで巻き戻す(FM-04、AC-36拡張)。
- **AC-60 (must)**: 全mutatorがview_fingerprintを変えることのproperty test(FM-02格上げ)。
- **AC-61 (code-review)**: チップ行追加のwgpu/native縮退戦略が明示的に対応していること(FM-06)。
- **AC-62 (must)**: Undo再commitの割込みガード(トースト表示後に別commit/再openがあれば無効化)(FM-08)。favorites書込失敗は無音にしない(FM-09)。
- **FM-10は ACCEPT-RISK**: コントラスト閾値4.5は独立リテラル定数(ユーザーの minimum-contrast 設定と無関係)で妥当。
