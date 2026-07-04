# Spec: Tab Overview (タブ俯瞰ビュー)

## Metadata
- slug: `tab-overview`
- title: 全タブをタイルで俯瞰する監視ダッシュボードビュー
- status: `locked`(2026-07-03 ユーザーサインオフ: SHAPE 追認 + ⚠A-⚠G 全推奨案で承認)
- owner: simota
- build-path: **orbit loop(engine: codex, gpt-5.5)** — 2026-07-03 ユーザー指定。ランナー: `.nexus/loops/tab-overview/`(L3 AC 25 件を verify.sh の完了契約へ写像済み。manual/visual AC は done.md の manual-verified 枠)

## L0 — Vision
- **問題**: noa はネイティブ macOS タブ(各タブ=別 NSWindow、1タブグループ)を持つが、タブが増えると各タブで走る処理(ビルド、テスト、ログ等)の状態を把握するにはタブを1つずつ巡回するしかない。
- **対象**: noa 利用者(複数タブで長時間処理を並行実行するターミナルヘビーユーザー)。
- **Job-to-be-done**: 全タブの画面内容をタイル状に一覧表示し、**出しっぱなしで複数タブの出力を同時にライブ監視**する。あわせて内容で判別して目的のタブへ素早く切り替えられる。
- **成功条件**: キー一発で俯瞰ビューを表示でき、表示中は全タブのタイルがライブ更新され、クリック/キー選択でそのタブにフォーカスできる。
- **主目的**: 監視ダッシュボード(ライブ更新は必須要件)。切替ナビゲーションは副次目的。
- **タイル粒度**: タブ単位(1タブ=1タイル)。splits のあるタブはフォーカス中ペインまたは分割レイアウトごと代表表示(詳細は SPECIFY で確定)。
- **パリティ例外(⚠E, 確定)**: Tab Overview は Ghostty に対応機能が存在しない、本リポジトリ初の「忠実クローン哲学からの意図的な逸脱」であり、Ghostty パリティ照合の対象外として L0 に記録する。

## FRAME — 再利用資産と制約 (Lens 調査 2026-07-03)

### 既存資産
- タブ=ネイティブ NSWindow(1タブグループ)。全タブは `App.windows: HashMap<WindowId, WindowState>` + `window_order: Vec<WindowId>` で列挙可能(`crates/noa-app/src/app.rs:190-205`)。
- `Renderer::draw_panes`(`renderer.rs:345`)+ `build_draw_plan`/`PaneRect`(`draw_plan.rs`)が splits 用に「1サーフェスに N ペインをシザー描画」を実現済み — タイル描画の最近傍の既存アナログ。
- `FrameSnapshot::from_terminal`(`snapshot.rs:43`)は自己完結スナップショット。タブ横断で収集可能(タブごとに短時間の Mutex ロック)。
- `KeybindEngine` + `AppCommand` + `CommandScope`(`commands.rs`, `app.rs:376+`, `app.rs:2116`)— トグルコマンド追加の確立済みパターン(`ToggleSplitZoom` と同型)。
- `split_tree.rs` の hit_test — クリック→タイル→タブ解決の前例。
- `macos_menu.rs` — ネイティブメニュー項目追加のパターン。

### 制約
- **縮小描画パスが存在しない**: グリフアトラスは固定セルサイズ。矩形縮小はクリップするだけで文字は縮まない。サムネイル化は (a) シェーダへのスケール導入、(b) オフスクリーン描画→縮小ブリット、(c) 小フォントサイズでの再ラスタライズ等の新規工事。
- オーバーレイ/モーダル UI の前例なし(コマンドパレット等 Inc 6 は未着手)— インタラクション設計は新規。
- 監視ダッシュボード用途のため**表示中のライブ更新が必須** → 全タブの Terminal ロック+描画コストが継続的に発生。タブ数スケールへの配慮が必要。
- ロードマップ(Inc 4-6)外の新規スコープ。位置づけはプロダクト判断。
- 新コマンドの `CommandScope` 分類が必要(タブグループ全体に効く → `NativeTabGroup` 寄り)。

## EXPAND — 候補 (確定 2026-07-03)

Riff(8案)+ Flux(リフレーム5点)から5方向に統合し、ユーザー選定を実施。

- **B. オフスクリーン縮小サムネイル方式(生存 → CHALLENGE へ)**: タブごとにフルサイズでオフスクリーンテクスチャへ描画→縮小ブリットでタイル配置。ピクセル忠実な Exposé 型ライブミラー。新規ブリットパイプラインが必要。
- **表示場所: 専用ウィンドウ/タブ(確定)**: 作業タブと並行表示できる監視用途前提。

### Considered but rejected (EXPAND でユーザーが落選)
- **A. 仮想スプリット・クリップ方式**(draw_panes 流用・等倍クリップ): 工数最小だがミニチュア俯瞰にならない — ピクセル忠実な俯瞰を優先し落選。
- **C. 小フォント第2アトラス方式**: シェーピング+アトラス二重化で工数最大。
- **D. バッジ+活動ランキング方式**(Flux badges-first): 「タイルで出しっぱなし監視」の JTBD から最も遠い。
- **E. アダプティブ複合方式**: コードパス3系統の維持コスト。

### CHALLENGE へ持ち越す論点(Flux の反論)
- グリフは ~8pt 未満では読めない — 小タイル時の忠実度は無駄コストにならないか(タイルサイズ/タブ数上限の設計で吸収するか)。
- ライブ監視=常時 N オフスクリーンパス/フレームの GPU コスト。dirty-row diffing(WP4, 5524a1a)でタイル再描画を活動タブに限定できるか。

## CHALLENGE — 裁定 (2026-07-03, Magi+Void+Ripple+Omen)

### Magi 裁定(設計上の4決定)
1. **忠実度**: フル解像度は過剰投資(~8pt 未満は判読不能)。縮小解像度レンダ(タイルの ~2倍サイズ)+**グリッド上限 ~9-12 タブ**、超過分はページング/活動アイコンに退化。(3-0, 信頼度78)*(上限は後続 ⚠B=9 で確定、退化は ⚠F=placeholder 行で確定。)*
2. **更新方式**: dirty-gate(WP4 dirty-row diffing 流用)+**レート上限 ~10-15Hz**。連続再描画も固定周期のみも不採用。(3-0, 82)
3. **テクスチャ戦略**: フルウィンドウ解像度ではなく **タイル ~2倍の縮小解像度**でオフスクリーン描画(VRAM をタブ数・メイン解像度から切り離す)。(収束, 75)*(後続 ⚠A で部分的に上書き: 単一共有フル解像度スクラッチ再利用によりタブ数からは独立させるが、メイン解像度からの独立は将来最適化=案(ii)へ保留。理由は L2 ⚠A 根拠参照。)*
4. **ウィンドウモデル**: 専用ウィンドウは**ネイティブタブグループの外**。フォーカス時は overview 専用キーマップ(移動/選択/切替/閉じる)のみ、PTY 入力パススルーなし。(3-0, 80)

### Void スコープ (v1)
- **KEEP**: click-to-focus / タイルラベル(タブ題名) / 起動キーバインド+ネイティブメニュー項目 / 1タブ・全タブ閉鎖の縮退ケース定義 / overview 自身の除外(構造上無料)。
- **CUT**: タイル間キーボードナビ / 活動バッジ / 活動順の並替・リサイズ / 更新周期等の config ノブ / タイル内スクロールバック / 複数 overview。
- **DEFER**: ウィンドウ位置記憶 / タイル内 split ペイン再現(v1 はタブ全体を1画像)。
- **FLAG**: **Ghostty に類例なし** — 本リポジトリ初の「忠実クローン哲学の意図的例外」。スペック L0 に明記必須。

### Ripple 影響分析 — リスク 6/10 (MEDIUM上縁)、Conditional-Go
- 好材料: `Renderer::draw`/`draw_panes` は既にサーフェスレス(`&wgpu::TextureView` 受け)— **render-to-texture はコア変更なしで可能**。新規は quad-blit パイプライン(~150-250 LOC、`CellPipeline` が雛形)。
- 必須緩和策: (1) サムネイルは**各タブの既存 Renderer を再利用**(新規 Renderer N 個はアトラス N 重複+dirty-cache コールドスタート)。(2) オフスクリーンテクスチャは各タブのサーフェス `TextureFormat` と**同一**(4e2fd7f の非 sRGB/ガンマ計算が構築時固定)。(3) overview WindowId を `window_order`/`tab_group_identifier` から除外。(4) overview フォーカス時の `CommandScope` 明示処理(端末系コマンドはクリーンに no-op)。(5) `noa-render/tests/pipeline.rs` に blit パスのヘッドレステスト追加。(6) occlusion-aware redraw 抑制を迂回せず尊重/拡張(バックグラウンドタブの GPU 節約と正面衝突するため)。
- 未解決プラミング: 「ペイン X が更新された」を overview が知る fan-out 経路が存在しない(`pane_render_cache` は noa-render 私有)— 新規プラミング必要。
- 最大影響ファイル: `crates/noa-app/src/app.rs`(WindowState/window_order/CommandScope/redraw 経路)、`noa-render/src/{renderer,pipeline}.rs`、`noa-render/tests/pipeline.rs`、`noa-app/src/macos_menu.rs`。

### Omen プレモーテム(16 モード、S≥9 なし、上位は GPU+並行性)
スペックに焼き込む上位5緩和策:
1. 新パイプラインの bind-group visibility / uniform layout を**ヘッドレス実 GPU テストでゲート**(非サンドボックス実行必須 — サンドボックスは GPU テストを skip)。
2. **入力遅延 NFR**: overview 表示中、フォーカスタブの keystroke-to-echo に +Xms 超の遅延を加えない。全 N terminal の無条件毎フレームロック禁止(dirty-gate+同時レンダ上限)。
3. **スレッド規律**: overview が他ウィンドウの Renderer/device 状態へ横断アクセスしない(各タブ自身の描画出力のテクスチャコピーで受け渡し)。
4. **上限+退化**: 最大タイル数と VRAM 予算を定義し、超過時は placeholder/低解像度へ。device-lost/surface-lost 回復パスを明記。
5. **ライフサイクル AC**: タブ閉鎖中フレーム / アプリ終了順 / Spaces・フルスクリーンの各 AC+テスト。パリティ例外は L0 に文書化。

