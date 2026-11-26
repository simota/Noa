---
title: [足場] gamma launch: 登録漏れ
state: open
labels: scaffold
scaffold-asset: check-widget-registration
scaffold-improvement: imp-0123456789abcdef
---
## 改善候補

- id: imp-0123456789abcdef
- rule: missing-registration（登録漏れ）
- target: gamma/launch

## 根拠

- crates/noa-app/src/helper_program.rs:83
- widgets/gamma/Cargo.toml:2

## 関連

- 検証 ver-1732600000000: /data/noa/verifications/ver-1732600000000

## 完了の条件

- `python3 -I -B scripts/check-widget-registration.py` が `gamma launch: missing-registration` を報告しなくなること。
- 手順は `docs/runbooks/add-widget.md` に従う。

## 報告

- Tasks の実行指示のとおり、結果を `$NOA_RESULT` に書く。
