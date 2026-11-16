#!/usr/bin/env bash
# Check the test-result cache against its Lean model
# (crates/gents/proofs/Proofs/Ci/TestCache.lean): it builds without `sorry`,
# and the gate's implementation matches its execution. The contract job's
# TLA+ runner checks the store protocol (crates/gents/proofs/tla/TestCacheStore.tla).
set -euo pipefail

proofs="crates/gents/proofs"
if grep -n -E '(^|[^[:alnum:]_])sorry([^[:alnum:]_]|$)' "${proofs}/Proofs/Ci/TestCache.lean" "${proofs}/TestCacheConformance.lean"; then
  echo "::error::the test cache model holds sorry"
  exit 1
fi
(cd "${proofs}" && lake build Proofs.Ci.TestCache)
python3 scripts/ci/test_test_cache.py
