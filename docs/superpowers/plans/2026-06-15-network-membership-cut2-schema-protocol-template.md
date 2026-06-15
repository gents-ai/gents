# Network-Membership PR Cut 2 — Schema + Protocol Signing + Discovery Template

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the four network control-plane DefraDB collections (`AgentNetwork`, `NetworkMembership`, `PeerEndpoint`, `NetworkJoinRequest`), canonical CBOR signing-payload helpers for them + a `danet1-` network-pointer token in `defra-agent-protocol`, and the built-in `discovery` scope template — so cuts 3–5 (CLI, reconciler) have the schema, signing vocabulary, and on-ramp template to build on.

**Architecture:** Pure additive, Rust-only on the schema/protocol side, plus the §9 Lean model this slice rests on. SDL files mirror the Lean network-membership entities field-for-field (the model is the source of truth). Composite uniqueness uses a derived key field (mirroring `AgentMessage.message_key`), since DefraDB `@index(unique)` is per-field. Signing helpers mirror `pairing_token::signing_payload` (CBOR of the struct with the sig field zeroed) so CLI/runtime/tests sign identical bytes. The `danet1-` pointer mirrors the `dapair1-` token's encode/decode. No CLI, no reconciler wiring (those are cuts 3–5).

**Lean-foundation note (added during review):** the §9 network-membership Lean model did **not** exist when this slice was first cut (cut 1 per spec §10 was skipped — `PeerRegistryDiscovery/State.lean` only modeled the self-asserted `RegistryEntry`). Per the foundation flow (Lean → impl), the SDL cannot rest on a model that isn't there. This PR therefore lands the §9 model first: `Proofs/PeerRegistryDiscovery/NetworkMembership.lean` (entities `Network`/`Membership`/`Endpoint`/`JoinRequest`, predicates `validNetwork`/`adminSignedMembership`/`memberSignedEndpoint`/`admittedMember`/`endpointMaterializable`, and the five §9 obligations, zero `sorry`, non-vacuous). The runtime conformance fence (`decideMaterializable` ↔ the reconciler) lands with cut 5.

**Tech Stack:** Rust — `defra-agent-schemas` (SDL consts), `defra-agent-protocol` (re-export + signing + pointer token), `defra-agent` (`migration.rs` registration + `templates.rs`).

**Spec:** `docs/superpowers/specs/2026-06-15-network-document-membership-design.md` §4 (data model), §5 (protocol primitives), §6 (discovery template). Worktree `defra-agent-network-cut2` (branch `network-cut2`, off `origin/main`). Gate: `cargo test -p defra-agent-schemas -p defra-agent-protocol -p defra-agent` (full packages, never `--lib`).

**Field-source note:** SDL field names match the §9 Lean entities in `NetworkMembership.lean` (spec §4): `Network{networkId,adminDid,adminSigValid→admin_sig}`, `Membership{networkId,memberDid,active→status,adminSigValid→admin_sig}`, `Endpoint{did,nodeId→node_id,fresh,bindingSigValid→binding_sig}`, `JoinRequest{networkId,candidateDid,reqSigValid→req_sig}`. The Lean models `active:Bool`; the SDL uses `status: String` (active|revoked) per spec §4 — the bridge is `m.active = (status=="active")`, which the reconciler (cut 5) applies.

---

## File Structure

- `crates/defra-agent-schemas/schemas/agent/{agent_network,network_membership,peer_endpoint,network_join_request}.graphql` (CREATE) — the 4 SDL types.
- `crates/defra-agent-schemas/src/lib.rs` — 4 `*_NAME`/`*` consts; add to `ALL` + `ALL_COLLECTION_NAMES`; bump the `ALL.len()` count test.
- `crates/defra-agent-protocol/src/schemas.rs` — re-export the 4 consts; add to `ALL` + `ALL_COLLECTION_NAMES`; bump the `ALL.len()` count assertion.
- `crates/defra-agent/src/migration.rs` — 4 `ensure_*_migrations` fns; register them in `ensure_all_runtime_migrations`.
- `crates/defra-agent-protocol/src/network_token.rs` (CREATE) — 4 record signing-payload structs + the `danet1-` `NetworkPointer` token; register `pub mod network_token;` in `defra-agent-protocol/src/lib.rs`.
- `crates/defra-agent/src/agent/p2p_reconcile/templates.rs` — `DISCOVERY_COLLECTIONS` const + the `discovery` `ScopeTemplate` in `BUILTIN_TEMPLATES`.

---

## Task 1: The four control-plane SDL collections + registration + migrations

**Files:** 4 new `.graphql`; modify `defra-agent-schemas/src/lib.rs`, `defra-agent-protocol/src/schemas.rs`, `defra-agent/src/migration.rs`.