### 設計テンション(SPECIFY へ持ち越し)
- Magi「~2倍タイル解像度で直接描画」と現行「セルサイズ固定(スケール uniform なし)」の間に実装選択が残る: (i) フル解像度オフスクリーン+GPU 縮小ブリット(単純・高コスト) vs (ii) 投影行列/uniform にスケール導入して縮小解像度へ直接描画(安価・新 uniform)。L2 で確定。

### ユーザー確定 (2026-07-03)
- **B′ で確定**: 緩和策付き縮小サムネイル(縮小解像度レンダ + dirty-gate + 10-15Hz 上限 + タブグループ外専用ウィンドウ + Ripple 緩和6件 + Omen 緩和5件を仕様要件化)。
- **スコープ復活なし**: Void の最小 v1 のまま(CUT/DEFER 項目は将来増分)。

## SHAPE — 提案 (暫定採用 → 2026-07-03 LOCK サインオフで追認済み)

### 提案する解決策 (B′)
- **ウィンドウライフサイクル**: キーバインド+ネイティブメニューで起動する専用 NSWindow(タブグループ外、`window_order`/`tab_group_identifier` から除外)。1タブ・全タブ閉鎖時の縮退表示を定義。overview 自身は俯瞰対象から構造的に除外。
- **タイルグリッド+上限/退化**: 1タブ=1タイル、グリッド上限 **⚠B=9(3×3)**。レイアウトは `cols=ceil(sqrt(N))`, `rows=ceil(N/cols)`(N≤9)で、最終行はタイル数が少なくても各タイルは同一サイズ(N=5,7,8 では末尾に空きセルが生じる — 「隙間なくタイリング」ではなくこの等サイズ格子不変条件が正)。超過分(>9)は **⚠F=ページングなし・title-only placeholder 行**へ退化(フルタイルは最近フォーカス上位9タブ)。VRAM は **⚠A=単一共有フル解像度スクラッチ(1枚)+ N 枚のタイルサイズテクスチャ**で、タブ数から独立(フル解像度は1枚のみ)。メイン解像度からは非独立(共有スクラッチはメインウィンドウ解像度に比例)。
- **更新パイプライン**: dirty-gate(WP4 流用)→ **⚠G=10Hz(min_interval=100ms、10-15Hz が許容チューニング帯、コンパイル時定数・config ノブなし)**スロットル → dirty タブのみ各タブの既存 Renderer 再利用で共有スクラッチへ描画 → quad-blit で当該タイルテクスチャへ縮小 → タイル合成。新規 Renderer をタブ数分作らない。
- **インタラクション**: click-to-focus。overview フォーカス中は専用キーマップ(移動/選択/切替/閉じる)のみ、PTY 入力パススルーなし。

### スコープ内 (v1)
click-to-focus / タイルラベル(タブ題名) / キーバインド+メニュー項目 / 縮退ケース / 自己除外 + **緩和策11件の要件化**(Ripple 6 + Omen 5、CHALLENGE 節参照)。

### スコープ外
- CUT: タイル間キーボードナビ、活動バッジ、活動順並替、config ノブ、タイル内スクロールバック、複数 overview。
- DEFER: ウィンドウ位置記憶、タイル内 split ペイン再現(v1 はタブ全体を1画像)。
- 非ゴール: PTY 入力パススルー、overview 経由の端末操作。

### 前提
WP4 dirty-row diffing が活動シグナル源として再利用可 / レンダラのサーフェスレス性(`&wgpu::TextureView` 受け)維持 / macOS のみ / **Ghostty 類例なし=忠実クローン哲学の意図的例外(L0 明記必須)**。

### SPECIFY へ持ち越した問い(すべて SPECIFY で解決済み → Open Questions ⚠ 参照)
1. レンダ戦略: (i) vs (ii) → **⚠A=案(i′)単一共有スクラッチ**で確定(LOCK 待ち)。
2. グリッド上限の数値(9-12 のどこか)→ **⚠B=9(3×3)**で確定(LOCK 待ち)。
3. 更新通知 fan-out 方式 → **⚠C=既存 `UserEvent::Redraw` 再利用**で確定(LOCK 待ち)。
4. 入力遅延 NFR の ms 値 → **⚠D=≤2ms(非ゲート smoke)+ 観測可能ゲートは hermetic unit**(F3 対応)で確定(LOCK 待ち)。
5. Ghostty パリティ例外の L0 文言 → **⚠E**で確定・L0 反映済み。

## SPECIFY — L1/L2/L3

- scope mode: **Full**(21 要件 = 10 functional + 11 non-functional、CHALLENGE 緩和策11件を要件化、新規 GPU パイプライン+新規ウィンドウモデル=高複雑度)。
- 優先度タグ: **[MH]** = must-have(v1 ブロッカー)、**[NH]** = nice-to-have(WP 内フォローで可)。
- 検証タグ: **[unit]** `cargo test -p <crate>` / **[headless]** `noa-render/tests/pipeline.rs`(実 GPU、アダプタ無ければ skip、**非サンドボックス実行必須**)/ **[inspection]** 型・構造の静的検査(private field / 非 pub 型によるコンパイル境界 = 違反はコンパイルエラー、または `cargo tree` / code-review) / **[visual]** 手動目視 / **[manual]** 手動操作確認(非ゲート smoke)。CLAUDE.md の pty(openpty=非サンドボックス)/ GPU(サンドボックスは headless を skip)制約が適用される。

### L1 — Requirements

#### Functional (REQ-OV-*)

- **REQ-OV-1** [MH]: 専用キーバインド+ネイティブメニュー項目で Tab Overview 専用ウィンドウをトグル表示/非表示する(`ToggleSplitZoom` と同型のトグルコマンド、`KeybindEngine`/`AppCommand`/`macos_menu.rs` の確立パターンを踏襲)。
- **REQ-OV-2** [MH](Ripple 緩和3): Overview は専用 NSWindow としてネイティブタブグループの外に生成し、`window_order`/`tab_group_identifier`(app.rs:196,201,220)から除外する。タブ巡回・タブ数カウント・タブグループ tabbing にオーバービュー自身が現れてはならない。
- **REQ-OV-3** [MH]: 監視対象タブを 1タブ=1タイルでグリッド配置する。グリッド上限は **⚠B=9(3×3)**。レイアウト不変条件(N≤9)は `cols=ceil(sqrt(N))`, `rows=ceil(N/cols)` の**等サイズ格子** — 全タイルは同一サイズで、最終行はタイル数が cols 未満になり得る(N=5,7,8 では末尾に空きセルが生じる)。「隙間なくタイリング」は等サイズ制約下では N=1,2,3,4,6,9 でのみ厳密成立するため要件化しない。要件は「全タイル同一サイズ・行優先・末尾行のみ不足可・タイルは互いに重ならない」とする。
- **REQ-OV-4** [MH]: 各タイルは対象タブの画面内容の縮小ライブミラーである。Overview 表示中、対象タブの出力が変化したタイルはライブ更新される(監視ダッシュボードの核心要件)。
- **REQ-OV-5** [MH]: 各タイルにそのタブの題名(tab title)をラベル表示する(内容+題名でタブを判別可能にする)。
- **REQ-OV-6** [MH]: タイルのクリックでそのタブにフォーカスを移す(click-to-focus)。`split_tree.rs` の hit_test 前例に倣いクリック点→タイル→WindowId を解決する。
- **REQ-OV-7** [MH](Ripple 緩和4 / Magi 決定4): 2つのスコープを別物として扱う。(a) 起動コマンド `ToggleTabOverview` は **`CommandScope::NativeTabGroup`** で発火(どのタブからでもタブグループ全体に効く、`ToggleSplitZoom` と同型)。(b) Overview がフォーカスされている間のディスパッチは新規 **`CommandScope::Overview`** で解決し、Overview 専用キーマップ(dismiss)のみ処理して**PTY 入力パススルーを行わない**。端末系 `AppCommand` は `CommandScope::Overview` でクリーンに no-op になる。(a) と (b) は異なる関心事であり矛盾しない。
- **REQ-OV-8** [MH]: Overview 自身は俯瞰対象から構造的に除外される(グリッドに供給されるタブ集合に Overview の WindowId は含まれない)。REQ-OV-2 の除外から自動的に従う。
- **REQ-OV-9** [MH](Omen 緩和5): 縮退ケースを定義する — 監視対象タブが 0/1 個、全タブ閉鎖、および対象タブが Overview 表示中に閉じられた場合(タイル除去+再レイアウト、パニックなし)、アプリ終了順序、最後のタブ閉鎖。Spaces / フルスクリーン下での表示も含む。
- **REQ-OV-10** [MH](Omen 緩和4 / ⚠F): グリッド上限超過(>9 タブ)時の退化を定義する — **v1 はページングなし**。フルタイルは**最近フォーカス上位9タブ**に割り当て、それ以外は **title-only の placeholder タイル行**(ライブミラーなし・題名のみ)へ退化する(⚠F、LOCK サインオフ待ち)。VRAM 予算超過時は注入された budget-flag に従い低解像度/placeholder へ退化する(REQ-NF-8 と対)。ページング=タイルナビゲーションは CUT 済みのため v1 では採らない。

#### Non-Functional (REQ-NF-*)

