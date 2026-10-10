#!/usr/bin/env bash
# Run native onboarding evals with a live browser or terminal view.
#
#   scripts/evals/run-ladder.sh [SUITE|all|list] [TRIALS] [CONCURRENCY]
#       [--backend TARGET | --target TARGET] [--trials N] [--concurrency N]
#
# `all` runs L1-L5 and the configuration-only capstone. L6 is an explicit
# capability probe, excluded from acceptance while graph authoring is missing.
# Defaults: 3 trials per case; global trial cap floor(4 / PER_TRIAL); train + validation.
# The cap covers active trials across all runs, not every provider/background call.
# Held-out cases run only with GENTS_EVAL_SPLITS=held_out.
#
# GENTS_EVAL_TARGET: target file name (default workstation-1).
# GENTS_EVAL_SHARD: 1/2 or 2/2 splits the selected suites across two launchers.
# GENTS_EVAL_HOME / GENTS_EVAL_PORT: isolated home / optional fixed port.
# GENTS_EVAL_PER_TRIAL: concurrent calls per trial (default 1).
# GENTS_EVAL_REASONING / GENTS_EVAL_TEMPERATURE / GENTS_EVAL_TOP_P: high / 1 / .95.
# GENTS_EVAL_WATCH: auto, 1/web, tui or 0. Auto opens the browser in a terminal.
# GENTS_EVAL_WEB_PORT: local dashboard port (default 9495).
# GENTS_BIN: existing gents binary; otherwise builds this checkout.
set -euo pipefail

usage() { sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-2}"; }
POSITIONAL=()
TARGET=${GENTS_EVAL_TARGET:-workstation-1}
NAMED_TRIALS=
NAMED_CONCURRENCY=
while [ $# -gt 0 ]; do
  case "$1" in
    --backend|--target) [ $# -ge 2 ] || usage; TARGET=$2; shift 2 ;;
    --trials) [ $# -ge 2 ] || usage; NAMED_TRIALS=$2; shift 2 ;;
    --concurrency) [ $# -ge 2 ] || usage; NAMED_CONCURRENCY=$2; shift 2 ;;
    -h|--help) usage 0 ;;
    --*) usage ;;
    *) POSITIONAL+=("$1"); shift ;;
  esac
