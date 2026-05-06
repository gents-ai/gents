# defra-agent Desktop

This is the Tauri 2 + React desktop shell for `defra-agent`.

It is intentionally a local-first client. The app pairs with a running
`defra-agent` runtime, consumes the replicated document surface through
`defra-agent-desktop-core`, and renders conversation, configuration, runtime,
and fleet views from that local store.

## Development

Prerequisites:

- Rust toolchain
- Bun
- a running or discoverable `defra-agent` runtime for live chat flows

Install frontend dependencies:

```bash
bun install
```

Run the frontend-only Vite app:

```bash
bun run dev
```

Run the full Tauri shell:

```bash
bun run tauri dev
```

Build the frontend:

```bash
bun run build
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

Frontend/unit tests:

```bash
bun run test
```

Live UI smoke tests:

```bash
bun run test:live
bun run test:live:chat
bun run test:live:config
```

Remote fleet smoke:

```bash
bun run smoke:remote-fleet
```

The live tests expect real runtime connectivity and should be treated as manual
or release validation, not the default fast correctness gate.
