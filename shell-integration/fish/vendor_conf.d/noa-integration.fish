# noa shell integration (fish).
#
# fish auto-sources this file because noa prepends its shell-integration/fish
# directory to XDG_DATA_DIRS, and fish reads conf.d from every data dir's
# fish/vendor_conf.d. No user config edits required.

status is-interactive; or exit 0

function _noa_report_cwd
    # OSC 7: report cwd as a file:// URL.
    printf '\e]7;file://%s%s\a' (hostname) "$PWD"
end

function _noa_preexec --on-event fish_preexec
    # OSC 133 C: a command is about to run.
    printf '\e]133;C\a'
end

function _noa_prompt --on-event fish_prompt
    # OSC 133 D (last command's exit status) + A (prompt start) + B (input
    # start), plus OSC 7, emitted before each prompt.
    set -l ret $status
    printf '\e]133;D;%s\a' $ret
    _noa_report_cwd
    printf '\e]133;A\a'
    printf '\e]133;B\a'
end
