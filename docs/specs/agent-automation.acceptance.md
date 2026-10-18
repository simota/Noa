# Acceptance Test Specification — エージェント自動化サーフェス強化

| | |
|---|---|
| slug | agent-automation |
| spec source | `docs/specs/agent-automation.md` as-of 2026-10-17 |
| status | locked |
| owner | simota |
| as-of | 2026-10-17 |
| review trigger | spec の変更 |

## 1. Test scope
- **In scope** — AC-001..033（AppleScript の新しい動詞・プロパティ、Noa Extensions、AX 公開、CFR-001..003）。
- **Out of scope** — spec の Out of scope 一覧の項目。既存 `applescript.md` の AC はここでは扱わない（AC-024 で回帰のみ確認）。
- **Not proven by this suite** — US 配列と Dvorak 以外の配列での `send key`（JIS かな入力・IME 入力ソース）。Claude 以外の computer use クライアントからの読み取り。マルチモニタでの AXFrame。巨大な scrollback の reflow 中の読み取りの待ち時間（spec OQ-2）。

## 2. Entry & exit criteria
| | Criterion |
|---|-----------|
| **Entry** | spec が `locked`。対象コミットから `scripts/bundle-macos.sh` で作った署名済み Noa.app を `/Applications` 以外の場所から起動している。§3 の能力を満たしている |
| **Exit** | must-have の TC がすべて PASS。§6 の手順の結果が記録されている。S1/S2 の未解決欠陥がない |

## 3. Environment
| Item | Value | How to obtain / set |
|------|-------|---------------------|
| Environment | local の Apple Silicon Mac、macOS 13 以上、画面 1 枚 | OQ-1（割り当ては TBD(simota)） |
| Build under test | 対象コミットの release ビルドを bundle したもの | `scripts/bundle-macos.sh` |
| Data state | 新規起動。session restore の対象がない状態 | 起動前に `~/Library/Application Support/noa` のセッションファイルを退避する |
| Accounts & roles | 実行用ターミナル（Ghostty か Terminal.app）に TCC の Automation（→ Noa、System Events）と Accessibility を付与 | システム設定 › プライバシーとセキュリティ |
| Feature flags | `macos-applescript = true`（TC-026 のみ false） | Noa の config |
| External dependencies | なし | — |
| Clock / timezone / locale | 入力ソースは U.S.（TC-014 のみ Dvorak） | システム設定 › キーボード |

## 4. Test data
| Fixture | Purpose | Source | Reset procedure | Contains personal data |
|---------|---------|--------|-----------------|------------------------|
| snapshot fixture | AC-001 | ユニットテスト内で構築 | テストごとに生成 | no |
| Terminal fixture（200×60 全角／300 KiB scrollback） | AC-017, AC-022 | ユニットテスト内で `Terminal` に書き込む | テストごとに生成 | no |
| `seq 1 5000` の出力 | AC-016・AC-031 の scrollback | terminal で実行 | 該当 terminal を閉じる | no |

## 5. Test cases
smoke は `scripts/agent-automation-smoke.sh`（新規）にまとめ、各 TC を `PASS <TC-id>` / `FAIL <TC-id>` の行として出力する。既存の `scripts/applescript-smoke.sh` は TC-024 のため変更しない。smoke はスクリプト側で絶対パスを決めてから terminal に渡す（Noa 側の `$TMPDIR` には依存しない）。マーカーは spec L3 の規則に従う。

