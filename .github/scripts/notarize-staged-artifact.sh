#!/usr/bin/env bash
# Notarize an already Developer-ID-signed macOS .app that an operator staged,
# as a zip, in a fixed inbox on the self-hosted runner host. Nothing is built
# or re-signed and nothing is published. The workflow runs three phases as
# separate steps so that only the credentialed phases receive notary secrets:
#
#   verify    (no secrets)  validate inputs; copy the staged zip privately and
#             check its SHA-256; validate every archive entry BEFORE extraction
#             (paths, types, symlink containment, single .app root); extract;
#             re-check the extracted tree physically; strict deep codesign,
#             TeamIdentifier, bundle id, hardened runtime, pinned leaf
#             certificate; in submit mode the app must not already be stapled.
#             Candidate binaries are never executed.
#   submit    (secrets)     submit mode: refuse if the host state directory
#             already holds ANY record for this artifact's full SHA-256; strict
#             history check (secondary); durably create intent.json; exactly
#             one `notarytool submit --no-wait`; announce the id; write
#             receipt.json. Failures after the intent exists are recorded as
#             uncertain and require reconciliation, never a blind resubmit.
#             resume mode: never submits; checks the id against any recorded
#             receipt and Apple's submission info BEFORE any waiting.
#             (The workflow uploads the receipt as an artifact right after.)
#   complete  (secrets)     one `notarytool wait`; info must be Accepted;
#             Apple's log MUST be retrievable and must bind jobId to the
#             submission id and sha256 to the full staged SHA-256; staple,
#             validate staple, strict codesign, Gatekeeper; package; SHA256SUMS.
#
# Credential handling mirrors rc1-notary-auth.sh: a per-step 0700 directory
# under RUNNER_TEMP holding a constant AuthKey.p8 (0600); raw notarytool
# stderr is captured privately and never printed. Provider-derived values that
# are printed (ids, names, statuses, dates) are restricted to a safe character
# set. Apple's info/log JSON (non-secret provider data) is written to the
# output directory and uploaded. Cleanup runs on EXIT/INT/TERM; it cannot
# cover SIGKILL or runner loss, so secrecy ultimately rests on the API key
# being revocable.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NOTARY_JSON="${SCRIPT_DIR}/notary_json.py"
PHASE="${1:-}"
CRED_DIR=""

cleanup() {
  local rc=$?
  if [[ -n "${CRED_DIR}" && -d "${CRED_DIR}" ]]; then
    rm -rf -P "${CRED_DIR}" 2>/dev/null || rm -rf "${CRED_DIR}" || true
  fi
  exit "${rc}"
}
trap cleanup EXIT INT TERM

say() { printf 'notarize-staged[%s]: %s\n' "${PHASE}" "$*"; }
fail() {
  local code="$1"
  shift
  printf 'notarize-staged[%s]: error: %s\n' "${PHASE}" "$*" >&2
  exit "${code}"
}
summary() {
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '%s\n' "$*" >>"${GITHUB_STEP_SUMMARY}"
  fi
}
safe() { python3 "${NOTARY_JSON}" safe "$1"; }
json() { python3 "${NOTARY_JSON}" "$@"; }

# Fixed diagnostic for a failed notarytool call whose stderr was captured
# privately to $2. Provider text is never printed.
notary_failure() {
  local what="$1" errfile="$2" status="$3" hint="${4:-}"
  if grep -Eq 'HTTP status( code)?:? 403' "${errfile}" 2>/dev/null; then
    fail 20 "notarytool ${what}: notary credential rejected (HTTP 403); verify the credential material and its authorization state before retrying${hint:+; ${hint}}"
  fi
  fail 21 "notarytool ${what} exited ${status}; provider errors withheld${hint:+; ${hint}}"
}

# --------------------------------------------------------- common inputs
for var in ARTIFACT_SHA256 NOTARY_MODE RUN_DIR; do
  [[ -n "${!var:-}" ]] || fail 2 "required setting ${var} is empty"
done
SUBMISSION_ID="${SUBMISSION_ID:-}"
[[ "${ARTIFACT_SHA256}" =~ ^[0-9a-f]{64}$ ]] \
  || fail 2 "artifact_sha256 must be 64 lowercase hex characters"
case "${NOTARY_MODE}" in
  submit)
    [[ -z "${SUBMISSION_ID}" ]] || fail 2 "submission_id must be empty in submit mode"
    ;;
  resume)
    json is-uuid "${SUBMISSION_ID}" 2>/dev/null \
      || fail 2 "resume mode requires submission_id to be a UUID"
    ;;
  *)
    fail 2 "mode must be submit or resume"
    ;;
