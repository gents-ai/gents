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
  DEFRA_AGENT_DEMO_INFERENCE_URL  Backend URL used by init.
                                  Default: http://127.0.0.1:8080/v1
  DEFRA_AGENT_DEMO_MODEL          Model name used by init. Default: demo-model
  AMY_HTTP_PORT                   Amy HTTP port. Default: 19391
  CODING_HTTP_PORT                Coding HTTP port. Default: 19392
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
MODEL_NAME="${DEFRA_AGENT_DEMO_MODEL:-demo-model}"
AMY_HTTP_PORT="${AMY_HTTP_PORT:-19391}"
CODING_HTTP_PORT="${CODING_HTTP_PORT:-19392}"

AMY_HOME="$ROOT/amy"
CODING_HOME="$ROOT/coding"
AMY_WORK="$ROOT/amy-work"
CODING_WORK="$ROOT/coding-work"
LOG_DIR="$ROOT/logs"
AMY_GRAPHQL="http://127.0.0.1:${AMY_HTTP_PORT}/api/v0/graphql"
CODING_GRAPHQL="http://127.0.0.1:${CODING_HTTP_PORT}/api/v0/graphql"

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

say() {
  echo
  echo "==> $*"
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

wait_http() {
  local url="$1"
  local pid="$2"
  local label="$3"
  for _ in $(seq 1 300); do
    if curl -fsS "$url" >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      echo "$label exited before becoming ready" >&2
      return 1
    fi
    sleep 0.2
  done
  echo "timed out waiting for $label at $url" >&2
  return 1
}

start_server() {
  local name="$1"
  local home="$2"
  local port="$3"
  local log="$LOG_DIR/${name}.log"
  if [[ "$KEEP" == "1" ]]; then
    nohup env \
      RUST_LOG="warn,defra_agent::agent::p2p_reconcile=debug" \
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
      RUST_LOG="warn,defra_agent::agent::p2p_reconcile=debug" \
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
  wait_http "http://127.0.0.1:${port}/healthz" "$pid" "$name"
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

mkdir -p "$AMY_HOME" "$CODING_HOME" "$AMY_WORK" "$CODING_WORK" "$LOG_DIR"

say "Initializing isolated nodes"
AMY_INIT="$(run_json init --home "$AMY_HOME" --dangerously-overwrite --agent-name amy --tool-root "$AMY_WORK" --inference-url "$MODEL_URL" --model-name "$MODEL_NAME")"
CODING_INIT="$(run_json init --home "$CODING_HOME" --dangerously-overwrite --agent-name coding --tool-root "$CODING_WORK" --inference-url "$MODEL_URL" --model-name "$MODEL_NAME")"
AMY_DID="$(jq -r '.agent_did' <<<"$AMY_INIT")"
CODING_DID="$(jq -r '.agent_did' <<<"$CODING_INIT")"
echo "  Amy DID:    $AMY_DID"
echo "  Coding DID: $CODING_DID"

say "Starting two loopback P2P runtimes"
start_server "amy" "$AMY_HOME" "$AMY_HTTP_PORT"
start_server "coding" "$CODING_HOME" "$CODING_HTTP_PORT"

say "Reading live P2P identities"
AMY_P2P="$(p2p_status "$AMY_HOME")"
CODING_P2P="$(p2p_status "$CODING_HOME")"
AMY_PEER="$(jq -r '.p2p_peer_id' <<<"$AMY_P2P")"
CODING_PEER="$(jq -r '.p2p_peer_id' <<<"$CODING_P2P")"
AMY_SHAREABLE="$(jq -r '.p2p_shareable_address' <<<"$AMY_P2P")"
CODING_SHAREABLE="$(jq -r '.p2p_shareable_address' <<<"$CODING_P2P")"
echo "  Amy peer:        $AMY_PEER"
echo "  Coding peer:     $CODING_PEER"
echo "  Amy shareable:   $AMY_SHAREABLE"
echo "  Coding shareable: $CODING_SHAREABLE"

say "Creating the network and granting Coding membership"
run_json p2p network create --home "$AMY_HOME" --name "Two Node Demo" --output json | jq .
run_json p2p network grant --home "$AMY_HOME" "$CODING_DID" --output json | jq .

say "Joining Coding to Amy with a signed network-control invite"
AMY_INVITE="$(run_json p2p pairings invite --home "$AMY_HOME" --member-did "$CODING_DID" --template network-control)"
TOKEN="$(jq -r '.token' <<<"$AMY_INVITE")"
run_json p2p pairings join --home "$CODING_HOME" "$TOKEN" | jq .

say "Adding bidirectional conversation data-plane rows"
upsert_data_plane "$AMY_GRAPHQL" "$CODING_PEER" "$AMY_DID" "$CODING_SHAREABLE"
upsert_data_plane "$CODING_GRAPHQL" "$AMY_PEER" "$CODING_DID" "$AMY_SHAREABLE"

say "Waiting for reconciled replicators"
wait_replicator "$CODING_GRAPHQL" "$AMY_PEER" "Coding -> Amy"
wait_replicator "$AMY_GRAPHQL" "$CODING_PEER" "Amy -> Coding"

say "Reconciled data-plane state"
echo "  Amy -> Coding: network-control subscribed, conversation replicator installed"
echo "  Coding -> Amy: network-control subscribed, conversation replicator installed"

say "Proving document replication with one no-wait request"
REQUEST="$(run_json request submit --graphql "$CODING_GRAPHQL" --agent-did "$CODING_DID" --content "two-node p2p demo ping from Coding" --no-wait)"
REQUEST_ID="$(jq -r '.request_id' <<<"$REQUEST")"
echo "  Coding created AgentRequest $REQUEST_ID"
wait_request_on_peer "$AMY_GRAPHQL" "$REQUEST_ID" "Amy"

say "Demo complete"
echo "  Root:        $ROOT"
echo "  Amy home:    $AMY_HOME"
echo "  Coding home: $CODING_HOME"
echo "  Logs:        $LOG_DIR"
echo
if [[ "$KEEP" == "1" ]]; then
  echo "Try manually:"
  echo "  $BIN p2p pairings list --home $AMY_HOME --output table"
  echo "  $BIN p2p pairings list --home $CODING_HOME --output table"
  echo "  $BIN request show --graphql $AMY_GRAPHQL $REQUEST_ID --output json | jq .request"
else
  echo "Run with DEFRA_AGENT_DEMO_KEEP=1 to keep the runtimes for manual inspection."
fi
