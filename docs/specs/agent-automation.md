# エージェント自動化サーフェス強化（AppleScript v2 + Accessibility）

| | |
|---|---|
| slug | agent-automation |
| status | locked（2026-10-17、owner サインオフ） |
| owner | simota |
| as-of | 2026-10-17 |
| review trigger | Ghostty の `macos/Ghostty.sdef` 変更、winit メジャー更新、または `docs/specs/applescript.md` の改訂 |
| depth | standard |
| build path | one-shot autonomous build |

## L0 — Vision
- **Problem** — エージェントが Noa を外から操作して出力を読み返すには、今は noa-server（`noa.getText`/`sendText`/`split`、`docs/specs/noa-server.md:70-76`）を使うしかない。noa-server は既定で無効で、使うには起動とトークンの払い出しが必要になる。設定なしで使える AppleScript は Ghostty 1.3 の辞書より狭く、出力を読めず、実際のキーイベントも送れない（`input text` は paste 扱い）。AX ツリーには端末ペインが一切出てこないので、AX を読むアシスタントやエージェントからは Noa の中身が見えない。（FRAME 改訂 2026-10-17：反証パネルの指摘を受けて書き直し、ratified）
- **Audience** — (a) shell から `osascript` で Noa を操作するエージェント。設定もトークンも使わない。(b) macOS の AX ツリーを読むアシスタントや、AX ベースのエージェントフレームワーク。スクリーンショットと座標操作だけで動く computer use クライアントには、AX は何も足さない（Anthropic の computer use ツールの公開仕様）。（elicited 2026-10-17、改訂 ratified）
- **Job to be done** — 設定なしで、エージェントが Noa の terminal を特定し、キーやテキストを送り、その結果をテキストとして読み返すこと。
- **Success definition** — 1 本の osascript で「split → input text → contents 読み取り」が完走すること（AC-010）。AX を読むクライアントから各ペインの画面テキストと位置が取れること（AC-019/020）。Ghostty.sdef の用語がすべて Noa の辞書にあること（AC-028）。
- **Existing assets & constraints**（Reuse scan 2026-10-17、HEAD 25577d2）
  - 既存 AppleScript：`assets/Noa.sdef`、`crates/noa-app/src/macos_applescript.rs`、`crates/noa-app/src/app/applescript.rs`。spec は `docs/specs/applescript.md`（locked）。今は `coregetd`、`coreclos`、`miscactv` と Noa Suite だけを自前で登録している（`macos_applescript.rs:640-662`）。それ以外の標準イベント（`count` など）には、`NSAppleScriptEnabled` によって Cocoa Scripting が応答している。
  - **C1**：Apple Event は main スレッドで dispatch される。ハンドラ内でイベントループの応答を待つことは禁止。読み取りは `Arc<Mutex<AppStateSnapshot>>` から行う（applescript Amendment 1.1）。`sync_applescript_snapshot` は、signature が変わるか 500 ms 経つと snapshot を丸ごと置き換える。
  - **C2**：`PaneId` は `split_tree/tree/types.rs:31` のグローバル `AtomicU64`。tab id は今 `u64::from(WindowId)`。`WindowGroupId` は `App::next_group_id` が採番する（`lifecycle.rs:729`）。既存の `UserEvent::WriteText`/`RaiseWindow`/`ClosePane` は winit の `WindowId` を運ぶ（`events.rs:100-125`）。`UserEvent` は `Eq` を derive している。
  - **C3**：winit 0.30.13 の macOS 実装に accessibility の実装はない。NSView は既定で AX から無視されるので、子要素の AX 上の親は、無視されていない最も近い祖先になる。
  - **C4**：フォントサイズはタブ単位（`WindowState.font_px`、`state.rs:262`）。
  - **C5**：`PtyConfig`（`noa-pty/src/pty.rs:21`）は環境変数を受け取れない。`Pty` は子の pid も tty 名も公開していない。wait-after-command は未実装。
  - **C6**：io thread は最大 64 KiB のチャンクごとに `Terminal` のロックを握る（`io_thread/feed.rs:220`）。リサイズ時の reflow もロックの中で行う。
  - 再利用：`ipc_bridge.rs:498` の `screen_text()`（今は private。`pub(crate)` にする）、`Terminal::scrollback_text_tail()`（`noa-grid/src/terminal.rs:427`、active screen を含む全履歴の末尾）、`Terminal::selected_text()`、`encode_key_with_modes`（`input/key.rs:82`）、`encode_mouse_input/motion/wheel`（`mouse.rs:244-300`）、`APPLESCRIPT_INPUT_TEXT_CAP`（8 MiB）、`noa_ipc::DEFAULT_TEXT_MAX_BYTES`（256 KiB）、`noa_ipc::truncate_tail`。
  - 準拠の基準：Ghostty `macos/Ghostty.sdef` と `macos/Sources/Features/AppleScript/*.swift`（main、2026-10-17 取得）。必要な部分は Appendix A/B としてこの文書に取り込んである。

## Glossary
| Term | Definition | Not to be called | Code identifier |
|------|-----------|------------------|-----------------|
| window | ネイティブタブを束ねたタブグループ 1 つ | tab group | `WindowGroupId` |
| tab | ネイティブ NSWindow タブ 1 枚 | window（AppleScript 文脈では） | `WindowState` / 新設 `TabId` |
| terminal | split の 1 ペイン | pane、surface（辞書・AC 上で） | `PaneId` |
| surface configuration | 生成時の設定値をまとめた AE record（6 フィールド） | config、profile | `SurfaceConfiguration` |
| pending object | 事前採番済みで、まだ生成処理が終わっていない window/tab/terminal | — | `PendingRegistry` の要素 |
| contents | terminal が**今表示している** viewport のテキスト。ユーザーが scrollback を遡っていれば、その位置の行になる | screen text、buffer | `screen_text(visible_rows())` |
| scrollback | active screen を含む全履歴の末尾 256 KiB | history | `scrollback_text_tail` |
| AX element | terminal 1 つに対応する NSAccessibility 要素 | accessibility node | `TerminalAxElement` |

## L1 — Requirements

