# defra-agent

`defra-agent` is a DefraDB-backed agent runtime with a small consumer CLI.

If you want to try it, the shortest path is:
1. Build the CLI.
2. Start a local server with `--init <inference-endpoint>`.
3. Open `chat` in another terminal.

## Demo Quickstart

Prerequisites:

- Rust toolchain
- one reachable OpenAI-compatible inference endpoint
- if your endpoint requires auth, set `AGENT_DAEMON_API_KEY`

The backend endpoint must be the OpenAI-compatible base URL, including `/v1`.

### 1. Build the CLI

```bash
cargo build -p defra-agent-cli --release
```

### 2. Set demo variables

Change only `INFERENCE_ENDPOINT`. The rest can stay as-is for a local demo.

```bash
export AGENT=./target/release/defra-agent
export INFERENCE_ENDPOINT=http://127.0.0.1:8000/v1
```

By default the CLI keeps its local node, keys, and runtime state in `~/.defra-agent`.
If you want to isolate a demo, pass `--home /some/path` to both `server` and `chat`.

### 3. Start the server and initialize the default runtime

Run this in terminal 1:

```bash
$AGENT server --init "$INFERENCE_ENDPOINT"
```

This is idempotent. It bootstraps the principal, creates or updates the default backend document, and binds the default behavior to it before the runtime starts.

If you want to override the default binding, pass both `--backend-id` and `--model-name` together:

```bash
$AGENT server \
  --init "$INFERENCE_ENDPOINT" \
  --backend-id my-backend \
  --model-name my-model
```

The server prints JSON when it is ready. The most useful fields are:

- `agent_did`
- `graphql`
- `default_behavior_id`
- `init.backend_id`
- `init.model_name`

If your inference endpoint requires auth, export `AGENT_DAEMON_API_KEY` before starting the server.

### 4. Start chatting

Run this in terminal 2:

```bash
$AGENT chat
```

That opens a terminal session using the runtime state written by `server`. Type a message, press Enter, and keep going on the same session. Exit with `/exit`.

If you only want a single turn, pass the message directly:

```bash
$AGENT chat "Introduce yourself in two short sentences."
```

## Optional: Enable Read-Only Local Tools

If you want a simple tools demo, start the server with a read-only ceiling:

```bash
$AGENT server \
  --init "$INFERENCE_ENDPOINT" \
  --tool-ceiling readonly
```

Then create a tool selection and attach it to the default behavior. Read `agent_did`, `graphql`, `init.backend_id`, and `init.model_name` from the server startup JSON.

```bash
export GRAPHQL=http://127.0.0.1:9191/api/v0/graphql
export AGENT_DID="did:defra-agent:default"
export TOOL_SELECTION_ID=default-tools
export BACKEND_ID=default-backend
export MODEL_NAME="<model-name-from-serve-output>"

$AGENT tool-selection upsert \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --selection-id "$TOOL_SELECTION_ID" \
  --enable-file-tools

$AGENT behavior upsert \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --backend-id "$BACKEND_ID" \
  --model-name "$MODEL_NAME" \
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
