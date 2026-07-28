# Scrollback persistence（記録ビュー）

**Status: 実装済み（Stage 0 + Stage 1）。GUI 目視確認は未。**
Q1 / Q2 は回答済み、Q3–Q5 は仮決めのまま実装（§10 決定事項）。実装中に確定した
逸脱・縮小は §12 に記録した。
**Provenance: synthetic (`synthetic: true`)。** 発端は Plea が生成した合成ペルソナ
「陽菜 (27) / 継続ビギナー / 個人 Mac」の要望であり、実ユーザーの検証済みの声では
ない。需要そのものが仮説であることを、優先度判断のときに必ず思い出すこと。

---

## 1. 問題

`docs/specs/session-restore.md` は **トポロジ（ウィンドウ / タブ / split）と
cwd だけ**を復元し、端末の内容は「Ghostty に合わせて復元しない」と定めている。
これは仕様として正しいが、**UI が復元されたことが、内容も復元されたという期待を
生む**という副作用がある。

> タブが全部そのまま戻ってきたから「あ、続きが読める」って思ったんです。なのに
> 開いたら真っ白で。じゃあ何が復元されたんですか? 私が見たかったのは配置じゃなくて
> エラーの文章なんですけど…。

ここには**独立した 2 つの欠陥**がある。混ぜて議論しない:

| # | 欠陥 | 種類 | 対応 |
|---|------|------|------|
| D-1 | 「何が復元され、何が復元されないか」が事前にも事後にも伝わらない | 正直さの欠如（今のコードのバグに近い） | Stage 0 |
| D-2 | 直前の表示内容そのものが失われる | 機能の不在 | Stage 1–3 |

D-1 は D-2 を実装しなくても単独で解消でき、コストが桁違いに小さい。**先に D-1 を
潰す**。

### 受入基準の対応表

| AC（ユーザー視点） | 満たす Stage |
|---|---|
| 終了→起動後に各タブの直前の表示内容が読める | Stage 1（履歴テール） |
| 中身が戻らない仕様なら事前に分かる | **Stage 0** |
| ライブ/記録の区別がつく | Stage 1（記録ビュー） |

---

## 2. 前提と仮定（明示）

- **A-1**: 需要は合成。ペルソナ 1 体ぶんの仮説であり、実ユーザーの要求頻度は未知。
- **A-2**: 陽菜が読みたかった「エラーの文章」は **primary screen** 上にある想定。
  vim / less などの alt screen 内の表示は本提案の対象外（§6 参照）。
- **A-3**: 想定環境は**個人所有の単独ユーザー Mac**。共有マシン・管理端末・
  マルチユーザーの脅威モデルは v1 の対象外。
- **A-4**: マシンをまたいだ復元（同期）は対象外。
- ~~**A-5**: 「直前の表示内容」の粒度は要望文から確定できない~~ → **Q2 で解決。
  履歴全体（末尾テール）が対象。** 1 画面モードは作らない（§10 DEC-2）。

---

## 3. 既存資産

| 資産 | 場所 | 本提案での役割 |
|---|---|---|
| セッション復元 | `docs/specs/session-restore.md`, `crates/noa-app/src/session.rs` | 復元の入れ物。leaf にスナップショット参照を 1 フィールド追加 |
| 非同期書き出しワーカー | `crates/noa-app/src/session_persist.rs` | コアレス済み・アトミック書き込み・Drop で flush。スナップショット書き出しも相乗り |
| ページ化 scrollback | `crates/noa-grid/src/scrollback.rs` | `PackedCell` + ページ単位 `StyleTable` + grapheme table。~~保存形式にそのまま使う~~ → プロセススコープの id を含むため不可（§4.1 実装時の訂正）。実際の形式は `crates/noa-grid/src/snapshot.rs` |
| 行の折返しフラグ | `Row::wrapped` (`cell.rs:151`) | 復元後にウィンドウ幅が変わったとき `screen/reflow.rs` が正しく reflow するために必須。保存対象 |
| テキスト抽出 | `Screen::scrollback_text_tail(max_bytes)` (`screen/text.rs:435`) | Stage 1 のフォールバック実装（色を捨てる版）に流用可 |
| 検索 | `noa-grid/src/search.rs` | 復元領域も scrollback なので**追加実装なしで検索対象になる** |
| 圧縮 | `flate2`（workspace dep、現状 `noa-grid` のみ利用） | deflate。新規依存ゼロ |
| 設定行レジストリ | `crates/noa-app/src/theme_settings/rows.rs` | 新キーの GUI 露出。`SettingsRowKind::COUNT`（現在 33）の**手動 bump が必要**（過去に踏んだ罠） |

