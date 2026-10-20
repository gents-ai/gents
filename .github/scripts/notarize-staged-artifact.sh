#!/usr/bin/env bash
# Notarize an already Developer-ID-signed macOS .app that an operator staged,
# as a zip, in a fixed inbox on the self-hosted runner host. Nothing is built
# or re-signed. Stages, strictly in order:
#
#   1. Validate inputs.                                  (no credentials yet)
#   2. Verify the staged zip: SHA-256, single .app, strict deep signature,
#      TeamIdentifier, bundle identifier, hardened runtime, pinned leaf
#      certificate SHA-256; in submit mode it must not already be stapled.
#      Candidate binaries are never executed.               (no credentials yet)
#   3. Materialize notary credentials privately.
#   4. submit mode: duplicate guard against Apple's submission history using a
#      deterministic submission name derived from the artifact SHA-256; fail
#      closed on any match or if history is unavailable. Then exactly ONE
#      `notarytool submit --no-wait`, and the receipt (submission id) is
#      written and announced BEFORE any waiting.
#      resume mode: never submits; the supplied submission id must belong to
#      this artifact's deterministic submission name.
#   5. One `notarytool wait`, then `info` and `log`; status must be Accepted.
#   6. Staple the extracted .app, validate staple, re-verify signature,
#      Gatekeeper assessment, re-package, write SHA256SUMS.
#
# Credential handling mirrors rc1-notary-auth.sh: 0700 dir under RUNNER_TEMP,
# constant AuthKey.p8 (0600), raw notarytool stderr captured privately and
# only fixed diagnostics printed. Cleanup runs on EXIT/INT/TERM; it cannot
# cover SIGKILL or runner loss, so secrecy ultimately rests on the API key
# being revocable.
#
# Required env: ARTIFACT_SHA256, NOTARY_MODE (submit|resume), SUBMISSION_ID
# (resume only), NOTARY_INBOX_DIR, NOTARY_OUTPUT_DIR, EXPECTED_TEAM_ID,
# EXPECTED_BUNDLE_ID, EXPECTED_LEAF_CERT_SHA256, and (from stage 3 on)
# MACOS_NOTARY_API_KEY / MACOS_NOTARY_ISSUER_ID / MACOS_NOTARY_KEY_ID.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NOTARY_JSON="${SCRIPT_DIR}/notary_json.py"

WORK_DIR=""
CRED_DIR=""

cleanup() {
  local rc=$?
  for dir in "${CRED_DIR}" "${WORK_DIR}"; do
    if [[ -n "${dir}" && -d "${dir}" ]]; then
      rm -rf -P "${dir}" 2>/dev/null || rm -rf "${dir}" || true
    fi
  done
  exit "${rc}"
}
trap cleanup EXIT INT TERM

say() { printf 'notarize-staged: %s\n' "$*"; }
fail() {
  local code="$1"
  shift
  printf 'notarize-staged: error: %s\n' "$*" >&2
  exit "${code}"
}
summary() {
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '%s\n' "$*" >>"${GITHUB_STEP_SUMMARY}"
  fi
}

# Fixed, provider-text-free diagnostic for a failed notarytool call whose
# stderr was captured privately to $2.
notary_failure() {
  local what="$1" errfile="$2" status="$3"
  if grep -Eq 'HTTP status( code)?:? 403' "${errfile}" 2>/dev/null; then
    fail 20 "notarytool ${what}: notary credential rejected (HTTP 403); verify the credential material and its authorization state before retrying"
  fi
  fail 21 "notarytool ${what} exited ${status}; provider errors withheld"
}

# ---------------------------------------------------------------- 1. inputs
for var in ARTIFACT_SHA256 NOTARY_MODE NOTARY_INBOX_DIR NOTARY_OUTPUT_DIR \
  EXPECTED_TEAM_ID EXPECTED_BUNDLE_ID EXPECTED_LEAF_CERT_SHA256; do
  [[ -n "${!var:-}" ]] || fail 2 "required setting ${var} is empty"
done
SUBMISSION_ID="${SUBMISSION_ID:-}"

[[ "${ARTIFACT_SHA256}" =~ ^[0-9a-f]{64}$ ]] \
  || fail 2 "artifact_sha256 must be 64 lowercase hex characters"
