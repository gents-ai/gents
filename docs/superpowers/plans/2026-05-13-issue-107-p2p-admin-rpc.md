# Issue #107 — P2P-only DefraDB Subscription Management Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build defra-agent's consumer-side reconcile + transport-agnostic `RemoteP2pAdmin` trait + per-peer retry surface + Lean module + minimal conformance harness, exactly as scoped in `docs/superpowers/specs/2026-05-13-issue-107-p2p-admin-rpc-design.md`.

**Architecture:** A new `RemoteP2pAdmin` trait (sibling to `defra_p2p_adapter::P2POperations`, which stays local-only) with an `HttpRemoteP2pAdmin` impl wrapping today's HTTP `/p2p/*` surface. The existing `spawn_p2p_supervisor_task` is extended to read a new `PeerPairingDesired` DefraDB doc per peer, diff against the remote's actual state (via `RemoteP2pAdmin::list_*`), and emit install/teardown RPCs only for divergence. Per-peer-per-collection retry counters live in-memory on `ClientPeerStatus`. A new Lean module `Proofs/PairingReconcile/*` proves the supervisor honors the TLA+ `Reconcile(n)` action semantics. A conformance harness scaffold lands with one hand-written scenario.

**Tech Stack:** Rust 2021 (workspace at repo root), `tokio`, `reqwest` (rustls), `serde`, `async_trait`, `tracing`. Lean 4 via `lake build` from `crates/defra-agent/proofs/`. DefraDB schemas registered through `defra-agent-protocol`. Existing test pattern uses `cargo test -p defra-agent` for conformance + integration tests against an embedded node.

**Spec source of truth:** `docs/superpowers/specs/2026-05-13-issue-107-p2p-admin-rpc-design.md`. Every task names the spec section it implements.

**Feature flag:** All behavior changes are gated on `DEFRA_AGENT_PAIRING_RECONCILE=1` env var until Task 12 dogfooding completes and the flag is dropped in a follow-up PR.

---

## File map

**Created:**
- `crates/defra-agent-protocol/schemas/agent/peer_pairing_desired.graphql`
- `crates/defra-agent-desktop-core/src/remote_admin/mod.rs` — module root
- `crates/defra-agent-desktop-core/src/remote_admin/trait_def.rs` — `RemoteP2pAdmin` trait + types + error enum
- `crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs` — `HttpRemoteP2pAdmin`
- `crates/defra-agent-desktop-core/src/remote_admin/diff.rs` — pure `compute_pairing_diff` function
- `crates/defra-agent-desktop-core/src/remote_admin/error_class.rs` — `PairingErrorClass` classifier
- `crates/defra-agent/proofs/Proofs/PairingReconcile/State.lean`
- `crates/defra-agent/proofs/Proofs/PairingReconcile/Transition.lean`
- `crates/defra-agent/proofs/Proofs/PairingReconcile/Executable.lean`
- `crates/defra-agent/proofs/Proofs/PairingReconcile/Convergence.lean`
- `crates/defra-agent/proofs/Proofs/PairingReconcile.lean` — barrel
- `crates/defra-agent/tests/support/pairing_conformance/mod.rs` — harness module root
- `crates/defra-agent/tests/support/pairing_conformance/scenario.rs` — JSON IR + parser
- `crates/defra-agent/tests/support/pairing_conformance/runner.rs` — orchestration + observer
- `crates/defra-agent/tests/support/pairing_conformance/invariants.rs` — safety + leads-to evaluator
- `crates/defra-agent/tests/fixtures/pairing_scenarios/install_teardown_happy_path.json`
- `crates/defra-agent/tests/pairing_reconcile_conformance.rs` — harness entry test

**Modified:**
- `crates/defra-agent-protocol/src/schemas.rs` — register `PeerPairingDesired` schema
- `crates/defra-agent-desktop-core/src/lib.rs` — `pub mod remote_admin;`
- `crates/defra-agent-desktop-core/src/client/core.rs` — `ClientPeerStatus` extension
- `crates/defra-agent-desktop-core/src/client/core/supervisor.rs` — reconcile loop wiring
- `crates/defra-agent-desktop-core/src/client/core/bootstrap.rs` — replace `configure_local_runtime_pairing` callers
- `crates/defra-agent/src/lean_vocab_test.rs` — register `PairingReconcile` domain
- `crates/defra-agent/proofs/lakefile.lean` — include new module (if Lake requires explicit entry; verify against existing `RuntimeReconcile.lean` registration pattern)

---

## Task 1: Add `PeerPairingDesired` GraphQL schema

**Spec section:** §6.2 Operator desired state.

**Files:**
- Create: `crates/defra-agent-protocol/schemas/agent/peer_pairing_desired.graphql`
- Modify: `crates/defra-agent-protocol/src/schemas.rs` (add constants + register in `ALL`/`ALL_COLLECTION_NAMES`, update the `assert_eq!(ALL.len(), 18, ...)` test to 19)
- Test: `crates/defra-agent-protocol/src/schemas.rs` (existing tests at the bottom — `all_contains_every_schema`, `collection_names_align_with_sdl_arrays`, `collection_names_are_unique`)

- [ ] **Step 1: Update the failing test count first**

In `crates/defra-agent-protocol/src/schemas.rs`, change:
```rust
fn all_contains_every_schema() {
    assert_eq!(
        ALL.len(),
        18,
        "ALL should enumerate every non-runtime schema"
    );
}
```
to:
```rust
fn all_contains_every_schema() {
    assert_eq!(
        ALL.len(),
        19,
        "ALL should enumerate every non-runtime schema"
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p defra-agent-protocol --lib schemas`
Expected: `all_contains_every_schema` FAILS with `left: 18, right: 19`; `collection_names_align_with_sdl_arrays` FAILS once the const arrays are updated (or already passes — depends on order). Stage just the first fail.

- [ ] **Step 3: Create the schema file**

Write `crates/defra-agent-protocol/schemas/agent/peer_pairing_desired.graphql`:
```graphql
type PeerPairingDesired {
    peer_id: String @index(unique: true)
    collections: [String!]!
    replicator_addresses: [String!]!
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
}
```