### Functional (`REQ-n`)
| ID | Requirement | Priority | Source |
|----|-------------|----------|--------|
| REQ-001 | 参照プロパティ：application `front window`（key な tab が属する window。key がなければ最後に key だった window）、window `selected tab`、tab `focused terminal`、terminal `pid`・`tty`。application と window に `terminals` 要素を追加する。index 形式、id 形式、every 形式（`terminals of window 1` はリストを返す）で解決できる。index の順序は、window は `window_order` で最初に現れる順、tab は `window_order` 内の相対順、terminal は `PaneId` の昇順 | MUST | elicited（範囲 1） |
| REQ-002 | コマンド `close tab <tab>`、`close window <window>`、`count`（`corecnte`）、`exists`（`coredoex`）を自前で登録する（Cocoa Scripting の応答を置き換える）。`count windows` は window（タブグループ）の数を返す。旧形式 `close tab 2 of window 1` は Amendment 2 により breaking change | MUST | grounded（Ghostty.sdef）+ elicited（close の競合の回答、Amendment 2） |
| REQ-003 | `new surface configuration [from <record>]` が 6 フィールドの record を返す。`new window`、`new tab`、`split` が `with configuration <record>` を受け付ける。既存の `initial working directory` / `command` パラメータも引き続き受け付ける | MUST | elicited（範囲 1） |
| REQ-004 | surface configuration の意味は L2.4 に従う。`font size` は new window/new tab で作られる tab にだけ反映し、split では無視する | MUST | elicited（font size の回答） |
| REQ-005 | `wait after command` が true のとき、子プロセスが終了しても（`command` の有無にかかわらず）ペインを残し、`Process exited. Press any key to close the terminal.` を表示する。次のキー押下（物理または `send key`）でペインを閉じる | MUST | elicited（wait after の回答） |
| REQ-006 | `new tab in <window>` で指定 window（pending でもよい）に tab を追加する。`in` を省略すると、AE を受け取った時点の live な `front window` に追加する（pending の window は対象にしない。live な window がなければ新しい window を作る）。new window と new tab で作った tab は、UI の `cmd+n`/`cmd+t` と同じく Noa 内で key・選択状態になる。アプリの activate はしない | MUST | elicited（範囲 1）+ ASSUME-6 |
| REQ-007 | `split <terminal> direction <dir> [with configuration]` は、指定した terminal を分割する。旧形式 `split <dir>` は focused terminal を分割する | MUST | elicited + 互換（REQ-013） |
| REQ-008 | 生成コマンドは作ったオブジェクトの id 形式の参照を返す（new window→window、new tab→tab、split→terminal）。返した参照（とその要素：`selected tab of w`、`focused terminal of b`）は、直後の Apple Event からすぐ解決でき、操作対象にできる | MUST | elicited（D2 の選択） |
| REQ-009 | `perform action <s> on <terminal>`。対象 terminal に focus を移してから dispatch する（ASSUME-3）。`on` を省略すると focused terminal が対象。dispatch したら `true` を返す | MUST | elicited（範囲 1） |
| REQ-010 | `send key <name> [action press\|release] [modifiers <s>] [to <terminal>]`。キー名は Appendix A。現在のキーボード配列で 1 回だけ変換し、物理キーの押下と同じエンコーダ経路（DECCKM、kitty、modifyOtherKeys、`macos-option-as-alt` を含む）を通す | MUST | elicited（配列の回答）+ grounded |
| REQ-011 | `send mouse button`、`send mouse position`、`send mouse scroll` を、対象 terminal 上の同じ位置で物理イベントが起きたのと同じ効果にする。OS のカーソルは動かさない | MUST | elicited（範囲 2） |
| REQ-012 | Noa Extensions suite：terminal の `contents`、`selection`、`scrollback` プロパティ（読み取り専用。Glossary の定義どおり） | MUST | elicited（範囲 3） |
| REQ-013 | `docs/specs/applescript.md` の既存の動詞とプロパティは、すべてそのまま動き続ける | MUST | derived from REQ-002/003/007 |
| REQ-014 | 各 terminal を AX element として公開する。属性は L2.2 の AX 表のとおりで、すべて読み取り専用 | MUST | elicited（範囲 4） |
| REQ-015 | AX の子要素一覧と frame は、ペイン構成の変化（split、close、divider の移動、window のリサイズ）を適用する main スレッドの処理の中で更新する | MUST | derived from REQ-014 |

### Cross-functional (`CFR-n`)
| ID | Requirement | Measure / criterion | Target / required outcome | Verified how | Priority | Source |
|----|-------------|---------------------|---------------------------|--------------|----------|--------|
| CFR-001 | main スレッドの読み取りコスト | ロック取得を含む抽出 1 回分の時間（release ビルド）。(a) 200×60 全セル全角の `contents`、競合なし、(b) (a) と同じだが、別スレッドが 10 MB を 64 KiB チャンクで `Stream` に流し続ける、(c) 256 KiB の `scrollback`、競合なし | (a) p95 ≤ 1 ms、(b) p99 ≤ 8.3 ms（120 Hz の 1 フレーム）、(c) p95 ≤ 4.2 ms（半フレーム）。各 1000 回 | `#[ignore]` 付きの計測テストの出力 | HIGH | derived（C1、C6） |
| CFR-002 | 問い合わせがないときのコストはゼロ | AX と AE の値計算は getter の中だけで行い、定期処理やスレッドは増やさない／idle RSS | 新しいタイマー・スレッドは 0（レビュー）。idle RSS は main 比 +1 MiB 以下 | コードレビュー + `bench/` の idle 計測 | HIGH | derived（Noa の省メモリ方針） |
| CFR-003 | スレッド規律 | AE と AX のハンドラが、イベントループ・チャネル・condvar を待たず、winit オブジェクトに触れない（`Terminal` の mutex は CFR-001 の範囲で可） | 該当箇所 0（レビュー） | コードレビュー | CRITICAL | grounded（applescript Amendment 1.1） |

## L2 — Detail

### L2.1 Data model
| Entity | Field | Type | Constraints | Default | Nullable | Owned by |
|--------|-------|------|-------------|---------|----------|----------|
| `PendingRegistry` | window/tab/terminal の仮エントリ | 独立した `Arc<Mutex<…>>`（snapshot とは別） | AE ハンドラが挿入する。`sync_applescript_snapshot` は毎回、実体の後ろに pending をマージする。削除は 2 通り：(1) 実体のトポロジに同じ id が現れた sync、(2) 生成に失敗したときに UserEvent の処理側が明示的に削除 | 空 | — | AE ハンドラ / main |
| 仮エントリの位置 | — | — | window は最後、tab はその window の最後、terminal はその tab の最後。live な容器の `front window`・`selected tab`・`focused terminal` の結果にはならない。pending な容器の中では、最後に追加した pending な子がその容器の `selected tab`・`focused terminal` になる（`selected tab of w`、`focused terminal of b` が解決できる） | — | — | — |
| TerminalSnapshot | `terminal` | `Arc<Mutex<Terminal>>` | contents/selection/scrollback 用。pending の間は None（読むと `""`） | — | yes | main（sync） |
| TerminalSnapshot | `pid` / `tty` | `u32` / `String` | spawn 時に `Pty` から取得する（新しく公開する）。取れなければ missing value | — | yes | main |
| TabId | — | `u64`（新設のグローバル `AtomicU64`） | AppleScript の tab id。`u64::from(WindowId)` を置き換える | — | no | `WindowState` |
| WindowGroupId 採番 | — | `next_group_id` をグローバル `AtomicU64` に変更 | AE ハンドラから事前採番できるようにする | — | no | `lifecycle.rs` |
| UserEvent の宛先 | — | 新しく作るイベントと pending を対象にするイベントは `PaneId`（生成系はさらに事前採番した `TabId`/`WindowGroupId`）を運ぶ | main 側は pane → tab を検索して解決する。既存イベントの `WindowId` フィールドは、pane から解決できるときは使わない | — | — | — |
| `SurfaceConfigPayload` | 6 フィールド | `Eq` を満たす型。font size は 1/100 pt 単位の `u32` | `SpawnTab` と `Split` 系のイベントに載せる | — | — | — |

