# noa shell integration (fish).
#
# fish auto-sources this file because noa prepends its shell-integration/fish
# directory to XDG_DATA_DIRS, and fish reads conf.d from every data dir's
# fish/vendor_conf.d. No user config edits required.

status is-interactive; or exit 0

function _noa_preexec --on-event fish_preexec
    # OSC 133 C: a command is about to run.
    printf '\e]133;C\a'
end

function _noa_prompt --on-event fish_prompt
    # OSC 133 D (last command's exit status) + OSC 7 (cwd, raw path via the
    # kitty-shell-cwd:// scheme) + A (prompt start) + B (input start), emitted
    # before each prompt. One builtin printf and the $hostname variable — no
    # command substitution, since this runs on every prompt.
    printf '\e]133;D;%s\a\e]7;kitty-shell-cwd://%s%s\a\e]133;A\a\e]133;B\a' \
        $status $hostname $PWD
end
