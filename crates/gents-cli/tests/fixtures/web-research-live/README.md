# Live web-research acceptance fixture

This fixture has no mock mode. It starts the released public MCP gateway plus
real SearXNG and the complete self-hosted Firecrawl dependency stack. The test
then starts a fresh Gents node, registers that MCP service, installs the bundled
`web-deep-research` graph, and runs the graph against a real model.

Firecrawl and Playwright reserve roughly 12 GB; allow 14–16 GB for the complete
stack. Run through the wrapper so failed containers are logged and all volumes
are cleaned up:

```bash
GENTS_CLI_E2E_MODEL_ENDPOINT=https://example.invalid/v1 \
GENTS_CLI_E2E_MODEL_NAME=real-model \
GENTS_CLI_E2E_API_KEY=... \
scripts/web-research-live-e2e.sh
```

Acceptance requires a successful four-stage graph, at least nine completed
searches, eight completed extractions, three quote verifications, ten model
calls, 20,000 reported-or-context-estimated tokens, a populated evidence and
verdict ledger, and one substantive cited report.