- **REQ-NF-1** [MH](Ripple 緩和1): 各タイルの描画は**対象タブが既に保持する `Renderer` を再利用**する。タブ数分の新規 `Renderer` を生成しない(新規 Renderer N 個はアトラス N 重複+dirty-cache コールドスタートを招く)。
- **REQ-NF-2** [MH](Ripple 緩和2): タイル用オフスクリーンテクスチャは対象タブのサーフェスの `TextureFormat` と**同一**フォーマットで生成する(4e2fd7f の非 sRGB / ガンマ計算が `CellPipeline` 構築時に固定されるため。renderer.rs:120,214 `target_format_is_srgb`)。
- **REQ-NF-3** [MH](⚠A): 縮小合成は**新規 quad-blit パイプライン**で行う。各タブは既存 `Renderer` を無改変で**単一共有のフル解像度スクラッチテクスチャ**へ描画し(タブ k を描画 → その小さなタイルテクスチャへ縮小 blit → タブ k+1 で同じスクラッチを再利用)、線形フィルタでサンプルしタイル矩形へ縮小描画する(`CellPipeline` を雛形、~150-250 LOC)。フル解像度テクスチャは**同時に1枚のみ存在**(タブ数から独立)。`Uniforms`(instance.rs:54)/`populate_pane_uniform` の std140 レイアウトには手を入れない。
- **REQ-NF-4** [MH](Magi 決定2 / Omen 緩和2 / ⚠G): タイル更新は **dirty-gate(WP4 dirty-row diffing 流用)→ レートスロットル**を通す。スロットルの既定は **10Hz(min_interval=100ms)**、10-15Hz を許容チューニング帯とする**コンパイル時定数**(config ノブなし、⚠G)。全 N 端末の無条件毎フレーム Mutex ロック+再描画を禁止する。clean なタブは再描画しない。1 Overview フレームでロックする端末数は dirty かつ間隔経過したタブに限定する(上限 K)。
- **REQ-NF-5** [MH](⚠D / Omen 緩和2): Overview 表示中、フォーカスタブの入力応答性を低下させない。**観測可能なゲート**は「Overview フレームが (a) clean タブをロックしない、(b) 同時オフスクリーン描画数が 1フレーム上限 K を超えない」を注入クロック付きの hermetic unit で検証する(AC-NF-05)。keystroke-to-echo の追加遅延 **≤ 2ms** は実 GPU 実測の**非ゲート manual smoke ノート**へ降格する(手動では厳密観測不能なため)。
- **REQ-NF-6** [MH](Omen 緩和3): スレッド規律 — Overview は他ウィンドウの `Renderer`/`Device` 状態へ横断アクセスしない。各タブ自身の描画出力のテクスチャコピー経由でのみ受け渡す。
- **REQ-NF-7** [MH](Ripple 緩和6): 既存の occlusion-aware redraw 抑制(app.rs:2092-2110 `TargetedRedrawDecision`)を迂回せず尊重/拡張する(バックグラウンドタブの GPU 節約と正面衝突するため)。
- **REQ-NF-8** [MH](Omen 緩和4 / ⚠A): VRAM 予算モデルを **1 枚の共有フル解像度スクラッチ(メインウィンドウ解像度に比例)+ N 枚のタイルサイズテクスチャ**と定義する(タブ数に対しフル解像度は増えない;タイルテクスチャは小サイズ)。最大タイル数(⚠B=9)と VRAM 予算を定義し、注入された budget-flag 超過時は placeholder/低解像度へ退化する(REQ-OV-10 と対)。`device-lost`/`surface-lost` の回復パス(共有スクラッチ+タイルテクスチャ群の再生成)を明記し、クラッシュしない。
- **REQ-NF-9** [MH](Ripple 緩和5 / Omen 緩和1): `noa-render/tests/pipeline.rs` に blit パスのヘッドレス実 GPU テストを追加する。新パイプラインの bind-group `visibility` と uniform layout(CLAUDE.md GPU gotcha: 頂点段サンプルは `VERTEX_FRAGMENT`、std140 整列)を検証し、1フレーム描画が wgpu 検証エラーなしで完了する。**非サンドボックス実行必須**(サンドボックスは GPU テストを skip)。
- **REQ-NF-10** [MH](⚠C): 「タブ X が更新された」の fan-out は**既存の `UserEvent::Redraw(WindowId, PaneId)` 経路を再利用**する(io_thread.rs:128 → app.rs:1027)。Overview は同じ redraw シグナルからタブ別 dirty-set を維持する。新規イベント variant も renderer 私有状態の露出も行わない。クレート依存規則を尊重し `noa-grid` 以下は GUI 非依存のまま(winit は `noa-app` のみ)。
- **REQ-NF-11** [MH]: `cargo test --workspace` と `cargo clippy --workspace` が Overview 追加後も green を維持する。

> Must 比率メモ: 21 要件すべてが [MH]。本機能は監視ダッシュボードの安全性・並行性・GPU 正しさに直結する緩和策要件が中心であり(11件は CHALLENGE でユーザー確定済み)、v1 での [NH] 降格は縮退 UX を招くため意図的に全 [MH] とする(splits/rendering-improvements の locked 先例と同傾向)。

### L2 — Detail

#### noa-app(ウィンドウモデル / CommandScope / イベント配線)

- **ウィンドウモデル**: Overview は独立 `WindowState`(または軽量な専用 `OverviewWindow`)として生成し、`window_order`/`tab_group_identifier` から除外する(REQ-OV-2)。生成時に `with_tabbing_identifier`(app.rs:600)を付与しない。タブ巡回(app.rs:948-959)と CloseTab 経路は Overview WindowId を透過する。
- **CommandScope(2つを区別、REQ-OV-7)**: 既存の `CommandScope { FocusedTab, NativeTabGroup, App }`(app.rs:2116)に新規 **`CommandScope::Overview`** を追加する。
  - **起動スコープ**: `AppCommand::ToggleTabOverview`(仮)は `CommandScope::NativeTabGroup` で発火し `KeybindEngine`/`menu_id`/`macos_menu.rs` に配線(`ToggleSplitZoom` と同型、どのタブからでもトグル)。
  - **Overview フォーカス時スコープ**: Overview がフォーカス中のディスパッチは `CommandScope::Overview` で解決。端末系コマンドは `resolve_command_target` 段でクリーンに no-op、dismiss(トグルキー/Esc)のみ処理。PTY 入力・IME・選択・コピー等はパススルーしない。
- **fan-out(⚠C)**: Overview はタブ別 dirty-set(`HashMap<WindowId, bool>` 相当)を保持。app が `UserEvent::Redraw(window_id, pane_id)`(app.rs:1027)を処理する際、Overview が開いていれば当該タブのタイルを dirty マークし、スロットル済み Overview 再描画を要求する。新規イベント variant は追加せず既存 Redraw 配線を拡張。`noa-grid` は GUI 非依存のまま(REQ-NF-10)。
- **click-to-focus**: Overview の `WindowEvent` マウス座標 → 純関数 `hit_test_overview_grid(&[(WindowId, TileRect)], point) -> Option<WindowId>`(`split_tree.rs` hit_test 前例)→ 当該タブへ `focus`/`order_front`。
- **縮退/ライフサイクル(REQ-OV-9)**: 対象タブ閉鎖時は dirty-set と grid からエントリ除去+再レイアウト(stale WindowId no-op はタブの前例に倣う)。0/1 タブ、全タブ閉鎖、アプリ終了順、Spaces・フルスクリーンの各挙動を定義。
- **純関数シーム(GPU/Window 不要でユニットテスト可)**:
  - `compute_overview_grid(tab_count, bounds, cap=9) -> OverviewLayout { tiles: Vec<TileRect>, placeholders: Vec<TileRect>, overflow: bool }` — `cols=ceil(sqrt(n))`, `rows=ceil(n/cols)`(n=min(tab_count,cap))の等サイズ格子。全 `TileRect` は同一サイズ、行優先、末尾行のみ不足可、重なりなし(REQ-OV-3)。tab_count>cap のとき上位9=`tiles`、残りは title-only `placeholders` 行、`overflow=true`(REQ-OV-10、⚠F)。
  - `overview_command_scope(AppCommand) -> CommandScope`(REQ-OV-7、端末系→`CommandScope::Overview` で no-op)。
  - `should_render_tile(dirty, last_render_at, now, min_interval) -> bool` — **注入クロック**で dirty-gate+スロットルを判定。clean→false、dirty かつ `now - last_render_at < min_interval`→false、以上→true(REQ-NF-4/5、⚠G min_interval=100ms)。
  - `overview_redraw_decision(...)`(REQ-NF-7、`TargetedRedrawDecision` 拡張)。

#### noa-render(オフスクリーン + blit パイプライン)

