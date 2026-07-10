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
