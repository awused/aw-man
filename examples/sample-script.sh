#! /bin/sh
# A sample script. Changes the background to white on even pages and black on odd pages, then advances to the next page.

set -e

page="$AWMAN_PAGE_NUMBER"

if [ `expr $page % 2` == 0 ]; then
  echo "SetBackground white"
else
  echo "SetBackground black"
fi

echo "NextPage"

