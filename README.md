# defra-agent

`defra-agent` is a DefraDB-backed agent runtime with a small consumer CLI.

If you want to try it, the shortest path is:
1. Build the CLI and desktop binaries.
2. Run `init`.
3. Start `server`.
4. For the desktop demo, run `defra-agent-desktop init`, launch the desktop app, and wait for `replication: subscriptions armed`.
5. Open `chat` in another terminal or submit a prompt from the desktop Chat view.

## Demo Quickstart

Prerequisites:

- Rust toolchain
- local Ollama for the default path, or another reachable OpenAI-compatible inference endpoint
- default model `gemma4-26b-a4b` pulled in Ollama, or one model name served by your custom endpoint
- if your endpoint requires auth, pass `--api-key` or `--api-key-env-var` during `init`

The backend endpoint must be the OpenAI-compatible base URL, including `/v1`.

### 1. Build the CLI and desktop

```bash
cargo build -p defra-agent-cli -p defra-agent-desktop --release
```

### 2. Set demo variables

Set the binary paths. The default `init` target is local Ollama at `http://localhost:11434/v1` with model `gemma4-26b-a4b`.

```bash
export AGENT=./target/release/defra-agent
export DESKTOP=./target/release/defra-agent-desktop
```

By default the CLI keeps its local node, keys, and runtime state in `~/.defra-agent`.
If you want to isolate a demo, pass `--home /some/path` to `init`, `server`, and `chat`, and pass the same path as `--agent-home /some/path` to `$DESKTOP init`.

### 3. Initialize the default runtime

Run this once:

```bash
$AGENT init
```

This is idempotent. It provisions a standard safe home directory under `~/.defra-agent`:

- default principal
- default backend
- default tool selection
- default behavior
- a CLI-oriented default system prompt
- read-only file and bash tools by default

The default tool root is the current directory where you run `init`. Pass `--tool-root /path/to/root` if you want a different read-only scope.

For the default local backend, pull the default model before starting the server:

```bash
ollama pull gemma4-26b-a4b
```

To point at a different OpenAI-compatible backend during init:

```bash
export INFERENCE_ENDPOINT=http://127.0.0.1:8000/v1
export MODEL_NAME=your-model-name

$AGENT init "$INFERENCE_ENDPOINT" --model-name "$MODEL_NAME"
```

If you want write-capable local tools for a demo, opt in explicitly:

```bash
$AGENT init --write-tools
```

With `--write-tools`, the same tool root also caps write/edit and unrestricted bash access.

If you want to wipe and recreate the configured agent home from scratch:

```bash
$AGENT init --dangerously-overwrite
```

Re-running `init` does not clear persisted runtime connectivity state. Clear it explicitly when needed:

```bash
$AGENT reset
# or combine it with init:
$AGENT init --reset
```

If you want to isolate a demo, pass `--home /some/path` to `init`, `server`, and `chat`, and pass the same path as `--agent-home /some/path` to `defra-agent-desktop init`.

If you want to override the default backend id as well:

```bash
$AGENT init "$INFERENCE_ENDPOINT" \
  --model-name "$MODEL_NAME" \
  --backend-id my-backend
```

The init output is JSON. The most useful fields are:

- `agent_did`
- `key_path`
- `default_behavior_id`
- `tool_ceiling`
- `init.backend_id`
- `init.endpoint`
- `init.model_name`
- `init.tool_selection_id`
- `next_steps`

If your inference endpoint requires auth, pass `--api-key-env-var NAME` or `--api-key VALUE` when running `init`.

### 4. Start the server

Run this in terminal 1:

```bash
$AGENT server
```

The server reads the initialized home directory, starts the local node, and prints JSON when it is ready. The most useful fields are:

- `agent_did`
- `graphql`
- `default_behavior_id`
- `tool_ceiling`
- `p2p_transport`
- `p2p_peer_id`
- `p2p_listen_addresses`

The same connectivity fields are persisted to `runtime.json` under the agent home, usually `~/.defra-agent/runtime.json`. Use `defra-agent reset` to remove that file.

The runtime HTTP port also exposes a stable machine-readable operations surface:

- `GET /version`: build and package metadata for the running `defra-agent` binary.
- `GET /healthz`: JSON process/runtime health. It returns HTTP 200 when the runtime is serving, including degraded-but-running states, and HTTP 503 when the runtime status cannot be read or no runtime is ready.
- `GET /metrics`: Prometheus metrics for runtime state, backend health, and admission settings.

`server` stays in the foreground until you stop it with `Ctrl-C`. By default it keeps logs quiet and prints the readiness JSON plus a short status line. If you want debug output, set `RUST_LOG=info` or a more specific filter before starting it.

The standard server path always starts the IROH P2P transport for local desktop pairing. It binds to localhost on an ephemeral P2P port by default, with relay and discovery disabled.

To pin the local P2P socket for demos:

```bash
$AGENT server \
  --p2p-bind-addr 127.0.0.1 \
  --p2p-port 4017 \
  --p2p-relay-mode disabled \
  --p2p-discovery disabled
```

### 5. Pair and launch the desktop

Run this in terminal 2:

```bash
$DESKTOP init
$DESKTOP
```

`defra-agent-desktop init` only discovers the local runtime and saves it in the desktop peer directory. The desktop app completes the P2P pairing and replication bootstrap when it launches. For the replicated chat demo, leave the desktop app open and wait for the status bar to show `replication: subscriptions armed` before sending prompts you expect to render in the UI.

### 6. Start chatting

Run this in terminal 3, after the desktop has finished bootstrapping:

```bash
$AGENT chat
```

That opens a terminal session using the runtime state written by `server`. Type a message, press Enter, and keep going on the same session. Exit with `/exit`. While the agent is working, `chat` now prints live tool progress and streamed response text instead of waiting silently for the turn to finish.

If you only want a single turn, pass the message directly:

```bash
$AGENT chat "Introduce yourself in two short sentences."
```

## Tool Defaults

`init` enables a standard local toolset by default:

- file tools: `ReadOnly`
- bash: `ReadOnly`
- meta tools: enabled

That means a fresh demo can immediately inspect the local filesystem after `init -> server -> chat`.

If you want write-capable tools instead, rerun `init` with `--write-tools`.

`init` writes the default backend, behavior, and tool-selection documents through the same upsert code used by `config backend set`, `config behavior set`, and `config tools set`.

The remaining `init.json` file stores only filesystem-local context that is not represented in DefraDB documents: the home path, agent name, agent DID, key path, operator tool ceiling, and tool root. Runtime configuration itself lives in DefraDB documents.

## Advanced: Change Tool Selection After Init

If you want to replace the default tool selection, you can still update documents directly. Read `agent_did` and `graphql` from the `server` startup JSON.

```bash
export GRAPHQL=http://127.0.0.1:9191/api/v0/graphql
export AGENT_DID="did:key:..."
export TOOL_SELECTION_ID="${AGENT_DID}:default-tools"

$AGENT config tools set \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --selection-id "$TOOL_SELECTION_ID" \
  --enable-file-tools

$AGENT config behavior set \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --tool-selection-id "$TOOL_SELECTION_ID" \
  --display-name "Demo"
```

After that, the agent can use read-only file tools on new requests. This change is live; no restart is required.

## Connected Runtime Bring-Up

To bring up two runtimes and connect them through the operator CLI:

```bash
$AGENT server --home /tmp/amy --p2p-bind-addr 127.0.0.1 --p2p-port 4017
$AGENT server --home /tmp/coding --p2p-bind-addr 127.0.0.1 --p2p-port 4018
```

Read Amy's startup JSON or `/tmp/amy/runtime.json` and take one of the values from `p2p_listen_addresses`.

Then connect Coding to Amy:

```bash
$AGENT p2p connect --home /tmp/coding --peer "<peer-id-or-listen-address>"
```

Inspect connectivity from either runtime:

```bash
$AGENT p2p status --home /tmp/amy
$AGENT p2p peers --home /tmp/coding
$AGENT status --home /tmp/coding
```

The most useful fields for bring-up are:

- `p2p_transport`
- `p2p_peer_id`
- `p2p_listen_addresses`
- `p2p_connected_peers`

## Useful Commands

Submit a single request without using `chat`:

```bash
$AGENT request submit \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --content "Introduce yourself in two short sentences."
```

Show a request document:

```bash
$AGENT request show \
  --graphql "$GRAPHQL" \
  --request-id "<request-id>"
```

Show a response document:

```bash
$AGENT response show \
  --graphql "$GRAPHQL" \
  --request-id "<request-id>"
```

Wait for a response explicitly:

```bash
$AGENT response wait \
  --graphql "$GRAPHQL" \
  --request-id "<request-id>"
```

## Testing

The main regression path should use the shipped CLI binary against the full local flow:

1. `defra-agent init`
2. `defra-agent server`
3. `defra-agent chat` or `defra-agent request submit`

That flow already has a mocked end-to-end harness in [crates/defra-agent-cli/tests/cli_e2e.rs](crates/defra-agent-cli/tests/cli_e2e.rs). Those tests are intentionally:

- idempotent
- isolated to a temp home directory
- bound to ephemeral local ports
- cleaned up when the test exits

## Further Reading

- Schema/data model: `crates/defra-agent-protocol/schemas/README.md`
- Lean proof guide: `crates/defra-agent/proofs/README.md`
- macOS release signing: `docs/macos-signing.md`

Run the mocked binary-flow suite locally with:

```bash
cargo test -p defra-agent-cli --test cli_e2e -- --nocapture --test-threads=1
```

Run the library/integration suite with:

```bash
cargo test -p defra-agent --lib --tests
```

Run the Lean proofs with:

```bash
cd crates/defra-agent/proofs && lake build
```

There is also an ignored live smoke test for the real binary flow against an external inference endpoint:

```bash
export DEFRA_AGENT_CLI_E2E_MODEL_ENDPOINT=http://workstation-1:8000/v1
export DEFRA_AGENT_CLI_E2E_MODEL_NAME=MiniMax-M2.7-NVFP4
# export DEFRA_AGENT_CLI_E2E_API_KEY=...   # if your endpoint requires auth

cargo test -p defra-agent-cli \
  --test cli_e2e \
  cli_flow_runs_real_tool_loop_against_live_endpoint \
  -- --ignored --nocapture --test-threads=1
```

The mocked binary-flow suite is what CI should gate on. The live smoke test is for manual or release validation, not the main correctness gate.

## Repository Layout

- `crates/defra-agent`
  runtime library, schemas, state-machine conformance tests, and Lean proofs
- `crates/defra-agent-cli`
  compiled CLI used in the demo flow above

## Proofs

The Lean proof tree lives under `crates/defra-agent/proofs`.

Good entry points:

- `crates/defra-agent/proofs/Proofs/SessionRecovery.lean`
- `crates/defra-agent/proofs/Proofs/RuntimeReconcile.lean`
- `crates/defra-agent/proofs/README.md`