### L2.2 Interface contract
AppleScript。用語の綴りは Ghostty と同じ。四文字コードは、既存の用語は Noa の既存コードのまま、新しい用語は Appendix B の Ghostty のコードをそのまま使う。Noa Extensions は `contents=NEct`、`selection=NEsl`、`scrollback=NEsb`（suite `noaE`）。

| Operation | Input | Output | Errors |
|-----------|-------|--------|--------|
| get `front window` / `selected tab` / `focused terminal` | specifier | id 形式の参照 | 対象がない → -1728 |
| get `terminals of …` | every 形式 | 参照のリスト（要素 0 なら空リスト） | 容器がない → -1728 |
| get `pid` / `tty` / `contents` / `selection` / `scrollback` | terminal specifier | integer / text | -1728 |
| `close tab` / `close window` | specifier | — | -1728 |
| `count` / `exists` | specifier | integer / boolean | 容器がない → -1728（`exists` は false） |
| `new surface configuration [from r]` | record? | record（L2.4） | フィールド不正 → -1700 |
| `new window [with configuration r] [initial working directory s] [command s]` | | window 参照 | -1700 |
| `new tab [in w] [with configuration r] […]` | | tab 参照 | w がない → -1728 |
| `split <terminal\|dir> [direction d] [with configuration r]` | | terminal 参照 | 対象がない → -1728。direction がない（specifier 形式のとき） → -1715 |
| `perform action s [on t]` | | boolean `true` | 表にない action → -1708 |
| `send key name [action a] [modifiers m] [to t]` | 既定は press / 修飾キーなし / focused terminal | — | 未知のキー・修飾キー → -1700 |
| `send mouse button b [action a] [modifiers m] [to t]` | b は `left button`/`right button`/`middle button` | — | -1700 / -1728 |
| `send mouse position x x y y [modifiers m] [to t]` | real。ペインの左上が原点の point 座標 | — | 欠落 → -1715 / -1728 |
| `send mouse scroll x dx y dy [precision p] [momentum m] [to t]` | y>0 で上方向。precision の既定は false。false のとき dx/dy は 0 方向に丸めた整数のホイール刻み数（1 刻み = 物理ホイール 1 ノッチと同じ処理）。true のときは point 単位で、トラックパッドの delta と同じ処理。momentum は列挙値を検証し（不正なら -1700）受け付けるが、効果はない（Noa は物理ホイールの phase も使わない。Amendment 1） | — | 欠落 → -1715 / -1728 |

AX（`TerminalAxElement`、`NSAccessibilityElement` のサブクラス）：

| Attribute | Value |
|-----------|-------|
| AXRole / AXRoleDescription | `AXTextArea` / `terminal` |
| AXIdentifier | `noa-terminal-<terminal id>`（AppleScript の terminal id と同じ値） |
| AXTitle | terminal `name` |
| AXValue | `contents` と同じ文字列 |
| AXNumberOfCharacters | AXValue の UTF-16 長 |
| AXSelectedText | `selection` と同じ文字列 |
| AXInsertionPointLineNumber | viewport の中でのカーソル行（0 起点）。カーソルが viewport の外にあれば属性値なし |
| AXFrame | ペイン矩形（スクリーン座標） |
| AXFocused | その tab が key window で、かつ focused pane のとき true |
| AXParent | 無視されていない最も近い祖先（NSWindow、または代替案の AXGroup） |

取り付け方：第一案は winit の NSView（raw-window-handle の `ns_view`）に `setAccessibilityChildren:` で付ける。AX ツリーに出てこなかった場合の代替案は、hit-test しない透明 NSView を子に置き、そこに要素をぶら下げる。どちらでも REQ-014 の観測結果（AC-019/020 は `entire contents` で探す）は同じ。

### L2.3 State & transitions
| From | Event | To | Guard | Side effect |
|------|-------|----|-------|-------------|
| （なし） | 生成コマンドの AE | pending | 採番成功 | `PendingRegistry` に挿入し、事前採番した id を UserEvent に載せる |
| pending | main が UserEvent を処理 | live | spawn 成功 | 事前採番した id で実体を作る。次の sync で registry から消える |
| pending | spawn 失敗 | （削除） | — | 処理側が registry から削除する。その id を対象にした後続 UserEvent は破棄 |
| live（wait after=true） | 子プロセス終了 | exited | — | 終了メッセージを表示。contents は保持 |
| exited | キー押下（物理または `send key`） | （閉じる） | — | 通常の close-pane 経路 |

### L2.4 Validation rules
| Field | Rule | On violation | Message source |
|-------|------|--------------|----------------|
| font size | real。0 だけを未指定扱いにし、それ以外の有限値（負の値・巨大な値を含む）は f32 に変換する前に `clamp_runtime_font_size` の範囲（6〜96 pt）へクランプ | 数値でない → -1700 | errAECoercionFail |
| initial working directory | text。空文字は未指定扱い。存在しないディレクトリなら既存の spawn と同じフォールバック | 型不正 → -1700 | 同上 |
| command | text。空文字は未指定扱い。シェルの代わりに `/bin/sh -c <command>` を pty の子として起動する（CLI の `-e` と同じ。Amendment 1） | 型不正 → -1700 | 同上 |
| initial input | text。spawn 直後に pty へ生バイトとして書き込む（bracketed paste で包まない）。8 MiB（`APPLESCRIPT_INPUT_TEXT_CAP`）に切り詰める | 型不正 → -1700 | 同上 |
| wait after command | boolean（数値は 0 → false、それ以外 → true） | 型不正 → -1700 | 同上 |
| environment variables | `KEY=VALUE` の text のリスト。KEY は空でないこと。最初の `=` で分割する | 不正なエントリ → -1700 | 同上 |
| `with configuration` と旧パラメータの両方 | record 側のフィールドが空でなければそちらを優先し、空なら旧パラメータを使う | — | — |
| send key name | Appendix A の閉じた集合（大文字小文字を区別）。キーコードがあるキーは、現在の入力ソース（`kTISPropertyUnicodeKeyLayoutData` がなければ `TISCopyCurrentASCIICapableKeyboardLayoutInputSource`）で UCKeyTranslate を 1 回かけて文字を得る。dead key は合成しない。`-` のキーは文字なしで `KeyCode` だけで符号化する | -1700 | errAECoercionFail |
| modifiers | `,` で区切り、前後の空白を除き、大文字小文字を区別しない。`shift`/`control`/`option`/`command` のみ | -1700 | 同上 |
| scroll x/y | 1 回の量を、刻み数なら ±1000、point なら ±100000 にクランプしてから既存のホイール処理へ渡す（巨大な有限値で行数の変換が溢れないため） | 欠落 → -1715 | errAEParamMissed |
| mouse x/y | セルは `floor((p − padding) / cell) + 1`（境界上の点は右・下のセル）。ペインの範囲外ならペイン端のセルにクランプ（物理ドラッグがペイン外に出たときと同じ）。座標は動かさず、イベントは対象ペインに直接届ける（ドラッグ中の捕捉と同じく対象ペインの矩形で解決し、divider のドラッグは始めない）。ボタン・ホイールは、ウィンドウのポインタが別のペインにあるときは、そのペインに最後にスクリプトで送った位置（ペイン相対の point で保持し、現在の矩形と倍率で変換し直す）へ戻してから送る。一度も位置を送っていないペインでは左上に置く | 欠落 → -1715 | errAEParamMissed |