Note: no `@branchable` directive — desired state is a current-value document, not a historical one (mirrors `EventTrigger`'s structure since pairing-desired-state is operator-set state, not append-only history).

- [ ] **Step 4: Register the schema**

In `crates/defra-agent-protocol/src/schemas.rs` add to the agent-domain block (after `EVENT_TRIGGER`):
```rust
pub const PEER_PAIRING_DESIRED_NAME: &str = "PeerPairingDesired";
pub const PEER_PAIRING_DESIRED: &str =
    include_str!("../schemas/agent/peer_pairing_desired.graphql");
```

Append to `ALL`:
```rust
pub const ALL: &[&str] = &[
    AGENT_PRINCIPAL,
    // ... (existing) ...
    TOOL_SERVICE_REGISTRY,
    PEER_PAIRING_DESIRED,   // <-- add at end
];
```

Append to `ALL_COLLECTION_NAMES`:
```rust
pub const ALL_COLLECTION_NAMES: &[&str] = &[
    AGENT_PRINCIPAL_NAME,
    // ... (existing) ...
    TOOL_SERVICE_REGISTRY_NAME,
    PEER_PAIRING_DESIRED_NAME,   // <-- add at end
];
```

Do **not** add to `BRANCHABLE_COLLECTION_NAMES`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p defra-agent-protocol`
Expected: all tests pass, `ALL.len() == 19`, collection names unique, every schema starts with `type`.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-protocol/schemas/agent/peer_pairing_desired.graphql \
        crates/defra-agent-protocol/src/schemas.rs
git commit -m "Add PeerPairingDesired schema (#107)"
```

---

## Task 2: Define `RemoteP2pAdmin` trait + types

**Spec section:** §4.2 (1), §8 (15-op surface).

**Files:**
- Create: `crates/defra-agent-desktop-core/src/remote_admin/mod.rs`
- Create: `crates/defra-agent-desktop-core/src/remote_admin/trait_def.rs`
- Modify: `crates/defra-agent-desktop-core/src/lib.rs`
- Test: inline in `trait_def.rs` (compilation test only — pure trait definition)

- [ ] **Step 1: Write a trait-shape compile-time test**

Add to `crates/defra-agent-desktop-core/src/remote_admin/trait_def.rs` at the bottom:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_trait_object_safe(_: &dyn RemoteP2pAdmin) {}

    #[test]
    fn error_classes_are_distinct() {
        use RemoteP2pAdminError::*;
        let errors = [
            RpcTimeout,
            RpcError("x".into()),
            RemoteNotFound("c".into()),
            RemoteUnauthorized,
            LocalError("y".into()),
        ];
        // exhaustive match must compile (compile-time test)
        for e in &errors {
            match e {
                RpcTimeout => {}
                RpcError(_) => {}
                RemoteNotFound(_) => {}
                RemoteUnauthorized => {}
                LocalError(_) => {}
            }
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo build -p defra-agent-desktop-core`
Expected: FAIL with `cannot find type RemoteP2pAdmin` / `RemoteP2pAdminError` not in scope.

- [ ] **Step 3: Write the trait + types**

`crates/defra-agent-desktop-core/src/remote_admin/mod.rs`:
```rust
//! Transport-agnostic admin client for talking to a remote peer's P2P management
//! surface.
//!
//! Sibling to `defra_p2p_adapter::P2POperations`, which stays local-only.
//! `RemoteP2pAdmin` is the consumer-side trait the supervisor reconcile uses;
//! impls dispatch over HTTP today and over a future defradb.rs admin
//! CommChannel later. See `docs/superpowers/specs/2026-05-13-issue-107-p2p-admin-rpc-design.md`
//! §4.2 (1).

pub mod diff;
pub mod error_class;
pub mod http_impl;
pub mod trait_def;

pub use diff::{compute_pairing_diff, DiffOp, PairingActual, PairingDesired};
pub use error_class::{classify_remote_admin_error, PairingErrorClass};
pub use http_impl::HttpRemoteP2pAdmin;
pub use trait_def::{RemoteP2pAdmin, RemoteP2pAdminError, RemoteReplicator, RemoteP2pDocument};
```

`crates/defra-agent-desktop-core/src/remote_admin/trait_def.rs`:
```rust
//! `RemoteP2pAdmin` trait definition + error types + value types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors any `RemoteP2pAdmin` impl can produce. Categories chosen for the
/// classifier in `error_class::classify_remote_admin_error` and the
/// `PairingErrorClass` enum exposed on `ClientPeerStatus` (see spec §7.2).
#[derive(Debug, Error)]
pub enum RemoteP2pAdminError {
    #[error("remote admin RPC timed out")]
    RpcTimeout,

    #[error("remote admin RPC failed: {0}")]
    RpcError(String),

    #[error("remote reports not-found: {0}")]
    RemoteNotFound(String),

    #[error("remote admin rejected request as unauthorized")]
    RemoteUnauthorized,

    #[error("local error: {0}")]
    LocalError(String),
}

pub type RemoteP2pAdminResult<T> = Result<T, RemoteP2pAdminError>;

/// Subset of replicator info we care about for the reconcile diff. Wider than
/// strictly needed so callers can render replicator status without re-reading.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteReplicator {
    /// libp2p / iroh peer ID of the replicator target.
    pub id: Option<String>,
    /// Collection IDs this replicator pushes.
    pub collections: Vec<String>,
    /// Multiaddr; the form `/ip4/.../p2p/<peerid>` for libp2p, iroh-equivalent
    /// for iroh transports.
    pub address: Option<String>,
}

/// P2P document subscription record. Mirrors `P2pDocumentInfo` in
/// defradb.rs `crates/http/src/router/traits.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteP2pDocument {
    pub collection: String,
    pub doc_id: String,
}

/// Talking-to-a-remote-peer admin surface. One impl per transport; the
/// supervisor holds one `Arc<dyn RemoteP2pAdmin>` per saved peer.
///
/// Lifecycle: an impl is constructed with the peer's address(es) baked in
/// (`HttpRemoteP2pAdmin::new(graphql_url)`, future `P2pRemoteP2pAdmin::new(node_addr)`).
/// All methods operate against that one peer.
///
/// Auth: `RemoteP2pAdmin` impls do not currently sign requests
/// (#180 tracks closing this gap). When that lands, signing is an impl-level
/// concern; the trait surface is unchanged.
#[async_trait]
pub trait RemoteP2pAdmin: Send + Sync {
    // ---- peer ----

    /// Remote's peer info (its libp2p/iroh listen addresses + peer id).
    /// Maps to `GET /p2p/info`.
    async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>>;

    /// Remote's active peer connections.
    /// Maps to `GET /p2p/active-peers`.
    async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>>;

    /// Ask the remote to connect to the given multiaddrs.
    /// Maps to `POST /p2p/connect`.
    async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()>;

    // ---- replicators ----

    /// List the remote's installed replicators.
    /// Maps to `GET /p2p/replicators`.
    async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>>;

    /// Install a replicator on the remote pushing the named collections to
    /// the given multiaddrs.
    /// Maps to `POST /p2p/replicators`.
    async fn add_replicator(
        &self,
        addresses: &[String],
        collections: &[String],
    ) -> RemoteP2pAdminResult<()>;

    /// Remove a replicator (or specific collections) from the remote.
    /// Maps to `DELETE /p2p/replicators`.
    async fn delete_replicator(
        &self,
        id: &str,
        collections: &[String],
    ) -> RemoteP2pAdminResult<()>;

    // ---- p2p collections ----

    /// List the remote's installed P2P collection subscriptions.
    /// Maps to `GET /p2p/collections`.
    async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>>;

    /// Install P2P collection subscriptions on the remote.
    /// Maps to `POST /p2p/collections`.
    async fn add_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()>;

    /// Remove P2P collection subscriptions from the remote.
    /// Maps to `DELETE /p2p/collections`.
    async fn delete_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()>;

    // ---- p2p documents ----

    /// List the remote's per-document P2P subscriptions.
    /// Maps to `GET /p2p/documents`.
    async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>>;

    /// Install per-document P2P subscriptions on the remote.
    /// Maps to `POST /p2p/documents`.
    async fn add_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()>;

    /// Remove per-document P2P subscriptions from the remote.
    /// Maps to `DELETE /p2p/documents`.
    async fn delete_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()>;

    // ---- sync (control-plane triggers; not loop-driven in v1) ----

    /// Sync specific documents from the remote.
    /// Maps to `POST /p2p/documents/sync`.
    async fn sync_documents(
        &self,
        collection_name: &str,
        doc_ids: &[String],
        timeout: Option<std::time::Duration>,
    ) -> RemoteP2pAdminResult<()>;

    /// Sync collection version definitions from the remote.
    /// Maps to `POST /p2p/collections/sync-versions`.
    async fn sync_collection_versions(
        &self,
        version_ids: &[String],
        timeout: Option<std::time::Duration>,
    ) -> RemoteP2pAdminResult<()>;

    /// Sync a branchable collection's DAG from the remote.
    /// Maps to `POST /p2p/collections/sync-branchable`.
    async fn sync_branchable_collection(
        &self,
        collection_id: &str,
        timeout: Option<std::time::Duration>,
    ) -> RemoteP2pAdminResult<()>;
}
```

Modify `crates/defra-agent-desktop-core/src/lib.rs` to add at top-level:
```rust
pub mod remote_admin;
```

- [ ] **Step 4: Run tests to verify**

Run: `cargo build -p defra-agent-desktop-core && cargo test -p defra-agent-desktop-core --lib remote_admin::trait_def`
Expected: build succeeds, test `error_classes_are_distinct` passes.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-desktop-core/src/remote_admin/ \
        crates/defra-agent-desktop-core/src/lib.rs
git commit -m "Add RemoteP2pAdmin trait + types (#107)"
```

---

## Task 3: `HttpRemoteP2pAdmin` skeleton + collections triple

**Spec section:** §4.2 (2) + §8.

**Files:**
- Create: `crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs`
- Test: inline in `http_impl.rs` using `wiremock` (add dev-dep if not already present — check first: `grep wiremock crates/defra-agent-desktop-core/Cargo.toml`)

- [ ] **Step 1: Verify dev-dep**

Run: `grep wiremock crates/defra-agent-desktop-core/Cargo.toml`. If not present, add to `[dev-dependencies]`: `wiremock = "0.6"`.

- [ ] **Step 2: Write the three collections tests first**

`crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs`:
```rust
//! HTTP transport impl for `RemoteP2pAdmin`. Wraps reqwest calls against the
//! remote's `/p2p/*` surface. Replaces the bespoke client in
//! `local_runtime/pairing.rs`.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::trait_def::{
    RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteP2pDocument, RemoteReplicator,
};

const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(10);

pub struct HttpRemoteP2pAdmin {
    /// `http://host:port/api/v0` — the base used to compose `/p2p/*` URLs.
    api_base: String,
    client: Client,
}

impl HttpRemoteP2pAdmin {
    /// Construct against the remote's GraphQL URL (e.g. `http://localhost:9181/api/v0/graphql`).
    /// Strips the trailing `/graphql` to yield `/api/v0`.
    pub fn new(graphql_url: &str) -> RemoteP2pAdminResult<Self> {
        let trimmed = graphql_url.trim_end_matches('/');
        let api_base = trimmed
            .strip_suffix("/graphql")
            .ok_or_else(|| {
                RemoteP2pAdminError::LocalError(format!(
                    "expected GraphQL URL ending in /graphql, got {trimmed}"
                ))
            })?
            .to_string();
        let client = Client::builder()
            .timeout(DEFAULT_RPC_TIMEOUT)
            .build()
            .map_err(|e| RemoteP2pAdminError::LocalError(format!("reqwest build: {e}")))?;
        Ok(Self { api_base, client })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.api_base, path)
    }
}

// Internal request bodies. These match the existing HTTP surface 1:1.
// See `defradb/http/handler_p2p.go` and `defradb.rs/crates/http/src/router/traits.rs`.

#[derive(Debug, Serialize)]
struct AddReplicatorBody<'a> {
    #[serde(rename = "Collections")]
    collections: &'a [String],
    #[serde(rename = "Addresses")]
    addresses: &'a [String],
}

#[derive(Debug, Serialize)]
struct DeleteReplicatorBody<'a> {
    #[serde(rename = "Collections")]
    collections: &'a [String],
    #[serde(rename = "ID")]
    id: &'a str,
}

#[async_trait]
impl RemoteP2pAdmin for HttpRemoteP2pAdmin {
    async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
        unimplemented!("Task 5")
    }
    async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
        unimplemented!("Task 5")
    }
    async fn connect(&self, _addresses: &[String]) -> RemoteP2pAdminResult<()> {
        unimplemented!("Task 5")
    }

    async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
        unimplemented!("Task 4")
    }
    async fn add_replicator(
        &self,
        _addresses: &[String],
        _collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        unimplemented!("Task 4")
    }
    async fn delete_replicator(
        &self,
        _id: &str,
        _collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        unimplemented!("Task 4")
    }

    async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .client
            .get(self.url("/p2p/collections"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<Vec<String>>()
            .await
            .map_err(|e| RemoteP2pAdminError::RpcError(format!("decoding list response: {e}")))
    }

    async fn add_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .post(self.url("/p2p/collections"))
            .json(collections)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }

    async fn delete_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .delete(self.url("/p2p/collections"))
            .json(collections)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }

    async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> {
        unimplemented!("Task 6")
    }
    async fn add_p2p_documents(&self, _doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
        unimplemented!("Task 6")
    }
    async fn delete_p2p_documents(&self, _doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
        unimplemented!("Task 6")
    }

    async fn sync_documents(
        &self,
        _collection_name: &str,
        _doc_ids: &[String],
        _timeout: Option<std::time::Duration>,
    ) -> RemoteP2pAdminResult<()> {
        unimplemented!("Task 7")
    }
    async fn sync_collection_versions(
        &self,
        _version_ids: &[String],
        _timeout: Option<std::time::Duration>,
    ) -> RemoteP2pAdminResult<()> {
        unimplemented!("Task 7")
    }
    async fn sync_branchable_collection(
        &self,
        _collection_id: &str,
        _timeout: Option<std::time::Duration>,
    ) -> RemoteP2pAdminResult<()> {
        unimplemented!("Task 7")
    }
}

fn map_reqwest_err(e: reqwest::Error) -> RemoteP2pAdminError {
    if e.is_timeout() {
        RemoteP2pAdminError::RpcTimeout
    } else {
        RemoteP2pAdminError::RpcError(e.to_string())
    }
}

async fn check_status(resp: reqwest::Response) -> RemoteP2pAdminResult<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
    {
        Err(RemoteP2pAdminError::RemoteUnauthorized)
    } else if status == reqwest::StatusCode::NOT_FOUND {
        let body = resp.text().await.unwrap_or_default();
        Err(RemoteP2pAdminError::RemoteNotFound(body))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(RemoteP2pAdminError::RpcError(format!(
            "status {status}: {body}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn admin_for(server: &MockServer) -> HttpRemoteP2pAdmin {
        let graphql = format!("{}/api/v0/graphql", server.uri());
        HttpRemoteP2pAdmin::new(&graphql).expect("admin constructs")
    }

    #[tokio::test]
    async fn list_p2p_collections_returns_remote_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/collections"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec!["c1", "c2"]))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        let cols = admin.list_p2p_collections().await.expect("list");
        assert_eq!(cols, vec!["c1".to_string(), "c2".to_string()]);
    }

    #[tokio::test]
    async fn add_p2p_collections_posts_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/collections"))
            .and(body_json(vec!["c1", "c2"]))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .add_p2p_collections(&["c1".into(), "c2".into()])
            .await
            .expect("add");
    }

    #[tokio::test]
    async fn delete_p2p_collections_deletes_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v0/p2p/collections"))
            .and(body_json(vec!["c1"]))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .delete_p2p_collections(&["c1".into()])
            .await
            .expect("delete");
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p defra-agent-desktop-core --lib remote_admin::http_impl`
Expected: three tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs \
        crates/defra-agent-desktop-core/Cargo.toml
git commit -m "Add HttpRemoteP2pAdmin skeleton + collections triple (#107)"
```

---

## Task 4: HttpRemoteP2pAdmin — replicators triple

**Spec section:** §8.

**Files:**
- Modify: `crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs`

- [ ] **Step 1: Add three replicators tests at the bottom of the `tests` mod**

```rust
    #[tokio::test]
    async fn list_replicators_returns_remote_replicators() {
        let server = MockServer::start().await;
        let body = serde_json::json!([
            {"id": "peer1", "collections": ["c1"], "address": "/ip4/1.2.3.4/tcp/9000/p2p/peer1", "status": 1, "lastStatusChange": null}
        ]);
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/replicators"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        let reps = admin.list_replicators().await.expect("list");
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].id.as_deref(), Some("peer1"));
        assert_eq!(reps[0].collections, vec!["c1".to_string()]);
    }

    #[tokio::test]
    async fn add_replicator_posts_addresses_and_collections() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "Collections": ["c1"],
            "Addresses": ["/ip4/1.2.3.4/tcp/9000/p2p/peer1"]
        });
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/replicators"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .add_replicator(
                &["/ip4/1.2.3.4/tcp/9000/p2p/peer1".into()],
                &["c1".into()],
            )
            .await
            .expect("add_replicator");
    }

    #[tokio::test]
    async fn delete_replicator_deletes_with_id_and_collections() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "Collections": ["c1"],
            "ID": "peer1"
        });
        Mock::given(method("DELETE"))
            .and(path("/api/v0/p2p/replicators"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .delete_replicator("peer1", &["c1".into()])
            .await
            .expect("delete_replicator");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p defra-agent-desktop-core --lib remote_admin::http_impl`
Expected: three new tests FAIL with `unimplemented! Task 4`.

- [ ] **Step 3: Replace the three `unimplemented!("Task 4")` impls**

In `crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs`, replace the three replicator methods:
```rust
    async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
        #[derive(Deserialize)]
        struct WireReplicator {
            #[serde(default)]
            id: Option<String>,
            #[serde(default)]
            collections: Vec<String>,
            #[serde(default)]
            address: Option<String>,
        }
        let resp = self
            .client
            .get(self.url("/p2p/replicators"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        let wire: Vec<WireReplicator> = resp
            .json()
            .await
            .map_err(|e| RemoteP2pAdminError::RpcError(format!("decoding list_replicators: {e}")))?;
        Ok(wire
            .into_iter()
            .map(|w| RemoteReplicator {
                id: w.id,
                collections: w.collections,
                address: w.address,
            })
            .collect())
    }

    async fn add_replicator(
        &self,
        addresses: &[String],
        collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        let body = AddReplicatorBody {
            collections,
            addresses,
        };
        let resp = self
            .client
            .post(self.url("/p2p/replicators"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }

    async fn delete_replicator(
        &self,
        id: &str,
        collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        let body = DeleteReplicatorBody { collections, id };
        let resp = self
            .client
            .delete(self.url("/p2p/replicators"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p defra-agent-desktop-core --lib remote_admin::http_impl`
Expected: six tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs
git commit -m "HttpRemoteP2pAdmin: replicators triple (#107)"
```

---

## Task 5: HttpRemoteP2pAdmin — peer triple (info / active_peers / connect)

**Spec section:** §8.

**Files:**
- Modify: `crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs`

- [ ] **Step 1: Write three peer tests**

```rust
    #[tokio::test]
    async fn peer_info_returns_addresses() {
        let server = MockServer::start().await;
        let body = serde_json::json!(["/ip4/1.2.3.4/tcp/9000/p2p/peer1"]);
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        assert_eq!(
            admin.peer_info().await.expect("info"),
            vec!["/ip4/1.2.3.4/tcp/9000/p2p/peer1".to_string()]
        );
    }

    #[tokio::test]
    async fn active_peers_returns_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/active-peers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec!["peer1", "peer2"]))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        assert_eq!(
            admin.active_peers().await.expect("active"),
            vec!["peer1".to_string(), "peer2".to_string()]
        );
    }

    #[tokio::test]
    async fn connect_posts_addresses() {
        let server = MockServer::start().await;
        let expected = serde_json::json!(["/ip4/1.2.3.4/tcp/9000/p2p/peer1"]);
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/connect"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .connect(&["/ip4/1.2.3.4/tcp/9000/p2p/peer1".into()])
            .await
            .expect("connect");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p defra-agent-desktop-core --lib remote_admin::http_impl`
Expected: three new tests FAIL with `unimplemented! Task 5`.

- [ ] **Step 3: Implement the three peer methods**

Replace the `unimplemented!("Task 5")` bodies:
```rust
    async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .client
            .get(self.url("/p2p/info"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<Vec<String>>()
            .await
            .map_err(|e| RemoteP2pAdminError::RpcError(format!("decoding peer_info: {e}")))
    }

    async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .client
            .get(self.url("/p2p/active-peers"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<Vec<String>>()
            .await
            .map_err(|e| RemoteP2pAdminError::RpcError(format!("decoding active_peers: {e}")))
    }

    async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .post(self.url("/p2p/connect"))
            .json(addresses)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p defra-agent-desktop-core --lib remote_admin::http_impl
git add crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs
git commit -m "HttpRemoteP2pAdmin: peer triple (#107)"
```

---

## Task 6: HttpRemoteP2pAdmin — documents triple

**Spec section:** §8.

**Files:**
- Modify: `crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs`

- [ ] **Step 1: Write three documents tests**

```rust
    #[tokio::test]
    async fn list_p2p_documents_returns_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/documents"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec!["doc1", "doc2"]))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        assert_eq!(
            admin.list_p2p_documents().await.expect("list_docs"),
            vec!["doc1".to_string(), "doc2".to_string()]
        );
    }

    #[tokio::test]
    async fn add_p2p_documents_posts_doc_ids() {
        let server = MockServer::start().await;
        let expected = serde_json::json!(["doc1"]);
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/documents"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin.add_p2p_documents(&["doc1".into()]).await.expect("add");
    }

    #[tokio::test]
    async fn delete_p2p_documents_deletes_doc_ids() {
        let server = MockServer::start().await;
        let expected = serde_json::json!(["doc1"]);
        Mock::given(method("DELETE"))
            .and(path("/api/v0/p2p/documents"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .delete_p2p_documents(&["doc1".into()])
            .await
            .expect("delete");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p defra-agent-desktop-core --lib remote_admin::http_impl`
Expected: three new tests FAIL with `unimplemented! Task 6`.

- [ ] **Step 3: Implement**

Replace the `unimplemented!("Task 6")` bodies:
```rust
    async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .client
            .get(self.url("/p2p/documents"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<Vec<String>>()
            .await
            .map_err(|e| RemoteP2pAdminError::RpcError(format!("decoding list_p2p_documents: {e}")))
    }

    async fn add_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .post(self.url("/p2p/documents"))
            .json(doc_ids)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }

    async fn delete_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .delete(self.url("/p2p/documents"))
            .json(doc_ids)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p defra-agent-desktop-core --lib remote_admin::http_impl
git add crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs
git commit -m "HttpRemoteP2pAdmin: documents triple (#107)"
```

---

## Task 7: HttpRemoteP2pAdmin — sync triple

**Spec section:** §8. Sync ops are reachable via the trait but never driven from the reconcile loop in v1.

**Files:**
- Modify: `crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs`

- [ ] **Step 1: Write three sync tests**

```rust
    #[tokio::test]
    async fn sync_documents_posts_request_body() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "collectionName": "Foo",
            "docIDs": ["doc1"],
            "timeout": "5s"
        });
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/documents/sync"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .sync_documents("Foo", &["doc1".into()], Some(Duration::from_secs(5)))
            .await
            .expect("sync_documents");
    }

    #[tokio::test]
    async fn sync_collection_versions_posts_versions() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "versionIDs": ["v1"],
            "timeout": ""
        });
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/collections/sync-versions"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .sync_collection_versions(&["v1".into()], None)
            .await
            .expect("sync_collection_versions");
    }

    #[tokio::test]
    async fn sync_branchable_collection_posts_collection_id() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "collectionID": "col1",
            "timeout": ""
        });
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/collections/sync-branchable"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .sync_branchable_collection("col1", None)
            .await
            .expect("sync_branchable");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p defra-agent-desktop-core --lib remote_admin::http_impl`
Expected: three new tests FAIL with `unimplemented! Task 7`.

- [ ] **Step 3: Implement**

Add to `http_impl.rs` (above the impl block):
```rust
#[derive(Debug, Serialize)]
struct SyncDocumentsBody<'a> {
    #[serde(rename = "collectionName")]
    collection_name: &'a str,
    #[serde(rename = "docIDs")]
    doc_ids: &'a [String],
    timeout: String,
}

#[derive(Debug, Serialize)]
struct SyncVersionsBody<'a> {
    #[serde(rename = "versionIDs")]
    version_ids: &'a [String],
    timeout: String,
}

#[derive(Debug, Serialize)]
struct SyncBranchableBody<'a> {
    #[serde(rename = "collectionID")]
    collection_id: &'a str,
    timeout: String,
}

fn format_timeout(t: Option<Duration>) -> String {
    match t {
        Some(d) => format!("{}s", d.as_secs()),
        None => String::new(),
    }
}
```

Replace the `unimplemented!("Task 7")` bodies:
```rust
    async fn sync_documents(
        &self,
        collection_name: &str,
        doc_ids: &[String],
        timeout: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let body = SyncDocumentsBody {
            collection_name,
            doc_ids,
            timeout: format_timeout(timeout),
        };
        let resp = self
            .client
            .post(self.url("/p2p/documents/sync"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }

    async fn sync_collection_versions(
        &self,
        version_ids: &[String],
        timeout: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let body = SyncVersionsBody {
            version_ids,
            timeout: format_timeout(timeout),
        };
        let resp = self
            .client
            .post(self.url("/p2p/collections/sync-versions"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }

    async fn sync_branchable_collection(
        &self,
        collection_id: &str,
        timeout: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let body = SyncBranchableBody {
            collection_id,
            timeout: format_timeout(timeout),
        };
        let resp = self
            .client
            .post(self.url("/p2p/collections/sync-branchable"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let _ = check_status(resp).await?;
        Ok(())
    }
```

- [ ] **Step 4: Run + commit**

```bash
cargo test -p defra-agent-desktop-core --lib remote_admin::http_impl
git add crates/defra-agent-desktop-core/src/remote_admin/http_impl.rs
git commit -m "HttpRemoteP2pAdmin: sync triple (#107)"
```

---

## Task 8: Pure `compute_pairing_diff` function

**Spec section:** §4.2 (4) — diff logic; §6.3 — consumer idempotence follows from this diff.

**Files:**
- Create: `crates/defra-agent-desktop-core/src/remote_admin/diff.rs`
- Test: inline

- [ ] **Step 1: Write the failing tests**

```rust
//! Pure desired-vs-actual diff for pairing reconcile. Honors the
//! `applyInstall`/`applyTeardown` set semantics from
//! `Proofs/ReversePairingHandlers.lean` (#178).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Operator-set desired pairing for one peer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingDesired {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
}

/// Actual pairing state read from the remote.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingActual {
    pub collections: BTreeSet<String>,
    pub replicator_addresses: BTreeSet<String>,
}

/// One emit-an-RPC instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    InstallCollection(String),
    TeardownCollection(String),
    InstallReplicator(String),
    TeardownReplicator(String),
}

/// Diff in canonical sorted order: collections first (BTreeSet iteration is
/// ascending), then replicators. Deterministic output is essential for the
/// conformance harness.
pub fn compute_pairing_diff(desired: &PairingDesired, actual: &PairingActual) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    for c in desired.collections.difference(&actual.collections) {
        ops.push(DiffOp::InstallCollection(c.clone()));
    }
    for c in actual.collections.difference(&desired.collections) {
        ops.push(DiffOp::TeardownCollection(c.clone()));
    }
    for r in desired
        .replicator_addresses
        .difference(&actual.replicator_addresses)
    {
        ops.push(DiffOp::InstallReplicator(r.clone()));
    }
    for r in actual
        .replicator_addresses
        .difference(&desired.replicator_addresses)
    {
        ops.push(DiffOp::TeardownReplicator(r.clone()));
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(xs: &[&str]) -> BTreeSet<String> {
        xs.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn empty_inputs_yield_no_ops() {
        let ops = compute_pairing_diff(&PairingDesired::default(), &PairingActual::default());
        assert!(ops.is_empty());
    }

    #[test]
    fn missing_collection_yields_install() {
        let desired = PairingDesired {
            collections: s(&["c1"]),
            ..Default::default()
        };
        let actual = PairingActual::default();
        assert_eq!(
            compute_pairing_diff(&desired, &actual),
            vec![DiffOp::InstallCollection("c1".into())]
        );
    }

    #[test]
    fn extra_collection_yields_teardown() {
        let desired = PairingDesired::default();
        let actual = PairingActual {
            collections: s(&["c1"]),
            ..Default::default()
        };
        assert_eq!(
            compute_pairing_diff(&desired, &actual),
            vec![DiffOp::TeardownCollection("c1".into())]
        );
    }

    #[test]
    fn same_state_yields_no_ops() {
        // Idempotence: once converged, no further RPCs emitted.
        let desired = PairingDesired {
            collections: s(&["c1"]),
            replicator_addresses: s(&["/ip4/1/p2p/p"]),
        };
        let actual = PairingActual {
            collections: s(&["c1"]),
            replicator_addresses: s(&["/ip4/1/p2p/p"]),
        };
        assert!(compute_pairing_diff(&desired, &actual).is_empty());
    }

    #[test]
    fn collections_diff_emits_before_replicators_diff() {
        let desired = PairingDesired {
            collections: s(&["c1"]),
            replicator_addresses: s(&["/ip4/1/p2p/p"]),
        };
        let actual = PairingActual::default();
        let ops = compute_pairing_diff(&desired, &actual);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], DiffOp::InstallCollection(_)));
        assert!(matches!(ops[1], DiffOp::InstallReplicator(_)));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p defra-agent-desktop-core --lib remote_admin::diff`
Expected: file doesn't compile (`diff` module not in `mod.rs`).

- [ ] **Step 3: Verify the module is exported**

`crates/defra-agent-desktop-core/src/remote_admin/mod.rs` already lists `pub mod diff;` from Task 2. If not, add it.

- [ ] **Step 4: Run tests**

Run: `cargo test -p defra-agent-desktop-core --lib remote_admin::diff`
Expected: five tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-desktop-core/src/remote_admin/diff.rs
git commit -m "Add compute_pairing_diff (#107)"
```

---

## Task 9: `PairingErrorClass` classifier

**Spec section:** §7.2.

**Files:**
- Create: `crates/defra-agent-desktop-core/src/remote_admin/error_class.rs`

- [ ] **Step 1: Write the failing tests**

```rust
//! Classify `RemoteP2pAdminError` into the tight enum exposed on
//! `ClientPeerStatus.pairing[].last_retry_error_class`.

use serde::{Deserialize, Serialize};

use super::trait_def::RemoteP2pAdminError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingErrorClass {
    RpcTimeout,
    RpcError,
    RemoteNotFound,
    RemoteUnauthorized,
    LocalError,
}

pub fn classify_remote_admin_error(err: &RemoteP2pAdminError) -> PairingErrorClass {
    match err {
        RemoteP2pAdminError::RpcTimeout => PairingErrorClass::RpcTimeout,
        RemoteP2pAdminError::RpcError(_) => PairingErrorClass::RpcError,
        RemoteP2pAdminError::RemoteNotFound(_) => PairingErrorClass::RemoteNotFound,
        RemoteP2pAdminError::RemoteUnauthorized => PairingErrorClass::RemoteUnauthorized,
        RemoteP2pAdminError::LocalError(_) => PairingErrorClass::LocalError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_maps_distinctly() {
        let pairs = [
            (RemoteP2pAdminError::RpcTimeout, PairingErrorClass::RpcTimeout),
            (
                RemoteP2pAdminError::RpcError("x".into()),
                PairingErrorClass::RpcError,
            ),
            (
                RemoteP2pAdminError::RemoteNotFound("c".into()),
                PairingErrorClass::RemoteNotFound,
            ),
            (
                RemoteP2pAdminError::RemoteUnauthorized,
                PairingErrorClass::RemoteUnauthorized,
            ),
            (
                RemoteP2pAdminError::LocalError("y".into()),
                PairingErrorClass::LocalError,
            ),
        ];
        for (err, class) in pairs {
            assert_eq!(classify_remote_admin_error(&err), class);
        }
    }
}
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p defra-agent-desktop-core --lib remote_admin::error_class
git add crates/defra-agent-desktop-core/src/remote_admin/error_class.rs
git commit -m "Add PairingErrorClass classifier (#107)"
```

---

## Task 10: Extend `ClientPeerStatus` with pairing sub-status

**Spec section:** §7.1.

**Files:**
- Modify: `crates/defra-agent-desktop-core/src/client/core.rs` (the struct lives here per the existing layout — verify with `grep -rn 'struct ClientPeerStatus' crates/defra-agent-desktop-core/src/` if uncertain)

- [ ] **Step 1: Write a unit test that constructs the new sub-status**

In the same file or its sibling `crates/defra-agent-desktop-core/src/client/core/tests.rs` (create if not present):
```rust
#[cfg(test)]
mod pairing_status_tests {
    use super::*;
    use std::time::SystemTime;
    use crate::remote_admin::PairingErrorClass;

    #[test]
    fn pairing_collection_status_default_is_clean() {
        let s = PairingCollectionStatus::new("c1");
        assert_eq!(s.collection_id, "c1");
        assert_eq!(s.pairing_retry_count, 0);
        assert!(s.last_retry_at.is_none());
        assert!(s.last_retry_error_class.is_none());
        assert!(s.stuck_since.is_none());
    }

    #[test]
    fn client_peer_status_carries_pairing_vec() {
        let peer_status = ClientPeerStatus {
            peer_id: "p1".into(),
            label: "l".into(),
            agent_did: None,
            addr: "/ip4/1/p2p/p1".into(),
            dial_succeeded: true,
            last_error: None,
            pairing: vec![PairingCollectionStatus::new("c1")],
        };
        assert_eq!(peer_status.pairing.len(), 1);
    }

    #[test]
    fn record_retry_increments_and_classifies() {
        let mut s = PairingCollectionStatus::new("c1");
        let before = SystemTime::now();
        s.record_retry(PairingErrorClass::RpcTimeout);
        assert_eq!(s.pairing_retry_count, 1);
        assert!(s.last_retry_at.unwrap() >= before);
        assert_eq!(s.last_retry_error_class, Some(PairingErrorClass::RpcTimeout));
    }

    #[test]
    fn mark_stuck_after_threshold() {
        let mut s = PairingCollectionStatus::new("c1");
        for _ in 0..STUCK_THRESHOLD_ATTEMPTS {
            s.record_retry(PairingErrorClass::RpcTimeout);
        }
        s.update_stuck_indicator(SystemTime::now());
        assert!(s.stuck_since.is_some());
    }

    #[test]
    fn record_success_clears_state() {
        let mut s = PairingCollectionStatus::new("c1");
        s.record_retry(PairingErrorClass::RpcError);
        s.record_success();
        assert_eq!(s.pairing_retry_count, 0);
        assert!(s.last_retry_error_class.is_none());
        assert!(s.stuck_since.is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p defra-agent-desktop-core --lib pairing_status_tests`
Expected: FAIL with `cannot find PairingCollectionStatus`.

- [ ] **Step 3: Add the struct + methods**

In `crates/defra-agent-desktop-core/src/client/core.rs` near the existing `ClientPeerStatus` definition, add:
```rust
use std::time::{Duration, SystemTime};

use crate::remote_admin::PairingErrorClass;

/// Per-peer-per-collection reconcile retry state, in-memory v1.
/// See spec §7.1, §7.2. Lost on restart by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCollectionStatus {
    pub collection_id: String,
    pub pairing_retry_count: u32,
    pub last_retry_at: Option<SystemTime>,
    pub last_retry_error_class: Option<PairingErrorClass>,
    pub stuck_since: Option<SystemTime>,
    /// When the *first* failure for this peer/collection happened in the
    /// current failure streak. Used by `update_stuck_indicator` to drive
    /// `STUCK_THRESHOLD_DURATION`. Reset by `record_success`.
    first_failure_at: Option<SystemTime>,
}

pub const RPC_TIMEOUT: Duration = Duration::from_secs(10);
pub const STUCK_THRESHOLD_ATTEMPTS: u32 = 6;
pub const STUCK_THRESHOLD_DURATION: Duration = Duration::from_secs(5 * 60);

impl PairingCollectionStatus {
    pub fn new(collection_id: impl Into<String>) -> Self {
        Self {
            collection_id: collection_id.into(),
            pairing_retry_count: 0,
            last_retry_at: None,
            last_retry_error_class: None,
            stuck_since: None,
            first_failure_at: None,
        }
    }

    pub fn record_retry(&mut self, class: PairingErrorClass) {
        let now = SystemTime::now();
        self.pairing_retry_count = self.pairing_retry_count.saturating_add(1);
        self.last_retry_at = Some(now);
        self.last_retry_error_class = Some(class);
        if self.first_failure_at.is_none() {
            self.first_failure_at = Some(now);
        }
    }

    pub fn record_success(&mut self) {
        self.pairing_retry_count = 0;
        self.last_retry_at = None;
        self.last_retry_error_class = None;
        self.stuck_since = None;
        self.first_failure_at = None;
    }

    /// Flip `stuck_since` to `Some(now)` when either threshold fires.
    /// Idempotent: leaves an already-set `stuck_since` alone.
    pub fn update_stuck_indicator(&mut self, now: SystemTime) {
        if self.stuck_since.is_some() {
            return;
        }
        let attempts_trigger = self.pairing_retry_count >= STUCK_THRESHOLD_ATTEMPTS;
        let duration_trigger = self
            .first_failure_at
            .and_then(|first| now.duration_since(first).ok())
            .map(|elapsed| elapsed >= STUCK_THRESHOLD_DURATION)
            .unwrap_or(false);
        if attempts_trigger || duration_trigger {
            self.stuck_since = Some(now);
        }
    }
}
```

Add the `pairing` field to `ClientPeerStatus`:
```rust
pub struct ClientPeerStatus {
    pub peer_id: String,
    pub label: String,
    pub agent_did: Option<String>,
    pub addr: String,
    pub dial_succeeded: bool,
    pub last_error: Option<String>,
    /// Per-collection pairing reconcile retry state. v1 in-memory only.
    pub pairing: Vec<PairingCollectionStatus>,
}
```

Update any construction site of `ClientPeerStatus` to set `pairing: Vec::new()`. Use `grep -rn 'ClientPeerStatus {' crates/defra-agent-desktop-core/src/` to find them.

- [ ] **Step 4: Run tests + commit**

```bash
cargo test -p defra-agent-desktop-core --lib pairing_status_tests
cargo build -p defra-agent-desktop-core   # ensure all callers compile
git add crates/defra-agent-desktop-core/src/client/core.rs \
        crates/defra-agent-desktop-core/src/client/core/  # if other call sites changed
git commit -m "Extend ClientPeerStatus with pairing sub-status (#107)"
```

---

## Task 11: Wire reconcile into supervisor tick (feature-flagged)

**Spec section:** §4.2 (4), §9 (feature flag).

**Files:**
- Modify: `crates/defra-agent-desktop-core/src/client/core/supervisor.rs`

The supervisor already exists. The integration extends `run_saved_peer_repair_cycle` with desired-vs-actual diff + RPC emission, gated on `DEFRA_AGENT_PAIRING_RECONCILE=1`.

- [ ] **Step 1: Write an integration test that drives the supervisor with a stub `RemoteP2pAdmin`**

Create `crates/defra-agent-desktop-core/src/client/core/supervisor_pairing_tests.rs` (or inline at the bottom of supervisor.rs):
```rust
#[cfg(test)]
mod pairing_reconcile_tests {
    use super::*;
    use crate::remote_admin::{
        DiffOp, PairingActual, PairingDesired, RemoteP2pAdmin, RemoteP2pAdminError,
        RemoteP2pAdminResult, RemoteReplicator,
    };
    use async_trait::async_trait;
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::time::Duration;

    struct StubRemoteAdmin {
        installed_collections: Mutex<BTreeSet<String>>,
        installed_replicators: Mutex<BTreeSet<String>>,
        emitted: Mutex<Vec<DiffOp>>,
    }

    impl StubRemoteAdmin {
        fn new() -> Self {
            Self {
                installed_collections: Mutex::new(BTreeSet::new()),
                installed_replicators: Mutex::new(BTreeSet::new()),
                emitted: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl RemoteP2pAdmin for StubRemoteAdmin {
        async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> { Ok(vec![]) }
        async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> { Ok(vec![]) }
        async fn connect(&self, _addresses: &[String]) -> RemoteP2pAdminResult<()> { Ok(()) }
        async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
            let reps = self.installed_replicators.lock().unwrap();
            Ok(reps
                .iter()
                .map(|addr| RemoteReplicator {
                    id: None,
                    collections: vec![],
                    address: Some(addr.clone()),
                })
                .collect())
        }
        async fn add_replicator(
            &self,
            addresses: &[String],
            _collections: &[String],
        ) -> RemoteP2pAdminResult<()> {
            for a in addresses {
                self.installed_replicators.lock().unwrap().insert(a.clone());
                self.emitted
                    .lock()
                    .unwrap()
                    .push(DiffOp::InstallReplicator(a.clone()));
            }
            Ok(())
        }
        async fn delete_replicator(
            &self,
            _id: &str,
            _collections: &[String],
        ) -> RemoteP2pAdminResult<()> {
            Ok(())
        }
        async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
            Ok(self.installed_collections.lock().unwrap().iter().cloned().collect())
        }
        async fn add_p2p_collections(&self, cols: &[String]) -> RemoteP2pAdminResult<()> {
            for c in cols {
                self.installed_collections.lock().unwrap().insert(c.clone());
                self.emitted.lock().unwrap().push(DiffOp::InstallCollection(c.clone()));
            }
            Ok(())
        }
        async fn delete_p2p_collections(&self, cols: &[String]) -> RemoteP2pAdminResult<()> {
            for c in cols {
                self.installed_collections.lock().unwrap().remove(c);
                self.emitted.lock().unwrap().push(DiffOp::TeardownCollection(c.clone()));
            }
            Ok(())
        }
        async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> { Ok(vec![]) }
        async fn add_p2p_documents(&self, _d: &[String]) -> RemoteP2pAdminResult<()> { Ok(()) }
        async fn delete_p2p_documents(&self, _d: &[String]) -> RemoteP2pAdminResult<()> { Ok(()) }
        async fn sync_documents(
            &self,
            _c: &str,
            _d: &[String],
            _t: Option<Duration>,
        ) -> RemoteP2pAdminResult<()> { Ok(()) }
        async fn sync_collection_versions(
            &self,
            _v: &[String],
            _t: Option<Duration>,
        ) -> RemoteP2pAdminResult<()> { Ok(()) }
        async fn sync_branchable_collection(
            &self,
            _c: &str,
            _t: Option<Duration>,
        ) -> RemoteP2pAdminResult<()> { Ok(()) }
    }

    #[tokio::test]
    async fn diff_drives_install_and_no_op_after_convergence() {
        let stub = StubRemoteAdmin::new();
        let desired = PairingDesired {
            collections: ["c1", "c2"].iter().map(|s| s.to_string()).collect(),
            replicator_addresses: ["/ip4/1/p2p/p"].iter().map(|s| s.to_string()).collect(),
        };

        // First tick — divergence, install fires.
        let actual_1 = read_remote_actual(&stub).await;
        let ops_1 = crate::remote_admin::compute_pairing_diff(&desired, &actual_1);
        apply_ops(&stub, &ops_1).await;
        assert_eq!(ops_1.len(), 3);

        // Second tick — already converged, no-op.
        let actual_2 = read_remote_actual(&stub).await;
        let ops_2 = crate::remote_admin::compute_pairing_diff(&desired, &actual_2);
        assert!(ops_2.is_empty());
    }

    async fn read_remote_actual(stub: &StubRemoteAdmin) -> PairingActual {
        let cols = stub.list_p2p_collections().await.unwrap();
        let reps = stub.list_replicators().await.unwrap();
        PairingActual {
            collections: cols.into_iter().collect(),
            replicator_addresses: reps.into_iter().filter_map(|r| r.address).collect(),
        }
    }

    async fn apply_ops(stub: &StubRemoteAdmin, ops: &[DiffOp]) {
        for op in ops {
            match op {
                DiffOp::InstallCollection(c) => {
                    stub.add_p2p_collections(&[c.clone()]).await.unwrap();
                }
                DiffOp::TeardownCollection(c) => {
                    stub.delete_p2p_collections(&[c.clone()]).await.unwrap();
                }
                DiffOp::InstallReplicator(r) => {
                    stub.add_replicator(&[r.clone()], &[]).await.unwrap();
                }
                DiffOp::TeardownReplicator(_) => {}
            }
        }
    }
}
```

- [ ] **Step 2: Run to verify the diff/read/apply harness logic works**

Run: `cargo test -p defra-agent-desktop-core --lib pairing_reconcile_tests`
Expected: `diff_drives_install_and_no_op_after_convergence` passes.

- [ ] **Step 3: Add the reconcile pass to `run_saved_peer_repair_cycle`**

In `supervisor.rs`, near the top of the file add:
```rust
use crate::remote_admin::{
    classify_remote_admin_error, compute_pairing_diff, DiffOp, HttpRemoteP2pAdmin,
    PairingActual, PairingDesired, RemoteP2pAdmin,
};
```

After the existing `run_saved_peer_repair_cycle` body's `repair_saved_peer` call but before the loop continues, add a feature-flagged pairing reconcile pass:
```rust
fn pairing_reconcile_enabled() -> bool {
    std::env::var("DEFRA_AGENT_PAIRING_RECONCILE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

async fn run_pairing_reconcile_for_peer(
    record: &PeerRecord,
    desired: PairingDesired,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
) {
    let Some(graphql_url) = record.graphql.as_deref() else {
        return; // P2P-only peer, no transport available yet (waits for Task 7-like P2pRemoteP2pAdmin)
    };
    let admin = match HttpRemoteP2pAdmin::new(graphql_url) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(
                target: "defra_agent_desktop_core::pairing_reconcile",
                peer = %record.label,
                error = %e,
                "failed to construct HttpRemoteP2pAdmin"
            );
            return;
        }
    };

    let actual_collections = match admin.list_p2p_collections().await {
        Ok(c) => c.into_iter().collect(),
        Err(e) => {
            record_failure(record, peer_statuses, &desired, &e);
            return;
        }
    };
    let actual_replicators = match admin.list_replicators().await {
        Ok(reps) => reps.into_iter().filter_map(|r| r.address).collect(),
        Err(e) => {
            record_failure(record, peer_statuses, &desired, &e);
            return;
        }
    };
    let actual = PairingActual {
        collections: actual_collections,
        replicator_addresses: actual_replicators,
    };
    let ops = compute_pairing_diff(&desired, &actual);

    for op in &ops {
        let result = match op {
            DiffOp::InstallCollection(c) => admin.add_p2p_collections(&[c.clone()]).await,
            DiffOp::TeardownCollection(c) => admin.delete_p2p_collections(&[c.clone()]).await,
            DiffOp::InstallReplicator(r) => {
                admin.add_replicator(&[r.clone()], &desired.collections.iter().cloned().collect::<Vec<_>>()).await
            }
            DiffOp::TeardownReplicator(_) => Ok(()), // v1 leaves replicator teardown to operator UX
        };
        match result {
            Ok(()) => record_success_for_op(record, peer_statuses, op),
            Err(e) => {
                record_failure_for_op(record, peer_statuses, op, &e);
                break; // stop this tick; next tick re-evaluates
            }
        }
    }
}

fn record_failure(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    desired: &PairingDesired,
    err: &crate::remote_admin::RemoteP2pAdminError,
) {
    let class = classify_remote_admin_error(err);
    let mut statuses = peer_statuses.write().expect("peer_statuses poisoned");
    let Some(status) = statuses.iter_mut().find(|s| s.peer_id == record.peer_id) else {
        return;
    };
    // List failed: charge every desired-collection sub-status with the error.
    for col in desired.collections.iter() {
        let sub = ensure_pairing_status(status, col);
        sub.record_retry(class);
        sub.update_stuck_indicator(std::time::SystemTime::now());
    }
}

fn record_success_for_op(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    op: &DiffOp,
) {
    let Some(target_id) = op_collection_id(op) else { return };
    let mut statuses = peer_statuses.write().expect("peer_statuses poisoned");
    let Some(status) = statuses.iter_mut().find(|s| s.peer_id == record.peer_id) else {
        return;
    };
    let sub = ensure_pairing_status(status, target_id);
    sub.record_success();
}

fn record_failure_for_op(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    op: &DiffOp,
    err: &crate::remote_admin::RemoteP2pAdminError,
) {
    let Some(target_id) = op_collection_id(op) else { return };
    let class = classify_remote_admin_error(err);
    let mut statuses = peer_statuses.write().expect("peer_statuses poisoned");
    let Some(status) = statuses.iter_mut().find(|s| s.peer_id == record.peer_id) else {
        return;
    };
    let sub = ensure_pairing_status(status, target_id);
    sub.record_retry(class);
    sub.update_stuck_indicator(std::time::SystemTime::now());
}

fn ensure_pairing_status<'a>(
    status: &'a mut ClientPeerStatus,
    target_id: &str,
) -> &'a mut PairingCollectionStatus {
    if let Some(pos) = status
        .pairing
        .iter()
        .position(|s| s.collection_id == target_id)
    {
        &mut status.pairing[pos]
    } else {
        status.pairing.push(PairingCollectionStatus::new(target_id));
        status.pairing.last_mut().unwrap()
    }
}

fn op_collection_id(op: &DiffOp) -> Option<&str> {
    match op {
        DiffOp::InstallCollection(c) | DiffOp::TeardownCollection(c) => Some(c),
        DiffOp::InstallReplicator(r) | DiffOp::TeardownReplicator(r) => Some(r),
    }
}
```

Then inside `run_saved_peer_repair_cycle` add an invocation, gated on the flag, after the existing `repair_saved_peer` call:
```rust
        // ... existing repair_saved_peer logic above ...

        if pairing_reconcile_enabled() {
            let desired = load_desired_for_peer(&record).await;
            run_pairing_reconcile_for_peer(&record, desired, peer_statuses).await;
        }
```

`load_desired_for_peer` is a placeholder reading from the new `PeerPairingDesired` collection. For Task 11, stub it as:
```rust
async fn load_desired_for_peer(record: &PeerRecord) -> PairingDesired {
    // TODO Task 12: replace with a real DefraDB query against PeerPairingDesired.
    PairingDesired {
        collections: crate::client::schema::subscribed_collection_names()
            .iter()
            .map(|s| s.to_string())
            .collect(),
        replicator_addresses: [record.addr.clone()].into_iter().collect(),
    }
}
```

The stub uses the existing default-collection set + the peer's known addr so the supervisor has *something* to reconcile against. Task 12 swaps in the real query.

- [ ] **Step 4: Run + commit**

```bash
cargo test -p defra-agent-desktop-core --lib pairing_reconcile_tests
cargo build -p defra-agent-desktop-core
git add crates/defra-agent-desktop-core/src/client/core/supervisor.rs
git commit -m "Wire pairing reconcile into supervisor tick (#107)"
```

---

## Task 12: Read `PeerPairingDesired` from DefraDB + replace `pairing.rs` callers

**Spec section:** §6.2, §9.

**Files:**
- Modify: `crates/defra-agent-desktop-core/src/client/core/supervisor.rs` (real query)
- Modify: `crates/defra-agent-desktop-core/src/client/core/bootstrap.rs` (replace `configure_local_runtime_pairing`)

- [ ] **Step 1: Write an integration test that writes a `PeerPairingDesired` doc and reads it back**

Add to `crates/defra-agent/tests/peer_pairing_desired_query.rs` (new file):
```rust
//! Integration test for reading PeerPairingDesired docs via the runtime
//! GraphQL surface. Requires an embedded DefraDB node; mirrors the test_db
//! pattern in tests/support/.

mod support;

use defra_agent::defra_node::EmbeddedNode;
use support::test_db;

#[tokio::test]
async fn peer_pairing_desired_round_trip() {
    let (node, _paths) = test_db().await;
    // Ensure schema is registered (test_db calls ensure_runtime_schemas; verify
    // PeerPairingDesired is included).
    let create = r#"mutation {
        create_PeerPairingDesired(input: {
            peer_id: "p1",
            collections: ["c1","c2"],
            replicator_addresses: ["/ip4/1/p2p/p1"],
            created_at: "2026-05-13T00:00:00Z",
            updated_at: "2026-05-13T00:00:00Z"
        }) { peer_id collections }
    }"#;
    let result = node.execute(create.into(), None).await.expect("create");
    assert!(result.errors.is_empty(), "create errors: {:?}", result.errors);

    let query = r#"query { PeerPairingDesired(filter: {peer_id: {_eq: "p1"}}) { peer_id collections replicator_addresses } }"#;
    let result = node.execute(query.into(), None).await.expect("query");
    assert!(result.errors.is_empty(), "query errors: {:?}", result.errors);
    let data = result.data.expect("data");
    assert!(data.to_string().contains("c1"));
}
```

- [ ] **Step 2: Run to verify (likely passes already once schema is registered)**

Run: `cargo test -p defra-agent --test peer_pairing_desired_query`
Expected: PASS if Task 1's schema registration is correct. If the test_db helper's `ensure_runtime_schemas` doesn't register the new schema, debug there.

- [ ] **Step 3: Replace the stub `load_desired_for_peer` with a real query**

In `crates/defra-agent-desktop-core/src/client/core/supervisor.rs`, replace the stub with:
```rust
async fn load_desired_for_peer(
    node: &Arc<EmbeddedNode>,
    record: &PeerRecord,
) -> PairingDesired {
    use defra_agent::graphql::escape_graphql_string;
    let query = format!(
        r#"query {{ PeerPairingDesired(filter: {{peer_id: {{_eq: "{}"}}}}) {{ collections replicator_addresses }} }}"#,
        escape_graphql_string(&record.peer_id),
    );
    let result = match node.execute(query.into(), None).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "defra_agent_desktop_core::pairing_reconcile",
                peer = %record.label,
                error = %e,
                "PeerPairingDesired query failed; falling back to empty desired"
            );
            return PairingDesired::default();
        }
    };
    // Parse a single-row result. Implementation note: node.execute returns a
    // serde_json::Value; the structure is {"PeerPairingDesired": [{...}]}.
    let row = result
        .data
        .as_ref()
        .and_then(|d| d.get("PeerPairingDesired"))
        .and_then(|arr| arr.get(0))
        .cloned();
    let Some(row) = row else { return PairingDesired::default() };
    let collections = row
        .get("collections")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    let replicator_addresses = row
        .get("replicator_addresses")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    PairingDesired {
        collections,
        replicator_addresses,
    }
}
```

Thread `node: Arc<EmbeddedNode>` through the supervisor task spawn — it should already have a node handle via `Arc<EmbeddedNode>` in `ClientCore::start_with_paths_and_options`.

- [ ] **Step 4: Replace `pairing.rs` HTTP one-shot with the new reconcile path**

In `crates/defra-agent-desktop-core/src/client/core/bootstrap.rs`, locate `configure_local_runtime_pairing`. Behind the same feature flag, write `PeerPairingDesired` instead of calling the one-shot `pairing.rs`:

```rust
async fn configure_local_runtime_pairing(
    node: &Arc<EmbeddedNode>,
    p2p: &Arc<dyn P2POps>,
    record: &PeerRecord,
) -> Result<()> {
    if pairing_reconcile_enabled() {
        write_peer_pairing_desired(node, record).await?;
        // Supervisor tick will pick up the doc and converge.
        Ok(())
    } else {
        // Legacy path: existing pairing.rs.
        crate::local_runtime::pairing::complete_runtime_pairing(
            record.graphql.as_deref().expect("requires graphql"),
            /* desktop_listen_address */ &p2p_listen_addresses_first(p2p).await?,
            crate::client::schema::subscribed_collection_names()
                .into_iter()
                .map(str::to_owned)
                .collect(),
        )
        .await
    }
}

