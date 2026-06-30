#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Run a local two-node defra-agent P2P demo.

The demo starts two isolated runtimes, creates an admin-signed network on Amy,
grants Coding membership, joins Coding with a v5 network-control invite, adds
conversation data-plane rows in both directions, waits for reconciled
replicators, then submits one no-wait request on Coding and verifies the
AgentRequest document appears on Amy.

Environment:
  DEFRA_AGENT_BIN                 Path to defra-agent binary. Defaults to
                                  target/debug/defra-agent, then PATH, then
                                  cargo build -p defra-agent-cli.
  DEFRA_AGENT_DEMO_ROOT           Demo state root. Defaults to mktemp under /tmp.
  DEFRA_AGENT_DEMO_KEEP=1         Keep homes/logs and leave servers running.
  DEFRA_AGENT_DEMO_BACKEND_PRESET Backend preset passed to init (for example:
                                  openai, openrouter, llama-cpp, vllm).
  DEFRA_AGENT_DEMO_INFERENCE_URL  Backend URL used by init.
                                  Default: http://127.0.0.1:8080/v1
  DEFRA_AGENT_DEMO_MODEL          Model name used by init. The default local
                                  path uses google/gemma-4-12B-it-qat-q4_0-gguf;
                                  backend presets use their CLI defaults unless
                                  this is set.
  DEFRA_AGENT_DEMO_API_KEY_ENV_VAR
                                  API-key env var stored in the backend document.
  DEFRA_AGENT_DEMO_SUBAGENT=1      Also prove cross-node subagent routing: node A
                                  delegates a 'worker' subagent that runs on node
                                  B and replicates back. Requires a real
                                  tool-calling backend (set a preset or URL).
  AGENT_A_HTTP_PORT               Amy HTTP port. Default: 19391
  AGENT_B_HTTP_PORT               Coding HTTP port. Default: 19392
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

need curl
need jq

if [[ -n "${DEFRA_AGENT_BIN:-}" ]]; then
  BIN="$DEFRA_AGENT_BIN"
elif [[ -x "target/debug/defra-agent" ]]; then
  BIN="target/debug/defra-agent"
elif command -v defra-agent >/dev/null 2>&1; then
  BIN="$(command -v defra-agent)"
else
  echo "building defra-agent CLI..." >&2
  cargo build -p defra-agent-cli
  BIN="target/debug/defra-agent"
fi

if [[ ! -x "$BIN" ]]; then
  echo "defra-agent binary is not executable: $BIN" >&2
  exit 1
fi

ROOT="${DEFRA_AGENT_DEMO_ROOT:-$(mktemp -d "${TMPDIR:-/tmp}/defra-agent-p2p-demo.XXXXXX")}"
KEEP="${DEFRA_AGENT_DEMO_KEEP:-0}"
MODEL_URL="${DEFRA_AGENT_DEMO_INFERENCE_URL:-http://127.0.0.1:8080/v1}"
DEFAULT_LOCAL_MODEL_NAME="google/gemma-4-12B-it-qat-q4_0-gguf"
MODEL_NAME="${DEFRA_AGENT_DEMO_MODEL:-}"
BACKEND_PRESET="${DEFRA_AGENT_DEMO_BACKEND_PRESET:-}"
if [[ -z "$BACKEND_PRESET" && -z "$MODEL_NAME" ]]; then
  MODEL_NAME="$DEFAULT_LOCAL_MODEL_NAME"
fi
API_KEY_ENV_VAR="${DEFRA_AGENT_DEMO_API_KEY_ENV_VAR:-}"
# Optional capstone: after proving replication, prove cross-node subagent
# delegation (node A delegates a child that runs on node B and replicates back).
# It needs a real tool-calling backend (set a preset or inference URL).
SUBAGENT_PROOF="${DEFRA_AGENT_DEMO_SUBAGENT:-0}"
# Agent names are parameterized so wrappers (e.g. the desktop demo) can relabel
# the two runtimes; the standalone runbook keeps amy/coding.
AGENT_A_NAME="${DEFRA_AGENT_DEMO_AGENT_A_NAME:-amy}"
AGENT_B_NAME="${DEFRA_AGENT_DEMO_AGENT_B_NAME:-coding}"
AGENT_A_HTTP_PORT="${AGENT_A_HTTP_PORT:-${AMY_HTTP_PORT:-19391}}"
AGENT_B_HTTP_PORT="${AGENT_B_HTTP_PORT:-${CODING_HTTP_PORT:-19392}}"

