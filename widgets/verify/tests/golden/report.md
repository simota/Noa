> 証跡のリンク先: `/golden/repo/.noa/verify/ver-golden`（このマシン上で、このフォルダがある間だけ開けます）

# 検証結果

- 対象: feat/cwd … main（3 ファイル）（Git 差分）
- 場所: `/golden/repo`
- 比較: main `aaaaaaaaaa` … feat/cwd `bbbbbbbbbb`（merge-base `cccccccccc`）
- ファイル: 3（+33 −9）
- 依頼: Verify that new tabs inherit the cwd; it must stay "fast" & <safe>.
- 仕様: `docs/specs/cwd.md`（12 行）
- 隔離: 個別 worktree
- パネル:
  - claude → 仕様適合（claude · opus · high）
  - codex → 反証（codex）
  - agy → Odd locales & ve…（agy）
  - 判定役: claude
- ラウンド: 2 / 2・終了

## 総合判定

**条件付き合格**

Every must criterion holds; the slow cold start (should) and the nested-link defect remain.

止めているもの:
- **AC-3** Opening a tab stays under 50 ms
- **D1** A symlinked cwd is resolved to its target's parent

## 受け入れ基準

| ID | 優先度 | 基準 | 判定 | claude | codex | agy |
|---|---|---|---|---|---|---|
| AC-1 | 必須 | New tabs open in the focused pane's cwd | 検証済 | ✓↻ | ✗↻ | ✓↻ |
| AC-2 | 必須 | A cwd that no longer exists falls back to $HOME | 検証済 | ✓↻ | ✓≠ | ✓↻ |
| AC-3 | 推奨 | Opening a tab stays under 50 ms | 不合格 | ✓ | ⊘ | ✗ |
| AC-5 | 推奨 | Quitting keeps every tab's cwd for the next launch | 対象外 | – | ✓≠ | ⊘ |

### AC-1 New tabs open in the focused pane's cwd

- 判定: 検証済（R2）— claude's re-run matched and the person's screenshot shows /tmp; codex's failure did not reproduce.
  - `claude/r1/e1` コマンド `cargo test -p app cwd_`（.） · 終了コード 0 · 再実行 一致（0） · digest `3145b15b544f640b` · 1029 バイト · `evidence/r1/claude/logs/1.log`
  - `user/1` 画像（PNG）`Screenshot 2026-11-24.png`「pwd shows /tmp in the new tab」（あなたが添付） · digest `d08a6e548ee8ef4f` · 1019 バイト · `evidence/user/1.png`
- claude（R1）: 成功 — pwd shows /tmp.
- codex（R1）: 失敗 — With a symlinked cwd the tab opens in $HOME.
- agy（R1）: 成功
- スクショ・動画:
  - ![pwd in the new tab shows /tmp](/golden/repo/.noa/verify/ver-golden/evidence/r1/claude/shots/new%20tab.png)
  - ▶ [cd into a symlink, Cmd-T, pwd](/golden/repo/.noa/verify/ver-golden/evidence/r1/codex/video/symlink.mov)
    - 00:04 Cmd-T pressed
    - 00:06.5 the new tab shows $HOME
  - ![pwd shows /tmp in the new tab](/golden/repo/.noa/verify/ver-golden/evidence/user/1.png)（あなたが添付）

### AC-2 A cwd that no longer exists falls back to $HOME

- 判定: 検証済（R2）— claude's test re-ran with the same status.
  - `claude/r1/e1` コマンド `cargo test -p app cwd_`（.） · 終了コード 0 · 再実行 一致（0） · digest `3145b15b544f640b` · 1029 バイト · `evidence/r1/claude/logs/1.log`
- claude（R1）: 成功
- codex（R1）: 成功
- agy（R1）: 成功

### AC-3 Opening a tab stays under 50 ms

- 判定: 不合格（R2）— agy's cold-start benchmark takes 72 ms, over the limit.
  - `agy/r2/e1` コマンド `./bench open-tab --cold`（bench） · 終了コード 1 · digest `e1baf61110e31231` · 1026 バイト · `evidence/r2/agy/logs/1.log`
- claude（R1）: 成功 — 31 ms median.
- codex（R1）: 実行不能 — No display in the sandbox.
- agy（R2）: 失敗 — A cold start takes 72 ms.

### AC-5 Quitting keeps every tab's cwd for the next launch

- 判定: 対象外（R2）— Session restore on quit is not part of this change.
- claude（R1）: 未実施 — Out of my strategy's reach this round.
- codex（R1）: 成功
- agy（R1）: 実行不能 — Quitting kills my pane.

## 不具合

### D1 [高] A symlinked cwd is resolved to its target's parent

- 反証 · 合意 · 支持 2 · 未表明 1（多数）· 確信度 中
- 場所: `src/cwd.rs:4`
- 出典: codex/1
- 判断: claude reproduced it with a nested link.
- 再現手順: 1. ln -s /tmp /tmp/l 2. cd /tmp/l 3. Cmd-T 4. pwd
- 証跡: codex/r1/e1

