# theme-settings-v2 技術設計 (Atlas / MADR軽量)

- **status:** proposed（実装ループ着手前の設計固め）
- **対象spec:** `docs/specs/theme-settings-v2.md`（R-19〜R-34 / NFR-7〜9 / AC-25〜51）
- **設計原則:** 最小構造変更・既存34+6+11テスト保全・R-12 commit順序不変・依存規則（noa-appのみwinit、noa-render/noa-appのみwgpu）・GPU gotchas不変
- **実査済み根拠ファイル:** `theme_settings/state.rs` `rows.rs`、`macos_overlay/sync.rs` `model.rs`、`app/render.rs`、`app/sidebar/palette.rs`、`app/input_ops/theme_settings.rs`、`app/config.rs`、`noa-config/src/writer.rs` `lib.rs`、`debounce.rs`、`session.rs`

---

## 全体像（決定サマリ）

| # | 論点 | 決定（1行） |
|---|------|------|
| ADR-1 | F1 スナップショット型 | 新型を作らず `filtered`/`available_font_families` を `Arc` 化して既存 `clone()` をO(1)化。両描画経路の署名は不変。 |
| ADR-2 | F2 軽量キー | ViewModelを組まない `ThemeSettings::view_fingerprint(&mut Hasher)` を状態側に置き、ADR-1の `Arc` ポインタ恒等＋favorites世代で照合。冪等フレームはViewModel構築0回。 |
| ADR-3 | F3 fuzzy | タイマーdebounceを採らずprefix差分絞込み一本。前進入力は即時（前回集合内のみ走査）、prefix破壊時のみ全574再走査。 |
| ADR-4 | AC-13 pair層 | `commit_updates()` 内でpair文字列を生成（AC-49がこれを直接検証するため配置はここ一択）。`ThemeSettingsInit` に解決済み `ThemePairContext` を通す。writer無改変。 |
| ADR-5 | 3同期点への新要素通し | 描画モデルを統合せず、既存 `settings_row_display_value` 型の純粋leaf関数を `theme_settings` モジュールに追加し2経路が共有。favorites永続とtoast一般化もここに従属。 |

---

## ADR-1 — F1: 描画スナップショットは新型ではなく `Arc` 内包で作る

### Context
`App::redraw`（`app/render.rs:44-48`）は毎フレーム `session.state.clone()` で `ThemeSettings` 全体を複製する。`#[derive(Clone)]`（`state.rs:80`）が deep-copy するのは主に `filtered: Vec<ThemeMatch>`（最大574件、各 `Vec<usize>` 付き）と `available_font_families: Vec<String>`（フォント一覧、数十〜数百）。この複製された値は macOS 経路（`sync_theme_settings` → ViewModel構築）と 非macOS 経路（`draw_theme_settings_card` → ANSIテキスト）へ `&ThemeSettings` として渡される。clone が必要な理由は既存 doc コメント（`state.rs:71-79`）が明言する通り「redraw 後半の `&mut self` 呼び出しをまたいで `App::theme_settings` の借用を保持しないため、早期に owned へ切り出す」。

実測上、両描画経路は `filtered` の**窓化スライスしか読まない**（native=`THEME_LIST_ROWS`8件、wgpu=`LIST_ROWS`件、いずれも highlighted 中心）。fuzzy match の `positions` は両経路とも破棄している（`palette.rs:783` `let Some((name, _positions))`、`model.rs:190` `.map(|(name,_)|`）。つまり574件全体を owned で持つ必要は無い。

### Considered Options
1. **新「薄い描画専用スナップショット型」を導入**（spec R-19 の字義通り）。`ThemeSettingsSnapshot { mode, section, badge, filter, windowed_themes: Vec<(String,bool)>, swatches, rows: [(label,value,restart);16], commit_error, count, contrast, ... }` を builder で組み、両描画関数の署名を `&ThemeSettings` → `&ThemeSettingsSnapshot` に変更。
2. **`filtered` と `available_font_families` を `Arc` 内包**し、`clone()` を参照カウント増加へ退化させる。両描画関数は `&ThemeSettings` のまま。
3. clone をやめ借用を延命（redraw構造の再編）。

