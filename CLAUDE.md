# CLAUDE.md

## What This Is

defra-agent is a Rust agent runtime backed by DefraDB. It leverages DefraDB's identity/access control (DID-based cryptographic identity) and P2P replication to give agents properties you can't get from a normal database: verifiable identity, document-level permissions, and gossip-based event propagation across nodes.

The entire control plane is document-driven. Configuration, requests, responses, sessions, tool calls -- everything is a DefraDB document. The agent watches for new request documents, processes them, and writes response documents back. This deep integration means the data store *is* the control plane: you configure agents by writing documents, trigger work by writing documents, and debug by reading documents.

Extracted from a larger project called Amygdala. This repo is intentionally narrow -- just the runtime framework and its formal specification.

## Development Flow

**The Lean proofs are the source of truth for all state machine behavior.**

When making changes that affect state transitions, lifecycle rules, or scheduling behavior:

1. **Start in the Lean spec** (`crates/defra-agent/proofs/`). Understand the current model. Make the change there first. Verify that the change doesn't violate safety/liveness properties you want to keep.
2. **Update conformance tests** (`tests/state_machine_conformance.rs`, `tests/lifecycle_regression.rs`). The spec change should drive what the tests expect.
3. **Update the Rust implementation** to satisfy the new tests and match the updated spec.

Not every code change touches the formal spec -- plumbing, tooling, and infrastructure changes don't. But anything that changes *what transitions are legal* or *what invariants hold* starts in Lean.

## Identity Model (Evolving)

The current code uses `AgentProfile` as the main runtime object, but this conflates two things that need to be separate (see sourcenetwork/defra-agent#9):

- **AgentPrincipal** -- the DID-backed identity. The permission and audit boundary. The thing that signs documents and is recognized by the wider system.
- **AgentBehavior** -- prompt, tools, model, backend policy. Multiple behaviors can exist for one principal.
- **AgentDeployment** -- where a principal runs (host/service). References the principal.

This distinction matters: `amy-general` and `amy-code` should be two behaviors of the same principal (shared permissions, shared audit trail), while `amy-rumination` should be a separate principal with narrower permissions. Principals are least-privilege boundaries; behaviors are reusable interfaces.

## Architecture

### State Machines

Three formal state machines modeled in Lean 4 and implemented in Rust:

1. **Process Lifecycle** (5 states): `Uninitialized -> Recovering -> Ready -> ShuttingDown -> Shutdown`
2. **Request Lifecycle** (8 states): `Pending -> Claimed -> Processing -> {Completed, Failed, Superseded, Dead}` (plus `InputRequired`)
3. **Persistence Lifecycle** (4 states): DB commit tracking

Key proven properties: terminal irreversibility (S1), monotonic progress (S3), deadline bounding (S4), recovery exclusivity (S5), persistence before completion (S6), bounded termination (L1), recovery convergence (L3).

### Document-Driven Control Plane

All state lives in DefraDB as GraphQL collections (schemas in `crates/defra-agent-protocol/schemas/`). The runtime reads configuration from documents and writes results back to documents. A CLI can validate, diff, and apply manifests from checked-in files into the DB (see sourcenetwork/defra-agent#8).

Field ownership matters: the apply path owns desired-state fields (config, prompts, backend references), while the runtime owns live-state fields (probe_status, run counts, lifecycle state). Neither clobbers the other.

### Event-Driven Tasks

Automated work is split into two collections: `Task` (a reusable unit of work — prompt template, target behavior, output schema) and `Schedule` (a cron-style trigger that references a Task). These replace the legacy `ScheduledTask` collection. The `TriggerEngine` runtime subsystem dispatches fires produced by pluggable `TriggerSource` implementations; `ScheduleSource` is the only source in PR 1 (future PRs add `EventSource` and manual runs). Every materialized `AgentRequest` carries `caused_by_trigger_id` + `caused_by_trigger_kind` so lineage and concurrency queries can tuple-match against the originating trigger. See the spec at `docs/superpowers/specs/2026-04-21-event-driven-tasks-design.md` (PR 2 adds EventTrigger, PR 3 adds manual runs).

## Key External Dependencies

- **defradb.rs** (`sourcenetwork/defradb.rs`, private, via SSH git): The core database. Provides `defra-node` (embedded node), `crypto`, `identity`, and `events` crates. Pinned by git rev in workspace `Cargo.toml`. When working on features that touch the node, schema behavior, or identity, look at this repo for context.
- **rig-core**: LLM agent framework. Provides the completion model trait, tool trait, and hook system that defra-agent integrates with.
- **rmcp**: MCP protocol client for connecting to external tool services.

## Build & Test

```bash
cargo check                          # Fast compilation check
cargo build                          # Debug build
cargo build --release                # Release (LTO, stripped)
cargo test                           # All tests
cargo test -p defra-agent            # Library tests only
cargo test -p defra-agent -- <name>  # Specific test

# Lean proofs (requires Lean 4 / Lake)
cd crates/defra-agent/proofs && lake build
```

## Code Conventions

- Workspace-level dependency management in root `Cargo.toml`
- Tracing for all logging (`tracing::{debug, info, warn, error}`), not `println`
- GraphQL queries are constructed inline (not code-generated) -- always use `graphql::escape_graphql_string()` for interpolated content
- DefraDB schemas are `include_str!` compiled into the binary from `schemas/`
- Error types carry retry classification (`is_retryable()`) for inference failures
- Trait-based extensibility for core interfaces: `Watcher`, `StreamWriter`, `PromptBuilder`, `Compactor`, `Truncator`, `AgentIdentity`
- Unit tests in `<module>/tests.rs` submodules; integration tests in `tests/` (require a running DefraDB node)
