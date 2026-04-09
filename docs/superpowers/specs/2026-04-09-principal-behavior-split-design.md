# Split Agent Principal Identity from Behavior Configuration

Issue: sourcenetwork/defra-agent#9
Date: 2026-04-09

## Problem

`ProfileConfig` conflates three distinct concerns into one object:

1. The DID-backed agent identity / permission boundary
2. The prompt + tools + model/backend behavior for a particular interface or job
3. Inference tuning parameters (context window, temperature, deadlines)

This makes it impossible to have multiple behaviors for one principal, prevents narrow-purpose agents with reduced permissions, and blocks the document-driven control plane (#8) from having clean schema boundaries.

## Design Decisions

These were established during brainstorming:

- **One binary = one principal.** No separate AgentDeployment abstraction. The binary's identity IS the agent principal.
- **Identity is pluggable.** File-based for dev/test, secure enclave / YubiKey / DKG for production. The `AgentIdentity` trait already supports this.
- **Everything document-driven.** Deploy the binary, configure it entirely through DefraDB document writes. Bootstrap state, who configured what, is held in DefraDB.
- **`agent_did` is required on requests.** The watcher filters by `agent_did` in its query -- it only sees requests for its principal.
- **`behavior_id` is optional on requests.** Omit it and the principal's default behavior handles it.
- **ACP on collections handles caller authorization.** If you can write to the request collection, you're authorized to call the agent. No separate authz layer needed.
- **Tool mode is server-level config, not document-driven.** The deployer sets the tool capability ceiling at startup. No document write can escalate tool capabilities. This is a security boundary.
- **This is the foundation for #8.** The document-driven control plane (manifests, apply, reconcile) builds on the schemas and types defined here.

## Document Model

Four document types, clean separation of concerns:

### AgentPrincipal -- who

```graphql
type AgentPrincipal {
  agent_did: String!
  display_name: String
  default_behavior_id: String
  enabled: Boolean!
  created_at: DateTime
  created_by: String
}
```

Self-registered on first boot. The binary derives its DID from the identity source, creates the principal document if it doesn't exist, and creates a default blank-slate behavior alongside it.

### AgentBehavior -- what

```graphql
type AgentBehavior {
  behavior_id: String!
  agent_did: String!
  display_name: String
  system_prompt: String
  backend_id: String
  model_name: String
  inference_profile_id: String
  compaction_strategy: String
  compaction_threshold: Float
  created_at: DateTime
}
```

Defines what the agent does: prompt, which backend + model to use, how to manage context. Multiple behaviors per principal. Compaction lives here because it's about how the behavior manages conversation context, not about model inference parameters.

### InferenceProfile -- how to call the model

```graphql
type InferenceProfile {
  profile_id: String!
  display_name: String
  context_window: Int
  max_output_tokens: Int
  max_turns: Int
  temperature: Float
  stream_batch_ms: Int
  deadline_duration_secs: Int
}
```

Pure model-calling knobs. Reusable across behaviors. All fields have sensible defaults. Optional -- if a behavior omits `inference_profile_id`, the runtime uses built-in defaults.

### InferenceBackend -- where (already exists)

```graphql
type InferenceBackend {
  backend_id: String
  name: String
  endpoint: String
  max_concurrent: Int
  enabled: Boolean
  models: [String]
  last_probe: DateTime
  probe_status: String
}
```

No changes needed. Behaviors reference backends by `backend_id`. The runtime resolves the endpoint from the backend document.

### Resolution chain

```
AgentPrincipal -> AgentBehavior -> InferenceBackend + InferenceProfile -> composed ProfileConfig
```

### Incomplete behaviors

A behavior may reference a backend that doesn't exist yet (e.g., on first boot before any backends are registered). The runtime loads the behavior but does not spin up a daemon for it until its backend is resolvable and healthy. This is normal during bootstrap -- the operator creates the principal/behavior first, then registers backends.

## Server-Level Configuration

The binary starts with minimal, non-document config:

- **Data directory** -- where DefraDB stores data
- **HTTP port** -- how to reach the node
- **Identity procurement** -- how to get the principal's key (file path, enclave, YubiKey, DKG)
- **Tool mode** -- `readonly`, `readwrite`, `meta_only` (capability ceiling for all behaviors)

Everything else comes from DefraDB documents.

## Implementation: Four Vertical Slices

Each slice adds schema + Rust types + runtime integration together.

### Slice 1: AgentPrincipal

**Schema:** Add `AgentPrincipal` collection to DefraDB.

**Rust type:** `AgentPrincipal` struct mapping to the document.

**First-boot flow:**
1. Binary starts with: data dir, HTTP port, identity source, tool mode
2. Boots DefraDB node, ensures schemas
3. Derives DID from identity source
4. Queries for `AgentPrincipal` where `agent_did` matches
5. If not found: creates principal document (enabled=true, display_name from DID) + default blank-slate behavior
6. Stores the principal in the runtime context

**Runtime change:** `DefraAgent` gets an `Arc<AgentPrincipal>` instead of deriving identity per-profile.

**Builder API:** `DefraAgent::builder().identity(...)` sets the principal. A new document-driven path `DefraAgent::from_node(node)` discovers the principal from DB.

### Slice 2: AgentBehavior + InferenceProfile

**Schemas:** Add `AgentBehavior` and `InferenceProfile` collections.

**Rust types:** `AgentBehavior` and `InferenceProfile` structs.

**Default behavior:** Created alongside the principal in slice 1's bootstrap. Principal's `default_behavior_id` points to it. Starts as a blank slate with sensible defaults.

**ProfileConfig becomes composed:**

```rust
pub struct ProfileConfig {
    pub principal: Arc<AgentPrincipal>,
    pub behavior: AgentBehavior,
    pub identity: Arc<dyn AgentIdentity>,
    // resolved fields: tool set (from server config), inference params, etc.
}
```

Built from: principal doc + behavior doc + backend doc + optional inference profile doc + server-level tool mode.

**Runtime loading:**
1. After principal is established, query `AgentBehavior` where `agent_did` matches
2. For each behavior, optionally resolve its `InferenceProfile` and `InferenceBackend`
3. Build a `ProfileConfig` per behavior
4. Spin up a daemon per behavior

### Slice 3: Request Routing

**AgentRequest schema update:** Add `behavior_id` field (optional).

**Watcher flow:**
1. Request arrives (watcher query already filters by `agent_did` -- only sees its own requests)
2. Resolve `behavior_id`: present -> use it, absent -> use `default_behavior_id` from principal
3. Route to that behavior's daemon

**Daemon dispatch:** Runtime holds a map of `behavior_id -> ProfileDaemon`. Watcher resolves behavior and hands request to the right daemon.

**RequestLifecycle:** Updated to carry `behavior_id` alongside `agent_did`. Flows through to response and conversation documents for debugging and audit.

### Slice 4: Scheduler + Cleanup + CLI

**ScheduledTask:** `profile_name` replaced with `behavior_id`. Scheduler resolves behavior from the loaded set. If behavior isn't loaded, skip the task.

**DaemonConfig removal:** The TOML + env override config struct. Its fields are now split across server config (tool_mode), InferenceProfile documents, and AgentBehavior documents. Reduced to server-level fields only, or removed entirely.

**Builder API reshape:**

```rust
DefraAgent::builder()
    .node(node)
    .tool_mode(ToolMode::ReadOnly)
    .principal("amy", identity)
    .behavior("general")
        .system_prompt("...")
        .backend_id("local")
        .model_name("default")
        .done()
    .behavior("code")
        .system_prompt("...")
        .backend_id("local")
        .model_name("default")
        .done()
    .build()?
```

One principal, multiple behaviors. Constructs the same internal types as the document-driven path.

**CLI slim-down:** `defra-agent-cli serve` takes only server-level flags: `--data-dir`, `--http-port`, `--key-path`, `--tool-mode`. Everything else comes from DB.

**Chat mode:** New `defra-agent-cli chat` command:

```bash
defra-agent-cli chat \
  --graphql http://127.0.0.1:9191/api/v0/graphql \
  --agent-did did:defra-agent:amy \
  --behavior-id general
```

Creates a session, loops: take user input -> write AgentRequest document -> poll for response document -> print -> repeat. Proof that the document-driven model works end-to-end.

## What This Enables

- **Multiple behaviors per principal:** `amy-general` and `amy-code` share a DID, permissions, and audit trail
- **Narrow-purpose agents:** `amy-rumination` is a separate principal with reduced permissions
- **Document-driven everything:** Deploy binary, configure via DB writes, debug by reading documents
- **Foundation for #8:** Manifest apply/diff/reconcile builds on these schemas
- **Foundation for tool work:** #1, #4, #7 build on the behavior model for tool surface control
- **ACP-enforced authorization:** DefraDB collection permissions control who can call the agent

## Out of Scope

- Manifest validate/diff/apply CLI workflow (#8)
- Startup reconciliation from manifests (#8)
- Custom tool factories and consumer-provided tools (#1)
- Configurable meta-tool/delegate injection (#4)
- Backend capability model (#7)
- Granular per-behavior tool surface control (future tool work)
- Hot-reload of behavior documents (future -- restart required for now)