### Decision
**Option 2 を採る。** `ThemeSettings` の 2 フィールドを
```
filtered: Arc<Vec<ThemeMatch>>,
available_font_families: Arc<Vec<String>>,
```
に変更。`recompute_filtered` は `self.filtered = Arc::new(filter_themes(...))` と全置換（既存も in-place 変異せず全置換なので意味論不変）。`render.rs:44-48` の `session.state.clone()` は文面そのままでコストが O(574) → O(1)＋小フィールドの微少複製（filter文字列、row内2 String、`RevertValues` の2 String＝計~5個の小alloc）に落ちる。両描画関数・全アクセサ・ViewModel builder は無改変。

### Rationale
- AC-25 の測定条件「`filtered: Vec<ThemeMatch>` を含む全体複製が発生しない／複製データ量が旧比で大幅減」を**最小差分（約10行＋アクセサの返り値調整）**で満たす。
- Option 1 は spec の字義に忠実だが、2 経路の窓化容量（8 vs `LIST_ROWS`）とオフセット算法（`overlay_scroll_window` vs `saturating_sub(list_rows/2)`）が異なるため、単一窓化スナップショットに両者を畳むにはどちらかの窓方針をもう一方へ結合する必要が生じ、`palette.rs`/`model.rs` の両builderと2署名・F2の再設計まで波及（~150行、テスト改修多数）。単一ユーザーのローカルアプリでオーバーレイは一時的にしか開かないため、この投資に見合う追加の測定利得は無い（AC-25はOption 2で充足）。
- **ADR-2 との相乗**: `Arc<Vec<ThemeMatch>>` の `Arc::as_ptr` 恒等が「filtered集合が変わったか」を O(1) で表す。F2の軽量キーの中核をタダで得る。これがOption 2を決定づける最大の理由。

### Rejected
- Option 1（新型）: 高churn＋二重窓化の結合負債、AC-25以上の利得なし。`state.rs:71-79` の doc コメントが予告する「follow-up の zero-copy 型」だが、借用切り出しの制約上どのみち owned 化が要り、`Arc` の cheap-copy が実務的な zero-copy 等価物。
- Option 3（借用延命）: redraw 後半の `&mut self`（`sidebar_draw_model` 等）と衝突し借用検査を通せない。既存 doc コメントが clone を選んだ理由そのもの。

### Consequences
- (+) 最小差分・署名不変・F2のキーを誘発。(+) `available_font_families` の毎フレーム複製も同時に消える（spec未言及の副次負債）。
- (−) spec R-19 の「新しい描画専用スナップショット型」という字面から逸脱。→ 意図的逸脱として本ADRに記録（AC-25は測定充足）。
- **Fitness**: `#[cfg(debug_assertions)]` の clone 計数は不要（Arc化で deep-copy 経路自体が消滅）。AC-25 は「`ThemeMatch` を deep-copy しないこと」を `Arc::strong_count` の増加＝共有で示すユニットテストに落とす（GPU不要）。

---

## ADR-2 — F2: ViewModelを組まない `view_fingerprint` を状態側に置く

### Context
`sync_theme_settings`（`sync.rs:61-83`）はハッシュ比較の**前**に `theme_settings_view_model(state)` を無条件構築（69行）し、変化があれば rebuild で**もう一度**構築（80行）する。ViewModel構築は 8件の窓化 `String` clone＋`noa_theme::resolve`＋`sample_swatches`（swatch配列）＋16行の表示値`String`＋footer`String` を毎回alloc する。R-20/NFR-7 は「冪等フレームで構築0回」「変化フレームでも見逃しゼロ」を要求（AC-26/AC-27）。

