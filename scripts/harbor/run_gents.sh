#!/bin/sh
set -eu

# Classify a terminal response document. Budget exhaustion is the only
# terminal error Harbor should verifier-score: the workspace may hold real
# work. The match is anchored to the full owned-loop error shape — the
# max-turn guard's `PromptError::MaxTurnsError` display as persisted by
# `agent stream failed: {error}` (pinned by the runtime max-turns test) at
# the very start of the error_message value. A provider error that merely
# echoes upstream text mentioning MaxTurnError therefore stays an agent
# exception, as does everything else unrecognized. Matching the quoted
# key-plus-prefix is safe against model content: JSON escapes quotes inside
# string values, so this byte sequence can only introduce the real field.
_MAX_TURN_ERROR_PREFIX='"error_message": "agent stream failed: PromptError: MaxTurnError: '

classify_response() {
  response_file=$1
  response_file_status=$(sed -n 's/^[[:space:]]*"status": "\([^"]*\)",*$/\1/p' "${response_file}" | head -1)
  case "${response_file_status}" in
    complete|completed)
      printf 'completed\n'
      ;;
    error)
      if grep -qF "${_MAX_TURN_ERROR_PREFIX}" "${response_file}"; then
        printf 'max_turns_exhausted\n'
      else
        printf 'agent_error\n'
      fi
      ;;
    *)
      printf 'unexpected:%s\n' "${response_file_status:-missing}"
      ;;
  esac
}

# Fixture-driven check of terminal-response classification. Runs without any
# Gents environment; CI executes it next to the shell-syntax check.
run_self_test() {
  self_test_dir=$(mktemp -d /tmp/gents-harbor-self-test.XXXXXX)
  trap 'rm -rf "${self_test_dir}"' EXIT
  failures=0

  expect_outcome() {
    fixture_name=$1
    expected=$2
    fixture_file="${self_test_dir}/${fixture_name}.json"
    actual=$(classify_response "${fixture_file}")
    if [ "${actual}" = "${expected}" ]; then
      printf 'ok: %s -> %s\n' "${fixture_name}" "${actual}"
    else
      printf 'FAIL: %s expected %s, got %s\n' \
        "${fixture_name}" "${expected}" "${actual}" >&2
      failures=$((failures + 1))
    fi
  }

  cat >"${self_test_dir}/complete.json" <<'EOF'
{
  "request_id": "req-1",
  "status": "complete",
  "content": "done",
  "error_message": null
}
EOF
  cat >"${self_test_dir}/completed.json" <<'EOF'
{
  "request_id": "req-2",
  "status": "completed",
  "content": "done",
  "error_message": null
}
EOF
  cat >"${self_test_dir}/max-turns.json" <<'EOF'
{
  "request_id": "req-3",
  "status": "error",
  "content": "partial work",
  "error_message": "agent stream failed: PromptError: MaxTurnError: (reached max turn limit: 250)"
}
EOF
  cat >"${self_test_dir}/provider-error.json" <<'EOF'
{
  "request_id": "req-4",
  "status": "error",
  "content": null,
  "error_message": "agent stream failed: CompletionError: ProviderError: upstream returned HTTP 500"
}
EOF
  cat >"${self_test_dir}/compaction-error.json" <<'EOF'
{
  "request_id": "req-5",
  "status": "error",
  "content": null,
  "error_message": "compaction failed: summary request rejected by provider"
}
EOF
  cat >"${self_test_dir}/content-mentions-max-turn.json" <<'EOF'
{
  "request_id": "req-6",
  "status": "error",
  "content": "I hit MaxTurnError: in a log I was reading",
  "error_message": "agent stream failed: CompletionError: ProviderError: connection reset"
}
EOF
  cat >"${self_test_dir}/unexpected-status.json" <<'EOF'
{
  "request_id": "req-7",
  "status": "interrupted",
  "content": null,
  "error_message": null
}
EOF
  cat >"${self_test_dir}/missing-status.json" <<'EOF'
{
  "request_id": "req-8",
  "content": null
}
EOF
  # Full `response wait` envelope: flat AgentResponse fields plus a nested
  # `request` object whose `failure_reason` duplicates the terminal error.
  cat >"${self_test_dir}/envelope-max-turns.json" <<'EOF'
{
  "request_id": "req-9",
  "behavior_id": "b-1",
  "session_id": "s-1",
  "status": "error",
  "content": "partial work",
  "reasoning": null,
  "error_message": "agent stream failed: PromptError: MaxTurnError: (reached max turn limit: 250)",
  "token_count": 12345,
  "completed_at": "2026-08-04T00:00:00Z",
  "request": {
    "request_id": "req-9",
    "lifecycle_state": "failed",
    "failure_reason": "agent stream failed: PromptError: MaxTurnError: (reached max turn limit: 250)"
  }
}
EOF
  # A provider error may embed upstream response text that mentions the
  # MaxTurn token; that is still an infrastructure failure.
  cat >"${self_test_dir}/provider-echoes-max-turn.json" <<'EOF'
{
  "request_id": "req-11",
  "status": "error",
  "content": null,
  "error_message": "agent stream failed: CompletionError: ProviderError: upstream mentioned MaxTurnError: (reached max turn limit: 250)"
}
EOF
  # The nested request's failure_reason must never classify on its own: here
  # it mentions MaxTurnError but the response's own error is a provider one.
  cat >"${self_test_dir}/envelope-nested-max-turn-only.json" <<'EOF'
{
  "request_id": "req-10",
  "status": "error",
  "content": null,
  "error_message": "agent stream failed: CompletionError: ProviderError: upstream returned HTTP 500",
  "request": {
    "request_id": "req-10",
    "lifecycle_state": "failed",
    "failure_reason": "child subagent hit MaxTurnError: before the provider failed"
  }
}
EOF

  expect_outcome complete completed
  expect_outcome completed completed
  expect_outcome max-turns max_turns_exhausted
  expect_outcome provider-error agent_error
  expect_outcome compaction-error agent_error
  expect_outcome content-mentions-max-turn agent_error
  expect_outcome unexpected-status unexpected:interrupted
  expect_outcome missing-status unexpected:missing
  expect_outcome envelope-max-turns max_turns_exhausted
  expect_outcome envelope-nested-max-turn-only agent_error
  expect_outcome provider-echoes-max-turn agent_error

  if [ "${failures}" -ne 0 ]; then
    printf 'self-test failed: %s classification(s) wrong\n' "${failures}" >&2
    exit 1
  fi
  printf 'self-test passed\n'
  exit 0
}

