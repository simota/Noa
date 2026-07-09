# Spec: About パネル拡張 (about-enhancements)

> **Implementation note:** This feature is implemented. Design-time line
> numbers below are historical; current regression tests live in
> `crates/noa-app/src/cli.rs` and
> `crates/noa-grid/src/tests/terminal_state.rs`.

## Metadata
- slug: about-enhancements
- title: About パネル拡張(ビルドメタデータ + アイコン明示指定)
- status: locked (2026-07-05)
- owner: simota
- scope: Standard(要件11件 — 4〜11件の中複雑度帯)
- build-path decision: feature(監督付き単一ビルド、同セッション実装。コミットA=アイコン、コミットB=ビルドメタデータ)

## L0 — Vision
- **問題**: Noa の About パネル(macOS標準 `orderFrontStandardAboutPanel`)は実装済みだが、表示内容が `CARGO_PKG_VERSION` のみで薄い。git ハッシュ・ビルド日時の埋め込みがなく(build.rs 不在)、`ApplicationIcon` 未指定のため非バンドル起動(`cargo run`)時のアイコン表示が保証されない。
- **対象**: Noa を macOS アプリとして使う・開発するユーザー(主に開発者自身)。
- **Job-to-be-done**: macOS アプリとしての体裁を完成させる — About から正確なバージョン+ビルド情報とアプリアイコン+名称が確認できる。
- **成功定義**: About パネルにバージョン・ビルド情報・アイコン・名称が正しく表示される(バンドル/非バンドル起動の両方)。

## 既存実装状況(Lens スキャン結果・グラウンディング)
- 実装済み: `crates/noa-app/src/app_actions.rs:27-71` `show_about()`/`show_about_macos()` — objc2 経由 `orderFrontStandardAboutPanelWithOptions:`(ApplicationName="Noa", ApplicationVersion=CARGO_PKG_VERSION)。
- メニュー: `crates/noa-app/src/macos_menu.rs:34,291` — muda 0.19 製 NSMenu に「About Noa」項目(`app_menu.append_items(&[&about, ...])`)。
- コマンド: `AppCommand::About`(`commands.rs:9`)、dispatch `crates/noa-app/src/app.rs:1026`(`AppCommand::About => crate::app_actions::show_about()`)。
- バージョン源: `env!("CARGO_PKG_VERSION")` のみ。使用箇所は3か所 — `app_actions.rs:31,61`、`cli.rs:138`(`--version` 出力)、`crates/noa-grid/src/terminal.rs:1527`(XTVERSION/DA応答 `>|noa {version}`)。build.rs 不在。
- 既存回帰防止テスト: `crates/noa-app/src/cli.rs` の `version_output_names_the_binary_and_version`(`--version` が素の `CARGO_PKG_VERSION` で始まることを assert)、`crates/noa-grid/src/tests/terminal_state.rs` の `xtversion_query_reports_name_and_version`(XTVERSION DCS応答が素の `CARGO_PKG_VERSION` を埋め込むことを assert)。
- アイコン: `assets/noa.icns`(`scripts/gen-icon.sh` で生成、`scripts/bundle-macos.sh:41-42` が `.app/Contents/Resources/noa.icns` にコピー済み、`CFBundleIconFile=noa` を Info.plist に設定)。About は `ApplicationIcon` 未指定でバンドル依存。
- 制約: 非 macOS は log のみ(no-op、`app_actions.rs:28-31`)。About はネイティブ AppKit ダイアログで wgpu/renderer 非関与。
- 導入コミット: 38bdd51(About/Preferences/Toggle Sidebar)、74a3f8c(表示名 noa→Noa)。

## L1 — Requirements

### Functional Requirements