---

## 4. 設計方針

### 4.1 何を保存するか — 「バイト列の再生」ではなく「描画済みの行」

| 案 | 内容 | 判定 |
|---|---|---|
| (a) pty バイト列を保存し、起動時に `Stream` へ再投入 | 最も忠実 | **却下**。副作用が再生される（OSC 7 cwd 上書き、タイトル変更、ベル、kitty gfx、alt screen 遷移）。サイズが非有界。復元が遅い |
| (b) **描画済みの行（cells + attrs）を保存** | 決定的、副作用ゼロ、行数で有界 | **採用** |
| (c) プレーンテキストのみ | 最安 | 単独では却下。色が落ちると「赤 = エラー」という初心者の唯一の手掛かりが消える。Stage 1 の縮退パスとしてのみ保持 |

**実装時の訂正:** 当初は「packed 表現をそのまま直列化する」つもりだったが、これは
不可能だった。packed 表現の 2 つの id が*プロセス*スコープだからである ——
`GraphemeId` はグローバルな `LazyLock` interner の索引であり、`HyperlinkId` は
`Terminal::hyperlinks` の索引。どちらもディスクに書くと**次のプロセスでは別の
テキストにデコードされる**。したがって形式は materialize 済みの `Row`/`Cell`
レベルで動き、両者を内容（クラスタのバイト列、リンクの URI）へ解決してからロード
時に id を張り直す。スタイルはスナップショット単位で再 intern し、残りは deflate
が回収する。

### 4.2 保存範囲（scope）

- **primary screen のみ**。alt screen は定義上一時的な表示であり、保存しない。
  終了時に alt screen にいたペインは、その下の primary 履歴が復元される。
- **末尾から**。先頭ではなく最新側を残す（陽菜が読みたいのは「直前」）。
- **ペイン単位**。split の各 leaf が独立したスナップショットを持つ。
- **除外**: scratch terminal（使い捨てポップアップ／`docs/specs/scratch-terminal.md`）
  は常に非保存。remote attach ペイン（`RemotePane`）も v1 では非保存
  （内容の所有者がローカルではないため）。

### 4.3 容量上限

多層のキャップで、暴走を構造的に不可能にする:

| キャップ | 既定値 | 単位 | 目的 |
|---|---|---|---|
| `scrollback-persist-limit` | 1 MiB | **エンコード後・deflate 前**のバイト／ペイン | 保存するテールの上限。**これが唯一の量的つまみ**（1 画面だけ欲しい人は小さくする）。圧縮前で測るのは、キャプチャ側が二度圧縮せずに予算を確定できる唯一の量だから。ディスク上のファイルは必ずこれより小さい |
| `scrollback-persist-total-limit` | 64 MiB | 実ファイルサイズ／全体 | ディスク総量。起動時に実サイズで検査し、超過時は mtime の古い順に破棄 |
| `scrollback-persist-max-age-days` | 7 | 日（整数） | 古い記録の自動失効。`0` で無期限。suffix 構文は noa の config に存在しないので `7d` は診断エラー |

さらに起動時 GC: `session.json` から参照されていないスナップショットファイルは
削除する（孤児回収）。

### 4.4 書き出しのタイミング（session.json と**同じにしない**）

`persist_session()` はトポロジ変更のたびに走る。ペインごとに最大 1 MiB をコピー
する処理を同じ頻度で回すのは論外。スナップショットのトリガは別立てにする:

1. **クリーン終了時**（winit `exiting`）— 主経路。
2. **アイドルチェックポイント** — 出力が **5 秒**止まったら、前回チェックポイント
   以降に出力を出したペインだけを捕捉する。静穏だけを条件にすると*出力が止まらない
   限り一度も走らない* —— まさに末尾を残したい長時間ビルドが該当する —— ので、
   前回から **60 秒**を上限として、洪水中でも必ず一度は走る。クラッシュ耐性はここ
   から来る。
3. **明示コマンド** — コマンドパレット `Checkpoint Scrollback Now`。

キャプチャ（`Arc<Mutex<Terminal>>` の読み取り＋エンコード）はメインスレッド、
ディスク書き込みは専用ワーカー（`scrollback_persist::ScrollbackPersister`）。
キーごとにコアレスするので、10 ペインぶんのチェックポイントが重なっても各ペインの
最新バイト列だけが書かれる。

### 4.5 ファイル形式

`<data-dir>/noa/scrollback/<pane-key>.nsb`（macOS では
`~/Library/Application Support/noa/scrollback/`）。`session.json` の leaf に
`"scrollback": "<pane-key>"` を 1 フィールド追加する（`SESSION_VERSION` は 2 → **3**。2 は remote メタデータで既に使われていた）。pane-key は 16 桁小文字 hex に限定し、`session.json` は編集可能なファイルである以上、パストラバーサルの入力口として扱って検証する。
**バージョン不一致・破損・欠落は「記録なし」に落ちるだけで、起動を阻害しない**
（既存 session.json と同じ規約）。

```
magic   "NOASB\0"        6 B
version u16              = 1
flags   u16              bit0: deflate, bit1: encrypted
cols    u32              保存時のグリッド幅（reflow 判断用）
saved_at u64             Unix 秒（記録ビューのラベル表示に使う）
rows    u32              行数
payload …                deflate(style table ++ link table ++ grapheme table ++ rows)
```

serde は使わない（`noa-config` / `session.rs` の手書きパーサ規約に合わせる）。

### 4.6 暗号化と権限 ← **本提案で最も重い判断**

端末の scrollback は日常的に機微情報を含む: `export AWS_SECRET_ACCESS_KEY=…`、
CLI が echo したトークン、PAT 入りの `git remote` URL、非公開ソース。**今日それは
RAM にしか存在せず、プロセスと共に消える。永続化はこの脅威モデルを変える。**
「便利だから」で既定 ON にしてよい変更ではない。

**v1 のベースライン（Stage 1 から必須）:**

- ディレクトリ `0700`、ファイル `0600`。
- 保存先は既にユーザースコープの Application Support 配下（FileVault の保護下）。
- **Time Machine / iCloud バックアップから除外**する
  （`NSURLIsExcludedFromBackupKey`）。記録がバックアップ経由で外へ漏れる経路を塞ぐ。
- `scrollback-persist = never`（既定）のときは**ディレクトリごと作らない**。

**Stage 3 の追加（オプトイン）:**

- `scrollback-persist-encrypt = true` で AES-256-GCM。鍵は Keychain
  （`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`）。ファイルごとに nonce。
- 鍵が取得できない場合は**復号を諦めて「記録なし」に落ちる**（起動は止めない）。

**既定 OFF は確定（§10 DEC-1）。** noa は Ghostty の忠実クローンであり、Ghostty は
内容を復元しない。既定 ON は観測可能な挙動の逸脱になる。よって本機能は**明示的な
noa 拡張として既定 `never`**。AC-1 の意図は Stage 0 が担保する ——「まさに落胆した
その瞬間に」有効化方法が目に入る導線を作ることで、opt-in が「気づかれない機能」に
ならないようにする。**この導線が無ければ Stage 1 は価値を持たない**（§7 の順序が
固定である理由）。

---

## 5. 記録ビュー（AC-3「ライブ/記録の区別」の答え）