if [ "${1:-}" = "self-test" ]; then
  run_self_test
fi

: "${GENTS_BINARY:=/usr/local/bin/gents}"
: "${GENTS_HOME:?GENTS_HOME is required}"
: "${GENTS_INSTRUCTION_FILE:?GENTS_INSTRUCTION_FILE is required}"
: "${GENTS_INFERENCE_URL:?GENTS_INFERENCE_URL is required}"
: "${GENTS_MODEL:?GENTS_MODEL is required}"
: "${GENTS_TOOL_ROOT:=/app}"
: "${GENTS_API_KEY:=no-key}"
: "${GENTS_TEMPERATURE:=1.0}"
: "${GENTS_TOP_P:=0.95}"
: "${GENTS_TOP_K:=}"
: "${GENTS_REASONING_EFFORT:=max}"
: "${GENTS_MAX_OUTPUT:=393216}"
: "${GENTS_CONTEXT_WINDOW:=458752}"
: "${GENTS_MAX_TURNS:=250}"
: "${GENTS_RETRY_MAX_TRANSPORT:=3}"
: "${GENTS_REQUEST_TIMEOUT_SECS:=86400}"
: "${GENTS_COMMAND_TIMEOUT_SECS:=600}"
: "${GENTS_COMMAND_TIMEOUT_MAX_SECS:=3600}"
: "${GENTS_SERVER_STARTUP_TIMEOUT_SECS:=300}"

logs_dir=/logs/agent
server_log="${logs_dir}/gents-server.log"
init_log="${logs_dir}/gents-init.json"
request_log="${logs_dir}/request.json"
request_stdout="${logs_dir}/request.stdout.json"
response_log="${logs_dir}/response.json"
trajectory_path="${logs_dir}/trajectory.json"
outcome_log="${logs_dir}/gents-outcome.json"
status_log="${logs_dir}/gents-status.json"
profile_log="${logs_dir}/gents-profile.json"
request_id=""

mkdir -p "${logs_dir}"
test -x "${GENTS_BINARY}"
test -f "${GENTS_INSTRUCTION_FILE}"
test -d "${GENTS_TOOL_ROOT}"

case "${GENTS_MODEL}" in
  *[!A-Za-z0-9._:/-]*)
    echo "GENTS_MODEL contains unsupported characters" >&2
    exit 2
    ;;
esac

case "${GENTS_REASONING_EFFORT}" in
  low|high|max) ;;
  *)
    echo "GENTS_REASONING_EFFORT must be one of: low, high, max" >&2
    exit 2
    ;;