| ID | Verifies | Title | Precondition | Steps | Expected result | Mode | Automation ref | Must-have | Priority | Result |
|----|----------|-------|--------------|-------|-----------------|------|----------------|-----------|----------|--------|
| TC-001 | AC-001 | 参照の解決 | — | `cargo test -p noa-app applescript_resolve` | AC-001 の Then がすべて assert される | machine | unit | yes | CRITICAL | NOT_RUN |
| TC-002 | AC-002 | pid/tty | terminal 1 つ | `pid`/`tty` を取得。smoke 側で `ps -o pid=,comm= -t ttysNNN` を実行 | シェルの行の pid と tty が一致 | human | smoke | yes | HIGH | NOT_RUN |
| TC-003 | AC-003 | focused terminal の e2e | AC-003 の Given | AC-003 の When | 同じ id | human | smoke | yes | HIGH | NOT_RUN |
| TC-004 | AC-004 | record の解析 | — | `cargo test -p noa-app surface_configuration` | AC-004 の Then が assert される | machine | unit | yes | HIGH | NOT_RUN |
| TC-005 | AC-005 | 3 動詞の configuration | — | AC-005 の When を動詞ごとに実行し、2 秒待って各 `contents` | 3 つとも `CFG-/usr-bar` を含む | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-006 | AC-006 | wait after command | — | AC-006 の (a)〜(c) を順に実行。(d) は手で 1 キー押す | AC-006 の Then | human | smoke + 目視 | yes | HIGH | NOT_RUN |
| TC-007 | AC-007 | font size | 既定 14pt の window がある | AC-007 の When を実行し、スクリーンショットを撮る | 行の高さの比が 1.8〜2.2、split は親と同じ | human | — | yes | MEDIUM | NOT_RUN |
| TC-008 | AC-008 | new tab in | window 2 個 | `count tabs` を両 window で取る → `new tab in window 2` → 1 秒待つ → 再度 count | window 2 が +1、window 1 は不変 | human | smoke | yes | HIGH | NOT_RUN |
| TC-009 | AC-009 | split の対象と 2 形式 | AC-009 の Given | AC-009 の When。各操作の 250 ms 後に AX frame を読む | AC-009 の Then | human | smoke | yes | HIGH | NOT_RUN |
| TC-010 | AC-010 | 返り値をすぐ使う | Noa を `activate` 済み、window 1 個 | AC-010 の When を 1 本の osascript で実行 | AC-010 の Then | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-011 | AC-011 | perform action on | AC-011 の Given | AC-011 の When。250 ms 後に AX frame を読む | AC-011 の Then | human | smoke | yes | HIGH | NOT_RUN |
| TC-012 | AC-012 | キーの解析と符号化 | — | `cargo test -p noa-app applescript_send_key` | AC-012 の Then が assert される | machine | unit | yes | HIGH | NOT_RUN |
| TC-013 | AC-013 | send key の e2e と DECCKM | US 配列 | AC-013 の (1)(2)。各段階で 1 秒待って `contents` | `^[[A`、`^[OA`、プロンプト復帰 | human | smoke | yes | HIGH | NOT_RUN |
| TC-014 | AC-014 | 配列での変換 | 入力ソースが Dvorak | AC-014 の When → `contents` | 最終行が `o` | human | — | yes | MEDIUM | NOT_RUN |
| TC-015 | AC-015 | mouse tracking あり | AC-015 の Given | AC-015 の When → 1 秒待つ → `contents` とカーソル位置 | 3 つの列が同じ c;r（c>1, r>1）で含まれ、カーソル位置は不変 | human | smoke | yes | HIGH | NOT_RUN |
| TC-016 | AC-016 | mouse tracking なし | `seq 1 5000` 実行済み | `contents` を保存 → AC-016 の When → `contents` | 2 回目の先頭行の番号が小さい | human | smoke | yes | HIGH | NOT_RUN |
| TC-017 | AC-017 | テキスト抽出 | — | `cargo test -p noa-app applescript_text` | AC-017 の Then が assert される | machine | unit | yes | HIGH | NOT_RUN |
| TC-018 | AC-018 | 全角・secure input | Noa のメニューで Secure Keyboard Entry を ON | AC-018 の When | `あいう-AC18` を 1 回だけ含む | human | smoke | yes | HIGH | NOT_RUN |
| TC-019 | AC-019 | AX ツリー | AC-019 の Given | AC-019 の When を、パレットを閉じた状態と開いた状態（`cmd+shift+p`）の 2 回 | どちらも AC-019 の Then | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-020 | AC-020 | AX focus・frame・追従 | AC-019 の Given | AC-020 の (a)〜(f) | AC-020 の Then | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-021 | AC-021 | 選択と挿入点 | AC-021 の Given | AC-021 の When | `hello`、`4`、`hello` | human | smoke | yes | MEDIUM | NOT_RUN |
| TC-024 | AC-024 | 既存の smoke の回帰 | — | 変更前の smoke の項目を実行 | 既存の全項目が PASS | human | smoke | yes | CRITICAL | NOT_RUN |
| TC-026 | AC-026 | 無効化 | config に `macos-applescript = false` を入れて再起動 | AC-026 の When。差分を grep して権限 UI のコードがないことを確認 | AC-026 の Then | human | smoke + review | yes | HIGH | NOT_RUN |
| TC-027 | AC-027 | 不正な入力 | terminal 1 つ。別の terminal を閉じて、その id を記録しておく | AC-027 の When を 1 つずつ実行 | AC-027 の Then の順にエラー番号が返る。Noa のプロセスが残っている | human | smoke | yes | HIGH | NOT_RUN |
| TC-028 | AC-028 | sdef の照合 | Appendix B を `crates/noa-app/tests/fixtures/ghostty-sdef-terms.txt` に転記済み | `cargo test -p noa-app sdef` | 照合テストが 2 本とも成功 | machine | unit | yes | HIGH | NOT_RUN |
| TC-029 | AC-029 | close tab/window・count・exists | AC-029 の Given | AC-029 の When | AC-029 の Then | human | smoke | yes | HIGH | NOT_RUN |
| TC-030 | AC-030 | pending registry | — | `cargo test -p noa-app applescript_pending` | AC-030 の Then が assert される | machine | unit | yes | CRITICAL | NOT_RUN |
| TC-031 | AC-031 | scrollback と selection の e2e | AC-031 の Given | AC-031 の When | AC-031 の Then | human | smoke | yes | HIGH | NOT_RUN |
| TC-032 | AC-032 | point→セル変換 | — | `cargo test -p noa-app applescript_mouse_cell` | AC-032 の Then が assert される | machine | unit | yes | HIGH | NOT_RUN |
| TC-033 | AC-033 | close 形式のコンパイル | bundle 済みの Noa.app | `scripts/applescript-sdef-check.sh target/release/Noa.app`（sandbox 外） | AC-033 の Then | machine | script | yes | HIGH | NOT_RUN |

