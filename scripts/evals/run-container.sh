#!/usr/bin/env bash
# Run native evals with every trial in its own container.
#
#   scripts/evals/run-container.sh image [COMMIT] [--tag TAG]
#   scripts/evals/run-container.sh start --name RUN --image TAG --target FILE...
#       [--suite SUITE|all]... [--splits "train validation"] [--trials N]
#       [--per-replica N] [--max-running N] [--seed-base N] [--command-execution off|unrestricted]
#       [--subject DIR] [--definition DIR] [--trial-timeout SECS] [--disk SIZE]
#       [--cpus N] [--memory SIZE] [--pids N]
#   scripts/evals/run-container.sh resume|status|stop --name RUN
#
# image builds gents-eval:<commit> on the docker host from `git archive COMMIT`
# (default HEAD) with this checkout's scripts/evals/container/Dockerfile: the
# Linux CLI with its core packs embedded, the archives, and the parts of
# COMMIT's tree run-ladder.sh reads, without any suite's fixtures.
# start stages a run under $GENTS_EVAL_CONTAINER_ROOT/RUN on the docker host
# (with the image commit's suite fixtures, read from this repository) and
# starts its orchestrator there, detached: one slot per case and trial, each
# attempt a `--network none` container (non-root, read-only image, no
# capabilities, CPU/memory/pid limits, a --disk tmpfs, default 2g, for its
# eval home and workspace, a --trial-timeout wall clock, default 3600s) that
# sees only its own suite's fixtures, reaches its replica's model through a
# relay socket opened for it alone, and runs the image's
# scripts/evals/run-ladder.sh for that one case.
# Each --target is a target file whose endpoint is http://127.0.0.1:<port> on
# the docker host; slots run --per-replica (default 2) at a time on each and
# --max-running (default no cap beyond that) in all.
# Every trial runs the Engineer the image's `gents init --engineer` makes
# (`gents utils eval subject`), with its suite's fixtures; --subject replaces
# it with a subject directory.
# --command-execution names the mode every trial's Engineer runs commands in
# (GENTS_EVAL_COMMAND_EXECUTION, default off): unrestricted keeps the host bash
# and background processes init gives the Engineer and sets
# GENTS_EVAL_ALLOW_HOST_BASH=1 inside the containers, so they hold there; such
# a run's summary is marked unauthenticated, since the trial shares a uid with
# its grader. resume restarts the orchestrator: finished slots stay, the others
# run again from a fresh eval home. status prints the
# slot table and totals (also in RUN/summary.txt and RUN/summary.json); stop
# ends the orchestrator, which kills the run's containers.
#
# GENTS_EVAL_CONTAINER_HOST: docker host, reached over ssh (default `local`, this
#   machine).
# GENTS_EVAL_CONTAINER_ROOT: run root on that host (default /var/tmp/gents-evals).
set -euo pipefail

usage() { sed -n "2,$(( $(grep -n '^set -euo pipefail' "$0" | cut -d: -f1) - 1 ))p" "$0" | sed 's/^# \{0,1\}//'; exit "${1:-2}"; }
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
HOST=${GENTS_EVAL_CONTAINER_HOST:-local}
RUNS=${GENTS_EVAL_CONTAINER_ROOT:-/var/tmp/gents-evals}
ID='^[A-Za-z0-9][A-Za-z0-9._-]*$'

on_host() {
  if [ "$HOST" = local ]; then bash -c "$1"; else ssh -o BatchMode=yes "$HOST" "$1"; fi
}
quoted() { printf '%q ' "$@"; }
# The checkout's commit, marked when the named paths differ from it.
revision() {
  local head
  head=$(git -C "$ROOT" rev-parse HEAD)
  if [ -n "$(git -C "$ROOT" status --porcelain -- "$@")" ]; then echo "$head-dirty"; else echo "$head"; fi
}
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT

