#! /bin/sh
# A basic script to save the current page as a file using zenity.
# Remembers the previous directory used.

# You may want to edit this.
prevdirfile="$HOME/.config/aw-man/.save-page-dir"

set -e

src="$AWMAN_CURRENT_FILE"

if [ ! -f "$src" ]; then
  echo "No current file"
  exit 1
fi

bn=$(basename "$AWMAN_RELATIVE_FILE_PATH")
echo $bn
dr=""
[ -f "$prevdirfile" ] && dr=$(cat "$prevdirfile")
[ -d "$dr" ] || dr="$HOME"

dst=$(zenity --file-selection --filename="$dr/$bn" --save --confirm-overwrite)

dn=$(dirname "$dst")
if [ -n "$dn" ]; then
  echo "$dn" > "$prevdirfile"
fi

if [ -n "$dst" ]; then
  echo "Saving $src as $dst"
  cp "$src" "$dst"
fi