## Behavior matrix
| Condition | Applies to | Expected behavior | User-visible | Verifies |
|-----------|-----------|-------------------|--------------|----------|
| Empty state：window が 0 個 | REQ-001/002 | `front window` → -1728。`count windows` → 0。`terminals` → 空リスト | osascript のエラー | AC-001 |
| Empty state：選択なし | REQ-012/014 | `selection` と AXSelectedText は `""` | — | AC-017 |
| Empty state：pending の terminal を読む | REQ-008/012 | `contents`/`selection`/`scrollback` は `""`、`pid`/`tty` は missing value | — | AC-030 |
| Invalid input：未知のキー名・修飾キー・env エントリ・型不正 | REQ-003/010 | -1700。アプリは何もしない | osascript のエラー | AC-027 |
| Invalid input：必須パラメータの欠落 | REQ-007/011 | -1715 | osascript のエラー | AC-027 |
| Unauthorized：TCC Automation の拒否、Accessibility 権限がない | 全体 | OS が拒否する。Noa は独自の権限 UI を出さない（applescript R-12） | OS のダイアログ | N/A（OS が強制。権限 UI のコードがないことは AC-026 のレビューで確認） |
| Unauthorized：`macos-applescript = false` | REQ-001..013 | Noa のハンドラは何も登録しない。Noa Suite の動詞はエラーになり、何も起きない。標準イベント（`count windows` など）には Cocoa Scripting が従来どおり応答する。AX は影響を受けない（config キーなし、ratified） | osascript のエラー | AC-026 |
| Dependency unavailable：resolve から処理までの間に対象が閉じた | REQ-009..011 | UserEvent を破棄する。プロパティ読み取りは -1728 | — | AC-027 |
| Dependency unavailable：pending の生成に失敗 | REQ-008 | 処理側が registry から削除し、後続イベントを破棄。参照の読み取りは -1728 | — | AC-030 |
| Concurrent modification：生成直後に返した参照を使う | REQ-008 | registry で解決する。UserEvent は FIFO なので、生成の後に処理される | — | AC-010 |
| Concurrent modification：snapshot の再構築と pending の挿入が重なる | REQ-008 | 再構築は registry をマージするので、仮エントリは消えない | — | AC-030 |
| Concurrent modification：複数のスクリプトが同時に送る | 全体 | AE は main スレッドで直列に処理される。追加の制御は不要 | — | N/A（構造上保証） |
| Concurrent modification：io thread がロックを保持中 | REQ-012/014 | 読み取りはロックを待つ（CFR-001 の範囲）。reflow 中の待ちは CFR-001 の対象外（OQ-2） | 一時的な待ち | AC-022 |
| Limit exceeded：scrollback が 256 KiB を超える | REQ-012 | 末尾を優先して UTF-8 境界で切り詰める | 先頭が欠ける | AC-017 |
| Limit exceeded：initial input / input text が 8 MiB を超える | REQ-004 | 既存の `cap_input_text` と同じく切り詰める | — | AC-017 |
| Domain：全角文字・結合文字 | REQ-012/014 | `screen_text` と同じく、wide spacer を出力しない | — | AC-018 |
| Domain：Secure Keyboard Entry 中 | REQ-012/014 | 隠さずにそのまま公開する（ratified 2026-10-17） | — | AC-018 |
| Domain：ユーザーが scrollback を遡っている | REQ-012/014 | `contents` と AXValue は表示中の行を返す（Glossary） | — | AC-016 |
| Domain：key release（kitty の report-events が無効） | REQ-010 | 物理キーと同じく何も送らない | — | AC-012 |
| Domain：split の font size | REQ-004 | 無視し、親 tab のサイズを使う | — | AC-007 |
| Domain：モーダル（パレット・設定）を開いている | REQ-014 | AX のペイン要素はそのまま。モーダル自体は公開しない（対象外） | — | AC-019 |
| Domain：Noa が key window でない | REQ-014 | AXFocused はすべて false | — | AC-020 |

## Dependencies & integration points
| Dependency | Kind | We control? | Contract / version | Failure mode | Fallback |
|------------|------|-------------|--------------------|--------------|----------|
| NSAccessibility（AppKit） | OS API | no | macOS 13+（`LSMinimumSystemVersion`）の `NSAccessibilityElement` | winit のビューで子要素が出てこない | L2.2 の透明 NSView 案 |
| TIS / UCKeyTranslate（HIToolbox） | OS API | no | 現在の入力ソースの `kTISPropertyUnicodeKeyLayoutData` | レイアウトデータがない入力ソース（IME など） | ASCII 対応の入力ソース（L2.4） |
| Cocoa Scripting（`NSAppleScriptEnabled`） | OS | no | 自前で登録したイベントは自前のハンドラが優先される | — | — |
| portable-pty | crate | no | 子の `process_id()` と tty 名 | 取れない | pid/tty は missing value |
| Ghostty.sdef の用語 | 外部仕様 | no | Appendix B（main 2026-10-17） | Ghostty 側の改名 | review trigger で再照合 |

## Migration & rollout
- **Existing data** — 永続データはない。
- **挙動の変化** — (1) tab id が `WindowId` 由来から `TabId` に変わる。id は実行中だけ有効な値なので互換性の問題はない。(2) `count windows` は、Cocoa Scripting が数えていた NSWindow の数（ネイティブタブ 1 枚ごとに 1）から、window（タブグループ）の数に変わる。Noa の `window` クラスの定義に合わせた変更として CHANGELOG に記載する。
- **Staged release** — フラグは使わない。既存の `macos-applescript` が AppleScript 側の有効・無効を決める。
- **Rollback** — コミットを revert するだけ。不可逆な変更はない。

## L3 — Acceptance Criteria
マーカー文字列は、入力したコマンド行には現れず、実行結果にだけ現れる形で作る（例：`printf 'NOA-%s\n' AC10` の出力は `NOA-AC10`）。

