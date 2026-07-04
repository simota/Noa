# Spec: Kitty Graphics Protocol

## Metadata

- slug: `kitty-graphics`
- title: Kitty graphics protocol（画像転送・表示・削除・Unicode placeholder）
- status: `implemented`（Phase 5 / Wave4）
- owner: simota
- Ghostty analog: `terminal/kitty/graphics_*.zig`, `terminal/kitty/graphics_unicode.zig`
- 上流仕様: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

端末に画像を転送・表示するプロトコル。`kitten icat` / `timg -pk` / `notcurses` 等が使う。
制御データは APC（`ESC _ G <control> ; <base64 payload> ESC \`）に乗る。

## アーキテクチャ（層の分担）

```
pty bytes
  └ noa-vt   Parser: ESC _ → APC bounded capture(≤1 MiB) → Action::ApcDispatch
      └ Stream: 先頭 'G' → kitty_graphics::parse → Handler::kitty_graphics(KittyGraphicsCommand)
          └ noa-grid Terminal:
                kitty::ImageStore   — 画像データ（画面横断・グローバル quota）
                Screen::kitty_placements — placement（画面ごと・alt 分離）
                応答 → pending_writes（既存の pty writer 経路）
              └ FrameSnapshot: 可視 placement + 参照画像を投影
                  └ noa-render image_layer: id→wgpu テクスチャキャッシュ + z 3 帯域描画