復元された行を、ただ scrollback に流し込むだけでは**過去の出力がライブ出力に見える**。
これは元の欠陥（D-1）を別の場所で再生産することになる。よって復元領域は
**明示的にマークされた読み取り専用の帯**として提示する。

```
  │ $ cargo build
  │ error[E0308]: mismatched types      ← 記録（左ガター: 淡いアクセント罫）
  │   --> src/main.rs:42:9
  ├──── 2026-07-28 14:03 までの記録 ─── ここから下がライブ ────
    $ ▏                                   ← ライブ（ガターなし）
```

- **セパレータ行**: 全幅の淡い罫 + 保存時刻ラベル。pty 由来ではない合成行。
- **左ガター**: 記録領域の各行の**第 0 桁**にアクセント淡色の背景矩形（実装上の
  逸脱については §12 D-2）。減光は**しない**（陽菜が読みたいのはその文字であり、
  読みにくくしては本末転倒）。選択・検索ヒット・カーソルはガターより優先される。
- ~~**「記録」バッジ**~~ → 見送り（§12 D-3）。
- 記録領域は**通常の scrollback**なので、選択・コピー・検索はそのまま効く。
- 記録領域では shell integration のセマンティクス（プロンプトジャンプ再実行等）は
  無効。OSC 8 リンクは有効のまま。
- **破棄手段**: コマンドパレット `Discard restored history`。`clear` でも消える。

---

## 6. 非目標

- 実行中プロセスの復元。復元されるのは**死んだテキスト**であり、シェルは新規。
- ログ機能の代替（`script` / tmux logging の置き換えではない）。
- alt screen の内容（vim / less / TUI の画面）の復元。
- kitty graphics / sixel の復元。Stage 1–2 では画像セルは落とし、
  1 行のプレースホルダに置換する（誤って「画像があったこと」まで消さない）。
- マシン間同期。

---

## 7. 段階的スコープ

| Stage | 内容 | 閉じる AC | 規模の目安 |
|---|---|---|---|
| **0 — 正直さ** | 復元されたペインに 1 行の告知（`layout restored · previous output was not saved · set scrollback-persist = tail to keep it`）。Settings に `Persist Scrollback` 行。**永続化なし** | AC-2 | 実装済み |
| **1 — 履歴テール永続化** | 末尾 `scrollback-persist-limit` バイトぶんの履歴を属性付きで保存／復元（deflate 圧縮）。記録ビュー（セパレータ + ガター + バッジ）。ファイル形式・権限・Time Machine 除外・起動時 GC のベースライン。復元領域は既存 `search.rs` でそのまま検索対象になる | AC-1, AC-3 | 本命。`noa-grid` に serialize/deserialize、`noa-app` に capture/restore/記録ビュー、`noa-config` に 4 キー |
| **2 — 堅牢化** | Keychain 暗号化、総量 LRU、`max-age` 失効、`+scrollback-gc` サブコマンド、ペイン単位の非保存トグル | — | 独立して後追い可 |

**Stage 0 は Stage 1 が無くても価値がある**。逆は成り立たない（Stage 1 だけ入れて
既定 OFF のままだと、誰も気づかない）。順序は固定。

---

## 8. 設定キー（すべて noa 拡張 — Ghostty 非互換。importer で noa-only として扱う）

| キー | 値 | 既定 |
|---|---|---|
| `scrollback-persist` | `never` \| `tail` | `never` |
| `scrollback-persist-limit` | バイト | `1048576` |
| `scrollback-persist-total-limit` | バイト | `67108864` |
| `scrollback-persist-max-age-days` | 整数（日） | `7` |
| `scrollback-persist-encrypt` | bool | `false`（Stage 2・未実装） |

`+show-config` への露出と、`theme_settings/rows.rs` の `SettingsRowKind::COUNT`
bump を忘れないこと。

---

## 9. リスク

