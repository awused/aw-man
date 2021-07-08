#! /bin/sh
# Writes the URL to the current page on mangadex to the clipboard.
# Only works with chapters downloaded by manga-syncer.
# Only tested on x11.

hashed=$(echo "$AWMAN_ARCHIVE" | sed -E 's/.* - ([^ ]+).zip/\1/')

# Reverse the hash that manga-syncer uses on manadex IDs.
mid=$(python - $hashed <<"END"
from base64 import *
from uuid import *
import sys

print(UUID(bytes=urlsafe_b64decode(sys.argv[1] + "==")))

END
)

if [ -n "$mid" ]; then
  echo "https://mangadex.org/chapter/$mid/$AWMAN_PAGE_NUMBER" | xsel --input --clipboard
  exit 0
else
  # It's important to set the clipboard to something to avoid the case where the user reflexively
  # pastes it after assuming something overwrote the clipboard's contents.
  # This avoids pasting anything sensitive.
  echo "error" | xsel --input --clipboard
fi