- [ ] **Step 1: Create the 4 SDL files** in `crates/defra-agent-schemas/schemas/agent/`. Use a derived key field for composite uniqueness (mirror `AgentMessage.message_key`).

`agent_network.graphql`:
```graphql
type AgentNetwork {
    # One row per network. Admin-authored; admin_sig is the admin DID's signature
    # over the canonical record (see defra-agent-protocol network_token). Mirrors
    # Lean PeerRegistryDiscovery.Network.
    network_id: String @index(unique: true)
    admin_did: String @index
    display_name: String
    default_template: String
    created_at: String
    admin_sig: String
}
```
`network_membership.graphql`:
```graphql
type NetworkMembership {
    # Admin-authored membership grant. Unique per (network_id, member_did) via the
    # derived membership_key. status is "active" | "revoked" (a revoked tombstone).
    # admin_sig signs the canonical record. Mirrors Lean Membership.
    membership_key: String @index(unique: true)
    network_id: String @index
    member_did: String @index
    status: String
    granted_at: String
    revoked_at: String
    admin_sig: String
}
```
`peer_endpoint.graphql`:
```graphql
type PeerEndpoint {
    # Member-self-asserted transport binding (global per node — one per DID).
    # binding_sig is the member DID's signature. Mirrors Lean Endpoint.
    did: String @index(unique: true)
    node_id: String
    address: String
    updated_at: String
    binding_sig: String
}
```
`network_join_request.graphql`:
```graphql
type NetworkJoinRequest {
    # Candidate-authored join request. Unique per (network_id, candidate_did) via
    # request_key. status is informational ("pending"); membership is the authority.
    # req_sig is the candidate DID's signature. Mirrors Lean JoinRequest.
    request_key: String @index(unique: true)
    network_id: String @index
    candidate_did: String @index
    candidate_node_id: String
    candidate_address: String
    requested_at: String
    status: String
    req_sig: String
}
```

- [ ] **Step 2: Register consts in `defra-agent-schemas/src/lib.rs`** (mirror the `CONSUMED_INVITE_NONCE` pattern at ~line 55): add, in the agent-domain block, for each of the four:
```rust
pub const AGENT_NETWORK_NAME: &str = "AgentNetwork";
pub const AGENT_NETWORK: &str = include_str!("../schemas/agent/agent_network.graphql");
pub const NETWORK_MEMBERSHIP_NAME: &str = "NetworkMembership";
pub const NETWORK_MEMBERSHIP: &str = include_str!("../schemas/agent/network_membership.graphql");
pub const PEER_ENDPOINT_NAME: &str = "PeerEndpoint";
pub const PEER_ENDPOINT: &str = include_str!("../schemas/agent/peer_endpoint.graphql");
pub const NETWORK_JOIN_REQUEST_NAME: &str = "NetworkJoinRequest";
pub const NETWORK_JOIN_REQUEST: &str = include_str!("../schemas/agent/network_join_request.graphql");
```
Add the four `*` consts to `pub const ALL` and the four `*_NAME` consts to `pub const ALL_COLLECTION_NAMES`.

- [ ] **Step 3: Bump the count test** in `lib.rs` (currently `assert_eq!(ALL.len(), 23);` at ~line 139). Count the merged `ALL` array AFTER your edit (do not guess — add 4 to the actual current count) and set the literal. Keep `assert_eq!(ALL.len(), ALL_COLLECTION_NAMES.len())` (it stays correct if you added 4 to each).

- [ ] **Step 4: Re-export in `defra-agent-protocol/src/schemas.rs`**: add the 4 `*`/`*_NAME` consts to the `use defra_agent_schemas::{...}` block, to `pub const ALL`, and to `pub const ALL_COLLECTION_NAMES`. Bump the `ALL.len()` assertion (the `"ALL should enumerate every non-runtime schema"` test, ~line 118) by counting the new array length (add 4 to current).

- [ ] **Step 5: Add 4 migration fns** in `crates/defra-agent/src/migration.rs` (mirror `ensure_consumed_invite_nonce_migrations` exactly — `get_collection` short-circuit, `add_schema`, tolerate "already exists"):
```rust
pub async fn ensure_agent_network_migrations(node: Arc<EmbeddedNode>) -> Result<()> {
    if node.get_collection("AgentNetwork").context("get AgentNetwork collection")?.is_some() {
        return Ok(());
    }
    match node.add_schema(defra_agent_protocol::schemas::AGENT_NETWORK).await {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("already exists") => Ok(()),
        Err(error) => Err(error).context("add AgentNetwork schema"),
    }
}
```
…and the analogous `ensure_network_membership_migrations` (`NetworkMembership` / `NETWORK_MEMBERSHIP`), `ensure_peer_endpoint_migrations` (`PeerEndpoint` / `PEER_ENDPOINT`), `ensure_network_join_request_migrations` (`NetworkJoinRequest` / `NETWORK_JOIN_REQUEST`).