done
[ ${#POSITIONAL[@]} -le 3 ] || usage
SELECTION=${POSITIONAL[0]:-all}

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
FIXTURES="$ROOT/crates/gents/tests/fixtures/configurator_evals"
LADDER="$FIXTURES/ladder"
MATRIX="$ROOT/scripts/evals/matrix.json"
SHA=$(git -C "$ROOT" rev-parse --short HEAD)
TRIALS=${NAMED_TRIALS:-${POSITIONAL[1]:-3}}
CONCURRENCY=${NAMED_CONCURRENCY:-${POSITIONAL[2]:-4}}
PER_TRIAL=${GENTS_EVAL_PER_TRIAL:-1}
SPLITS=${GENTS_EVAL_SPLITS:-train validation}
EVAL_HOME=${GENTS_EVAL_HOME:-$HOME/gents-eval-homes/ladder-$SHA-$TARGET}
EVAL_HOME=$(python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$EVAL_HOME")
PORT_OVERRIDE=${GENTS_EVAL_PORT:-}
PORT=$(python3 - "$EVAL_HOME/runtime.json" "$PORT_OVERRIDE" "$TARGET" <<'PYPORT'
import json, pathlib, sys, urllib.parse
path, override, target = sys.argv[1:]
port = 9494 if target == "workstation-2" else 9493
try:
    url = urllib.parse.urlparse(json.loads(pathlib.Path(path).read_text())["graphql"])
    if url.hostname == "127.0.0.1" and url.port:
        port = url.port
except (OSError, ValueError, KeyError, TypeError):
    pass
if override:
    try:
        port = int(override)
    except ValueError:
        sys.exit("GENTS_EVAL_PORT must be an integer between 1 and 65535")
if not 1 <= port <= 65535:
    sys.exit("GENTS_EVAL_PORT must be between 1 and 65535")
print(port)
PYPORT
)
REASONING=${GENTS_EVAL_REASONING:-high}
TEMPERATURE=${GENTS_EVAL_TEMPERATURE:-1.0}
TOP_P=${GENTS_EVAL_TOP_P:-0.95}
TARGET_FILE="$ROOT/scripts/evals/targets/$TARGET.json"

if [ "$SELECTION" = list ]; then
  python3 - "$MATRIX" "$FIXTURES" "$TRIALS" <<'PYLIST'
import collections, json, pathlib, sys
matrix=json.load(open(sys.argv[1])); root=pathlib.Path(sys.argv[2]); trials=int(sys.argv[3])
print(f"{'Suite':<20} {'Train':>5} {'Valid':>5} {'Held':>5} {'Stages':>6} {'Checks':>6}  Coverage")
for suite in matrix['suites']:
    definition=json.load(open(root/suite['path']/'pack_config.json'))['eval_definitions'][0]
    cases=[json.load(open(root/suite['path']/p)) for p in definition['cases']]
    splits=collections.Counter(c['split'] for c in cases)
    stages=[s for c in cases for s in c['stages']]
    print(f"{suite['id']:<20} {splits['train']:>5} {splits['validation']:>5} {splits['held_out']:>5} {len(stages):>6} {sum(len(s['checks']) for s in stages):>6}  {', '.join(suite['coverage'])}")
    if 'blocked_reason' in suite: print('  '+suite['blocked_reason'])
print(f"Stage/check totals include held-out cases; each selected case runs {trials} trials. Capstone stops at configuration.")
PYLIST
  exit 0
fi
SELECTED=()
SELECTED_TEXT=$(python3 - "$MATRIX" "$SELECTION" "${GENTS_EVAL_SHARD:-1/1}" <<'PYSELECT'
import json,sys
m=json.load(open(sys.argv[1])); selection=sys.argv[2]
try:
    shard, total = map(int, sys.argv[3].split('/'))
    if not 1 <= shard <= total: raise ValueError()
except ValueError:
    sys.exit('GENTS_EVAL_SHARD must be INDEX/COUNT with 1 <= INDEX <= COUNT')
if selection=='all': matches=m['default']
else:
    matches=[s['id'] for s in m['suites'] if selection in [s['id'],s['id'].split('-')[0]]]
    if len(matches)!=1: sys.exit('unknown or ambiguous suite: '+selection)
selected=matches[shard-1::total]
if not selected: sys.exit('this shard selects no suites')
print('\n'.join(selected))
PYSELECT
)
while IFS= read -r level; do [ -z "$level" ] || SELECTED+=("$level"); done <<< "$SELECTED_TEXT"
[ ${#SELECTED[@]} -gt 0 ] || { echo "no suites selected for $SELECTION and shard ${GENTS_EVAL_SHARD:-1/1}; use list" >&2; exit 2; }
[[ "$TRIALS" =~ ^[1-9][0-9]*$ && "$CONCURRENCY" =~ ^[1-9][0-9]*$ && "$PER_TRIAL" =~ ^[1-9][0-9]*$ ]] || usage
[ -f "$TARGET_FILE" ] || { echo "no target $TARGET_FILE" >&2; exit 2; }
[ "$PORT" != 9191 ] || { echo "port 9191 belongs to the desktop node; pick another GENTS_EVAL_PORT" >&2; exit 2; }
(( PER_TRIAL >= 1 && CONCURRENCY >= PER_TRIAL )) || { echo "CONCURRENCY must be at least GENTS_EVAL_PER_TRIAL" >&2; exit 2; }
TRIAL_CONCURRENCY=$(( CONCURRENCY / PER_TRIAL ))
python3 - "$SPLITS" <<'PYVALIDATESPLITS'
import sys
splits = sys.argv[1].split()
if not splits or len(splits) != len(set(splits)) or any(s not in {"train", "validation", "held_out"} for s in splits):
    sys.exit("GENTS_EVAL_SPLITS must contain distinct train, validation or held_out splits")
PYVALIDATESPLITS

if [ -n "${GENTS_BIN:-}" ]; then
  GENTS=$GENTS_BIN
else
  echo "building gents at $SHA ..." >&2
  BUILD_SOURCE=$(git -C "$ROOT" rev-parse HEAD)
  BUILD_REF=$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)
  BUILD_DIRTY=false
  [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ] || BUILD_DIRTY=true
  env -u GENTS_BUILD_GIT_TAG GENTS_BUILD_GIT_SHA="$BUILD_SOURCE" GENTS_BUILD_GIT_REF="$BUILD_REF" GENTS_BUILD_GIT_DIRTY="$BUILD_DIRTY" \
    cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p gents-cli --bin gents -p gents-fs-runner --bin gents-fs-runner
  GENTS="$ROOT/target/debug/gents"
  BUILT_VERSION=$("$GENTS" version)
  case "$BUILT_VERSION" in
    *"$BUILD_SOURCE"*) ;;
    *) echo "built binary does not match checkout $BUILD_SOURCE: $BUILT_VERSION" >&2; exit 1 ;;
  esac
fi

# Versioned binary names do not use the runtime's built-in runner discovery.
# Resolve the packaged helper before any inference is started.
if [ -z "${GENTS_FS_RUNNER:-}" ] && [ -x "$(dirname "$GENTS")/gents-fs-runner" ]; then
  export GENTS_FS_RUNNER="$(dirname "$GENTS")/gents-fs-runner"
fi
if [ -n "${GENTS_FS_RUNNER:-}" ]; then
  [ -x "$GENTS_FS_RUNNER" ] || { echo "GENTS_FS_RUNNER is not executable: $GENTS_FS_RUNNER" >&2; exit 1; }
elif [ "$(python3 -c 'import os,sys; print(os.path.basename(os.path.realpath(sys.argv[1])))' "$GENTS")" != gents ]; then
  echo "Install gents-fs-runner beside $GENTS, or set GENTS_FS_RUNNER to its executable before running evals." >&2
  exit 1
fi

target_field() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]][0][sys.argv[3]])' "$TARGET_FILE" "$1" "$2"; }
ENDPOINT=$(target_field inference_backends endpoint)
MODEL=$(target_field inference_profiles model_name)