| ID | Given | When | Then | Verifies | Mode | Oracle | Must-have |
|----|-------|------|------|----------|------|--------|-----------|
| AC-001 | fixture snapshot（window 2 個。window 1 に tab 2 枚、tab 2 が選択中でペイン 2 つ、ペイン 2 に focus。加えて window 0 個の fixture） | resolver で `front window`、`selected tab of window 1`、`focused terminal of tab 2 of window 1`、`terminals of window 1`、`terminals`（app）、`count windows` を解決する | window 1、tab 2、ペイン 2、window 1 の全ペイン（PaneId 昇順）、全ペイン、2 になる。0 個の fixture では -1728、空リスト、0 | REQ-001, REQ-002 | machine | `macos_applescript.rs` のユニットテスト | yes |
| AC-002 | 実機。terminal 1 つ（ログインシェルが動いている） | `pid of t` と `tty of t` を取得する | pid が `ps -o pid=,comm= -t <tty の ttysNNN 部分>` の中で、comm の basename（先頭の `-` を除く）が `$SHELL` の basename と一致する行の pid と一致する。tty が `/dev/` + その ttysNNN と一致する | REQ-001 | human | smoke | yes |
| AC-003 | Noa を前面にし、ペイン 2 つの tab がある。ペイン 2 に focus | `focused terminal of selected tab of front window` の id を取る。System Events で AXFocused が true の要素の AXIdentifier を取る | 両者が同じ id を指す | REQ-001, REQ-014 | human | smoke | yes |
| AC-004 | — | record の解析・直列化関数に、6 フィールド／`from` による複製／不正な env `"NOEQ"`／型不正／旧パラメータとの優先順位（record の空文字と非空文字）を与える | 往復で値が保たれる。不正な入力は -1700。優先順位は L2.4 のとおり | REQ-003, REQ-004 | machine | ユニットテスト | yes |
| AC-005 | — | new window、new tab、split（`direction right`）のそれぞれで `with configuration {initial working directory:"/usr", environment variables:{"FOO=bar"}, initial input:"printf 'CFG-%s-%s\\n' \"$PWD\" \"$FOO\"\n"}` | 2 秒以内に、3 つの terminal の `contents` がそれぞれ `CFG-/usr-bar` を含む | REQ-003, REQ-004 | human | smoke | yes |
| AC-006 | — | (a) `command:"/bin/echo done", wait after command:true` で new tab を作る。(b) command なし、`wait after command:true`、`initial input:"exit\n"`。(c) (a) と同じだが wait なし。(d) (a) の状態から物理キーを押す | (a) と (b) は 2 秒後も terminal が存在し、`contents` が終了メッセージを含む（(a) は `done` も含む）。`send key "a" to` で閉じる。(c) は 2 秒以内に消える。(d) も閉じる | REQ-005 | human | smoke（(d) は目視） | yes |
| AC-007 | 既定 14pt | `new window with configuration {font size:28}`。そのペインで `split … with configuration {font size:8}` | 新しい window の行の高さが既定の window の 1.8〜2.2 倍。split したペインの行の高さは親と同じ | REQ-004 | human | スクリーンショットで行の高さを比較 | yes |
| AC-008 | window 2 個。window 1 に focus | `new tab in window 2` | window 2 の tab 数が +1、window 1 は変わらない | REQ-006 | human | smoke | yes |
| AC-009 | Noa を前面にし、ペイン 1 つの tab で `split right` して右のペインを作った状態（左 = terminal 1）。右に focus | `split (terminal 1 of selected tab of front window) direction down`、続けて旧形式 `split right` | 1 回目の後、AX の frame で左のペインが上下に分かれている。2 回目の後、focused terminal が左右に分かれている。ペイン数は 4 | REQ-007, REQ-013 | human | smoke（AX frame で判定） | yes |
| AC-010 | Noa を `activate` 済み。window 1 個 | 1 本の osascript で：`set t to split (focused terminal of selected tab of front window) direction right` → `input text "printf 'NOA-%s\\n' AC10\n" to t`。`set w to new window` → `input text "printf 'NOA-%s\\n' W10\n" to focused terminal of selected tab of w`。`set b to new tab in w` → `input text "printf 'NOA-%s\\n' B10\n" to focused terminal of b`。2 秒待ち、3 つの `contents` を読む。最後に `id of w` と `id of front window`、`id of b` と `id of selected tab of front window` を比べる | どのステップも -1728 にならない。3 つの contents がそれぞれ `NOA-AC10`、`NOA-W10`、`NOA-B10` を含む。w は最後に key になった window なので `id of w` = `id of front window`。b は w の中で最後に選択された tab なので `id of b` = `id of selected tab of front window` | REQ-006, REQ-008 | human | smoke | yes |
| AC-011 | Noa を前面にし、ペイン 1 つの tab で `split right` して右のペインを作った状態（左 = terminal 1）。右に focus | `perform action "new_split:down" on terminal 1 of selected tab of front window`。続けて `perform action "nonexistent"` | 1 つ目は `true` を返し、AX の frame で左のペインが上下に分かれている。2 つ目は -1708 | REQ-009 | human | smoke（AX frame で判定） | yes |
| AC-012 | — | 解析・符号化関数に `enter`、`arrowUp`、`a`、`digit1`、`f21`、`foo`、`"shift, Control"`、`hyper` を与える。release を kitty フラグ 0 と report-events ありで符号化する。固定の US レイアウトデータで `semicolon`+shift を変換する | 既知の名前は Appendix A のキーコードと winit `KeyCode` に対応する。`foo` と `hyper` はエラー。release はフラグ 0 では空、report-events ありでは kitty の release 列になる。US の変換結果は `:` | REQ-010 | machine | ユニットテスト | yes |
| AC-013 | US 配列 | (1) `cat -v` を実行し、`send key "arrowUp"` → `send key "enter"` → `send key "c" modifiers "control"`。(2) `printf '\e[?1h'; cat -v` を 1 行で実行し、同じ 3 つを送る | (1) の後 `contents` に `^[[A`、(2) の後 `^[OA` が含まれる。どちらも cat が終了してプロンプトに戻る | REQ-010 | human | smoke | yes |
| AC-014 | 入力ソースを Dvorak にする | `cat -v` に対して `send key "s"`、`send key "enter"` | 物理 S キーを押したときと同じく `o` が出る | REQ-010 | human | 目視 | yes |
| AC-015 | Noa を前面にする。terminal で `printf '\e[?1000h\e[?1006h'; cat -v` を実行中。`NSEvent.mouseLocation` を記録しておく | `send mouse position x 300 y 120` → `send mouse button left button` → 同じく `action release` → `send mouse scroll x 0 y 1` | `contents` に `^[[<0;c;rM`、`^[[<0;c;rm`、`^[[<64;c;rM` が、同じ c;r で含まれる。c>1 かつ r>1（端へのクランプではない）。`NSEvent.mouseLocation` が変わっていない。point 座標からセルへの変換の正しさは AC-032 で確かめる | REQ-011 | human | smoke | yes |
| AC-016 | mouse tracking なし。`seq 1 5000` の出力がある | `send mouse scroll x 0 y 5` | `contents` の先頭行の番号が scroll 前より小さい（物理ホイールと同じく遡る） | REQ-011, REQ-012 | human | smoke | yes |
| AC-017 | fixture Terminal | contents、selection、scrollback の抽出関数を呼ぶ（選択なし／選択あり／300 KiB の履歴／9 MiB の initial input） | contents は `screen_text(visible_rows)` と一致する。selection は選択なしで `""`、選択ありで `selected_text()` と一致する。scrollback は 256 KiB 以下で、履歴全体の末尾と一致する。initial input は 8 MiB に切り詰められる | REQ-012, REQ-004 | machine | ユニットテスト | yes |
| AC-018 | 実機。Secure Keyboard Entry を ON にしている | `input text "printf '%s-%s\\n' あいう AC18\n"` → `contents` | `あいう-AC18` がちょうど 1 回含まれる（spacer なし、隠されない） | REQ-012 | human | smoke | yes |
| AC-019 | ペイン 2 つの tab が前面にある。各ペインで `printf 'AXM-%s\n' <n>` を実行済み（コマンドパレットを開いた状態でもう 1 回） | System Events で `entire contents of window 1 of process "Noa"` から role が `AXTextArea` の要素を集め、`value`、`role description`、`AXIdentifier` を読む | 2 要素が返る。`value` にそれぞれ `AXM-<n>` が含まれる。role description が `terminal`。AXIdentifier が `noa-terminal-<AppleScript の id>` | REQ-014 | human | smoke（Accessibility 権限を持つ端末から実行） | yes |
| AC-020 | AC-019 と同じ | (a) Noa を `activate` し、ペイン 2 に focus して AXFocused を読む。(b) 実行元の端末を前面にしてもう一度読む。(c) split を追加する。(d) ペインを 1 つ閉じる。(e) `tell application "System Events" to set size of window 1 of process "Noa" to {900, 600}` で大きさを変える。(f) 各操作の AE が返ってから 250 ms 後に、全要素の `position` と `size` を読む | (a) true はペイン 2 だけ。(b) すべて false。(c)(d) 要素数が増減する。(e)(f) 全要素の frame が重ならず、合わせて window のコンテンツ領域を埋める（divider の幅は除く）。リサイズ後の新しい大きさに追従している | REQ-014, REQ-015 | human | smoke | yes |
| AC-021 | ペイン 1 つ。`clear; printf 'hello\e[5;1H'; cat` を実行中（cat はカーソルを動かさず何も出力しない）。1 行目の `hello` をダブルクリックで選択済み | `AXSelectedText`、`AXInsertionPointLineNumber`、AE の `selection of t` を読む | `hello`、`4`、`hello` | REQ-012, REQ-014 | human | smoke | yes |
| AC-022 | release ビルド | CFR-001 の (a)(b)(c) を計測する | CFR-001 の目標を満たす | CFR-001 | machine | `#[ignore]` の計測テスト | yes |
| AC-023 | AX クライアントなし、AE なし | idle 計測を main と比べる。差分をレビューする | CFR-002 を満たす | CFR-002 | machine + review | `bench/` の idle ハーネスとレビュー記録 | yes |
| AC-024 | — | 既存の `scripts/applescript-smoke.sh` を変更せずに実行する | 既存の全項目が PASS | REQ-013 | human | smoke | yes |
| AC-025 | at review | AE と AX のハンドラから到達する関数を読む | `recv`、`block_on`、condvar による待ち、winit オブジェクトへの直接アクセスが 0 | CFR-003 | review | レビュアーのチェックリスト | yes |
| AC-026 | `macos-applescript = false` で起動 | `tell app "Noa" to perform action "new_tab"`、`tell app "Noa" to count windows`、AC-019 の手順 | 1 つ目はエラーになり、tab 数が変わらない。2 つ目は Cocoa Scripting が答える（エラーにならない）。AX は AC-019 と同じく取得できる。独自の権限 UI のコードはない（レビュー） | REQ-014, Behavior matrix | human + review | smoke | yes |
| AC-027 | 実機 | `send key "foo"`、`send key "a" modifiers "hyper"`、env `"NOEQ"`、`send mouse position x 1`（y なし）、閉じた terminal の id への `send key` と `contents` | -1700、-1700、-1700、-1715、-1728、-1728。アプリはクラッシュしない | REQ-003, REQ-010, REQ-011 | human | smoke | yes |
| AC-028 | at build | sdef と Rust のコード表の照合テスト。sdef と Appendix B（リポジトリ内のフィクスチャファイルに転記）の照合テスト | Noa の sdef が Appendix B の名前をすべて含み、新しい用語のコードが Appendix B と一致し、すべてのコードが Rust の表にある | REQ-001..012 | machine | `sdef_codes_are_all_in_the_rust_table` と新しい照合テスト | yes |
| AC-029 | tab 2 枚の window 1。シェルは idle（close の確認が出ない状態） | `count tabs of window 1` → `exists tab 3 of window 1` → `exists tab 1 of window 1` → `close tab (tab 2 of window 1)` → 新しい tab を作ってから `close (tab 2 of window 1)` → `close window (window 1)` | 2 → false → true → 1 → 1 → window 数が 1 減る | REQ-002, REQ-013 | human | smoke | yes |
| AC-030 | — | `PendingRegistry` と snapshot の再構築のユニットテスト：pending を挿入した直後に再構築する／実体が現れる／生成失敗で削除する／pending の terminal を読む | 再構築後も pending が残る。実体が現れると消える。失敗で消えて、その後の解決は -1728。pending の読み取りは Behavior matrix のとおり | REQ-008 | machine | ユニットテスト | yes |
| AC-031 | `seq 1 5000` の出力がある terminal。`printf 'SEL-%s\n' X` の出力をマウスで選択済み | `scrollback of t` と `selection of t` を読む | scrollback が `5000` を含み、256 KiB 以下で、`SEL-X` で終わる行を含む。selection が `SEL-X` | REQ-012 | human | smoke | yes |
| AC-032 | — | point 座標→セル変換関数に、セル 8×16 pt・padding 左 4/上 2 pt・80×24 のペインで (300,120)、(0,0)、(10000,10000) を与える | (38, 8)、(1, 1)、(80, 24)。規則は `floor((p − padding) / cell) + 1` を 1..cols / 1..rows にクランプ（境界上の点は右・下のセル） | REQ-011 | machine | ユニットテスト | yes |
| AC-033 | at build | 同梱する sdef を辞書として、`osacompile` で `tell app "Noa" to close tab 2 of window 1`、`close tab (tab 2 of window 1)`、`close (tab 2 of window 1)` をコンパイルし、`osadecompile` で戻す | 旧形式はコンパイルエラーになる（Amendment 2 の breaking change が実在する）。新形式 2 つはコンパイルでき、元の文に戻る | REQ-002, REQ-013 | machine | `scripts/applescript-sdef-check.sh`（sandbox 外で実行） | yes |