- **オフスクリーン描画(⚠A 決定 = 案(i′)= 単一共有スクラッチ)**: 各タブの既存 `Renderer` を**無改変で**、**全タブで共有する 1 枚のフル解像度スクラッチ `TextureView`** へ順次描画する(`draw`/`draw_panes` は既に `&wgpu::TextureView` 受け、surface-less。renderer.rs:326,345)。描画直後にそのタブの小さなタイルテクスチャへ縮小 blit し、次の dirty タブで同じスクラッチを再利用する。スクラッチは対象タブのサーフェス `TextureFormat` と同一(REQ-NF-2)。→ フル解像度テクスチャは同時に 1 枚のみ(タブ数から独立)。
- **blit パイプライン(新規)**: 新規 `BlitPipeline`(`CellPipeline` を雛形)が共有スクラッチを線形フィルタ `Sampler` でサンプルし、そのタブのタイルテクスチャ(=グリッドタイル矩形サイズ)へ縮小 quad 描画する。bind-group は color texture + sampler(fragment サンプルのみ → `visibility = FRAGMENT`。頂点段 `textureDimensions` を導入する場合のみ `VERTEX_FRAGMENT`。CLAUDE.md gotcha)。`Uniforms`/std140 は不変更(REQ-NF-3)。
- **⚠A 根拠(Magi 決定3 の明示的 partial override)**: Magi 決定3 は「タイル ~2倍の縮小解像度で直接描画し VRAM をタブ数**とメイン解像度**から切り離す」を裁定したが、これは `Uniforms`(instance.rs:54-66)にスケール項が無く std140 ロック済み(CLAUDE.md GPU gotcha)、かつ `cell_size` がグリフアトラスのサンプルを駆動するため、スケール uniform 導入は再ラスタコストを下げられず silent な std140 drift リスクだけを生む。よって Magi #3 を**部分的に上書き**する: 案(i′)は各タブ Renderer を無改変再利用しコア変更ゼロ、追加は blit のみ(Ripple「render-to-texture はコア変更なしで可能」に一致)。**単一共有スクラッチ**により VRAM は「フル解像度 1 枚 + タイルサイズ N 枚」で**タブ数から独立**(Magi #3 のこの目的は達成)。ただし共有スクラッチはメインウィンドウ解像度に比例するため**メイン解像度からは非独立**(ここが Magi #3 との差分 = override 範囲)。Apple Silicon 統合メモリでフル解像度 1 枚は許容(splits 先例)。**フォールバック=案(ii)**(スケール uniform 導入で縮小解像度へ直接描画、メイン解像度からも独立)は VRAM/perf が許容不能と判明した場合の将来最適化として保留。
- **更新パイプライン**: dirty-gate(WP4 流用)→ 10-15Hz スロットル → dirty タブのみオフスクリーン再描画(既存 Renderer)→ blit でタイル合成(REQ-NF-4)。1フレームあたり同時オフスクリーン描画上限を設け入力遅延を担保(REQ-NF-5)。
- **スレッド規律(REQ-NF-6)**: Overview は他ウィンドウの `Renderer`/`Device` フィールドへ横断アクセスせず、テクスチャコピー(オフスクリーン出力)経由でのみ受け渡す。macOS は present をウィンドウ所有スレッドで行う制約を尊重。
- **回復(REQ-NF-8)**: `device-lost`/`surface-lost` 時はオフスクリーンテクスチャ群を再生成。VRAM 予算超過は低解像度/placeholder へ退化(REQ-OV-10)。
- **テスト(REQ-NF-9)**: `noa-render/tests/pipeline.rs` に blit パスの headless ケースを追加 — 実アダプタで 1フレーム縮小 blit が `pop_error_scope() == None` で完了、bind-group visibility / uniform layout を検証。非サンドボックス実行必須。

#### noa-font / noa-pty / noa-grid

- **noa-font / noa-pty**: 無改変。アトラス(共有シングルトン)と Pty をそのまま再利用。
- **noa-grid**: GUI 非依存を維持(REQ-NF-10)。dirty シグナルは既存 WP4 dirty-row の再利用に留め、winit/wgpu を導入しない。

### L3 — Acceptance Criteria

- **AC-OV-01** (REQ-OV-1) [MH] [unit]+[manual] — Given Overview 非表示状態、When `ToggleTabOverview` を dispatch、Then overview-visible 状態フラグが true に反転し、再 dispatch で false へ戻る(unit、Window 不要な状態遷移);Given 複数タブが開いている、When 起動キーバインドまたはメニュー項目を発火、Then Overview 専用ウィンドウが実際に表示/非表示される(manual)。
- **AC-OV-02** (REQ-OV-2) [MH] [unit] — Given Overview WindowId を含むウィンドウ集合、When `window_order` 巡回・タブ数カウント・タブグループ列挙を評価、Then いずれにも Overview WindowId が現れず、tabbing identifier も付与されない。
- **AC-OV-03** (REQ-OV-3) [MH] [unit] — Given `compute_overview_grid(tab_count, bounds, cap=9)`、When 1 ≤ tab_count ≤ 9、Then `cols=ceil(sqrt(tab_count))`, `rows=ceil(tab_count/cols)` の等サイズ格子で tab_count 個の `TileRect` を返し、全タイルが同一サイズ・行優先・末尾行のみ不足可・互いに重ならない(N=5,7,8 は末尾に空きセルを許容、厳密 gapless は要求しない);When tab_count > 9、Then 9 タイル+`overflow=true`+placeholder 群を返す。
- **AC-OV-04** (REQ-OV-4) [MH] [headless]+[visual] — Given 実アダプタで 1 タイルを描画しそのピクセルハッシュを取得、When 対象タブの内容を変化させ再描画、Then タイルのピクセルハッシュが変化する(内容不変時はハッシュ不変)(headless、非サンドボックスレーン);Given ノイジーな出力を出すタブと静止タブを Overview 表示中、When 複数フレーム観察、Then 活動タブのタイルがライブ更新され静止タイルは変化しない(visual)。
- **AC-OV-05** (REQ-OV-5) [MH] [unit]+[visual] — Given 既知の題名を持つタブ群、When Overview が構築される、Then 各タイルのラベルが対応タブの題名にマップされる(unit)/ 目視で題名が表示される(visual)。
- **AC-OV-06** (REQ-OV-6) [MH] [unit]+[manual] — Given `compute_overview_grid` が返した `TileRect` 群、When タイル k の内部点で `hit_test_overview_grid(&[(WindowId, TileRect)], point)` を評価、Then タブ k の `WindowId` を返し、タイル境界外・placeholder 行・空きセルの点では `None` を返す(unit、逆写像を直接検証);Given Overview 表示中で複数タイルあり、When あるタイルをクリック、Then 対応タブがフォーカスされ前面に出る(manual)。
- **AC-OV-07** (REQ-OV-7) [MH] [unit]+[manual] — Given Overview フォーカス中、When 各端末系 `AppCommand` を dispatch、Then `overview_command_scope` により no-op へ解決される(unit);When 文字入力、Then どの pty にも到達しない(manual)。
- **AC-OV-08** (REQ-OV-8) [MH] [unit] — Given グリッドへ供給するタブ集合の列挙、When 評価、Then Overview 自身の WindowId は含まれない。
- **AC-OV-09a** (REQ-OV-9) [MH] [unit] — Given 監視対象タブが 0 個・1 個・全タブ閉鎖済みの各集合、When `compute_overview_grid` + 供給タブ集合を評価、Then 空 or 単一タイルのレイアウトを返すか no-op になり、パニックしない(0 タブ=空グリッド、全閉鎖=空)。
- **AC-OV-09b** (REQ-OV-9) [MH] [unit] — Given Overview 表示中に対象タブが閉じられる、When 当該 WindowId を dirty-set と grid から除去、Then タイル除去+再レイアウトが完了し、stale WindowId 参照は no-op になる(タブ前例に倣う)。
- **AC-OV-09c** (REQ-OV-9) [MH] [headless]+[inspection] — Given 実アダプタ上で Overview の共有スクラッチ+タイルテクスチャ+blit パイプラインを生成した状態、When 規定の順序(Overview リソース → タブ Renderer → Device)で drop、Then wgpu 検証エラーもパニックもなく teardown が完了する(headless、非サンドボックスレーン);加えてアプリ終了経路の drop 順序が規定順序に従うことを code-review で確認(inspection)。hermetic unit オラクルは存在しない(Device 実体が必要)ため [unit] を掲げない。
- **AC-OV-09d** (REQ-OV-9) [MH] [manual] — Given Spaces・フルスクリーン、When Overview を表示、Then 各環境で正しく表示される(目視確認)。
- **AC-OV-10** (REQ-OV-10) [MH] [unit] — Given tab_count > cap=9、When `compute_overview_grid` を評価、Then フルタイルは上位9タブ、残りは title-only `placeholders` へ退化し `overflow=true`(v1 は paging なし、⚠F);Given 注入された `budget_exceeded` フラグ、When 退化判定を評価、Then 低解像度/placeholder へ退化する(フラグ注入を明示)。
- **AC-NF-01** (REQ-NF-1) [MH] [unit]+[inspection] — Given Overview のオフスクリーン描画経路に注入した `Renderer::new` 呼び出しカウンタ、When N タブのタイルを描画、Then カウンタ増分が 0(新規 `Renderer` を生成せず既存を再利用)(unit);加えて Overview 経路の型シームに `Renderer::new` が現れないことを code-review で確認(inspection)。
- **AC-NF-02** (REQ-NF-2) [MH] [unit] — Given あるタブのサーフェス `TextureFormat`、When そのタブのオフスクリーンテクスチャを生成、Then フォーマットが対象サーフェスと一致する。
- **AC-NF-03** (REQ-NF-3) [MH] [headless] — Given blit パイプラインとオフスクリーンテクスチャ、When 1タイルへ縮小 blit を実行、Then wgpu 検証エラーなしで 1フレーム完了する(`pop_error_scope() == None`)。**非サンドボックス実行**。
- **AC-NF-04** (REQ-NF-4) [MH] [unit] — Given `should_render_tile(dirty, last_render_at, now, min_interval)`(既定 min_interval=100ms=10Hz、許容帯 1/15..1/10 s、コンパイル時定数、⚠G)、When clean タブ、Then false;When dirty かつ間隔未満、Then false(遅延);When dirty かつ間隔以上、Then true。全 N 端末の無条件毎フレームロックが発生しないこと。
- **AC-NF-05** (REQ-NF-5) [MH] [unit]+[manual] — Given `should_render_tile(dirty, last_render_at, now, min_interval)` に**注入クロック**、When clean タブ、Then false;When dirty かつ `now - last_render_at < min_interval`、Then false;When dirty かつ間隔以上、Then true(unit)。Given N タブ(一部 clean)を含む 1 Overview フレームのゲート評価、When フレームを構築、Then ロックされる端末数が「dirty かつ間隔経過」のタブに一致し、clean タブはロックされず、同時オフスクリーン描画数が上限 K を超えない(lock-count assertion、unit)。keystroke-to-echo の追加遅延 ≤ 2ms は**非ゲート manual smoke**(実 GPU・非サンドボックス実測、参考値)。
- **AC-NF-06** (REQ-NF-6) [MH] [inspection] — Given 他ウィンドウの `Renderer`/`Device` 状態が private field / 非 pub 型としてカプセル化されている、When Overview 合成経路が他ウィンドウのテクスチャコピー**のみ**を入力に取る設計、Then 横断アクセスを書こうとするとコンパイルエラーになる(オラクル=コンパイラ)。可視性は private field / 非 pub 型で強制し、テクスチャコピー以外の受け渡しを型で不可能にする。
- **AC-NF-07** (REQ-NF-7) [MH] [unit] — Given occluded/background タブ、When `overview_redraw_decision`(`TargetedRedrawDecision` 拡張)を評価、Then 既存の occlusion 抑制が尊重され、Overview 経路が抑制を迂回しない。
- **AC-NF-08a** (REQ-NF-8) [MH] [unit] — Given 注入された `budget_exceeded` フラグ、When VRAM 退化判定を評価、Then 低解像度/placeholder へ退化する(VRAM モデル=フル解像度 1 枚+タイル N 枚、⚠A)。
- **AC-NF-08b** (REQ-NF-8) [MH] [unit]+[headless]+[manual] — Given **注入された `device-lost`/`surface-lost` イベント**、When 再生成要否の判定関数を評価、Then `regen_required=true` へ遷移しパニックしない(判定は GPU 不要の unit);Given 実アダプタ、When 回復ルーチンを実行、Then 共有スクラッチ+タイルテクスチャ群が `device.create_texture` で再生成され状態が有効へ戻る(headless、非サンドボックスレーン);実 GPU での手動誘発でクラッシュしないことを確認(非ゲート manual smoke)。
- **AC-NF-09** (REQ-NF-9) [MH] [headless] — Given `noa-render/tests/pipeline.rs` の blit ケース、When 実アダプタで描画、Then bind-group visibility と uniform layout が検証され 1フレーム検証エラーなしで完了する。サンドボックスでは skip、**非サンドボックス実行でゲート**。
- **AC-NF-10** (REQ-NF-10) [MH] [unit] — Given `UserEvent::Redraw(window_id, pane_id)` を Overview 表示中に処理、When 当該タブが更新、Then そのタブのタイルが dirty-set にマークされる;When `cargo tree` を検査、Then `noa-grid`/`noa-vt` が `wgpu`/`winit` に依存しない。
- **AC-NF-11** (REQ-NF-11) [MH] [unit]+[headless] — Given Overview の全テスト集合、When `cargo test --workspace` と `cargo clippy --workspace` を実行、Then documented な pty サンドボックス制約以外の `#[ignore]` なしで green、headless パイプラインテストも green。

