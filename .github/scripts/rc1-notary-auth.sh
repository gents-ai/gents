#!/bin/sh
# Auth-only notarytool probe: materializes the notary credentials in a private
# temp dir, runs the default notarytool history (read-only, no submission), and
# always cleans up the key material. Sanitized diagnostics only; never echoes
# credential contents or unsanitized provider errors. No submission is ever
# started here.

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

if [ "${MACOS_NOTARY_API_KEY:-}" = "" ] || [ "${MACOS_NOTARY_ISSUER_ID:-}" = "" ] || [ "${MACOS_NOTARY_KEY_ID:-}" = "" ]; then
  echo "rc1-notary-auth: missing MACOS_NOTARY_API_KEY / MACOS_NOTARY_ISSUER_ID / MACOS_NOTARY_KEY_ID" >&2
  exit 2
fi

NOTARY_DIR="$(mktemp -d "${TMPDIR:-/tmp}/gents-rc1-notary-auth.XXXXXX")"
chmod 0700 "${NOTARY_DIR}"
NOTARY_KEY_PATH="${NOTARY_DIR}/AuthKey_${MACOS_NOTARY_KEY_ID}.p8"
umask 077
printf '%s\n' "${MACOS_NOTARY_API_KEY}" > "${NOTARY_KEY_PATH}"
chmod 0600 "${NOTARY_KEY_PATH}"

history_output="$(xcrun notarytool history \
  --key "${NOTARY_KEY_PATH}" \
  --key-id "${MACOS_NOTARY_KEY_ID}" \
  --issuer "${MACOS_NOTARY_ISSUER_ID}" \
  --output-format json 2>&1)" || status=$?
status=${status:-0}

# Never let the key file path or its contents escape; sanitize before report.
printf '%s\n' "${history_output}" | sed -E \
  -e 's/(--key[ =])[^ ]+/\1<redacted>/g' \
  -e 's/(AuthKey_)[^.]+(\.p8)/\1<redacted>\2/g' \
  -e 's/-----BEGIN[A-Z ]*PRIVATE KEY-----/<private key redacted>/g'

if [ "${status}" -ne 0 ]; then
  case "${history_output}" in
    *403*)
      echo "rc1-notary-auth: notary auth rejected (HTTP 403); check credential validity and team permissions" >&2
      ;;
    *)
      echo "rc1-notary-auth: notary auth probe failed (exit ${status}); credential material withheld" >&2
      ;;
  esac
  exit "${status}"
fi

printf '%s\n' "${history_output}" | jq -e 'type == "object"' >/dev/null || {
  echo "rc1-notary-auth: unexpected history payload" >&2
  exit 3
}
echo "rc1-notary-auth: auth-only preflight succeeded (history ok, no submission)"
