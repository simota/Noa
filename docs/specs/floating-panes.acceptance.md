# フローティングペインの受け入れテスト仕様

| | |
|---|---|
| slug | floating-panes |
| spec source | `docs/specs/floating-panes.md` as-of 2026-11-10 |
| status | locked |
| owner | simota |
| reader | 実装者と受け入れ担当。スイートを実行し、ビルドを受け入れるか決める |
| as-of | 2026-11-10 |
| review trigger | 仕様が変わったとき |

## 1. Test scope

- In scope: 仕様 L3 の AC-001〜AC-044。
- Out of scope: 仕様の Out of scope に挙げた項目。どこでもテストしない。
- Not proven by this suite: ドラッグ中の描画の滑らかさ。上限 8 枚を使い切ったときの資源消費。マルチモニタと HiDPI の混在。IME 入力（分割ペインと同じ入力経路を通る前提で、個別には確かめない）。

## 2. Entry & exit criteria

| | Criterion |
|---|-----------|
| Entry | 仕様が `locked`。コミットで識別できるビルドが §3 の環境にある |
| Exit | must-have の TC が macOS と Linux の両方で全 PASS。§8 の S1・S2 が残っていない。must-have でない失敗はすべて欠陥として記録済み |

## 3. Environment

| Item | Value | How to obtain / set |
|------|-------|---------------------|
| Environment | ローカル。(a) macOS Apple Silicon、(b) Linux。どちらもディスプレイと GPU があり、pty を開ける（サンドボックス外）こと | (b) の具体構成は `TBD(simota)`（仕様 OQ-3） |
| Build under test | 実行時のコミット | `git rev-parse HEAD` を §9 に記録 |
| Data state | 空。セッションファイルなしで起動 | §4 F-0 |
| Accounts & roles | N/A（ローカル単一利用者） | — |
| Feature flags | N/A（フラグなし） | — |
| External dependencies | N/A（外部依存なし。エージェント状態は OSC を手で出して再現する） | §4 F-2 |
| Clock / timezone / locale | N/A（時刻に依存する挙動なし） | — |

## 4. Test data

| Fixture | Purpose | Source | Reset procedure | Contains personal data |
|---------|---------|--------|-----------------|------------------------|
| F-0 | 空の起動状態 | noa のセッションファイルと scrollback ディレクトリを退避して起動 | テスト後に退避したものを戻す | no |
| F-1 | 幾何の単体テスト用の固定値 | 仕様 L3 冒頭の F-1 をテストコード内の定数にする | N/A（状態を持たない） | no |
| F-2a | `AgentState::Permission` の再現 | フロート内で `printf '\033]7501;state=blocked:kind=permission:app=test\033\\'` | フロートを閉じる | no |
| F-2b | `AgentState::Input` の再現 | フロート内で `printf '\033]7501;state=blocked:app=test\033\\'` | フロートを閉じる | no |
| F-2c | `AgentState::Error` の再現 | フロート内で `printf '\033]7501;state=blocked:kind=auth:app=test\033\\'` | フロートを閉じる | no |
| F-3 | 壊れたセッションファイル: フロート記録2件、うち1件の origin の x が `NaN` | テストコード内で生成 | N/A（一時ファイル） | no |

## 5. Test cases

machine モードのケースは自動テストで実装し、テスト名に TC の ID を含める。ファイルパスは `TBD(実装者)`（仕様 OQ-2）。Automation ref 列の `OQ-2` はこれを指す。TC の番号は検証する AC と同じ番号にしてある。

