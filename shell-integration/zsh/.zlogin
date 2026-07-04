# noa shell integration (zsh) — login shell .zlogin passthrough.
[[ -f "$USER_ZDOTDIR/.zlogin" ]] && source "$USER_ZDOTDIR/.zlogin"
