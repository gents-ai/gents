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
defra-agent-desktop init --graphql http://agent-host:9191/api/v0/graphql
# or:
defra-agent-desktop init --status-endpoint http://agent-host:9191/status
```

The discovery URL is used to read connection metadata; the saved deployment
stores the runtime's P2P address and GraphQL endpoint. The desktop app
completes the P2P pairing and replication bootstrap when it launches. For the
replicated chat demo, leave the desktop app open and wait for the status bar
to show `replication: subscriptions armed` before sending prompts you expect
to render in the UI.

If you isolated the agent home, pass the same path as
`--agent-home /some/path` to `defra-agent-desktop init`.

For an isolated desktop data directory — useful for demos and QA runs — set
`DEFRA_AGENT_DESKTOP_HOME` before launching the Tauri app:

```bash
DEFRA_AGENT_DESKTOP_HOME=/tmp/defra-agent-desktop-demo/desktop \
  npm --prefix apps/desktop-tauri run tauri -- dev
```

The launcher, bootstrap summary, logs, peer directory, and embedded desktop
node all use that directory when the variable is set.

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

For the scripted local version, run:

```bash
make demo-p2p-two-node
```

To run the same two-node substrate and open it in the native desktop fleet UI:

```bash
make demo-desktop-two-node
```

The desktop demo needs a real OpenAI-compatible backend reachable on both
nodes — e.g. a local inference server at `http://127.0.0.1:8080/v1`, matching the
getting-started
`llama-server` command:

```bash
llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf
```

For hosted inference, select a preset and a provider model before launching:

```bash
DEFRA_AGENT_DEMO_BACKEND_PRESET=openai \
DEFRA_AGENT_DEMO_MODEL=gpt-4.1-mini \
OPENAI_API_KEY=... \
  make demo-desktop-two-node
```

That command:

- starts two runtimes, **Orchestrator** and **Worker**, through the two-node
  P2P script;
- tightens each agent's tool surface — drops the `defra_query` tool — and
  enables the Orchestrator to delegate to the **Worker on node B** via a
  cross-node subagent (it prints the resolved surface via
  `defra-agent tools explain`);
- seeds an isolated desktop peer directory with both runtimes;
- launches the Tauri dev app with `DEFRA_AGENT_DESKTOP_HOME` pointed at the
  demo desktop home.

In the app, open **Fleet Dashboard** to see both deployments. Open Chat for the
**Orchestrator** and ask it to use its worker subagent: it calls
`spawn_subagent` (background await), the runtime materializes a child request
**on the Worker node** (`agent_did` = the Worker), the Worker runs it, and its
result replicates back to the Orchestrator over P2P. Open the Worker's Chat to
watch the delegated child run there. Cross-node delegation uses background await
(foreground remote spawns are rejected) and is enabled for the trusted local
loopback fleet via `subagent_allow_cross_deployment` on both runtimes. Set
`DEFRA_AGENT_DESKTOP_DEMO_LAUNCH=0` to prepare
the demo without launching the app, or `DEFRA_AGENT_DESKTOP_DEMO_KEEP=1` to keep
runtimes and data after exit. By default the launcher refuses to open the GUI if
the local model backend is unreachable; set
`DEFRA_AGENT_DESKTOP_DEMO_ALLOW_UNAVAILABLE_BACKEND=1` only when you want to
inspect the seeded fleet UI without sending live chat turns:

```bash
DEFRA_AGENT_DESKTOP_DEMO_ALLOW_UNAVAILABLE_BACKEND=1 \
  make demo-desktop-two-node
```

To bring up two runtimes manually and enroll Coding into Amy's signed network:

```bash
defra-agent init --home /tmp/amy --agent-name amy
defra-agent init --home /tmp/coding --agent-name coding

defra-agent server --home /tmp/amy --no-codex-shim \
  --p2p-bind-addr 127.0.0.1 --p2p-port 4017 \
  --p2p-relay-mode disabled --p2p-discovery disabled
defra-agent server --home /tmp/coding --no-codex-shim \
  --p2p-bind-addr 127.0.0.1 --p2p-port 4018 \
  --p2p-relay-mode disabled --p2p-discovery disabled
```

Create the network root on Amy, grant Coding's DID, then join Coding with a
signed `network-control` invite:

```bash
CODING_DID=$(jq -r .agent_did /tmp/coding/init.json)

defra-agent p2p network create --home /tmp/amy --name "Two Node Demo"
defra-agent p2p network grant --home /tmp/amy "$CODING_DID"

AMY_INVITE=$(
  defra-agent p2p pairings invite \
    --home /tmp/amy \
    --member-did "$CODING_DID" \
    --template network-control \
    | jq -r .token
)

defra-agent p2p pairings join --home /tmp/coding "$AMY_INVITE"
```

