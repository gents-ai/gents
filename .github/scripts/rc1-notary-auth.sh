#!/bin/sh
# Auth-only notarytool probe for the RC1 existing-artifact notary workflow.
#
# Guarantees:
# - The notary credentials are materialized in a private temp dir (0700 dir,
#   0600 .p8 file). The preferred location is $RUNNER_TEMP (runner-scoped,
#   cleaned by the runner between jobs); $TMPDIR or /tmp is the fallback.
#   The EXIT/INT/TERM trap removes the whole dir on every in-process exit.
#   NOTE: this cleanup is best-effort. It cannot guarantee removal if the
#   process is killed with SIGKILL, the runner loses power, or the job is
#   reaped externally; durable secrecy therefore rests on the credential's
#   revocability, not on this trap.
# - The full `notarytool history` stdout and stderr, including any errors, are
#   captured to 0600 files inside the private temp dir; they are never echoed.
# - Failure classification is driven ONLY by the HTTP status in the private
#   stderr capture. Only an allowlisted success line, or one of the fixed
#   failure diagnostics below (which name the credential variable names and
#   give neutral guidance), may reach stdout/stderr. Raw history/error output
#   is never printed.
# - The probe runs exactly once; there is no retry loop. No submission, upload,
#   or notary state change is ever attempted here.

set -eu

NOTARY_DIR=""

cleanup() {
  rc=$?
  if [ -n "${NOTARY_DIR}" ] && [ -d "${NOTARY_DIR}" ]; then
    rm -rf -P "${NOTARY_DIR}" 2>/dev/null || rm -rf "${NOTARY_DIR}" || true
  fi
  exit "${rc}"
}
trap cleanup EXIT INT TERM

# Fail before touching notarytool if any credential is absent; nothing about
# the missing credentials beyond their variable names is reported.
missing=""
[ "${MACOS_NOTARY_API_KEY:-}" != "" ] || missing="${missing} MACOS_NOTARY_API_KEY"
[ "${MACOS_NOTARY_ISSUER_ID:-}" != "" ] || missing="${missing} MACOS_NOTARY_ISSUER_ID"
[ "${MACOS_NOTARY_KEY_ID:-}" != "" ] || missing="${missing} MACOS_NOTARY_KEY_ID"
if [ -n "${missing}" ]; then
  echo "rc1-notary-auth: auth failure: missing notary secret(s):${missing}" >&2
  exit 2
fi

umask 077
# Prefer the runner-scoped private temp; fall back to TMPDIR or /tmp.
notary_tmp_base="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
NOTARY_DIR="$(mktemp -d "${notary_tmp_base%/}/gents-rc1-notary-auth.XXXXXX")"
chmod 0700 "${NOTARY_DIR}"
# Constant key filename: never embed the key id in a path that could leak it.
NOTARY_KEY_PATH="${NOTARY_DIR}/AuthKey.p8"
printf '%s\n' "${MACOS_NOTARY_API_KEY}" > "${NOTARY_KEY_PATH}"
chmod 0600 "${NOTARY_KEY_PATH}"
HISTORY_FILE="${NOTARY_DIR}/notary-history.json"
ERROR_FILE="${NOTARY_DIR}/notary-history.err"

# Single auth probe; output (history or error) goes only to the private files.
status=0
xcrun notarytool history \
  --key "${NOTARY_KEY_PATH}" \
  --key-id "${MACOS_NOTARY_KEY_ID}" \
  --issuer "${MACOS_NOTARY_ISSUER_ID}" \
  --output-format json >"${HISTORY_FILE}" 2>"${ERROR_FILE}" || status=$?

if [ "${status}" -ne 0 ]; then
  # Classification reads only the private stderr capture and is driven by the
  # HTTP status alone; provider wording (e.g. agreement language) never
  # selects the diagnostic, and the captured body is never printed.
  if grep -q 'HTTP status 403' "${ERROR_FILE}" 2>/dev/null; then
    echo "rc1-notary-auth: auth failure: notary credential rejected (HTTP 403); verify the credential material and its authorization state before retrying" >&2
  else
    echo "rc1-notary-auth: auth failure: notarytool history exited ${status}; credential material and provider errors withheld" >&2
  fi
  exit "${status}"
fi

# Success requires a non-empty private history capture; its contents are
# verified only by size, never parsed or printed.
if [ ! -s "${HISTORY_FILE}" ]; then
  echo "rc1-notary-auth: auth failure: notary history returned no data" >&2
  exit 3
fi

echo "rc1-notary-auth: success: auth-only preflight passed (notary history ok, no submission attempted)"