async fn write_peer_pairing_desired(
    node: &Arc<EmbeddedNode>,
    record: &PeerRecord,
) -> Result<()> {
    use defra_agent::graphql::escape_graphql_string;
    let collections = crate::client::schema::subscribed_collection_names()
        .iter()
        .map(|s| format!(r#""{}""#, escape_graphql_string(s)))
        .collect::<Vec<_>>()
        .join(",");
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{ create_PeerPairingDesired(input: {{
            peer_id: "{}",
            collections: [{}],
            replicator_addresses: ["{}"],
            created_at: "{}",
            updated_at: "{}"
        }}) {{ peer_id }} }}"#,
        escape_graphql_string(&record.peer_id),
        collections,
        escape_graphql_string(&record.addr),
        now,
        now,
    );
    let result = node.execute(mutation.into(), None).await
        .map_err(|e| anyhow::anyhow!("create PeerPairingDesired: {e}"))?;
    if !result.errors.is_empty() {
        return Err(anyhow::anyhow!("PeerPairingDesired errors: {:?}", result.errors));
    }
    Ok(())
}
```

(Handle the upsert/update case: if a doc already exists for `peer_id`, attempt an update mutation instead. Implementation detail; the test below covers the create case.)

- [ ] **Step 5: Run + commit**

```bash
cargo test -p defra-agent --test peer_pairing_desired_query
cargo test -p defra-agent-desktop-core --lib pairing_reconcile_tests
cargo build -p defra-agent-desktop-core
git add crates/defra-agent-desktop-core/src/client/core/supervisor.rs \
        crates/defra-agent-desktop-core/src/client/core/bootstrap.rs \
        crates/defra-agent/tests/peer_pairing_desired_query.rs
