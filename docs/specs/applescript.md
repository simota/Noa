# AppleScript 連携

- slug: applescript
- status: locked (2026-07-10)
- build-path: 未決定 (LOCK 後チェックポイントで選定)
- owner: simota

## L0 — Vision

Ghostty parity gap `REQ-MACOS-003` / `IMPL-MACOS-003`（AppleScript dictionary and command bridge）を埋める。
Ghostty は 1.3.0 で AppleScript 対応を導入（preview 扱い）:

- Hierarchy: application → windows → tabs → terminals
- Commands: new window / new tab / split / focus / select tab / close* / input text / send key / send mouse * / perform action / new surface configuration
- Config: `macos-applescript`（default true）、TCC Automation 権限で保護
- 出典: https://ghostty.org/docs/features/applescript

## FRAME — reuse-scan / 制約 (frame-lens 2026-07-10)

- objc2 基盤あり: `noa-app/Cargo.toml:32-40`(objc2 0.6 / objc2-foundation / objc2-app-kit、features は要追加)。生 msg_send! 先例多数(macos_window.rs, notification.rs 等)
- 注入経路確立済み: `macos_hotkey.rs:388,582` が winit 外コールバック→`EventLoopProxy::send_event(UserEvent::…)` の先例。AppleScript ハンドラも同構造でよい
- コマンド語彙: `commands/command.rs:9` `AppCommand`(NewTab/NewWindow/CloseTab/SelectTab/NewSplit*/ToggleFullscreen 等)がそのまま verb にマップ可能
- **制約1**: ウィンドウ/タブ生成は `ActiveEventLoop` 必須 → Apple Event コールバックから直接不可、必ず `UserEvent` 注入で `user_event`(event_loop.rs:36)内処理
- **制約2**: `input text` 相当は AppCommand 未存在 → `UserEvent::WriteText` 新設。配管は `queue_pane_pty_bytes`(input_ops/terminal.rs:234)既存
- **制約3**: winit が NSApp delegate を所有 → full Cocoa Scripting(NSScriptSuiteRegistry オブジェクトモデル)は delegate 差し替えリスク。`NSAppleEventManager` 手動登録が低リスクだが、オブジェクトモデルクエリ(`every terminal whose …`)の再現度に影響
- bundle: Info.plist は bundle-macos.sh のヒアドキュメント手書き。`NSAppleScriptEnabled`/`OSAScriptingDefinition` + .sdef の Resources 配置が要追加。リポジトリに .sdef なし

## L1 — Requirements

### Functional
- **R-1 配線**: `Noa.sdef` を新規作成し `Contents/Resources/` へ配置。`bundle-macos.sh` の Info.plist に `NSAppleScriptEnabled=true` と `OSAScriptingDefinition=Noa.sdef` を追加。sdef の用語(クラス名/verb名/パラメータ名)は Ghostty 1.3.0 と同一綴り。
- **R-2 ハンドラ登録**: 起動時に `NSAppleEventManager.sharedAppleEventManager().setEventHandler(...)` で Apple Event ハンドラを登録。config `macos-applescript`(bool, default true)が false なら登録をスキップ。
- **R-3 生成 verb**: `new window` / `new tab` を受理し `AppCommand::NewWindow/NewTab` へマップ。optional パラメータ `initial working directory`(alias/POSIX path) と `command`(string) をサポート。
- **R-4 分割 verb**: `split` を direction(`right`/`left`/`down`/`up`)付きで受理し `AppCommand::NewSplit*` へマップ。
- **R-5 フォーカス verb**: `focus`(terminal) / `activate window` / `select tab` を受理。activate は `activateIgnoringOtherApps:` + 対象 window 前面化。
- **R-6 クローズ verb**: `close`(terminal) / `close tab` / `close window` を受理し既存クローズ経路へマップ。
- **R-7 テキスト入力**: `input text` を受理し、新設 `UserEvent::WriteText { window_id, pane_id, text }` 経由で `queue_pane_pty_bytes` へ流す。挙動は paste と同一(bracketed paste モード中は括り付与、改行はそのまま送出)。
- **R-8 アクション実行**: `perform action "<ghostty-action>"` を受理し変換テーブルで `AppCommand` へマップ。**閉規則: L2 の対応表に列挙された action のみ受理**し、それ以外は AE エラー `errAEEventNotHandled (-1708)` を返す。
- **R-9 プロパティ読み取り**: get 系イベントに応答 — application: `name`/`version`/`frontmost`、window: `id`/`name`、tab: `id`/`name`/`index`/`selected`、terminal: `id`/`name`/`working directory`。オブジェクト指定は **index 形式と id 形式のみ**(`whose` フィルタ非対応)。
- **R-10 エラー応答**: パース不能・対象不在・非対応形式のイベントには黙殺でなく AE エラー reply を返す(未対応 verb/action は `errAEEventNotHandled (-1708)`、対象不在は `errAENoSuchObject (-1728)`、不正パラメータは `errAEParamMissed (-1715)` を基準とする)。

