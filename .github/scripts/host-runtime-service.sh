#!/usr/bin/env bash
# Backstop for self-hosted macOS jobs, which share a host with its live Gents
# node: `record` notes whether the runner user's ai.gents.runtime LaunchAgent is
# running, `verify` fails the job when it was running and no longer is. Test
# builds must never reach the real service manager
# (crates/gents-service/src/native_service/isolation.rs); this catches a
# regression after the fact. Read-only: it only ever runs `launchctl print`,
# and never starts, stops or changes the service.
set -euo pipefail

label="ai.gents.runtime"
target="gui/$(id -u)/${label}"
record_file="${RUNNER_TEMP:?RUNNER_TEMP must be set}/gents-host-runtime-service.state"

service_state() {
  local printed
  if ! printed="$(launchctl print "${target}" 2>/dev/null)"; then
    echo absent
  elif grep -q 'state = running' <<<"${printed}"; then
    echo running
  else
    echo stopped
  fi
}

case "${1:-}" in
  record)
    state="$(service_state)"
    echo "${state}" > "${record_file}"
    if [[ "${state}" == absent ]]; then
      echo "This host has no ${target}; nothing to watch."
    else
      echo "${target} is ${state} before this job's steps."
    fi
    ;;
  verify)
    if [[ ! -s "${record_file}" ]]; then
      echo "No state was recorded before this job's steps; nothing to compare."
      exit 0
    fi
    before="$(cat "${record_file}")"
    after="$(service_state)"
    echo "${target}: ${before} before this job, ${after} now."
    if [[ "${before}" == running && "${after}" != running ]]; then
      message="The host's live Gents node (${target}) was running when this job started and is ${after} now. A step of this job very probably reached the real service manager: a test or the binary it spawned must address a test-only service, never ${label}. Do not rerun this job on this host. A person must restart the node on $(hostname -s) and find the step that stopped it (the host's system log shows which process ran launchctl)."
      echo "::error title=Live Gents node stopped::${message}"
      if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
        printf '### Live Gents node stopped\n\n%s\n' "${message}" >> "${GITHUB_STEP_SUMMARY}"
      fi
      exit 1
    fi
    ;;
  *)
    echo "usage: $0 record|verify" >&2
    exit 2
    ;;
esac