esac

SHA16="${ARTIFACT_SHA256:0:16}"
SUBMISSION_NAME="gents-notary-${SHA16}.zip"
WORK="${RUN_DIR}/work"
OUT="${RUN_DIR}/out"
SUBMIT_ZIP="${WORK}/${SUBMISSION_NAME}"
VERIFIED="${RUN_DIR}/verified.json"
umask 077

materialize_credentials() {
  local missing=""
  [[ -n "${MACOS_NOTARY_API_KEY:-}" ]] || missing="${missing} MACOS_NOTARY_API_KEY"
  [[ -n "${MACOS_NOTARY_ISSUER_ID:-}" ]] || missing="${missing} MACOS_NOTARY_ISSUER_ID"
  [[ -n "${MACOS_NOTARY_KEY_ID:-}" ]] || missing="${missing} MACOS_NOTARY_KEY_ID"
  [[ -z "${missing}" ]] || fail 7 "missing notary secret(s):${missing}"
  local tmp_base="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
  CRED_DIR="$(mktemp -d "${tmp_base%/}/gents-notarize-cred.XXXXXX")"
  chmod 0700 "${CRED_DIR}"
  KEY_PATH="${CRED_DIR}/AuthKey.p8"
  printf '%s\n' "${MACOS_NOTARY_API_KEY}" >"${KEY_PATH}"
  chmod 0600 "${KEY_PATH}"
  AUTH=(--key "${KEY_PATH}" --key-id "${MACOS_NOTARY_KEY_ID}" --issuer "${MACOS_NOTARY_ISSUER_ID}")
}

require_verified() {
  [[ -f "${VERIFIED}" ]] || fail 3 "verify phase has not completed for this run"
  [[ "$(json json-field "${VERIFIED}" artifact_sha256)" == "${ARTIFACT_SHA256}" ]] \
    || fail 3 "verified artifact does not match artifact_sha256"
  APP_NAME="$(json json-field "${VERIFIED}" app_name)"
  APP="${WORK}/x/${APP_NAME}"
  [[ -d "${APP}" && ! -L "${APP}" ]] || fail 3 "verified app is missing"
  local actual
  actual="$(shasum -a 256 "${SUBMIT_ZIP}" | awk '{print $1}')"
  [[ "${actual}" == "${ARTIFACT_SHA256}" ]] || fail 3 "private artifact copy changed since verification"
}

