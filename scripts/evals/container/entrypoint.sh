#!/usr/bin/env bash
# One attempt at a trial slot, run through `docker exec` in the container
# scripts/evals/container/orchestrate.py starts: reach the model through the
# attempt's relay socket, then run the slot's one case and trial with the
# image's scripts/evals/run-ladder.sh in a fresh eval home on /eval. Reads the
# host's read-only /slot/slot.json and /slot/target.json; writes
# /eval/result.json. Exits 3 when the run failed before any trial started (a
# refused freeze), which is not worth retrying.
set -euo pipefail

SLOT=/slot
EVAL=/eval
SRC=/opt/gents/src
GENTS=/opt/gents/bin/gents
field() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$SLOT/slot.json" "$1"; }
SUITE=$(field suite)
SPLIT=$(field split)
CASE=$(field case_id)
SEED_BASE=$(field seed_base)
TARGET_FILE="$SLOT/target.json"
PORT=$(python3 - "$TARGET_FILE" <<'PY'
import json, sys, urllib.parse
url = urllib.parse.urlparse(json.load(open(sys.argv[1]))["inference_backends"][0]["endpoint"])
if url.hostname != "127.0.0.1" or not url.port:
    sys.exit(f"the target endpoint must be http://127.0.0.1:<port>/..., not {url.geturl()}")
print(url.port)
PY
)

case "$("$GENTS" utils eval run --help)" in
  *--case*) ;;
  *) echo "this image's gents cannot select one case (no eval run --case); rebuild it from a newer commit" >&2; exit 2 ;;
esac

python3 -B /launcher/relay.py forward --port "$PORT" --socket /relay/model.sock >"$EVAL/relay-forward.log" 2>&1 </dev/null &
for _ in $(seq 1 50); do
  python3 -c 'import socket,sys; socket.create_connection(("127.0.0.1", int(sys.argv[1])), 1).close()' "$PORT" 2>/dev/null && break
  sleep 0.1
done

mkdir -p "$HOME"
export GENTS_BIN=$GENTS GENTS_EVAL_TARGET_FILE=$TARGET_FILE GENTS_EVAL_HOME=$EVAL/home \
  GENTS_EVAL_PORT=9494 GENTS_EVAL_SPLITS=$SPLIT GENTS_EVAL_WATCH=0 \
  GENTS_EVAL_SHA=${GENTS_IMAGE_COMMIT:0:10} GENTS_EVAL_CASES=$CASE GENTS_EVAL_SEED_BASE=$SEED_BASE
[ ! -d /overrides/subject ] || export GENTS_EVAL_SUBJECT=/overrides/subject
[ ! -d /overrides/definition ] || export GENTS_EVAL_DEFINITION_SOURCE=/overrides/definition

status=0
"$SRC/scripts/evals/run-ladder.sh" "$SUITE" 1 1 || status=$?
PLAN=$(find "$EVAL/home" -maxdepth 1 -name 'batch-*.json' 2>/dev/null | sort | tail -n 1 || true)
if [ "$status" != 0 ]; then
  for batch_log in "$EVAL"/home/batch-*.log; do
    [ -f "$batch_log" ] || continue
    echo "== $batch_log" >&2
    grep -E 'Error:|error:' "$batch_log" | tail -n 5 >&2 || tail -n 5 "$batch_log" >&2
  done
  if ! compgen -G "$EVAL/home/eval/runs/*/trials/*" >/dev/null; then
    status=3
  fi
fi

python3 - "$EVAL/result.json" "$status" "$PLAN" <<'PY'
import datetime, json, os, sys
path, status, plan = sys.argv[1], int(sys.argv[2]), sys.argv[3]
runs = []
if plan:
    for run in json.load(open(plan))["runs"]:
        runs.append(run["args"][run["args"].index("--run-id") + 1])
result = {"exit_code": status, "run_ids": runs,
          "finished_at": datetime.datetime.now(datetime.timezone.utc).isoformat()}
with open(path + ".tmp", "w") as out:
    json.dump(result, out, indent=2)
os.replace(path + ".tmp", path)
PY
exit "$status"
