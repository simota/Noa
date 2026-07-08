# Terminal IO Throughput Benchmark

このプロジェクトは、各種ターミナルエミュレータ（Ghostty, Alacritty, Kitty, Warp, iTerm2 等）の I/O スループットおよびレンダリングパフォーマンスを測定するためのベンチマーク環境です。

## ベンチマークデータについて
- `150MB_ascii.txt`: ASCII文字のみで構成された150MBのテキストファイル。
- `150MB_unicode.txt`: 日本語、英語、韓国語、中国語、ロシア語、絵文字、およびCSI（制御シーケンス）文字が混在した150MBのテキストファイル。

## クイックスタート

### 1. ベンチマークデータの生成
Python 3 を使用して、150MBのテスト用テキストファイル2点を生成します。

```bash
python3 generate_data.py
```

### 2. ベンチマークの実行
ターミナルの描画速度を計測するため、出力をリダイレクトせずに直接ターミナルに流します。

```bash
chmod +x run_benchmark.sh
./run_benchmark.sh
```

手動で個別に計測する場合は以下を実行してください。
```bash
time cat 150MB_ascii.txt
time cat 150MB_unicode.txt
```

## 参考データ（ベンチマーク測定結果）

Ghostty の最適化（6つの独立した最適化）適用によるスループット改善の比較データです。

### 1. `time cat 150MB_ascii.txt`
- **Ghostty nightly**: **575ms** (最速)
- **Alacritty**: 1.2秒
- **Ghostty 1.3.2**: 1.5秒
- **Kitty**: 1.7秒
- **Warp**: 3.8秒
- **iTerm2、Terminal**: 60秒後に停止（フリーズまたはタイムアウト）

### 2. `time cat 150MB_unicode.txt` (混合言語)
- **Ghostty nightly**: **536ms** (最速)
- **Alacritty**: 1.05秒
- **Ghostty 1.3.2**: 1.22秒
- **Kitty**: 1.35秒
- **Warp**: 3.4秒
- **iTerm2、Terminal**: 60秒後に停止

### 3. `DOOM-Fire-Zig` (IOテスト)
- **Ghostty nightly**: **842 fps**
- **Alacritty**: 593 fps
- **Warp**: 577 fps
- **Ghostty 1.3.2**: 532 fps
- **Kitty**: 485 fps
- **iTerm2、Terminal**: 60 fps