### Considered Options
1. **単調 `revision: u64` カウンタ**をミューテータ各所で bump し、キー=`(revision, rect, colors)`。
2. **`ThemeSettings::view_fingerprint(&mut impl Hasher)`** を状態側に実装。ViewModelに影響する素の場を直接ハッシュ（`Arc::as_ptr(filtered) as usize`／mode／section／filter／highlighted／selected_row／highlight_moved／commit_error／16行の(draft,touched)／編集バッファ長／favorites世代／attribute_filter）。sync は fingerprint＋rect＋colors をハッシュしてキーとし、変化時のみ ViewModel を**1回だけ**構築して rebuild へ渡す。
3. 現状維持＋二重構築の解消のみ（構築は変化時1回に、冪等時も1回は構築）。

### Decision
**Option 2。** `view_fingerprint` を `ThemeSettings` のメソッドとして**フィールド定義の隣**に置く。`sync_theme_settings` は:
```
let key = model.map(|(state, rect)| hash_u64(|h| { state.view_fingerprint(h); rect.hash_into(h); colors.hash_into(h); }));
if cache.theme_settings == key { return; }
cache.theme_settings = key;
let vm = theme_settings_view_model(state);   // 変化時のみ1回
imp::rebuild_theme_settings(window, model.map(|(_,r)|(vm, r)), colors);
```
冪等フレーム: ViewModel構築0回（fingerprintは O(16)、alloc無し）。変化フレーム: 構築1回（二重構築解消）。

### Rationale
- **見逃しゼロ（AC-27）を構造で保証**: ViewModelの各入力は (i) fingerprint に直接入る素の場、(ii) `filtered`（`Arc` ポインタで恒等）＋highlighted から純粋に導かれる値（窓化リスト・swatch・件数・コントラスト・R-33サンプル）、(iii) 16行 draft＋selected_row＋section から純粋に導かれる行表示、のいずれか。全入力が fingerprint に写像される。
- **false-negative の抑止をコロケーションで**: 「ViewModelに効く場」の知識を sync.rs ではなく状態の隣に集約。新フィールド追加者が fingerprint も隣で更新する（`Hash` 導出と同じ責務局在）。
- Option 1（revision）は bump 箇所が8+のミューテータに散り、追加ミューテータが bump を忘れると即 false-negative。「見逃しゼロ」義務下では脆い。
- `Arc::as_ptr` は ADR-1 が `filtered` を全置換する（in-place変異しない）ため恒等が集合変化と一致。favorites トグルは集合が同じでも★装飾が変わり得るので `favorites_epoch: u64` を別途 fingerprint に含める（下記ADR-5）。

### Rejected
- Option 1: 分散 bump の見逃しリスク。
- Option 3: 冪等フレームの構築が消えず NFR-7/AC-26 未達。

### Consequences
- (+) `#[derive(Hash)]` on ViewModel は**残す**（sync以外の用途・回帰の保険）が sync のホットパスからは外れる。(+) `f32` を含む draft のハッシュは `to_bits()` で手当（fingerprint内に閉じる）。
- **Fitness (AC-27の恒久ガード)**: 「全ミューテータを一巡し、ViewModel が変われば fingerprint も必ず変わる」ことをプロパティテスト化（各 `move_*`/`push_text`/`adjust`/favorites操作の前後で `view_fingerprint` と `theme_settings_view_model` の差分同時性を assert）。CI保持。

---

## ADR-3 — F3: fuzzyはprefix差分絞込み一本、タイマーdebounceは持たない

### Context
`push_text`/`backspace`（`state.rs:384-437`）は各キーで `recompute_filtered` → `filter_themes`（574件全走査、`fuzzy_match` 全適用）を無条件実行。spec R-21 は (a) debounce coalescing と (b) prefix拡張時の前回集合内差分絞込みを挙げる。NFR-8/AC-28/AC-29 の**測定対象は走査スコープ**（prefix拡張→前回`filtered`集合、prefix破壊→全574）。

