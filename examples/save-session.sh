#! /bin/sh

# Saves a file that can be used to reopen all aw-man instances at once.
# Requires jq.
#
# It's recommended to call this using the various lifecycle hooks in aw-man.toml.

# The session directory will be created if it doesn't exist.
# If changing this, make sure to update clear-session and restore-session.
session_dir="$HOME/.local/state/aw-man/session"

# For automatic session saving using the example scripts:
#
# startup_command = "Execute /path/to/save-session.sh"
# idle_command = "Execute /path/to/save-session.sh"
# archive_change_command = "Execute /path/to/save-session.sh"
# quit_command = "Execute /path/to/clear-session.sh"
#
# This will save the session every time the open archive changes, and when the application
# goes idle it'll also save the specific page. You can then restore any sessions that
# exit abnormally with restore-session.sh.

set -e

mkdir -p "$session_dir"

# Restoring won't work at all without at least these
[ -z "$AWMAN_PID" ] && exit 0
[ -z "$AWMAN_ARCHIVE" ] && exit 0
[ -z "$AWMAN_MANGA_MODE" ] && exit 0

# Reject non-numeric PIDs
case "$AWMAN_PID" in
    ''|*[!0-9]*) exit 0;;
    *) ;;
esac

# This is a good place to reject sessions for any other reason.

session="${session_dir}/${AWMAN_PID}.json"

jq -n 'env | with_entries(select(.key | startswith("AWMAN")))' > "$session"