| ID | 要件 |
|---|---|
| R-1 | About パネルの `ApplicationIcon` は二段ルックアップで解決する: (1) バンドル起動時は `NSBundle.mainBundle().resourcePath` 配下の `noa.icns`、(2) 非バンドル起動(`cargo run`)時は `CARGO_MANIFEST_DIR` 相対の `../../assets/noa.icns` にフォールバックする。 |
| R-2 | (1)(2) いずれのパスにもファイルが存在しない場合、`show_about_macos()` は panic・crash せず、`ApplicationIcon` キーを設定しないことで AppKit 標準アイコン表示にフォールバックする。 |
| R-3 | `crates/noa-app/build.rs` を新設し、ビルド時に git 短縮ハッシュ(7桁、`git rev-parse --short=7 HEAD` 相当)を `NOA_GIT_HASH` 環境変数として `cargo:rustc-env=` 経由で埋め込む。 |
| R-4 | 同 build.rs が UTC ビルド日時(`YYYY-MM-DD`)を `NOA_BUILD_DATE` 環境変数として埋め込む。`SOURCE_DATE_EPOCH` が設定されている場合はそれを優先し(reproducible builds)、未設定時はビルド実行時の UTC 日付を使う。 |
| R-5 | git が利用不可(リポジトリ外・git 未インストール等、例: tarball 展開ビルド)の場合、build.rs は `NOA_GIT_HASH` のみを空文字列として埋め込む(`env!()` はビルドを常にコンパイル可能に保つため必ず値を emit する)。`NOA_BUILD_DATE` は git 有無と独立に常に取得される(R-4)。`NOA_GIT_HASH` が空文字列の場合、About のバージョン表示は素の `CARGO_PKG_VERSION` のみにフォールバックする。 |
| R-6 | git ハッシュ・日付が非空の場合、About パネルの `ApplicationVersion` は `"{CARGO_PKG_VERSION} ({NOA_GIT_HASH}, {NOA_BUILD_DATE})"` 形式(例: `"0.1.0 (a1b2c3d, 2026-07-05)"`)で表示する。この拡張表示は About パネル限定とする。 |
| R-7 | アイコン明示指定(R-1, R-2)とビルドメタデータ埋め込み(R-3〜R-6)は独立した2コミット(A=アイコン、B=ビルドメタデータ)として実装し、個別に revert 可能とする。 |

### Non-Functional Requirements (Cross-Functional Requirements)

| ID | 要件 |
|---|---|
| NFR-1 | 新規 crate 依存を追加しない。git 呼び出しは build.rs 内で `std::process::Command` により直接 `git` バイナリを起動する(`git2` 等のクレートは使わない)。 |
| NFR-2 | `noa --version` の出力(`cli.rs:138` 由来)、および XTVERSION/DA 応答(`terminal.rs:1527` 由来)は本変更で一切変わらない。拡張バージョン文字列は About パネル表示にのみ適用する。 |
| NFR-3 | build.rs の `cargo:rerun-if-changed` 指定は `.git/HEAD`・`.git/refs` 配下・`.git/packed-refs`(`git gc` で pack された ref はこちらにのみ現れる)、および `build.rs` 自身に限定し、無関係な変更後の `cargo build` で不要な再ビルド(build.rs 再実行含む)を発生させない。 |
| NFR-4 | 非 macOS ビルドは本変更後も no-op のまま — `show_about()` の `#[cfg(not(target_os = "macos"))]` 分岐(ログ出力のみ)の挙動・形は変更しない。 |

## L2 — Detail (Development)

### コンポーネント設計

**A. アイコン明示指定** — `crates/noa-app/src/app_actions.rs`
- パス選択ロジックはテスト注入可能な純粋ヘルパーに分離する: `icon_path_from_candidates(candidates: &[PathBuf]) -> Option<PathBuf>` — 先頭から順に `Path::exists()` を確認し、最初に存在するパスを返す。全候補が不在なら `None`(R-2 準拠、`ApplicationIcon` キーは options 辞書に追加しない)。
- `show_about_macos()` 側で候補リストを構築して渡す:
  1. `NSBundle::mainBundle()` → `resourcePath` (NSString) から `{resourcePath}/noa.icns`(バンドル起動時、`bundle-macos.sh:41-42` が配置した実体と一致)。
  2. `Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/noa.icns")`(`cargo run` 等の非バンドル起動時、ワークスペースルート `assets/noa.icns` を直接参照)。
- ヘルパーが `Some(path)` を返した場合、`NSImage` を `initWithContentsOfFile:` で生成し、`options` 辞書に `ApplicationIcon` キー(既存の `ApplicationName`/`ApplicationVersion` と同じ objc2 `setObject:forKey:` パターン)として設定する。

**B. build.rs ビルドメタデータ** — 新規 `crates/noa-app/build.rs`
- `std::process::Command::new("git").args(["rev-parse", "--short=7", "HEAD"])` を `CARGO_MANIFEST_DIR` から実行。成功(exit 0 かつ stdout 非空)なら trim した stdout を `NOA_GIT_HASH` として emit、失敗時(非 git リポジトリ・git 未インストール・エラー終了)は空文字列を emit。
- 日付: `SOURCE_DATE_EPOCH` 環境変数が設定されていればそれを UTC の `YYYY-MM-DD` に変換、未設定なら `std::process::Command::new("date").args(["-u", "+%Y-%m-%d"])` の出力を使用して `NOA_BUILD_DATE` として emit(git 有無と独立 — 日付埋め込みは常に成功する)。
- 常に emit: `println!("cargo:rustc-env=NOA_GIT_HASH={hash}")` / `println!("cargo:rustc-env=NOA_BUILD_DATE={date}")`(値が空文字列でも emit し、消費側 `env!("NOA_GIT_HASH")` が常にコンパイル可能であることを保証する — R-5)。
- `println!("cargo:rerun-if-changed=build.rs")` に加え、リポジトリの `.git/HEAD`・`.git/refs`(ディレクトリ指定 — cargo は再帰的に監視)・`.git/packed-refs` を指す再実行トリガーを emit(具体パスは `CARGO_MANIFEST_DIR/../../.git/HEAD` 等 — noa-app は `crates/noa-app` にありワークスペースルートの `.git` は2階層上)。
- `app_actions.rs` 側に `version_string() -> String` ヘルパーを追加: `NOA_GIT_HASH` と `NOA_BUILD_DATE` が両方非空なら `format!("{} ({}, {})", CARGO_PKG_VERSION, hash, date)`、いずれかが空なら素の `CARGO_PKG_VERSION` を返す(R-5, R-6)。`show_about_macos()` の `ApplicationVersion` はこの `version_string()` を使う。`cli.rs:138` / `terminal.rs:1527` は `env!("CARGO_PKG_VERSION")` を直接参照したまま変更しない(NFR-2)。