mkdir -p "$EVAL_HOME/work"
if [ ! -f "$EVAL_HOME/init.json" ]; then
  echo "initializing $EVAL_HOME ..." >&2
  (cd "$EVAL_HOME/work" && "$GENTS" init --home "$EVAL_HOME" --write --inference-url "$ENDPOINT" \
    --model-name "$MODEL" --max-concurrent "$PER_TRIAL" --tool-root "$EVAL_HOME/work" >/dev/null)
fi
DID=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["node_did"])' "$EVAL_HOME/init.json")

GRAPHQL="http://127.0.0.1:$PORT/api/v0/graphql"
# Served means the endpoint answers and runtime.json names it: CLI commands
# with --home fall back to opening the store themselves until runtime.json does.
served() {
  grep -qF "127.0.0.1:$PORT/" "$EVAL_HOME/runtime.json" 2>/dev/null &&
    "$GENTS" query --home "$EVAL_HOME" --graphql "$GRAPHQL" --collection Node --field node_did 2>/dev/null |
      python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit(0 if any(r.get("node_did")==sys.argv[1] for r in d.get("results",[])) else 1)' "$DID" 2>/dev/null
}
if ! served; then
  if [ -z "$PORT_OVERRIDE" ]; then
    PORT=$(python3 - "$PORT" <<'PYFREE'
import socket, sys
with socket.socket() as listener:
    try:
        listener.bind(("127.0.0.1", int(sys.argv[1])))
    except OSError:
        listener.bind(("127.0.0.1", 0))
    print(listener.getsockname()[1])
PYFREE
)
    GRAPHQL="http://127.0.0.1:$PORT/api/v0/graphql"
  fi
  echo "serving $EVAL_HOME on $PORT (log $EVAL_HOME/server.log) ..." >&2
  (cd "$EVAL_HOME/work" && exec nohup "$GENTS" server --home "$EVAL_HOME" --http-port "$PORT" \
    </dev/null >"$EVAL_HOME/server.log" 2>&1) &
  EVAL_SERVER_PID=$!
  echo "$EVAL_SERVER_PID" >"$EVAL_HOME/server.pid"
  disown
  for _ in $(seq 1 90); do
    served && break
    if ! kill -0 "$EVAL_SERVER_PID" 2>/dev/null; then
      echo "the eval server exited during startup:" >&2
      tail -n 20 "$EVAL_HOME/server.log" >&2
      exit 1
    fi
    sleep 2
  done
  served || { echo "the eval home did not become ready; see $EVAL_HOME/server.log" >&2; tail -n 20 "$EVAL_HOME/server.log" >&2; exit 1; }
