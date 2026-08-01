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
: "${GENTS_TOP_P:=1.0}"
: "${GENTS_TOP_K:=}"
: "${GENTS_MAX_TOKENS:=32768}"
: "${GENTS_MAX_TURNS:=250}"
: "${GENTS_RETRY_MAX_TRANSPORT:=3}"
: "${GENTS_REQUEST_TIMEOUT_SECS:=1800}"
: "${GENTS_COMMAND_TIMEOUT_SECS:=900}"
: "${GENTS_SERVER_STARTUP_TIMEOUT_SECS:=120}"

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

"${GENTS_BINARY}" server \
  --home "${GENTS_HOME}" \
  --http-addr 127.0.0.1 \
  --http-port 9191 \
  --tool-ceiling readwrite \
  --tool-root "${GENTS_TOOL_ROOT}" \
  --command-timeout-secs "${GENTS_COMMAND_TIMEOUT_SECS}" \
  >"${server_log}" 2>&1 &
server_pid=$!

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

server_ready=0
attempt=0
while [ "${attempt}" -lt "${GENTS_SERVER_STARTUP_TIMEOUT_SECS}" ]; do
  if "${GENTS_BINARY}" status --home "${GENTS_HOME}" >"${status_log}" 2>/dev/null; then
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

profile_id=$(sed -n 's/^[[:space:]]*"inference_profile_id": "\([^"]*\)",*$/\1/p' "${init_log}" | head -1)
if [ -z "${profile_id}" ]; then
  echo "Gents init output did not contain inference_profile_id" >&2
  exit 1
fi
"${GENTS_BINARY}" config profile set \
  --graphql http://127.0.0.1:9191/api/v0/graphql \
  --profile-id "${profile_id}" \
  --max-turns "${GENTS_MAX_TURNS}" \
  --stream-liveness-timeout-secs "${GENTS_REQUEST_TIMEOUT_SECS}" \
  --deadline-duration-secs "${GENTS_REQUEST_TIMEOUT_SECS}" \
  --retry-max-transport "${GENTS_RETRY_MAX_TRANSPORT}" \
  >"${profile_log}"
# The server reconciles document-backed configuration asynchronously. Give the
# updated profile a moment to enter the runtime snapshot before submitting.
sleep 1

metadata=$(printf '{"harness":"harbor","model_name":"%s"}' "${GENTS_MODEL}")
set -- \
  request submit
set -- "$@" \
  --home "${GENTS_HOME}" \
  --content-file "${GENTS_INSTRUCTION_FILE}" \
  --temperature "${GENTS_TEMPERATURE}" \
  --top-p "${GENTS_TOP_P}" \
  --max-tokens "${GENTS_MAX_TOKENS}" \
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
printf 'gents request %s completed; trajectory=%s\n' "${request_id}" "${trajectory_path}"
