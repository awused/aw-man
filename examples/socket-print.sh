#! /bin/sh
# An example script for x11 that connects to the socket for the instance of aw-man that the
# user clicks on. Requires that SocketDir be configured (and this script assumes /tmp/).
# Requires python with the xcffib module.

set -e

wid=$(xwininfo | grep -oE "id: 0x[0-9a-f]+" | sed 's/id: //')
# The python script is just to get the pid from the window id.
pid=$(python - $wid <<"END"
import xcffib, xcffib.xproto, xcffib.res
import sys

connection = xcffib.connect()
resext = connection(xcffib.res.key)
spec = xcffib.res.ClientIdSpec.synthetic(
  int(sys.argv[1], 0), xcffib.res.ClientIdMask.LocalClientPID)
cookie = resext.QueryClientIds(1, [spec])
reply = cookie.reply()

print(reply.ids[0].value[0])

END
)

echo "status" | nc -U "/tmp/aw-man${pid}.sock" | jq