| ID | Verifies | Title | Precondition | Steps | Expected result | Mode | Automation ref | Must-have | Priority | Result |
|----|----------|-------|--------------|-------|-----------------|------|----------------|-----------|----------|--------|
| TC-001 | AC-001 | 作成で分割が変わらない | F-1（タブ1は分割ペイン1枚、タブ2は2枚）、フロート0枚 | タブ1で `float.new` | 全分割ペインの矩形不変。フロート (200,100)-(1000,600)、80×24、フォーカスあり、cwd 一致 | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-002 | AC-002 | 2・3枚目のずらし | TC-001 の直後 | `float.new` ×2 | (180,80)-(980,580)、(160,60)-(960,560)。別に、1枚目をタブアンカーにしてから `float.new` → (180,80)-(980,580)。A/B 分割で B 上の (600,100) 300×200 px だけがあるとき A で `float.new` → (50,100) 450×500 px（ずれなし） | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-003 | AC-003 | 小さいアンカーでの縮小 | アンカー 600×300 px | `float.new` | 540×270 px、54×12 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-005 | AC-005 | 移動のクランプ | F-1、フロート 400×300 px を (100,100) | (a) 帯を (+900,+900) (b) 初期状態から (−900,−900) | (a) 左上 (600,300) (b) (0,0) | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-006 | AC-006 | タブ切替時の表示と pty 継続 | タブ2枚、3種のフロート各1枚（ペインアンカーはタブ1） | タブ2を表示し、非表示2枚の pty に `x` を届ける | 表示はウィンドウアンカーのみ・位置不変。非表示2枚の端末に `x` が入っている | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-007 | AC-007 | ズーム時の非表示 | F-1、A にフロート | B をズーム、解除 | ズーム中は非表示、解除後に元の位置 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-008 | AC-008 | アンカー切替 | F-1、ペインアンカー(A) 300×200 px を (100,100) | `float.cycle-anchor`、帯を (+500,0)、`float.cycle-anchor`、アイコンをクリック | タブ→ウィンドウ→ペイン(B)。切替の各回で画面矩形不変、直後は新しい層の最前面 | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-009 | AC-009 | 比率追従と下限 | F-1、origin (0.1,0.1) size (0.2,0.2)、帯をドラッグ中 | アンカーを 500×300 px、次に 1000×600 px | (50,30) 200×80 px、次に (100,60) 200×120 px。ドラッグ状態は継続 | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-010 | AC-010 | 下限未満で非表示 | フロート1枚 | アンカーを 150×60 px、次に 1000×600 px | 非表示かつ存在、次に元の比率で表示 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-011 | AC-011 | 層つき z順 | P1, P2, T1, W1 の順に作成し重ねる。分割ペインをクリック | P1 をクリック、`float.focus-next` | W1,T1,P1,P2、次に W1,T1,P2,P1 | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-012 | AC-012 | 折りたたみ往復（3つの方法） | F-1、80×24 でフォーカス中の通常フロート | 折りたたみボタン、帯ダブルクリック、`float.toggle-collapse` のそれぞれで折りたたみ→展開 | 各方法で: 帯 20 px・幅と左上不変、フォーカスは復帰先ペイン→展開でフロート、pty サイズ通知 0 回、80×24。別に、分割ペインにフォーカスした状態で折りたたみボタン → フォーカスは分割ペインのまま | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-013 | AC-013 | 最大化往復 | F-1、(200,100) 400×300 px | `float.toggle-maximize`、最大化ボタン | (0,0) 1000×600 px・100×29・帯あり・通知 1 回、次に (200,100) 400×300 px | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-014 | AC-014 | 最大化中の制限 | 最大化したフロート | (a) 折りたたみ→展開 (b) 帯を (+50,+50) (c) 右辺を (−50,0) (d) 通常状態のフロートを折りたたみ、最大化ボタン、`float.toggle-maximize`、展開 | (a) 最大化 (b)(c) 矩形不変 (d) 折りたたみのまま、展開で通常状態 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-015 | AC-015 | フォーカス移動 | F-1、B のペインアンカーで F1,F2,F3 を作成し F3 を折りたたむ。F1 をクリック、A をクリック。「B をクリック」は (520,10) | `float.toggle-focus`、`float.focus-next` ×2、`float.focus-previous`、`float.toggle-focus`、B をクリック、`float.focus-next`、B をクリック、`float.focus-previous`、B をクリック、F1 内容領域をクリック、F1 上でホイールとマウス移動 | フォーカス F1→F2→F1→F2→A→B→F1→B→F2→B→F1。クリック・ホイール・マウス移動は B に届かず、F1 がスクロール | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-017 | AC-017 | 閉じる確認 | 分割ペインとそのフロートで `sleep 600` を実行中 | (a) 閉じるボタン (b) 作り直して `tab.close` (c) 作り直して分割ペインを閉じる | (a)(b) 確認→承認でフロートのみ閉じ、フォーカスは復帰先ペイン (c) 実行中プログラム数 2 の確認→両方閉じる | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-018 | AC-018 | 自動承認 | 自動承認有効、同じ承認待ち画面を分割ペインとフロートに用意 | 自動承認を走らせる | 判定が等しく、承認入力がフロートの pty に書かれる | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-020 | AC-020 | セッション往復 | AC-020 の Given | 保存→読み戻し | AC-020 の Then（タブアンカーは折りたたみで復元、展開で (300,200) 300×200 px の通常状態。ウィンドウアンカーは (200,100) 400×300 px の通常状態。復元直後の `float.toggle-focus` はウィンドウアンカーのフロートへ） | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-021 | AC-021 | 壊れた記録・旧ファイル・無効時 | (a) F-3 (b) バージョン 3 のファイル (c) `window-save-state` 無効でフロート1枚 | (a)(b) 読む (c) 保存処理 | (a) 1枚復元 (b) 0枚で正常 (c) 書かれない。書き出すバージョンは 4 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-022 | AC-022 | 上限 | (a) 同じ分割ペインに 8 枚 (b) タブアンカー 8 枚＋ペインアンカー1枚 (c) フロート 8 枚、分割ペイン 8 枚 | (a) `float.new` (b) ペインアンカーで `float.cycle-anchor` (c) 分割ペインを追加 | (a) 増えずトースト (b) ペインのままトースト (c) 分割ペイン 9 枚 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-023 | AC-023 | 0枚での無操作 | フロート0枚 | `float.new` 以外の6アクション | 状態とフォーカス不変 | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-024 | AC-024 | pty 生成失敗 | pty 生成を失敗させる差し替え | `float.new` | フロート数とフォーカス不変、エラーログ 1 件 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-025 | AC-025 | ドラッグ中の破棄 | フロートをドラッグ中 | (a) アンカーの分割ペインを閉じる (b) シェル終了による破棄 | ドラッグ状態なし、フロートなし | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-026 | AC-026 | リサイズ | F-1、フロート (300,100) 400×300 px。(e) は A/B 分割でタブアンカーのフロート (500,100) 400×300 px。(f) は同じ分割ペインで F1 (100,100) 300×300 px、F2 (350,150) 300×200 px の順に作り（F2 が前面）、分割ペインの (50,50) をクリックしてフォーカスを分割ペインへ移す | (a) 右下隅 (−2000,−2000) (b) 右下隅 (+2000,+2000) (c) 左辺 (−50,0) (d) 8ハンドルを外向きに 10 px (e) (497,200) を押して (−50,0) (f) (402,250) を押して (+50,0) | (a) 内容領域 200×60 px (b) 右下 (1000,600) (c) 450 px・+5 列・最後の通知が新グリッド (d) 対応する辺のみ 10 px (e) フロート幅 450 px、分割線は x=500 のまま (f) F1 は 300×300 px のまま、F2 がフォーカス | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-027 | AC-027 | キーバインド | (a) 既定 (b) `float.new`〜`float.cycle-anchor` に `cmd+alt+1`〜`cmd+alt+7` を割り当てる `keybind =` 7行 | エンジン構築、(b) は各コードを引く | (a) 既定は2アクション、衝突なし、`cmd+shift+f` 未割り当て (b) 7つとも解決 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-028 | AC-028 | フロート上の分割操作 | フロートにフォーカス | `split.` で始まる全アクション名を列挙して実行 | 分割ツリー・ズーム・フォーカス不変 | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-029 | AC-029 | 対象外ウィンドウ | (a) Scratch Terminal (b) Quick Terminal にフォーカス | `float.new` | フロート数不変 | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-031 | AC-031 | 外部面に出ない | フロート1枚 | AppleScript・IPC・AX のスナップショットと Overview のレイアウトを生成 | どれにもフロートの `PaneId` がない | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-032 | AC-032 | 既存ゲート | ビルド済み | `cargo test --workspace`、`cargo clippy --workspace`（サンドボックス外） | どちらも終了コード 0、clippy の警告 0 件 | machine | コマンドそのもの | yes | CRITICAL | NOT_RUN |
| TC-033 | AC-033 | 非表示フロートを描かない | (a) 別タブ (b) 他ペインがズーム中 (c) アンカー 150×60 px | 描画計画を作る | どの場合もそのフロートが計画にない | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-036 | AC-036 | シェル終了 | 既定設定、フロートと分割ペイン | それぞれで `exit` | フロートの結果が分割ペインと同じ | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-037 | AC-037 | アンカーの移動と消滅 | AC-037 の Given。フロートでは `sleep 600` を実行中 | (a) 分割ペインを別タブへ (b) タブを別ウィンドウへ (c) 別々に: タブ1を閉じる／ウィンドウを閉じる／アプリを終了 | (a) 同じ比率で同じ分割ペインに付く (b) タブと一緒に移る (c) 実行中プログラム数 2／3／3、承認後に pty 解放 | machine | OQ-2 | yes | CRITICAL | NOT_RUN |
| TC-038 | AC-038 | 非表示化でのフォーカス | (a) タブ1の A のフロートにフォーカス、タブ2に C (b) A のフロートにフォーカス | (a) タブ2へ、タブ1へ (b) アンカーを 150×60 px、1000×600 px | (a) C、戻ると A (b) A、戻しても A | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-039 | AC-039 | 折りたたみ中の操作 | F-1、(500,250) 400×300 px を折りたたむ | (a) 帯を (+900,+900) (b) 右辺をドラッグ (c) `float.cycle-anchor` (d) 展開 | (a) 帯左上 (600,580) (b) 不変 (c) タブ (d) 左上 (600,300) | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-040 | AC-040 | 最大化中のアンカー切替 | F-1、ウィンドウアンカー・通常時 (0,100) 800×500 px を最大化 | `float.cycle-anchor`、`float.toggle-maximize` | (0,0) 500×600 px で最大化、解除後は (0,100) 500×500 px | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-041 | AC-041 | フロートからの新規作成 | F-1、A のフロートにフォーカス | `float.new` | 新フロートのアンカーは A | machine | OQ-2 | yes | MEDIUM | NOT_RUN |
| TC-042 | AC-042 | タイトル帯クリック | F1（背面）、F2（前面・折りたたみ）、フォーカスは分割ペイン | F1 の帯、F2 の帯を順にクリック | F1 最前面＋フォーカス、次に F2 最前面でフォーカスは F1 | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-043 | AC-043 | サイドバーからのジャンプ（特殊な状態） | (a) B ズーム中の A のフロート (b) 別ウィンドウのフロート (c) アンカー 150×60 px のペインアンカー | 各カードを選択 | (a) ズーム解除＋フロートにフォーカス (b) ウィンドウ前面＋フロートにフォーカス (c) アンカーの分割ペインにフォーカス | machine | OQ-2 | yes | HIGH | NOT_RUN |
| TC-044 | AC-044 | フォントサイズ変更 | F-1、(200,100) 400×300 px、40×14 | セルを 20×40 px にする | 矩形不変、タイトル帯 40 px、20×6、最後の通知が 20×6 | machine | OQ-2 | yes | HIGH | NOT_RUN |

