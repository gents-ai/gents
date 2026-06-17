# defra-agent Desktop

This is the Tauri 2 + React desktop shell for `defra-agent`.

It is intentionally a local-first client. The app pairs with a running
`defra-agent` runtime, consumes the replicated document surface through
`defra-agent-desktop-core`, and renders conversation, configuration, runtime,
and fleet views from that local store.

## Development

Prerequisites:

- Rust toolchain
- Node.js 22+ and npm
- a running or discoverable `defra-agent` runtime for live chat flows

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
npm run tauri dev
```

Build the frontend:

```bash
npm run build
```

Build the desktop binary from the repo root:

```bash
cargo build -p defra-agent-desktop --release
```

## Pairing

The desktop binary has an `init` subcommand that discovers or seeds a runtime
deployment before the GUI starts:

```bash
defra-agent-desktop init
defra-agent-desktop
```

To seed a remote runtime explicitly:

```bash
defra-agent-desktop init --graphql http://agent-host:9181/api/v0/graphql
# or:
defra-agent-desktop init --status-endpoint http://agent-host:9181/status
```

The saved deployment stores both GraphQL and P2P connection metadata. The app
finishes replication bootstrap after launch; chat views should wait for the
status bar to report `replication: subscriptions armed`.

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
npm run test:ui:fuzz -- --time-limit 30s
npm run test:ui:fuzz:long
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

Artifacts are written under `test-results/` and Playwright's HTML report under
`playwright-report/`.

Live UI smoke tests:

```bash
npm run test:live
npm run test:live:chat
npm run test:live:config
npm run test:live:operations
npm run test:live:interrupt
```

Remote fleet smoke:

```bash
npm run smoke:remote-fleet
```

The live tests expect real runtime connectivity and should be treated as manual
or release validation, not the default fast correctness gate.