git commit -m "Read PeerPairingDesired from DefraDB; flag-gate pairing.rs replacement (#107)"
```

---

## Task 13: Lean `Proofs/PairingReconcile/State.lean`

**Spec section:** §4.2 (6) — mandatory Lean module.

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/PairingReconcile/State.lean`

Prior art to model after: `crates/defra-agent/proofs/Proofs/RuntimeReconcile/State.lean`. The supervisor's reconcile state machine is simpler than `RuntimeReconcile` — there's no generation/router-visibility coupling, just per-peer desired-vs-actual diff cycles.

- [ ] **Step 1: Write the state file**

```lean
import Proofs.Basic
import Proofs.ApplyReconcile.Collections
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card

/-!
# Pairing Reconcile State

Per-peer pairing reconcile state for the defra-agent supervisor. Each tick
reads desired and actual from the peer; diff drives RPC emission. This file
models the persisted state shape; transitions are in `Transition.lean`.

This is the consumer-side counterpart to `Proofs/ReversePairingHandlers.lean`
(receiver-side) and to the TLA+ spec at
`crates/defra-agent/proofs/tla/ReversePairing.tla`.
-/

namespace PairingReconcile

/-- A peer identifier as the supervisor sees it. -/
abbrev PeerId := String

/-- Collection vocabulary shared with the apply-reconcile model. -/
abbrev Collection := ApplyReconcile.Collection

/-- Per-peer-per-collection retry state. Visibility, not correctness. -/
structure PairingCollectionStatus where
  collectionId  : String
  retryCount    : Nat
  stuck         : Bool
  deriving DecidableEq, Repr

/-- Operator-set desired pairing for one peer. -/
structure PairingDesired where
  collections : Finset String
  deriving DecidableEq

/-- Remote-observed actual pairing for one peer. -/
structure PairingActual where
  collections : Finset String
  deriving DecidableEq

/-- One emit-an-RPC instruction (matches Rust `DiffOp`). -/
inductive DiffOp where
  | installCollection (c : String)
  | teardownCollection (c : String)
  deriving DecidableEq, Repr

/-- Full reconcile state: desired, actual, and the per-collection visibility
records. -/
structure ReconcileState where
  peer       : PeerId
  desired    : PairingDesired
  actual     : PairingActual
  pairing    : List PairingCollectionStatus
  deriving DecidableEq

namespace ReconcileState

/-- A reconcile state is *converged* when desired and actual agree on
collections. The visibility records are not part of the safety property —
they're for operator UX. -/
def converged (s : ReconcileState) : Prop :=
  s.desired.collections = s.actual.collections

instance (s : ReconcileState) : Decidable s.converged := by
  unfold converged
  infer_instance

end ReconcileState

end PairingReconcile
```