fi

# The trial copies the backend, sampling, profile and the profile's bound
# InferenceExecution the run freezes; the ladder profile reuses the execution
# `gents init` created for the home's default profile.
PROFILE_ID="$DID:ladder-$TARGET"
INFERENCE="$EVAL_HOME/inference-$TARGET"
python3 - "$TARGET_FILE" "$DID" "$PER_TRIAL" "$INFERENCE" "$REASONING" "$TEMPERATURE" "$TOP_P" <<'PY'
import json, os, sys
target, did, per_trial, root, reasoning, temperature, top_p = sys.argv[1:8]
t = json.load(open(target))
name = os.path.basename(target)[:-5]
backend = dict(t["inference_backends"][0])
backend.update(backend_id=f"{did}:ladder-{name}", node_did=did, max_concurrent=int(per_trial))
sampling = {"sampling_id": f"{did}:ladder-{name}-sampling", "node_did": did,
            "display_name": f"Ladder {name}", "temperature": float(temperature), "top_p": float(top_p)}
profile = {"profile_id": f"{did}:ladder-{name}", "node_did": did, "display_name": f"Ladder {name}",
           "backend_id": backend["backend_id"], "model_name": t["inference_profiles"][0]["model_name"],
           "reasoning_effort": reasoning, "sampling_id": sampling["sampling_id"],
           "execution_id": f"{did}:default-profile-execution"}
os.makedirs(root, exist_ok=True)
json.dump({"manifest_version": 1, "name": "ladder_inference", "version": "0.1.0",
           "description": "The ladder's trial inference binding", "authors": ["gents-ai contributors"],
           "tags": ["eval"], "kind": "documents", "assets": ["pack_config.json"], "config": "pack_config.json"},
          open(f"{root}/manifest.json", "w"))
json.dump({"node": {"default_agent_id": f"{did}:default"}, "inference_backends": [backend], "inference_sampling": [sampling],
           "inference_profiles": [profile]}, open(f"{root}/pack_config.json", "w"), indent=2)
PY
"$GENTS" config apply --root "$INFERENCE" --bind-node-did home --home "$EVAL_HOME" >/dev/null
echo "profile $PROFILE_ID: $MODEL, reasoning $REASONING, temperature $TEMPERATURE, top_p $TOP_P, $PER_TRIAL call(s) per trial" >&2

