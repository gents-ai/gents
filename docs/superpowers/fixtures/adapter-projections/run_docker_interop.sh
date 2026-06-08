#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../../../.." && pwd)"
out_dir="${1:-${DEFRA_AGENT_DOCKER_INTEROP_OUT:-/tmp/defra-agent-adapter-interop-fixtures}}"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required for adapter projection interop fixture generation" >&2
  exit 127
fi

if [[ "${DEFRA_AGENT_DOCKER_INTEROP_KEEP:-0}" != "1" ]]; then
  rm -rf "${out_dir}"
fi
mkdir -p "${out_dir}"

run_generator() {
  local name="$1"
  local image="$2"
  local rel_dir="$3"
  shift 3

  echo "building ${name} fixture image"
  docker build -t "${image}" "${repo_root}/${rel_dir}"

  echo "running ${name} fixture generator"
  docker run --rm "$@" -v "${out_dir}:/out" "${image}"
}

langgraph_env=()
for var in \
  DEFRA_LANGGRAPH_PROVIDER_MODE \
  DEFRA_LANGGRAPH_OPENAI_MODEL \
  OPENAI_MODEL \
  OPENAI_BASE_URL \
  OPENAI_API_KEY
do
  if [[ -n "${!var+x}" ]]; then
    langgraph_env+=("-e" "${var}")
  fi
done

run_generator \
  "langgraph" \
  "defra-agent-langgraph-fixture" \
  "docs/superpowers/fixtures/adapter-projections/generators/langgraph" \
  "${langgraph_env[@]}"

run_generator \
  "autogen" \
  "defra-agent-autogen-fixture" \
  "docs/superpowers/fixtures/adapter-projections/generators/autogen"

run_generator \
  "crewai" \
  "defra-agent-crewai-fixture" \
  "docs/superpowers/fixtures/adapter-projections/generators/crewai"

run_generator \
  "microsoft-agent-framework" \
  "defra-agent-msaf-fixture" \
  "docs/superpowers/fixtures/adapter-projections/generators/microsoft-agent-framework"

fixture_count="$(find "${out_dir}" -type f -name '*.json' | wc -l | tr -d '[:space:]')"
if [[ "${fixture_count}" -lt 8 ]]; then
  echo "expected at least 8 generated adapter fixtures, found ${fixture_count}" >&2
  find "${out_dir}" -type f -name '*.json' -print | sort >&2
  exit 1
fi

echo "generated ${fixture_count} adapter fixture files in ${out_dir}"
find "${out_dir}" -type f -name '*.json' -print | sort

if [[ "${DEFRA_AGENT_DOCKER_INTEROP_SKIP_RUST:-0}" != "1" ]]; then
  echo "validating generated fixtures with Rust external adapter harness"
  (
    cd "${repo_root}"
    DEFRA_AGENT_ADAPTER_INTEROP_FIXTURES="${out_dir}" \
      cargo test -p defra-agent --test adapter_projection_external_fixtures -- --ignored --nocapture
  )

  export_dir="${out_dir}/defra-exports"
  rm -rf "${export_dir}"
  mkdir -p "${export_dir}"
  echo "round-tripping mapped native captures through embedded DefraDB and the defra-agent binary"
  (
    cd "${repo_root}"
    DEFRA_AGENT_ADAPTER_INTEROP_ROUNDTRIP_FIXTURES="${out_dir}" \
      DEFRA_AGENT_ADAPTER_INTEROP_EXPORTS="${export_dir}" \
      cargo test -p defra-agent-cli --test cli_adapter_interop_roundtrip -- --ignored --nocapture
  )

  echo "verifying AutoGen Defra exports inside the AutoGen fixture image"
  docker run --rm \
    --entrypoint python \
    -v "${out_dir}:/out" \
    -v "${export_dir}:/exports" \
    defra-agent-autogen-fixture \
    /fixture/verify_export.py --fixtures /out --exports /exports

  echo "verifying LangGraph Defra exports inside the LangGraph fixture image"
  docker run --rm \
    --entrypoint python \
    -v "${out_dir}:/out" \
    -v "${export_dir}:/exports" \
    defra-agent-langgraph-fixture \
    /fixture/verify_export.py --fixtures /out --exports /exports

  echo "verifying CrewAI Defra exports inside the CrewAI fixture image"
  docker run --rm \
    --entrypoint python \
    -v "${out_dir}:/out" \
    -v "${export_dir}:/exports" \
    defra-agent-crewai-fixture \
    /fixture/verify_export.py --fixtures /out --exports /exports

  echo "verifying Microsoft Agent Framework Defra exports inside the fixture image"
  docker run --rm \
    --entrypoint python \
    -v "${out_dir}:/out" \
    -v "${export_dir}:/exports" \
    defra-agent-msaf-fixture \
    /fixture/verify_export.py --fixtures /out --exports /exports
fi