### Traceability — REQ ↔ AC(双方向)

| REQ | AC | 優先度 |
|---|---|---|
| REQ-OV-1 | AC-OV-01 | MH |
| REQ-OV-2 | AC-OV-02 | MH |
| REQ-OV-3 | AC-OV-03 | MH |
| REQ-OV-4 | AC-OV-04 | MH |
| REQ-OV-5 | AC-OV-05 | MH |
| REQ-OV-6 | AC-OV-06 | MH |
| REQ-OV-7 | AC-OV-07 | MH |
| REQ-OV-8 | AC-OV-08 | MH |
| REQ-OV-9 | AC-OV-09a, AC-OV-09b, AC-OV-09c, AC-OV-09d | MH |
| REQ-OV-10 | AC-OV-10 (+AC-NF-08a) | MH |
| REQ-NF-1 | AC-NF-01 | MH |
| REQ-NF-2 | AC-NF-02 | MH |
| REQ-NF-3 | AC-NF-03 | MH |
| REQ-NF-4 | AC-NF-04 | MH |
| REQ-NF-5 | AC-NF-05 | MH |
| REQ-NF-6 | AC-NF-06 | MH |
| REQ-NF-7 | AC-NF-07 | MH |
| REQ-NF-8 | AC-NF-08a, AC-NF-08b | MH |
| REQ-NF-9 | AC-NF-09 | MH |
| REQ-NF-10 | AC-NF-10 | MH |
| REQ-NF-11 | AC-NF-11 | MH |

**Coverage: 21/21 要件が ≥1 AC にトレース = 100%**(Full-scope 最小 ≥95%)。AC 総数 **25**(全 [MH];逆方向: 全 AC が発生元 REQ を明示)。内訳: AC-OV-01..08(8)+ AC-OV-09a/b/c/d(4)+ AC-OV-10(1)+ AC-NF-01..07(7)+ AC-NF-08a/b(2)+ AC-NF-09/10/11(3)。緩和策11件マッピング: Ripple 緩和1-6 → REQ-NF-1/REQ-NF-2/REQ-OV-2/REQ-OV-7/REQ-NF-9/REQ-NF-7;Omen 緩和1-5 → REQ-NF-9/REQ-NF-5(+REQ-NF-4)/REQ-NF-6/REQ-NF-8(+REQ-OV-10)/REQ-OV-9(+L0 パリティ例外)。

### Quality Gate Run 1 (FAIL) — 修正記録

Judge + Attest が FAIL を返した Run 1 の全指摘への対応(各1行)。

- **F1 (BLOCKER)**: レンダ戦略の二重権威を解消。⚠A を**単一共有フル解像度スクラッチ再利用**(案 i′)へ変更し Magi 決定3 の**明示的部分上書き**として記録(タブ数からは独立、メイン解像度からは非独立)。SHAPE / REQ-NF-3 / REQ-NF-8(VRAM モデル)/ L2 noa-render を一貫更新。
- **F2 (MAJOR)**: `compute_overview_grid` の行列分配を `cols=ceil(sqrt(N))`,`rows=ceil(N/cols)` の等サイズ格子と規定し、REQ-OV-3 / AC-OV-03 を「全タイル同一サイズ・末尾行のみ不足可・重なりなし」不変条件へ改稿(blanket gapless を撤回)。
- **F3 (MAJOR)**: REQ-NF-5 / AC-NF-05 の ≤2ms [manual] を非ゲート smoke へ降格し、観測可能な hermetic unit(ゲートロジック+lock-count)へ置換。
- **F4 (MINOR)**: オーバーフローを ⚠F=ページングなし・title-only placeholder 行に単一化(上位9タブにフルタイル)。REQ-OV-10 / AC-OV-10 反映。
- **F5 (MINOR)**: 起動 `ToggleTabOverview`=`CommandScope::NativeTabGroup` と Overview フォーカス時=新規 `CommandScope::Overview` の2スコープを明確に分離(REQ-OV-7 / L2)。
- **F6 (MINOR)**: スロットルを ⚠G=10Hz(min_interval=100ms)コンパイル時定数・10-15Hz 許容帯・config ノブなしに確定(REQ-NF-4 / AC-NF-04 / AC-NF-05)。
- **F7 (MINOR)**: AC-NF-06 をコンパイル境界 [inspection](private/非 pub でコンパイルエラー化)、AC-NF-01 を `Renderer::new` カウンタ [unit]+[inspection] へ retag([inspection] タグを凡例に定義)。
- **F8 (MINOR)**: SHAPE の「~9-12」を ⚠B=9 へ更新、CHALLENGE Magi 裁定 #1/#3 に ⚠B/⚠F/⚠A 上書き注記を追加。
- **Attest AC-NF-05**: hermetic [unit](注入クロック+lock-count、clean タブ非ロック)へ全面書き換え、2ms は manual smoke。
- **Attest AC-OV-01**: `ToggleTabOverview` で overview-visible 状態が反転する [unit] を追加(ウィンドウ自体は [manual] 維持)。
- **Attest AC-OV-04**: 実アダプタでのピクセルハッシュ [headless] オラクル(内容変化前後でハッシュ差)を追加。
- **Attest AC-OV-09**: 09a(0/1/全閉鎖)/09b(表示中閉鎖再レイアウト)/09c(終了 teardown 順)/09d(Spaces・フルスクリーン manual)へ分割。
- **Attest AC-NF-08**: 08a(VRAM 退化、注入 budget-flag)/08b(device-lost 回復、注入 lost-event で regen)へ分割。
- **Attest AC-OV-10**: 注入 budget-flag を明示。
- **Bookkeeping**: AC 総数 21→**25**、トレーサビリティ表・coverage 行を更新(100% 維持)、⚠F/⚠G を Open Questions に追加。

### Quality Gate Run 2 (PASS) — 2026-07-03

- **Judge**: 全5次元 PASS(BLOCKER/MAJOR 残 0)。F1-F8 の解消を本文で検証、新規コード引用も grounding 確認済み(`Uniforms` にスケール項なし = ⚠A 根拠を実コードで裏付け)。
- **Attest**: OK 22 / RISK 3 / FAIL 0(CONDITIONAL)。残 RISK 3 件は本 Run 後に Nexus が直接解消:
  - AC-OV-06: `hit_test_overview_grid` 逆写像の [unit] を直接付与([manual] のみ→ [unit]+[manual])。
  - AC-OV-09c: hermetic unit オラクル不在(Device 実体要)のため [unit]→[headless]+[inspection] へ retag。
  - AC-NF-08b: 判定 [unit] / 再生成 [headless] / 手動誘発 [非ゲート manual smoke] に3分割。
- **検証レーン集計(Attest)**: サンドボックス cargo-test レーン ~18 AC / 非サンドボックス実 GPU レーン 4-6 AC(NF-03/NF-09/OV-04/OV-09c/NF-08b/NF-11 headless 半)/ 純 manual 2(OV-09d、OV-06 の manual 半)— **OV-09d と各 manual 半は自動ループの DONE ゲート対象外とし、LOCK 時に「manual-verified サインオフ枠」として明記**(ループ停止を防ぐ)。

**ゲート判定: PASS(lock precondition 充足)** — testable L3 AC(Attest)+ 5次元品質ゲート(Judge)の両方を満たす。

## Open Questions / Deferred Decisions — ⚠A–⚠G(2026-07-03 LOCK で全推奨案承認済み)