### コミット分割(R-7)
- コミット A: `icon_path_from_candidates()` 追加 + `ApplicationIcon` 設定ロジック(`app_actions.rs` のみ)。
- コミット B: `build.rs` 新設 + `version_string()` + `ApplicationVersion` 呼び出し差し替え(`app_actions.rs` + `build.rs`)。

## L3 — Acceptance Criteria

| AC | 対応 R/NFR | 内容 | 検証種別 |
|---|---|---|---|
| AC-1 | R-1 | `scripts/bundle-macos.sh` でビルドした `.app` を起動すると、About パネルに Noa アイコン(`noa.icns`)が表示される。 | 目視(human-visual) |
| AC-2 | R-1 | `cargo run -p noa` で起動(非バンドル)しても、About パネルに Noa アイコンが表示される(`CARGO_MANIFEST_DIR/../../assets/noa.icns` 経由)。 | 目視(human-visual) |
| AC-3 | R-2 | `icon_path_from_candidates()` が全候補パス不在時に `None` を返し(hermetic unit test: 存在しないパスのみを渡して assert)、`None` 時に `show_about_macos()` が `ApplicationIcon` キーを設定しない(コードレビュー確認)。アイコン不在環境でも panic せず About パネルが AppKit 標準アイコンで表示される。 | 機械検証可能(unit test)+ 目視(human-visual、アイコン不在時の表示確認) |
| AC-4 | R-3, R-4, R-6 | git リポジトリ内でビルドした場合、About の `ApplicationVersion` が `"{version} ({7桁hash}, {YYYY-MM-DD})"` 形式(例 `"0.1.0 (a1b2c3d, 2026-07-05)"`)で表示される。 | 目視(human-visual) + 機械検証可能(`version_string()` の unit test で書式を assert) |
| AC-5 | R-5 | git が存在しない環境(例: `.git` を含まない tarball 展開ビルド、または `NOA_GIT_HASH`/`NOA_BUILD_DATE` が空文字列の状態を模したテスト)でビルドした場合、`version_string()` が素の `CARGO_PKG_VERSION` のみを返す。 | 機械検証可能(unit test) |
| AC-6 | R-6 | `version_string()` の unit test が「両方非空→合成書式」「片方でも空→素バージョン」の2分岐を網羅する。 | 機械検証可能(unit test) |
| AC-7 | R-7 | git log 上でアイコン変更(コミットA)とビルドメタデータ変更(コミットB)が独立した2コミットに分かれている。 | 目視(human-visual, `git log` 確認) |
| AC-8 | NFR-1 | 本変更の diff に `Cargo.toml` / `Cargo.lock` への新規 crate 追加が含まれない(git 呼び出しは `std::process::Command` のみ)。 | 機械検証可能(diff確認 / `cargo tree` 比較) |
| AC-9 | NFR-2 | 既存テスト `crates/noa-app/src/cli.rs` (`version_output_names_the_binary_and_version`) と `crates/noa-grid/src/tests/terminal_state.rs` (`xtversion_query_reports_name_and_version`) が変更なしで PASS し続ける — `--version` と XTVERSION/DA 応答が素の `CARGO_PKG_VERSION` のまま不変であることの回帰確認。 | 機械検証可能(`cargo test --offline -p noa-app -p noa-grid`) |
| AC-10 | NFR-3 | 変更なしで連続 `cargo build --offline -p noa` を2回実行した場合、2回目に build.rs の再実行("Running build script" 相当の再コンパイル)が発生しない。注: バックグラウンドの git housekeeping(index refresh・loose-ref 書き込み)が `.git/HEAD`/`.git/refs` の mtime を更新した場合の再実行は正当であり flake として許容する(検証時は事前に ref の mtime を確認)。 | 機械検証可能(`cargo build -vv` 出力比較、mtime事前確認付き) |
| AC-11 | NFR-4 | `show_about()` の `#[cfg(not(target_os = "macos"))]` 分岐がログ出力のみのまま変更されず、`cargo check --offline --workspace` が成功する。 | 機械検証可能(コードレビュー + `cargo check`) |
| AC-12 | 全体品質ゲート | `cargo clippy --workspace --offline` が新規 warning なしで通る。 | 機械検証可能(`cargo clippy`) |

