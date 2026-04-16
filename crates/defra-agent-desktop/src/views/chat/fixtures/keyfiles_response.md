

Based on my review of the codebase, here's how the P2P connection between the **defra-agent runtime** and the **defra-agent-desktop** app works:

## Architecture Overview

Both the agent runtime and desktop app are embedded DefraDB nodes using the **Iroh** P2P stack. They connect and replicate document collections over a P2P channel while using HTTP for API calls.

### Key Components

**1. Peer Directory** (`client/peer_directory.rs`)
- JSON file (`peers.json`) stored in the desktop data directory
- Persists peer records with: `peer_id`, `label`, `addr` (Iroh address), `agent_did`, and optional `graphql` URL
- Two types of peer sources:
  - `"local-standard"` — auto-discovered local runtime (stored alongside its HTTP GraphQL endpoint)
  - `"saved"` — manually added remote peers

**2. Transport Layer** (`client/core.rs` + `defra_node::P2PConfig`)
- Uses **Iroh** as the P2P transport (`p2p_transport: "iroh"`)
- Desktop app runs its own embedded node with configurable:
  - `relay_mode` (Disabled for local-only, Enabled for NAT traversal)
  - `discovery` config (Disabled for local-only)
  - Iroh secret key persisted to `iroh_secret_key_path()`
- Both nodes expose HTTP API endpoints (used for pairing/control) alongside their P2P transport

**3. Pairing Flow** (`local_runtime.rs`)

For local runtime pairing, the desktop completes a 3-step HTTP handshake (with retries):

```
POST /p2p/connect       → tells the agent to dial the desktop's P2P address
POST /p2p/collections   → registers collection names to replicate
POST /p2p/replicators    → sets up the replicator for DAG sync
```

This happens in `complete_runtime_pairing()` with exponential backoff (250ms backoff, 20s timeout).

**4. Document Subscription** (`client/schema.rs`)

Collections are subscribed via:
```rust
p2p.add_collections(vec![name.to_owned()]).await
```

The desktop subscribes to all runtime + agent protocol collections (`RUNTIME_COLLECTION_NAMES` + `ALL_COLLECTION_NAMES`) — things like `AgentRequest`, `AgentResponse`, `AgentSession`, `AgentBehavior`, etc.

**5. Observation Loop** (`client/observe.rs`)

The desktop spawns an observer task that:
- Subscribes to `EventName::Update` from the embedded node
- Debounces updates (150ms) to batch rapid changes
- Reloads a full snapshot from the DB on each batch
- Broadcasts version increments to UI watchers via a `watch::Sender<u64>`

**6. Bootstrap** (`client/core.rs`)

On startup, `ClientCore::start_with_paths_and_options()`:
1. Builds its own embedded node with P2P config
2. Loads peer directory from disk
3. Bootstraps connections to all saved peers:
   - `connect_peer_with_retry()` — dials the P2P address
   - `add_replicator_with_retry()` — installs collection replicators
   - `configure_local_runtime_pairing()` — for "local-standard" peers, completes the HTTP handshake
4. Spawns a background maintenance task that re-pairs peers every 5s if connections fail

**7. Replicator Setup**

Replicators are installed with:
```rust
p2p.add_replicator(collections, Some(addr), Vec::new(), None).await
```

This tells the node's DAG sync system to push/pull documents for those collections to/from the given peer address.

### Key Files

| File | Role |
|------|------|
| `local_runtime.rs` | Discovers agent runtime, writes peer record, completes HTTP pairing |
| `client/core.rs` | Desktop node bootstrap, peer maintenance loop, mutation API |
| `client/peer_directory.rs` | Persistent peer registry |
| `client/observe.rs` | Event subscription → snapshot refresh |
| `client/schema.rs` | Schema registration + collection subscription |
| `app/tests/bootstrap.rs` | End-to-end test showing full pairing + replication round-trip |

### Important Constraints

- Desktop only supports Iroh transport — if the local runtime was started with a different transport, initialization fails with a clear error
- Peer removal doesn't actively disconnect transport sessions; it just stops future reconnection/maintenance
- The "local-standard" peer stores both a P2P listen address (for DAG replication) and a GraphQL HTTP URL (for control plane operations like requesting new sessions)