## Scope
- **In scope** — REQ-001..015、CFR-001..003。新しい smoke スクリプト `scripts/agent-automation-smoke.sh` と sdef コンパイル確認 `scripts/applescript-sdef-check.sh`。sdef の更新。Appendix B のフィクスチャファイル。CHANGELOG への `count windows` の変化の記載。
- **Out of scope** — `wait for` 系の待機プリミティブ、AX の値変化通知、自前描画 UI（サイドバー、Overview、設定、パレット、モーダル）の AX 化、`whose` フィルタ、VoiceOver 向けの調整、Shortcuts（App Intents）、AppleScript からの設定変更、AX の範囲指定属性（`AXBoundsForRange` など）、ペイン単位のフォントサイズ、AXValue の書き込み、Secure Input 中のマスク処理、AX 公開の config キー、noa-server 側の変更。

## Considered but rejected
- D1 Ghostty 準拠の厳守のみ — 出力の読み返しがなく、エージェントの JTBD を満たせない（独自 suite に隔離する部分だけ採用）。
- D3 AccessKit でアプリ全体を AX 化 — 依存と作業量が最大。自前描画 UI は対象外とした。
- D4 読み取りを noa-server に任せる — 起動とトークンが要り、設定なしの osascript 経路にならない。反証パネルで再検討したが、ユーザーは範囲の維持を選んだ（2026-10-17）。
- D5 読み取り面を AX に一本化 — 呼び出す側に Accessibility 権限が要り、System Events 経由は脆い。
- 範囲の縮小（AX と send mouse を外す、または準拠と send key だけにする）— 反証パネルを受けて提示したが、ユーザーは範囲の維持を選んだ（2026-10-17）。
- `wait for` — C1 の制約で、AE の suspend/resume 機構が必要になる。`contents` のポーリングで代わりになる。
- AX の値変化通知 — VoiceOver は対象外。大量出力中の CPU コストに見合わない。
- ペイン単位のフォントサイズ — 描画とレイアウトへの影響が大きい（ユーザー選択 2026-10-17）。
- US-ANSI 固定のキー変換 — Ghostty と結果がずれる（ユーザー選択 2026-10-17）。
- `close tab`/`close window` を足さない — Ghostty 向けスクリプトとの一致を優先した（ユーザー選択 2026-10-17）。