- [ ] **Step 2: Verify Lake builds**

Run: `cd crates/defra-agent/proofs && lake build PairingReconcile.State`
Expected: builds (warnings OK if any).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/PairingReconcile/State.lean
git commit -m "Lean: PairingReconcile state model (#107)"
```

---

## Task 14: Lean `Proofs/PairingReconcile/Transition.lean`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/PairingReconcile/Transition.lean`

Models the supervisor's tick as a transition relation. One firing of `reconcile_tick` reads `desired`/`actual`, computes the diff, and (in this abstract model) applies one `DiffOp` worth of progress per firing — closer to the TLA+ `Reconcile(n)` action.

- [ ] **Step 1: Write the file**

```lean
import Proofs.PairingReconcile.State

/-!
# Pairing Reconcile Transitions

One transition per tick. `reconcile_install` brings actual closer to desired
by installing one missing collection; `reconcile_teardown` removes one extra.
`operator_write` adjusts desired.
-/

namespace PairingReconcile

inductive Transition : ReconcileState → ReconcileState → Prop where
  | operatorWrite {pre post : ReconcileState} (newDesired : PairingDesired) :
      newDesired ≠ pre.desired →
      post = { pre with desired := newDesired } →
      Transition pre post
  | reconcileInstall {pre post : ReconcileState} (c : String) :
      c ∈ pre.desired.collections →
      c ∉ pre.actual.collections →
      post = { pre with actual := { collections := insert c pre.actual.collections } } →
      Transition pre post
  | reconcileTeardown {pre post : ReconcileState} (c : String) :
      c ∈ pre.actual.collections →
      c ∉ pre.desired.collections →
      post = { pre with actual := { collections := pre.actual.collections.erase c } } →
      Transition pre post
  | crash {pre post : ReconcileState} :
      -- Crash clears in-memory pairing visibility but preserves desired/actual.
      post = { pre with pairing := [] } →
      Transition pre post

/-- Every reconcile transition preserves `desired`/`actual` membership facts
that already held; only the targeted collection changes. -/
theorem install_only_changes_target
    {pre post : ReconcileState} {c d : String}
    (h_trans : Transition pre post)
    (h_install : pre.desired.collections = post.desired.collections)
    (h_other : d ≠ c) :
    (d ∈ pre.actual.collections) → (d ∈ post.actual.collections) := by
  intro h_pre
  cases h_trans with
  | operatorWrite _ _ h_post =>
      cases h_post; exact h_pre
  | reconcileInstall c' _ _ h_post =>
      cases h_post
      by_cases hc : d = c'
      · subst hc; exact Finset.mem_insert_self _ _
      · exact Finset.mem_insert_of_mem h_pre
  | reconcileTeardown c' _ _ h_post =>
      cases h_post
      -- d ≠ c'; preservation under erase
      by_cases hc : d = c'
      · -- This case requires h_other; if we're tearing down `d`, then we need
        -- d ≠ c (the param of the lemma) to apply h_other. The lemma's c is
        -- not necessarily the action's c'; tighten or restrict scope.
        sorry
      · exact Finset.mem_erase.mpr ⟨hc, h_pre⟩
  | crash _ h_post =>
      cases h_post; exact h_pre

end PairingReconcile
```

