# noa shell integration (zsh) — interactive setup.
#
# Source the user's real .zshrc first so noa's hooks wrap their prompt, then
# restore ZDOTDIR so any zsh spawned later reads the user's config directly.

[[ -f "$USER_ZDOTDIR/.zshrc" ]] && source "$USER_ZDOTDIR/.zshrc"
ZDOTDIR="$USER_ZDOTDIR"

# Only wire up integration for a real interactive terminal.
if [[ -o interactive ]]; then
  # OSC 7: report the working directory as a file:// URL (percent-encoding the
  # few characters that would otherwise break the URL).
  _noa_report_cwd() {
    local encoded="" c
    local i=1
    while (( i <= ${#PWD} )); do
      c="${PWD[i]}"
      case "$c" in
        [a-zA-Z0-9/._~-]) encoded+="$c" ;;
        *) encoded+=$(printf '%%%02X' "'$c") ;;
      esac
      (( i++ ))
    done
    printf '\e]7;file://%s%s\a' "${HOST}" "$encoded"
  }

  # OSC 133 D (previous command's exit status) + A (prompt start) + B (prompt
  # end / input start), plus OSC 7, emitted just before each prompt.
  _noa_precmd() {
    local ret="$?"
    printf '\e]133;D;%s\a' "$ret"
    _noa_report_cwd
    printf '\e]133;A\a'
    printf '\e]133;B\a'
  }

  # OSC 133 C: a command is about to run.
  _noa_preexec() {
    printf '\e]133;C\a'
  }

  autoload -Uz add-zsh-hook
  add-zsh-hook precmd _noa_precmd
  add-zsh-hook preexec _noa_preexec
fi