```

- 制御データ解析は **noa-vt**（`kitty_graphics.rs`、`sgr.rs` と同格の純関数）。
- 画像デコード・状態・応答は **noa-grid**（`kitty.rs` / `terminal.rs`）。
- 投影と描画は **noa-render**（`snapshot.rs` / `image_layer.rs` / `shaders/image.wgsl`）。

## 対応範囲

### アクション（`a=`）

| 値 | 意味 | 対応 |
|---|---|---|
| `t` | 転送のみ | ✅ |
| `T` | 転送して即表示 | ✅ |
| `p` | 転送済み画像を表示（put） | ✅ |
| `d` | 画像/placement の削除 | ✅（下記削除指定子） |
| `q` | クエリ（保存せず検証のみ応答） | ✅ |
| `f` / `a` / `c` | アニメーションフレーム/制御 | ❌ `EUNSUPPORTED` |

### フォーマット（`f=`）

- `f=24`（RGB, 3 byte/px）→ RGBA へ展開。
- `f=32`（RGBA, 4 byte/px、既定）。
- `f=100`（PNG）→ `png` crate。RGB/RGBA/グレースケール/グレースケール+α を RGBA8 へ正規化。
  16bit サンプルは上位バイトへ丸め。**パレット PNG は非対応**（`EBADPNG`）。

### 媒体（`t=`）

- `t=d`（direct、既定）: payload = base64 の画像バイト。`o=z`（zlib）解凍対応。
- `t=f`（file）: payload = base64 の**絶対パス**。`canonicalize` → regular file → `S=`/`O=` で部分読み出し。
- `t=t`（temp file）: `t=f` に加え、canonical path が temp ディレクトリ（`$TMPDIR` / `/tmp` /
  `/dev/shm` / `/var/tmp`）配下、またはパスに `tty-graphics-protocol` を含む場合のみ受理し、
  読了後に best-effort で削除。条件を満たさなければ `EINVAL`。
- `t=s`（POSIX 共有メモリ）: ❌ `EUNSUPPORTED`。

### チャンク転送（`m=1`）

同時 1 本（kitty 仕様）。最初のチャンクの制御データが最終判断を駆動し、継続チャンクは
payload のみ連結。進行中に別の graphics コマンドが来たら転送を破棄して新コマンドを処理。
`full_reset`（RIS）でも破棄。

### ID 割り当てと応答

- `i=` 指定 → その id（上書き転送は epoch++ でテクスチャキャッシュ無効化）。
- `i=0, I=n` → 自動採番し、応答に採番した id を反映。
- `i=0 ∧ I=0` → 自動採番するが**一切応答しない**（kitty 挙動）。
- 応答形式: `ESC _ G i=<id>[,I=<n>][,p=<pid>] ; OK ESC \` / エラーは `; E<code>:<message>`。
- 抑制: `q=1` は OK を抑制、`q=2` はエラーも抑制。

### 表示（placement）

- `c=`/`r=` でセルスケーリング、無指定なら `ceil(表示px / cellpx)`。
- `x,y,w,h` で画像クロップ、`X=`/`Y=` で開始セル内 px オフセット。
- `z=` で z-index（下記帯域）。`C=1` でカーソル非移動。
- placement のアンカーは**セッション絶対行**（shell marks と同方式）。通常スクロールは無変換で追従、
  scrollback から落ちた分は snapshot 生成時に遅延掃除。リージョンスクロール/IL/DL は
  交差 placement を削除する v1 近似。

### 削除（`a=d`, `d=`）

`a`(全) / `i`(id) / `n`(番号) / `c`(カーソル) / `p`(セル) / `q`(セル+z) / `r`(id 範囲) /
`x`(列) / `y`(行) / `z`(z) に対応。大文字指定子は、その画像を参照する placement が全画面から
消えた時点で画像データも解放。`d=f`/`F`（アニメ）は `EUNSUPPORTED`。
`ED 2`（画面消去）は交差 placement を削除、RIS は全消去。

### Unicode placeholder（`U=1`）

`U=1` の placement は仮想 placement として保存のみ（直接描画しない）。クライアントは基底スカラ
`U+10EEEE` のセルを印字し、セルのスタイルに描画位置を埋め込む:

- **前景色** → image id の下位ビット（`Palette(n)`→8bit、`Rgb`→24bit）。
- **第 1 結合発音記号** → 画像の行。
- **第 2 結合発音記号** → 画像の列。
- **第 3 結合発音記号** → image id の最上位バイト。
- **下線色** → placement id（省略時 0）。

行/列/最上位バイトの省略時は同一画面行の直前セルから推論（行と最上位バイトは踏襲、列は +1）。
行/列の対応表は kitty の `rowcolumn-diacritics`（結合クラス 230・分解写像なしの 297 個、
Unicode 6.0.0 由来）をコードポイント昇順のソート済み配列として埋め込み、二分探索で値へ写像
（`crates/noa-grid/src/kitty_placeholder.rs`）。同一 (image id, placement id, 画像行) の連続列ランを
1 クワッドに融合し、仮想 placement の rows×cols 仮想グリッドに対する src 部分矩形を算出。
placeholder セルはグリフ描画から除外され、画像だけが見える。

## z 帯域描画

同一 render pass 内で cell パスに割り込み、placement の `z` で 3 帯域に分けて合成する:

1. `z < -2^30` — セル背景より**下**。
2. cell 背景パス。
3. `-2^30 ≤ z < 0` — 背景の上・テキストの下。
4. cell グリフ/装飾パス。
5. `z ≥ 0` — テキストの**上**（ただし UI オーバーレイより下）。

## quota

- 単一画像の寸法上限 `MAX_IMAGE_DIM = 10_000`（幅・高さ各、Ghostty 準拠）。超過 → `EFBIG`。
- 合計デコード RGBA 上限 `TOTAL_BYTES_LIMIT = 320 MB`（kitty/Ghostty 既定）。超過時は可視 placement を
  持たない画像から seq 昇順に破棄、足りなければ最古から破棄。
- renderer 側テクスチャキャッシュは別途 512 MB / 300 フレーム LRU。
- `o=z` の inflate 後サイズも単体上限でガード（zip bomb 抑止）。APC capture は 1 MiB 上限、
  超過は破棄せず `truncated` フラグ付きで dispatch し `EFBIG` 応答。

## 非対応（応答コード）

| 機能 | 応答 |
|---|---|
| アニメーション（`a=f`/`a`/`c`、`d=f`/`F`） | `EUNSUPPORTED` |
| 共有メモリ（`t=s`） | `EUNSUPPORTED` |
| パレット PNG | `EBADPNG` |

エラーコード一覧: `EINVAL`（不正要求）/ `EFBIG`（過大・truncated）/ `ENODATA`（サイズ不一致）/
`EBADPNG` / `ENOENT`（ファイル未検出）/ `EUNSUPPORTED`。

## 実機確認手順

```bash
kitten icat --detect-support        # 対応検出（応答が返ること）
kitten icat path/to/image.png       # 画像表示
kitten icat --clear                 # 全消去
# スクロール追従: icat 後に出力を流し、画像が本文と共に上へ流れること
tmux new; kitten icat image.png     # tmux 内（passthrough 設定時）
timg -pk image.png                  # 別クライアントでの表示
```

Unicode placeholder は `kitten icat --unicode-placeholder image.png` で確認。

## 未確定点（kitty 実機と要突き合わせ）

- 画像表示後のカーソル最終位置（右端到達時の pending_wrap 扱い）。
- リージョンスクロール時の画像移動規則（v1 は交差 placement 削除で近似）。
- `ED 2`/`EL` と画像の関係（本実装は Ghostty パリティ）。
