#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Run the local two-node P2P demo and launch the desktop fleet UI on top.

The demo reuses scripts/demo-p2p-two-node.sh as two runtimes named Orchestrator
and Worker, tightens their tool surface (drops defra_query) and lets the
Orchestrator delegate to the Worker on node B via a cross-node 'worker'
subagent, then launches the Tauri dev app. The Fleet Dashboard shows both
runtimes; ask the Orchestrator to use its worker subagent and watch the child
request run on the Worker node and its result replicate back.

Environment:
  DEFRA_AGENT_BIN                    Path to defra-agent. Default:
                                     target/debug/defra-agent.
  DEFRA_AGENT_DESKTOP_BIN            Path to defra-agent-desktop. Default:
                                     target/debug/defra-agent-desktop.
  DEFRA_AGENT_DESKTOP_DEMO_ROOT      Demo state root. Defaults to mktemp.
  DEFRA_AGENT_DESKTOP_DEMO_KEEP=1    Keep homes/logs/runtimes after exit.
  DEFRA_AGENT_DESKTOP_DEMO_LAUNCH=0  Seed the demo but do not launch Tauri.
                                     Keeps state by default unless KEEP=0 is
                                     explicitly set.
  DEFRA_AGENT_DESKTOP_DEMO_ALLOW_UNAVAILABLE_BACKEND=1
                                     Launch even when the configured model
                                     backend cannot be checked locally.
  DEFRA_AGENT_DEMO_BACKEND_PRESET    Passed through to the two-node demo.
                                     Use openai with OPENAI_API_KEY for hosted chat.
  DEFRA_AGENT_DEMO_INFERENCE_URL     Passed through to the two-node demo.
  DEFRA_AGENT_DEMO_MODEL             Passed through to the two-node demo.
  DEFRA_AGENT_DEMO_API_KEY_ENV_VAR   Passed through to the two-node demo.
  AGENT_A_HTTP_PORT                  Orchestrator HTTP port. Default: 19791.
  AGENT_B_HTTP_PORT                  Worker HTTP port. Default: 19792.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

STEP=0
say() {
  STEP=$((STEP + 1))
  echo
  echo "==> [step $STEP] $*"
}

truthy() {
  case "${1:-}" in
    1 | true | TRUE | yes | YES | on | ON) return 0 ;;
    *) return 1 ;;
  esac
}

need jq
need curl

ROOT="${DEFRA_AGENT_DESKTOP_DEMO_ROOT:-$(mktemp -d "${TMPDIR:-/tmp}/defra-agent-desktop-demo.XXXXXX")}"
KEEP_WAS_SET="${DEFRA_AGENT_DESKTOP_DEMO_KEEP+x}"
KEEP="${DEFRA_AGENT_DESKTOP_DEMO_KEEP:-0}"
LAUNCH="${DEFRA_AGENT_DESKTOP_DEMO_LAUNCH:-1}"
P2P_ROOT="$ROOT/p2p"
DESKTOP_HOME="$ROOT/desktop"
STATE_FILE="$P2P_ROOT/demo-state.json"
PID_FILE="$P2P_ROOT/demo-pids.txt"
AGENT_A_HTTP_PORT="${AGENT_A_HTTP_PORT:-${AMY_HTTP_PORT:-19791}}"
AGENT_B_HTTP_PORT="${AGENT_B_HTTP_PORT:-${CODING_HTTP_PORT:-19792}}"
DESKTOP_BIN="${DEFRA_AGENT_DESKTOP_BIN:-target/debug/defra-agent-desktop}"
AGENT_BIN="${DEFRA_AGENT_BIN:-target/debug/defra-agent}"
MODEL_URL="${DEFRA_AGENT_DEMO_INFERENCE_URL:-http://127.0.0.1:8080/v1}"
DEFAULT_LOCAL_MODEL_NAME="google/gemma-4-12B-it-qat-q4_0-gguf"
MODEL_NAME="${DEFRA_AGENT_DEMO_MODEL:-}"
BACKEND_PRESET="${DEFRA_AGENT_DEMO_BACKEND_PRESET:-}"
ALLOW_UNAVAILABLE_BACKEND="${DEFRA_AGENT_DESKTOP_DEMO_ALLOW_UNAVAILABLE_BACKEND:-0}"
if [[ -z "$BACKEND_PRESET" && -z "$MODEL_NAME" ]]; then
  MODEL_NAME="$DEFAULT_LOCAL_MODEL_NAME"
