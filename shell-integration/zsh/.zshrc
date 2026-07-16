# noa shell integration (zsh) — interactive setup.
#
# Source the user's real .zshrc first so noa's hooks wrap their prompt, then
# restore ZDOTDIR (or unset it if the user never had one — Ghostty parity) so
# any zsh spawned later reads the user's config directly.

[[ -f "$USER_ZDOTDIR/.zshrc" ]] && source "$USER_ZDOTDIR/.zshrc"
if [[ -n "$NOA_USER_HAD_ZDOTDIR" ]]; then
  ZDOTDIR="$USER_ZDOTDIR"
  unset NOA_USER_HAD_ZDOTDIR
else
  unset ZDOTDIR
fi

# Only wire up integration for a real interactive terminal.
if [[ -o interactive ]]; then
  # OSC 133 D (previous command's exit status) + OSC 7 (cwd) + OSC 133 A
  # (prompt start) + B (prompt end / input start), emitted just before each
  # prompt. This runs on every prompt, so it must stay cheap: one builtin
  # printf, no subshells. The kitty-shell-cwd:// scheme carries the path raw
  # (no percent-encoding), same as Ghostty/kitty.
  _noa_precmd() {
    builtin printf '\e]133;D;%s\a\e]7;kitty-shell-cwd://%s%s\a\e]133;A\a\e]133;B\a' \
      "$?" "$HOST" "$PWD"
  }

  # OSC 133 C: a command is about to run.
  _noa_preexec() {
    builtin printf '\e]133;C\a'
  }

  autoload -Uz add-zsh-hook
  add-zsh-hook precmd _noa_precmd
  add-zsh-hook preexec _noa_preexec
fi