### Considered Options
1. **prefix差分＋タイマーdebounce併用**: 前進入力は即時差分、prefix破壊（Backspace/置換）時の全再走査を `Debouncer<String>` で coalesce。
2. **prefix差分のみ（debounceなし）**: 前進入力＝直前 `filtered` 部分集合のみ再走査（即時）、prefix破壊＝全574再走査（即時）。
3. **全編集を一律debounce**（末尾値のみ発火）。

### Decision
**Option 2。** filter編集経路に差分絞込みを実装:
- 新filter文字列が旧filterの拡張（`new.starts_with(&old)` かつ長い）→ 走査対象は直前 `Arc<Vec<ThemeMatch>>` の name 群のみ（`fuzzy_match(new, name)` を prior set に適用）。
- それ以外（短縮・非prefix置換・空化）→ `filter_themes(new)` で全574再走査へフォールバック。
`Debouncer` は font-size 専用のまま（`state.rs:96`）、filterには導入しない。

### Rationale
- AC-28 の load-bearing 主張は走査スコープ（初回1回だけ574、以降は前回集合内）。差分絞込みが**タイマー無しで**これを満たす: `"3"` で574起点→縮小、`"30""302""3024"` は各々**縮小した直前集合のみ**を走査。
- AC-29 の主張（prefix破壊→全走査フォールバック）も差分判定の else 分岐で満たす。
- 574件の `fuzzy_match`（短文字列）は実測サブms級。前進入力は差分で更に小。**debounceを足すと1文字目・Backspaceに150ms遅延が乗り、リスト即時更新（＝視認性）を損なう**——spec の「即時1文字目 vs debounce」トレードオフで即時側を選ぶ。574規模でcoalescingの測定利得は無く、YAGNI（`void` 相当の判断）。
- debounce併用（Option 1）は「pending中に表示リストと適用集合が乖離」する状態を生み、preview（`should_preview`/`gpu.preview_theme`）のタイミングも二経路化する。574規模で不要な複雑性。

### Rejected
- Option 1: 574規模で複雑性に見合う利得なし・前進入力に不要遅延。
- Option 3: 1文字目遅延・spec の即時選好に反する。

### Consequences
- (−) **AC-28のテスト実装を調整**: 「debounceウィンドウ経過をシミュレート」ではなく「`fuzzy_match` 呼び出し回数（走査スコープ）」で assert する。走査計数の debug カウンタ（`#[cfg(test)]` の `AtomicUsize` か戻り値記録）で「初回=574、以降=直前len」を検証。この AC 実装形の変更は本アーキ決定に伴う正当な帰結。
- (+) `filter_themes` に加え差分版 `narrow_filtered(prior: &[ThemeMatch], filter)` を1関数追加するのみ。既存 `fuzzy_match` 単一マッチャー契約を保持。
- **Open**: 将来プロファイルで前進入力の縮小集合走査すら重い実測が出た場合に限り、**widen（フォールバック）経路のみ** `Debouncer<String>` を後付けする（前進の即時性は保つ）。現時点は不要と判断。

---

## ADR-4 — AC-13: pair保全は `commit_updates()` 内でpair文字列を生成する

### Context
`theme = light:X,dark:Y` 設定下でパネルから単一テーマをcommitすると、`commit_updates()`（`state.rs:806-812`）が `("theme", name)` を生成し、`apply_updates`（`writer.rs`）が `theme` の最終行を単一名で無条件置換→pair構文破壊。`apply_updates` は「keyの最終行を `key = value` に置換」する契約で、`value` にpair文字列を渡せば構文は保たれる（writer無改変で解決可能、実査で確認）。現状 `open_theme_settings`（`input_ops/theme_settings.rs:43-49`）は `self.config.theme`（解決済み単一名）だけを渡し、`self.config.theme_appearance: Option<ThemeAppearancePair>`（`config.rs:23`、pair生値保持）を渡していない。アクティブ側判定は `self.system_appearance: winit::window::Theme`（`app.rs:126`）＋既存 `effective_theme_name`（`config.rs:363`）と同型。