[[ "${EXPECTED_LEAF_CERT_SHA256}" =~ ^[0-9a-f]{64}$ ]] \
  || fail 2 "EXPECTED_LEAF_CERT_SHA256 must be 64 lowercase hex characters"
case "${NOTARY_MODE}" in
  submit)
    [[ -z "${SUBMISSION_ID}" ]] || fail 2 "submission_id must be empty in submit mode"
    ;;
  resume)
    python3 "${NOTARY_JSON}" is-uuid "${SUBMISSION_ID}" 2>/dev/null \
      || fail 2 "resume mode requires submission_id to be a UUID"
    ;;
  *)
    fail 2 "mode must be submit or resume"
    ;;
esac

SHA16="${ARTIFACT_SHA256:0:16}"
SUBMISSION_NAME="gents-notary-${SHA16}.zip"
SOURCE_ZIP="${NOTARY_INBOX_DIR%/}/${ARTIFACT_SHA256}.zip"

umask 077
mkdir -p "${NOTARY_OUTPUT_DIR}"
tmp_base="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
WORK_DIR="$(mktemp -d "${tmp_base%/}/gents-notarize-work.XXXXXX")"
chmod 0700 "${WORK_DIR}"

# ------------------------------------------------- 2. artifact verification
[[ -f "${SOURCE_ZIP}" ]] || fail 3 "staged artifact not found: ${SOURCE_ZIP}"
# Work only on a private copy; the inbox original is never modified.
cp "${SOURCE_ZIP}" "${WORK_DIR}/${SUBMISSION_NAME}"
actual_sha="$(shasum -a 256 "${WORK_DIR}/${SUBMISSION_NAME}" | awk '{print $1}')"
[[ "${actual_sha}" == "${ARTIFACT_SHA256}" ]] \
  || fail 3 "artifact SHA-256 mismatch: expected ${ARTIFACT_SHA256}, got ${actual_sha}"
say "artifact sha256 verified: ${actual_sha}"

mkdir "${WORK_DIR}/x"
ditto -x -k "${WORK_DIR}/${SUBMISSION_NAME}" "${WORK_DIR}/x" \
  || fail 4 "could not extract staged zip"