# The subject is the Engineer this checkout seeds: its canonical prompt and grant,
# copied over the pack's so the cell never drifts from gents_protocol.
SUBJECT="$EVAL_HOME/engineer_subject-$SHA"
rm -rf "$SUBJECT" && cp -R "${GENTS_EVAL_SUBJECT:-$LADDER/engineer_subject}" "$SUBJECT"
cp "$ROOT/crates/gents-protocol/prompts/engineer.md" "$SUBJECT/agents/engineer/system_prompt.md"
python3 - "$SUBJECT/pack_config.json" "$ROOT/crates/gents-protocol/presets/engineer-self-config.json" <<'PY'
import json, sys
config, grant = sys.argv[1], sys.argv[2]
c = json.load(open(config))
c["tools"][0]["self_config"] = json.load(open(grant))
json.dump(c, open(config, "w"), indent=2)
PY

STAMP=$(python3 -c 'import datetime,uuid; print(datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d-%H%M%S-%f") + "-" + uuid.uuid4().hex[:8])')
PLAN="$EVAL_HOME/batch-$STAMP.json"
PLAN_DRAFT="$PLAN.tmp"
printf '{"runs":[]}\n' >"$PLAN_DRAFT"
WATCH=${GENTS_EVAL_WATCH:-auto}
if [ "$WATCH" = auto ]; then WATCH=0; [ ! -t 1 ] || WATCH=1; fi
if [ "$WATCH" = 1 ] || [ "$WATCH" = web ]; then
  WATCH=web
  WEB_PORT=${GENTS_EVAL_WEB_PORT:-9495}
  if ! python3 "$ROOT/scripts/evals/watch-web.py" --root "$(dirname "$EVAL_HOME")" --port "$WEB_PORT" --ensure --open; then
    echo "web viewer unavailable; opening the terminal watcher instead" >&2
    WATCH=tui
  fi
fi
for level in "${SELECTED[@]}"; do
  SUITE_PATH=$(python3 -c 'import json,sys; print(next(s["path"] for s in json.load(open(sys.argv[1]))["suites"] if s["id"]==sys.argv[2]))' "$MATRIX" "$level")
  DEFINITION_ID=$(python3 -c 'import json,sys; print(next(s["definition_id"] for s in json.load(open(sys.argv[1]))["suites"] if s["id"]==sys.argv[2]))' "$MATRIX" "$level")
  RUN_SUBJECT=$SUBJECT
  if [ "$level" = sessions ]; then
    RUN_SUBJECT="$EVAL_HOME/sessions_subject-$SHA"
    rm -rf "$RUN_SUBJECT"
    cp -R "$FIXTURES/sessions/subject" "$RUN_SUBJECT"
    cp "$ROOT/crates/gents-protocol/prompts/engineer.md" "$RUN_SUBJECT/agents/engineer/system_prompt.md"
    python3 - "$RUN_SUBJECT/pack_config.json" "$ROOT/crates/gents-protocol/presets/engineer-self-config.json" <<'PYSESSIONS'
import json, sys
config, grant = sys.argv[1:]
c = json.load(open(config))
c["tools"][0]["self_config"] = json.load(open(grant))
json.dump(c, open(config, "w"), indent=2)
PYSESSIONS
  fi
  if [ "$level" = factory-setup ]; then
    RUN_SUBJECT="$EVAL_HOME/factory_subject-$SHA"
    rm -rf "$RUN_SUBJECT"
    cp -R "$FIXTURES/factory_setup/engineer_kspec" "$RUN_SUBJECT"
    cp "$ROOT/crates/gents-protocol/prompts/engineer.md" "$RUN_SUBJECT/engineer/system_prompt.md"
  fi
  # A definition pack's empty node would clear the home's default
  # agent, and the served home then refuses to restart; keep the default.
  DEFINITION="$EVAL_HOME/definitions/$level"
  rm -rf "$DEFINITION" && mkdir -p "$EVAL_HOME/definitions" && cp -R "${GENTS_EVAL_DEFINITION_SOURCE:-$FIXTURES/$SUITE_PATH}" "$DEFINITION"
  python3 -c 'import json,sys; p=sys.argv[1]; c=json.load(open(p)); c["node"]={"default_agent_id": sys.argv[2]}; json.dump(c, open(p,"w"), indent=2)' \
    "$DEFINITION/pack_config.json" "$DID:default"
  python3 - "$DEFINITION" "$RUN_SUBJECT" <<'PYENGINEERBASELINE'