## Assumption Ledger
| ID | Assumption | Default chosen | Why | Status |
|----|-----------|----------------|-----|--------|
| ASSUME-1 (delegated) | CHALLENGE の選択 | D2 + D1 の suite 隔離 −`wait for` −通知 | 「推奨で」（2026-10-17） | ratified |
| ASSUME-2 | id の型 | integer のまま（Ghostty は text） | 既存スクリプトとの互換 | ratified（2026-10-17） |
| ASSUME-3 | `perform action … on t` の実現方法 | 対象 terminal に focus を移してから dispatch する | Noa の AppCommand は focused window を対象にする構造のため | ratified（2026-10-17） |
| ASSUME-4 | `pid` の意味 | pty の直接の子（シェル）の pid | Ghostty の surface の pid と同じ意味と判断 | ratified（2026-10-17） |
| ASSUME-5 | scroll の符号 | y>0 で上方向（NSEvent と同じ） | Ghostty は NSEvent の delta をそのまま渡している | ratified（2026-10-17） |
| ASSUME-6 | `in` を省略した `new tab` の対象と、生成後の key | AE を受け取った時点の live な front window。作った tab は Noa 内で key になるが、アプリは activate しない | UI の `cmd+t` と同じ挙動。pending を対象にすると、スクリプトの順序で結果が変わる | ratified（2026-10-17） |
| ASSUME-7 | 旧形式 `close tab 2 of window 1` を保てない場合の扱い（AC-033 で発生し、Amendment 2 で breaking change に確定） | AC-033 が失敗したら、実装で回避せずに spec に戻り、REQ-002 と AC-029/033 を breaking change（旧形式は `close (tab 2 of window 1)` で代替）として改訂するかをユーザーが決める | Ghostty との一致を優先した選択（2026-10-17）の帰結 | ratified（2026-10-17） |

## Open Questions / Deferred Decisions
| ID | Question | Blocks | Owner | Resolve by | Impact if wrong |
|----|----------|--------|-------|-----------|-----------------|
| OQ-1 | 実機テストを行う Mac の割り当て（Dvorak 入力ソース、Accessibility と Automation の権限を付与した端末） | nothing — 能力要件は acceptance §3 で確定済み。割り当てだけ | simota | before-ship | 受け入れを実行できない |
| OQ-2 | 巨大な scrollback の reflow 中（10 MB で約 76 ms の実測あり）に AE/AX の読み取りが待つ時間に上限を設けるか（`try_lock` と古い値での応答など） | nothing — CFR-001 は reflow 中を対象外として確定済み。発生頻度が低く、ユーザー操作中に限られるため、今回のビルドには影響しない | simota | deferred | リサイズ中に AX クライアントが読むと、最大で数十 ms 止まる |

## Build-path decision
one-shot autonomous build（elicited 2026-10-17）。machine AC（AC-001/004/012/017/022/028/030/032/033）と `cargo build/test/clippy --workspace` をビルドのゲートにする。human AC は `scripts/agent-automation-smoke.sh` を渡して実機確認待ちとする（OQ-1）。

## Appendix A — send key のキー名（Ghostty `Ghostty.Input.Key` rawValue、main 2026-10-17）

形式：`名前=macOS 仮想キーコード(hex)`。`-` は Mac に対応するキーコードがないキー。配列変換を経ずに、キー名から winit `KeyCode` へ直接対応させて符号化する（L2.4）。

```
backquote=0x32 backslash=0x2a bracketLeft=0x21 bracketRight=0x1e comma=0x2b digit0=0x1d digit1=0x12 
digit2=0x13 digit3=0x14 digit4=0x15 digit5=0x17 digit6=0x16 digit7=0x1a digit8=0x1c digit9=0x19 equal=0x18 
intlBackslash=0x0a intlRo=0x5e intlYen=0x5d a=0x00 b=0x0b c=0x08 d=0x02 e=0x0e f=0x03 g=0x05 h=0x04 i=0x22 
j=0x26 k=0x28 l=0x25 m=0x2e n=0x2d o=0x1f p=0x23 q=0x0c r=0x0f s=0x01 t=0x11 u=0x20 v=0x09 w=0x0d x=0x07 
y=0x10 z=0x06 minus=0x1b period=0x2f quote=0x27 semicolon=0x29 slash=0x2c altLeft=0x3a altRight=0x3d 
backspace=0x33 capsLock=0x39 contextMenu=0x6e controlLeft=0x3b controlRight=0x3e enter=0x24 metaLeft=0x37 
metaRight=0x36 shiftLeft=0x38 shiftRight=0x3c space=0x31 tab=0x30 convert=- kanaMode=- nonConvert=- 
delete=0x75 end=0x77 help=- home=0x73 insert=0x72 pageDown=0x79 pageUp=0x74 arrowDown=0x7d arrowLeft=0x7b 
arrowRight=0x7c arrowUp=0x7e numLock=0x47 numpad0=0x52 numpad1=0x53 numpad2=0x54 numpad3=0x55 numpad4=0x56 
numpad5=0x57 numpad6=0x58 numpad7=0x59 numpad8=0x5b numpad9=0x5c numpadAdd=0x45 numpadBackspace=- 
numpadClear=- numpadClearEntry=- numpadComma=0x5f numpadDecimal=0x41 numpadDivide=0x4b numpadEnter=0x4c 
numpadEqual=0x51 numpadMemoryAdd=- numpadMemoryClear=- numpadMemoryRecall=- numpadMemoryStore=- 
numpadMemorySubtract=- numpadMultiply=0x43 numpadParenLeft=- numpadParenRight=- numpadSubtract=0x4e 
numpadSeparator=- numpadUp=- numpadDown=- numpadRight=- numpadLeft=- numpadBegin=- numpadHome=- numpadEnd=- 
numpadInsert=- numpadDelete=- numpadPageUp=- numpadPageDown=- escape=0x35 f1=0x7a f2=0x78 f3=0x63 f4=0x76 
f5=0x60 f6=0x61 f7=0x62 f8=0x64 f9=0x65 f10=0x6d f11=0x67 f12=0x6f f13=0x69 f14=0x6b f15=0x71 f16=0x6a 
f17=0x40 f18=0x4f f19=0x50 f20=0x5a f21=- f22=- f23=- f24=- f25=- fn=- fnLock=- printScreen=- scrollLock=- 
pause=- browserBack=- browserFavorites=- browserForward=- browserHome=- browserRefresh=- browserSearch=- 
browserStop=- eject=- launchApp1=- launchApp2=- launchMail=- mediaPlayPause=- mediaSelect=- mediaStop=- 
mediaTrackNext=- mediaTrackPrevious=- power=- sleep=- audioVolumeDown=0x49 audioVolumeMute=0x4a 
audioVolumeUp=0x48 wakeUp=- copy=- cut=- paste=-
```

