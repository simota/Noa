#!/usr/bin/env bash
# Fixture: the gate.
alp=widgets/alpha/Cargo.toml
bet=widgets/beta/Cargo.toml
widget_check() {
  cargo build --manifest-path "$1" --locked --all-targets
}
usage_check() {
  cargo test --manifest-path "$1" --locked
}

if [ "$full" = 1 ]; then
  in_part core && run alpha widget_check "$alp"
  in_part widgets && run beta usage_check "$bet"
elif [ "${#changed[@]}" -gt 0 ]; then
  for path in "${changed[@]}"; do
    case $path in widgets/alpha/*) alpc=1 ;; esac
    case $path in widgets/beta/*) betc=1 ;; esac
  done
  uses=$(widget_uses "$alp")
  uses=$(widget_uses "$bet")
  [ "$alpc" = 1 ] && run alpha widget_check "$alp"
  [ "$betc" = 1 ] && run beta usage_check "$bet"
fi