- **⚠A レンダ戦略 = 案(i′)単一共有フル解像度スクラッチ + GPU 縮小 blit(暫定、Magi 決定3 の部分的上書き)**: `Uniforms`(instance.rs:54)にスケール項が無く std140 ロック済み、かつ `cell_size` がアトラスサンプルを駆動するためスケール uniform 単独では再ラストを下げられず silent drift リスクのみ残る。案(i′)は各タブ Renderer を無改変再利用しコア変更ゼロ、追加は blit のみ。**全タブで 1 枚のフル解像度スクラッチを順次再利用**(タブ k 描画→タイルテクスチャへ blit→タブ k+1 で再利用)し、VRAM =「フル解像度 1 枚 + タイルサイズ N 枚」。**Magi 決定3 の部分的上書き**: 「VRAM をタブ数から独立」は達成(フル解像度 1 枚のみ)、ただし「メイン解像度からも独立」は未達(共有スクラッチはメイン解像度に比例)= override 範囲。Apple Silicon 統合メモリでフル解像度 1 枚は許容。フォールバック=案(ii)スケール uniform 直接描画(メイン解像度からも独立)。*LOCK 承認済み (2026-07-03)。*
- **⚠B グリッド上限 = 9(3×3)(暫定)**: 3×3 は厳密に隙間なく敷き詰まる唯一の上限候補で、VRAM に保守的、退化境界(⚠F)が最も単純。超過は REQ-OV-10 で退化。*LOCK 承認済み (2026-07-03)。*
- **⚠C 更新 fan-out = 既存 `UserEvent::Redraw(WindowId, PaneId)` 経路の再利用(暫定)**: io_thread.rs:128 → app.rs:1027 の既存シグナルから Overview がタブ別 dirty-set を維持。新規 event variant も renderer 私有状態露出も不要。`noa-grid` 以下は GUI 非依存のまま(winit は `noa-app` のみ)でクレート依存規則を尊重。*LOCK 承認済み (2026-07-03)。*
- **⚠D 入力遅延 NFR = ≤ 2ms(暫定)**: フォーカスタブの keystroke-to-echo への追加遅延上限。人間に非知覚で測定可能。dirty-gate + 10-15Hz スロットル + 同時オフスクリーン描画上限で担保、実 GPU 非サンドボックスで実測。*LOCK 承認済み (2026-07-03)。*
- **⚠E Ghostty パリティ例外文言(確定・L0 反映済み)**: 「Tab Overview は Ghostty に対応機能が存在しない、本リポジトリ初の『忠実クローン哲学からの意図的な逸脱』であり、Ghostty パリティ照合の対象外として L0 に記録する。」*LOCK 承認済み (2026-07-03)。*
- **⚠F オーバーフロー方針(>9 タブ)= ページングなし・title-only placeholder 行(暫定)**: v1 はページング(タイルナビゲーション=CUT 済み)を採らない。フルタイルは最近フォーカス上位9タブ、残りは題名のみの placeholder 行(ライブミラーなし)へ退化。REQ-OV-10/AC-OV-10 に反映。*LOCK 承認済み (2026-07-03)。*
- **⚠G 更新スロットル = 10Hz(min_interval=100ms)、10-15Hz 許容帯、コンパイル時定数(暫定)**: 既定 10Hz を採用、10-15Hz を許容チューニング帯とし config ノブは設けない(コンパイル時定数)。REQ-NF-4/AC-NF-04/AC-NF-05 に反映。*LOCK 承認済み (2026-07-03)。*
- **(継続)splits タブの代表表示**: フォーカス中ペインのみ vs 分割レイアウト縮小再現 — DEFER(v1 はタブ全体を1画像。Void DEFER に整合)。

## v2 — Mockup Parity

### Metadata (v2)
- trigger: ユーザー提示のターゲット UI モックアップ画像(2026-07-04)。本節の要件はエージェントが画像を直接見た結果ではなく、依頼者が言語化した観察記述を正としてスペック化したもの。
- scope mode: **Standard 追記**(REQ-OV-11..17 の 7 functional + REQ-NF-12..13 の 2 non-functional = 9 要件、AC 16 件)。
- 継続方針: v1 の不変条件(等サイズ・行優先・非重複、REQ-OV-3)・スコープ境界(Void CUT/DEFER、L0 パリティ例外)は維持し上書きしない。本節は v1 requirement の**補完**(REQ-OV-5 の未達解消含む)であり、v1 REQ/AC 番号は再利用しない。
- グラウンディング: 本節の現状記述はすべて `feat/tab-overview-v2` ワークツリーのコード調査(2026-07-04)に基づく。主要参照: `crates/noa-app/src/tab_overview.rs`(純関数層)、`crates/noa-app/src/app.rs`(ウィンドウ/描画/コマンド配線)、`crates/noa-app/src/command_palette.rs`(フィルタ機構の参考実装)。

### L0 — Vision delta (v2)
- v1 は「出しっぱなし監視ダッシュボード」を主目的としクリック操作のみを KEEP 、タイル間キーボードナビ・活動バッジ・並替等を CUT した(Void スコープ、90-102行)。ユーザー提示のモックアップは、その CUT 済みキーボードナビの一部(矢印移動・Cmd+1-9直接切替)と、v1 で未実装のタイトルバー表示・クローズボタン・検索フィルタ・ヒントバーを新たに要求している。これは v1 の Void CUT を撤回するものではなく、**ユーザーが明示的にスコープを復活させた** v2 追加要求として扱う。
- 現状ギャップの棚卸し(コード裏付け、下記 REQ で個別に要件化):
  - タイトルバーは placeholder 行にのみ描画され(`render_overview_placeholder_labels`, `app.rs:1563-1623`、呼び出しは `app.rs:1573` の `overview_tile_labels`)、ライブタイル(`render_due_overview_tiles`, `app.rs:1468-1526`)には題名描画経路が存在しない — v1 REQ-OV-5/AC-OV-05 は**実質未達**(REQ-OV-12 で解消)。
  - キーボードは `overview_command_scope`(`app.rs:3561-3587`)がほぼ全 `AppCommand`(`SelectTab`/`CloseTab`/`NextTab`/`PrevTab`/`CloseWindow` 含む)を `CommandScope::Overview` に分類し、`handle_app_command`(`app.rs:551-552`)がこのスコープを一律 no-op にする。矢印/Enter/Esc 専用のキー処理関数(`handle_search_prompt_key`/`handle_command_palette_key` に相当するもの)は存在しない — 事実上「再トグルによる dismiss」のみが機能する(REQ-OV-14/15 で新設)。
  - `compute_overview_grid`(`tab_overview.rs:65-107`)と `rect_at`(`tab_overview.rs:224-233`)はガター・マージンを一切加算せず、タイルは境界いっぱいに敷き詰められる(隙間ゼロ)(REQ-OV-11 で拡張)。
  - close(✕)ボタンに相当する UI・ヒットテスト対象は存在しない(REQ-OV-13 で新設)。
  - タブ検索フィルタは存在しないが、`command_palette.rs:139` `command_palette_filter` / `command_palette.rs:151` `is_subsequence_ci` という非連続部分列(subsequence)マッチの手書き実装が既にある。ただしモックアップの「Search tabs」はタイトルの**部分一致(substring)**であり意味論が異なるため、パターンは参考にしつつ新規関数として要件化する(REQ-OV-16)。
  - `redraw_overview`(`app.rs:1710-1745`)は `present_overview_frame`(`app.rs:1727`)をフレーム内容の変化有無に関わらず毎回無条件に呼び、`backlog_remains`(`app.rs:1737-1741`、dirty だがスロットル未到来のタイルが残っている)なら即座に次フレームを要求する(`app.rs:1742-1744`)。スロットル待ち期間中(既定 100ms)、何も変化していないのに毎フレーム合成+present を繰り返す — これが依頼にある既知バグの実体(REQ-NF-12 で解消)。
  - `render_due_overview_tiles`(`app.rs:1493-1496`)は対象タブの `surface.terminal` を直接ロックし `FrameSnapshot::from_terminal` を呼ぶ。これは `Screen::take_visible_rows_with_damage`(`noa-render/src/snapshot.rs:114`、"take" = 消費型)を経由するため、通常タブ自身の redraw 経路が消費すべき damage を Overview が横取りする可能性がある(REQ-NF-13 で解消)。

### Non-Goals (v2)
- 背景ブラー壁紙(タイル外周の装飾背景)。
- ページング / タイルナビゲーション(v1 Void CUT を継続。REQ-OV-10 の title-only placeholder 行退化方式は維持)。
- タイルのドラッグ並べ替え。
- アニメーション遷移(開閉・選択移動・フィルタ再レイアウトはいずれも瞬時反映、トランジションなし)。

### SPECIFY — v2 L1/L2/L3

検証タグ・優先度タグの凡例は本ファイル 118 行の SPECIFY 節を継承する([MH]/[NH]、[unit]/[headless]/[inspection]/[visual]/[manual])。

#### L1 — Requirements (v2 追加分)

##### Functional (REQ-OV-11..17)

