# Getting started on a Mac

One story, end to end: local inference on your Mac driving a native Defra
agent that can inspect — and, when you allow it, change — your computer,
through the Codex terminal UI. No accounts, no API keys, nothing leaves your
machine.

```bash
brew install llama.cpp
llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf

cargo install --profile dev-install --locked --path crates/defra-agent-cli

defra-agent init
defra-agent server
defra-agent codex
```

The rest of this document walks those six commands, then the permission
presets, then the paths off the happy path. Desktop app, fleet bring-up, and
P2P pairing live in [operations.md](operations.md).

## Prerequisites

- macOS on Apple silicon
- [Homebrew](https://brew.sh)
- A Rust toolchain (`rustup`), for `cargo install`

## 1. Start local inference

```bash
brew install llama.cpp
llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf
```

The first run downloads the model (~7 GB) from Hugging Face, then serves an
OpenAI-compatible API on `http://127.0.0.1:8080/v1`. Leave it running. Gemma 4
12B QAT is the demo default because it is small enough for a 16 GB machine
and strong enough to drive tools; any model llama-server can load works the
same way.

Check it:

```bash
curl -s http://127.0.0.1:8080/v1/models | head -c 200
```

## 2. Install defra-agent

```bash
cargo install --profile dev-install --locked --path crates/defra-agent-cli
```

This installs the `defra-agent` binary into `~/.cargo/bin`.

## 3. Initialize the agent

```bash
defra-agent init
```

This is idempotent. It provisions a safe home directory under
`~/.defra-agent`: a DID-keyed principal, a backend document pointing at
`http://127.0.0.1:8080/v1`, a default behavior, and a **read-only** tool
selection — file and bash tools that can inspect but not change anything.

The tool root — the directory the agent can see — defaults to wherever you
ran `init`. Pass `--tool-root /path` for a different scope. The output is
JSON; the fields you will use are `agent_did` and `next_steps`.

### Letting it write

Two presets, both explicit opt-ins:

```bash
defra-agent init --write   # sandboxed writes, scoped to the tool root
defra-agent init --yolo    # unrestricted: full host access as your user
```

What they actually guarantee is in [Permission presets](#permission-presets)
below.

## 4. Start the runtime

In terminal 1:

```bash
defra-agent server
```

This starts the embedded DefraDB node, the GraphQL API, P2P transport for
desktop pairing, and the Codex endpoint on `ws://127.0.0.1:9292/` (loopback
only). It prints readiness JSON and stays in the foreground; stop it with
`Ctrl-C`. For debug output, set `RUST_LOG=info`.

## 5. Chat

In terminal 2:

```bash
defra-agent codex
```

This opens the Codex terminal UI — running embedded inside `defra-agent`, so
there is nothing to install or configure — connected to your local runtime.
Ask it something about the directory you initialized in:

> what is in this directory? summarize the project.

You'll see tool calls stream as the agent reads files and runs read-only
commands. If you initialized with `--write`, try asking it to create or edit
a file under the tool root.

Codex-side approvals and sandboxing are intentionally bypassed: every tool
call executes inside the Defra runtime, where the preset you chose at `init`
is enforced. The TUI is a window, not a boundary.

Prefer a plain REPL, or scripting a turn? `defra-agent chat` talks to the
same runtime without the Codex UI:

```bash
defra-agent chat
defra-agent chat "Introduce yourself in two short sentences."
```

## Permission presets

| | file tools | bash | containment |
|---|---|---|---|
| default | read-only | read-only allowlist | n/a — nothing can write |
| `--write` | read + write | write-capable | `sandbox-exec` seatbelt; writes contained to the tool root |
| `--yolo` | read + write | unrestricted | **none** — anything your user can do |

- **Default (read-only).** File tools can list, read, glob, and grep under
  the tool root. Bash is limited to a Rust-side allowlist of diagnostic
  commands (`ps`, `df`, `uptime`, ...). There is no write path.
- **`--write`.** File write/edit and write-capable bash, both capped by the
  tool root. On macOS, bash runs under a `sandbox-exec` deny-by-default
  seatbelt profile: writes outside the root are blocked by the OS, not by
  convention. See [macOS Bash Sandbox Policies](macos-bash-sandbox.md) for
  the exact policy tiers.
- **`--yolo`.** The same write tools with the sandbox off. The agent can run
  any command and touch any file your user account can. `init` prints a
  warning; mean it.

Identity is the other half of the boundary: every action the runtime takes is
attributed to the agent's DID, and every tool call is persisted as a document
you can audit afterwards (`defra-agent response show <request-id>`, or
`defra-agent trace timeline`).

## Other backends

`init` speaks to anything OpenAI-compatible:

```bash
defra-agent init --backend-preset ollama --model-name MODEL
defra-agent init --backend-preset openai --model-name MODEL
defra-agent init --backend-preset openrouter --model-name MODEL
defra-agent init --backend-preset chatgpt-codex --model-name MODEL   # uses your ~/.codex OAuth
defra-agent init --inference-url http://HOST:PORT/v1 --model-name MODEL
```

If the endpoint needs auth, pass `--api-key` or `--api-key-env-var NAME`.
OpenAI and OpenRouter presets default to their standard env vars
(`OPENAI_API_KEY`, `OPENROUTER_API_KEY`).

## Off the happy path

**Re-running init.** `init` is idempotent and re-running it with a different
preset (`--write`, `--yolo`) updates the tool documents in place. It does not
clear persisted runtime connectivity state; `defra-agent reset` does, and
`defra-agent init --reset` combines them. `--dangerously-overwrite` wipes the
home entirely.

**Isolated homes.** Pass `--home /some/path` to `init`, `server`, `chat`, and
`codex`-adjacent commands to keep a demo out of `~/.defra-agent`.

**The shim port is taken.** Something else is on 9292; either stop it or run
`defra-agent server --codex-shim-port PORT` and
`defra-agent codex --remote ws://127.0.0.1:PORT/`.

**No Codex endpoint.** If the server was started with `--no-codex-shim`,
`defra-agent codex` will tell you nothing is listening. Restart the server
without the flag.

**Inference is down.** The server starts degraded if the backend is
unreachable; `defra-agent status` and `GET /healthz` show backend health.
Make sure `llama-server` is still running and serving `/v1` on port 8080.

**Single requests without a UI:**

```bash
export GRAPHQL=http://127.0.0.1:9191/api/v0/graphql
defra-agent request submit --graphql "$GRAPHQL" --agent-did "did:key:..." \
  --content "Introduce yourself in two short sentences."
defra-agent response wait --graphql "$GRAPHQL" --request-id "<request-id>"
```

# Part 2 — Pair a second node

Part 1 is one runtime talking to itself. The reason defra-agent is built on
DefraDB is that the *same documents* can live on more than one node and stay
in sync over a peer-to-peer link — no shared server in the middle. Part 2
pairs a second runtime to the first and replicates a chat between them.

This is also the reference example for **how to drive Defra P2P replication
from an application**. The pattern is the point: you do not imperatively wire
peers together and hope the two sides agree. You **write a document that
describes the pairing you want, and the runtime reconciles live P2P state
toward it** — connecting, subscribing collections, installing replicators,
and recording what it did. The same idea as `kubectl apply`, applied to
gossip replication.

## The two documents

| Document | Who writes it | Meaning |
|---|---|---|
| `PeerPairingDesired` | you (the operator) | the pairing you want: which peer, which DID you expect it to have, which collections/profiles to replicate |
| `PeerPairingApplied` | the runtime reconciler | what it actually installed for that peer — the **ownership record** |

The split matters. The reconciler only ever tears down wiring it finds in
`PeerPairingApplied` — so collections or replicators you added by hand with
the low-level `p2p collections`/`p2p replicators` commands are never touched,
and deleting a pairing removes exactly what the pairing introduced and nothing
else. You declare intent; the runtime owns the consequences and can always
undo precisely its own work.

## 1. Start a second runtime

Keep Part 1's runtime running. In a fresh home, start a second one. Both bind
P2P to loopback for the demo:

```bash
defra-agent init  --home /tmp/coding --agent-name coding \
  --inference-url http://127.0.0.1:8080/v1 --model-name "$MODEL"
defra-agent server --home /tmp/coding --p2p-bind-addr 127.0.0.1 --p2p-port 0
```

Use Part 1's home (say `~/.defra-agent`) as the first node, "Amy", and
`/tmp/coding` as the second, "Coding".

## 2. Exchange a pairing invite

Pairing carries the remote agent's **DID** — the identity that is the
permission and audit boundary for everything that replicates. The
invite/join flow moves the DID for you, so you never hand-type it.

On Amy, mint an invite token. It encodes Amy's shareable P2P address, peer id,
DID, and the collection profiles it offers:

```bash
AMY_INVITE=$(defra-agent p2p pairings invite | jq -r .token)
```

On Coding, accept it. This writes Coding's `PeerPairingDesired` row for Amy
and — because this is the first leg — prints a **reciprocal token** so Amy can
pair back:

```bash
CODING_JOIN=$(defra-agent p2p pairings join --home /tmp/coding "$AMY_INVITE")
CODING_INVITE=$(printf '%s\n' "$CODING_JOIN" | jq -r .reciprocal_token)
```

Pairing is **directional**: Coding now wants documents from Amy, but Amy
doesn't yet know about Coding. Close the loop by joining the reciprocal token
on Amy:

```bash
defra-agent p2p pairings join "$CODING_INVITE"
```

## 3. Watch the runtime reconcile

You wrote documents; you installed nothing. Watch the reconciler converge:

```bash
defra-agent p2p pairings list --home /tmp/coding --output table
```

```
PEER       DID            PROFILES       CONNECTED  SUBSCRIBED  REPLICATING
12D3Koo…   did:key:amy…   chat-requests  yes        yes         yes
```

Those last three columns are the health of the pairing, each derived from a
different source so you can see *where* a stuck pairing is stuck:

- **CONNECTED** — the live peer list includes that peer id (the dial worked).
- **SUBSCRIBED** — every desired collection appears in `PeerPairingApplied`
  (the reconciler subscribed them).
- **REPLICATING** — every desired replicator address appears in
  `PeerPairingApplied` (the push replicator is installed).

All three flip to `yes` on their own, on the runtime's pairing sweep — no
further commands. If one stays `no`, that column tells you whether the problem
is connectivity, subscription, or replication.

## 4. Confirm replication

Send a chat on one node and read it on the other. Because the `chat-requests`
profile replicates the request/response/message collections, a request created
on Coding and its streamed response are visible from Amy:

```bash
defra-agent chat --home /tmp/coding "Say hello to the other node."
defra-agent request submit --graphql http://127.0.0.1:9191/api/v0/graphql \
  --agent-did "$CODING_DID" --content "ping" | jq -r '.request_id'
# read the replicated row back from Amy's endpoint
```

## 5. Unpair

Delete the desired row. The runtime sees the row gone, reads its
`PeerPairingApplied` record, tears down **only** what it installed for that
pairing, then deletes the applied record:

```bash
defra-agent p2p pairings rm --home /tmp/coding --peer "$AMY_PEER_ID"
```

Any wiring you had added by hand survives — the reconciler never owned it.

## When to reach past invite/join

- **`p2p pairings set`** is the scripted/manual path: you supply `--did`,
  `--address`, and `--collection`/`--profile` directly. `--did` is required —
  a pairing always names the identity it trusts. `--peer` is optional when an
  `--address` is a shareable ticket the peer id can be derived from.
- **`p2p connect` / `p2p collections` / `p2p replicators`** are imperative
  surgery on live state, for non-paired topologies and debugging. They do not
  write desired documents, so the reconciler leaves their wiring alone.

One last boundary: replication moves *documents*, not *permission*. A child
node replicating a parent's requests still cannot act as a delegated subagent
across deployments unless `subagent_allow_cross_deployment: true` is set on
both sides — that gate is off by default and deferred. Pairing is the
transport; trust is still configured explicitly.

## How this is wired (for the curious)

Everything above is documents. `init` writes config documents (principal,
backend, behavior, tool selection) through the same upsert code as
`defra-agent config ... set`. `codex` and `chat` create request documents;
the runtime claims them, drives the proven request lifecycle, executes tool
calls inside the preset's boundary, and persists every observable step. Bash
results come back with a `defra_exec:` JSON metadata envelope (command, exit
code, sandbox mode, truncation); command environments are stripped of
variables containing `KEY`, `SECRET`, or `TOKEN`.

The only file outside the database is `init.json` (home path, agent name,
DID, key path, tool ceiling, tool root) — filesystem context that documents
cannot hold.

## Testing

The mocked end-to-end harness for the `init → server → codex/chat` flow lives
in [crates/defra-agent-cli/tests](../crates/defra-agent-cli/tests). Gate with
the full package suites:

```bash
cargo test -p defra-agent
cargo test -p defra-agent-cli
```

There is also an ignored live smoke test against a real inference endpoint:

```bash
export DEFRA_AGENT_CLI_E2E_MODEL_ENDPOINT=http://127.0.0.1:8080/v1
export DEFRA_AGENT_CLI_E2E_MODEL_NAME=google/gemma-4-12B-it-qat-q4_0-gguf
# Optional for hosted OpenAI-compatible endpoints:
# export DEFRA_AGENT_CLI_E2E_API_KEY="$OPENAI_API_KEY"

cargo test -p defra-agent-cli \
  --test cli_live \
  cli_flow_runs_real_tool_loop_against_live_endpoint \
  -- --ignored --nocapture --test-threads=1

cargo test -p defra-agent-cli \
  --test cli_codex_shim \
  codex_shim_live_protocol_uses_real_backend \
  -- --ignored --nocapture --test-threads=1
```

## Further reading

- Desktop app, fleet, and P2P: [operations.md](operations.md)
- macOS bash sandbox tiers: [macos-bash-sandbox.md](macos-bash-sandbox.md)
- macOS release signing: [macos-signing.md](macos-signing.md)
- Schema/data model: `crates/defra-agent-protocol/schemas/README.md`
- Lean proof guide: `crates/defra-agent/proofs/README.md`