## 6. Non-functional procedures (`CFR-n`)
| ID | Verifies | CFR | Measure / criterion | Required outcome (from spec) | Procedure | Tool / observer | Conditions / sample | Observed result | Result |
|----|----------|-----|---------------------|------------------------------|-----------|-----------------|---------------------|-----------------|--------|
| TC-022 | AC-022 | CFR-001 | (a)(b)(c) の p95/p99 | see spec CFR-001 | `cargo test -p noa-app --release applescript_read_perf -- --ignored --nocapture` | テストの出力 | spec CFR-001 の (a)(b)(c) の条件、各 1000 回。計測スレッドは main スレッドと同じ user-interactive QoS（テスト内で設定） | — | NOT_RUN |
| TC-023 | AC-023 | CFR-002 | idle RSS の差分と、新しいタイマー・スレッドの数 | see spec CFR-002 | main と対象ビルドで `bench/` の idle 計測を各 3 回 → 中央値を比較。diff の `spawn`/`Timer`/`NSTimer` を grep | ハーネス + レビュアー | 新規起動から 60 秒後、ペイン 1 つ | — | NOT_RUN |
| TC-025 | AC-025 | CFR-003 | ハンドラ内の待ち | see spec CFR-003 | AE と AX のハンドラから到達する関数をレビューし、`recv`/`block_on`/`wait`/winit の型を確認 | レビュアー | 対象コミットの diff | — | NOT_RUN |

## 7. Coverage matrix
| AC | Must-have | Covered by | Mode | Gap |
|----|-----------|-----------|------|-----|
| AC-001 | yes | TC-001 | machine | — |
| AC-002 | yes | TC-002 | human | — |
| AC-003 | yes | TC-003 | human | — |
| AC-004 | yes | TC-004 | machine | — |
| AC-005 | yes | TC-005 | human | — |
| AC-006 | yes | TC-006 | human | — |
| AC-007 | yes | TC-007 | human | — |
| AC-008 | yes | TC-008 | human | — |
| AC-009 | yes | TC-009 | human | — |
| AC-010 | yes | TC-010 | human | — |
| AC-011 | yes | TC-011 | human | — |
| AC-012 | yes | TC-012 | machine | — |
| AC-013 | yes | TC-013 | human | — |
| AC-014 | yes | TC-014 | human | — |
| AC-015 | yes | TC-015 | human | — |
| AC-016 | yes | TC-016 | human | — |
| AC-017 | yes | TC-017 | machine | — |
| AC-018 | yes | TC-018 | human | — |
| AC-019 | yes | TC-019 | human | — |
| AC-020 | yes | TC-020 | human | — |
| AC-021 | yes | TC-021 | human | — |
| AC-022 | yes | TC-022 | machine | — |
| AC-023 | yes | TC-023 | machine + review | — |
| AC-024 | yes | TC-024 | human | — |
| AC-025 | yes | TC-025 | review | — |
| AC-026 | yes | TC-026 | human | — |
| AC-027 | yes | TC-027 | human | — |
| AC-028 | yes | TC-028 | machine | — |
| AC-029 | yes | TC-029 | human | — |
| AC-030 | yes | TC-030 | machine | — |
| AC-031 | yes | TC-031 | human | — |
| AC-032 | yes | TC-032 | machine | — |
| AC-033 | yes | TC-033 | machine | — |

## 8. Defect handling
| Severity | Definition | Blocks exit |
|----------|-----------|-------------|
| S1 | must-have の AC を満たさない、またはクラッシュ・ハングする | yes |
| S2 | must-have の AC を回避策を使わないと満たせない | yes |
| S3 | must-have 以外の AC を満たさない | no — 記録する |
| S4 | 表示・文言 | no — 記録する |

must-have の TC が失敗したら、ビルドの欠陥か、AC が書かれたままでは作れないかを判定する。作れない場合は `spec-dialogue agent-automation` に戻す。

## 9. Execution record
| Run | Date | Build | Executed by | Passed | Failed | Blocked | Skipped | Defects raised |
|-----|------|-------|-------------|--------|--------|---------|---------|----------------|
| 1 | | | | | | | | |

## 10. Residual risk
- smoke の大半は、署名済み bundle と TCC の権限が必要な human 手順。CI では回らない。
- AX は System Events 経由で確認する。Claude computer use 本体からの読み取りは、この suite では確かめない。
- TC-007 と TC-014 は目視判定。

## 11. Sign-off
| | |
|---|---|
| accepted by | |
| date | |
| verdict | |
