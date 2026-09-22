#!/bin/sh
set -eu
test ! -e /runtime/server.pid
echo $$ > /runtime/server.pid
exec gents server --home /runtime/agent --http-addr 0.0.0.0 --http-port 9191 \
  --tool-ceiling readwrite --tool-root /host --no-codex-shim \
  > /runtime/server.log 2>&1
