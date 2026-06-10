# Operations

Operational reference beyond the [getting-started walkthrough](demo.md):
desktop pairing, multi-runtime bring-up, and the operations API.

## Desktop app

Build and install the desktop binaries:

```bash
scripts/install-local.sh
```

Pair and launch (with a `defra-agent server` already running):

```bash
defra-agent-desktop init
defra-agent-desktop
```

`defra-agent-desktop init` discovers a runtime and saves it in the desktop
peer directory. To seed a remote or deployed runtime from its operations API,
pass its GraphQL or status endpoint:

```bash
defra-agent-desktop init --graphql http://agent-host:9181/api/v0/graphql
# or:
defra-agent-desktop init --status-endpoint http://agent-host:9181/status
```

The discovery URL is used to read connection metadata; the saved deployment
stores the runtime's P2P address and GraphQL endpoint. The desktop app
completes the P2P pairing and replication bootstrap when it launches. For the
replicated chat demo, leave the desktop app open and wait for the status bar
to show `replication: subscriptions armed` before sending prompts you expect
to render in the UI.

If you isolated the agent home, pass the same path as
`--agent-home /some/path` to `defra-agent-desktop init`.

## P2P defaults and pinning

The standard server path always starts the IROH P2P transport for local
desktop pairing. It binds to localhost on an ephemeral P2P port by default,
with relay and discovery disabled.

To pin the local P2P socket:

```bash
defra-agent server \
  --p2p-bind-addr 127.0.0.1 \
  --p2p-port 4017 \
  --p2p-relay-mode disabled \
  --p2p-discovery disabled
```

## Connected runtime bring-up

To bring up two runtimes and connect them through the operator CLI:

```bash
defra-agent server --home /tmp/amy --p2p-bind-addr 127.0.0.1 --p2p-port 4017
defra-agent server --home /tmp/coding --p2p-bind-addr 127.0.0.1 --p2p-port 4018
```

Read Amy's startup JSON or `/tmp/amy/runtime.json` and take one of the values
from `p2p_listen_addresses`. Then connect Coding to Amy:

```bash
defra-agent p2p connect --home /tmp/coding --peer "<peer-id-or-listen-address>"
```

Inspect connectivity from either runtime:

```bash
defra-agent p2p status --home /tmp/amy
defra-agent p2p peers --home /tmp/coding
defra-agent status --home /tmp/coding
```

The most useful fields for bring-up are `p2p_transport`, `p2p_peer_id`,
`p2p_listen_addresses`, and `p2p_connected_peers`.

## Remote Codex clients

The Codex endpoint binds loopback by default and has no transport
authentication. To drive a runtime from another machine, bind it to a trusted
private or Tailscale IP and point the client at it:

```bash
defra-agent server --codex-shim-bind-addr <trusted-private-or-tailscale-ip>
defra-agent codex --remote ws://<that-host>:9292/
```

Never bind it to an unspecified address; the server refuses `0.0.0.0`.

## Operations API

The runtime HTTP port (default 9191) exposes a stable machine-readable
surface:

- `GET /version` — build and package metadata for the running binary
- `GET /healthz` — JSON process/runtime health; HTTP 200 when serving
  (including degraded-but-running), 503 when no runtime is ready
- `GET /status` — detailed runtime/backend/P2P status
- `GET /metrics` — Prometheus metrics for runtime state, backend health, and
  admission settings
- `GET /self`, `/sessions`, `/fleet`, `/fleet/slots`, `/mcp/pool`,
  `/subagents/dispatches`, `/subagents/tree` — runtime introspection

Connectivity fields are also persisted to `runtime.json` under the agent home
(usually `~/.defra-agent/runtime.json`). `defra-agent reset` removes it.

## Live updates to tool selections

To replace the default tool selection on a running agent, update documents
directly. Read `agent_did` and `graphql` from the `server` startup JSON.

```bash
export GRAPHQL=http://127.0.0.1:9191/api/v0/graphql
export AGENT_DID="did:key:..."
export TOOL_SELECTION_ID="${AGENT_DID}:default-tools"

defra-agent config tools set \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --selection-id "$TOOL_SELECTION_ID" \
  --enable-file-tools

defra-agent config behavior set \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --tool-selection-id "$TOOL_SELECTION_ID" \
  --display-name "Demo"
```

The change is live; no restart is required.