**決定的制約**: AC-49 は `commit_updates()` の**戻り値**が `("theme","light:C,dark:B")` を含むことを直接検証する。したがって変換は `commit_updates()` 内で起きねばならず、「Appレイヤの前処理wrapper」（spec L2 prose）では AC-49 を満たせない。**AC が spec prose に優先**する（spec metadata の逸脱許容に沿う）。

### Considered Options
1. **`commit_updates()` 内でpair文字列生成**。`ThemeSettings` に解決済みpair文脈を保持し、theme差分emit時に active 側=新名・他側=保持で `light:_,dark:_` を組む。
2. Appレイヤで `updates` を後変換（`commit()` を transform 受け取りに改造 or `commit_updates()`外出し）。
3. magi裁定のもう一方「提示＋明示confirm」モーダル。

### Decision
**Option 1。**
- `ThemeSettingsInit` に純粋文脈を追加:
  ```
  theme_pair: Option<ThemePairContext>,   // struct ThemePairContext { active_is_light: bool, light: String, dark: String }
  ```
  `open_theme_settings` が `self.config.theme_appearance` と `self.system_appearance` から解決して渡す（winit `Theme` の判定はApp側で行い、純粋モジュールは `bool` だけ受ける＝依存規則・testability保持）。
- `ThemeSettings` は `theme_pair: Option<ThemePairContext>` を保持。`commit_updates()` の theme 分岐:
  ```
  if let Some(name) = highlighted_theme_name(), name != snapshot.theme_name {
    match &self.theme_pair {
      Some(ctx) => {
        let (light, dark) = if ctx.active_is_light { (name, ctx.dark.as_str()) } else { (ctx.light.as_str(), name) };
        updates.push(("theme".into(), format!("light:{light},dark:{dark}")));
      }
      None => updates.push(("theme".into(), name.into())),   // 既存挙動（AC-51回帰防止）
    }
  }
  ```
- `apply_updates` は無改変。
- **in-memory 同期の追随**: commit成功後の `commit_theme_settings`（`input_ops/theme_settings.rs:395-397`）は現状 `self.config.theme = Some(name.1)` とするが、pairでは `name.1` が `"light:C,dark:B"` になり `self.config.theme`（bare名前提）を汚す。pair時は `self.config.theme_appearance` のアクティブ側を新名で更新し `self.config.theme` は触らない分岐を追加（R-34の精神＝再openの整合、AC-49/50の書込みとは別レイヤの正しさ）。

### Rationale
- AC-49 が戻り値検証である以上、配置は `commit_updates()` 一択（Option 2は不成立）。
- Option 1 は writer契約・NFR-5バイト精度・他キー挙動に一切触れず、渡す value をpair文字列に変えるだけ＝最小差分でNFR-9充足。
- 「アクティブ側のみ書換」採用理由（magiの2択のうち後者）: (a) 新モーダル追加はmagiの対話コスト最小化方針と不整合、(b) 常にEsc/Undoトースト（R-31）で可逆、(c) 既存 touched モデル（保全制約2）にpair判断が副作用として収まり新UI状態不要。

### Rejected
- Option 2: AC-49（戻り値検証）を満たせない／`commit()` 改造は R-12 失敗段の単純さを損なう。
- Option 3: 新モーダル＝スコープ方針違反。AC-C2（pair両側編集UI）はこの上の**拡張**であって代替でない。

### Consequences
- (+) AC-49/50/51 を1分岐で網羅。AC-51（`theme_pair=None` 時 `("theme","C")`）は else 分岐で回帰なし。
- (−) `ThemeSettingsInit` にフィールド追加（全14構築サイトへ機械的追記、下記「テスト波及」）。
- **Fitness**: AC-50 は tempdir 結合テストで `parse_theme_pair` 往復＋非アクティブ側バイト一致を検証（既存 writer テスト11件の隣）。

---

## ADR-5 — 新表示要素・favorites・toastは「統合モデル化」せず既存leaf共有パターンに従属させる