### TC-004: タイトル帯の構成
- Verifies: `AC-004` (`REQ-002`)
- Mode: human · Must-have: yes · Automation ref: N/A（目視） · Result: `NOT_RUN`
- Precondition: F-0 で起動。
- Steps:
  1. `float.new` を実行する。
  2. フロートを含むスクリーンショットを撮る。
- Expected result: 実装者が、タイトル帯に左からアンカー種別アイコン、タイトル、エージェント状態の領域、最大化、折りたたみ、閉じるの6要素を確認できる。欠けまたは順序違いがあれば FAIL。
- Teardown: フロートを閉じる。

### TC-016: 折りたたみ中の入力待ち表示
- Verifies: `AC-016` (`REQ-014`)
- Mode: human · Must-have: yes · Automation ref: N/A（目視） · Result: `NOT_RUN`
- Precondition: F-0 で起動。サイドバーを開く。分割ペインで F-2a を実行し、そのカードの attention 色を控える。
- Steps:
  1. フロートを作り、`sleep 5; ` に続けて F-2a のコマンドを実行する。
  2. 5秒以内にフロートを折りたたむ。
  3. 10秒待ってスクリーンショットを撮る。
  4. フロートを閉じ、F-2b、F-2c でそれぞれ 1〜3 を繰り返す。