# ================================================================ verify
phase_verify() {
  for var in NOTARY_INBOX_DIR EXPECTED_TEAM_ID EXPECTED_BUNDLE_ID EXPECTED_LEAF_CERT_SHA256; do
    [[ -n "${!var:-}" ]] || fail 2 "required setting ${var} is empty"
  done
  [[ "${EXPECTED_LEAF_CERT_SHA256}" =~ ^[0-9a-f]{64}$ ]] \
    || fail 2 "EXPECTED_LEAF_CERT_SHA256 must be 64 lowercase hex characters"
  if [[ -n "${MACOS_NOTARY_API_KEY:-}${MACOS_NOTARY_ISSUER_ID:-}${MACOS_NOTARY_KEY_ID:-}" ]]; then
    fail 2 "verify phase must run without notary credentials in its environment"
  fi
  [[ ! -e "${RUN_DIR}" ]] || fail 2 "run directory already exists: ${RUN_DIR}"
  mkdir -p "$(dirname "${RUN_DIR}")"
  mkdir -m 0700 "${RUN_DIR}"
  mkdir -m 0700 "${WORK}" "${OUT}"

  local source_zip="${NOTARY_INBOX_DIR%/}/${ARTIFACT_SHA256}.zip"
  [[ -f "${source_zip}" && ! -L "${source_zip}" ]] || fail 3 "staged artifact not found: ${source_zip}"
  # Work only on a private copy; the inbox original is never modified.
  cp "${source_zip}" "${SUBMIT_ZIP}"
  chmod 0400 "${SUBMIT_ZIP}"
  local actual
  actual="$(shasum -a 256 "${SUBMIT_ZIP}" | awk '{print $1}')"
  [[ "${actual}" == "${ARTIFACT_SHA256}" ]] \
    || fail 3 "artifact SHA-256 mismatch: expected ${ARTIFACT_SHA256}, got ${actual}"
  say "artifact sha256 verified: ${actual}"

  local root
  root="$(json zip-inspect "${SUBMIT_ZIP}")" || fail 4 "archive failed pre-extraction containment checks"
  mkdir -m 0700 "${WORK}/x"
  ditto -x -k "${SUBMIT_ZIP}" "${WORK}/x" || fail 4 "could not extract staged zip"
  json tree-check "${WORK}/x" "${root}" || fail 4 "extracted tree failed containment checks"
  local app="${WORK}/x/${root}"
  say "archive contained: single app root ${root}"

  codesign --verify --deep --strict "${app}" >/dev/null 2>&1 \
    || fail 5 "strict deep signature verification failed"
  local details team ident
  details="$(codesign -dv --verbose=4 "${app}" 2>&1)" || fail 5 "could not read signature details"
  team="$(printf '%s\n' "${details}" | sed -n 's/^TeamIdentifier=//p' | head -n1)"
  ident="$(printf '%s\n' "${details}" | sed -n 's/^Identifier=//p' | head -n1)"
  [[ "${team}" == "${EXPECTED_TEAM_ID}" ]] \
    || fail 5 "TeamIdentifier '$(safe "${team}")' does not match expected ${EXPECTED_TEAM_ID}"
  [[ "${ident}" == "${EXPECTED_BUNDLE_ID}" ]] \
    || fail 5 "Identifier '$(safe "${ident}")' does not match expected ${EXPECTED_BUNDLE_ID}"
  printf '%s\n' "${details}" | grep -Eq '^CodeDirectory .*flags=0x[0-9a-f]*\([^)]*runtime[^)]*\)' \
    || fail 5 "hardened runtime flag not present"
  codesign -d --extract-certificates="${WORK}/cert" "${app}" >/dev/null 2>&1 \
    || fail 5 "could not extract signing certificates"
  [[ -f "${WORK}/cert0" ]] || fail 5 "leaf certificate missing"
  local leaf
  leaf="$(shasum -a 256 "${WORK}/cert0" | awk '{print $1}')"
  [[ "${leaf}" == "${EXPECTED_LEAF_CERT_SHA256}" ]] \
    || fail 5 "leaf certificate SHA-256 ${leaf} does not match pinned value"
  say "signature verified: team ${team}, identifier ${ident}, hardened runtime, pinned leaf certificate"

  if [[ "${NOTARY_MODE}" == "submit" ]] && xcrun stapler validate "${app}" >/dev/null 2>&1; then
    fail 6 "artifact already carries a stapled ticket; nothing to submit"
  fi

  json write-json-excl "${VERIFIED}" artifact_sha256 "${ARTIFACT_SHA256}" app_name "${root}" \
    submission_name "${SUBMISSION_NAME}" mode "${NOTARY_MODE}"
}