## Appendix B — Ghostty.sdef の用語と四文字コード（main 2026-10-17。新規用語はこのコードをそのまま使う）

```
application=capp; name=pnam; frontmost=pisf; front window=GFWn; version=vers; window=Gwnd; id=ID  ; 
name=pnam; selected tab=GWsT; tab=Gtab; id=ID  ; name=pnam; index=pidx; selected=GTsl; focused terminal=GTfT; 
terminal=Gtrm; id=ID  ; name=pnam; working directory=Gwdr; pid=Gpid; tty=Gtty; surface configuration=GScf; 
font size=GScF; initial working directory=GScD; command=GScC; initial input=GScI; wait after command=GScW; 
environment variables=GScE; split direction=GSpD; right=GSrt; left=GSlf; down=GSdn; up=GSup; input 
action=GIAc; press=GIpr; release=GIrl; mouse button=GMBt; left button=GMlf; right button=GMrt; middle 
button=GMmd; scroll momentum=GSMo; none=SMno; began=SMbg; changed=SMch; ended=SMen; cancelled=SMcn; may 
begin=SMmb; stationary=SMst; perform action=GhstPfAc; on=GonT; new surface configuration=GhstNSCf; from=GScS; 
new window=GhstNWin; with configuration=GNwS; new tab=GhstNTab; in=GNtW; with configuration=GNtS; 
split=GhstSplt; direction=GSpd; with configuration=GSpS; focus=GhstFcus; close=GhstClos; activate 
window=GhstAcWn; select tab=GhstSlTb; close tab=GhstClTb; close window=GhstClWn; input text=GhstInTx; 
to=GItT; send key=GhstSKey; action=GKeA; modifiers=GKeM; to=GKeT; send mouse button=GhstSMBt; action=GMbA; 
modifiers=GMbM; to=GMbT; send mouse position=GhstSMPs; x=GMpX; y=GMpY; modifiers=GMpM; to=GMpT; send mouse 
scroll=GhstSMSc; x=GMsX; y=GMsY; precision=GMsP; momentum=GMsM; to=GMsT; count=corecnte; each=kocl; 
exists=coredoex; quit=aevtquit
```

## Amendment 1 — 実装時の確定（2026-10-17）
- **`command` の実行方式**：L2.4 の「既存の `SpawnTab.command` と同じ（シェルに 1 行入力）」と AC-006(c)（wait なしなら 2 秒以内に消える）が矛盾していた。ユーザーの選択により、Ghostty と同じ「シェルの代わりに起動」に確定する（elicited 2026-10-17）。旧パラメータ `new tab command "x"` も同じ意味になり、従来の「シェルに入力して残る」挙動から変わる。CHANGELOG に記載する。`docs/specs/applescript.md` の AC-4（指定コマンドが新しい surface で実行される）はこの意味でも満たされる。
- **`momentum`**：Ghostty 辞書との互換（AC-028）のため引数は残し、列挙値を検証する（不正なら -1700）が、効果はない。Noa のホイール処理（`event_loop.rs` の `MouseWheel { delta, .. }`）は物理イベントの phase も使わないため、渡し先がない。sdef の説明にも明記した（2026-10-17 レビュー指摘）。
- **`font size` の寿命**：tab に設定したサイズは、その window の scale 変更や実行時のフォントサイズ変更（`cmd+=` など全体操作・config reload）で全体のサイズに戻る。REQ-004 は生成時の反映だけを求めている。
- **`wait after command` の保持中のキー**：修飾キー単独と Cmd ショートカットでは閉じない（出力のコピーやアプリ切り替えのため）。IME の入力（空でない Preedit・Commit）でも閉じ、変換中の入力は破棄する。
- **`close tab`**：対象タブ全体を UI と同じ確認つきの経路（`request_close_tab`）で閉じる。`AppCommand::CloseTab` は分割タブではフォーカス中のペインしか閉じないため使わない。
- **`space` キー**：Space も配列変換を通す（US 配列の Option+Space は U+00A0）。普通の空白になったときだけ名前付きキー Space として扱う。
- **pending の退役**：生成に成功した時点で、その id を registry から外す（短命なコマンドで snapshot の再構築より先に閉じても、pending として残り続けない）。外すのはその id だけで、同じ tab に先に約束した split は残す。
- **UserEvent のペイロード**：L2.1 の「`Eq` を満たす `SurfaceConfigPayload`」の代わりに、noa-server の `IpcAction` と同じ形（共有の待ち表にペイロードを置き、`UserEvent` は request id だけを運ぶ）にする。`Eq` の制約を受けずに f64 などを運べる。観測できる挙動は変わらない。

## Amendment 2 — 旧形式 `close tab N of window M` の扱い（2026-10-17）
- AC-033 の実測で、Ghostty の `close tab` コマンドを辞書に加えると、旧形式 `close tab 2 of window 1` がコンパイルできなくなる（AppleScript が `close tab` コマンド ＋ `2 of window 1` と解釈し、`Can't get 2 of window 1 (-1723)` になる）ことを確認した。`close window 1` と新形式 `close tab (tab 2 of window 1)` / `close (tab 2 of window 1)` は動く。
- ASSUME-7 に従って spec に戻し、ユーザーが breaking change として受け入れた（elicited 2026-10-17）。REQ-002 の「旧形式も同じ結果になる」は削除し、旧形式は CHANGELOG で置き換え先（`close (tab 2 of window 1)` または `close tab (tab 2 of window 1)`）を案内する。AC-029 の旧形式ステップは `close (tab 2 of window 1)` に置き換え、AC-033 の Then は「旧形式はコンパイルエラー、新形式 2 つはコンパイルできる」に改める。

## Quality Gate Record
- 2026-10-17 初回：3 PASS / 5 FAIL（BLOCKING 7）→ 修正。反証パネル 2 名：「問題は実在する」は根拠付きで一部反証 → FRAME に戻して L0 を書き直し（範囲はユーザーが維持を選択）。「AC が要件を証明する」は 18 件の穴 → AC を修正・追加。
- 2 回目：7 件は解決、新たな BLOCKING 4 → 修正。3 回目：AC-032 の期待値 1 件 → 修正。それ以外はすべて解決済み、Completeness PASS。
