#! /bin/sh
# An example script for x11 that connects to the socket for the instance of aw-man that the
# user clicks on. Requires that SocketDir be configured (and this script assumes /tmp/).

set -e

pid=$(xprop _NET_WM_PID | sed 's/_NET_WM_PID(CARDINAL) = //')

echo "status" | nc -U "/tmp/aw-man${pid}.sock" | jq