apps=()
for candidate in "${WORK_DIR}/x"/*.app; do
  [[ -d "${candidate}" ]] && apps+=("${candidate}")
done
[[ "${#apps[@]}" -eq 1 ]] || fail 4 "expected exactly one top-level .app, found ${#apps[@]}"
APP="${apps[0]}"
APP_NAME="$(basename "${APP}")"

codesign --verify --deep --strict "${APP}" >/dev/null 2>&1 \
  || fail 5 "strict deep signature verification failed"
sig_details="$(codesign -dv --verbose=4 "${APP}" 2>&1)" \
  || fail 5 "could not read signature details"
team="$(printf '%s\n' "${sig_details}" | sed -n 's/^TeamIdentifier=//p' | head -n1)"
ident="$(printf '%s\n' "${sig_details}" | sed -n 's/^Identifier=//p' | head -n1)"
[[ "${team}" == "${EXPECTED_TEAM_ID}" ]] \
  || fail 5 "TeamIdentifier '${team}' does not match expected ${EXPECTED_TEAM_ID}"
[[ "${ident}" == "${EXPECTED_BUNDLE_ID}" ]] \
  || fail 5 "Identifier '${ident}' does not match expected ${EXPECTED_BUNDLE_ID}"
printf '%s\n' "${sig_details}" | grep -Eq '^CodeDirectory .*flags=0x[0-9a-f]*\([^)]*runtime[^)]*\)' \
  || fail 5 "hardened runtime flag not present"
codesign -d --extract-certificates="${WORK_DIR}/cert" "${APP}" >/dev/null 2>&1 \
  || fail 5 "could not extract signing certificates"
[[ -f "${WORK_DIR}/cert0" ]] || fail 5 "leaf certificate missing"
leaf_sha="$(shasum -a 256 "${WORK_DIR}/cert0" | awk '{print $1}')"
[[ "${leaf_sha}" == "${EXPECTED_LEAF_CERT_SHA256}" ]] \
  || fail 5 "leaf certificate SHA-256 ${leaf_sha} does not match pinned value"
say "signature verified: team ${team}, identifier ${ident}, hardened runtime, pinned leaf certificate"

if [[ "${NOTARY_MODE}" == "submit" ]]; then
  if xcrun stapler validate "${APP}" >/dev/null 2>&1; then
    fail 6 "artifact already carries a stapled ticket; nothing to submit"
  fi
fi

# ----------------------------------------------- 3. credential materialize
missing=""
[[ -n "${MACOS_NOTARY_API_KEY:-}" ]] || missing="${missing} MACOS_NOTARY_API_KEY"
[[ -n "${MACOS_NOTARY_ISSUER_ID:-}" ]] || missing="${missing} MACOS_NOTARY_ISSUER_ID"
[[ -n "${MACOS_NOTARY_KEY_ID:-}" ]] || missing="${missing} MACOS_NOTARY_KEY_ID"
[[ -z "${missing}" ]] || fail 7 "missing notary secret(s):${missing}"

CRED_DIR="$(mktemp -d "${tmp_base%/}/gents-notarize-cred.XXXXXX")"
chmod 0700 "${CRED_DIR}"
KEY_PATH="${CRED_DIR}/AuthKey.p8"
printf '%s\n' "${MACOS_NOTARY_API_KEY}" >"${KEY_PATH}"
chmod 0600 "${KEY_PATH}"
auth=(--key "${KEY_PATH}" --key-id "${MACOS_NOTARY_KEY_ID}" --issuer "${MACOS_NOTARY_ISSUER_ID}")

RECEIPT="${NOTARY_OUTPUT_DIR}/receipt.json"

# ------------------------------------------------ 4. duplicate guard/submit
if [[ "${NOTARY_MODE}" == "submit" ]]; then
  status=0
  xcrun notarytool history "${auth[@]}" --output-format json \
    >"${CRED_DIR}/history.json" 2>"${CRED_DIR}/history.err" || status=$?
  [[ "${status}" -eq 0 ]] || notary_failure history "${CRED_DIR}/history.err" "${status}"
  matches="$(python3 "${NOTARY_JSON}" history-matches "${CRED_DIR}/history.json" "${SUBMISSION_NAME}" 2>/dev/null)" \
    || fail 8 "notary history could not be parsed; refusing to submit"
  if [[ -n "${matches}" ]]; then
    printf 'notarize-staged: existing submission(s) for %s (id status createdDate):\n%s\n' \
      "${SUBMISSION_NAME}" "${matches}" >&2
    fail 9 "a submission for this artifact already exists; re-dispatch with mode=resume and its submission_id instead of submitting again"
  fi
  say "duplicate guard passed: no prior submission named ${SUBMISSION_NAME}"

  status=0
  xcrun notarytool submit "${WORK_DIR}/${SUBMISSION_NAME}" "${auth[@]}" --no-wait \
    --output-format json >"${CRED_DIR}/submit.json" 2>"${CRED_DIR}/submit.err" || status=$?
  [[ "${status}" -eq 0 ]] || notary_failure submit "${CRED_DIR}/submit.err" "${status}"
  SUBMISSION_ID="$(python3 "${NOTARY_JSON}" submit-id "${CRED_DIR}/submit.json" 2>/dev/null)" \
    || fail 10 "submit returned no parseable submission id; check notary history for ${SUBMISSION_NAME} before any retry"
fi

# Durable record before any waiting.
python3 "${NOTARY_JSON}" write-receipt "${RECEIPT}" \
  submission_id "${SUBMISSION_ID}" submission_name "${SUBMISSION_NAME}" \
  artifact_sha256 "${ARTIFACT_SHA256}" mode "${NOTARY_MODE}" \
  run_id "${GITHUB_RUN_ID:-local}" run_attempt "${GITHUB_RUN_ATTEMPT:-0}"
echo "::notice::notary submission_id=${SUBMISSION_ID} name=${SUBMISSION_NAME} mode=${NOTARY_MODE}"
summary "notary submission_id: \`${SUBMISSION_ID}\` (name \`${SUBMISSION_NAME}\`, mode ${NOTARY_MODE})"
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  printf 'submission_id=%s\n' "${SUBMISSION_ID}" >>"${GITHUB_OUTPUT}"
fi

# --------------------------------------------------------- 5. wait/inspect
status=0
xcrun notarytool wait "${SUBMISSION_ID}" "${auth[@]}" --timeout 2h \
  >"${CRED_DIR}/wait.out" 2>"${CRED_DIR}/wait.err" || status=$?
if [[ "${status}" -ne 0 ]]; then
  # Fall through to `info`, which decides; a 403 here is still reported.
  if grep -Eq 'HTTP status( code)?:? 403' "${CRED_DIR}/wait.err" 2>/dev/null; then
    notary_failure wait "${CRED_DIR}/wait.err" "${status}"
  fi
  say "notarytool wait exited ${status}; consulting submission info"
fi

status=0
xcrun notarytool info "${SUBMISSION_ID}" "${auth[@]}" --output-format json \
  >"${NOTARY_OUTPUT_DIR}/notary-info.json" 2>"${CRED_DIR}/info.err" || status=$?
[[ "${status}" -eq 0 ]] || notary_failure info "${CRED_DIR}/info.err" "${status}"
info_name="$(python3 "${NOTARY_JSON}" info-field "${NOTARY_OUTPUT_DIR}/notary-info.json" name 2>/dev/null)" \
  || fail 11 "submission info has no name"
[[ "${info_name}" == "${SUBMISSION_NAME}" ]] \
  || fail 11 "submission ${SUBMISSION_ID} is named '${info_name}', not ${SUBMISSION_NAME}; refusing"
notary_status="$(python3 "${NOTARY_JSON}" info-field "${NOTARY_OUTPUT_DIR}/notary-info.json" status 2>/dev/null)" \
  || fail 11 "submission info has no status"

status=0
xcrun notarytool log "${SUBMISSION_ID}" "${auth[@]}" "${NOTARY_OUTPUT_DIR}/notary-log.json" \
  >/dev/null 2>"${CRED_DIR}/log.err" || status=$?
issues="unavailable"
if [[ "${status}" -eq 0 ]]; then
  issues="$(python3 "${NOTARY_JSON}" log-issue-count "${NOTARY_OUTPUT_DIR}/notary-log.json" 2>/dev/null || echo unparsed)"
fi
say "notary status: ${notary_status}; log issues: ${issues}"
summary "notary status: **${notary_status}**; log issues: ${issues}"

if [[ "${notary_status}" != "Accepted" ]]; then
  if [[ "${notary_status}" == "In Progress" ]]; then
    fail 12 "submission ${SUBMISSION_ID} still in progress; re-dispatch with mode=resume and this submission_id (do not submit again)"
  fi
  fail 12 "submission ${SUBMISSION_ID} was not accepted (status ${notary_status}); see notary-log.json"
fi

# ------------------------------------------------------ 6. staple/validate
xcrun stapler staple "${APP}" >/dev/null 2>"${CRED_DIR}/staple.err" \
  || fail 13 "stapler staple failed"
xcrun stapler validate "${APP}" >/dev/null 2>&1 || fail 13 "stapled ticket did not validate"
codesign --verify --deep --strict "${APP}" >/dev/null 2>&1 \
  || fail 13 "signature verification failed after stapling"
gatekeeper="$(spctl -a -vvv -t exec "${APP}" 2>&1)" \
  || fail 13 "Gatekeeper assessment rejected the stapled app"
printf '%s\n' "${gatekeeper}" | grep -q '^source=Notarized Developer ID' \
  || fail 13 "Gatekeeper source is not Notarized Developer ID"

OUT_ZIP="${NOTARY_OUTPUT_DIR}/${APP_NAME%.app}-${SHA16}-notarized.zip"
ditto -c -k --keepParent "${APP}" "${OUT_ZIP}" || fail 14 "could not package stapled app"
(
  cd "${NOTARY_OUTPUT_DIR}"
  shasum -a 256 "$(basename "${OUT_ZIP}")" >SHA256SUMS
)
say "stapled, validated and packaged: $(cat "${NOTARY_OUTPUT_DIR}/SHA256SUMS")"
summary "stapled output: \`$(cat "${NOTARY_OUTPUT_DIR}/SHA256SUMS")\`"