fi

if [[ -z "$ROOT" || "$ROOT" == "/" || "$ROOT" == "$HOME" ]]; then
  echo "refusing unsafe demo root: $ROOT" >&2
  exit 1
fi

should_keep_demo() {
  if [[ "$KEEP" == "1" ]]; then
    return 0
  fi
  if [[ -z "$KEEP_WAS_SET" ]] && ! truthy "$LAUNCH"; then
    return 0
  fi
  return 1
}

stop_pid() {
  local pid="$1"
  [[ -n "$pid" ]] || return 0
  kill -0 "$pid" >/dev/null 2>&1 || return 0
  kill "$pid" >/dev/null 2>&1 || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    kill -0 "$pid" >/dev/null 2>&1 || return 0
    sleep 0.2
  done
  kill -9 "$pid" >/dev/null 2>&1 || true
}

stop_runtimes() {
  local pid_source=""
  if [[ -f "$STATE_FILE" ]]; then
    pid_source="$STATE_FILE"
    while read -r pid; do
      stop_pid "$pid"
    done < <(jq -r '.pids[]?' "$STATE_FILE")
  elif [[ -f "$PID_FILE" ]]; then
    pid_source="$PID_FILE"
    while read -r pid; do
      stop_pid "$pid"
    done <"$PID_FILE"
  fi
  if [[ -n "$pid_source" ]]; then
    echo "  Stopped P2P runtimes recorded in $pid_source"
  fi
}

cleanup() {
  # Teardown is best-effort; don't let a failing kill re-enter the traps or
  # let set -e abort partway through.
  trap - EXIT INT TERM ERR
  set +e
  # Only preserve state if we actually seeded a demo; an early failure (e.g. a
  # busy port) leaves nothing worth keeping.
  if should_keep_demo && [[ -f "$STATE_FILE" ]]; then
    echo
    echo "Keeping desktop demo state:"
    echo "  $ROOT"
    echo "Stop P2P runtimes manually with:"
    jq -r '.pids[]? | "  kill \(.)"' "$STATE_FILE"
    return
  fi

  echo
  echo "Cleaning up desktop demo"
  stop_runtimes
  rm -rf "$ROOT"
  echo "  Removed $ROOT"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'status=$?; echo "demo-desktop-two-node failed (exit $status); see logs under $P2P_ROOT/logs if present" >&2' ERR

print_live_chat_backend_note() {
  local endpoint
  endpoint="$(effective_backend_url)"
  local model_label="${MODEL_NAME:-"(preset default)"}"

  echo
  if [[ -n "$BACKEND_PRESET" ]]; then
    echo "Live chat backend: preset=$BACKEND_PRESET endpoint=$endpoint model=$model_label"
    local key_var
    key_var="$(backend_api_key_env_var)"
    if [[ -n "$key_var" && -z "${!key_var:-}" ]]; then
      echo "  $key_var is not set, so live chat will fail until you export it."
    fi
    return
  fi

  echo "Live chat backend: endpoint=$endpoint model=$model_label"
  if is_local_backend_url "$endpoint"; then
    if curl -fsS --max-time 2 "$endpoint/models" >/dev/null 2>&1; then
      echo "  Local inference endpoint is reachable."
    else
      echo "  Local inference endpoint is not reachable."
      echo "  Start it before sending chat messages:"
      echo "    llama-server -hf $MODEL_NAME"
      echo "  Or rerun with hosted inference:"
      echo "    DEFRA_AGENT_DEMO_BACKEND_PRESET=openai DEFRA_AGENT_DEMO_MODEL=gpt-4.1-mini OPENAI_API_KEY=... make demo-desktop-two-node"
    fi
  fi
}

effective_backend_url() {
  case "$BACKEND_PRESET" in
    openai)
      echo "${DEFRA_AGENT_DEMO_INFERENCE_URL:-https://api.openai.com/v1}"
      ;;
    openrouter)
      echo "${DEFRA_AGENT_DEMO_INFERENCE_URL:-https://openrouter.ai/api/v1}"
      ;;
    ollama)
      echo "${DEFRA_AGENT_DEMO_INFERENCE_URL:-http://localhost:11434/v1}"
      ;;
    vllm)
      echo "${DEFRA_AGENT_DEMO_INFERENCE_URL:-http://127.0.0.1:8000/v1}"
      ;;
    llama-cpp)
      echo "${DEFRA_AGENT_DEMO_INFERENCE_URL:-http://127.0.0.1:8080/v1}"
      ;;
    *)
      echo "$MODEL_URL"
      ;;
  esac
}