### Non-functional
- **R-11 スレッド規律**: AE コールバックから winit オブジェクトへ直接触れない。全操作は `EventLoopProxy<UserEvent>` 注入 → `user_event`(ActiveEventLoop 文脈)で実行。プロパティ読み取りは main スレッドへの同期問い合わせ(reply 保留)または App state のスレッド安全スナップショットから応答。
- **R-12 権限**: TCC Automation プロンプトは OS 任せ。独自プロンプト・独自権限 UI を作らない。
- **R-13 検証性**: `osascript` 駆動のスモークテストスクリプトを `scripts/applescript-smoke.sh` として同梱(実機・手動実行前提、CI 対象外)。

## L2 — Detail (実装骨子)

- 新規モジュール `noa-app/src/macos_applescript.rs`: ハンドラ登録(`Registration` 構造体が proxy を Box 所有 — macos_hotkey.rs:388 と同型)、AEDesc パース、reply 構築。
- `UserEvent::WriteText` 追加(events.rs)。window/pane 解決は id 指定なければ focused。
- id 系: window id = 既存 `WindowGroupId`/winit WindowId の安定整数、tab/terminal id = session_store の既存 id を流用(新規 id 体系を作らない)。
- objc2-foundation features 追加(NSAppleEventManager / NSAppleEventDescriptor)。
- **perform action 対応表(初期集合・閉)**: `new_tab`→NewTab, `new_window`→NewWindow, `new_split:right|left|up|down`→NewSplit*, `close_tab`→CloseTab, `close_window`→CloseWindow, `next_tab`→NextTab, `previous_tab`→PrevTab, `goto_tab:<n>`→SelectTab(n-1), `toggle_fullscreen`→ToggleFullscreen, `copy_to_clipboard`→Copy, `paste_from_clipboard`→Paste, `reload_config`→ReloadConfig, `quit`→Quit。表にない action は -1708。テーブルは `commands/command.rs` 隣接に置き、keybind の action 文字列パーサがあれば共用。
- **pane(terminal)対象 verb の経路**: `focus`(terminal) と `close`(terminal) は AppCommand に変種がないため、`UserEvent::FocusPane { window_id, pane_id }` / `UserEvent::ClosePane { window_id, pane_id }` を新設し、既存 `split_tree` のフォーカス移動 / `request_close_pane` 経路へ接続する(直接呼び出しは R-11 により禁止)。

## L3 — Acceptance Criteria

