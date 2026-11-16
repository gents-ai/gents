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
# Self-hosted Linux runners have no JDK: install the pinned Temurin 17.
if ! command -v java >/dev/null 2>&1; then
  jdk_version="17.0.21+9"
  jdk_asset="OpenJDK17U-jdk_x64_linux_hotspot_17.0.21_9.tar.gz"
  jdk_sha256="8b9b611d917e3dfd8419d3566b5949226f69b052fed7e109c1368add1dfe8711"
  jdk_dir="${RUNNER_TOOL_CACHE:-${HOME}/.cache}/temurin/${jdk_version}"
  if [[ ! -x "${jdk_dir}/bin/java" ]]; then
    staging="$(mktemp -d)"
    curl -fsSL --retry 3 -o "${staging}/${jdk_asset}" \
      "https://github.com/adoptium/temurin17-binaries/releases/download/jdk-17.0.21%2B9/${jdk_asset}"
    echo "${jdk_sha256}  ${staging}/${jdk_asset}" | sha256sum -c -
    mkdir -p "${jdk_dir}"
    tar -xzf "${staging}/${jdk_asset}" -C "${jdk_dir}" --strip-components=1
    rm -rf "${staging}"
  fi
  export PATH="${jdk_dir}/bin:${PATH}"
fi
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
    tail -n 30 "${work}/${config}.log"
    exit 1
  fi
done