backend_api_key_env_var() {
  if [[ -n "${DEFRA_AGENT_DEMO_API_KEY_ENV_VAR:-}" ]]; then
    echo "$DEFRA_AGENT_DEMO_API_KEY_ENV_VAR"
    return
  fi
  case "$BACKEND_PRESET" in
    openai)
      echo "OPENAI_API_KEY"
      ;;
    openrouter)
      echo "OPENROUTER_API_KEY"
      ;;
  esac
}

backend_requires_explicit_model() {
  case "$BACKEND_PRESET" in
    generic-openai-compatible | openai | openrouter | vllm) return 0 ;;
    *) return 1 ;;
  esac
}

is_local_backend_url() {
  case "$1" in
    http://127.0.0.1:* | http://localhost:*) return 0 ;;
    *) return 1 ;;
  esac
}

ensure_live_chat_backend_ready() {
  if ! truthy "$LAUNCH" || truthy "$ALLOW_UNAVAILABLE_BACKEND"; then
    return
  fi

  if backend_requires_explicit_model && [[ -z "$MODEL_NAME" ]]; then
    cat >&2 <<EOF
The desktop demo is configured with backend preset '$BACKEND_PRESET', but no model was set.

Set DEFRA_AGENT_DEMO_MODEL to a model supported by that provider, for example:
  DEFRA_AGENT_DEMO_BACKEND_PRESET=openai \\
  DEFRA_AGENT_DEMO_MODEL=gpt-4.1-mini \\
  OPENAI_API_KEY=... \\
    make demo-desktop-two-node
EOF
    exit 1
  fi

  if [[ "$BACKEND_PRESET" == "chatgpt-codex" ]]; then
    cat >&2 <<EOF
The desktop demo is configured with backend preset 'chatgpt-codex'.

That backend needs a DefraDB OAuthCredential document for each runtime before
live chat can work. Prepare the demo without launching, then run codex-login for
the printed Amy/Coding homes and agent DIDs:

  DEFRA_AGENT_DEMO_BACKEND_PRESET=chatgpt-codex \\
  DEFRA_AGENT_DESKTOP_DEMO_LAUNCH=0 \\
  DEFRA_AGENT_DESKTOP_DEMO_KEEP=1 \\
    make demo-desktop-two-node

For a one-command hosted demo, use OpenAI API auth instead:
  DEFRA_AGENT_DEMO_BACKEND_PRESET=openai \\
  DEFRA_AGENT_DEMO_MODEL=gpt-4.1-mini \\
  OPENAI_API_KEY=... \\
    make demo-desktop-two-node
EOF
    exit 1
  fi

  local key_var
  key_var="$(backend_api_key_env_var)"
  if [[ -n "$key_var" && -z "${!key_var:-}" ]]; then
    cat >&2 <<EOF
The desktop demo is configured with backend preset '$BACKEND_PRESET', but $key_var is not set.

Export the key before launching:
  export $key_var=...
EOF
    exit 1
  fi

  local endpoint
  endpoint="$(effective_backend_url)"
  if is_local_backend_url "$endpoint" && ! curl -fsS --max-time 2 "$endpoint/models" >/dev/null 2>&1; then
    cat >&2 <<EOF
The desktop demo is configured to use $endpoint, but that local inference server is not reachable.

Start local inference first:
  llama-server -hf $DEFAULT_LOCAL_MODEL_NAME

Or run the demo with hosted inference:
  DEFRA_AGENT_DEMO_BACKEND_PRESET=openai \\
  DEFRA_AGENT_DEMO_MODEL=gpt-4.1-mini \\
  OPENAI_API_KEY=... \\
    make demo-desktop-two-node

To inspect the seeded fleet UI without sending live chat, set:
  DEFRA_AGENT_DESKTOP_DEMO_ALLOW_UNAVAILABLE_BACKEND=1
EOF
    exit 1
  fi
}