The `sorry` is intentional in this draft — closing it is part of the task. Use the same shape as `Proofs/RuntimeReconcile/Transition.lean`'s `coherent_preserved` to discharge the case-by-case; iterate until the `sorry` is replaced with a real proof or the lemma is restated to match the actual invariant the supervisor honors. The `Proofs/PairingReconcile/Convergence.lean` task will establish the leads-to result that's the real verification deliverable; this transition file is the supporting machinery.

- [ ] **Step 2: Iterate until the file builds with zero sorrys, or restate the lemma**

Run: `cd crates/defra-agent/proofs && lake build PairingReconcile.Transition`
Expected: builds. If a `sorry` remains, either close it or remove the lemma — the safety obligation is "actual changes only via reconcile actions, and only on the targeted collection," which is provable.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/PairingReconcile/Transition.lean
git commit -m "Lean: PairingReconcile transitions (#107)"
```

---

## Task 15: Lean `Proofs/PairingReconcile/Executable.lean`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/PairingReconcile/Executable.lean`

Provides the decidable contract used by the Rust conformance bridge. Mirrors `Proofs/RuntimeReconcile/Executable.lean` shape.

- [ ] **Step 1: Write the file**

```lean
import Proofs.PairingReconcile.Transition

/-!
# Pairing Reconcile Executable Contract

Decidable vocabulary the Rust conformance bridge consults via the
`assert_lean_transition_is_legal` helper.
-/

namespace PairingReconcile

/-- Vocabulary string for the conformance bridge. -/
def domainName : String := "PairingReconcile"

/-- Stringly-typed transition kinds emitted by the supervisor. -/
inductive TransitionKind where
  | operatorWrite
  | reconcileInstall
  | reconcileTeardown
  | crash
  deriving DecidableEq, Repr

def TransitionKind.fromString? : String → Option TransitionKind
  | "operatorWrite" => some .operatorWrite
  | "reconcileInstall" => some .reconcileInstall
  | "reconcileTeardown" => some .reconcileTeardown
  | "crash" => some .crash
  | _ => none

def TransitionKind.toString : TransitionKind → String
  | .operatorWrite => "operatorWrite"
  | .reconcileInstall => "reconcileInstall"
  | .reconcileTeardown => "reconcileTeardown"
  | .crash => "crash"

theorem fromString_toString (k : TransitionKind) :
    TransitionKind.fromString? k.toString = some k := by
  cases k <;> rfl

end PairingReconcile
```