[ $# -gt 0 ] || usage
COMMAND=$1; shift
NAME='' IMAGE='' TAG='' COMMIT='' SUBJECT='' DEFINITION=''
TARGETS=() SUITES=()
SPLITS="train validation" TRIALS=1 PER_REPLICA=2 MAX_RUNNING=0 SEED_BASE=1000 COMMAND_EXECUTION=off
CPUS=4 MEMORY=8g PIDS=4096 DISK=2g TRIAL_SECS=3600
while [ $# -gt 0 ]; do
  case "$1" in
    --name) NAME=$2; shift 2 ;;
    --image) IMAGE=$2; shift 2 ;;
    --tag) TAG=$2; shift 2 ;;
    --target) TARGETS+=("$2"); shift 2 ;;
    --suite) SUITES+=("$2"); shift 2 ;;
    --splits) SPLITS=$2; shift 2 ;;
    --trials) TRIALS=$2; shift 2 ;;
    --per-replica) PER_REPLICA=$2; shift 2 ;;
    --max-running) MAX_RUNNING=$2; shift 2 ;;
    --seed-base) SEED_BASE=$2; shift 2 ;;
    --command-execution) COMMAND_EXECUTION=$2; shift 2 ;;
    --subject) SUBJECT=$2; shift 2 ;;
    --definition) DEFINITION=$2; shift 2 ;;
    --trial-timeout) TRIAL_SECS=$2; shift 2 ;;
    --disk) DISK=$2; shift 2 ;;
    --cpus) CPUS=$2; shift 2 ;;
    --memory) MEMORY=$2; shift 2 ;;
    --pids) PIDS=$2; shift 2 ;;
    -h|--help) usage 0 ;;
    --*) usage ;;
    *) [ -z "$COMMIT" ] || usage; COMMIT=$1; shift ;;
  esac
done

image() {
  local commit tag context=$SCRATCH
  commit=$(git -C "$ROOT" rev-parse --verify "${COMMIT:-HEAD}^{commit}")
  tag=${TAG:-gents-eval:${commit:0:12}}
  git -C "$ROOT" archive --format=tar -o "$context/context.tar" "$commit"
  mkdir -p "$context/extra/.gents-eval-image"
  cp "$HERE/container/Dockerfile" "$context/extra/.gents-eval-image/Dockerfile"
  tar -rf "$context/context.tar" -C "$context/extra" .gents-eval-image/Dockerfile
  echo "building $tag on $HOST from $commit ..." >&2
  on_host "docker build --progress=plain $(quoted --build-arg "GENTS_COMMIT=$commit" \
    --label "gents.eval.dockerfile-commit=$(revision scripts/evals/container/Dockerfile)" \
    -f .gents-eval-image/Dockerfile -t "$tag") -" <"$context/context.tar"
  echo "$tag"
}

require_name() {
  [[ "$NAME" =~ $ID ]] || { echo "--name must be a plain directory name" >&2; exit 2; }
  RUN_DIR="$RUNS/$NAME"
}

launch() {
  on_host "cd $(quoted "$RUN_DIR") || exit 1
    test -d source || { echo 'this run predates the staged suite fixtures; start a new one' >&2; exit 2; }
    setsid nohup python3 -B launcher/orchestrate.py run $(quoted "$RUN_DIR") >>orchestrator.log 2>&1 </dev/null &
    sleep 3
    if [ -f orchestrator.pid ]; then echo \"orchestrator process \$(cat orchestrator.pid)\"; else tail -n 20 orchestrator.log; fi"
  cat >&2 <<EOF
run $NAME is orchestrated on $HOST under $RUN_DIR
follow: ssh $HOST tail -f $RUN_DIR/orchestrator.log
status: $0 status --name $NAME
resume: $0 resume --name $NAME
EOF
}

