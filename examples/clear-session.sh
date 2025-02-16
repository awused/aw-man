#! /bin/sh

# Clears out a session saved with save-session.
#
# It's recommended to call this using the quit_command lifecycle hook in aw-man.toml.

# Keep this in sync with the value in save-session.sh
session_dir="$HOME/.local/state/aw-man/session"

set -e

[ -z "$AWMAN_PID" ] && exit 0

# Reject non-numeric PIDs
case "$AWMAN_PID" in
    ''|*[!0-9]*) exit 0;;
    *) ;;
esac

session="${session_dir}/${AWMAN_PID}.json"

[ -f "$session" ] || exit 0

rm "$session"
