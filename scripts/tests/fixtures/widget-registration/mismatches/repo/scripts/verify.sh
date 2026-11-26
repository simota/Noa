#!/usr/bin/env bash
# Fixture: the gate.
alp=widgets/alpha/Cargo.toml
bet=widgets/beta/Cargo.toml
del=widgets/delta/Cargo.toml
eps=widgets/epsilon/Cargo.toml
zet=widgets/zeta/Cargo.toml
the=widgets/theta/Cargo.toml
kap=widgets/kappa/Cargo.toml
xi=widgets/xi/Cargo.toml
nu=widgets/nu/Cargo.toml
widget_check() {
  cargo build --manifest-path "$1" --locked --all-targets
}
usage_check() {
  cargo test --manifest-path "$1" --locked
}

if [ "$full" = 1 ]; then
  run alpha widget_check "$alp"
  run beta usage_check "$bet"
  run delta usage_check "$del"
  run epsilon usage_check "$eps"
  run zeta usage_check "$zet"
  run theta usage_check "$the"
  run kappa usage_check "$kap"
  run xi usage_check "$xi"
  run nu usage_check "$nu"
elif [ "${#changed[@]}" -gt 0 ]; then
  for path in "${changed[@]}"; do
    case $path in widgets/alpha/*) alpc=1 ;; esac
    case $path in widgets/beta/*) betc=1 ;; esac
    case $path in widgets/delta/*) delc=1 ;; esac
    case $path in widgets/epsilon/*) epsc=1 ;; esac
    case $path in widgets/zeta/*) zetc=1 ;; esac
    case $path in widgets/theta/*) thec=1 ;; esac
    case $path in widgets/kappa/*) kapc=1 ;; esac
    case $path in widgets/xi/*) xic=1 ;; esac
    case $path in widgets/nu/*) nuc=1 ;; esac
  done
  uses=$(widget_uses "$alp")
  uses=$(widget_uses "$bet")
  uses=$(widget_uses "$del")
  uses=$(widget_uses "$eps")
  uses=$(widget_uses "$the")
  uses=$(widget_uses "$kap")
  uses=$(widget_uses "$xi")
  uses=$(widget_uses "$nu")
  [ "$alpc" = 1 ] && run alpha widget_check "$alp"
  [ "$betc" = 1 ] && run beta usage_check "$bet"
  [ "$delc" = 1 ] && run delta usage_check "$del"
  [ "$epsc" = 1 ] && run epsilon usage_check "$eps"
  [ "$zetc" = 1 ] && run zeta usage_check "$zet"
  [ "$thec" = 1 ] && run theta usage_check "$the"
  [ "$kapc" = 1 ] && run kappa usage_check "$kap"
  [ "$xic" = 1 ] && run xi usage_check "$xi"
  [ "$nuc" = 1 ] && run nu usage_check "$nu"
fi