import json, pathlib, sys
root, subject = map(pathlib.Path, sys.argv[1:])
config = json.loads((subject / "pack_config.json").read_text())
engineers = [agent for agent in config.get("agents", []) if agent.get("agent_id") == "engineer"]
if len(engineers) == 1:
    contexts = [context for context in config.get("contexts", []) if context.get("context_id") == engineers[0].get("context_id")]
    tools = [tool for tool in config.get("tools", []) if len(contexts) == 1 and tool.get("tools_id") == contexts[0].get("tools_id")]
    if len(tools) == 1:
        definition = json.loads((root / "pack_config.json").read_text())
        for evaluation in definition.get("eval_definitions", []):
            for relative in evaluation.get("cases", []):
                path = root / relative
                case = json.loads(path.read_text())
                changed = False
                for stage in case.get("stages", []):
                    captures = {capture.get("name"): capture for capture in stage.get("capture", [])}
                    for check in stage.get("checks", []):
                        group = check.get("params", {}).get("agents", {})
                        if check.get("check") != "crew_spec_match" or not isinstance(group, dict):
                            continue
                        if not all(captures.get(group.get(key), {}).get("collection") == collection
                                   and captures.get(group.get(key), {}).get("filter") == {"node_did": {"_eq": "$trial"}}
                                   for key, collection in [("agents", "Agent"), ("contexts", "AgentContext"), ("tools", "Tools")]):
                            continue
                        for expected in group.get("expect", []):
                            if expected.get("agent_id") != "engineer" or not any(
                                item.get("field") == "tools_id" and item.get("equals") == tools[0]["tools_id"]
                                for item in expected.get("context", [])
                            ):
                                continue
                            for assertion in expected.get("tools", []):
                                if "equals" not in assertion:
                                    continue
                                value = tools[0]
                                for part in assertion["field"].split("."):
                                    value = value.get(part) if isinstance(value, dict) else None
                                if assertion["equals"] == value:
                                    continue
                                known_baselines = {
                                    "built_ins": (None, {"enable_schema_tool": True}),
                                    "self_config.self_config_categories": (
                                        ["agent", "tools", "profile", "node", "backend", "mcp_service", "automation"],
                                        ["node", "agent", "tools", "profile", "backend", "mcp_service", "automation"],
                                    ),
                                }
                                baseline = known_baselines.get(assertion["field"])
                                if baseline is None or assertion["equals"] != baseline[0] or value != baseline[1]:
                                    raise ValueError(
                                        f"{path}: unexpected Engineer invariant mismatch for {assertion['field']}: "
                                        f"expected {assertion['equals']!r}, input {value!r}"
                                    )
                                assertion["equals"] = value
                                changed = True
                if changed:
                    path.write_text(json.dumps(case, indent=2) + "\n")
PYENGINEERBASELINE
  "$GENTS" config apply --root "$DEFINITION" --bind-node-did home --home "$EVAL_HOME" >/dev/null
  for split in $SPLITS; do
    if ! python3 - "$DEFINITION" "$split" <<'PYSPLIT'
import json,pathlib,sys
root=pathlib.Path(sys.argv[1]); definition=json.load(open(root/'pack_config.json'))['eval_definitions'][0]
sys.exit(0 if any(json.load(open(root/p))['split']==sys.argv[2] for p in definition['cases']) else 1)
PYSPLIT
    then continue; fi
    RUN_ID="ladder-$level-$SHA-$TARGET-$split-$STAMP"
    cat >&2 <<EOF