# ================================================================ submit
phase_submit() {
  [[ -n "${NOTARY_STATE_DIR:-}" ]] || fail 2 "required setting NOTARY_STATE_DIR is empty"
  require_verified
  local state="${NOTARY_STATE_DIR%/}/${ARTIFACT_SHA256}"
  materialize_credentials
  local status=0

  if [[ "${NOTARY_MODE}" == "resume" ]]; then
    if [[ -f "${state}/receipt.json" ]]; then
      local recorded
      recorded="$(json json-field "${state}/receipt.json" submission_id)" \
        || fail 11 "recorded receipt for this artifact is unreadable; reconcile state manually"
      [[ "${recorded}" == "${SUBMISSION_ID}" ]] \
        || fail 11 "submission_id does not match the id recorded for this artifact ($(safe "${recorded}"))"
    fi
    xcrun notarytool info "${SUBMISSION_ID}" "${AUTH[@]}" --output-format json \
      >"${OUT}/notary-info-preflight.json" 2>"${CRED_DIR}/info.err" || status=$?
    [[ "${status}" -eq 0 ]] || notary_failure info "${CRED_DIR}/info.err" "${status}"
    local pre_status
    pre_status="$(json info-check "${OUT}/notary-info-preflight.json" "${SUBMISSION_ID}" "${SUBMISSION_NAME}" 2>/dev/null)" \
      || fail 11 "submission ${SUBMISSION_ID} does not belong to artifact ${SUBMISSION_NAME}; refusing before waiting"
    echo "::notice::notary resume submission_id=${SUBMISSION_ID} name=${SUBMISSION_NAME} status=${pre_status}"
    summary "notary resume: \`${SUBMISSION_ID}\` (\`${SUBMISSION_NAME}\`), status at resume: ${pre_status}"
    json write-json-excl "${OUT}/receipt.json" submission_id "${SUBMISSION_ID}" \
      submission_name "${SUBMISSION_NAME}" artifact_sha256 "${ARTIFACT_SHA256}" mode resume \
      run_id "${GITHUB_RUN_ID:-local}" run_attempt "${GITHUB_RUN_ATTEMPT:-0}"
    return 0
  fi

  # --- submit mode: host state is the primary duplicate authority.
  mkdir -p "${NOTARY_STATE_DIR}"
  chmod 0700 "${NOTARY_STATE_DIR}"
  if [[ -e "${state}" ]]; then
    local f
    for f in "${state}"/*.json; do
      [[ -f "${f}" ]] || continue
      printf 'notarize-staged[submit]: existing record %s: submission_id=%s state=%s\n' \
        "$(basename "${f}")" \
        "$(json json-field "${f}" submission_id 2>/dev/null || echo none)" \
        "$(json json-field "${f}" state 2>/dev/null || echo unknown)" >&2
    done
    fail 9 "a notarization record already exists for this artifact; use mode=resume with its submission_id, or reconcile ${state} per the runbook"
  fi

  # Secondary guard: Apple history (completeness cannot be verified).
  xcrun notarytool history "${AUTH[@]}" --output-format json \
    >"${CRED_DIR}/history.json" 2>"${CRED_DIR}/history.err" || status=$?
  [[ "${status}" -eq 0 ]] || notary_failure history "${CRED_DIR}/history.err" "${status}"
  local matches
  matches="$(json history-matches "${CRED_DIR}/history.json" "${SUBMISSION_NAME}" 2>/dev/null)" \
    || fail 8 "notary history was incomplete or unparseable; refusing to submit"
  if [[ -n "${matches}" ]]; then
    printf 'notarize-staged[submit]: existing submission(s) named %s (id status createdDate):\n%s\n' \
      "${SUBMISSION_NAME}" "${matches}" >&2
    fail 9 "a submission for this artifact already exists in notary history; use mode=resume with its submission_id"
  fi

  # Durable intent BEFORE the external operation.
  mkdir -m 0700 "${state}" || fail 9 "could not create state for this artifact (concurrent attempt?)"
  json write-json-excl "${state}/intent.json" state submitting artifact_sha256 "${ARTIFACT_SHA256}" \
    submission_name "${SUBMISSION_NAME}" run_id "${GITHUB_RUN_ID:-local}" run_attempt "${GITHUB_RUN_ATTEMPT:-0}"
  cp "${state}/intent.json" "${OUT}/intent.json"

  xcrun notarytool submit "${SUBMIT_ZIP}" "${AUTH[@]}" --no-wait --output-format json \
    >"${CRED_DIR}/submit.json" 2>"${CRED_DIR}/submit.err" || status=$?
  if [[ "${status}" -ne 0 ]]; then
    json write-json-excl "${state}/outcome.json" state uncertain reason submit-exit-nonzero \
      run_id "${GITHUB_RUN_ID:-local}" || true
    cp "${state}/outcome.json" "${OUT}/outcome.json" 2>/dev/null || true
    notary_failure submit "${CRED_DIR}/submit.err" "${status}" \
      "outcome recorded as uncertain; check notary history and ${state} before any retry"
  fi
  local id
  if ! id="$(json submit-id "${CRED_DIR}/submit.json" 2>/dev/null)"; then
    json write-json-excl "${state}/outcome.json" state uncertain reason no-submission-id \
      run_id "${GITHUB_RUN_ID:-local}" || true
    cp "${state}/outcome.json" "${OUT}/outcome.json" 2>/dev/null || true
    fail 10 "submit returned no parseable submission id; outcome recorded as uncertain; check notary history and ${state} before any retry"
  fi

  # Announce first (log evidence), then persist.
  echo "::notice::notary submission_id=${id} name=${SUBMISSION_NAME} mode=submit"
  summary "notary submission_id: \`${id}\` (name \`${SUBMISSION_NAME}\`, mode submit)"
  if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf 'submission_id=%s\n' "${id}" >>"${GITHUB_OUTPUT}"
  fi
  json write-json-excl "${state}/receipt.json" state submitted submission_id "${id}" \
    submission_name "${SUBMISSION_NAME}" artifact_sha256 "${ARTIFACT_SHA256}" mode submit \
    run_id "${GITHUB_RUN_ID:-local}" run_attempt "${GITHUB_RUN_ATTEMPT:-0}"
  cp "${state}/receipt.json" "${OUT}/receipt.json"
}

# ============================================================== complete
phase_complete() {
  require_verified
  [[ -f "${OUT}/receipt.json" ]] || fail 3 "no receipt from the submit phase"
  local id
  id="$(json json-field "${OUT}/receipt.json" submission_id)" || fail 3 "receipt has no submission id"
  json is-uuid "${id}" 2>/dev/null || fail 3 "receipt submission id is malformed"
  if [[ "${NOTARY_MODE}" == "resume" ]]; then
    [[ "${id}" == "${SUBMISSION_ID}" ]] || fail 3 "receipt id differs from submission_id"
  fi
  materialize_credentials
  local status=0

  xcrun notarytool wait "${id}" "${AUTH[@]}" --timeout 2h \
    >"${CRED_DIR}/wait.out" 2>"${CRED_DIR}/wait.err" || status=$?
  if [[ "${status}" -ne 0 ]]; then
    if grep -Eq 'HTTP status( code)?:? 403' "${CRED_DIR}/wait.err" 2>/dev/null; then
      notary_failure wait "${CRED_DIR}/wait.err" "${status}" "resume with this submission_id; do not submit again"
    fi
    say "notarytool wait exited ${status}; consulting submission info"
  fi

  status=0
  xcrun notarytool info "${id}" "${AUTH[@]}" --output-format json \
    >"${OUT}/notary-info.json" 2>"${CRED_DIR}/info.err" || status=$?
  [[ "${status}" -eq 0 ]] || notary_failure info "${CRED_DIR}/info.err" "${status}" "resume with this submission_id; do not submit again"
  local notary_status
  notary_status="$(json info-check "${OUT}/notary-info.json" "${id}" "${SUBMISSION_NAME}" 2>/dev/null)" \
    || fail 11 "submission info does not match this submission/artifact; refusing"
  say "notary status: ${notary_status}"
  summary "notary status: **${notary_status}**"

  if [[ "${notary_status}" == "In Progress" ]]; then
    fail 12 "submission ${id} still in progress; re-dispatch with mode=resume and this submission_id (do not submit again)"
  fi

  status=0
  xcrun notarytool log "${id}" "${AUTH[@]}" "${OUT}/notary-log.json" \
    >/dev/null 2>"${CRED_DIR}/log.err" || status=$?
  if [[ "${notary_status}" != "Accepted" ]]; then
    fail 12 "submission ${id} was not accepted (status ${notary_status}); see notary-log.json if retrieved"
  fi
  [[ "${status}" -eq 0 ]] || notary_failure log "${CRED_DIR}/log.err" "${status}" \
    "acceptance evidence unavailable; resume with this submission_id"
  local issues
  issues="$(json log-check "${OUT}/notary-log.json" "${id}" "${ARTIFACT_SHA256}" 2>/dev/null)" \
    || fail 12 "notary log does not bind submission ${id} to artifact ${ARTIFACT_SHA256} as Accepted; refusing to staple"
  say "notary log binds job ${id} to sha256 ${ARTIFACT_SHA256}; issues: ${issues}"
  summary "notary log: job \`${id}\`, archive sha256 \`${ARTIFACT_SHA256}\`, issues: ${issues}"

  xcrun stapler staple "${APP}" >/dev/null 2>"${CRED_DIR}/staple.err" || fail 13 "stapler staple failed"
  xcrun stapler validate "${APP}" >/dev/null 2>&1 || fail 13 "stapled ticket did not validate"
  codesign --verify --deep --strict "${APP}" >/dev/null 2>&1 \
    || fail 13 "signature verification failed after stapling"
  local gatekeeper
  gatekeeper="$(spctl -a -vvv -t exec "${APP}" 2>&1)" || fail 13 "Gatekeeper assessment rejected the stapled app"
  printf '%s\n' "${gatekeeper}" | grep -q '^source=Notarized Developer ID' \
    || fail 13 "Gatekeeper source is not Notarized Developer ID"

  local out_zip="${OUT}/${APP_NAME%.app}-${SHA16}-notarized.zip"
  ditto -c -k --keepParent "${APP}" "${out_zip}" || fail 14 "could not package stapled app"
  (
    cd "${OUT}"
    shasum -a 256 "$(basename "${out_zip}")" >SHA256SUMS
  )
  if [[ -n "${NOTARY_STATE_DIR:-}" && -d "${NOTARY_STATE_DIR%/}/${ARTIFACT_SHA256}" ]]; then
    json write-json-excl "${NOTARY_STATE_DIR%/}/${ARTIFACT_SHA256}/accepted-${id}.json" state accepted \
      submission_id "${id}" run_id "${GITHUB_RUN_ID:-local}" 2>/dev/null || true
  fi
  say "stapled, validated and packaged: $(cat "${OUT}/SHA256SUMS")"
  summary "stapled output: \`$(cat "${OUT}/SHA256SUMS")\`"
}

case "${PHASE}" in
  verify) phase_verify ;;
  submit) phase_submit ;;
  complete) phase_complete ;;
  *) fail 64 "usage: notarize-staged-artifact.sh verify|submit|complete" ;;
esac
