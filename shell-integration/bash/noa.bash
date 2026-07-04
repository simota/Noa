# noa shell integration (bash).
#
# noa launches bash with `--rcfile` pointed at this file (an interactive,
# non-login bash reads it in place of ~/.bashrc). We hand back to the user's
# normal startup files, then install the OSC 133 / OSC 7 hooks. No user config
# edits required.

if [[ -n "$NOA_BASH_LOGIN" ]]; then
  if [[ -f ~/.bash_profile ]]; then
    source ~/.bash_profile
  elif [[ -f ~/.bash_login ]]; then
    source ~/.bash_login
  elif [[ -f ~/.profile ]]; then
    source ~/.profile
  fi
else
  [[ -f ~/.bashrc ]] && source ~/.bashrc
fi
unset NOA_BASH_LOGIN NOA_BASH_INJECT NOA_BASH_RCFILE

# Only wire up integration for a real interactive terminal.
case "$-" in
  *i*)
    _noa_report_cwd() {
      printf '\e]7;file://%s%s\a' "${HOSTNAME}" "$PWD"
    }

    # OSC 133 D (last command's exit status) + A (prompt start) + B (input
    # start), plus OSC 7, emitted before each prompt. Runs first in
    # PROMPT_COMMAND so `$?` is still the finished command's status.
    _noa_prompt() {
      local ret="$?"
      printf '\e]133;D;%s\a' "$ret"
      _noa_report_cwd
      printf '\e]133;A\a'
      printf '\e]133;B\a'
    }
    PROMPT_COMMAND="_noa_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}"

    # OSC 133 C: a command is about to run. The DEBUG trap fires for every
    # simple command, so skip the prompt hook itself.
    _noa_preexec() {
      [[ "$BASH_COMMAND" == _noa_prompt* ]] && return
      printf '\e]133;C\a'
    }
    trap '_noa_preexec' DEBUG
    ;;
esac