if [[ -z "$ROOT" || "$ROOT" == "/" || "$ROOT" == "$HOME" ]]; then
  echo "refusing unsafe demo root: '$ROOT'" >&2
  exit 1
fi
if [[ "$AGENT_A_HTTP_PORT" == "$AGENT_B_HTTP_PORT" ]]; then
  echo "AGENT_A_HTTP_PORT and AGENT_B_HTTP_PORT must differ (both are $AGENT_A_HTTP_PORT)" >&2
  exit 1
fi

AGENT_A_HOME="$ROOT/$AGENT_A_NAME"
AGENT_B_HOME="$ROOT/$AGENT_B_NAME"
AGENT_A_WORK="$ROOT/$AGENT_A_NAME-work"
AGENT_B_WORK="$ROOT/$AGENT_B_NAME-work"
LOG_DIR="$ROOT/logs"
STATE_FILE="$ROOT/demo-state.json"
PID_FILE="$ROOT/demo-pids.txt"
AGENT_A_GRAPHQL="http://127.0.0.1:${AGENT_A_HTTP_PORT}/api/v0/graphql"
AGENT_B_GRAPHQL="http://127.0.0.1:${AGENT_B_HTTP_PORT}/api/v0/graphql"

PIDS=()

cleanup() {
  if [[ "$KEEP" == "1" ]]; then
    echo
    echo "Keeping demo state and running servers:"
    echo "  $ROOT"
    echo "Stop servers manually with:"
    for pid in "${PIDS[@]:-}"; do
      echo "  kill $pid"
    done
    return
  fi

  for pid in "${PIDS[@]:-}"; do
    if kill -0 "$pid" >/dev/null 2>&1; then
      kill "$pid" >/dev/null 2>&1 || true
    fi
  done
  for pid in "${PIDS[@]:-}"; do
    wait "$pid" >/dev/null 2>&1 || true
  done
  rm -rf "$ROOT"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

STEP=0
say() {
  STEP=$((STEP + 1))
  echo
  echo "==> [step $STEP] $*"
}

run_json() {
  "$BIN" "$@"
}

graphql_escape() {
  jq -Rn --arg value "$1" '$value' | sed 's/^"//; s/"$//'
}

post_graphql() {
  local graphql="$1"
  local query="$2"
  local response
  response="$(jq -n --arg query "$query" '{query: $query}' \
    | curl -fsS -H 'content-type: application/json' --data-binary @- "$graphql")"
  if jq -e '.errors? // empty' >/dev/null <<<"$response"; then
    echo "$response" | jq . >&2
    return 1
  fi
  printf '%s\n' "$response"
}

dump_log_tail() {
  local log="$1"
  [[ -n "$log" && -f "$log" ]] || return 0
  echo "  --- last 20 lines of $log ---" >&2
  tail -n 20 "$log" | sed 's/^/  | /' >&2
  echo "  --- end of log ---" >&2
}

wait_http() {
  local url="$1"
  local pid="$2"
  local label="$3"
  local log="${4:-}"
  for _ in $(seq 1 300); do
    if curl -fsS "$url" >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      echo "$label exited before becoming ready" >&2
      dump_log_tail "$log"
      return 1
    fi
    sleep 0.2
  done
  echo "timed out waiting for $label at $url" >&2
  dump_log_tail "$log"
  return 1
}

port_in_use() {
  # Connect to a loopback listener using bash's /dev/tcp; success means busy.
  local port="$1"
  (exec 3<>"/dev/tcp/127.0.0.1/${port}") 2>/dev/null
}

ensure_ports_available() {
  local conflict=0 entry name port
  for entry in "${AGENT_A_NAME}:${AGENT_A_HTTP_PORT}" "${AGENT_B_NAME}:${AGENT_B_HTTP_PORT}"; do
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
  AGENT_A_HTTP_PORT=<free> AGENT_B_HTTP_PORT=<free> $0
Inspect a listener with:
  lsof -nP -iTCP:<port> -sTCP:LISTEN
EOF
    exit 1
  fi
}

start_server() {
  local name="$1"
  local home="$2"
  local port="$3"
  local log="$LOG_DIR/${name}.log"
  if [[ "$KEEP" == "1" ]]; then
    nohup env \
      RUST_LOG="${DEFRA_AGENT_DEMO_RUST_LOG:-warn,defra_agent::agent::p2p_reconcile=debug}" \
      DEFRA_AGENT_REGISTRY_HEARTBEAT_MS=1000 \
      DEFRA_AGENT_PAIRING_SWEEP_MS=1000 \
      DEFRA_AGENT_REGISTRY_STALE_MS=300000 \
      DEFRA_AGENT_ENDPOINT_HEARTBEAT_MS=1000 \
      "$BIN" server \
        --home "$home" \
        --http-port "$port" \
        --no-codex-shim \
        --p2p-bind-addr 127.0.0.1 \
        --p2p-port 0 \
        --p2p-relay-mode disabled \
        --p2p-discovery disabled \
        >"$log" 2>&1 &
  else
    env \
      RUST_LOG="${DEFRA_AGENT_DEMO_RUST_LOG:-warn,defra_agent::agent::p2p_reconcile=debug}" \
      DEFRA_AGENT_REGISTRY_HEARTBEAT_MS=1000 \
      DEFRA_AGENT_PAIRING_SWEEP_MS=1000 \
      DEFRA_AGENT_REGISTRY_STALE_MS=300000 \
      DEFRA_AGENT_ENDPOINT_HEARTBEAT_MS=1000 \
      "$BIN" server \
        --home "$home" \
        --http-port "$port" \
        --no-codex-shim \
        --p2p-bind-addr 127.0.0.1 \
        --p2p-port 0 \
        --p2p-relay-mode disabled \
        --p2p-discovery disabled \
        >"$log" 2>&1 &
  fi
  local pid=$!
  PIDS+=("$pid")
  echo "$pid" >>"$PID_FILE"
  wait_http "http://127.0.0.1:${port}/healthz" "$pid" "$name" "$log"
  echo "  $name ready on http://127.0.0.1:${port} (pid $pid, log $log)"
}

p2p_status() {
  local home="$1"
  run_json p2p status --home "$home"
}

upsert_data_plane() {
  local graphql="$1"
  local peer_id="$2"
  local local_did="$3"
  local peer_address="$4"
  local now
  local mutation
  peer_id="$(graphql_escape "$peer_id")"
  local_did="$(graphql_escape "$local_did")"
  peer_address="$(graphql_escape "$peer_address")"
  now="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  now="$(graphql_escape "$now")"
  mutation=$(cat <<EOF
mutation {
  upsert_DataPlanePairingDesired(
    filter: { peer_id: { _eq: "$peer_id" } },
    add: {
      peer_id: "$peer_id",
      agent_did: "$local_did",
      collections: [
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry"
      ],
      replicator_addresses: ["$peer_address"],
      template: "conversation",
      created_at: "$now",
      updated_at: "$now"
    },
    update: {
      agent_did: "$local_did",
      collections: [
        "AgentRequest",
        "AgentResponse",
        "AgentMessage",
        "AgentToolCall",
        "AgentToolResult",
        "AgentSession",
        "AgentConversation",
        "CompactionEntry"
      ],
      replicator_addresses: ["$peer_address"],
      template: "conversation",
      updated_at: "$now"
    }
  ) { _docID }
}
EOF
)
  post_graphql "$graphql" "$mutation" >/dev/null
}

wait_replicator() {
  local graphql="$1"
  local peer_id="$2"
  local label="$3"
  local escaped
  escaped="$(graphql_escape "$peer_id")"
  local query
  query=$(cat <<EOF
{
  PeerPairingApplied(filter: { peer_id: { _eq: "$escaped" } }, limit: 1) {
    peer_id
    collections
    replicator_addresses
    replicator_filter
  }
}
EOF
)
  for _ in $(seq 1 240); do
    local response
    response="$(post_graphql "$graphql" "$query")"
    if jq -e '
      .data.PeerPairingApplied[0] as $row
      | ($row.replicator_addresses | type == "array" and length > 0)
      and ($row.replicator_filter | type == "string" and contains("AgentRequest"))
    ' >/dev/null <<<"$response"; then
      echo "  $label reconciled"
      return 0
    fi
    sleep 0.5
  done
  echo "timed out waiting for $label replicator" >&2
  post_graphql "$graphql" "$query" | jq . >&2 || true
  return 1
}

wait_request_on_peer() {
  local graphql="$1"
  local request_id="$2"
  local label="$3"
  local escaped
  escaped="$(graphql_escape "$request_id")"
  local query
  query=$(cat <<EOF
{
  AgentRequest(filter: { request_id: { _eq: "$escaped" } }, limit: 1) {
    request_id
    agent_did
    content
  }
}
EOF
)
  for _ in $(seq 1 120); do
    local response
    response="$(post_graphql "$graphql" "$query")"
    if jq -e '.data.AgentRequest | length == 1' >/dev/null <<<"$response"; then
      echo "  $label saw replicated AgentRequest $request_id"
      return 0
    fi
    sleep 0.5
  done
  echo "timed out waiting for replicated AgentRequest $request_id on $label" >&2
  return 1
}

wait_conversation_on_peer() {
  local graphql="$1"
  local session_id="$2"
  local label="$3"
  local escaped
  escaped="$(graphql_escape "$session_id")"
  local query
  query=$(cat <<EOF
{
  AgentConversation(filter: { session_id: { _eq: "$escaped" } }, limit: 1) {
    session_id
    title
    preview_text
    status
    latest_request_id
  }
}
EOF
)
  for _ in $(seq 1 120); do
    local response
    response="$(post_graphql "$graphql" "$query")"
    if jq -e '.data.AgentConversation | length == 1' >/dev/null <<<"$response"; then
      echo "  $label saw AgentConversation $session_id"
      return 0
    fi
    sleep 0.5
  done
  echo "timed out waiting for AgentConversation $session_id on $label" >&2
  return 1
}


runtime_generation() {
  post_graphql "$1" '{ AgentRuntime { active_generation } }' 2>/dev/null \
    | jq -r '.data.AgentRuntime[0].active_generation // 0' 2>/dev/null
}

wait_runtime_reconcile() {
  local graphql="$1" prev="$2" label="$3" resp gen phase
  for _ in $(seq 1 80); do
    resp="$(post_graphql "$graphql" '{ AgentRuntime { active_generation reconcile_phase } }' 2>/dev/null)"
    gen="$(jq -r '.data.AgentRuntime[0].active_generation // 0' <<<"$resp" 2>/dev/null)"
    phase="$(jq -r '.data.AgentRuntime[0].reconcile_phase // ""' <<<"$resp" 2>/dev/null)"
    if [[ "${gen:-0}" -gt "$prev" && "$phase" == "idle" ]]; then
      echo "  $label reconciled (generation $gen)"
      return 0
    fi
    sleep 0.5
  done
  echo "  warning: $label reconcile not confirmed; continuing" >&2
}

# Capstone proof: node A (orchestrator) delegates a cross-node 'worker' subagent
# whose target is node B; the child must materialize and run on node B (its DID)
# and replicate back. Cross-node delegation requires background await + the
# allow-cross-deployment flag on BOTH selections.
prove_subagent_routing() {
  local worker_did="$AGENT_B_DID" worker_gql="$AGENT_B_GRAPHQL"
  local orch_did="$AGENT_A_DID" orch_gql="$AGENT_A_GRAPHQL"
  local wgen ogen esc query resp

  wgen="$(runtime_generation "$worker_gql")"
  run_json config tools set --graphql "$worker_gql" --agent-did "$worker_did" \
    --selection-id "$worker_did:default-tools" \
    --enable-defra-query false --subagent-allow-cross-deployment true >/dev/null
  wait_runtime_reconcile "$worker_gql" "$wgen" "$AGENT_B_NAME (worker)"

  ogen="$(runtime_generation "$orch_gql")"
  run_json config tools set --graphql "$orch_gql" --agent-did "$orch_did" \
    --selection-id "$orch_did:default-tools" \
    --enable-defra-query false \
    --subagent-spawn-enabled true \
    --subagent-background-enabled true \
    --subagent-allow-cross-deployment true \
    --subagent-target "{\"name\":\"worker\",\"agent_did\":\"$worker_did\",\"behavior_id\":\"$worker_did:default\",\"description\":\"Remote worker subagent\"}" >/dev/null
  wait_runtime_reconcile "$orch_gql" "$ogen" "$AGENT_A_NAME (orchestrator)"

  echo "  Submitting a delegation request to $AGENT_A_NAME"
  run_json request submit --graphql "$orch_gql" --agent-did "$orch_did" \
    --content "Delegate to the worker subagent: ask it to describe the worker node, then summarize." \
    --timeout-secs 90 >/dev/null 2>&1 || true

  esc="$(graphql_escape "$worker_did")"
  query=$(cat <<EOF
{
  AgentRequest(filter: { agent_did: { _eq: "$esc" } }) {
    request_id
    lifecycle_state
    caused_by_parent_tool_call_id
  }
}
EOF
)
  for _ in $(seq 1 120); do
    resp="$(post_graphql "$worker_gql" "$query" 2>/dev/null)"
    if jq -e '[.data.AgentRequest[] | select(.caused_by_parent_tool_call_id != null and .lifecycle_state == "completed")] | length >= 1' >/dev/null 2>&1 <<<"$resp"; then
      local child_id
      child_id="$(jq -r '[.data.AgentRequest[] | select(.caused_by_parent_tool_call_id != null)][0].request_id' <<<"$resp")"
      echo "  PROVEN: cross-node child $child_id ran on $AGENT_B_NAME (worker) and completed"
      if post_graphql "$orch_gql" "$query" 2>/dev/null \
        | jq -e '[.data.AgentRequest[] | select(.caused_by_parent_tool_call_id != null)] | length >= 1' >/dev/null 2>&1; then
        echo "  PROVEN: the worker-owned child replicated back to $AGENT_A_NAME (orchestrator)"
      else
        echo "  warning: child not yet visible on $AGENT_A_NAME" >&2
      fi
      return 0
    fi
    sleep 0.5
  done
  echo "subagent routing proof FAILED: no completed cross-node child on $AGENT_B_NAME" >&2
  return 1
}

say "Checking demo HTTP ports are free"
ensure_ports_available
echo "  Amy port:    $AGENT_A_HTTP_PORT free"
echo "  Coding port: $AGENT_B_HTTP_PORT free"

mkdir -p "$AGENT_A_HOME" "$AGENT_B_HOME" "$AGENT_A_WORK" "$AGENT_B_WORK" "$LOG_DIR"
: >"$PID_FILE"

INIT_BACKEND_ARGS=()
if [[ -n "$MODEL_NAME" ]]; then
  INIT_BACKEND_ARGS+=(--model-name "$MODEL_NAME")
fi
if [[ -n "$BACKEND_PRESET" ]]; then
  INIT_BACKEND_ARGS+=(--backend-preset "$BACKEND_PRESET")
  if [[ -n "${DEFRA_AGENT_DEMO_INFERENCE_URL:-}" ]]; then
    INIT_BACKEND_ARGS+=(--inference-url "$MODEL_URL")
  fi
else
  INIT_BACKEND_ARGS+=(--inference-url "$MODEL_URL")
fi
if [[ -n "$API_KEY_ENV_VAR" ]]; then
  INIT_BACKEND_ARGS+=(--api-key-env-var "$API_KEY_ENV_VAR")
fi

say "Initializing isolated nodes"
AGENT_A_INIT="$(run_json init --home "$AGENT_A_HOME" --dangerously-overwrite --agent-name "$AGENT_A_NAME" --tool-root "$AGENT_A_WORK" "${INIT_BACKEND_ARGS[@]}")"
AGENT_B_INIT="$(run_json init --home "$AGENT_B_HOME" --dangerously-overwrite --agent-name "$AGENT_B_NAME" --tool-root "$AGENT_B_WORK" "${INIT_BACKEND_ARGS[@]}")"
AGENT_A_DID="$(jq -r '.agent_did' <<<"$AGENT_A_INIT")"
AGENT_B_DID="$(jq -r '.agent_did' <<<"$AGENT_B_INIT")"
echo "  Amy DID:    $AGENT_A_DID"
echo "  Coding DID: $AGENT_B_DID"

say "Starting two loopback P2P runtimes"
start_server "$AGENT_A_NAME" "$AGENT_A_HOME" "$AGENT_A_HTTP_PORT"
start_server "$AGENT_B_NAME" "$AGENT_B_HOME" "$AGENT_B_HTTP_PORT"

say "Reading live P2P identities"
AGENT_A_P2P="$(p2p_status "$AGENT_A_HOME")"
AGENT_B_P2P="$(p2p_status "$AGENT_B_HOME")"
AGENT_A_PEER="$(jq -r '.p2p_peer_id' <<<"$AGENT_A_P2P")"
AGENT_B_PEER="$(jq -r '.p2p_peer_id' <<<"$AGENT_B_P2P")"
AGENT_A_SHAREABLE="$(jq -r '.p2p_shareable_address' <<<"$AGENT_A_P2P")"
AGENT_B_SHAREABLE="$(jq -r '.p2p_shareable_address' <<<"$AGENT_B_P2P")"
echo "  Amy peer:        $AGENT_A_PEER"
echo "  Coding peer:     $AGENT_B_PEER"
echo "  Amy shareable:   $AGENT_A_SHAREABLE"
echo "  Coding shareable: $AGENT_B_SHAREABLE"

say "Creating the network and granting Coding membership"
run_json p2p network create --home "$AGENT_A_HOME" --name "Two Node Demo" --output json | jq .
run_json p2p network grant --home "$AGENT_A_HOME" "$AGENT_B_DID" --output json | jq .

say "Joining Coding to Amy with a signed network-control invite"
AGENT_A_INVITE="$(run_json p2p pairings invite --home "$AGENT_A_HOME" --member-did "$AGENT_B_DID" --template network-control)"
TOKEN="$(jq -r '.token' <<<"$AGENT_A_INVITE")"
run_json p2p pairings join --home "$AGENT_B_HOME" "$TOKEN" | jq .

say "Adding bidirectional conversation data-plane rows"
upsert_data_plane "$AGENT_A_GRAPHQL" "$AGENT_B_PEER" "$AGENT_A_DID" "$AGENT_B_SHAREABLE"
upsert_data_plane "$AGENT_B_GRAPHQL" "$AGENT_A_PEER" "$AGENT_B_DID" "$AGENT_A_SHAREABLE"

say "Waiting for reconciled replicators"
wait_replicator "$AGENT_B_GRAPHQL" "$AGENT_A_PEER" "Coding -> Amy"
wait_replicator "$AGENT_A_GRAPHQL" "$AGENT_B_PEER" "Amy -> Coding"

say "Reconciled data-plane state"
echo "  Amy -> Coding: network-control subscribed, conversation replicator installed"
echo "  Coding -> Amy: network-control subscribed, conversation replicator installed"

say "Proving document replication with one no-wait request"
REQUEST="$(run_json request submit --graphql "$AGENT_B_GRAPHQL" --agent-did "$AGENT_B_DID" --content "two-node p2p demo ping from Coding" --no-wait)"
REQUEST_ID="$(jq -r '.request_id' <<<"$REQUEST")"
REQUEST_SESSION_ID="$(jq -r '.session_id' <<<"$REQUEST")"
echo "  Coding created AgentRequest $REQUEST_ID"
wait_conversation_on_peer "$AGENT_B_GRAPHQL" "$REQUEST_SESSION_ID" "Coding"
wait_request_on_peer "$AGENT_A_GRAPHQL" "$REQUEST_ID" "Amy"
wait_conversation_on_peer "$AGENT_A_GRAPHQL" "$REQUEST_SESSION_ID" "Amy"

if [[ "$SUBAGENT_PROOF" == "1" ]]; then
  say "Proving cross-node subagent routing ($AGENT_A_NAME delegates to $AGENT_B_NAME)"
  prove_subagent_routing
fi

PIDS_JSON="$(printf '%s\n' "${PIDS[@]}" | jq -R 'select(length > 0) | tonumber' | jq -s '.')"
jq -n \
  --arg root "$ROOT" \
  --arg logs "$LOG_DIR" \
  --arg requestId "$REQUEST_ID" \
  --arg requestSessionId "$REQUEST_SESSION_ID" \
  --arg amyHome "$AGENT_A_HOME" \
  --arg amyWork "$AGENT_A_WORK" \
  --arg amyDid "$AGENT_A_DID" \
  --arg amyPeer "$AGENT_A_PEER" \
  --arg amyShareable "$AGENT_A_SHAREABLE" \
  --arg amyGraphql "$AGENT_A_GRAPHQL" \
  --arg codingHome "$AGENT_B_HOME" \
  --arg codingWork "$AGENT_B_WORK" \
  --arg codingDid "$AGENT_B_DID" \
  --arg codingPeer "$AGENT_B_PEER" \
  --arg codingShareable "$AGENT_B_SHAREABLE" \
  --arg codingGraphql "$AGENT_B_GRAPHQL" \
  --arg nameA "$AGENT_A_NAME" \
  --arg nameB "$AGENT_B_NAME" \
  --argjson pids "$PIDS_JSON" \
  '{
    root: $root,
    logs: $logs,
    pids: $pids,
    request: {
      id: $requestId,
      session_id: $requestSessionId
    },
    node_a: {
      name: $nameA,
      home: $amyHome,
      work: $amyWork,
      agent_did: $amyDid,
      peer_id: $amyPeer,
      shareable_address: $amyShareable,
      graphql: $amyGraphql
    },
    node_b: {
      name: $nameB,
      home: $codingHome,
      work: $codingWork,
      agent_did: $codingDid,
      peer_id: $codingPeer,
      shareable_address: $codingShareable,
      graphql: $codingGraphql
    }
  }' >"$STATE_FILE"

say "Demo complete"
echo "  Root:        $ROOT"
echo "  Amy home:    $AGENT_A_HOME"
echo "  Coding home: $AGENT_B_HOME"
echo "  Logs:        $LOG_DIR"
echo "  State:       $STATE_FILE"
echo
if [[ "$KEEP" == "1" ]]; then
  echo "Try manually:"
  echo "  $BIN p2p pairings list --home $AGENT_A_HOME --output table"
  echo "  $BIN p2p pairings list --home $AGENT_B_HOME --output table"
  echo "  $BIN request show --graphql $AGENT_A_GRAPHQL $REQUEST_ID --output json | jq .request"
else
  echo "Run with DEFRA_AGENT_DEMO_KEEP=1 to keep the runtimes for manual inspection."
fi
