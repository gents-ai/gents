#!/bin/sh
set -eu
pid=$(cat /runtime/server.pid)
case "$pid" in ''|*[!0-9]*) exit 1;; esac
test "$(cat /proc/$pid/comm)" = gents
kill -TERM "$pid"
for i in $(seq 1 100); do
  if ! kill -0 "$pid" 2>/dev/null; then
    rm /runtime/server.pid
    exit 0
  fi
  sleep 0.1
done
echo 'Runtime did not stop within ten seconds' >&2
exit 1
