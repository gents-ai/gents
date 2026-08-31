# Agent browser

`agent-browser.mjs` gives Codex and other LLM agents a persistent browser
session over a small JSONL protocol. The agent can inspect the accessibility
tree, choose semantic controls, click and type, observe browser errors, and
capture screenshots without writing a one-off Playwright test for every
exploration.

## Modes

Deterministic mode renders the real React application against the in-memory
desktop adapter:

```bash
npm run test:ui:agent -- --backend deterministic --scenario default --viewport iphone
```

Live mode renders the same application against `bridge_runner`. It starts the
real Rust bridge, embedded DefraDB nodes, replication fixture, and a local
OpenAI-compatible mock inference endpoint:

```bash
npm run test:ui:agent -- --backend live --viewport iphone
```

Use `--headed` when a person also wants to watch. By default the viewport is
390×844 with touch and mobile browser semantics. `laptop`, `desktop`, and exact
sizes such as `430x932` are also accepted.

Live mode uses no provider credentials by default. To opt into a real provider,
pass `--inference-url`, `--model-name`, `--provider`, and
`--api-key-env-var`. Never put an API key directly in a JSON command or source
file.

## Protocol

The driver prints one `ready` record to stdout. It then reads one JSON object
per stdin line and emits one response with the same `id`. Logs and startup
diagnostics go to stderr, so stdout remains machine-readable.

Start by asking what is visible:

```json
{"id":"look","command":"snapshot"}
{"id":"controls","command":"inspect"}
```

Targets use exactly one semantic strategy:

```json
{"testId":"composer-input"}
{"role":"button","name":"Send"}
{"label":"Server address"}
{"placeholder":"Ask this agent anything"}
{"text":"Configure","exact":true}
{"css":".chat-workspace"}
```

When a locator has intentional duplicates, add `"index":0`. Role, label,
placeholder, and text matching are exact by default; use `"exact":false` only
when the visible name is expected to vary.

Common commands:

```json
{"id":"open-chat","command":"click","target":{"testId":"fleet-chat-name-peer-bombadil-local"}}
{"id":"prompt","command":"fill","target":{"testId":"composer-input"},"value":"Hello agent"}
{"id":"send","command":"click","target":{"role":"button","name":"Send"}}
{"id":"answer","command":"wait","target":{"text":"Fleet E2E live agent-browser confirmation."},"timeoutMs":60000}
{"id":"shot","command":"screenshot","name":"fleet-agent-chat"}
{"id":"errors","command":"console"}
{"id":"done","command":"close"}
```

Supported commands are:

- `snapshot`: URL, viewport, visible text, accessibility snapshot, and browser
  errors.
- `inspect`: visible interactive controls with roles, names, test IDs, state,
  and non-secret values.
- `click`, `fill`, `press`, `select`, `check`, `uncheck`: semantic actions.
- `wait`: wait for a target state, or pass `ms` for a short delay.
- `text`: read one target's visible text.
- `screenshot`: write a PNG under `test-results/agent-browser/`.
- `console`: read captured console and page-error events.
- `reload`, `back`, `viewport`: browser navigation and sizing.
- `goto`: switch deterministic fixture scenarios without restarting.
- `close`: close Chromium and cleanly stop Vite, the Rust fixture, and mock
  inference.

Failed actions automatically capture a screenshot and return the current URL
and browser errors. Command waits are capped at 60 seconds so an agent can
remain responsive while polling long-running work.

## What this proves

Deterministic mode is a fast UI contract and state-machine exploration layer.
Live mode additionally covers the React application, HTTP adapter, Rust Tauri
bridge command implementation, embedded DefraDB storage, replication fixture,
request lifecycle, and provider-shaped streaming response.

Chromium still stands in for the iOS `WKWebView`. Use
`npm run test:ui:ios:e2e` for the native lane: a debug-only in-app driver takes
the real app through clean-install authenticated enrollment, chat creation, prompt submission,
and the fleet agent's replicated response. That lane covers WebKit and native
lifecycle behavior, detects an unexpected app exit, and retains screenshots.
Physical-device networking remains the final release check.
