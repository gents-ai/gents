#!/usr/bin/env bash
# Check the test-result cache against its models: the Lean model
# (crates/gents/proofs/Proofs/Ci/TestCache.lean) builds, the gate's
# implementation matches its execution, and TLC checks the store protocol
# (crates/gents/proofs/tla/TestCacheStore.tla) against each MC*.cfg's
# `expect:` header until the repository's TLA+ runner checks every spec.
set -euo pipefail

proofs="crates/gents/proofs"
if grep -n -E '(^|[^[:alnum:]_])sorry([^[:alnum:]_]|$)' "${proofs}/Proofs/Ci/TestCache.lean" "${proofs}/TestCacheConformance.lean"; then
  echo "::error::the test cache model holds sorry"
  exit 1
fi
(cd "${proofs}" && lake build Proofs.Ci.TestCache)
python3 scripts/ci/test_test_cache.py

version="1.7.4"
sha256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
jar="${RUNNER_TEMP:-/tmp}/tla2tools-${version}.jar"
if [[ ! -f "${jar}" ]]; then
  curl -fsSL --retry 3 -o "${jar}" "https://github.com/tlaplus/tlaplus/releases/download/v${version}/tla2tools.jar"
fi
echo "${sha256}  ${jar}" | sha256sum -c -
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
cp "${proofs}"/tla/TestCacheStore.tla "${proofs}"/tla/MCTestCacheStore*.tla "${proofs}"/tla/MCTestCacheStore*.cfg "${work}/"
for config in MCTestCacheStore MCTestCacheStoreExpiring; do
  expected="$(head -1 "${work}/${config}.cfg" | sed 's/^\\\* expect: //')"
  (cd "${work}" && java -cp "${jar}" tlc2.TLC -workers auto -metadir "${work}/meta-${config}" \
    -config "${config}.cfg" "${config}.tla" > "${config}.log" 2>&1) || true
  if grep -q "No error has been found" "${work}/${config}.log"; then
    actual="holds"
  elif violated="$(grep -o -E 'Invariant [A-Za-z_]+ is violated' "${work}/${config}.log" | head -1)" && [[ -n "${violated}" ]]; then
    actual="violates $(echo "${violated}" | awk '{print $2}')"
  else
    actual="error"
  fi
  echo "${config}: expected ${expected}, TLC ${actual}"
  if [[ "${actual}" != "${expected}" ]]; then
    tail -30 "${work}/${config}.log"
    exit 1
  fi
done