esac

# GENTS_MAX_TURNS is interpolated into the outcome document as a JSON number,
# so leading zeros are as invalid as non-digits.
case "${GENTS_MAX_TURNS}" in
  ''|*[!0-9]*|0?*)
    echo "GENTS_MAX_TURNS must be a non-negative integer without leading zeros" >&2
    exit 2
    ;;
esac

"${GENTS_BINARY}" init \
  --home "${GENTS_HOME}" \
  --agent-name harbor-gents \
  --backend-preset vllm \
  --inference-url "${GENTS_INFERENCE_URL}" \
  --openai-wire-api chat-completions \
  --api-key "${GENTS_API_KEY}" \
  --model-name "${GENTS_MODEL}" \
  --max-concurrent 1 \
  --max-queue-depth 1 \
  --yolo \
  --tool-root "${GENTS_TOOL_ROOT}" \
  >"${init_log}"

start_server() {
  "${GENTS_BINARY}" server \
    --home "${GENTS_HOME}" \
    --http-addr 127.0.0.1 \
    --http-port 9191 \
    --tool-ceiling readwrite \
    --tool-root "${GENTS_TOOL_ROOT}" \
    --command-timeout-secs "${GENTS_COMMAND_TIMEOUT_SECS}" \
    --command-timeout-max-secs "${GENTS_COMMAND_TIMEOUT_MAX_SECS}" \
    >>"${server_log}" 2>&1 &
  server_pid=$!
}

wait_for_server_ready() {
  server_ready=0
  attempt=0
  while [ "${attempt}" -lt "${GENTS_SERVER_STARTUP_TIMEOUT_SECS}" ]; do
    if "${GENTS_BINARY}" status --home "${GENTS_HOME}" >"${status_log}" 2>/dev/null &&
      grep -q '"process_state": "ready"' "${status_log}" &&
      grep -q '"behavior_readiness": "ready"' "${status_log}"; then
      server_ready=1
      break
    fi
    if ! kill -0 "${server_pid}" >/dev/null 2>&1; then
      echo "Gents server exited during startup" >&2
      tail -200 "${server_log}" >&2 || true
      exit 1
    fi
    sleep 1
    attempt=$((attempt + 1))
  done
  if [ "${server_ready}" != "1" ]; then
    echo "Gents server did not become ready in ${GENTS_SERVER_STARTUP_TIMEOUT_SECS}s" >&2
    tail -200 "${server_log}" >&2 || true
    exit 1
  fi
}

: >"${server_log}"
profile_id=$(sed -n 's/^[[:space:]]*"inference_profile_id": "\([^"]*\)",*$/\1/p' "${init_log}" | head -1)
if [ -z "${profile_id}" ]; then
  echo "Gents init output did not contain inference_profile_id" >&2
  exit 1
fi

configure_profile() {
  profile_configured=0
  profile_attempt=0
  profile_attempt_limit=$((GENTS_SERVER_STARTUP_TIMEOUT_SECS * 10))
  while [ "${profile_attempt}" -lt "${profile_attempt_limit}" ]; do
    if "${GENTS_BINARY}" config profile set \
      --graphql http://127.0.0.1:9191/api/v0/graphql \
      --profile-id "${profile_id}" \
      --context-window "${GENTS_CONTEXT_WINDOW}" \
      --max-output-tokens "${GENTS_MAX_OUTPUT}" \
      --max-turns "${GENTS_MAX_TURNS}" \
      --reasoning-effort "${GENTS_REASONING_EFFORT}" \
      --stream-liveness-timeout-secs "${GENTS_REQUEST_TIMEOUT_SECS}" \
      --deadline-duration-secs "${GENTS_REQUEST_TIMEOUT_SECS}" \
      --retry-max-transport "${GENTS_RETRY_MAX_TRANSPORT}" \
      >"${profile_log}" 2>/dev/null; then
      profile_configured=1
      break
    fi
    if ! kill -0 "${server_pid}" >/dev/null 2>&1; then
      echo "Gents server exited before its inference profile could be configured" >&2
      tail -200 "${server_log}" >&2 || true
      exit 1
    fi
    sleep 0.1
    profile_attempt=$((profile_attempt + 1))
  done
  if [ "${profile_configured}" != "1" ]; then
    echo "Gents GraphQL did not accept the inference profile within ${GENTS_SERVER_STARTUP_TIMEOUT_SECS}s" >&2
    tail -200 "${server_log}" >&2 || true
    exit 1
  fi
}

start_server