## 追試

- R2 claude → codex/AC-1: 再現せず — cwd_symlink passes in my worktree.
- R2 claude → D1: 再現 — A link inside a link resolves wrong.
- R2 agy → codex/AC-5: 異議 — cwd_gone does not exercise quitting at all.

## 証跡一覧

再実行は各検証者自身のペインで、その検証者のエージェントがウィジェットの書いたスクリプトを起動して行いました。同じ環境で同じ終了コードが出るかの確認で、別の検証者による独立した追試ではありません（追試は「## 追試」）。

- `codex/r1/e3` コマンド `cargo test -p app cwd_gone`（.） · 終了コード 0 · 再実行 不一致（1） · digest `e789da942a51bc14` · 1028 バイト · `evidence/r1/codex/logs/3.log`
  - 再実行のログ: digest `40944dff7b077ac1` · 1029 バイト · `evidence/r1/codex.rerun/2.log`
- `claude/r1/e1` コマンド `cargo test -p app cwd_`（.） · 終了コード 0 · 再実行 一致（0） · digest `3145b15b544f640b` · 1029 バイト · `evidence/r1/claude/logs/1.log`
  - 再実行のログ: digest `419e46c259bc2dcb` · 1030 バイト · `evidence/r1/claude.rerun/1.log`
- `claude/r1/e2` 画像（PNG）`shots/new tab.png`「pwd in the new tab shows /tmp」 · digest `56af731184d382ca` · 1036 バイト · `evidence/r1/claude/shots/new tab.png`
- `claude/r1/e3` コマンド `for i in $(seq 20); do` …（bench） · 終了コード 0 · digest `233e39392ce9da75` · 1029 バイト · `evidence/r1/claude/logs/3.log`
- `codex/r1/e1` コマンド `cargo test -p app cwd_symlink -- --exact`（.） · 終了コード 101 · 再実行 一致（101） · digest `d91652720290f86e` · 1028 バイト · `evidence/r1/codex/logs/1.log`
  - 再実行のログ: digest `29e17307b368c040` · 1029 バイト · `evidence/r1/codex.rerun/1.log`
- `codex/r1/e2` 動画（MOV）`video/symlink.mov`「cd into a symlink, Cmd-T, pwd」 · digest `7fbb867bbedf39d4` · 1035 バイト · `evidence/r1/codex/video/symlink.mov`
- `agy/r1/e1` コマンド `LANG=ja_JP.UTF-8 cargo test -p app cwd_`（.） · 終了コード 0 · 再実行 一致（0） · digest `419d5086c11d4da2` · 1026 バイト · `evidence/r1/agy/logs/1.log`
  - 再実行のログ: digest `b44e0d77d18712ac` · 1027 バイト · `evidence/r1/agy.rerun/1.log`
- `agy/r1/e2` コマンド `./bench open-tab --long-path`（bench） · 終了コード 0 · 保存なし
- `agy/r1/o1` 観察（未裏付け）: The pane closed when I pressed Cmd-Q.
- `user/1` 画像（PNG）`Screenshot 2026-11-24.png`「pwd shows /tmp in the new tab」（あなたが添付） · digest `d08a6e548ee8ef4f` · 1019 バイト · `evidence/user/1.png`
- `claude/r2/e1` コマンド `cargo test -p app cwd_symlink -- --exact`（.） · 終了コード 0 · 再実行 一致（0） · digest `b5c829dabcf4f722` · 1029 バイト · `evidence/r2/claude/logs/1.log`
  - 再実行のログ: digest `f0c6d72cc0fc172c` · 1030 バイト · `evidence/r2/claude.rerun/1.log`
- `claude/r2/e2` コマンド `./scripts/nested-link.sh`（.） · 終了コード 1 · digest `034760f3b5757f7f` · 1029 バイト · `evidence/r2/claude/logs/2.log`
- `agy/r2/e1` コマンド `./bench open-tab --cold`（bench） · 終了コード 1 · digest `e1baf61110e31231` · 1026 バイト · `evidence/r2/agy/logs/1.log`

## 基準の変更

- 編集: AC-3 Opening a tab stays under 50 ms
- 追加: AC-5 Quitting keeps every tab's cwd for the next launch
- 削除: AC-4 The change is documented（request）
- 対象外と判定: AC-5 Quitting keeps every tab's cwd for the next launch — Session restore on quit is not part of this change.

## 検証の欠落

- AC-3 を codex が実行できませんでした（R1）: No display in the sandbox.
- AC-5 を agy が実行できませんでした（R1）: Quitting kills my pane.
- 取り込み前に消えた証跡: `agy/r1/e2`（logs/2.log）
- 観察だけを根拠にした結果: AC-5（agy R1・実行不能）

## ラウンド記録

- R1（独立検証）: AC-1 fails for a symlinked cwd; AC-2 holds; AC-3 rests on one stored benchmark; AC-5 is untested.
- R2（追試）: codex's AC-1 failure did not reproduce; the nested-link defect did; a cold start breaks AC-3. AC-5 does not apply to this change.