### Context（point 6 / 5 / R-31）
theme-settings は3描画同期点を持つ: (1) wgpu `theme_settings_overlay_text`（`palette.rs:718`）、(2) 共有値フォーマッタ `settings_row_display_value`/`RowDraft::display_value`（`rows.rs:145-203`、(1)(3)両方が呼ぶ）、(3) native `theme_settings_view_model`（`model.rs:178`）。保全制約5は「(2)を各経路がフォークしない」。R-26(件数)/R-27(コントラスト)/R-29(★)/R-33(サンプル複数行) は**テーマリスト側**の新表示だが、リスト描画は現状 (1)(3) が各々インラインで組み共有関数が無い。

### Decision
**描画モデルを統合しない。** (2) と同型の**純粋leaf関数**を `theme_settings` モジュールに追加し (1)(3) 双方が呼ぶ:
- `match_count_label(highlighted: usize, total: usize) -> String`（R-26、例 `"3 / 12"`）
- `contrast_label(fg: Rgb, bg: Rgb) -> (String, bool)`（R-27、`noa_render::theme::contrast_ratio` を呼ぶ／閾値4.5＝`DEFAULT_MINIMUM_CONTRAST`）
- `attribute_of(theme_def) -> Attribute {Light,Dark}`（R-30、noa-app 既存 `theme.rs:119 relative_luminance` を `pub(crate)` 昇格して再利用。**noa-render 無改変**）
- `sample_lines(theme_def) -> Vec<SampleLine>`（R-33、`sample_swatches` の色データを再利用し実fg/bg/選択色のテキスト行を生成。(1)(3)が同一関数を呼ぶ＝AC-48）
- ★印は ViewModel/overlay_text が `favorites.contains(name)` を引いて描くのみ（leaf化不要な単純分岐）

**favorites 永続（point 5 / R-29）:**
- **置き場**: `noa_config` に `theme_favorites_path()` ＋ `theme_favorites_path_in(dir)` を追加（`default_config_path`/`session_state_path` と同じ pub fn ペア規約）。**config dir 側** `~/.config/noa/theme-favorites`（`xdg_config_dir()` 経由）。session.json（data_dir、`window-save-state=never` で消える topology）ではなく config 隣＝UI選好として永続すべき性質のため。
- **形式**: 改行区切りの素テキスト（1行=テーマ名）。repo は serde 不使用（session.rs は手書きJSON）。favorites は `HashSet<String>` に過ぎず、パーサ不要の行区切りが最小。
- **機構**: `App` が `FavoritesStore { set: HashSet<String>, path: PathBuf }` を保持。初回 `open_theme_settings` で遅延ロード（best-effort、読めなければ空集合・起動非ブロック＝specエッジケース）。トグルで即 atomic 書込み（session.rs:363 の temp→rename 流用）。commit経路（`commit_updates`/`write`）には一切関与しない（AC-40/41）。
- **セッションへの供給**: `ThemeSettingsInit.favorites: Arc<HashSet<String>>` ＋ `favorites_epoch: u64`。トグルは**App側**で store 変異→新 `Arc` を session へ差し替え＋epoch bump（ADR-2 の fingerprint がこれを見て★変化を検出）。`filter_themes`/差分絞込みは favorites/attribute_filter を絞込条件として合流（commit_updates は不変）。

**toast 一般化（R-31）:**
- `WindowState.resize_overlay: Option<(String, Instant)>`（`state.rs:316`）を `Option<Toast>` へ:
  ```
  struct Toast { text: String, until: Instant, kind: ToastKind }
  enum ToastKind { Resize, Undo(Box<RevertValues>) }
  ```
  単一スロット維持（spec エッジケース: 新toastが旧を即置換）。`sync_toast`/`draw_toast_card` は `toast.text` を渡すのみ（描画は不変）。
- Undo トリガ: commit成功時に `commit` 直前の `RevertValues`（`state.rs:95` snapshot のクローン）を `App` へ返し `ToastKind::Undo` を積む。Undo実行は**既存 `write_config_updates` ＋ 既存 `apply_runtime_font_size`/`apply_live_*`** で snapshot 値を再commit（R-31「新機構を作らない」）。トリガキーは keybind（clickはmagiスコープ外）。