- Expected result: 実装者が各回で次の3点を確認できる: フロートが折りたたまれたまま、タイトル帯の背景色が控えた attention 色と同じ、状態ラベルが F-2a では `approval needed`、F-2b では `reply needed`、F-2c では `error`。1点でも欠ければ FAIL。
- Teardown: フロートを閉じる。

### TC-019: サイドバーと通知からのジャンプ
- Verifies: `AC-019` (`REQ-016`)
- Mode: human · Must-have: yes · Automation ref: N/A（目視） · Result: `NOT_RUN`
- Precondition: F-0 で起動。タブ1でフロートを作り、`sleep 8; ` に続けて F-2a のコマンドを実行して折りたたむ。タブ2を作って表示する。サイドバーを開き、8秒待つ。
- Steps:
  1. サイドバーでそのフロートのカードを確認し、選択する。
  2. Precondition をやり直し、`NextNotification`（macOS `cmd+shift+j`、Linux `ctrl+shift+j`）を実行する。
- Expected result: 実装者が次を確認できる: カードにフロートの印がある。どちらの操作でもタブ1が表示され、フロートが展開されており、打鍵がフロートに入る。1点でも欠ければ FAIL。
- Teardown: タブを閉じる。

### TC-030: 背景が透けない
- Verifies: `AC-030` (`REQ-022`)
- Mode: human · Must-have: yes · Automation ref: N/A（目視） · Result: `NOT_RUN`
- Precondition: 設定に `background-opacity = 0.5`。分割ペインで `yes ABCDEFGHIJ | head -200` を実行。
- Steps:
  1. `float.new` を実行する。
  2. フロートの内容領域を 400% に拡大したスクリーンショットを撮る。