- [ ] **Step 2: Build + commit**

```bash
cd crates/defra-agent/proofs && lake build PairingReconcile.Executable && cd -
git add crates/defra-agent/proofs/Proofs/PairingReconcile/Executable.lean
git commit -m "Lean: PairingReconcile executable contract (#107)"
```

---

## Task 16: Lean `Proofs/PairingReconcile/Convergence.lean`

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/PairingReconcile/Convergence.lean`
- Create: `crates/defra-agent/proofs/Proofs/PairingReconcile.lean` (barrel)

The convergence theorem is the verification deliverable: under finite reconcile actions and operator stability, the state reaches `converged`.

- [ ] **Step 1: Write the convergence file**

```lean
import Proofs.PairingReconcile.State
import Proofs.PairingReconcile.Transition

/-!
# Pairing Reconcile Convergence

Under finite steps and stable desired state, the supervisor reaches
`converged`. The proof mirrors the TLA+ leads-to-convergence in
`crates/defra-agent/proofs/tla/ReversePairing.tla` (`InstallConverges` /
`TeardownConverges`), but proved here as a Lean theorem on bounded finite
state.
-/

namespace PairingReconcile

open ReconcileState

/-- Symmetric difference of desired and actual collections — the count of
disagreements remaining. Each reconcile action strictly decreases this. -/
def disagreementCount (s : ReconcileState) : Nat :=
  (s.desired.collections \ s.actual.collections).card +
    (s.actual.collections \ s.desired.collections).card

theorem reconcileInstall_decreases
    {pre post : ReconcileState} {c : String}
    (h_pre_desired : c ∈ pre.desired.collections)
    (h_pre_not_actual : c ∉ pre.actual.collections)
    (h_post : post = { pre with actual := { collections := insert c pre.actual.collections } }) :
    disagreementCount post < disagreementCount pre := by
  sorry  -- standard Finset \ and insert arithmetic; complete during proof iteration

theorem reconcileTeardown_decreases
    {pre post : ReconcileState} {c : String}
    (h_pre_actual : c ∈ pre.actual.collections)
    (h_pre_not_desired : c ∉ pre.desired.collections)
    (h_post : post = { pre with actual := { collections := pre.actual.collections.erase c } }) :
    disagreementCount post < disagreementCount pre := by
  sorry  -- standard Finset arithmetic