### Rationale
- 統合描画モデル（両renderer を1モデルに畳む）は Mega 変更でGPU gotchas/2署名に波及、単一ユーザーアプリで過剰。既存の「純粋leaf関数を2経路が共有」パターン（`settings_row_display_value` が実証済み）を新要素へ延長するのが保全制約5の字義かつ最小。
- favorites を config dir に置くのは「topology でなく選好」という性質判断（`window-save-state` に消されない）。pub fn ペア規約は AC-41 の tempdir 注入口をそのまま提供。
- toast 単一スロット＋enum tag は spec エッジケース（新が旧を置換）に直結、描画層を触らない。

### Rejected
- 統合描画モデル: Mega-ADR・過剰抽象。
- favorites を session.json 相乗り: `never` 設定で消える・topology と混在。
- serde 導入 or 独自JSON: 行区切りで足りる set に対し過剰。

### Consequences
- (+) 新要素は leaf 追加のみで3同期点コピペ地獄を回避（AC-47/48）。(+) favorites/attribute は絞込条件合流のみで commit 不可侵（AC-40）。
- (−) `ThemeSettingsInit` に `favorites`/`favorites_epoch`/`attribute_filter`/`carryover`（R-25）/`theme_pair`（ADR-4）が加わる → テスト波及（下記）。
- **Fitness**: AC-48 は「(1)(3)が同一 `sample_lines`/leaf を呼ぶ」を code-review＋（可能なら）関数参照の存在テストで担保。

---

## 追加コンポーネント決定（ADR未満・機械的）

- **R-25 carryover**: `ThemeSettingsInit.carryover: Option<ThemeSettingsCarryover { filter: String, highlighted: usize, selected_row: usize }>`。Tabは `close`(Esc)/`commit`(Enter)いずれにも分岐しない**第三遷移**として、現session から carryover を取り `open_theme_settings(逆mode, carryover)` で新session再構築。`gpu.preview_theme`・runtime適用値（font-size等）は一切触れない（AC-36）。`ThemeSettings::open` は carryover が `Some` なら filter/highlighted/selected_row 初期化をそれで上書き。
- **R-32 wheel**: `on_mouse_wheel`（`event_loop.rs:1124`）冒頭、`handle_sidebar_wheel` 早期returnの直後に `if self.active_overlay(window_id)==ActiveOverlay::ThemeSettings { return self.handle_theme_settings_wheel(window_id, lines); }` を追加（同じ bool 消費契約）。蓄積は `apply_overview_wheel` の `WHEEL_PAGE_THRESHOLD` 型を踏襲し highlighted/selected_row 移動へ（AC-45/46）。
- **R-22/23/24 メニュー・キーバインド**: `AppCommand::Preferences` のディスパッチ本体（`app/commands.rs:66`）を `open_theme_settings(Settings)` へ差替（menu id/accelerator不変＝AC-31）。新 `AppCommand::EditConfigFile`（旧 `open_config_file()` 温存、menu項目＋palette行、既定chord無し）。`OpenThemePicker` に menu項目＋既定 `cmd+shift+,`（`KeybindEngine::default()` specs 追加）。

## テスト波及の扱い（14 `ThemeSettingsInit` 構築サイト）
新Initフィールドは全て `Option`／owned-空デフォルト（`theme_pair:None, carryover:None, favorites:Arc::new(HashSet::new()), favorites_epoch:0, attribute_filter:None`）とし、14サイトは**機械的追記**で済ませる（`tests.rs` の `init()/settings_init()/transparent_init()` ヘルパが大半を吸収、直書きは `rows.rs`/`input_ops` テストのみ）。設計リスクではなく churn。既存51テスト（`theme_settings/tests.rs`34＋`input_ops/theme_settings.rs`6＋`writer.rs`11）は本設計のどの決定でも意味論を破らない（Arc化=複製意味論不変、fingerprint=新規、commit_updates pair分岐=`theme_pair:None` で従来経路）。