port_in_use() {
  # Connect to a loopback listener using bash's /dev/tcp; success means busy.
  local port="$1"
  (exec 3<>"/dev/tcp/127.0.0.1/${port}") 2>/dev/null
}

ensure_ports_available() {
  if [[ "$AGENT_A_HTTP_PORT" == "$AGENT_B_HTTP_PORT" ]]; then
    echo "AGENT_A_HTTP_PORT and AGENT_B_HTTP_PORT must differ (both are $AGENT_A_HTTP_PORT)" >&2
    exit 1
  fi
  local conflict=0 entry name port
  for entry in "orchestrator:${AGENT_A_HTTP_PORT}" "worker:${AGENT_B_HTTP_PORT}"; do
    name="${entry%%:*}"
    port="${entry##*:}"
    if port_in_use "$port"; then
      echo "HTTP port $port (for $name) is already in use" >&2
      conflict=1
    fi
  done
  if [[ "$conflict" == "1" ]]; then
    cat >&2 <<EOF
Refusing to start: a demo HTTP port is busy (a previous demo may still be running).
Free the port, or choose different ports:
  AGENT_A_HTTP_PORT=<free> AGENT_B_HTTP_PORT=<free> make demo-desktop-two-node
Inspect a listener with:
  lsof -nP -iTCP:<port> -sTCP:LISTEN
EOF
    exit 1
  fi
}


# Fail fast on configuration before doing any slow work (build, node bring-up).
ensure_live_chat_backend_ready
say "Checking demo HTTP ports are free"
ensure_ports_available
echo "  Orchestrator port: $AGENT_A_HTTP_PORT free"
echo "  Worker port:         $AGENT_B_HTTP_PORT free"

if [[ ! -x "$DESKTOP_BIN" ]]; then
  say "Building defra-agent-desktop launcher"
  cargo build -p defra-agent-desktop
fi

say "Starting the two-node P2P substrate (orchestrator + worker, ~30-60s)"
# Capture the substrate's verbose runbook output to a log; surface it only on
# failure so the desktop demo's own output stays focused.
if DEFRA_AGENT_BIN="$AGENT_BIN" \
   DEFRA_AGENT_DEMO_KEEP=1 \
   DEFRA_AGENT_DEMO_ROOT="$P2P_ROOT" \
   DEFRA_AGENT_DEMO_AGENT_A_NAME=orchestrator \
   DEFRA_AGENT_DEMO_AGENT_B_NAME=worker \
   AGENT_A_HTTP_PORT="$AGENT_A_HTTP_PORT" \
   AGENT_B_HTTP_PORT="$AGENT_B_HTTP_PORT" \
     scripts/demo-p2p-two-node.sh >"$ROOT/substrate.log" 2>&1; then
  echo "  Substrate up: orchestrator + worker paired, conversation data-plane replicating"
else
  echo "two-node substrate failed; tail of $ROOT/substrate.log:" >&2
  tail -n 30 "$ROOT/substrate.log" >&2
  exit 1
fi

if [[ ! -f "$STATE_FILE" ]]; then
  echo "two-node demo did not write state file: $STATE_FILE" >&2
  exit 1
fi

# node_a = orchestrator (delegates work over P2P),
# node_b = worker (runs the delegated cross-node subagent child).
ORCH_GRAPHQL="$(jq -r '.node_a.graphql' "$STATE_FILE")"
ORCH_DID="$(jq -r '.node_a.agent_did' "$STATE_FILE")"
WORKER_GRAPHQL="$(jq -r '.node_b.graphql' "$STATE_FILE")"
WORKER_DID="$(jq -r '.node_b.agent_did' "$STATE_FILE")"
REQUEST_ID="$(jq -r '.request.id' "$STATE_FILE")"
REQUEST_SESSION_ID="$(jq -r '.request.session_id' "$STATE_FILE")"