The join wires the narrow control-plane substrate only. To move chat,
subagent, and trace rows, add `DataPlanePairingDesired` rows for the
conversation edge. The scripted demo writes those rows and then proves
replication by submitting a no-wait request on Coding and reading it from Amy.

Inspect desired pairing rows and live connectivity from either runtime:

```bash
defra-agent p2p pairings list --home /tmp/amy --output table
defra-agent p2p status --home /tmp/amy
defra-agent p2p peers --home /tmp/coding
defra-agent status --home /tmp/coding
```

The most useful fields for bring-up are `p2p_transport`, `p2p_peer_id`,
`p2p_listen_addresses`, `p2p_connected_peers`, and the `CONNECTED`,
`SUBSCRIBED`, and `REPLICATING` columns in `p2p pairings list`.

For the narrated, end-to-end version of this flow — what each document means
and how to watch the runtime reconcile it — see [Part 2 of the getting-started
walkthrough](demo.md#part-2--pair-a-second-node).

The low-level `p2p admin` commands (`connect`, `collections`, `replicators`,
`documents`) remain available for diagnostics and repair. They mutate live
P2P state directly; normal pairing should go through `p2p pairings`
(invite/join or `pairings set`).

## Scope templates

Scope templates are named pairing intents that bundle a fixed collection set, a
per-peer scoping policy (agent_did equality or unscoped), and a delivery mode
(push or replicate). Use `--template` on `p2p pairings invite`, `join`, and
`pairings set` instead of hand-authoring collection lists.

```bash
defra-agent p2p templates list          # print the built-in catalog
```

Built-in templates:

| Template | Collections | Scope | Delivery |
|---|---|---|---|
| `conversation` (default) | Requests, responses, messages, tool calls/results, sessions, conversations, compaction | `agent_did` equality | Push |
| `agent-config` | Behaviors, tool selections, backends, profiles, tool services, skills | Unscoped | Replicate |
| `backup` | Same collection set as `conversation` | Unscoped (all docs) | Replicate |
| `discovery` | Network membership + agent config bootstrap docs | Unscoped | Replicate |
| `network-control` | Network root, membership, endpoints, join requests | Unscoped | Replicate |

Use `network-control` for signed fleet enrollment and `conversation` for
application data-plane rows:

```bash
AMY_INVITE=$(
  defra-agent p2p pairings invite \
    --member-did "$CODING_DID" \
    --template network-control \
    | jq -r .token
)
defra-agent p2p pairings join --home /tmp/coding "$AMY_INVITE"
# join reads the template from the token; pass --template only to override
```

## Admin filtered replication

The low-level `p2p admin replicators add` command accepts `--filter` to express
per-collection field-equality predicates (repeatable). These are parsed into
`PairingFilters` and echoed in the command output. Full forwarding to the
DefraDB filtered-replication endpoint is pending defradb.rs #1033.

```bash
defra-agent p2p admin replicators add \
  --home /tmp/coding \
  --peer iroh://peer-id \
  --collection AgentRequest \
  --filter "AgentRequest:agent_did=did:key:alice" \
  --filter "AgentResponse:agent_did=did:key:alice"
```

Format: `<Collection>:<field>=<value>`. Parse errors (missing `:` or `=`,
empty component) are hard failures with a clear message.

## Service discovery and signed networks

There are two related network surfaces:

- `p2p network create|grant|revoke` writes the admin-signed
  `AgentNetwork`/`NetworkMembership` control plane used by v5 invite/join.
- `p2p network register|list|rm` writes and reads the `PeerRegistry`
  discovery view: display names, offered templates, heartbeat freshness, and
  pairing diagnostics.

```bash
defra-agent p2p network create --home /tmp/amy --name "Fleet One"
defra-agent p2p network grant --home /tmp/amy "$CODING_DID"

defra-agent p2p network register --home /tmp/amy --template conversation   # self-register in discovery
defra-agent p2p network list --home /tmp/coding --output table              # discovered members + liveness + paired/auto-pair
defra-agent p2p network rm --home /tmp/amy                                  # deregister this node's row
```

Auto-pairing of discovered members is **off by default**. Set the
`DEFRA_AGENT_DISCOVERY_AUTO_PAIR=1` environment variable on `server` to have
the discovery reconciler materialize registry-owned `PeerPairingDesired` rows
(`source: "registry"`) for live members; with it unset, `network list` shows
discovered peers and you pair explicitly. Registry-owned rows are retracted
when their entry stales/removed and never touch operator-authored pairings.

For the narrated walkthrough — the network-control and conversation data-plane
layers — see [Part 3 of the getting-started
walkthrough](demo.md#part-3--grow-the-link-into-a-fleet).

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