## Scope

### In Scope
- About パネルの `ApplicationIcon` 明示指定(バンドル / 非バンドル二段ルックアップ)。
- About パネル限定のビルドメタデータ(gitハッシュ7桁 + UTCビルド日付)埋め込みと表示。
- `crates/noa-app/build.rs` の新設。

### Out of Scope
- `-dirty` サフィックス(未コミット変更の検出・表示)— ストレステストで削減済み。
- rustc バージョン・ターゲットトリプル等の追加ビルドメタデータ表示。
- Credits リッチテキスト(rustc/主要crateバージョン一覧、Ghostty謝辞) — 候補C、閲覧頻度に対して過剰装飾のため棄却。
- CLI `+version` 相当の詳細出力拡充 — 候補D、`noa --version`(`cli.rs:138`)は既存で十分、apprt表面へのスコープ拡大を回避。
- 非 macOS の About UI(引き続き log-only no-op)。
- 本機能に関する config キーの追加。

## Considered but Rejected
- **C. Credits リッチテキスト**: rustc/主要crateバージョン+Ghostty謝辞を追加表示する案。閲覧頻度が低い画面に対して過剰装飾になるためユーザー判断で棄却。
- **D. CLI詳細バージョン拡充**: Ghostty `+version` 相当の詳細出力を CLI に追加する案。`noa --version`(`cli.rs:138`)は既存実装で要件を満たしており、apprt 表面(CLI)へのスコープ拡大を避けるため棄却。
- **E. YAGNI ベースライン(Aのみで打ち切り)**: アイコン明示指定のみで終える最小案。ビルドメタデータ(B)はAbout強化のJTBDに直結し実装コストも小さいため、ベースラインより一段広い A+B を採択。

## Open Questions / Deferred Decisions
- なし(SOURCE_DATE_EPOCH 対応は L2 で決定済み — 設定されていれば優先、未設定時はビルド時UTC日付にフォールバック)。
- 参考: noa-app がワークスペース外で単体ビルドされる場合(vendor/publish等)の git ルックアップ失敗は R-5 のフォールバックで素バージョン表示になるため個別対応不要。将来 crates.io 公開等でこの前提が崩れた場合は再検討する。

## Build-path decision
- **feature**(2026-07-05 LOCK時決定): 小規模(2コミット、S+M)のため監督付き単一ビルド。orbit/apex は過剰と判断。AC-1〜12 が検証契約。

## 経緯
- 2026-07-05 FRAME 確認済み: 当初「About機能の実装」→ Lens スキャンで実装済みと判明 → ユーザー確認のうえ「既存 About の拡張」spec にスコープ変更。
- 表示形態: macOS 標準 About パネル(維持)。表示内容の要望: バージョン+ビルド情報、アイコン+名称。動機: macOS アプリとしての体裁。
- 2026-07-05 SPECIFY: Spark+Accord により L0〜L3 の統合仕様へ精緻化。要件11件(機能7 + NFR4)、AC12件(全件R/NFRトレース済み)。Open Questionsなし(SOURCE_DATE_EPOCH方針をL2で確定)。
- 2026-07-05 Quality Gate(Judge+Attest): Ambiguity/Completeness/Scope=PASS、Consistency/Testability=FAIL → 5件修正を適用: (1) R-5 を NOA_GIT_HASH のみ空に修正(日付は git 非依存)、(2) AC-3 を注入可能ヘルパー `icon_path_from_candidates()` による hermetic unit test 設計に変更、(3) AC-10 に git housekeeping による mtime flake 許容の注記、(4) NFR-3/L2 に `.git/packed-refs` を追加、(5) 引用修正 `app.rs:1052`→`app.rs:1026`。全FAIL所見は修正済み — Gate通過。
- 2026-07-05 LOCK: ユーザーサインオフ。build-path=feature(同セッション実装)。
- 2026-07-05 実装完了: d1477e5(コミットA=アイコン)、5a546ce(コミットB=ビルドメタデータ)、bbfbae1(追補: `.git/packed-refs` 不在時は watch しない — cargo は不在パスを常に stale 扱いし毎ビルド再実行になるため。NFR-3 の意図(不要な再ビルド禁止)を守るための条件付き emit)。機械検証AC(3,5,6,8,9,10,11,12)全PASS。AC-1/2/4 の目視確認は未実施(GUI起動要)。
