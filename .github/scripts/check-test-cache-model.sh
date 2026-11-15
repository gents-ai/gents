#!/usr/bin/env bash
# Check the test-result cache's models (scripts/ci/test-cache): the Lean proofs
# build without `sorry`, the gate's implementation matches the Lean model's
# execution, and TLC finds the store protocol safe and its expiring variant not.
set -euo pipefail

model="scripts/ci/test-cache"
if grep -rn -E '(^|[^[:alnum:]_])sorry([^[:alnum:]_]|$)' --include='*.lean' --exclude-dir=.lake "${model}"; then
  echo "::error::the test cache model holds sorry"
  exit 1
fi
(cd "${model}" && lake build)
python3 scripts/ci/test_test_cache.py

version="1.7.4"
sha256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
jar="${RUNNER_TEMP:-/tmp}/tla2tools-${version}.jar"
if [[ ! -f "${jar}" ]]; then
  curl -fsSL --retry 3 -o "${jar}" "https://github.com/tlaplus/tlaplus/releases/download/v${version}/tla2tools.jar"
fi
echo "${sha256}  ${jar}" | sha256sum -c -
metadir="$(mktemp -d)"
trap 'rm -rf "${metadir}"' EXIT
tlc() {
  (cd "${model}" && java -cp "${jar}" tlc2.TLC -workers auto -metadir "${metadir}/$1" -config "$1.cfg" TestCacheStore.tla)
}
tlc TestCacheStore | tee "${metadir}/safe.log" | tail -3
grep -q "No error has been found" "${metadir}/safe.log"
if tlc TestCacheStoreExpiring > "${metadir}/expiring.log" 2>&1 || ! grep -q "Invariant ContradictionHonoured is violated" "${metadir}/expiring.log"; then
  echo "::error::TLC no longer finds the expiring-contradiction counterexample"
  exit 1
fi
rm -f "${model}"/TestCacheStore_TTrace_*
echo "TestCacheStoreExpiring: counterexample found, as intended"