say "Seeding isolated desktop fleet state"
# Capture --json stdout to a file, then pretty-print non-fatally: a cosmetic jq
# display must never abort the demo (only a failing init should).
seed_summary() {
  local file="$1"
  jq '{label, agent_did, graphql, peer_directory}' "$file" 2>/dev/null \
    || { echo "  (raw init summary)"; cat "$file"; }
}

"$DESKTOP_BIN" init \
  --desktop-home "$DESKTOP_HOME" \
  --status-endpoint "$ORCH_GRAPHQL" \
  --label "Orchestrator" \
  --dangerously-overwrite \
  --json >"$ROOT/desktop-init-orchestrator.json"
seed_summary "$ROOT/desktop-init-orchestrator.json"

"$DESKTOP_BIN" init \
  --desktop-home "$DESKTOP_HOME" \
  --status-endpoint "$WORKER_GRAPHQL" \
  --label "Worker" \
  --json >"$ROOT/desktop-init-worker.json"
seed_summary "$ROOT/desktop-init-worker.json"

say "Tightening tool surface + enabling cross-node delegation"
# config tools set updates the ToolSelection document; the runtime must then
# reconcile before new chat turns see the updated surface.
gql() { curl -gfsS -H 'content-type: application/json' --data-binary "$(jq -n --arg q "$1" '{query:$q}')" "$2"; }
runtime_generation() { gql '{ AgentRuntime { active_generation } }' "$1" | jq -r '.data.AgentRuntime[0].active_generation // 0'; }
wait_reconcile() {
  local ep="$1" prev="$2" label="$3" row gen phase
  for _ in $(seq 1 80); do
    row="$(gql '{ AgentRuntime { active_generation reconcile_phase } }' "$ep" | jq -c '.data.AgentRuntime[0] // {}')"
    gen="$(jq -r '.active_generation // 0' <<<"$row")"
    phase="$(jq -r '.reconcile_phase // ""' <<<"$row")"
    if [[ "$gen" -gt "$prev" && "$phase" == "idle" ]]; then
      echo "  $label reconciled (generation $gen)"
      return 0
    fi
    sleep 0.5
  done
  echo "  warning: $label reconcile not confirmed; continuing" >&2
}

# Worker (node B, the delegate): drop defra_query and accept cross-deployment
# spawns from the paired orchestrator (the receiver-side gate, #377).
WORKER_GEN="$(runtime_generation "$WORKER_GRAPHQL")"
"$AGENT_BIN" config tools set --graphql "$WORKER_GRAPHQL" --agent-did "$WORKER_DID" \
  --selection-id "$WORKER_DID:default-tools" \
  --enable-defra-query false \
  --subagent-allow-cross-deployment true >/dev/null \
  && echo "  worker: defra_query disabled, cross-deployment spawns accepted"
wait_reconcile "$WORKER_GRAPHQL" "$WORKER_GEN" "worker"

# Orchestrator (node A): drop defra_query and delegate to the worker on node B.
# Cross-node delegation requires background await (foreground remote spawns are
# rejected), so background must be enabled and the target points at the worker DID.
ORCH_GEN="$(runtime_generation "$ORCH_GRAPHQL")"
"$AGENT_BIN" config tools set --graphql "$ORCH_GRAPHQL" --agent-did "$ORCH_DID" \
  --selection-id "$ORCH_DID:default-tools" \
  --enable-defra-query false \
  --subagent-spawn-enabled true \
  --subagent-background-enabled true \
  --subagent-allow-cross-deployment true \
  --subagent-target "{\"name\":\"worker\",\"agent_did\":\"$WORKER_DID\",\"behavior_id\":\"$WORKER_DID:default\",\"description\":\"Remote worker subagent on the worker node\"}" \
  >/dev/null \
  && echo "  orchestrator: defra_query disabled, cross-node spawn_subagent -> worker enabled"