== $level ($split): $TRIALS trials per case, $TRIAL_CONCURRENCY trials at once, $PER_TRIAL call(s) per trial, $MODEL at $ENDPOINT
watch:  $GENTS eval watch $RUN_ID --home $EVAL_HOME
EOF
    python3 - "$PLAN_DRAFT" "$DEFINITION_ID" "$RUN_SUBJECT" "$PROFILE_ID" "$split" "$TRIALS" "$TRIAL_CONCURRENCY" "$RUN_ID" <<'PYPLAN'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
definition, subject, profile, split, trials, concurrency, run_id = sys.argv[2:]
plan = json.loads(path.read_text())
args = [definition, "--cell", f"engineer={subject}:engineer", "--profile", f"engineer={profile}",
        "--split", split, "--trials", trials, "--concurrency", concurrency, "--run-id", run_id]
if any(run["args"][run["args"].index("--run-id") + 1] == run_id for run in plan["runs"]):
    sys.exit(f"duplicate run id: {run_id}")
plan["runs"].append({"args": args})
path.write_text(json.dumps(plan, indent=2) + "\n")
PYPLAN
  done
done
python3 - "$PLAN_DRAFT" "$PLAN" <<'PYFINALPLAN'
import json, os, pathlib, sys
source, destination = map(pathlib.Path, sys.argv[1:])
plan = json.loads(source.read_text())
if not plan["runs"]:
    sys.exit("selected suites and splits contain no runs")
os.link(source, destination)
source.unlink()
PYFINALPLAN
BATCH_LOG="$EVAL_HOME/batch-$STAMP.log"
FIRST_RUN_ID=$(python3 -c 'import json,sys; a=json.load(open(sys.argv[1]))["runs"][0]["args"]; print(a[a.index("--run-id")+1])' "$PLAN")
echo "saved batch plan: $PLAN" >&2
echo "global active trial cap: $TRIAL_CONCURRENCY (floor($CONCURRENCY / $PER_TRIAL)); provider/background calls are not globally capped" >&2
(cd "$EVAL_HOME/work" && exec "$GENTS" eval batch --plan "$PLAN" --concurrency "$TRIAL_CONCURRENCY" --home "$EVAL_HOME") >"$BATCH_LOG" 2>&1 &
BATCH_PID=$!
WATCH_COMMAND="$GENTS eval watch $FIRST_RUN_ID --home $EVAL_HOME"
if [ "$WATCH" = web ]; then WATCH_COMMAND="http://127.0.0.1:$WEB_PORT"; fi
trap 'echo "Batch continues in process $BATCH_PID. Plan: $PLAN. Watch: $WATCH_COMMAND" >&2; disown "$BATCH_PID" 2>/dev/null || true; exit 130' INT
echo "batch log: $BATCH_LOG" >&2
if [ "$WATCH" = tui ]; then
  while kill -0 "$BATCH_PID" 2>/dev/null; do
    if [ -f "$EVAL_HOME/eval/runs/$FIRST_RUN_ID/progress.json" ] || [ -f "$EVAL_HOME/eval/runs/$FIRST_RUN_ID/report.json" ]; then
      "$GENTS" eval watch "$FIRST_RUN_ID" --home "$EVAL_HOME" || true
      break
    fi
    sleep 1
  done
fi
status=0
wait "$BATCH_PID" || status=$?
trap - INT
if [ "$status" != 0 ]; then tail -n 20 "$BATCH_LOG" >&2; fi
echo "resume saved batch: $GENTS eval batch --plan $PLAN --concurrency $TRIAL_CONCURRENCY --home $EVAL_HOME" >&2
echo "saved plan: $PLAN; inspect each run with $GENTS eval show <run-id> --home $EVAL_HOME" >&2
echo "the eval home stays served on $PORT; stop it with: kill \$(cat $EVAL_HOME/server.pid)" >&2
exit "$status"
