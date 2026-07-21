#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../../../.." && pwd)"
out_dir="${1:-${GENTS_DOCKER_INTEROP_OUT:-/tmp/gents-adapter-interop-fixtures}}"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker is required for adapter projection interop fixture generation" >&2
  exit 127
fi

if [[ "${GENTS_DOCKER_INTEROP_KEEP:-0}" != "1" ]]; then
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
  GENTS_LANGGRAPH_PROVIDER_MODE \
  GENTS_LANGGRAPH_OPENAI_MODEL \
  OPENAI_MODEL \
  OPENAI_BASE_URL \
  OPENAI_API_KEY
do
  if [[ -n "${!var+x}" ]]; then
    langgraph_env+=("-e" "${var}")
  fi
done

expected_fixtures=(
  "langgraph_state_history.capture.json"
  "langgraph_state_history.provider.capture.json"
  "langgraph_state_history.subgraph.capture.json"
  "multi_agent_task.autogen.capture.json"
  "multi_agent_task.autogen_swarm.capture.json"
  "multi_agent_task.crewai_hierarchical.capture.json"
  "multi_agent_task.crewai_sequential.capture.json"
  "multi_agent_task.microsoft_agent_framework_group_chat.capture.json"
)

run_generator \
  "langgraph" \
  "gents-langgraph-fixture" \
  "scripts/adapter-interop/generators/langgraph" \
  "${langgraph_env[@]}"

run_generator \
  "autogen" \
  "gents-autogen-fixture" \
  "scripts/adapter-interop/generators/autogen"

run_generator \
  "crewai" \
  "gents-crewai-fixture" \
  "scripts/adapter-interop/generators/crewai"

run_generator \
  "microsoft-agent-framework" \
  "gents-msaf-fixture" \
  "scripts/adapter-interop/generators/microsoft-agent-framework"

fixture_count="$(find "${out_dir}" -maxdepth 1 -type f -name '*.json' | wc -l | tr -d '[:space:]')"
if [[ "${fixture_count}" -ne "${#expected_fixtures[@]}" ]]; then
  echo "expected exactly ${#expected_fixtures[@]} generated adapter fixtures, found ${fixture_count}" >&2
  find "${out_dir}" -maxdepth 1 -type f -name '*.json' -print | sort >&2
  exit 1
fi
for fixture in "${expected_fixtures[@]}"; do
  if [[ ! -f "${out_dir}/${fixture}" ]]; then
    echo "missing expected adapter fixture ${fixture}" >&2
    exit 1
  fi
done

echo "generated ${fixture_count} adapter fixture files in ${out_dir}"
find "${out_dir}" -maxdepth 1 -type f -name '*.json' -print | sort

if [[ "${GENTS_DOCKER_INTEROP_SKIP_RUST:-0}" != "1" ]]; then
  echo "validating generated fixtures with Rust external adapter harness"
  (
    cd "${repo_root}"
    GENTS_ADAPTER_INTEROP_FIXTURES="${out_dir}" \
      cargo test -p gents --test e2e_runtime adapter_projection_external_fixtures -- --ignored --nocapture
  )

  export_dir="${out_dir}/gents-exports"
  rm -rf "${export_dir}"
  mkdir -p "${export_dir}"
  echo "round-tripping mapped native captures through embedded DefraDB and the gents binary"
  (
    cd "${repo_root}"
    GENTS_ADAPTER_INTEROP_ROUNDTRIP_FIXTURES="${out_dir}" \
      GENTS_ADAPTER_INTEROP_EXPORTS="${export_dir}" \
      cargo test -p gents-cli --test cli_adapter_interop_roundtrip -- --ignored --nocapture
  )
  export_count="$(find "${export_dir}" -type f \( -name '*.gents.json' -o -name '*.gents.jsonl' -o -name '*.gents.eval-jsonl' \) | wc -l | tr -d '[:space:]')"
  expected_export_count="$((${#expected_fixtures[@]} * 3))"
  if [[ "${export_count}" -ne "${expected_export_count}" ]]; then
    echo "expected exactly ${expected_export_count} Gents export files, found ${export_count}" >&2
    find "${export_dir}" -type f -print | sort >&2
    exit 1
  fi

  echo "verifying AutoGen Gents exports inside the AutoGen fixture image"
  docker run --rm \
    --entrypoint python \
    -v "${out_dir}:/out" \
    -v "${export_dir}:/exports" \
    gents-autogen-fixture \
    /fixture/verify_export.py --fixtures /out --exports /exports

  echo "verifying LangGraph Gents exports inside the LangGraph fixture image"
  docker run --rm \
    --entrypoint python \
    -v "${out_dir}:/out" \
    -v "${export_dir}:/exports" \
    gents-langgraph-fixture \
    /fixture/verify_export.py --fixtures /out --exports /exports

  echo "verifying CrewAI Gents exports inside the CrewAI fixture image"
  docker run --rm \
    --entrypoint python \
    -v "${out_dir}:/out" \
    -v "${export_dir}:/exports" \
    gents-crewai-fixture \
    /fixture/verify_export.py --fixtures /out --exports /exports

  echo "verifying Microsoft Agent Framework Gents exports inside the fixture image"
  docker run --rm \
    --entrypoint python \
    -v "${out_dir}:/out" \
    -v "${export_dir}:/exports" \
    gents-msaf-fixture \
    /fixture/verify_export.py --fixtures /out --exports /exports
fi
