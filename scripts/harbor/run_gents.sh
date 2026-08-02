#!/bin/sh
set -eu

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
: "${GENTS_COMMAND_TIMEOUT_SECS:=86400}"
: "${GENTS_SERVER_STARTUP_TIMEOUT_SECS:=300}"

logs_dir=/logs/agent
server_log="${logs_dir}/gents-server.log"
init_log="${logs_dir}/gents-init.json"
request_log="${logs_dir}/request.json"
request_stdout="${logs_dir}/request.stdout.json"
response_log="${logs_dir}/response.json"
trajectory_path="${logs_dir}/trajectory.json"
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
# zero. Preserve the response and trajectory above, then make the trial an
# agent exception so it can be retried or recovered separately.
response_status=$(sed -n 's/^[[:space:]]*"status": "\([^"]*\)",*$/\1/p' "${response_log}" | head -1)
case "${response_status}" in
  complete|completed) ;;
  error)
    echo "Gents request ${request_id} terminated with an error response" >&2
    sed -n '/^[[:space:]]*"error_message":/p' "${response_log}" >&2 || true
    exit 1
    ;;
  *)
    echo "Gents request ${request_id} returned unexpected response status: ${response_status:-missing}" >&2
    exit 1
    ;;
esac

printf 'gents request %s completed; trajectory=%s\n' "${request_id}" "${trajectory_path}"