- Expected result: 実装者がフロートの矩形内に `ABCDEFGHIJ` を判読できない。判読できれば FAIL。
- Teardown: 設定を戻す。

### TC-034: 両 OS での実行
- Verifies: `AC-034` (`CFR-004`)
- Mode: human · Must-have: yes · Automation ref: N/A（実行記録の確認） · Result: `NOT_RUN`
- Precondition: §3 の (a) と (b) の環境。
- Steps:
  1. このスイートを (a) で実行し、§9 に1行記録する。
  2. 同じコミットで (b) で実行し、§9 に1行記録する。
- Expected result: §9 に同じ Build の行が2つあり、どちらも must-have の Failed と Blocked が 0。
- Teardown: none required

### TC-035: オーバーレイがフロートより上
- Verifies: `AC-035` (`REQ-008`)
- Mode: human · Must-have: yes · Automation ref: N/A（目視） · Result: `NOT_RUN`
- Precondition: F-0 で起動。フロートを作り、最大化する。フロート内で `sleep 600` を実行する。
- Steps:
  1. コマンドパレットを開き、スクリーンショットを撮って閉じる。
  2. フロートの閉じるボタンを押して確認ダイアログを出し、スクリーンショットを撮ってキャンセルする。
  3. Overview を開き、スクリーンショットを撮って閉じる。
- Expected result: 実装者が3枚それぞれで、オーバーレイの領域にフロートの文字やタイトル帯が重なっていないことを確認できる。1枚でも重なっていれば FAIL。
- Teardown: フロートを閉じる。

## 6. Non-functional procedures (`CFR-n`)

N/A。負荷や標本を伴う測定がない。CFR-001〜CFR-004 は §5 の TC-032、TC-012、TC-033、TC-034 が担う。

## 7. Coverage matrix

| AC | Must-have | Covered by | Mode | Gap |
|----|-----------|-----------|------|-----|
| AC-001〜AC-044（下記6件を除く） | yes | 同じ番号の TC | machine | — |
| AC-004, AC-016, AC-019, AC-030, AC-034, AC-035 | yes | 同じ番号の TC | human | — |

## 8. Defect handling

| Severity | Definition | Blocks exit |
|----------|-----------|-------------|
| S1 | must-have の AC を満たさない。またはフロート内のプロセスが意図せず終了する | yes |
| S2 | must-have の AC を回避手順でしか満たせない | yes |
| S3 | must-have でない AC を満たさない | no（記録） |
| S4 | 見た目や文言 | no（記録） |

must-have の失敗は、ビルドの欠陥（修正して再実行）か、書かれたとおりには作れない AC かに振り分ける。後者はここで読み替えず、`spec-dialogue floating-panes` で仕様を改訂する。

## 9. Execution record

| Run | Date | Build | Executed by | Passed | Failed | Blocked | Skipped | Defects raised |
|-----|------|-------|-------------|--------|--------|---------|---------|----------------|
| 1 | | | | | | | | |

## 10. Residual risk

- machine モードのケースは状態と幾何の計算を確かめる。実際の画面でその矩形どおりに描かれることは、human の5ケース（TC-004, 016, 019, 030, 035）が触れる範囲でしか確かめていない。
- macOS のネイティブタブ切替でウィンドウアンカーのフロートが描き直される瞬間のちらつきは判定していない。
- human のケースは1人の目視に依る。
- Linux は1構成でのみ実行する（OQ-3）。

## 11. Sign-off

| | |
|---|---|
| accepted by | |
| date | |
| verdict | |
