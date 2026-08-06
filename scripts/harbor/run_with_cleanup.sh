#!/bin/sh
# Run Harbor while containing Docker resources to this invocation.
#
# Harbor normally runs `docker compose down` after every trial. If the
# controller itself is cancelled, its shielded async cleanup can be terminated
# with the event loop and leave task containers behind. This wrapper records
# pre-existing Compose projects and force-removes only new Harbor task projects
# when Harbor exits, including on INT/TERM.
set -eu

: "${HARBOR_BIN:=harbor}"
: "${HARBOR_HEALTHCHECK_URL:=}"
: "${HARBOR_HEALTHCHECK_INTERVAL_SECS:=15}"
: "${HARBOR_HEALTHCHECK_FAILURE_LIMIT:=3}"

baseline_file=$(mktemp "${TMPDIR:-/tmp}/gents-harbor-projects.XXXXXX")
health_failed_file=$(mktemp "${TMPDIR:-/tmp}/gents-harbor-health.XXXXXX")
harbor_pid=""
health_pid=""

compose_projects() {
  docker ps -a --format '{{.Label "com.docker.compose.project"}}' |
    sed -n '/[^[:space:]]/p' |
    sort -u
}

compose_projects >"${baseline_file}"

cleanup_new_projects() {
  compose_projects | while IFS= read -r project; do
    case "${project}" in
      *__env) ;;
      *) continue ;;
    esac
    if grep -Fqx "${project}" "${baseline_file}"; then
      continue
    fi

    container_ids=$(docker ps -aq --filter "label=com.docker.compose.project=${project}")
    if [ -n "${container_ids}" ]; then
      # Project names are derived from Harbor trial IDs. Selecting resources by
      # the exact Compose label prevents this from touching unrelated services.
      docker rm -f ${container_ids} >/dev/null 2>&1 || true
    fi
    network_ids=$(docker network ls -q --filter "label=com.docker.compose.project=${project}")
    if [ -n "${network_ids}" ]; then
      docker network rm ${network_ids} >/dev/null 2>&1 || true
    fi
    volume_ids=$(docker volume ls -q --filter "label=com.docker.compose.project=${project}")
    if [ -n "${volume_ids}" ]; then
      docker volume rm -f ${volume_ids} >/dev/null 2>&1 || true
    fi
  done
}

cleanup() {
  status=$?
  trap - EXIT INT TERM
  if [ -n "${health_pid}" ] && kill -0 "${health_pid}" >/dev/null 2>&1; then
    kill "${health_pid}" >/dev/null 2>&1 || true
    wait "${health_pid}" >/dev/null 2>&1 || true
  fi
  if [ -n "${harbor_pid}" ] && kill -0 "${harbor_pid}" >/dev/null 2>&1; then
    kill "${harbor_pid}" >/dev/null 2>&1 || true
    wait "${harbor_pid}" >/dev/null 2>&1 || true
  fi
  cleanup_new_projects
  rm -f "${baseline_file}" "${health_failed_file}"
  exit "${status}"
}
trap cleanup EXIT INT TERM

monitor_health() {
  failures=0
  while kill -0 "${harbor_pid}" >/dev/null 2>&1; do
    if curl -fsS --max-time 5 "${HARBOR_HEALTHCHECK_URL}" >/dev/null 2>&1; then
      failures=0
    else
      failures=$((failures + 1))
      echo "Harbor dependency health check failed (${failures}/${HARBOR_HEALTHCHECK_FAILURE_LIMIT}): ${HARBOR_HEALTHCHECK_URL}" >&2
      if [ "${failures}" -ge "${HARBOR_HEALTHCHECK_FAILURE_LIMIT}" ]; then
        echo "Stopping Harbor because its dependency remained unhealthy" >&2
        printf '%s\n' "${HARBOR_HEALTHCHECK_URL}" >"${health_failed_file}"
        kill "${harbor_pid}" >/dev/null 2>&1 || true
        return
      fi
    fi
    sleep "${HARBOR_HEALTHCHECK_INTERVAL_SECS}"
  done
}

if [ -n "${HARBOR_HEALTHCHECK_URL}" ]; then
  command -v curl >/dev/null 2>&1 || {
    echo "HARBOR_HEALTHCHECK_URL requires curl" >&2
    exit 2
  }
  curl -fsS --max-time 5 "${HARBOR_HEALTHCHECK_URL}" >/dev/null || {
    echo "Harbor dependency is unhealthy before launch: ${HARBOR_HEALTHCHECK_URL}" >&2
    exit 69
  }
fi

"${HARBOR_BIN}" "$@" &
harbor_pid=$!
if [ -n "${HARBOR_HEALTHCHECK_URL}" ]; then
  monitor_health &
  health_pid=$!
fi

run_status=0
wait "${harbor_pid}" || run_status=$?
harbor_pid=""
if [ -n "${health_pid}" ] && kill -0 "${health_pid}" >/dev/null 2>&1; then
  kill "${health_pid}" >/dev/null 2>&1 || true
  wait "${health_pid}" >/dev/null 2>&1 || true
fi
health_pid=""
if [ -s "${health_failed_file}" ]; then
  run_status=70
fi
exit "${run_status}"