| リスク | 影響 | 緩和 |
|---|---|---|
| 機微情報がディスクに残る | 高 | 既定 OFF、`0600`/`0700`、バックアップ除外、Stage 3 で暗号化。有効化時に一度だけ警告を出す |
| 復元時のウィンドウ幅が保存時と違う | 中 | `Row::wrapped` と `cols` を保存し、`screen/reflow.rs` に通す |
| 終了時のキャプチャで quit が遅くなる | 中 | 上限が効くので最悪ケースが有界。`exiting` は既に persister の flush を待っている |
| ディスクを食い潰す | 中 | 4 層キャップ + 起動時 GC |
| 記録がライブと誤認される | 中 | §5 の記録ビュー。これを削ると元の欠陥に戻る |
| Ghostty パリティからの逸脱 | 低 | 既定 OFF + Parity Map に明示的逸脱として記載 |

---

## 10. 決定事項

| # | 決定 | 根拠 | 状態 |
|---|---|---|---|
| **DEC-1** | 既定は `scrollback-persist = never`（opt-in） | Q1 回答。Ghostty パリティ維持。永続化は脅威モデルを変えるので既定 ON にしない | **確定** |
| **DEC-2** | 保存対象は**履歴全体の末尾テール**。1 画面モード（`screen`）は作らない | Q2 回答。量は `scrollback-persist-limit` 一本で制御でき、2 経路を持つ理由がない（`screen` は「小さい `tail`」に過ぎない） | **確定** |
| **DEC-3** | v1 の保護は FileVault + `0700`/`0600` + バックアップ除外。Keychain 暗号化は Stage 2 で opt-in | Q3 未回答につき §4.6 の推奨値を採用。A-3（個人所有の単独ユーザー Mac）が前提 | 仮決め |
| **DEC-4** | 復元された記録は**残す**。ライブ出力が来ても消えない（通常の scrollback と同じ寿命）。破棄はコマンドパレット `Discard restored history` と `clear` から明示的に行う | Q4 未回答。勝手に消える方が驚きが大きく、「読みに戻れる」ことがそもそもの要望 | 仮決め |
| **DEC-5** | 保存単位は**ペイン単位**（split の各 leaf が独立） | Q5 未回答。タブ単位に縮めると「復元されたのに片方の split だけ空」という、元の欠陥（D-1）と同型の非対称が生まれる。総量は §4.3 の 4 層キャップで抑える | 仮決め |

DEC-3 / 4 / 5 は仮決め。実装着手までに異議があれば差し替える。

---

## 11. 手動検証（Stage 1）

0. 既定（キー未設定）のまま起動 → 記録は復元されず、復元されたタブに Stage 0 の
   告知行が出る。ここから有効化方法が読み取れること。
1. `scrollback-persist = tail` を設定し `cargo run -p noa`。数タブ開き、片方で
   ビルドエラーを出したうえで**十分にスクロールさせる**。`cmd+q` → 再起動。
2. そのタブに、記録セパレータ + 左ガター付きでエラー本文が色付きで復元される。
   下にライブプロンプトがある。上にスクロールすると上限ぶんの履歴が遡れる。
   復元領域が検索（`cmd+f`）にヒットする。
3. ウィンドウ幅を変えて再起動 → 折返しが正しく reflow される。
4. `scrollback-persist = never` に戻して再起動 → 記録は復元されず、
   `~/Library/Application Support/noa/scrollback/` が空（または未作成）。
   `Discard restored history` で記録領域だけが消え、ライブ側は無傷。
5. スナップショットファイルを壊す → 正常起動、記録なし、エラーなし。
6. scratch terminal を開いて終了 → そのペインの記録は作られない。
7. `ls -l@` でパーミッション `0600` とバックアップ除外属性を確認。

---

## 12. 実装時に確定した逸脱（提案からの差分）

提案どおりに作れなかった／作らなかった点。理由込みで残す。