cleanup() {
  exit_code=$?
  trap - EXIT INT TERM
  if [ -n "${request_id}" ] && [ ! -s "${trajectory_path}" ] && \
    kill -0 "${server_pid}" >/dev/null 2>&1; then
    "${GENTS_BINARY}" trace project \
      --home "${GENTS_HOME}" \
      --request-id "${request_id}" \
      --projection atif \
      --format native-json \
      --output-file "${trajectory_path}" \
      >/dev/null 2>&1 || true
  fi
  kill "${server_pid}" >/dev/null 2>&1 || true
  wait "${server_pid}" >/dev/null 2>&1 || true
  exit "${exit_code}"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

configure_profile

# Configure through GraphQL before requiring behavior readiness. This also
# bootstraps binaries whose schema materializes an omitted nullable string as
# an empty value. Restarting makes the persisted profile part of the startup
# snapshot before any benchmark request can exist.
kill "${server_pid}" >/dev/null 2>&1 || true
wait "${server_pid}" || true
start_server
wait_for_server_ready

metadata=$(printf '{"harness":"harbor","model_name":"%s"}' "${GENTS_MODEL}")
set -- \
  request submit
set -- "$@" \
  --home "${GENTS_HOME}" \
  --content-file "${GENTS_INSTRUCTION_FILE}" \
  --temperature "${GENTS_TEMPERATURE}" \
  --top-p "${GENTS_TOP_P}" \
  --max-tokens "${GENTS_MAX_OUTPUT}" \
  --metadata "${metadata}" \
  --valid-until none \
  --no-wait \
  --output-file "${request_log}"
if [ -n "${GENTS_TOP_K}" ]; then
  set -- "$@" --top-k "${GENTS_TOP_K}"
fi
"${GENTS_BINARY}" "$@" >"${request_stdout}"

request_id=$(sed -n 's/^[[:space:]]*"request_id": "\([^"]*\)",*$/\1/p' "${request_log}" | head -1)
if [ -z "${request_id}" ]; then
  echo "Gents request output did not contain request_id" >&2
  tail -200 "${request_log}" >&2 || true
  exit 1
fi

"${GENTS_BINARY}" response wait \
  --home "${GENTS_HOME}" \
  --request-id "${request_id}" \
  --timeout-secs "${GENTS_REQUEST_TIMEOUT_SECS}" \
  --poll-secs 1 \
  >"${response_log}"

"${GENTS_BINARY}" trace project \
  --home "${GENTS_HOME}" \
  --request-id "${request_id}" \
  --projection atif \
  --format native-json \
  --output-file "${trajectory_path}"

test -s "${trajectory_path}"

# `response wait` exits successfully after any terminal response, including a
# provider/runtime failure. Do not let Harbor run the verifier against an
# untouched task filesystem and record that infrastructure failure as a model
# zero. The one exception is agent-budget exhaustion (MaxTurn): the workspace
# holds up to GENTS_MAX_TURNS turns of real work, so return control to Harbor
# and let the verifier score it. Preserve the response and trajectory above in
# every case; genuine failures stay agent exceptions so they can be retried or
# recovered separately.
response_status=$(sed -n 's/^[[:space:]]*"status": "\([^"]*\)",*$/\1/p' "${response_log}" | head -1)
outcome=$(classify_response "${response_log}")
printf '{\n  "outcome": "%s",\n  "response_status": "%s",\n  "max_turns": %s,\n  "request_id": "%s"\n}\n' \
  "${outcome}" "${response_status:-missing}" "${GENTS_MAX_TURNS}" "${request_id}" \
  >"${outcome_log}"
case "${outcome}" in
  completed)
    printf 'gents request %s completed; trajectory=%s\n' "${request_id}" "${trajectory_path}"
    ;;
  max_turns_exhausted)
    echo "Gents request ${request_id} exhausted its ${GENTS_MAX_TURNS}-turn budget; returning the workspace for verification" >&2
    sed -n '/^[[:space:]]*"error_message":/p' "${response_log}" >&2 || true
    printf 'gents request %s reached the %s-turn limit; trajectory=%s\n' \
      "${request_id}" "${GENTS_MAX_TURNS}" "${trajectory_path}"
    ;;
  agent_error)
    echo "Gents request ${request_id} terminated with an error response" >&2
    sed -n '/^[[:space:]]*"error_message":/p' "${response_log}" >&2 || true
    exit 1
    ;;
  *)
    echo "Gents request ${request_id} returned unexpected response status: ${response_status:-missing}" >&2
    exit 1
    ;;
esac