wait_reconcile "$ORCH_GRAPHQL" "$ORCH_GEN" "orchestrator"

echo
echo "Resolved model-callable tool surface (defra-agent tools explain):"
printf '  orchestrator: '
"$AGENT_BIN" tools explain --graphql "$ORCH_GRAPHQL" --agent-did "$ORCH_DID" 2>/dev/null \
  | jq -r '.behaviors[0].surface.tool_names | join(", ")' 2>/dev/null || echo "(unavailable)"
printf '  worker:       '
"$AGENT_BIN" tools explain --graphql "$WORKER_GRAPHQL" --agent-did "$WORKER_DID" 2>/dev/null \
  | jq -r '.behaviors[0].surface.tool_names | join(", ")' 2>/dev/null || echo "(unavailable)"

# Pre-seed a delegation conversation so the Fleet UI has one to inspect. Best
# effort: it only completes once a tool-calling backend is reachable; a real
# model decides whether to delegate (see the suggested prompts below).
SUBAGENT_SEED="$("$AGENT_BIN" request submit --graphql "$ORCH_GRAPHQL" --agent-did "$ORCH_DID" \
  --content "Delegate to the worker subagent: ask it to describe the worker node, then summarize its reply." --no-wait 2>/dev/null || true)"
SUBAGENT_SESSION_ID="$(jq -r '.session_id // empty' <<<"$SUBAGENT_SEED" 2>/dev/null)"

say "Desktop demo ready"
echo "  Root:               $ROOT"
echo "  Desktop home:       $DESKTOP_HOME"
echo "  Peer file:          $DESKTOP_HOME/peers.json"
echo "  Orchestrator GraphQL: $ORCH_GRAPHQL"
echo "  Worker GraphQL:       $WORKER_GRAPHQL"
echo "  Seed request:       $REQUEST_ID (session $REQUEST_SESSION_ID)"
[[ -n "$SUBAGENT_SESSION_ID" ]] && echo "  Delegation session: $SUBAGENT_SESSION_ID (orchestrator -> worker on node B)"
print_live_chat_backend_note
echo
echo "Suggested prompts for the Orchestrator (crafted to elicit cross-node delegation):"
echo "  - Delegate to the worker subagent: ask it to summarize what a worker node does, then report back."
echo "  - Use your worker subagent to draft a one-line status, then return it to me."
echo
echo "Manual checks:"
echo "  jq . $DESKTOP_HOME/peers.json"
echo "  $AGENT_BIN tools explain --graphql $ORCH_GRAPHQL --agent-did $ORCH_DID | jq .behaviors[0].surface"
echo "  $AGENT_BIN request show --graphql $ORCH_GRAPHQL $REQUEST_ID --output json | jq .request"
echo
echo "In the app:"
echo "  1. Open Fleet Dashboard and confirm Orchestrator + Worker are listed."
echo "  2. Open Chat for the Orchestrator and use a suggested prompt above; watch the"
echo "     spawn_subagent tool call delegate to the worker."
echo "  3. Open Chat for the Worker (node B) to see the child request run there and"
echo "     its result replicate back to the Orchestrator over P2P."
echo "  4. For live responses, make sure the chat backend above is reachable on BOTH"
echo "     nodes (the worker now executes the delegated child)."

if ! truthy "$LAUNCH"; then
  echo
  echo "Launch manually with:"
  echo "  cd apps/desktop-tauri"
  echo "  DEFRA_AGENT_DESKTOP_HOME=$DESKTOP_HOME DEFRA_AGENT_DESKTOP_PAIR_REMOTE_P2P=0 npm run tauri -- dev"
  exit 0
fi

need npm
if [[ ! -d apps/desktop-tauri/node_modules ]]; then
  echo "desktop npm dependencies are missing; run \`npm --prefix apps/desktop-tauri ci\` first" >&2
  exit 1
fi

say "Launching Tauri desktop app"
cd apps/desktop-tauri
DEFRA_AGENT_DESKTOP_HOME="$DESKTOP_HOME" \
DEFRA_AGENT_DESKTOP_PAIR_REMOTE_P2P=0 \
  npm run tauri -- dev