| # | 提案 | 実装 | 理由 |
|---|---|---|---|
| **D-1** | packed scrollback 表現をそのまま直列化 | materialize 済み `Row`/`Cell` を独自形式で直列化 | packed 表現の `GraphemeId` / `HyperlinkId` がプロセススコープ。ディスクに書くと次のプロセスで別テキストにデコードされる（§4.1） |
| **D-2** | 記録領域の左に **1px** の縦罫 | 第 0 桁の**セル 1 個ぶん**の背景矩形 | サブセル幅の矩形は新しい GPU プリミティブ（シェーダ／頂点レイアウト変更）を要求する。`CLAUDE.md` の GPU 罠の項が明示的に警告している領域であり、境界の可読性はセル粒度でも損なわれない。1 行あたり矩形 1 個で、選択の背景と同じ既存パスに乗る |
| **D-3** | ビューポートが記録領域に掛かっている間の「記録」バッジ | **未実装** | バッジはネイティブ AppKit オーバーレイ経路（scratch バッジと同じ）で、境界を示すという目的に対して費用が見合わない。セパレータ行は自分自身にラベル（`record · saved <時刻> · live below`）を持ち、記録領域に入るには必ずその行を跨ぐので、区別は成立している。バッジは追認が取れたら follow-up |
| **D-4** | `scrollback-persist-max-age = 7d` | `scrollback-persist-max-age-days = 7` | noa の config には suffix 構文がどこにも無い（`cursor-stop-blinking-after` も `background-image-interval` も素の整数秒）。単位はキー名で表すのが既存の規約 |
| **D-5** | per-pane 上限は「圧縮後バイト」 | 「エンコード後・deflate 前バイト」 | 圧縮後で予算を切るには、切って圧縮して溢れたらやり直す必要がある。圧縮前は一度で確定でき、圧縮は必ず約束より小さくする方向にしか効かない |
| **D-6** | `SESSION_VERSION` を 2 へ | **3** へ | 2 は remote ペインのメタデータで既に使用済みだった |
| **D-8** | （提案に無し） | 記録の破棄は既存 scrollback を作り直して実装 | ページは packed 後は不変で、ページ単位でしか evict されない。`erase_display(Scrollback)` はライブ履歴まで消してしまうため、残存行を materialize して詰め直す O(n) 経路を足した。ユーザー起点の稀なコマンドなので、bulk-output のホットパスには触っていない |

| **D-8** | 記録領域のガター範囲は常に有効 | 座標が再採番されたら**マーキングだけ捨てる** | `record_rows`/`annotation_row` は絶対行のキャッシュで、列数 reflow と scrollback clear は空間ごと振り直す。`Terminal::grid_coordinate_generation` はまさにその2つで bump し、通常の eviction では bump しない（既存テストが保証）ので、これを突き合わせるのが完全な陳腐化判定になる。不一致時はガターと `skip_row` と Discard を止める — 誤った行を消すより、印が消えるほうが遥かに軽い。境界そのものはセパレータ**行**（=内容）なので reflow を跨いで残り、AC-3 は維持される |
| **D-9** | （提案に無し） | 永続化リンクは 4 KiB で打ち切る | OSC 8 のペイロード上限はパーサの 12 MiB。リンクは side table に載るため行単位の予算計算からは見えず、**URI 1本でスナップショット全体の上限を突破**できた。これより長い URL は誰も踏まないので、セルの文字は残しリンクだけ落とす |

### 未実装（Stage 2 送り）

- `scrollback-persist-encrypt`（Keychain + AES-256-GCM）。v1 のベースラインは
  FileVault + `0600`/`0700` + バックアップ除外のまま（DEC-3）。
- kitty graphics / sixel のプレースホルダ置換（現状は画像セルが素の文字として落ちる）。
- 記録バッジ（D-3）。

### GUI 目視確認が必要な項目

自動テストで押さえられていない部分。実機で見ること:

1. 記録セパレータの見え方（罫の連続性・時刻の可読性・テーマ light/dark 双方）。
2. 第 0 桁ガターの色が、記録領域の文字を読みにくくしていないこと（特に light テーマ）。
3. 復元直後のビューポート位置（記録の末尾＝ライブ直前が見えていること）。
4. Settings の `Persist Scrollback` 行の表示とバッジ種別。
5. 幅を変えて再起動したときの reflow の見え方（§11 手順 3）。
