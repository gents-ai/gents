# defra-agent

`defra-agent` is a DefraDB-backed agent runtime with a small consumer CLI.

If you want to try it, the shortest path is:
1. Build the CLI.
2. Start a local server with `--init`.
3. Submit requests and keep chatting on the same session.

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
export AGENT=./target/release/defra-agent-cli
export INFERENCE_ENDPOINT=http://127.0.0.1:8000/v1

export DATA_DIR=$PWD/var/demo-agent
export HTTP_PORT=9191
export GRAPHQL=http://127.0.0.1:$HTTP_PORT/api/v0/graphql

export AGENT_NAME=demo
export AGENT_DID=did:defra-agent:$AGENT_NAME
```

### 3. Start the server and initialize the default runtime

Run this in terminal 1:

```bash
$AGENT serve \
  --data-dir "$DATA_DIR" \
  --http-port "$HTTP_PORT" \
  --agent-name "$AGENT_NAME" \
  --init \
  --inference-endpoint "$INFERENCE_ENDPOINT"
```

This is idempotent. It bootstraps the principal, creates or updates the default backend document, and binds the default behavior to it before the runtime starts.

The server prints JSON when it is ready. The most useful fields are:

- `agent_did`
- `graphql`
- `default_behavior_id`
- `init.backend_id`
- `init.model_name`

### 4. Send the first message

`request submit` waits for the terminal response by default and prints both the `session_id` and the final response.

```bash
$AGENT request submit \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --content "Introduce yourself in two short sentences."
```

### 5. Keep chatting on the same session

Take the `session_id` from the previous command's JSON output and pass it back in:

```bash
$AGENT request submit \
  --graphql "$GRAPHQL" \
  --agent-did "$AGENT_DID" \
  --session-id "<session-id-from-the-previous-response>" \
  --content "What did I ask you to do in the previous turn?"
```

## Optional: Enable Read-Only Local Tools

If you want a simple tools demo, start the server with a read-only ceiling:

```bash
$AGENT serve \
  --data-dir "$DATA_DIR" \
  --http-port "$HTTP_PORT" \
  --agent-name "$AGENT_NAME" \
  --init \
  --inference-endpoint "$INFERENCE_ENDPOINT" \
  --tool-ceiling readonly
```

Then create a tool selection and attach it to the default behavior. The `backend_id` is `$AGENT_NAME-backend`. The `model_name` should match `init.model_name` from the server's startup JSON.

```bash
export TOOL_SELECTION_ID=$AGENT_NAME-tools
export BACKEND_ID=$AGENT_NAME-backend
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