- [ ] **Step 6: Register them in `ensure_all_runtime_migrations`** (add the four `.await.context(...)?` calls alongside the existing `ensure_consumed_invite_nonce_migrations` call).

- [ ] **Step 7: Run + commit.** `cargo test -p defra-agent-schemas -p defra-agent-protocol` (count tests pass). `cargo build -p defra-agent` (migration fns compile). Commit: `feat(schema): network control-plane collections (AgentNetwork/NetworkMembership/PeerEndpoint/NetworkJoinRequest)`

## Task 2: Canonical signing-payload helpers + danet1- network pointer (protocol crate)

**Files:** Create `crates/defra-agent-protocol/src/network_token.rs`; register `pub mod network_token;` in `crates/defra-agent-protocol/src/lib.rs`. Read `crates/defra-agent-protocol/src/pairing_token.rs` first — mirror its `signing_payload`/`encode`/`decode` exactly.

- [ ] **Step 1: Write failing tests** (in `network_token.rs` `#[cfg(test)]`): each record's `signing_payload` covers its content fields (two records differing in one field → different payloads) and excludes the sig; the `danet1-` pointer round-trips (`encode`→`decode` equals original) and its `signing_payload` covers `network_id`/`admin_did`/`admin_ticket`; `decode` rejects a `dapair1-` (wrong prefix) and a wrong version.

- [ ] **Step 2: Implement the 4 record signing structs.** Each is the canonical signing form (CBOR of the struct with `sig` zeroed), mirroring `pairing_token::signing_payload`:
```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRecord {
    pub network_id: String,
    pub admin_did: String,
    pub display_name: String,
    pub default_template: String,
    pub created_at: String,
    pub sig: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipRecord {
    pub network_id: String,
    pub member_did: String,
    pub status: String,
    pub granted_at: String,
    pub revoked_at: String,
    pub sig: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRecord {
    pub did: String,
    pub node_id: String,
    pub address: String,
    pub updated_at: String,
    pub sig: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequestRecord {
    pub network_id: String,
    pub candidate_did: String,
    pub candidate_node_id: String,
    pub candidate_address: String,
    pub requested_at: String,
    pub sig: Vec<u8>,
}

macro_rules! signing_payload_impl {
    ($t:ty) => {
        impl $t {
            /// CBOR of this record with `sig` zeroed — the bytes signed/verified.
            /// Mirrors `pairing_token::signing_payload`.
            pub fn signing_payload(&self) -> Vec<u8> {
                let mut unsigned = self.clone();
                unsigned.sig = Vec::new();
                let mut bytes = Vec::new();
                ciborium::ser::into_writer(&unsigned, &mut bytes)
                    .expect("CBOR serialisation of signing payload is infallible");
                bytes
            }
        }
    };
}
signing_payload_impl!(NetworkRecord);
signing_payload_impl!(MembershipRecord);
signing_payload_impl!(EndpointRecord);
signing_payload_impl!(JoinRequestRecord);
```

