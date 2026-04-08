# defra-agent

`defra-agent` is a DefraDB-backed agent runtime with an explicit Lean proof
surface for request lifecycle, admission control, scheduling, session recovery,
and DB-visible debugging state.

This repo is intentionally narrow. The runtime depends on `defradb.rs` plus
ordinary external crates, and carries its own Lean proofs and runtime schemas
instead of reaching back into Amygdala.

This repository is being extracted from Amygdala. The core goals are:

- keep the Lean model as the source of legal transitions
- keep Rust fixtures as the source of conformance evidence
- keep DefraDB state as the source of debugging truth
- keep the runnable surface small and consumable from outside the library

## Repository Layout

- `crates/defra-agent`
  - runtime library, agent-private schemas, proofs, and conformance tests
- `crates/defra-agent-cli`
  - a consumer crate used to boot a local agent, register a backend, and submit requests

## Current Status

The request lifecycle, scheduler admission flow, session recovery model, and
DB-visible state machine have been aligned with the Lean model. The remaining
cleanup work is mostly code shape and extraction polish: large files such as
`toolset.rs` and `agent.rs` still need further decomposition.

## Quickstart

This quickstart runs a single local profile against an OpenAI-compatible
inference endpoint.

Prerequisites:

- Rust toolchain
- a reachable OpenAI-compatible endpoint
- `AGENT_DAEMON_API_KEY` set if your endpoint requires one

Start the agent in one terminal with the consumer CLI:

```bash
cargo run -p defra-agent-cli -- serve \
  --data-dir ./var/defradb \
  --http-port 9191 \
  --agent-name demo \
  --backend-id demo-backend \
  --model-endpoint http://127.0.0.1:8000/v1 \
  --model-name default
```

Or boot the library directly through its example:

```bash
DEFRA_AGENT_MODEL_ENDPOINT=http://127.0.0.1:8000/v1 \
cargo run -p defra-agent --example serve_single_profile
```

Register the backend in a second terminal:

```bash
cargo run -p defra-agent-cli -- backend upsert \
  --graphql http://127.0.0.1:9191/api/v0/graphql \
  --backend-id demo-backend \
  --name "Local Demo Backend" \
  --endpoint http://127.0.0.1:8000/v1 \
  --max-concurrent 1
```

Submit a request:

```bash
cargo run -p defra-agent-cli -- request submit \
  --graphql http://127.0.0.1:9191/api/v0/graphql \
  --agent-did did:defra-agent:demo \
  --content "Explain the request lifecycle state machine in one paragraph."
```

Then watch the response:

```bash
cargo run -p defra-agent-cli -- response wait \
  --graphql http://127.0.0.1:9191/api/v0/graphql \
  --request-id <request-id>
```

## Proofs

The Lean proof tree lives under `crates/defra-agent/proofs`.

Useful starting points:

- `crates/defra-agent/proofs/Proofs/Request.lean`
- `crates/defra-agent/proofs/Proofs/Fleet.lean`
- `crates/defra-agent/proofs/Proofs/SessionRecovery.lean`
- `crates/defra-agent/proofs/README.md`

## Next Extraction Steps

- split the remaining large runtime files
- add richer examples
- improve the CLI from “minimal local consumer” to “full operational shell”
- add CI around `cargo check`, targeted state-machine tests, and `lake build`