start() {
  require_name
  [[ -n "$IMAGE" && ${#TARGETS[@]} -gt 0 ]] || usage
  [ ${#SUITES[@]} -gt 0 ] || SUITES=(all)
  [[ "$TRIALS" =~ ^[1-9][0-9]*$ && "$PER_REPLICA" =~ ^[1-9][0-9]*$ && "$MAX_RUNNING" =~ ^[0-9]+$ &&
     "$SEED_BASE" =~ ^[0-9]+$ && "$TRIAL_SECS" =~ ^[1-9][0-9]*$ && "$DISK" =~ ^[1-9][0-9]*[kmgt]?$ &&
     "$COMMAND_EXECUTION" =~ ^(off|unrestricted)$ ]] || usage
  local suite
  for suite in "${SUITES[@]}"; do
    [[ "$suite" =~ $ID ]] || { echo "--suite $suite is not a suite id" >&2; exit 2; }
  done
  on_host "docker image inspect $(quoted "$IMAGE") >/dev/null" ||
    { echo "no image $IMAGE on $HOST; build it with $0 image" >&2; exit 2; }
  local commit stage=$SCRATCH/stage
  commit=$(on_host "docker image inspect --format '{{index .Config.Labels \"org.opencontainers.image.revision\"}}' $(quoted "$IMAGE")")
  git -C "$ROOT" cat-file -e "$commit^{commit}" 2>/dev/null ||
    { echo "image $IMAGE was built from $commit, which this repository lacks; fetch it" >&2; exit 2; }
  mkdir -p "$stage/launcher" "$stage/targets" "$stage/overrides" "$stage/logs" "$stage/slots" "$stage/relay" "$stage/source"
  cp "$HERE/container/orchestrate.py" "$HERE/container/relay.py" "$HERE/container/entrypoint.sh" "$HERE/suites.py" \
    "$stage/launcher/"
  git -C "$ROOT" archive --format=tar "$commit" -- scripts/evals/matrix.json crates/gents/tests/fixtures/configurator_evals |
    tar -x -C "$stage/source"
  local names=() target name
  for target in "${TARGETS[@]}"; do
    name=$(basename "$target" .json)
    [[ "$name" =~ $ID ]] || { echo "target file name $name is not a plain name" >&2; exit 2; }
    [ ! -e "$stage/targets/$name.json" ] || { echo "two --target files are named $name" >&2; exit 2; }
    cp "$target" "$stage/targets/$name.json"
    names+=("$name")
  done
  [ -z "$SUBJECT" ] || cp -R "$SUBJECT" "$stage/overrides/subject"
  [ -z "$DEFINITION" ] || cp -R "$DEFINITION" "$stage/overrides/definition"
  python3 - "$stage" "$NAME" "$IMAGE" "$commit" "$(revision scripts/evals/container scripts/evals/run-container.sh scripts/evals/suites.py)" \
    "$SPLITS" "$TRIALS" "$PER_REPLICA" "$MAX_RUNNING" "$SEED_BASE" "$COMMAND_EXECUTION" "$CPUS" "$MEMORY" "$PIDS" "$DISK" \
    "$TRIAL_SECS" "${#SUITES[@]}" "${SUITES[@]}" "${names[@]}" <<'PY'
import hashlib, json, pathlib, sys
(stage, name, image, commit, launcher, splits, trials, per_replica, max_running, seed_base, command_execution, cpus, memory,
 pids, disk, trial_secs, suite_count, *rest) = sys.argv[1:]
stage = pathlib.Path(stage)
suites, replicas = rest[:int(suite_count)], rest[int(suite_count):]
valid = {"train", "validation", "held_out"}
if not splits.split() or set(splits.split()) - valid:
    sys.exit("--splits must name train, validation or held_out")
launcher_files = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                  for path in sorted((stage / "launcher").iterdir())}
json.dump({"name": name, "image": image, "image_commit": commit, "launcher_commit": launcher,
           "launcher_sha256": launcher_files,
           "suites": suites, "splits": splits.split(), "trials": int(trials), "per_replica": int(per_replica),
           "max_running": int(max_running) or None,
           "seed_base": int(seed_base), "command_execution": command_execution, "replicas": replicas,
           "limits": {"cpus": cpus, "memory": memory, "pids": int(pids), "disk": disk,
                      "trial_secs": int(trial_secs)}},
          open(stage / "run.json", "w"), indent=2)
PY
  on_host "test ! -e $(quoted "$RUN_DIR") && mkdir -p $(quoted "$RUN_DIR")" ||
    { echo "$RUN_DIR exists on $HOST; resume it or pick another --name" >&2; exit 2; }
  tar -C "$stage" -cf - . | on_host "tar -C $(quoted "$RUN_DIR") -xf -"
  launch
}

case "$COMMAND" in
  image) image ;;
  start) start ;;
  resume)
    require_name
    on_host "test -f $(quoted "$RUN_DIR/run.json")" || { echo "no run $NAME under $RUNS on $HOST" >&2; exit 2; }
    launch ;;
  status)
    require_name
    on_host "python3 -B $(quoted "$RUN_DIR/launcher/orchestrate.py") status $(quoted "$RUN_DIR")" ;;
  stop)
    require_name
    on_host "pid=\$(cat $(quoted "$RUN_DIR/orchestrator.pid") 2>/dev/null) || { echo 'no orchestrator is running' >&2; exit 1; }
      if tr '\\0' ' ' </proc/\$pid/cmdline 2>/dev/null | grep -qF $(quoted "orchestrate.py run $RUN_DIR"); then
        kill \"\$pid\"
      else
        echo \"process \$pid is not this run's orchestrator; nothing stopped\" >&2; exit 1
      fi" ;;
  -h|--help|help) usage 0 ;;
  *) usage ;;
esac
