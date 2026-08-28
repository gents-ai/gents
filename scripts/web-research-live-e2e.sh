#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture_dir="${repo_dir}/crates/gents-cli/tests/fixtures/web-research-live"
compose=(docker compose --project-directory "${fixture_dir}" -f "${fixture_dir}/compose.yaml")

: "${GENTS_CLI_E2E_MODEL_ENDPOINT:?set GENTS_CLI_E2E_MODEL_ENDPOINT to a real OpenAI-compatible endpoint}"
: "${GENTS_CLI_E2E_MODEL_NAME:?set GENTS_CLI_E2E_MODEL_NAME to the real model name}"

cleanup() {
  exit_code=$?
  trap - EXIT INT TERM
  if [[ ${exit_code} -ne 0 ]]; then
    "${compose[@]}" ps || true
    "${compose[@]}" logs --no-color --tail 300 || true
  fi
  "${compose[@]}" down --volumes --remove-orphans || true
  exit "${exit_code}"
}
trap cleanup EXIT INT TERM

if [[ -n "${WEB_RESEARCH_MCP_IMAGE:-}" ]]; then
  "${compose[@]}" up --detach --pull never
else
  "${compose[@]}" up --detach --pull always
fi

gateway_ready=0
for _ in $(seq 1 90); do
  if curl --fail --silent --show-error http://127.0.0.1:19213/healthz >/dev/null \
    && "${compose[@]}" exec -T gateway /usr/local/bin/service smoke --backend searxng \
    && "${compose[@]}" exec -T gateway /usr/local/bin/service smoke --backend firecrawl; then
    gateway_ready=1
    break
  fi
  sleep 10
done

if [[ ${gateway_ready} -ne 1 ]]; then
  echo "real SearXNG + Firecrawl gateway did not pass live smoke checks" >&2
  exit 1
fi

export GENTS_WEB_RESEARCH_MCP_ENDPOINT=http://127.0.0.1:19213/mcp

cd "${repo_dir}"
cargo test -p gents-cli --features live-e2e --test cli_live_suite \
  cli_web_research_live::full_stack_web_deep_research_consumes_real_search_and_inference \
  -- --ignored --nocapture --test-threads=1
