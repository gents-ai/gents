#!/usr/bin/env bash
# Append one JSONL latency sample to the `ci-latency-data` branch (W53).
#
# Input: a JSON document on stdin with {run_id, job, steps: [{step, seconds}],
# total_seconds}. The caller builds it from the per-run JSON block that
# scripts/ci/gate.py writes to $GITHUB_STEP_SUMMARY (fenced ```json).
#
# The store is the `ci-latency-data` branch: one line per job appended to
# `latency.jsonl`, committed and pushed with a fetch-rebase-retry loop so
# concurrent writers serialize. A failed push after the retries is a lost
# sample, never a failed run: latency bookkeeping must not break CI.
set -euo pipefail

BRANCH=ci-latency-data
FILE=latency.jsonl
RETRIES=5
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)

if ! sample=$(jq -ce --arg timestamp "$TIMESTAMP" \
  '{timestamp: $timestamp, run_id: .run_id, job: .job,
    steps: [.steps[] | {step, seconds: (.seconds | tonumber)}],
    total_seconds: (.total_seconds | tonumber)}'); then
  echo "ci-latency: malformed input; skipping the sample" >&2
  exit 0
fi

remote_url=$(git remote get-url "${CI_LATENCY_REMOTE:-origin}")
work=$(mktemp -d "${TMPDIR:-/tmp}/ci-latency.XXXXXX")
trap 'rm -rf "$work"' EXIT

# Token reaches git through the environment, never argv or .git/config.
if [[ -n "${GITHUB_TOKEN:-}" ]]; then
  basic=$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')
  export GIT_CONFIG_COUNT=1 \
    GIT_CONFIG_KEY_0=http.https://github.com/.extraheader \
    GIT_CONFIG_VALUE_0="AUTHORIZATION: basic ${basic}"
fi

git clone --depth 1 --branch "$BRANCH" "$remote_url" "$work/store" 2>/dev/null \
  || { echo "ci-latency: cannot clone $BRANCH; skipping the sample" >&2; exit 0; }
cd "$work/store"
git config user.name "ci-latency-bot"
git config user.email "ci-latency-bot@users.noreply.github.com"

printf '%s\n' "$sample" >> "$FILE"

git add "$FILE"

if ! git commit -q -m "[ci-omit] latency sample ${TIMESTAMP}"; then
  echo "ci-latency: nothing to record" >&2
  exit 0
fi

for _ in $(seq "$RETRIES"); do
  if git push -q origin "$BRANCH"; then
    echo "ci-latency: recorded ${sample}"
    exit 0
  fi
  git fetch -q origin "$BRANCH"
  git rebase -q "origin/$BRANCH" || git rebase --abort
done
echo "ci-latency: gave up pushing after ${RETRIES} retries; sample lost" >&2
exit 0
