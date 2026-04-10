# defra-agent

`defra-agent` is a DefraDB-backed agent runtime with a small consumer CLI.

If you want to try it, the shortest path is:
1. Build the CLI.
2. Run `init <inference-endpoint>`.
3. Start `server`.
4. Open `chat` in another terminal.

## Demo Quickstart

Prerequisites:

- Rust toolchain
- one reachable OpenAI-compatible inference endpoint
- one model name served by that endpoint
- if your endpoint requires auth, set `AGENT_DAEMON_API_KEY`

The backend endpoint must be the OpenAI-compatible base URL, including `/v1`.

### 1. Build the CLI

```bash
cargo build -p defra-agent-cli --release
```

### 2. Set demo variables

Set the endpoint and model name for your inference backend.

```bash
export AGENT=./target/release/defra-agent
export INFERENCE_ENDPOINT=http://127.0.0.1:8000/v1
export MODEL_NAME=your-model-name
```

By default the CLI keeps its local node, keys, and runtime state in `~/.defra-agent`.
If you want to isolate a demo, pass `--home /some/path` to both `server` and `chat`.

### 3. Initialize the default runtime

Run this once:

```bash
$AGENT init "$INFERENCE_ENDPOINT" --model-name "$MODEL_NAME"
```

This is idempotent. It provisions a standard safe home directory under `~/.defra-agent`:

- default principal
- default backend
- default tool selection
- default behavior
- a CLI-oriented default system prompt
- read-only file and bash tools by default

If you want write-capable local tools for a demo, opt in explicitly:

```bash
$AGENT init "$INFERENCE_ENDPOINT" --model-name "$MODEL_NAME" --write-tools
```

With `--write-tools`, the default tool root is the current directory where you run `init`. Pass `--tool-root /path/to/root` if you want a different scope.

If you want to wipe and recreate the configured agent home from scratch:

```bash
$AGENT init "$INFERENCE_ENDPOINT" --model-name "$MODEL_NAME" --dangerously-overwrite
```

If you want to isolate a demo, pass `--home /some/path` to `init`, `server`, and `chat`.

If you want to override the default backend id as well:

```bash
$AGENT init "$INFERENCE_ENDPOINT" \
  --model-name "$MODEL_NAME" \
  --backend-id my-backend
```

The init output is JSON. The most useful fields are:

- `agent_did`
- `default_behavior_id`
- `tool_ceiling`
- `init.backend_id`
- `init.model_name`
- `init.tool_selection_id`

If your inference endpoint requires auth, export `AGENT_DAEMON_API_KEY` before running `init`.

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

`server` stays in the foreground until you stop it with `Ctrl-C`. By default it keeps logs quiet and prints the readiness JSON plus a short status line. If you want debug output, set `RUST_LOG=info` or a more specific filter before starting it.

### 5. Start chatting

Run this in terminal 2:

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

The bootstrap templates live in:

- `crates/defra-agent-cli/bootstrap/InferenceBackend/default.gql`
- `crates/defra-agent-cli/bootstrap/ToolSelection/standard-readonly.gql`
- `crates/defra-agent-cli/bootstrap/ToolSelection/standard-readwrite.gql`
- `crates/defra-agent-cli/bootstrap/AgentBehavior/standard-readonly.gql`
- `crates/defra-agent-cli/bootstrap/AgentBehavior/standard-readwrite.gql`

They are raw GraphQL mutations, one object per file, organized by collection so the default bootstrap documents stay easy to inspect and edit.

## Advanced: Change Tool Selection After Init

If you want to replace the default tool selection, you can still update documents directly. Read `agent_did` and `graphql` from the `server` startup JSON.

```bash
export GRAPHQL=http://127.0.0.1:9191/api/v0/graphql
export AGENT_DID="did:defra-agent:default"
export TOOL_SELECTION_ID=default-tools

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
