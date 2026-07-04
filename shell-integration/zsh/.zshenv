# noa shell integration (zsh) — startup bootstrap.
#
# noa launches zsh with ZDOTDIR pointed at this directory so it can layer its
# integration on top of the user's config without the user editing anything.
# Each of noa's startup files (.zshenv/.zprofile/.zshrc/.zlogin) sources the
# user's equivalent from their real ZDOTDIR, so their environment is unchanged;
# .zshrc additionally installs the OSC 133 / OSC 7 hooks.
#
# The user's real ZDOTDIR (or $HOME) is carried in NOA_ZDOTDIR, set by noa.

if [[ -n "$NOA_ZDOTDIR" ]]; then
  export USER_ZDOTDIR="$NOA_ZDOTDIR"
else
  export USER_ZDOTDIR="$HOME"
fi
unset NOA_ZDOTDIR

[[ -f "$USER_ZDOTDIR/.zshenv" ]] && source "$USER_ZDOTDIR/.zshenv"
