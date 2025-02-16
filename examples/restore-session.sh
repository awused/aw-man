#! /usr/bin/env bash

# Restores previous sessions.
#
# Assumes jq and xdotool are installed if on X11
#
# Does its best to only restore closed sessions that are likely to succeed.
# Also tries not to duplicate open processes.

# Keep this in sync with the value in save-session.sh
session_dir="$HOME/.local/state/aw-man/session"

# If you've renamed the aw-man binary, change this line
process_name="aw-man"

set -e

[ ! -d "$session_dir" ] && exit 0

find "$session_dir" -type f -print0 | while IFS= read -r -d $'\0' session; do
  contents=$(cat "$session") || { echo "Invalid session $session"; continue; }

  # echo $contents
  pid=$(echo "$contents" | jq -e -r '.AWMAN_PID') || { echo "No pid in $session"; continue; }
  archive=$(echo "$contents" | jq -e -r '.AWMAN_ARCHIVE') || { echo "No archive in $session"; continue; }
  type=$(echo "$contents" | jq -e -r '.AWMAN_ARCHIVE_TYPE') || { echo "No archive type in $session"; continue; }

  if running=$(ps -fp "$pid" -o comm=) && [ "$running" = "$process_name" ] ; then
    if socket=$(echo "$contents" | jq -e -r '.AWMAN_SOCKET') ; then
      # Most reliable - if the socket is present we can directly query the program state

      if [ -S "$socket" ] && \
          [ "$archive" = "$(echo "Status" | nc -U "$socket" -N | jq -e -r '.AWMAN_ARCHIVE')" ] ; then
        echo "Session $session is still running"
        continue
      fi
    elif window=$(echo "$contents" | jq -e -r '.AWMAN_WINDOW') ; then
      if [ "$pid" = "$(xdotool getwindowpid "$window" 2> /dev/null)" ] ; then
        echo "Session $session is still running in the same X11 window"
        continue
      fi
    else
      echo "Session $session is probably still running by PID + process name"
      continue
    fi
  fi

  cmd=("$process_name")

  [ "true" = "$(echo "$contents" | jq -e -r '.AWMAN_MANGA_MODE')" ] && cmd+=(--manga)

  # Filesets aren't restored and are treated as directories
  if [ "$type" = "archive" ]; then
    [ ! -f "$archive" ] && {
      echo "Archive '$archive' doesn't exist when restoring $session, skipping";
      continue;
    }

    if page=$(echo "$contents" | jq -e -r '.AWMAN_PAGE_NUMBER') && [ "$page" != "1" ]; then
      cmd+=(--command "Jump ${page}")
    fi

    cmd+=("$archive")
  elif current=$(echo "$contents" | jq -e -r '.AWMAN_CURRENT_FILE'); then
    [ ! -d "$archive" ] && {
      echo "Directory '$archive' doesn't exist when restoring $session, skipping";
      continue;
    }

    if [ -f "$current" ] ; then
      cmd+=("$current")
    else
      echo "Current page '$current' from $session no longer exists"
      cmd+=("$archive")
    fi
  else
    # We don't have a valid page, but if the parent directory also doesn't exist, skip
    [ ! -d "$archive" ] && [ ! -d "$(dirname "$archive")" ] && {
      echo "Directory '$archive' doesn't exist when restoring $session, skipping";
      continue;
    }

    cmd+=("$archive")
  fi

  echo Restoring: "${cmd[@]@Q}"
  nohup "${cmd[@]}" > /dev/null 2>&1 &
  rm "$session"
done