| ID | 対応 R | 基準 |
|---|---|---|
| AC-1 | R-1 | ビルド済み Noa.app の Script Editor でライブラリを開くと Noa の辞書(window/tab/terminal クラスと全 verb)が表示される [manual] |
| AC-2 | R-2 | `macos-applescript = false` 設定時、osascript からの `new window` がエラーになり app は無反応(クラッシュ/生成なし) [manual] |
| AC-3 | R-3 | `tell app "Noa" to make new window` 相当でウィンドウが増える。`initial working directory` 指定時、新 terminal の cwd が一致 [manual] |
| AC-4 | R-3 | `command` 指定時、指定コマンドが新 surface で実行される [manual] |
| AC-5 | R-4 | `split right/left/down/up` で focused terminal が対応方向に分割される [manual] |
| AC-6 | R-5 | `select tab 2 of window 1` / `activate window` が UI 上のタブ選択・前面化と一致 [manual] |
| AC-7 | R-6 | `close tab` / `close window` が UI のクローズと同じ確認ポリシーで動作 [manual] |
| AC-8 | R-7 | `input text "echo hi\n"` で pty に paste 同一経路のバイト列が届く(bracketed paste 中は ESC[200~/201~ 付与) — 単体テスト可能な変換関数として実装 [unit] |
| AC-9 | R-8 | `perform action "toggle_fullscreen"` が動き、未知 action `"nonexistent"` は AE エラーを返す [manual] |
| AC-10 | R-9 | `working directory of terminal 1 of tab 1 of window 1` が実 cwd を返す。加えて application の `frontmost`/`version`、tab の `index`/`selected` が実状態と一致。index/id 両形式で解決 [manual] |
| AC-11 | R-10 | 不正パラメータの verb 送信で osascript が R-10 規定のエラーコードを受け取り、app が落ちない [manual] |
| AC-12 | R-11 | AE ハンドラ内から create_window 等を直接呼ぶコードが存在しない(全て UserEvent 経由) — コードレビュー基準 [review] |
| AC-13 | R-13 | `scripts/applescript-smoke.sh` が AC-3/5/6/9/10/15/16 を一括実行し PASS/FAIL を出力(input text は送信後の画面/pty 出力で観測) [manual] |
| AC-14 | R-12 | 独自の権限プロンプト・権限 UI を生成するコードが存在しない(TCC は OS 任せ) — コードレビュー基準 [review] |
| AC-15 | R-5 | `focus terminal 2 of tab 1 of window 1` で対象 pane にフォーカスが移る(カーソル描画・入力先が一致) [manual] |
| AC-16 | R-6 | `close terminal 2 of ...` で対象 pane のみ閉じ、分割レイアウトが UI のペーンクローズと同一挙動で再配置される [manual] |

## Scope

**In:** R-1..R-10 の全 verb/プロパティと config キー、非機能要件 R-11..R-12、スモークスクリプト R-13。
**Out:** `whose` クエリ等フルオブジェクトモデル / `send key` / `send mouse *` / `new surface configuration` の残りフィールド(font size, env vars, initial input, wait after command) / Shortcuts App Intents / AppleScript からの設定変更。

## Open Questions / Deferred Decisions

- OQ-1: フルオブジェクトモデル(Cocoa Scripting 化)は Ghostty 1.4 の API 安定後に再評価(案C 相当)。
- OQ-2: `send key` / `send mouse` は必要になった時点で別 spec。
- OQ-3: プロパティ読み取りの同期方式(reply 保留 vs 状態スナップショット)は実装時に決定 — R-11 を満たす限り自由。

## Assumption Ledger

- ASSUME-1 (ratified): sdef 用語は Ghostty と同一綴り — SHAPE でユーザー承認済み提案に含まれる。
- ASSUME-2 (elicited): `send key` out-of-scope — SHAPE チェックポイントで確認事項として明示し「ok」で承認。
- ASSUME-3 (silent→要確認): id 体系に既存 session/tab id を流用する点はユーザー未確認(実装詳細のため L2 記載に留める)。

## 決定 (CHALLENGE)

**Pick: A — verb先行サブセット** (sdef + NSAppleEventManager 手動登録、UserEvent 注入)。問題文はユーザー確定済み (2026-07-10)。

Considered but rejected:
- B. フル Cocoa Scripting オブジェクトモデル — winit 所有 delegate との統合リスク + objc2 での NSScriptObjectSpecifier 実装コスト大。Ghostty 側も 1.3 preview で API 不安定
- C. 段階ハイブリッド — Phase 1 は A と同一。オブジェクトモデルは本 spec の Open Questions に記録して据え置き
- D. URL scheme / CLI ソケット — Ghostty 辞書と非互換でパリティ目的に不適合

## Amendment 1 — Risk Gate 決定事項 (2026-07-10, omen/ripple)

L1 と矛盾しない実装制約の確定。L2 と齟齬がある場合この節が優先する:

1. **OQ-3 解決: プロパティ読み取りはスナップショット方式で確定**(reply 保留方式は禁止)。実測により AE ハンドラは NSApp イベント処理中に **main スレッド上で**ディスパッチされる — R-11 の規律は「AE ハンドラ内で winit オブジェクト直接操作・channel recv/condvar/block_on による reply 待機を禁止。変更系は UserEvent 注入、読み取り系は main が更新する `Arc<Mutex<AppStateSnapshot>>`(window/tab/terminal の id/name/index/selected/cwd — cwd は既存 OSC7 追跡値)から同期 reply」と読み替える。
2. **R-3 パラメータ付き生成は専用 `UserEvent::SpawnTab { window_target, cwd, command }` を新設**して運ぶ。`AppCommand::NewTab/NewWindow` は unit 変種のまま非改変(ペイロード化は command_palette/tests 450件超の波及のため却下)。パラメータ無しの生成は従来どおり AppCommand 経由。`command` 実行の spawn 経路は新規配管(spawn_tab_with_cwd は cwd のみ既存)。
3. **ハンドラ登録は winit `resumed`**(finishLaunching 後)で一度きり。`hotkey_install_attempted`(app.rs:257)と同パターン。Registration の Drop で RemoveEventHandler + Box 回収(macos_hotkey.rs:401-528 同型)。
4. **AE 4文字コードは単一 const テーブル**から登録し、sdef XML と照合する単体テストを併設(サイレント no-op 防止)。catch-all で最低 -1708 を返す。
5. **WriteText 規約**: window_id/pane_id は AE 解決時に凍結(処理時 focused 再解決禁止)、対象消失時は破棄。bracketed paste 括りは処理時のペインモードで付与。AE 入力テキストにサイズキャップ(既存 paste 上限に合わせる)。
6. config キーは `macos_non_native_fullscreen` を鏡に10触点(default **true** に注意) + `is_supported_scalar_key`(overrides.rs:432)追加で import 経路も吸収。
7. perform action 表は既存 `command_from_keybind_action`/`ghostty_action_alias`(commands/keybind.rs:237,244)を再利用(AppCommand 変更不要、未知 None → -1708)。

## Amendment 2 — 実装時の確定事項 (2026-07-10, attest/judge)

- **1.7 の逸脱(正当)**: keybind パーサ再利用ではなく専用の閉表 `command_from_applescript_action` を新設。R-8 の閉規則(L1)は keybind の広い語彙の再利用と矛盾するため L1 を優先。表内容は L2 対応表と一致。
- **goto_tab の訂正**: L2 の「`goto_tab:<n>`→SelectTab(n-1)」は 0-based 誤仮定。実装の select_tab は 1-based のため `SelectTab(n)` が正。
- **make new window の返り値**: 生成が UserEvent 経由の非同期のため生成オブジェクトを同期返却しない(`set w to make new window` は missing value)。R-3 非要求につき設計制約として受理。
- **activate 実現方式**: `activateIgnoringOtherApps:` 明示呼び出し+window ordering(judge High 指摘の修正で確定)。

## 品質ゲート記録

2026-07-10 Spec Quality Gate 実施(spec-gate)。初回 2 PASS / 4 FAIL → 必須修正4件(perform action 閉規則化・エラーコード固定・pane verb 経路 + AC-15/16・R-12 AC-14 追加/AC-10 拡張)を反映済み。事実根拠(file:line)は全件検証合格。