## 依存規則・保全制約チェック
- winit `Theme` 判定は App 層のみ（ADR-4）。純粋 `theme_settings` モジュールは `bool`/owned 値のみ受領＝GUI非依存・testable 維持。
- noa-render 無改変（ADR-1/2/5）。`contrast_ratio` は既存 pub、`relative_luminance` は noa-app 内既存関数の可視性昇格のみ（noa-render に触れない）。
- noa-config 追加は path helper 2関数のみ（既存規約に整合）。writer 無改変（ADR-4）。
- R-12 commit順序（config書込→クロムスワップ）不変（ADR-4は value 生成のみ変更、順序不介入）。
- GPU gotchas: 本設計は uniform layout/bind group visibility に一切触れない（描画データ経路のみ）。

## Rollout / ロールバック
性能是正3件（ADR-1→2→3）を先行（局所・低リスク・既存テストで回帰検知）、次に ADR-4（データ安全、tempロ結合テスト先行）、最後に ADR-5 系リッチ化（leaf追加＋favorites/toast）。各 ADR は独立に revert 可能（Arc化/fingerprint/pair分岐/leaf関数は互いに疎結合）。AC-C1/C2 は spec 通り安価判断時のみ・単独見送り可。

## 次工程
Risk Gate（omen: FM-EA系 pre-mortem／ripple: `ThemeSettingsInit` 14サイト・`commit_updates`・`sync.rs` の波及確認／echo: Tab第三遷移・toast置換のUX）。実装は titan/builder ループ。

---

## Amendments (Phase 5 Risk Gate — omen FMEA / ripple 反映, 2026-07-11)

### ADR-4 修正 (FM-01, RPN=448 — 実装前提・必須)
`open_theme_settings` の `current_theme` 導出を修正する。現行は `self.config.theme` のみを読むため、pair (`theme = light:X,dark:Y`) 有効時は常に `""` に落ち、Settings-only コミットで `commit_updates()` が幻のtheme差分を出力し pair を無言上書きする。修正: `effective_theme_name(config, system_appearance)` と同型のロジック(pairならactive側の名前、非pairなら `config.theme`)で導出し、pair構成下で `current_theme` が空になる経路を根絶する。これは ThemePairContext 追加の**前提**であり独立の追加作業ではない。commit_updates 側で空snapshotを特例で握りつぶす対処は禁止(初回テーマ設定フローを壊す)。二重防御として「Settingsモードでは `highlight_moved` が常にfalse」の不変条件テストを追加。

### ADR-2 補強 (FM-02): 全mutator×view_fingerprint同時性のproperty testを nice-to-have から必須ACへ格上げ。
### ADR-3 補足 (FM-07): wheel蓄積閾値は `apply_overview_wheel` の `WHEEL_PAGE_THRESHOLD` を値として再利用せず、専用の別定数を新設する。
### ADR-5 補足 (FM-08/FM-09): Undo再commitは「トースト表示以降に別のcommit/再openが発生していたら無効化」ガードを付ける。favoritesファイル書込失敗は無音にせずwarnログ+可能ならcommit_error相当の通知1行。
### 構築サイト数の訂正 (ripple条件1 / omen実測): 「14箇所」はオフバイワン。omen実測13(tests.rs 10 + input_ops 3)+ヘルパー経由の間接32箇所(ripple)。Builderは着手時に機械的に再カウントすること。
### carryover設計変更 (FM-04): `ThemeSettingsCarryover` に最初のopen時点の `RevertValues`(最低限theme_name)を含め、Tab往復で snapshot を再取得せず引き継ぐ。Escは常に最初のopen時点まで巻き戻す。
### 描画2経路戦略の一致 (FM-06): チップ行追加の行数戦略について「wgpu/nativeが対応する縮退戦略を明示的に選び、AC-48隣接のcode-review ACで確認」する。