- **REQ-OV-11** [MH]: `compute_overview_grid` をガター(タイル間の一定間隔)と外周マージン(グリッド全体と Overview ウィンドウ境界の間隔)を受け取るよう拡張する(例: `gutter: u32, margin: u32` 引数、または `OverviewLayoutParams` 構造体)。v1 の不変条件(REQ-OV-3: 全タイル同一サイズ・行優先・末尾行のみ不足可・重なりなし)は維持し、`gutter=0, margin=0` は v1 の敷き詰めレイアウトと**ビット単位で一致**する(回帰安全性)。
- **REQ-OV-12** [MH]: 全タイル(ライブミラー・placeholder 行の両方)にタイトルバーを表示する。タイトルバーはタイル上部の帯で、中央にそのタブの題名(tab title)を表示する。placeholder 側は既存の `overview_tile_labels`(`tab_overview.rs:180-192`)をそのまま再利用できるが、ライブタイル側にはこの関数を呼ぶ描画経路自体が存在しないため新設が必要(`render_due_overview_tiles` への合成呼び出し追加)。本要件は **v1 REQ-OV-5 / AC-OV-05 の実質未達を、表示様式(タイトルバー)込みで充足する**。
- **REQ-OV-13** [MH]: タイトルバー右端に ✕ クローズボタンを表示する。クリックすると当該タブを閉じ、タイル除去+グリッド再レイアウトを行う(REQ-OV-9 の「Overview 表示中に対象タブが閉じられた場合」の縮退経路をそのまま再利用する — 閉鎖トリガーの発生源が pty 外部からユーザーの ✕ クリックに変わるだけで、以後の除去/再レイアウト/stale 参照 no-op の契約は同一)。✕ のヒットテストはタイルのフォーカス用ヒットテスト(`hit_test_overview_grid`, `tab_overview.rs:113-118`)と別領域として解決し、タイル本体クリックとは異なる標的(close target)を返す。
- **REQ-OV-14** [MH]: 選択モデルを導入する。Overview のライブグリッド(行優先、REQ-OV-3/11)上のちょうど1タイルが「選択中」状態を持つ。選択状態は青のフォーカスリング(グロー)で可視化する。Overview を開いた時点の初期選択は、フォーカス中タブがライブタイル集合に含まれればそのタイル、含まれなければ先頭(index 0)とする。
- **REQ-OV-15** [MH]: キーボードナビゲーション。以下は本ファイル 94・158 行が示す「Overview 専用キーマップ(移動/選択/切替/閉じる)」の具体化であり、新規の Overview 専用キー処理経路(`handle_search_prompt_key`/`handle_command_palette_key` と同型に `KeyboardInput` 先取りルーティングへ挿入)を要する。現行の `overview_command_scope`(`app.rs:3561-3587`)による一律 no-op 化はこの新経路と共存させ、no-op 対象は端末系 `AppCommand` のみに限定する。
  - (a) 矢印キー(↑↓←→)は行優先グリッド上で選択タイルを移動する。グリッド端でクランプしラップしない。placeholder 行のタイルも選択対象に含む(下記(b)参照)。
  - (b) Return は選択中タイルのタブへフォーカスを確定する。選択がライブタイルなら既存 `focus_tab_from_overview`(`app.rs:1765-1775`)を再利用する。選択が **placeholder 行でも選択可能とし、Return で当該タブへフォーカスする**(placeholder はタイトルのみで内容ミラーがないが、対象タブ自体は実在するため到達可能でなければならない)。
  - (c) Cmd+1..9 は現在ライブなタイル位置 N(1始まり、行優先の N 番目)へ直接切替する。既存 `AppCommand::SelectTab(n)`(`commands.rs:26`)とキーバインド(`commands.rs:363-371`)はそのまま流用するが、Overview フォーカス中はこれを **REQ-OV-15 専用経路で直接解決**し、`overview_command_scope` による no-op 対象からは除外する(v1 では `SelectTab` は `CommandScope::Overview` で no-op、`app.rs:3582`)。
  - (d) Esc は Overview を閉じる(`hide_tab_overview`, `app.rs:1331` 相当の非表示処理)。どのタブへもフォーカスを移さない。
- **REQ-OV-16** [MH]: 上部中央の「Search tabs」検索フィールドで、タブ題名の**部分一致(substring)・大文字小文字無視**によりライブタイル集合を絞り込み、絞り込み結果でグリッドを即座に再レイアウトする(REQ-OV-11 拡張後の `compute_overview_grid` に絞り込み後の source id 集合のみを渡す)。含有判定は新規の大文字小文字無視 substring 判定関数として実装する — `command_palette.rs:151` の `is_subsequence_ci` は**非連続部分列(subsequence)**判定であり意味論が異なるため、そのまま再利用はしない(関数を呼ぶのではなく、大文字小文字畳み込みや走査といった実装パターンの参考に留める)。フィルタ文字列変更のたびに選択インデックスは先頭(0)へリセットする(command-palette.md R-7 の `selected = 0` リセットパターンに整合)。REQ-OV-7 の「Overview フォーカス中の文字入力は PTY へ流さない」を具体化し、印字可能文字の入力先を本検索フィールドと明記する。
- **REQ-OV-17** [MH]: 下部中央にヒントバーを静的テキストで表示する。内容は「⌘1-N to switch・↑↓←→ to navigate・Return to open・esc to close」とし、**N は現在のライブタイル数(min(タブ数, 9))**に置き換える(モックアップの「⌘1-6」はユーザー提示時点のタブ数=6 に対応する具体例であり、実システムの Cmd+1..9 対応範囲(`OVERVIEW_GRID_CAP=9`, `tab_overview.rs:11`)と整合させるため固定文言「1-6」ではなく動的な N とする — ambiguous+reversible な設計判断としてここに明記)。ヒントバー自体に入力応答はない(表示のみ)。

##### Non-Functional (REQ-NF-12..13)

- **REQ-NF-12** [MH]: Overview の合成/present は、タイル更新が due でないフレームで全速実行してはならない。現行 `redraw_overview`(`app.rs:1710-1745`)は `present_overview_frame`(`app.rs:1727`)をフレーム内容の変化有無に関わらず無条件に呼び、かつ dirty だがスロットル未到来のタイルが残っている(`backlog_remains`, `app.rs:1737-1741`)限り即座に次フレームを要求する(`app.rs:1742-1744`)。この結果、スロットル待ち期間(既定 100ms、⚠G)中は何も変わらない合成+present を毎フレーム(ディスプレイのリフレッシュレートで)繰り返す既知バグがある。修正契約: 当該フレームで実際に描画されたタイルが 0 件(`due_window_ids` が空)かつレイアウト/選択/検索フィルタの変更もない場合、`present_overview_frame` を呼ばない。dirty backlog が残る場合でも、次回描画要求はスロットル期限(`min_interval` 経過時点)に合わせてスケジュールし、変化のない毎フレーム即時再要求を行わない。
- **REQ-NF-13** [MH](REQ-NF-6 の強化): Overview 描画経路は通常タブの `Terminal` Mutex をロックしてはならない。現行 `render_due_overview_tiles`(`app.rs:1493-1496`)は `surface.terminal.lock()` を直接取得し `FrameSnapshot::from_terminal` を呼ぶが、これは `Screen::take_visible_rows_with_damage`(`noa-render/src/snapshot.rs:114`、消費型の "take")を経由するため、通常タブ自身の redraw 経路が消費すべき damage を横取りしうる。修正契約: io スレッドが各タブについて read-only スナップショット(damage を**消費しない**取得経路)を publish し、Overview の描画経路はそれのみを読む。Overview はいかなるタブの `Terminal` Mutex も直接ロックしない。

> Must 比率メモ: v2 追加 9 要件すべて [MH]。モックアップはユーザーが明示提示したターゲット UI であり、UI 忠実度(タイトルバー/クローズボタン/フォーカスリング/検索/ヒントバー)とキーボードナビの正しさが本節の中核目的であるため、[NH] 降格は行わない(v1 の全 [MH] 先例に整合)。

#### L2 — Detail (v2)

##### noa-app / tab_overview.rs(純粋層の拡張)

- `compute_overview_grid` のシグネチャを `compute_overview_grid(tab_count: usize, bounds: TileRect, cap: usize, gutter: u32, margin: u32) -> OverviewLayout`(または引数をまとめた `OverviewLayoutParams`)へ拡張する。`rect_at`(`tab_overview.rs:224-233`)にガター・マージンのオフセット計算を追加し、`tile_w`/`tile_h` の算出(`tab_overview.rs:89-90`)から `(cols-1)*gutter + 2*margin` 等を差し引く形にする(REQ-OV-11)。既存呼び出し箇所は `gutter=0, margin=0` を渡すことで v1 挙動を保つ回帰テストを追加する。
- 新規純関数 `move_overview_selection(selected: usize, cols: usize, tile_count: usize, direction: Direction) -> usize`(REQ-OV-15a、行優先グリッド上の端クランプ・非ラップ移動、Window/GPU 不要)。`Direction` は `FocusDirection`(既存、`commands.rs` 参照)を再利用するか専用 enum を新設する。
- 新規純関数 `overview_tab_filter(query: &str, titles: &[(Id, String)]) -> Vec<Id>`(REQ-OV-16、大文字小文字無視 substring 判定、`command_palette.rs` の case-folding パターンを参考にしつつ独自実装)。
- `overview_tile_labels`(`tab_overview.rs:180-192`)はライブタイル・placeholder 行の両方から呼ばれるよう L2 の呼び出し側(app.rs)を変更するだけで、関数自体の変更は不要(REQ-OV-12)。
- 新規純関数 `overview_close_hit_test(&[(Id, TileRect)], point) -> Option<Id>`(REQ-OV-13、タイトルバー右端の close-button 矩形専用。`hit_test_overview_grid`(`tab_overview.rs:113-118`)とは別の入力矩形集合を取るため既存関数は変更せず並置する)。

##### noa-app / app.rs(ウィンドウイベント・コマンド配線・描画)

