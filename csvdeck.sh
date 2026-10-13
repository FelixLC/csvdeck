#!/bin/sh
# Terminal launcher: `csvdeck [file-or-folder]` opens a new csvdeck window, detached from the shell.
# Install: ln -s "$PWD/csvdeck.sh" ~/.local/bin/csvdeck  (any directory on your PATH)
[ $# -gt 0 ] && set -- "$(realpath "$1")"
exec open -na csvdeck --args "$@"
