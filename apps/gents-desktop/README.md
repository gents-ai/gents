# gents Desktop

This is the Tauri 2 + React desktop shell for `gents`.

It is intentionally a local-first client. The app pairs with a running
`gents` runtime, consumes the replicated document surface through
`gents-desktop-core`, and renders conversation, configuration, runtime,
and fleet views from that local store.

From the initial Fleet screen the desktop app can also provision and host the
standard `~/.gents` runtime in-process. This is opt-in; remote pairing remains
available. Once enabled, the app starts that runtime on later launches and
keeps it alive in the system tray when the main window closes. The CLI and
desktop host share the `gents_server::server_host` implementation, so no CLI
binary or sidecar is bundled.

The design for extracting this app's Tauri bridge and chat/fleet/operations
workflows into reusable packages (#877) lives in
[docs/reusable-desktop-packages.md](../../docs/reusable-desktop-packages.md).

## Development

Prerequisites:

- Rust toolchain
- Node.js 22+ and npm
- a running or discoverable `gents` runtime for live chat flows

Install frontend dependencies:

```bash
npm ci
```

Run the frontend-only Vite app:

```bash
npm run dev
```

Run the full Tauri shell:

```bash
npm run tauri -- dev
```

Build the frontend:

```bash
npm run build
```

Build the Tauri app:

```bash
npm run tauri -- build
```

From the repo root, the Makefile exposes the native app QA commands:

```bash
make desktop-native-preflight
make desktop-native-dev
make desktop-native-build
```

Build the desktop binary from the repo root:

```bash
cargo build -p gents-desktop --release
```

## Pairing

For a remote runtime, signed bearer pairing is the primary path. On the agent
host, mint a five-minute, single-use conversation invite:

```bash
gents p2p pairings invite --home ~/.gents/my-agent --bearer --qr
```

In Gents, choose **Add Agent**, then scan the QR code or paste the emitted
`dabear1-` token. The client verifies the issuer and network signatures,
publishes its own signed endpoint and claim, and installs filtered conversation
replication. The running agent burns the invite nonce and creates the reciprocal
replication leg.

Conversation pairing is requester-scoped: all eight transcript collections
replicate only rows whose immutable `requester_did` is the paired client DID.
Legacy rows created before requester lineage was recorded remain local by
design; pairing never widens the filter to import null-requester history.

The manual `/status` and connection-JSON form remains under **Advanced manual
discovery** for diagnostics and older runtimes. It does not perform the signed
membership exchange.

The desktop binary has an `init` subcommand that discovers or seeds a runtime
deployment before the GUI starts:

```bash
gents-desktop init
gents-desktop
```

To seed a remote runtime explicitly:

```bash
gents-desktop init --graphql http://agent-host:9181/api/v0/graphql
# or:
gents-desktop init --status-endpoint http://agent-host:9181/status
```

The saved deployment stores both GraphQL and P2P connection metadata. The app
finishes replication bootstrap after launch; chat views should wait for the
status bar to report `replication: subscriptions armed`.

For isolated demo or QA runs, set `GENTS_DESKTOP_HOME` before launching
the Tauri app. The bootstrap summary, peer directory, embedded desktop node,
and logs all resolve under that directory:

```bash
GENTS_DESKTOP_HOME=/tmp/gents-desktop-demo/desktop npm run tauri -- dev
```

From a release binary, `gents demo` drives the same fleet: `pair` brings up
two runtimes (**Orchestrator** and **Worker**) with a tightened tool surface (no
`defra_query`), `delegate` lets the Orchestrator delegate to the Worker on node B
via a cross-node subagent (the child runs on the Worker and its result replicates
back), and `desktop` seeds that isolated desktop home and opens the Fleet
Dashboard. Live chat needs a real backend reachable on both nodes — keep
`llama-server` running on `http://127.0.0.1:8080/v1`, or use a hosted preset
(`gents demo --desktop --backend-preset openai --model gpt-5.4-mini`, with
`OPENAI_API_KEY` in the environment).

## Tests

The deterministic desktop UI gate is layered so failures point at the right
surface:

```bash
npm run test:ui
```

That command checks formatting, builds the frontend, runs Vitest component/model
tests, runs Playwright browser journeys, and finishes with a short Bombadil
smoke run.

Individual layers:

```bash
npm run test:ui:unit
npm run test:ui:e2e
npm run test:ui:invariants
npm run test:ui:screenshots
npm run test:ui:fuzz -- --time-limit 30s
npm run test:ui:fuzz:long
npm run test:ui:ios:e2e
npm run test:ui:native:preflight
```

The Playwright suite serves `tests/ui-harness/harness.html` with Vite and renders
the real React shell against a deterministic in-memory `DesktopApiAdapter`. It
covers fleet, chat, config, operations, interrupt, sad-path, and responsive
journeys across desktop, laptop, and narrow viewports.

Bombadil uses the same harness and checks persistent invariants while exploring
the UI:

```bash
npm run test:ui:fuzz
npm run test:ui:fuzz -- --time-limit 2m
```

Codex and other LLM agents can drive a persistent browser session through the
JSONL agent protocol:

```bash
npm run test:ui:agent -- --backend deterministic --viewport iphone
npm run test:ui:agent -- --backend live --viewport iphone
```

The live form boots the real Rust bridge, embedded DefraDB runtime, and a local
mock inference endpoint. See
[tests/AGENT_BROWSER.md](./tests/AGENT_BROWSER.md) for commands and boundaries.

The iOS lane builds and installs the real app in an iPhone Simulator, pairs
with the isolated local `~/.gents/iphone-e2e` node through a fresh signed bearer
invite, and verifies an actual prompt/response round-trip. It never targets a
deployed agent unless the issuer environment variables are set explicitly. It
uses a debug-only in-app driver so the same React and Tauri command paths run
inside the native `WKWebView`, detects an unexpected app exit, and retains
screenshots from the sent and terminal stages:

```bash
npm run test:ui:ios:e2e
npm run test:ui:ios:e2e -- --runs=3
```

Artifacts are written under `test-results/` and Playwright's HTML report under
`playwright-report/`. Playwright failures include screenshots, videos, traces,
and browser console logs when the browser emitted errors. Stable screenshot
captures are diagnostic review artifacts, not visual golden snapshots.

Useful artifact commands:

```bash
npx playwright show-report
npx playwright show-trace test-results/playwright/<failed-test>/trace.zip
```

Use screenshots for quick visual triage, then use traces for the full timeline:
DOM snapshots, clicked elements, network/console signals, and the exact failed
assertion.

Live UI smoke tests:

```bash
npm run test:live
npm run test:live:chat
npm run test:live:config
npm run test:live:operations
npm run test:live:interrupt
npm run test:ui:live:e2e
npm run test:ui:live:e2e:real -- --inference-url <url> --model-name <model>
```

Remote fleet smoke:

```bash
npm run smoke:remote-fleet
```

The live tests expect real runtime connectivity and should be treated as manual
or release validation, not the default fast correctness gate. The browser live
smoke uses mock inference by default; use `test:ui:live:e2e:real` when a real
provider must be exercised and mock fallback would hide a staging problem.
The `Live Smoke` workflow uploads the browser smoke summary, request diagnostics,
and final screenshot even when the job passes so successful staging runs can be
reviewed later.

Native macOS/Tauri smoke coverage is intentionally smaller than the browser
matrix. See [tests/NATIVE_QA.md](./tests/NATIVE_QA.md) for the app-window,
WebView, menu/window chrome, runtime handoff, and clean-quit checklist.