- **Overview 専用キー処理(REQ-OV-15)**: `handle_search_prompt_key`/`handle_command_palette_key` と同型の `handle_overview_key(event_loop, window_id, event)` を新設し、`overview_window_event`(`app.rs:1777-1815` 付近)の `KeyboardInput` 分岐から委譲する。矢印→`move_overview_selection` で選択更新+redraw、Return→選択タイルの Id を解決し `focus_tab_from_overview`(ライブ)または同等のフォーカス経路(placeholder)を呼ぶ、Esc→`hide_tab_overview()` 相当を呼ぶ、印字可能文字→検索クエリへ追記+`overview_tab_filter` 再計算+選択を 0 へリセット+redraw。
- **Cmd+1..9 の直接解決(REQ-OV-15c)**: `overview_command_scope`(`app.rs:3561-3587`)の `AppCommand::SelectTab(_) => CommandScope::Overview` アームで一律 no-op にする現行分岐の前段に、`handle_overview_key` 相当の経路で `SelectTab(n)` を横取りしライブタイル N 番目へ直接フォーカスする分岐を追加する(既存 `overview_command_scope` の no-op 分類自体は他の端末系コマンドに対しては維持)。
- **✕ クローズボタン(REQ-OV-13)**: `overview_window_event` の `MouseInput`(`app.rs:1797` 付近、`focus_overview_tile_at_last_cursor` と同じイベントを消費)に、まず `overview_close_hit_test` で close-button 領域を判定し、ヒットすれば当該タブに対して REQ-OV-9 の閉鎖経路(`close_tab` 相当)を呼び、ミスであれば従来の `focus_overview_tile_at_last_cursor` にフォールバックする。
- **タイトルバー描画(REQ-OV-12)**: `render_due_overview_tiles`(`app.rs:1468-1526`)のループ内で、`overview_tile_labels` から得たタブ題名をタイル上部帯にオーバーレイ合成する(`render_overview_placeholder_labels` が使う `label_renderer` を共有し、ライブタイルのシンプルなラベル行合成に転用する。新規 GPU パイプラインは不要)。
- **フォーカスリング(REQ-OV-14)**: `present_overview_frame`(`app.rs:1628-1692`)のタイル合成ループ(`app.rs:1683-1688`)で、選択中タイルの矩形に対してのみ青いリング(グロー)のオーバーレイ描画を追加する。
- **present の due ゲート(REQ-NF-12)**: `redraw_overview`(`app.rs:1710-1745`)を、`due_window_ids.is_empty()` かつレイアウト/選択/検索の変更なしの場合に `present_overview_frame` 呼び出し(`app.rs:1727`)をスキップするよう改める。`backlog_remains` による次回redraw要求(`app.rs:1742-1744`)は、即時要求ではなく最も早いタイルのスロットル期限に合わせた要求へ変更する。
- **damage 非消費スナップショット(REQ-NF-13)**: `render_due_overview_tiles`(`app.rs:1468-1526`)の `surface.terminal.lock()` + `FrameSnapshot::from_terminal`(`app.rs:1493-1496`)を、io スレッドが publish する read-only スナップショット読み取りへ置き換える。`Terminal` 側に damage 消費経路(`take_visible_rows_with_damage`, `noa-render/src/snapshot.rs:114`)とは別の非消費 peek 経路を追加する必要がある(具体設計は Builder が noa-grid/noa-render の実装時に確定)。

##### noa-render / noa-grid

- タイトルバー・フォーカスリング・ヒントバーの描画は既存セル/オーバーレイ合成パイプラインの範囲内で完結させ(search_prompt/command palette と同じくグリッド整列モーダル描画パターンの延長)、新規 bind-group/uniform レイアウトは追加しない(既存 `noa-render/tests/pipeline.rs` の検証対象を増やさない)。
- damage 非消費 peek 経路(REQ-NF-13)は `noa-grid`(`Screen`)側に追加する。`noa-grid` は GUI 非依存のまま(winit/wgpu を導入しない、v1 REQ-NF-10 の制約を継続)。

#### L3 — Acceptance Criteria (v2 追加分)

- **AC-OV-11** (REQ-OV-11) [MH] [unit] — Given 拡張後の `compute_overview_grid(tab_count, bounds, cap, gutter, margin)`、When `gutter=0, margin=0`、Then 返る `OverviewLayout` は v1 の `compute_overview_grid(tab_count, bounds, cap)` と全タイル矩形が一致する(回帰);When `gutter>0` または `margin>0`、Then 全ライブタイルは同一サイズを保ち、隣接タイル間の間隔が `gutter` に一致し、グリッド全体と `bounds` 境界の間隔が `margin` に一致し、行優先・非重複の不変条件(REQ-OV-3)を維持する。
- **AC-OV-12** (REQ-OV-12) [MH] [unit]+[visual] — Given ライブタイルと既知のタブ題名、When `render_due_overview_tiles` 相当の合成呼び出しを評価、Then タイトルバー行の合成入力に当該タブの題名が渡される(unit);Given 実 GUI で複数ライブタブを Overview 表示、Then 各ライブタイル上部に自身の題名を表示するタイトルバーが目視できる(visual)— v1 REQ-OV-5/AC-OV-05 の未達(placeholder のみ描画)を解消したことを確認する。
- **AC-OV-13** (REQ-OV-13) [MH] [unit]+[manual] — Given タイル矩形とタイトルバー右端の close-button 領域、When `overview_close_hit_test` をタイル本体内の点/close-button 領域内の点で評価、Then タイル本体内は `None`(通常のフォーカス用 `hit_test_overview_grid` が処理)、close-button 領域内は当該タブの close target を返す(unit);Given 実 GUI、When ✕ をクリック、Then 当該タブが閉じてタイルが除去され再レイアウトされる(manual、REQ-OV-9 の縮退経路を再利用)。
- **AC-OV-14** (REQ-OV-14) [MH] [unit] — Given フォーカス中タブがライブタイル集合に含まれる状態で Overview を開く、When 初期選択を評価、Then 選択インデックスはフォーカス中タブのタイルを指す;Given フォーカス中タブがライブタイル集合に含まれない(例: フォーカス中タブが overflow 側)、When 初期選択を評価、Then 選択インデックスは 0;いずれの場合も選択中タイルはちょうど1つ。
- **AC-OV-15a** (REQ-OV-15a) [MH] [unit] — Given `cols`/`tile_count` と選択インデックス、When `move_overview_selection` を各方向で評価、Then グリッド端では該当方向へクランプ(ラップしない)、末尾行の欠けたセルをまたぐ移動でも範囲外インデックスを返さない。
- **AC-OV-15b** (REQ-OV-15b) [MH] [unit]+[manual] — Given 選択中タイルがライブタイル、When Return を処理、Then `focus_tab_from_overview` 相当が選択タブの WindowId で呼ばれる(unit);Given 選択中タイルが **placeholder 行**、When Return を処理、Then 同様に当該タブへフォーカスが解決される(unit、placeholder も選択可能であることを検証);Given 実 GUI、Then Return でハイライト中タブへ実際にフォーカスが移る(manual)。
- **AC-OV-15c** (REQ-OV-15c) [MH] [unit] — Given Overview フォーカス中で N 個のライブタイル、When `Cmd+k`(1≤k≤min(N,9))を dispatch、Then `overview_command_scope` の no-op 化を経由せず、行優先 k 番目のライブタイルのタブへ直接フォーカスが解決される;When `k>N`、Then no-op(存在しないタイルへは切り替わらない、パニックなし)。
- **AC-OV-15d** (REQ-OV-15d) [MH] [unit]+[manual] — Given Overview 表示中、When Esc を処理、Then Overview が非表示になり、いかなるタブへのフォーカス変更も発生しない(unit: dispatch記録);Given 実 GUI、Then Esc で Overview が閉じ元のタブ表示に戻る(manual)。
- **AC-OV-16a** (REQ-OV-16) [MH] [unit] — Given `overview_tab_filter(query, titles)`、When query `"log"` を大文字小文字混在タイトル群(例: `"Build Log"`, `"logs-worker"`, `"README"`)に適用、Then タイトルに `query` を大文字小文字無視の**連続部分文字列**として含むもの(`"Build Log"`, `"logs-worker"`)のみが順序保持で返り、`"README"` は含まれない(非連続部分列マッチとの違いを明示する回帰: `"lg"` のような非連続クエリはヒットしない)。
- **AC-OV-16b** (REQ-OV-16) [MH] [unit] — Given 検索クエリの変更、When グリッド再レイアウトを評価、Then `compute_overview_grid` へ渡される source id が絞り込み後の集合のみになり、選択インデックスが 0 へリセットされる。
- **AC-OV-16c** (REQ-OV-16) [MH] [unit]+[manual] — Given Overview フォーカス中で検索フィールドがアクティブ、When 印字可能文字を入力、Then クエリ末尾に追記されどの pty にも到達しない(unit: ルーティング分岐;manual: pty バイト非到達を確認)。
- **AC-OV-17** (REQ-OV-17) [MH] [unit]+[visual] — Given ライブタイル数 N(=min(タブ数,9))、When ヒントバー文字列を構築、Then `"⌘1-N to switch・↑↓←→ to navigate・Return to open・esc to close"` の N が実際のライブタイル数に一致する;Given 実 GUI、Then ヒントバーが下部中央に表示される(visual)。
- **AC-NF-12** (REQ-NF-12) [MH] [unit] — Given 1件のみ dirty かつスロットル未到来(`should_render_tile=false`)で他に変化なしの状態、When 当該フレームの `redraw_overview` 相当の判定ロジックを評価、Then `present_overview_frame` は呼ばれない(フレーム内容不変)、かつ次回描画要求はスロットル期限に合わせてスケジュールされ即時再要求されない(`app.rs:1710-1745` の現行無条件 present + 即時再要求バグの再発防止を回帰テストとして固定する)。
- **AC-NF-13** (REQ-NF-13) [MH] [unit]+[inspection] — Given Overview のタイル描画経路、When 対象タブの現在内容を取得、Then io スレッドが publish する read-only・damage 非消費のスナップショット経由でのみ読み取り、対象タブの `Terminal` Mutex を直接ロックしない(unit: publish/read が独立した経路であることを検証);`render_due_overview_tiles` 相当のコードに `surface.terminal.lock()` / `FrameSnapshot::from_terminal` の直接呼び出しが残っていないことをコードレビューで確認する(inspection)。

### Traceability — v2 追加分(REQ ↔ AC)

| REQ | AC | 優先度 |
|---|---|---|
| REQ-OV-11 | AC-OV-11 | MH |
| REQ-OV-12 | AC-OV-12 | MH |
| REQ-OV-13 | AC-OV-13 | MH |
| REQ-OV-14 | AC-OV-14 | MH |
| REQ-OV-15 | AC-OV-15a, AC-OV-15b, AC-OV-15c, AC-OV-15d | MH |
| REQ-OV-16 | AC-OV-16a, AC-OV-16b, AC-OV-16c | MH |
| REQ-OV-17 | AC-OV-17 | MH |
| REQ-NF-12 | AC-NF-12 | MH |
| REQ-NF-13 | AC-NF-13 | MH |

**Coverage: v2 追加 9/9 要件が ≥1 AC にトレース = 100%**(v2 追加 AC 総数 **16**、全 [MH])。v1 と合算した本ファイル全体のカバレッジ: 30/30 要件 = 100%、AC 総数 41。
