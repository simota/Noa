# AppleScript 連携 (draft)

- slug: applescript
- status: draft
- current-phase: SHAPE (提案書提示中)
- owner: simota

## L0 — Vision (作成中)

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

## Assumption Ledger

(未記入)

## 決定 (CHALLENGE)

**Pick: A — verb先行サブセット** (sdef + NSAppleEventManager 手動登録、UserEvent 注入)。問題文はユーザー確定済み (2026-07-10)。

Considered but rejected:
- B. フル Cocoa Scripting オブジェクトモデル — winit 所有 delegate との統合リスク + objc2 での NSScriptObjectSpecifier 実装コスト大。Ghostty 側も 1.3 preview で API 不安定
- C. 段階ハイブリッド — Phase 1 は A と同一。オブジェクトモデルは本 spec の Open Questions に記録して据え置き
- D. URL scheme / CLI ソケット — Ghostty 辞書と非互換でパリティ目的に不適合
