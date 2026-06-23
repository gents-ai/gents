# Desktop UI test layers

See [QA.md](./QA.md) for the desktop UI QA sweep, artifact review, and bug
issue workflow. See [NATIVE_QA.md](./NATIVE_QA.md) for the native macOS/Tauri
app smoke layer.

The desktop test stack has three layers:

- `npm run test:ui:unit` runs Vitest and Testing Library against component and
  model code.
- `npm run test:ui:e2e` runs Playwright in Chromium against
  `tests/ui-harness/harness.html`. By default this uses the deterministic
  in-memory adapter, so it is the fast browser regression gate.
- `npm run test:ui:fuzz` runs Bombadil against the same deterministic browser
  harness and checks persistent shell invariants under random interaction.
- `npm run test:ui:qa-sweep` runs the fuller manual QA sweep.
- `npm run test:ui:visual` runs golden screenshot checks for stable shell states.
- `npm run test:ui:live:e2e` runs the live browser-to-runtime smoke path through
  `bridge_runner`. It uses a local OpenAI-compatible mock inference endpoint by
  default; pass `-- --inference-url <url> --model-name <model>` or set the live
  backend env vars to exercise a real provider.
- `npm run test:ui:live:e2e:real` runs the same live browser-to-runtime smoke
  path, but refuses to fall back to the mock endpoint. Use this when validating
  a real inference provider or the `live-smoke.yml` manual workflow inputs.
- `npm run test:ui:native:preflight` runs the non-GUI native Tauri preflight.

The browser harness also has an explicit live-backend seam:

```text
/tests/ui-harness/harness.html?backend=live&bridgeUrl=<bridge-runner-url>
```

That mode swaps the deterministic adapter for the bridge-runner HTTP adapter.
The live Playwright project starts `LiveBridgeRunner`, passes its `baseUrl` as
`bridgeUrl`, and stays out of the fast PR-gating browser job until it is stable.

The existing `test:live:*` suites remain the lower-level live bridge/runtime
coverage until the live browser project reaches parity.

The `Desktop UI QA Sweep` GitHub workflow is the artifact-producing review
loop for the deterministic browser layer. It can be run manually or by its
weekday schedule and uploads screenshots, visual reports, Playwright traces, and
Bombadil output for review even on successful runs.

Real-provider live browser examples:

```bash
OPENAI_API_KEY=... npm run test:ui:live:e2e:real -- \
  --inference-url https://api.openai.com/v1 \
  --model-name gpt-4.1-mini \
  --api-key-env-var OPENAI_API_KEY

DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL=http://workstation-1:8000/v1 \
DEFRA_AGENT_TAURI_LIVE_MODEL_NAME=MiniMax-M2.7-NVFP4 \
npm run test:ui:live:e2e:real
```

The GitHub `Live Smoke` workflow exposes `inference_endpoint` and `model_name`
manual inputs and runs the live browser job with those values.
