#!/bin/sh
# Auth-only notarytool probe for the RC1 existing-artifact notary workflow.
#
# Guarantees:
# - The notary credentials are materialized in a 0700 temp dir; the .p8 file is
#   0600 and is removed by an EXIT/INT/TERM trap on every path.
# - The full `notarytool history` output, including any errors, is captured to
#   a 0600 file inside the same private temp dir; it is never echoed raw.
# - Only an allowlisted success line, or a fixed 403 / auth-failure diagnostic
#   naming the failing credential variables, may reach stdout/stderr.
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
NOTARY_DIR="$(mktemp -d "${TMPDIR:-/tmp}/gents-rc1-notary-auth.XXXXXX")"
chmod 0700 "${NOTARY_DIR}"
NOTARY_KEY_PATH="${NOTARY_DIR}/AuthKey_${MACOS_NOTARY_KEY_ID}.p8"
printf '%s\n' "${MACOS_NOTARY_API_KEY}" > "${NOTARY_KEY_PATH}"
chmod 0600 "${NOTARY_KEY_PATH}"
HISTORY_FILE="${NOTARY_DIR}/notary-history.json"

# Single auth probe; output (history or error) goes only to the private file.
status=0
xcrun notarytool history \
  --key "${NOTARY_KEY_PATH}" \
  --key-id "${MACOS_NOTARY_KEY_ID}" \
  --issuer "${MACOS_NOTARY_ISSUER_ID}" \
  --output-format json >"${HISTORY_FILE}" 2>"${NOTARY_DIR}/notary-history.err" || status=$?

if [ "${status}" -ne 0 ]; then
  if grep -q '403' "${HISTORY_FILE}" "${HISTORY_FILE}.err" 2>/dev/null; then
    echo "rc1-notary-auth: auth failure: notary auth rejected (HTTP 403); check MACOS_NOTARY_API_KEY/ISSUER_ID/KEY_ID validity and team permissions" >&2
  else
    echo "rc1-notary-auth: auth failure: notarytool history exited ${status}; credential material and provider errors withheld" >&2
  fi
  exit "${status}"
fi

# Structural check only: the payload shape is verified without printing it.
if ! jq -e 'type == "object"' "${HISTORY_FILE}" >/dev/null 2>&1; then
  echo "rc1-notary-auth: auth failure: unexpected notary history payload" >&2
  exit 3
fi

echo "rc1-notary-auth: success: auth-only preflight passed (notary history ok, no submission attempted)"