- [ ] **Step 3: Implement the `danet1-` NetworkPointer token** (mirror `InviteToken`'s encode/decode/signing_payload; it is a pointer to the admin, NOT a grant):
```rust
use std::io::Cursor;

/// Network bootstrap pointer: identifies a network + how to reach its admin.
/// Distinct from the pairwise `dapair1-` invite — carries no membership grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPointer {
    pub v: u8,
    pub network_id: String,
    pub admin_did: String,
    pub admin_ticket: String,
    pub issued_at: String,
    pub nonce: String,
    pub sig: Vec<u8>,
}

pub const NETWORK_POINTER_PREFIX: &str = "danet1-";
pub const NETWORK_POINTER_VERSION: u8 = 1;

impl NetworkPointer {
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.sig = Vec::new();
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&unsigned, &mut bytes)
            .expect("CBOR serialisation of signing payload is infallible");
        bytes
    }
}

pub fn encode_pointer(p: &NetworkPointer) -> Result<String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(p, &mut bytes).context("encoding network pointer")?;
    Ok(format!("{NETWORK_POINTER_PREFIX}{}", bs58::encode(bytes).into_string()))
}

pub fn decode_pointer(raw: &str) -> Result<NetworkPointer> {
    let encoded = raw.trim().strip_prefix(NETWORK_POINTER_PREFIX)
        .context("invalid network pointer prefix")?;
    let bytes = bs58::decode(encoded).into_vec().context("decoding network pointer")?;
    let p: NetworkPointer = ciborium::de::from_reader(Cursor::new(bytes)).context("parsing network pointer")?;
    match p.v {
        NETWORK_POINTER_VERSION => Ok(p),
        v => anyhow::bail!("network pointer version {v} is not supported; re-issue with a newer defra-agent"),
    }
}
```
Confirm `ciborium`, `bs58`, `anyhow`, `serde` are already deps of `defra-agent-protocol` (they are — `pairing_token.rs` uses them).

- [ ] **Step 4: Run + commit.** `cargo test -p defra-agent-protocol network_token` passes; `cargo test -p defra-agent-protocol` full green. Commit: `feat(protocol): canonical signing payloads for network records + danet1 pointer token`

## Task 3: The `discovery` scope template

**Files:** Modify `crates/defra-agent/src/agent/p2p_reconcile/templates.rs`.

- [ ] **Step 1: Write failing tests** (in `templates.rs` `#[cfg(test)]`, mirroring the existing `conversation`/`agent-config` template tests): `resolve_template("discovery")` is `Some`; its `delivery` is `Delivery::Replicate`; its `scope` is `Scope::Unscoped`; its `collections` contains `AgentNetwork`, `NetworkMembership`, `PeerEndpoint`, `NetworkJoinRequest` and the agent-config collections.

- [ ] **Step 2: Add the `DISCOVERY_COLLECTIONS` const + template.** After `AGENT_CONFIG_COLLECTIONS`:
```rust
/// Discovery (network control-plane) collections: the membership documents a
/// joiner needs to learn and run the network, plus agent-config so it can run
/// what it discovers. Unscoped Replicate — small control-plane docs, not
/// per-peer slices. This is the bootstrap on-ramp template.
const DISCOVERY_COLLECTIONS: &[&str] = &[
    "AgentNetwork",
    "NetworkMembership",
    "PeerEndpoint",
    "NetworkJoinRequest",
    "AgentBehavior",
    "ToolSelection",
    "InferenceBackend",
    "InferenceProfile",
    "ToolServiceRegistry",
    "Skill",
];
```
Add to `BUILTIN_TEMPLATES`:
```rust
    ScopeTemplate {
        id: "discovery",
        collections: DISCOVERY_COLLECTIONS,
        scope: Scope::Unscoped,
        delivery: Delivery::Replicate,
    },
```

- [ ] **Step 3: Run + commit.** `cargo test -p defra-agent templates` (or the p2p_reconcile module tests) passes; confirm `p2p templates list` would surface it (it iterates `builtin_templates()` — no extra wiring). Commit: `feat(p2p): discovery scope template (network control-plane + config, unscoped replicate)`

## Task 4: Final gate

- [ ] **Step 1: Full-package gates.** `cargo test -p defra-agent-schemas -p defra-agent-protocol` and `cargo test -p defra-agent` — all green (schema count tests, signing/pointer tests, template tests, migration compile). Note: the migration fns aren't exercised end-to-end until cut 5; a `cargo build -p defra-agent` + the existing migration unit-test style is sufficient here.
- [ ] **Step 2: Confirm no scope creep** — diff is only the 4 SDL files + schemas/protocol registration + migration fns + network_token.rs + the discovery template. No CLI, no reconciler wiring, no Lean.
- [ ] **Step 3: Commit any fmt drift** (`cargo fmt -p defra-agent-schemas -p defra-agent-protocol -p defra-agent`).

---

## Self-Review (spec coverage)

| Spec item | Task |
|---|---|
| §4 `AgentNetwork` / `NetworkMembership` / `PeerEndpoint` / `NetworkJoinRequest` SDL | T1 |
| §4 composite uniqueness (membership_key / request_key) | T1 (derived key field) |
| §5 canonical signing payloads (4 records, CBOR sig-zeroed) | T2 |
| §5 `danet1-` network pointer token (distinct from `dapair1-`) | T2 |
| §6 `discovery` scope template (control-plane + config, Replicate/unscoped) | T3 |
| migrations registered in the single sanctioned entry | T1 S5-6 |

**Out of scope (later cuts):** writing/reading these rows, signing them with a real DID key, the pointer mint/verify in the CLI (cut 3), join/approve document flow (cut 4), reconciler reading `deriveNetworkDesired`/`decideMaterializable` over these docs (cut 5). **Placeholder scan:** all SDL, consts, structs, migration fns, template entry, and gate commands are concrete; count-test literals are computed-from-actual (not guessed) per the cut-1 lesson. **Type consistency:** SDL field names ↔ spec §4 ↔ the §9 Lean entities in `NetworkMembership.lean`; signing-struct fields ↔ SDL signed fields (record content minus the `*_sig`, which becomes the struct's zeroed `sig`); `NetworkPointer` mirrors `InviteToken`.