/-- Under operator-stable desired state, finitely many reconcile actions
suffice to reach `converged`. -/
theorem reconcile_converges_in_finite_steps
    (s : ReconcileState) :
    ∃ (n : Nat), n ≤ disagreementCount s ∧
      (∀ (s' : ReconcileState), -- ... after n reconcile actions ...
        s'.desired = s.desired →
        s'.converged) := by
  sorry  -- structural induction on disagreementCount

end PairingReconcile
```

Same `sorry` discipline as Task 14 — iterate until closed. The `disagreementCount` decreases-strictly lemma is a Mathlib Finset exercise.

- [ ] **Step 2: Write the barrel `PairingReconcile.lean`**

```lean
import Proofs.PairingReconcile.State
import Proofs.PairingReconcile.Transition
import Proofs.PairingReconcile.Executable
import Proofs.PairingReconcile.Convergence

/-!
# Pairing Reconcile Model

Barrel import for the consumer-side reconcile state model, transition
relation, decidable executable contract, and convergence theorem. See spec
§4.2 (6) and the TLA+ source at
`crates/defra-agent/proofs/tla/ReversePairing.tla`.
-/
```

- [ ] **Step 3: Build + commit**

```bash
cd crates/defra-agent/proofs && lake build PairingReconcile && cd -
git add crates/defra-agent/proofs/Proofs/PairingReconcile/Convergence.lean \
        crates/defra-agent/proofs/Proofs/PairingReconcile.lean
git commit -m "Lean: PairingReconcile convergence + barrel (#107)"
```

---

## Task 17: Register `PairingReconcile` in the Rust conformance bridge

**Files:**
- Modify: `crates/defra-agent/src/lean_vocab_test.rs`
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs` (add `"PairingReconcile"` to the domain list)

- [ ] **Step 1: Read the existing conformance bridge to understand the shape**

Run: `grep -n 'RuntimeReconcile' crates/defra-agent/src/lean_vocab_test.rs`
Inspect how `RuntimeReconcile` is registered (it should appear in a domain list / contract table).

- [ ] **Step 2: Add `PairingReconcile` domain**

In `crates/defra-agent/src/lean_vocab_test.rs`, locate the contract registry and add an entry for `PairingReconcile`, listing the transition kinds: `"operatorWrite"`, `"reconcileInstall"`, `"reconcileTeardown"`, `"crash"`. Mirror the `RuntimeReconcile` registration pattern verbatim.

- [ ] **Step 3: Add the domain to `state_machine_conformance.rs`**

In `crates/defra-agent/tests/state_machine_conformance.rs`'s `lean_executable_contracts_cover_initial_domains` test, add `"PairingReconcile"` to the iteration list. Add a few `assert_lean_transition_is_legal` calls that exercise the new domain.

- [ ] **Step 4: Run + commit**

```bash
cargo test -p defra-agent --test state_machine_conformance lean_executable_contracts_cover_initial_domains
git add crates/defra-agent/src/lean_vocab_test.rs \
        crates/defra-agent/tests/state_machine_conformance.rs
git commit -m "Conformance: register PairingReconcile domain (#107)"
```

---

## Task 18: Conformance harness — scenario JSON IR

**Spec section:** §7.3.

**Files:**
- Create: `crates/defra-agent/tests/support/pairing_conformance/mod.rs`
- Create: `crates/defra-agent/tests/support/pairing_conformance/scenario.rs`

- [ ] **Step 1: Define the IR (mirrors §4.1 action mapping)**

`crates/defra-agent/tests/support/pairing_conformance/scenario.rs`:
```rust
//! Scenario IR for the pairing conformance harness. Each entry is a single
//! TLA+ action realized against two running defra-agent processes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "PascalCase")]
pub enum Action {
    /// Operator writes the per-peer desired set on node A.
    OperatorWrite {
        node: NodeId,
        peer: NodeId,
        collections: Vec<String>,
    },
    /// One reconcile tick on the named node.
    Reconcile { node: NodeId },
    /// Force-drop the next inbound HTTP for one tick on the named node.
    Drop { node: NodeId },
    /// Kill and restart the named node's process.
    Crash { node: NodeId },
    /// Wait for the convergence target (or `timeout_secs` elapses).
    WaitForConvergence { timeout_secs: u64 },
}

pub type NodeId = String; // "A" | "B"

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub actions: Vec<Action>,
}

impl Scenario {
    pub fn from_json_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let s: Self = serde_json::from_slice(&bytes)?;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_minimal_scenario() {
        let s = Scenario {
            name: "smoke".into(),
            actions: vec![
                Action::OperatorWrite {
                    node: "A".into(),
                    peer: "B".into(),
                    collections: vec!["c1".into()],
                },
                Action::WaitForConvergence { timeout_secs: 10 },
            ],
        };
        let json = serde_json::to_string(&s).unwrap();
        let parsed: Scenario = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.actions.len(), 2);
    }
}
```

`crates/defra-agent/tests/support/pairing_conformance/mod.rs`:
```rust
pub mod scenario;
pub mod runner;
pub mod invariants;
```

- [ ] **Step 2: Build the tests target + commit**

The scenario module's `round_trip_minimal_scenario` test runs as part of the entry test in Task 21 (support modules don't compile standalone). For this task just verify the module builds.

```bash
cargo build -p defra-agent --tests
git add crates/defra-agent/tests/support/pairing_conformance/
git commit -m "Conformance: scenario IR (#107)"
```

---

## Task 19: Conformance harness — runner + observer

**Files:**
- Create: `crates/defra-agent/tests/support/pairing_conformance/runner.rs`

- [ ] **Step 1: Write the runner**

```rust
//! Two-process orchestration for the pairing conformance harness.
//!
//! Each scenario runs against two `defra-agent` embedded nodes spawned in the
//! same test process (separate `EmbeddedNode` instances backed by separate
//! tempdirs). The harness drives operator writes via the runtime GraphQL
//! mutation surface, advances reconcile via the existing supervisor (with a
//! short tick interval), and reads observed state via per-node subscriptions.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent_desktop_core::remote_admin::{PairingActual, PairingDesired};

use super::scenario::{Action, NodeId, Scenario};

pub struct HarnessNode {
    pub id: NodeId,
    pub node: Arc<EmbeddedNode>,
    pub _tempdir: tempfile::TempDir,
}

pub struct Harness {
    pub a: HarnessNode,
    pub b: HarnessNode,
}

impl Harness {
    pub async fn start_two_nodes() -> Result<Self> {
        let a = Self::start_node("A").await?;
        let b = Self::start_node("B").await?;
        Ok(Self { a, b })
    }

    async fn start_node(id: &str) -> Result<HarnessNode> {
        let tempdir = tempfile::tempdir()?;
        // EmbeddedNode build mirrors test_db; cribbed for two-node setup.
        let node = Arc::new(
            defra_agent::defra_node::EmbeddedNode::builder()
                .data_path(tempdir.path().to_path_buf())
                .with_storage_backend(defra_node::StorageBackend::Memory)
                .with_p2p(/* config sufficient for in-test addressing */ Default::default())
                .build()
                .await?,
        );
        defra_agent_desktop_core::client::schema::ensure_runtime_schemas(node.as_ref()).await?;
        Ok(HarnessNode {
            id: id.to_string(),
            node,
            _tempdir: tempdir,
        })
    }

    pub fn node(&self, id: &NodeId) -> &HarnessNode {
        if id == &self.a.id {
            &self.a
        } else if id == &self.b.id {
            &self.b
        } else {
            panic!("unknown node {id}")
        }
    }

    pub async fn run(&mut self, scenario: &Scenario) -> Result<()> {
        for action in &scenario.actions {
            self.apply_action(action).await?;
        }
        Ok(())
    }

    async fn apply_action(&mut self, action: &Action) -> Result<()> {
        match action {
            Action::OperatorWrite { node, peer, collections } => {
                let n = self.node(node);
                let mutation = build_peer_pairing_desired_mutation(peer, collections);
                n.node.execute(mutation.into(), None).await?;
            }
            Action::Reconcile { node: _ } => {
                // Reconcile is driven by the supervisor tick; the harness
                // sleeps for one tick interval to let the loop fire.
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Action::Drop { node: _ } => {
                // v1: not implemented; future-work hook. Harness logs and continues.
                tracing::warn!("Drop action not implemented in v1 harness");
            }
            Action::Crash { node: _ } => {
                tracing::warn!("Crash action not implemented in v1 harness");
            }
            Action::WaitForConvergence { timeout_secs } => {
                wait_for_convergence(&self.a, &self.b, Duration::from_secs(*timeout_secs)).await?;
            }
        }
        Ok(())
    }
}

fn build_peer_pairing_desired_mutation(peer: &str, collections: &[String]) -> String {
    let cols = collections
        .iter()
        .map(|c| format!(r#""{}""#, c))
        .collect::<Vec<_>>()
        .join(",");
    let now = chrono::Utc::now().to_rfc3339();
    format!(
        r#"mutation {{ create_PeerPairingDesired(input: {{
            peer_id: "{peer}",
            collections: [{cols}],
            replicator_addresses: [],
            created_at: "{now}",
            updated_at: "{now}"
        }}) {{ peer_id }} }}"#
    )
}

async fn wait_for_convergence(
    a: &HarnessNode,
    b: &HarnessNode,
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let (desired, actual) = read_state(a, b).await?;
        if desired.collections == actual.collections {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "convergence timeout: desired={:?} actual={:?}",
                desired.collections,
                actual.collections
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn read_state(a: &HarnessNode, b: &HarnessNode) -> Result<(PairingDesired, PairingActual)> {
    // Read A's PeerPairingDesired for peer "B" + B's installed collections.
    let desired_q = r#"query { PeerPairingDesired(filter: {peer_id: {_eq: "B"}}) { collections } }"#;
    let result = a.node.execute(desired_q.into(), None).await?;
    let desired_cols: BTreeSet<String> = result
        .data
        .as_ref()
        .and_then(|d| d.get("PeerPairingDesired"))
        .and_then(|arr| arr.get(0))
        .and_then(|row| row.get("collections"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    // Read B's actual subscriptions. Production reads via HTTP; in-process
    // harness reads via direct query against B's p2p adapter.
    let actual_cols: BTreeSet<String> = b
        .node
        .p2p_arc()
        .ok_or_else(|| anyhow::anyhow!("B has no p2p"))?
        .get_collections()
        .await
        .map_err(|e| anyhow::anyhow!("get_collections: {e}"))?
        .into_iter()
        .collect();
    Ok((
        PairingDesired { collections: desired_cols, replicator_addresses: Default::default() },
        PairingActual { collections: actual_cols, replicator_addresses: Default::default() },
    ))
}
```

- [ ] **Step 2: Commit (no tests yet; covered in Task 21)**

```bash
cargo build -p defra-agent --tests
git add crates/defra-agent/tests/support/pairing_conformance/runner.rs
git commit -m "Conformance: harness runner + observer (#107)"
```

---

## Task 20: Conformance harness — invariant evaluator

**Files:**
- Create: `crates/defra-agent/tests/support/pairing_conformance/invariants.rs`

- [ ] **Step 1: Write the invariant evaluator**

```rust
//! Safety + leads-to invariant evaluator for the pairing conformance harness.
//! Mirrors `crates/defra-agent/proofs/tla/ReversePairing.tla` properties:
//! - InstallJustified / TeardownJustified (safety)
//! - InstallConverges / TeardownConverges (liveness, observed at quiescence)

use std::collections::BTreeSet;

use defra_agent_desktop_core::remote_admin::{PairingActual, PairingDesired};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedSnapshot {
    pub desired: PairingDesired,
    pub actual: PairingActual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyViolation {
    /// Some `c` is in actual but never appeared in any prior desired snapshot.
    ActualWithoutPriorDesired { collection: String },
}

/// Check that every collection observed in actual has appeared in some
/// previous (or current) desired observation.
pub fn check_safety(history: &[ObservedSnapshot]) -> Result<(), SafetyViolation> {
    let mut all_desired_ever: BTreeSet<String> = BTreeSet::new();
    for snapshot in history {
        all_desired_ever.extend(snapshot.desired.collections.iter().cloned());
        for c in snapshot.actual.collections.iter() {
            if !all_desired_ever.contains(c) {
                return Err(SafetyViolation::ActualWithoutPriorDesired {
                    collection: c.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Check the leads-to convergence property on the final observed snapshot.
pub fn check_liveness(final_snapshot: &ObservedSnapshot) -> bool {
    final_snapshot.desired.collections == final_snapshot.actual.collections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(desired: &[&str], actual: &[&str]) -> ObservedSnapshot {
        ObservedSnapshot {
            desired: PairingDesired {
                collections: desired.iter().map(|s| s.to_string()).collect(),
                replicator_addresses: Default::default(),
            },
            actual: PairingActual {
                collections: actual.iter().map(|s| s.to_string()).collect(),
                replicator_addresses: Default::default(),
            },
        }
    }

    #[test]
    fn safety_passes_when_actual_traces_to_desired() {
        let history = vec![snap(&["c1"], &[]), snap(&["c1"], &["c1"])];
        assert_eq!(check_safety(&history), Ok(()));
    }

    #[test]
    fn safety_fails_on_phantom_actual() {
        let history = vec![snap(&[], &["c1"])];
        assert!(matches!(
            check_safety(&history),
            Err(SafetyViolation::ActualWithoutPriorDesired { .. })
        ));
    }

    #[test]
    fn liveness_holds_when_desired_equals_actual() {
        let s = snap(&["c1", "c2"], &["c1", "c2"]);
        assert!(check_liveness(&s));
    }
}
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p defra-agent --tests pairing_conformance::invariants
git add crates/defra-agent/tests/support/pairing_conformance/invariants.rs
git commit -m "Conformance: invariant evaluator (#107)"
```

---

## Task 21: Hand-written install/teardown happy-path scenario + entry test

**Files:**
- Create: `crates/defra-agent/tests/fixtures/pairing_scenarios/install_teardown_happy_path.json`
- Create: `crates/defra-agent/tests/pairing_reconcile_conformance.rs`

- [ ] **Step 1: Write the scenario**

`crates/defra-agent/tests/fixtures/pairing_scenarios/install_teardown_happy_path.json`:
```json
{
  "name": "install_teardown_happy_path",
  "actions": [
    {"op": "OperatorWrite", "node": "A", "peer": "B", "collections": ["AgentRequest", "AgentResponse"]},
    {"op": "Reconcile", "node": "A"},
    {"op": "WaitForConvergence", "timeout_secs": 15},
    {"op": "OperatorWrite", "node": "A", "peer": "B", "collections": []},
    {"op": "Reconcile", "node": "A"},
    {"op": "WaitForConvergence", "timeout_secs": 15}
  ]
}
```

- [ ] **Step 2: Write the entry test**

`crates/defra-agent/tests/pairing_reconcile_conformance.rs`:
```rust
//! Entry test for the pairing reconcile conformance harness.
//! See spec §7.3.

mod support;

use std::path::PathBuf;

use support::pairing_conformance::invariants::{check_liveness, check_safety, ObservedSnapshot};
use support::pairing_conformance::runner::Harness;
use support::pairing_conformance::scenario::Scenario;

#[tokio::test]
async fn install_teardown_happy_path_satisfies_safety_and_liveness() {
    // Enable the feature flag so the supervisor runs the reconcile pass.
    std::env::set_var("DEFRA_AGENT_PAIRING_RECONCILE", "1");

    let scenario_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pairing_scenarios/install_teardown_happy_path.json");
    let scenario = Scenario::from_json_file(&scenario_path).expect("scenario parses");

    let mut harness = Harness::start_two_nodes().await.expect("harness starts");
    harness.run(&scenario).await.expect("scenario runs to convergence");

    // Snapshot at quiescence. For v1, just the final state.
    let history = harness.observation_history();
    check_safety(&history).expect("safety holds");
    let final_snapshot: &ObservedSnapshot = history.last().expect("non-empty history");
    assert!(check_liveness(final_snapshot), "convergence reached");
}
```

(`Harness::observation_history` is the per-tick observed-state log — add a `Vec<ObservedSnapshot>` field to the harness in Task 19's runner if not present, and append on each `read_state` call.)

- [ ] **Step 3: Run the conformance test**

Run: `cargo test -p defra-agent --test pairing_reconcile_conformance -- --nocapture`
Expected: passes. The harness brings up two embedded nodes, drives an operator write, waits for the supervisor to converge, then drives a teardown and waits for convergence.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/tests/fixtures/pairing_scenarios/install_teardown_happy_path.json \
        crates/defra-agent/tests/pairing_reconcile_conformance.rs
git commit -m "Conformance: install/teardown happy path scenario (#107)"
```

---

## Plan complete — what to do next

The plan implements the spec end-to-end. Open follow-ups (not in this plan):
- Flag drop (`DEFRA_AGENT_PAIRING_RECONCILE` → default-on, then removed) — after dogfooding.
- `P2pRemoteP2pAdmin` impl wrapping defradb.rs's eventual admin CommChannel.
- Durable retry surface (#170 v2) — gated on #166.
- Auth wiring (#180).
- R5 cross-deployment subagent worker — separate implementation track consuming the substrate this plan produces.
- `pairing.rs` deletion — separate PR after flag drop.
