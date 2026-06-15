# Peer Registry + Signed Invites Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn pairwise P2P pairing into network membership: a replicated `PeerRegistry` service-discovery document, signed member invites as the join credential, and a discovery reconciler that materializes registry-owned `PeerPairingDesired` rows over the proven pairing reconciler.

**Architecture:** Reuse the existing layered model. `PeerRegistry` is a self-registered, heartbeating, replicated collection (discovery). A signed invite (issuer's DID signature over the token) is the join credential (authorization). A discovery step reads the registry and writes registry-owned desired pairing rows; the unchanged `PairingReconcile` engine wires them (replication). Lean-first: a new discovery-derivation model + a signature guard on join, conformance-mirrored, then runtime, then CLI, then integration tests.

**Tech Stack:** Rust (clap, tokio, async-trait), Lean 4 + mathlib, GraphQL/DefraDB, `AgentIdentity` sign/verify (identity.rs), ciborium + bs58 (token codec, already deps).

**Spec:** `docs/superpowers/specs/2026-06-13-peer-registry-service-discovery-design.md`

---

## Execution notes (read first)

- **Worktree:** `../defra-agent-cli-normalization` (branch `cli-normalization`). Mathlib build cache already symlinked under `crates/defra-agent/proofs/.lake/packages/mathlib/.lake/build`; never run `lake exe cache get`.
- **Gates:** `cargo test -p defra-agent` (FULL — never `--lib`; the crate is a binary-less lib here but integration tests are separate compile units), `cargo test -p defra-agent-cli --bins` for CLI unit tests + `--test <name>` for integration, `lake build` from `crates/defra-agent/proofs/` for Lean. Zero `sorry`.
- **Sharp edges:** always `defra_agent::graphql::escape_graphql_string()` for interpolated values; never emit `[]` in a DefraDB mutation — emit `null` (see `graphql_nullable_string_list_literal` in `commands/p2p/pairings.rs`); `tracing`, never `println` (CLI output helpers excepted).
- **Decided (2026-06-13):** single network for the prototype (`network_id` is carried but not multi-network-enforced); the registry-ownership representation (source discriminator vs separate derived record) is chosen by **whichever keeps the Lean proof cleanest — decided in Task R4 before any Rust derivation code (R5) is written.**
- **Templates to mirror (these are the proven patterns; cite them, adapt names):**
  - Schema+const: `crates/defra-agent-schemas/src/lib.rs:49-52` (desired/applied).
  - Migration: `crates/defra-agent/src/migration.rs:581` `ensure_peer_pairing_desired_migrations`.
  - Reconciler daemon + state-store: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs` (`run_pairing_reconciler`, `GraphqlPairingStateStore`, `reconcile_peer_tick`).
  - Daemon spawn: `crates/defra-agent/src/agent/runtime/startup.rs:300-306`.
  - Lean machine: `crates/defra-agent/proofs/Proofs/PairingReconcile/{State,Transition,Convergence,Executable}.lean`.
  - Conformance harness: `crates/defra-agent/tests/support/pairing_conformance/` + `tests/conformance/pairing_reconcile.rs` + `tests/fixtures/pairing_scenarios/`.
  - Identity: `crates/defra-agent/src/identity.rs` `AgentIdentity::{did, sign, verify}`.

---

### Task R1: `PeerRegistry` schema, registration, migration

**Files:**
- Create: `crates/defra-agent-schemas/schemas/agent/peer_registry.graphql`
- Modify: `crates/defra-agent-schemas/src/lib.rs` (add NAME const + include_str, mirror `:49-52`)
- Modify: `crates/defra-agent-protocol/src/schemas.rs` (add `PEER_REGISTRY` / `PEER_REGISTRY_NAME` to the import list `:19-20` and the schema array `:71-72`)
- Modify: `crates/defra-agent/src/migration.rs` (new `ensure_peer_registry_migrations`, mirror `:581`)
- Test: migration test alongside existing ones in `migration.rs`

- [ ] **Step 1: Write the schema file**

```graphql
type PeerRegistry {
    peer_id: String @index(unique: true)
    agent_did: String @index
    addresses: [String!]!
    profiles: [String!]
    display_name: String
    status: String
    network_id: String @index
    invited_by: String
    registered_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
}
```

- [ ] **Step 2: Register the schema** in `defra-agent-schemas/src/lib.rs` (mirror the `PEER_PAIRING_APPLIED_NAME` + `include_str!` pair at `:50-52`) and add to `defra-agent-protocol/src/schemas.rs` import list and the all-schemas array.
- [ ] **Step 3: Write the failing migration test** in `migration.rs` test module (mirror the peer-pairing migration test): fresh node → `ensure_peer_registry_migrations(node)` → collection exists with all fields; re-run is idempotent.

```rust
#[tokio::test]
async fn ensure_peer_registry_migrations_creates_collection_idempotently() {
    let node = crate::test_support::fresh_embedded_node().await; // use whatever the existing migration tests use
    ensure_peer_registry_migrations(node.clone()).await.unwrap();
    ensure_peer_registry_migrations(node.clone()).await.unwrap(); // idempotent
    let active = active_collection_version(&node, "PeerRegistry").await.expect("exists");
    for field in ["peer_id","agent_did","addresses","profiles","display_name","status","network_id","invited_by","registered_at","updated_at"] {
        assert!(collection_has_field(&active, field), "missing {field}");
    }
}
```

(Use the exact node-construction + assertion helpers the surrounding migration tests use — read `:581`'s test for the pattern.)

- [ ] **Step 4: Run** `cargo test -p defra-agent ensure_peer_registry_migrations` — expect FAIL (fn missing).
- [ ] **Step 5: Implement `ensure_peer_registry_migrations`** mirroring `ensure_peer_pairing_desired_migrations` (`:581`) — apply the schema, add any post-hoc field patches if the helper pattern requires them. Wire it into the same startup migration sequence that calls `ensure_peer_pairing_desired_migrations` (`startup.rs:56` region).
- [ ] **Step 6: Run** `cargo test -p defra-agent ensure_peer_registry_migrations` — expect PASS.
- [ ] **Step 7: Commit** — `feat(schema): PeerRegistry service-discovery collection`

---

### Task R2: Self-registration + heartbeat daemon

**Files:**
- Create: `crates/defra-agent/src/agent/p2p_reconcile/registry.rs` (self-registration writer + heartbeat loop)
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/mod.rs` (export `run_registry_heartbeat`, `RegistryEntry`)
- Modify: `crates/defra-agent/src/agent/runtime/startup.rs:300` (spawn the heartbeat task next to the pairing reconciler)
- Modify: `crates/defra-agent/src/agent/runtime/mod.rs` `BackgroundTaskResult` enum (add `RegistryHeartbeat` variant)
- Test: unit tests in `registry.rs` (mutation shape), integration assertion folded into R7

- [ ] **Step 1: Write failing unit test** for the self-registration upsert builder:

```rust
#[test]
fn registry_upsert_mutation_escapes_and_emits_null_for_empty_profiles() {
    let m = registry_upsert_mutation(&RegistryEntry {
        peer_id: r#"p"1"#.into(), agent_did: "did:key:a".into(),
        addresses: vec!["/ip4/1/tcp/1".into()], profiles: vec![],
        display_name: Some("amy".into()), status: "online".into(),
        network_id: "default".into(), invited_by: None,
    }, "2026-06-13T00:00:00Z");
    assert!(m.contains(r#"peer_id: { _eq: "p\"1" }"#));
    assert!(m.contains("profiles: null"));        // empty list → null, never []
    assert!(!m.contains("profiles: []"));
    assert!(m.contains(r#"status: "online""#));
}
```

- [ ] **Step 2: Run** `cargo test -p defra-agent registry_upsert_mutation` — expect FAIL.
- [ ] **Step 3: Implement `RegistryEntry` + `registry_upsert_mutation`** in `registry.rs`. Reuse `escape_graphql_string`, `graphql_string_list_literal`, and the empty-list→null helper pattern from `commands/p2p/pairings.rs` (move those two helpers to a shared spot in the runtime crate if not already accessible — prefer reusing `defra_agent::graphql` utilities). The upsert filters on `peer_id`, sets `registered_at` only on `add`, refreshes `updated_at` on both.
- [ ] **Step 4: Implement `run_registry_heartbeat(node, identity, network_id, cancel)`**: on start and every `REGISTRY_HEARTBEAT_INTERVAL` (const, propose 30s), resolve self peer_id/addresses (same sources `commands/p2p/output.rs` and the embedded admin use — `node` P2P info), build a `RegistryEntry` for self (`status:"online"`, `agent_did` from `identity.did()`), and execute the upsert. On `cancel`, optionally write `status:"offline"` once, then return.
- [ ] **Step 5: Spawn it** in `startup.rs` next to the pairing reconciler (`:300`), guarded the same way (only when the embedded node has a P2P transport). Add the `BackgroundTaskResult::RegistryHeartbeat` variant.
- [ ] **Step 6: Run** `cargo test -p defra-agent registry_upsert_mutation` — expect PASS; `cargo build -p defra-agent` clean.
- [ ] **Step 7: Commit** — `feat(p2p): self-registration + heartbeat into PeerRegistry`

---

### Task R3: Signed invite token (v2) in the protocol crate

**Files:**
- Create: `crates/defra-agent-protocol/src/pairing_token.rs` (the `InviteToken` type + encode/decode + canonical signing payload)
- Modify: `crates/defra-agent-protocol/src/lib.rs` (export module)
- Modify: `crates/defra-agent-protocol/Cargo.toml` (add `ciborium`, `bs58` if not present)
- Modify: `crates/defra-agent-cli/src/commands/p2p/invite.rs` (sign with `AgentIdentity`, emit v2; re-export removed local type)
- Modify: `crates/defra-agent-cli/src/commands/p2p/join.rs` (verify signature; TOFU; record `invited_by`)
- Test: round-trip + signature tests in `pairing_token.rs`; sign/verify wiring test in invite/join

- [ ] **Step 1: Write failing tests** in `pairing_token.rs`:

```rust
#[test]
fn token_v2_round_trips_and_signing_payload_is_stable() {
    let t = InviteToken { v: 2, issuer_did: "did:key:a".into(), peer_id: "p".into(),
        ticket: "/ip4/1".into(), profiles: vec!["chat-requests".into()],
        network_id: "default".into(), issued_at: "2026-06-13T00:00:00Z".into(),
        sig: vec![1,2,3] };
    let enc = encode(&t).unwrap();
    assert!(enc.starts_with(TOKEN_PREFIX));
    assert_eq!(decode(&enc).unwrap(), t);
    // signing payload excludes sig and is deterministic
    let p1 = signing_payload(&t);
    let mut t2 = t.clone(); t2.sig = vec![9,9];
    assert_eq!(p1, signing_payload(&t2));
}

#[test]
fn decode_rejects_v1_unsigned_tokens() {
    // a v:1 CBOR blob (no sig) must be rejected with a re-issue hint
    let err = decode("dapair1-<v1blob>").unwrap_err().to_string();
    assert!(err.contains("re-issue") || err.contains("newer"));
}
```

- [ ] **Step 2: Run** `cargo test -p defra-agent-protocol token_v2` — expect FAIL.
- [ ] **Step 3: Implement the type + codec** in `pairing_token.rs`: move the v1 struct/encode/decode out of `commands/p2p/invite.rs`, bump to `v: 2` with `issuer_did`, `network_id`, `issued_at`, `sig: Vec<u8>` added. `signing_payload(&token)` serializes the token **with `sig` zeroed/omitted** (canonical: encode a copy with `sig: vec![]`), returning the bytes the issuer signs. `decode` rejects `v != 2` with "pairing invite token version N: re-issue with a newer defra-agent". Keep `TOKEN_PREFIX = "dapair1-"`.
- [ ] **Step 4: Run** `cargo test -p defra-agent-protocol token_v2` — expect PASS.
- [ ] **Step 5: Sign on invite.** In `commands/p2p/invite.rs`, build the unsigned token, compute `signing_payload`, `identity.sign(payload).await` (resolve the local `AgentIdentity` the same way other CLI commands resolve the principal — see `resolve_agent_did`/principal loading), set `sig`, then `encode`. Output `join_command` stays `defra-agent p2p pairings join <tok>`.
- [ ] **Step 6: Verify on join.** In `commands/p2p/join.rs`: `decode` → recompute `signing_payload` → `identity.verify(&token.issuer_did, &payload, &token.sig).await`. On failure, bail "pairing invite signature invalid for issuer <did>". On success, set `invited_by = issuer_did` when writing the desired row (carry it through `write_pairing_desired` or a sibling). (Registry-membership check is added in R5; this task does signature-only = TOFU.)
- [ ] **Step 7: Run** `cargo test -p defra-agent-protocol` and `cargo test -p defra-agent-cli --bins p2p` — expect PASS.
- [ ] **Step 8: Commit** — `feat(p2p): signed v2 invite tokens (protocol crate); verify on join`

---

### Task R4: Lean — discovery derivation model + signature guard (DECIDES ownership representation)

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/PeerRegistryDiscovery/{State,Transition,Derivation,Executable}.lean`
- Modify: `crates/defra-agent/proofs/Proofs/PairingReconcile/State.lean` if the derived/operator-owned split is best modeled as a `source` field on the desired row (the cleaner-proof decision)
- Modify: structure fence + coverage ledger (`tests/conformance/structure.rs`, `tests/conformance/coverage.rs`, `proofs/.../CoverageLedger.lean` + `Catalog.lean`) to register the new machine (mirror how `PairingReconcile` is registered)

- [ ] **Step 1: Model registry + derivation state.** A `RegistryEntry` (peerId, did, live: Bool) finset; the derivation produces a desired-set tagged by ownership. **Decide the representation here by proof cleanliness:** either (a) `desired : peer → Owner × PairingDesired` with `Owner = operator | registry`, or (b) two finsets `operatorDesired` / `registryDesired`. Pick whichever makes the ownership theorems shortest; record the choice in the file's module doc.

```lean
structure RegistryEntry where
  peer : String
  did  : String
  live : Bool
  deriving DecidableEq

/-- Pure derivation: live entries (minus self) become registry-owned desired rows. -/
def deriveRegistryDesired (self : String) (reg : Finset RegistryEntry) : Finset String := ...
```

- [ ] **Step 2: State and prove the four properties** (each a theorem, non-vacuous, quantified over the derivation/transitions — follow the no-vacuity bar from the PairingReconcile review):
  - `derive_idempotent`: `deriveRegistryDesired self (reg) = deriveRegistryDesired self reg` applied twice equals once (trivial for a pure fn; state convergence over a transition system if entries change).
  - `derive_convergent`: a stable `reg` yields a stable derived set across reconcile ticks.
  - `ownership_safe`: a derivation/retraction transition never adds, removes, or mutates an **operator-owned** desired row.
  - `retraction_sound`: removing or staling entry `e` removes exactly `e`'s derived row(s) and no others.
- [ ] **Step 3: Signature guard on join.** Add a `Join` transition (in this model or as an extension note to PairingReconcile) gated by `signedByMember : Token → Registry → Prop`; prove `join_requires_member_signature` — a `Join` step is enabled only when `signedByMember tok reg` (or TOFU bootstrap flag). This fences "non-member invite rejected" as a theorem, not prose.
- [ ] **Step 4: Executable contract** (`Executable.lean`): transition-kind vocabulary + `toContract`/`fromContract?` round-trip theorem (mirror `PairingReconcile/Executable.lean`).
- [ ] **Step 5: Register in the structure fence + coverage ledger** so a missing model is loud (mirror `PairingReconcile` entries).
- [ ] **Step 6: Run** `cd crates/defra-agent/proofs && lake build` — expect success, zero `sorry`.
- [ ] **Step 7: Commit** — `proof(discovery): registry derivation + signature guard; ownership rep = <chosen>`

---

### Task R5: Discovery reconciler (registry → registry-owned desired rows)

Implements the R4 model. **Read R4's chosen ownership representation first.**

**Files:**
- Create: `crates/defra-agent/src/agent/p2p_reconcile/discovery.rs` (`derive_registry_desired`, `reconcile_discovery_tick`)
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs` (`PairingStateStore`/desired row gains the ownership tag per R4; the pairing diff must treat operator-owned rows as authoritative and only let discovery manage registry-owned ones)
- Modify: `commands/p2p/join.rs` (registry-membership check: after TOFU bootstrap, reject issuer not live in `PeerRegistry`)
- Modify: `crates/defra-agent/src/agent/runtime/startup.rs` (spawn discovery in the registry/pairing sweep, or fold into the pairing reconciler loop)
- Test: unit tests in `discovery.rs` (derivation + ownership), membership-check test

- [ ] **Step 1: Write failing unit tests** mirroring the R4 properties in Rust (these are the conformance-to-engine bridge):

```rust
#[test]
fn derive_skips_self_and_offline_and_tags_registry_owned() {
    let reg = vec![entry("self", true), entry("peerA", true), entry("peerB", false)];
    let desired = derive_registry_desired("self", &reg, "default");
    assert!(desired.iter().any(|d| d.peer_id == "peerA" && d.is_registry_owned()));
    assert!(!desired.iter().any(|d| d.peer_id == "self"));   // self excluded
    assert!(!desired.iter().any(|d| d.peer_id == "peerB"));  // offline excluded
}

#[test]
fn discovery_never_mutates_operator_owned_rows() {
    // an operator-owned desired row for peerA + a registry entry for peerA →
    // discovery leaves the operator row intact (no duplicate, no overwrite)
}

#[test]
fn staling_entry_retracts_only_its_registry_owned_row() { /* ... */ }
```

- [ ] **Step 2: Run** `cargo test -p defra-agent discovery` — expect FAIL.
- [ ] **Step 3: Implement `derive_registry_desired`** (pure, mirrors the Lean `deriveRegistryDesired`) and `reconcile_discovery_tick(store, registry_reader)`: read live registry entries, derive registry-owned desired rows, upsert/retract them through the store **without touching operator-owned rows**. Liveness from `updated_at` age vs `REGISTRY_STALE_AFTER`.
- [ ] **Step 4: Ownership tag** — implement the R4-chosen representation on the desired row (a `source: "operator" | "registry"` field on `PeerPairingDesired`, or a parallel store). If it's a schema field, add a migration patch (mirror R1) and default existing rows to `operator`.
- [ ] **Step 5: Membership check on join** — in `join.rs`, after the bootstrap path, if a local `PeerRegistry` exists, require `issuer_did` to be a live registry member; bail otherwise. Gate auto-pair behind a `discovery_auto_pair` runtime setting (read where other runtime settings are read; default false).
- [ ] **Step 6: Spawn / fold the discovery tick** into the reconciler sweep so registry changes (Update events) drive re-derivation (the pairing reconciler already subscribes to `EventName::Update` — extend that loop or run discovery immediately before `sweep_pairings`).
- [ ] **Step 7: Run** `cargo test -p defra-agent discovery` and the full `cargo test -p defra-agent` — expect PASS.
- [ ] **Step 8: Commit** — `feat(p2p): discovery reconciler materializes registry-owned pairings`

---

### Task R6: CLI `p2p network` + conformance mirror

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs` (new `P2pNetworkCommand` under `P2pCommand`; declarative neighbor of `Pairings`)
- Create: `crates/defra-agent-cli/src/commands/p2p/network.rs` (register/list/rm handlers)
- Modify: `crates/defra-agent-cli/src/commands/p2p/mod.rs` (dispatch)
- Modify: `crates/defra-agent/tests/support/pairing_conformance/` + `tests/conformance/` + `tests/fixtures/` (discovery scenarios mirroring R4)
- Test: CLI unit tests in `network.rs`; conformance scenarios

- [ ] **Step 1: Add the CLI surface.** `P2pCommand::Network { command: P2pNetworkCommand }` with `Register(P2pNetworkRegisterArgs)`, `List(P2pNetworkListArgs)`, `Rm(P2pAccessArgs)`. `register` args: `--display-name`, repeated `--profile`, `--network` (default "default"); list args: `--output` (shared `OutputFormat`, Table default). Help frames `p2p network` as "discover and join the peer network" alongside `pairings`.
- [ ] **Step 2: Write failing CLI parse + handler tests** (mirror `commands/p2p/pairings.rs` tests): `network list --output table` parses; `register` builds the self-entry upsert (reuse R2's `registry_upsert_mutation`); `list` annotates each entry with `paired: bool` (join registry vs PeerPairingDesired) and `online: bool` (heartbeat age).
- [ ] **Step 3: Run** `cargo test -p defra-agent-cli --bins network` — expect FAIL.
- [ ] **Step 4: Implement handlers** in `network.rs`: `register` writes self via the shared upsert; `list` reads `PeerRegistry` (+ join `PeerPairingDesired` for `paired`) and renders table (PEER / DID / NAME / ONLINE / PAIRED / PROFILES) or JSON; `rm` deletes the node's own row. All interpolation through `escape_graphql_string`.
- [ ] **Step 5: Conformance mirror** — add discovery scenario fixtures (member appears → derived row; member stales → retract; operator row untouched) and extend the conformance invariants to assert ownership-safety against the real `derive_registry_desired` (call the engine fn from the harness, per the lesson from the pairing-conformance fix — do NOT reimplement the derivation in the harness).
- [ ] **Step 6: Run** `cargo test -p defra-agent-cli --bins` and `cargo test -p defra-agent --test conformance` — expect PASS.
- [ ] **Step 7: Commit** — `feat(cli): p2p network register/list/rm + discovery conformance`

---

### Task R7: Integration tests + docs

**Files:**
- Modify: `crates/defra-agent-cli/tests/cli_p2p.rs` (or a new `cli_p2p_network.rs`) — the 4 integration tests
- Modify: `docs/demo.md` (Part 3 — "join a network") and `docs/operations.md`
- Test: the four scenarios from the spec

- [ ] **Step 1: 3-node transitive discovery test** — boot S, A, B (mirror the existing two-node harness in `cli_p2p.rs`). A `p2p pairings join` S's signed invite; B joins S; assert A and B each self-register; enable auto-pair; assert A discovers B via the replicated registry and a `PeerPairingDesired` for B appears on A **without** a direct A↔B invite; write a doc on A, read it on B.
- [ ] **Step 2: Signed-invite authorization test** — a join with (a) a tampered `sig`, (b) a signature from a non-member DID once a registry exists, is rejected; a valid member signature is accepted and the desired row's `invited_by` equals the issuer DID.
- [ ] **Step 3: Cross-node delegation test** — with pairing live and `subagent_allow_cross_deployment` set on both behaviors' tool selections, a parent on node A delegates to a child behavior on node B and reads the result; with the gate off, the dispatch is refused (proves discovery/replication ≠ authorization). Reuse the subagent dispatch assertions from existing subagent tests.
- [ ] **Step 4: Ownership test** — a stale/removed registry entry retracts only its registry-owned pairing; a manual `p2p pairings set` row for the same peer survives.
- [ ] **Step 5: Run** the full suites — `cargo test -p defra-agent`, `cargo test -p defra-agent-cli`, `lake build` — all green, no ignores, no flakes (capture/file/fix if any).
- [ ] **Step 6: Docs** — add demo Part 3 "Join a network" (self-register → signed invite → join → watch discovery auto-pair via `p2p network list`), and a short operations.md reference. Frame the three layers (discovery/replication/authorization) explicitly.
- [ ] **Step 7: Commit** — `test(p2p): network discovery e2e + signed-invite auth + delegation; docs`

---

## Self-review notes

- **Spec coverage:** R1 (PeerRegistry collection/self-reg), R2 (heartbeat), R3 (signed v2 invites + token→protocol), R4 (Lean derivation + signature guard, ownership-rep decision), R5 (discovery reconciler + membership check + auto-pair), R6 (CLI `p2p network` + conformance), R7 (4 integration tests + delegation-gate e2e + docs). All spec sections map to a task.
- **Lean-first ordering enforced:** R4 (Lean) precedes R5 (Rust derivation); R4 decides the ownership representation R5 implements.
- **Deferred (matches spec):** filtered replication (#1013, upstream), wire-enforced admission + two-sided revocation (#1012/#180). Signed invites are the in-PR auth.
- **Type consistency:** `RegistryEntry` (R2 Rust / R4 Lean), `registry_upsert_mutation` (R2, reused R6), `InviteToken` v2 + `signing_payload`/`encode`/`decode` (R3, used R3 invite/join), `derive_registry_desired` (R5, mirrors Lean `deriveRegistryDesired` R4), `discovery_auto_pair` setting (R5, surfaced R6). Ownership tag representation is intentionally deferred to R4 and consumed consistently in R5/R6.
- **No-vacuity reminder (R4):** the join-signature and ownership theorems must be quantified over all transitions and have satisfiable hypotheses — the conformance harness must call the real `derive_registry_desired`, not a reimplementation (the exact gap caught in the pairing-conformance review).
