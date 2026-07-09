# Noa 機能一覧

Noa は [Ghostty](https://ghostty.org) の観測可能な挙動を Rust で忠実に再現するターミナルエミュレータ(macOS-first、winit + wgpu)。本書は実装済み機能のインベントリ。キーボードショートカットは [KEYBINDINGS.md](KEYBINDINGS.md) を参照。

## ターミナルコア(noa-grid)

- **スクリーングリッド / カーソル / モード** — DEC 準拠のカーソルクランプを持つアクティブ領域
- **ページ化スクロールバック** — style-interned・バイト上限(`scrollback-limit`)付きページ格納
- **Alt スクリーン / DECSC・DECRC** — 代替画面切替、カーソル保存・復元
- **スクロールリージョン / 左右マージン** — DECSTBM + DECLRMM
- **タブストップ** — 設定 / クリア / 全クリア
- **選択** — セル範囲選択(単語 / 行選択はマウス操作から)
- **インタラクティブ検索** — スクロールバック全文検索
- **URL 検出** — 平文 URL のヒットテスト(⌘クリックで開く)
- **文字セット** — G0–G3 指定・ロッキングシフト(DEC 特殊グラフィックス等)
- **ワイドセル** — CJK / 絵文字の幅 2 セル処理

制限: ソフトラップ reflow は未実装(リサイズはグリッドリサイズのみ)。

## VT プロトコル対応(noa-vt + noa-grid)

自作 DFA パーサ + `Handler` トレイトによるパース↔状態の分離。

- **C0 / CSI / SGR フルセット** — 16 色 + 256 色 + truecolor、bold / faint / italic / inverse / invisible / strike、下線バリエーション(single / double / curly / dotted / dashed)
- **カーソル・消去・スクロール・挿入削除系** — ICH / IL / DL / DCH / ECH / SU / SD / REP など
- **DA / DSR 応答** — DA1 `ESC[?62;4;22c`、カーソル位置レポート等
- **DEC プライベートモード** — DECAWM、DECTCEM、DECCKM、DECNKM / DECPAM、DECLRMM、DECOM 等
- **マウストラッキング** — X10 / 1000 / 1002 / 1003、エンコーディング Legacy / UTF-8(1005) / Urxvt(1015) / SGR(1006)
- **OSC** — 0/2(タイトル、22/23 でスタック)、7(cwd)、8(ハイパーリンク)、9/777(通知)、52(クリップボード、ポリシー付き)、4 / color 系、133(シェル統合マーク)
- **Kitty グラフィックスプロトコル** — 画像コマンドパース + 画像レイヤ描画
- **Sixel グラフィックス** — `DCS Pa;Pb;Ph q ... ST` のパース、Sixel ラスタ化、既存画像レイヤ描画
- **Kitty キーボードプロトコル** — 5 フラグ全対応(disambiguate / event-types / alternate-keys / all-keys / associated-text)、push / pop / set スタック
- **ブラケットペースト(2004) / full reset / DECSTR / DECALN**

## ウィンドウ・タブ・分割(noa-app)

- **マルチウィンドウ / ネイティブタブ** — 新規 / 閉じる / 番号選択 / 前後移動
- **タブタイトルの手動設定** — Set Tab Title プロンプト(パレット / Window メニュー)。設定中はシェル由来(OSC 0/2)のタイトル更新をマスクし、空欄コミットで解除。サイドバーカードにも表示(カード個別リネームが優先)。セッション復元で保持(Ghostty `prompt_surface_title` 相当)
- **分割ツリー(Splits)** — 左 / 右 / 上 / 下へのペイン追加、各行/列最大3枚・全体最大9ペイン(3x3相当)、方向フォーカス移動、リサイズ、均等化、ズームトグル
- **セッションオーバービュー** — 全タブをタイル状にライブ表示する監視ダッシュボード。キー / クリックで切替、インクリメンタル検索、quick-look ズーム

## UI オーバーレイ

- **コマンドパレット** — ファジー(サブシーケンス)検索でアクション実行
- **検索プロンプト** — インクリメンタル検索 UI
- **サイドバー(セッション一覧)** — ウィンドウ単位のセッションカード、プロセスバッジ、インラインリネーム
- **エージェントアテンション** — エージェントプロセス(claude 等)の分類、bell → アテンション昇格、ブリンク、Dock アテンション、git ブランチポーリング
- **About パネル** — バージョン + git ハッシュ + ビルド日付、バンドルアイコン解決
- **確認ダイアログ** — ペースト保護 / OSC 52 / クローズ確認
- **IME プリエディット** — 下線付き変換中テキスト表示

## レンダリング・外観(noa-render + noa-font)

- **wgpu インスタンス化セル描画** — サーフェスレス設計、`FrameSnapshot` 経由でロック時間最小化
- **カーソルスタイル** — block / bar / underline / hollow、フォーカス / ブリンク位相対応
- **下線描画** — single / double / curly / dotted / dashed、ホバーリンク下線
- **背景透過 / ブラー** — `background-opacity`、`background-blur-radius`(macOS ネイティブブラー)
- **minimum-contrast** — WCAG コントラスト比フロアの強制
- **フォントパイプライン** — font-kit 探索 → rustybuzz シェイピング → swash ラスタ → etagere アトラス(モノクロ + カラー絵文字)
- **リガチャ / フォールバック** — liga / calt、CJK フォールバック、Nerd Font・ボックス描画グリフ
- **合成スタイル** — synthetic bold / italic、`font-thicken`
- **テーマ** — Ghostty 互換テーマ 574 個を同梱

## 設定(noa-config)

`~/.config/noa/config`(`$XDG_CONFIG_HOME` 対応)から読み込み。TOML と Ghostty 形式の両対応、CLI フラグが上書き。

| カテゴリ | 主なキー |
|---|---|
| ウィンドウ | `window-width/height`, `window-padding[-x/-y]`, `window-save-state` |
| フォント | `font-family[-bold/-italic/-bold-italic]`, `font-size`, `font-feature`, `font-variation*`, `font-synthetic-style`, `font-thicken[-strength]` |
| 色・テーマ | `theme`, `background`, `foreground`, `cursor-color`, `selection-foreground/background`, `minimum-contrast`, `background-opacity`, `background-blur-radius` |
| カーソル | `cursor-style`, `cursor-style-blink` |
| 挙動 | `scrollback-limit`, `clipboard-read`, `clipboard-paste-protection`, `confirm-quit`, `alpha-blending`, `config-file`(インクルード) |
| macOS | `macos-option-as-alt`, `macos-titlebar-style`, `macos-non-native-fullscreen` |
| Quick Terminal | `quick-terminal-hotkey/-size/-autohide` |
| サイドバー | `sidebar-enabled/-width/-hotkey/-preview-lines` |

- **Ghostty 設定インポート** — 移行統計付きインポート
- **カスタムキーバインド** — `keybind = <chord>=<action>` / `unbind` / `clear` による再割り当て

## macOS 統合

- **ネイティブメニューバー** — Noa / File / Edit / View / Window / Help(Preferences は機能実装まで意図的に無効)
- **フルスクリーン切替** — `⌘⌃F` / View メニュー / コマンドパレット。既定は macOS native、`macos-non-native-fullscreen = true` で borderless fullscreen
- **Quick Terminal** — グローバルホットキー(既定 ⌃`)によるドロップダウン端末、自動非表示対応
- **セキュアキーボードエントリ** — トグル可能
- **デスクトップ通知** — OSC 9 / 777、Dock アテンション
- **クリップボード** — OSC 52(read / write ポリシー)、ペースト保護
- **`.app` バンドル** — `scripts/bundle-macos.sh` でアドホック署名付きバンドル生成
- **CLI アクション** — `+list-themes`, `+list-keybinds`, `+list-fonts`, `+list-actions`, `+show-config`

## セッション・シェル統合

- **セッション復元** — ウィンドウ / タブ / 分割レイアウトの保存・復元
- **シェル統合** — OSC 133(プロンプト / コマンド境界)+ OSC 7(cwd)。`shell-integration/` に bash / zsh / fish スクリプトを同梱
- **プロンプト間ジャンプ** — ⌘↑ / ⌘↓ で前後のプロンプトへスクロール

## 関連ドキュメント

- 機能ごとの詳細仕様: `docs/specs/`
- Ghostty とのパリティ計画: `docs/ghostty-parity-plan.md`
- ロードマップ: `docs/roadmaps/`
