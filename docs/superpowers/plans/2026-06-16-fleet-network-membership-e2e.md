# Fleet network-membership arc + 5-process e2e — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the #510 network-membership control-plane (network genesis → membership-gated join → two-layer reconciler) and validate it with a 5-process fleet e2e exercising discovery → join → scoped delegation.

**Architecture:** Two layers. *Layer 1* = a network-membership substrate: an admin-signed `AgentNetwork` + `NetworkMembership` + member-signed `PeerEndpoint`, materialized into a mesh of `source="network"` `PeerPairingDesired` rows by the reconciler (`deriveNetworkDesired`). *Layer 2* = an operator-owned conversation/delegation star in a new `DataPlanePairingDesired` collection. Both are gated by the shared Lean-fenced `decideMaterializable` predicate (membership = master gate), and **merged into one replicator per peer** at install time (defradb holds one `ReplicatorInfo` per peer, filters per-collection). The Lean model (`NetworkMembership.lean`) already proves admission/materialization/revocation; we build Rust + conformance to satisfy it.

**Tech Stack:** Rust (`defra-agent`, `defra-agent-cli`, `defra-agent-protocol`, `defra-agent-schemas`), Lean 4 (`crates/defra-agent/proofs`), DefraDB (pinned `defradb.rs` rev `bddfcea5`), GraphQL over HTTP, Ed25519 signing, iroh P2P.

**Spec:** `docs/superpowers/specs/2026-06-16-fleet-network-membership-e2e-design.md` (decisions D1–D11).

**Process rule (from CLAUDE.md):** Lean → conformance → Rust. Anything changing legal transitions/invariants starts in Lean. Gate with the full package suite (`cargo test -p defra-agent`, `-p defra-agent-cli`), never `--lib`. Zero `sorry`. `tracing`, never `println`. Always `graphql::escape_graphql_string()` for interpolated GraphQL; never emit `[]` in a mutation (use `null`).

**Build order:** Cut 0 → Cut 3 → Cut 4 → Cut 5 → Cut 6. Each cut is an independently testable unit; commit frequently. One PR on branch `fleet-discovery-e2e`.

---

## File Structure

**Cut 0 — interval config**
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/registry.rs` (heartbeat interval), `discovery.rs` (stale-after), `engine.rs` (sweep interval) — read intervals from env via a shared helper.
- Create: `crates/defra-agent/src/agent/p2p_reconcile/intervals.rs` — env-overridable interval resolution (one home for all three + the new endpoint intervals).

**Cut 3 — network control-plane CLI**
- Modify: `crates/defra-agent-cli/src/cli/args.rs` (add `Create`/`Grant`/`Revoke` to `P2pNetworkCommand`; use existing `home: Option<PathBuf>` + `output: OutputFormat` / `--output` style).
- Modify: `crates/defra-agent-cli/src/commands/p2p/network.rs` (handlers).
- Create: `crates/defra-agent-cli/src/commands/p2p/network_admin.rs` (genesis + grant + revoke logic) — keeps `network.rs` focused on register/list/rm.
- Modify: `crates/defra-agent-cli/src/commands/p2p/mod.rs` (wire subcommands).
- Test: `crates/defra-agent-cli/tests/cli_p2p_network.rs` (extend).
- Test: `crates/defra-agent/tests/conformance/peer_registry_discovery.rs` or `fleet.rs` (genesis/grant/revoke conformance vs `validNetwork`/`adminSignedMembership`/`tombstoneState`).

**Cut 4 — membership-gated join + token v5**
- Modify: `crates/defra-agent-protocol/src/pairing_token.rs` (`InviteToken` v5 + `grant`/`network` fields + freshness/decode).
- Modify: `crates/defra-agent-cli/src/cli/args.rs` (add `--member-did` to `P2pInviteArgs` for v5 grant selection).
- Modify: `crates/defra-agent-cli/src/commands/p2p/invite.rs` (mint v5: embed signed grant + network record).
- Modify: `crates/defra-agent-cli/src/commands/p2p/join.rs` (membership-arm admission from signed payload; admin-issued check; grantee check).
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/discovery.rs` (`decide_join_admission` membership arm, or a sibling fn).
- Test: `crates/defra-agent-cli/tests/cli_p2p.rs` (extend); conformance in `peer_registry_discovery.rs`.

**Cut 5 — reconciler + runtime fence (two layers, one gate)**
- Create: `crates/defra-agent-schemas/schemas/agent/data_plane_pairing_desired.graphql`; register in `crates/defra-agent-schemas/src/lib.rs`, `crates/defra-agent-protocol/src/schemas.rs`, `crates/defra-agent/src/schema.rs`, and the runtime migration/ensure path in `crates/defra-agent/src/migration.rs`.
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/templates.rs` (add narrow `network-control` template).
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/registry.rs` or new `endpoint.rs` (signed `PeerEndpoint` heartbeat).
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/discovery.rs` (`deriveNetworkDesired` in Rust → `source="network"` rows; membership gate).
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs` + `diff.rs` (merge Layer-1 + Layer-2 desired per peer; gate on `decideMaterializable`).
- Modify (maybe): `crates/defra-agent/proofs/Proofs/PairingReconcile/State.lean` (per-collection filter generalization) + new data-plane gate lemma in `NetworkMembership.lean`.
- Test: conformance `pairing_reconcile.rs`, `scope_templates.rs`, `peer_registry_discovery.rs`.

**Cut 6 — 5-process e2e**
- Create: `crates/defra-agent-cli/tests/cli_fleet_delegation_live.rs`.
- Reuse: `crates/defra-agent-cli/tests/support/{process,ports,waits,graphql}.rs`.

---

## Cut 0 — Env-overridable intervals (Lean-neutral)

**Files:**
- Create: `crates/defra-agent/src/agent/p2p_reconcile/intervals.rs`
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/mod.rs` (add `mod intervals;`), `registry.rs:20`, `discovery.rs:56-57`, `engine.rs:21`

- [ ] **Step 1: Write the failing test for env override**

Create `crates/defra-agent/src/agent/p2p_reconcile/intervals.rs`:

```rust
//! Env-overridable reconciler intervals. Production defaults match the historic
//! consts; tests set the env vars to compress convergence to ~seconds.
//! Lean-neutral: no transition/invariant/provider-input depends on these values.
use std::time::Duration;

/// Default heartbeat cadence (matches the historic `REGISTRY_HEARTBEAT_INTERVAL`).
pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(30);
/// Default pairing-reconcile sweep cadence.
pub const DEFAULT_SWEEP: Duration = Duration::from_secs(30);
/// Stale multiple: a heartbeat older than `multiple * heartbeat` is not fresh.
pub const DEFAULT_STALE_MULTIPLE: u32 = 3;

fn env_ms(key: &str) -> Option<Duration> {
    std::env::var(key).ok().and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_millis)
}

pub fn heartbeat_interval() -> Duration {
    env_ms("DEFRA_AGENT_REGISTRY_HEARTBEAT_MS").unwrap_or(DEFAULT_HEARTBEAT)
}

pub fn sweep_interval() -> Duration {
    env_ms("DEFRA_AGENT_PAIRING_SWEEP_MS").unwrap_or(DEFAULT_SWEEP)
}

/// Endpoint heartbeat reuses the registry heartbeat env unless overridden,
/// since signed `PeerEndpoint` freshness and registry freshness should not drift.
pub fn endpoint_interval() -> Duration {
    env_ms("DEFRA_AGENT_ENDPOINT_HEARTBEAT_MS").unwrap_or_else(heartbeat_interval)
}

pub fn stale_after() -> Duration {
    env_ms("DEFRA_AGENT_REGISTRY_STALE_MS")
        .unwrap_or_else(|| heartbeat_interval() * DEFAULT_STALE_MULTIPLE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_historic_values() {
        // No env set in this process by default.
        assert_eq!(heartbeat_interval(), Duration::from_secs(30));
        assert_eq!(sweep_interval(), Duration::from_secs(30));
        assert_eq!(stale_after(), Duration::from_secs(90));
    }
}
```

- [ ] **Step 2: Run it to verify it fails to compile/find the module**

Run: `cargo test -p defra-agent p2p_reconcile::intervals -- --nocapture`
Expected: FAIL — `intervals` module not declared in `mod.rs`.

- [ ] **Step 3: Declare the module**

In `crates/defra-agent/src/agent/p2p_reconcile/mod.rs`, add `pub mod intervals;` next to the other `mod` lines.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p defra-agent p2p_reconcile::intervals`
Expected: PASS.

- [ ] **Step 5: Replace the const reads at the three call sites**

In `registry.rs`, replace uses of `REGISTRY_HEARTBEAT_INTERVAL` (the `tokio::time::interval(...)` construction in `run_registry_heartbeat`) with `super::intervals::heartbeat_interval()`. Keep the `pub const REGISTRY_HEARTBEAT_INTERVAL` for back-compat references but stop using it for the live loop.
In `engine.rs`, replace the `PAIRING_SWEEP_INTERVAL` interval construction with `super::intervals::sweep_interval()`.
In `discovery.rs`, change `heartbeat_is_fresh` / liveness call sites that pass `REGISTRY_STALE_AFTER` to call `super::intervals::stale_after()` at the point of use (do NOT make the `const` itself dynamic — it can't be; thread the value in). Grep `REGISTRY_STALE_AFTER` and replace each *runtime* use with `intervals::stale_after()`; leave the const for conformance/tests that compare to the default.

- [ ] **Step 6: Run the full p2p_reconcile suite**

Run: `cargo test -p defra-agent p2p_reconcile`
Expected: PASS (no behavior change at default env).

- [ ] **Step 7: Commit**

```bash
git add crates/defra-agent/src/agent/p2p_reconcile/
git commit -m "feat(p2p): env-overridable reconciler intervals (cut 0)"
```

---

## Cut 3 — Network control-plane CLI (genesis + grant + revoke)

The schemas already exist (`agent_network.graphql`, `network_membership.graphql`, `peer_endpoint.graphql`) and are registered (`schemas/src/lib.rs:55-62`, `schema.rs`). The signed canonical forms exist in `network_token.rs` (`NetworkRecord`, `MembershipRecord`, `EndpointRecord` with `signing_payload_impl!`). This cut adds the CLI that writes them.

### Task 3.1 — `network_id` derivation helper (conformance-fenced)

**Files:**
- Modify: `crates/defra-agent-protocol/src/network_token.rs` (add `derive_network_id`)
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

In `network_token.rs` tests module:

```rust
#[test]
fn network_id_is_deterministic_and_admin_bound() {
    let a = derive_network_id("did:key:zAdmin", "Fleet One");
    let b = derive_network_id("did:key:zAdmin", "Fleet One");
    let other_admin = derive_network_id("did:key:zOther", "Fleet One");
    let other_name = derive_network_id("did:key:zAdmin", "Fleet Two");
    assert_eq!(a, b, "deterministic");
    assert_ne!(a, other_admin, "admin-bound");
    assert_ne!(a, other_name, "name-bound");
    assert!(a.starts_with("net-"), "stable, recognizable prefix");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p defra-agent-protocol network_id_is_deterministic`
Expected: FAIL — `derive_network_id` not found.

- [ ] **Step 3: Implement**

```rust
/// Deterministic, admin-bound network id computed BEFORE signing (D4).
/// It is a SIGNED field of `AgentNetwork`, so it must not depend on `_docID`.
pub fn derive_network_id(admin_did: &str, name: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(admin_did.as_bytes());
    h.update(b"\x1f"); // unit separator — avoids ("a","bc") == ("ab","c")
    h.update(name.as_bytes());
    let digest = h.finalize();
    format!("net-{}", bs58::encode(&digest[..16]).into_string())
}
```

(Confirm `sha2` + `bs58` are already deps of `defra-agent-protocol`; they back the existing token encoding. If `sha2` is absent, add it to that crate's `Cargo.toml`.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p defra-agent-protocol network_id_is_deterministic`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-protocol/src/network_token.rs crates/defra-agent-protocol/Cargo.toml
git commit -m "feat(protocol): derive_network_id (deterministic, admin-bound) (cut 3)"
```

### Task 3.2 — CLI args for `create`/`grant`/`revoke`

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs:1812` (`P2pNetworkCommand` enum)

- [ ] **Step 1: Add the variants + arg structs**

In `args.rs`, extend `P2pNetworkCommand`:

```rust
pub(crate) enum P2pNetworkCommand {
    Register(P2pNetworkRegisterArgs),
    List(P2pNetworkListArgs),
    /// Existing registry deregistration remains the generic access-args shape.
    Rm(P2pAccessArgs),
    /// Genesis: create the single AgentNetwork doc (admin-only, singleton).
    Create(P2pNetworkCreateArgs),
    /// Admin grants an active NetworkMembership to a member DID.
    Grant(P2pNetworkGrantArgs),
    /// Admin revokes a membership (status=revoked tombstone, row retained).
    Revoke(P2pNetworkRevokeArgs),
}

#[derive(Debug, clap::Args)]
pub(crate) struct P2pNetworkCreateArgs {
    /// Human-readable network name; network_id is derived from (admin_did, name).
    #[arg(long)]
    pub name: String,
    #[arg(long)]
    pub home: Option<std::path::PathBuf>,
    #[arg(long)]
    pub graphql: Option<String>,
    #[arg(long = "output", value_enum, default_value_t = OutputFormat::Text)]
    pub output: OutputFormat,
}

#[derive(Debug, clap::Args)]
pub(crate) struct P2pNetworkGrantArgs {
    /// The member DID to admit.
    pub member_did: String,
    #[arg(long)]
    pub home: Option<std::path::PathBuf>,
    #[arg(long)]
    pub graphql: Option<String>,
    #[arg(long = "output", value_enum, default_value_t = OutputFormat::Text)]
    pub output: OutputFormat,
}

#[derive(Debug, clap::Args)]
pub(crate) struct P2pNetworkRevokeArgs {
    pub member_did: String,
    #[arg(long)]
    pub home: Option<std::path::PathBuf>,
    #[arg(long)]
    pub graphql: Option<String>,
    #[arg(long = "output", value_enum, default_value_t = OutputFormat::Text)]
    pub output: OutputFormat,
}
```

(For `rm`, keep using the existing `P2pAccessArgs`. Match the existing `home`/`graphql`/`output` arg style already on `P2pNetworkRegisterArgs` / `P2pNetworkListArgs`.)

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p defra-agent-cli`
Expected: FAIL — `mod.rs` match arms not yet added (next task). That's expected; proceed.

### Task 3.3 — Genesis (`create`) handler with singleton guard + admin self-membership

**Files:**
- Create: `crates/defra-agent-cli/src/commands/p2p/network_admin.rs`
- Modify: `crates/defra-agent-cli/src/commands/p2p/mod.rs` (declare module + wire arms)
- Test: `crates/defra-agent-cli/tests/cli_p2p_network.rs`

- [ ] **Step 1: Write the failing integration test (daemon-spawned)**

In `crates/defra-agent-cli/tests/cli_p2p_network.rs`, add (using the support harness — see existing tests in this file for the exact `spawn_server_with_ready_json` / `run_cli_json` / `graphql_query` calls):

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_create_is_singleton_and_writes_admin_membership() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let home = tmp.path().join("admin");
    std::fs::create_dir_all(&home)?;
    let port = allocate_port()?;
    // init + spawn daemon (copy the init+spawn preamble from the register test in this file)
    let (mut serve, _ready) = spawn_server_with_ready_json(&home, port, &[], &[])?;
    wait_for_port(port, &mut serve)?;
    let gql = graphql_url(port);
    let agent_did = /* read from init/runtime as the register test does */;
    wait_for_runtime_ready(&gql, &agent_did, std::time::Duration::from_secs(30)).await?;

    // create
    let created = run_cli_json(&home, &["p2p", "network", "create", "--name", "Fleet One", "--output", "json"])?;
    let network_id = created["network_id"].as_str().expect("network_id").to_string();
    assert!(network_id.starts_with("net-"));

    // AgentNetwork exists with admin_sig
    let net = graphql_query(&gql, r#"query { AgentNetwork { network_id admin_did admin_sig } }"#).await?;
    assert_eq!(net["data"]["AgentNetwork"].as_array().unwrap().len(), 1);

    // admin's own active membership exists (genesis member)
    let mem = graphql_query(&gql, r#"query { NetworkMembership { member_did status } }"#).await?;
    let rows = mem["data"]["NetworkMembership"].as_array().unwrap();
    assert!(rows.iter().any(|r| r["member_did"] == serde_json::json!(agent_did) && r["status"] == "active"));

    // singleton guard: a second create errors
    let second = run_cli_json(&home, &["p2p", "network", "create", "--name", "Fleet Two", "--output", "json"]);
    assert!(second.is_err() || second.unwrap()["error"].is_string(), "second create must be rejected");
    Ok(())
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent-cli --test cli_p2p_network network_create_is_singleton`
Expected: FAIL — unknown subcommand `create`.

- [ ] **Step 3: Implement the genesis handler**

Create `network_admin.rs`. Read first: `network.rs:52-139` (`p2p_network_register`) for the `resolve_config_access` / mutation-render conventions, `invite.rs` for the synchronous `resolve_home_identity` helper, and `network_token.rs` for `NetworkRecord::signing_payload`. Then:

```rust
//! `p2p network create|grant|revoke`: the admin-signed control-plane writes
//! (genesis AgentNetwork + NetworkMembership grants/tombstones).
use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_agent::graphql::escape_graphql_string;
use defra_agent_protocol::network_token::{derive_network_id, NetworkRecord, MembershipRecord, EndpointRecord};
// ... resolve_config_access, graphql helpers (see network.rs imports)
// ... resolve_home_identity (import/reuse the synchronous helper from invite.rs, or move it to a shared p2p helper)

pub(super) async fn p2p_network_create(args: P2pNetworkCreateArgs) -> Result<()> {
    let (access, _home) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), true).await?;
    let identity = resolve_home_identity(args.home.as_deref())?;
    let admin_did = identity.did().to_string();

    // Singleton guard: refuse if an AgentNetwork already exists locally.
    let existing = graphql_rows(&access, "AgentNetwork", "query { AgentNetwork { network_id } }").await?;
    if !existing.is_empty() {
        bail!("a network already exists on this node (network_id={}); create is singleton", existing[0]["network_id"]);
    }

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let network_id = derive_network_id(&admin_did, &args.name);
    let default_template = "network-control".to_string();

    // Sign the network record (admin_sig over signing fields).
    let mut rec = NetworkRecord { network_id: network_id.clone(), admin_did: admin_did.clone(),
        display_name: args.name.clone(), default_template: default_template.clone(),
        created_at: now.clone(), sig: Vec::new() };
    let net_sig = identity.sign(&rec.signing_payload()).await?;
    rec.sig = net_sig.clone();

    // 1) AgentNetwork doc
    write_agent_network(&access, &rec).await?;
    // 2) admin's own active membership (genesis member)
    let mut mem = MembershipRecord { network_id: network_id.clone(), member_did: admin_did.clone(),
        status: "active".into(), granted_at: now.clone(), revoked_at: String::new(), sig: Vec::new() };
    mem.sig = identity.sign(&mem.signing_payload()).await?;
    write_membership(&access, &mem).await?;
    // 3) admin's signed PeerEndpoint (binding_sig) — reuse the endpoint publish helper if one exists,
    //    else write here from EndpointRecord with node_id/address read from live p2p status (see network.rs:55-87).
    publish_self_endpoint(&access, args.home.as_deref(), &args.graphql, &identity).await?;

    print_network_created(&args.output, &network_id, &admin_did)?;
    Ok(())
}
```

Implement the small `write_agent_network` / `write_membership` upsert helpers in this file using `escape_graphql_string` and emitting `null` (never `[]`) for empty list fields. Render `AgentNetwork.admin_sig`, `NetworkMembership.admin_sig`, and `PeerEndpoint.binding_sig` as base58 or hex consistently with how `network_token` encodes signatures (check `encode`/`decode` in `pairing_token.rs`).

- [ ] **Step 4: Wire the match arms**

In `mod.rs`, add `mod network_admin;` and:

```rust
P2pNetworkCommand::Create(args) => network_admin::p2p_network_create(args).await,
P2pNetworkCommand::Grant(args) => network_admin::p2p_network_grant(args).await,
P2pNetworkCommand::Revoke(args) => network_admin::p2p_network_revoke(args).await,
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p defra-agent-cli --test cli_p2p_network network_create_is_singleton -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-cli/src/commands/p2p/ crates/defra-agent-cli/src/cli/args.rs crates/defra-agent-cli/tests/cli_p2p_network.rs
git commit -m "feat(cli): p2p network create — genesis + singleton guard + admin self-membership (cut 3)"
```

### Task 3.4 — `grant` + `revoke` handlers

**Files:** `network_admin.rs`, test `cli_p2p_network.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grant_then_revoke_writes_active_then_tombstone() -> anyhow::Result<()> {
    // ... create network on admin daemon (reuse the preamble) ...
    let member = "did:key:zMember123";
    run_cli_json(&home, &["p2p", "network", "grant", member, "--output", "json"])?;
    let member_escaped = escape_graphql_string(member);
    let after_grant = graphql_query(&gql, &format!(
        r#"query {{ NetworkMembership(filter: {{ member_did: {{ _eq: "{member_escaped}" }} }}) {{ status revoked_at }} }}"#)).await?;
    assert_eq!(after_grant["data"]["NetworkMembership"][0]["status"], "active");

    run_cli_json(&home, &["p2p", "network", "revoke", member, "--output", "json"])?;
    let after_revoke = graphql_query(&gql, &format!(
        r#"query {{ NetworkMembership(filter: {{ member_did: {{ _eq: "{member_escaped}" }} }}) {{ status revoked_at }} }}"#)).await?;
    // tombstone retained, not deleted
    assert_eq!(after_revoke["data"]["NetworkMembership"].as_array().unwrap().len(), 1);
    assert_eq!(after_revoke["data"]["NetworkMembership"][0]["status"], "revoked");
    assert!(after_revoke["data"]["NetworkMembership"][0]["revoked_at"].as_str().unwrap().len() > 0);
    Ok(())
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent-cli --test cli_p2p_network grant_then_revoke`
Expected: FAIL — unknown subcommand `grant`.

- [ ] **Step 3: Implement `p2p_network_grant` / `p2p_network_revoke`**

Both read the local `AgentNetwork` to learn `network_id` + assert the caller is the admin (`identity.did() == admin_did`, else `bail!`). `grant` writes an active `MembershipRecord` (admin-signed). `revoke` upserts the SAME `membership_key` with `status="revoked"` + `revoked_at` set, re-signed (the row is retained — upsert update, not delete). Use `escape_graphql_string`; emit `null` for unused list fields.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p defra-agent-cli --test cli_p2p_network grant_then_revoke -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/p2p/network_admin.rs crates/defra-agent-cli/tests/cli_p2p_network.rs
git commit -m "feat(cli): p2p network grant/revoke — active membership + retained tombstone (cut 3)"
```

### Task 3.5 — Conformance: control-plane writes vs Lean predicates

**Files:** `crates/defra-agent/tests/conformance/peer_registry_discovery.rs` (or `fleet.rs`)

- [ ] **Step 1: Write conformance assertions**

Add tests that build a `NetworkRecord` / `MembershipRecord` and assert: (a) `validNetwork` holds iff `admin_sig` verifies (mirror `forged_membership_not_admitted` for the network); (b) a forged (unsigned/bad-sig) membership is NOT an admitted member — exercise the same `decide_join_admission`/`admittedMember` input projection the join path uses; (c) a `status="revoked"` tombstone row projects to `active=false` (the `tombstoneState` ≡ erase property — assert the derived admitted set excludes it). Read `NetworkMembership.lean` for the exact predicate names and `tests/conformance/structure.rs` for the model→home mapping convention.

- [ ] **Step 2: Run**

Run: `cargo test -p defra-agent --test conformance peer_registry_discovery`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/tests/conformance/
git commit -m "test(conformance): control-plane writes match validNetwork/admittedMember/tombstone (cut 3)"
```

---

## Cut 4 — Membership-gated join + InviteToken v5

### Task 4.1 — `InviteToken` v5: embed signed grant + network record

**Files:** `crates/defra-agent-protocol/src/pairing_token.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn v5_round_trips_with_grant_and_network() {
    let tok = sample_v5_token(); // construct with grant: MembershipRecord, network: NetworkRecord
    let encoded = encode_invite(&tok).expect("encode");
    let decoded = decode(&encoded).expect("decode");
    assert_eq!(decoded.v, 5);
    assert_eq!(decoded.grant.member_did, tok.grant.member_did);
    assert_eq!(decoded.network.network_id, tok.network.network_id);
}

#[test]
fn pre_v5_tokens_rejected_with_reissue_hint() {
    // a v4 token (no grant/network) decodes to an error mentioning re-issue
    let err = decode(&sample_v4_encoded()).unwrap_err();
    assert!(err.to_string().contains("re-issue") || err.to_string().contains("v5"));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent-protocol v5_round_trips`
Expected: FAIL — `grant`/`network` fields don't exist; `v` is 4.

- [ ] **Step 3: Implement v5**

Add to `InviteToken`: `pub grant: MembershipRecord,` and `pub network: NetworkRecord,` (both from `network_token`). Bump the minted `v` to 5 and `signing_payload` to include the new fields (they're inside the issuer's `sig`, so a tampered grant breaks the issuer signature too). Update `decode` to reject `v < 5` with the existing re-issue-hint pattern (mirror the current `v < 4` arm).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p defra-agent-protocol pairing_token`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-protocol/src/pairing_token.rs
git commit -m "feat(protocol): InviteToken v5 carries admin-signed grant + network record (cut 4)"
```

### Task 4.2 — `invite` mints v5 (admin-issued)

**Files:** `crates/defra-agent-cli/src/commands/p2p/invite.rs`, `crates/defra-agent-cli/src/cli/args.rs`

- [ ] **Step 1: Write a CLI test (extend `cli_p2p.rs`)**

Assert that `p2p pairings invite --member-did <peerB_did> --template network-control` from the admin daemon (after `network create` + `grant <peerB_did>`) emits a token that `decode`s to v5 with `grant.member_did == peerB_did` and `network.admin_did == admin_did`. (Run the CLI, capture stdout token, `decode` it in-test.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent-cli --test cli_p2p invite_mints_v5`
Expected: FAIL — invite still mints v4 / has no grant.

- [ ] **Step 3: Implement**

In `args.rs`, add `member_did: Option<String>` to `P2pInviteArgs` as `#[arg(long = "member-did")]`; require it for v5 network-control invites. In `invite.rs`, after resolving the live peer id/ticket (existing `build_live_token`), additionally: load the local `AgentNetwork` (admin record) and the `NetworkMembership` grant for the invitee DID. Populate `grant` + `network` from those signed rows, set the enrollment `template` default to `network-control`, and sign as today. Fail if the caller is not the admin (`issuer_did == network.admin_did`).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p defra-agent-cli --test cli_p2p invite_mints_v5 -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/p2p/invite.rs crates/defra-agent-cli/src/cli/args.rs
git commit -m "feat(cli): p2p pairings invite mints v5 with embedded grant (admin-issued) (cut 4)"
```

### Task 4.3 — `join` admits from the signed payload (membership arm)

**Files:** `crates/defra-agent-cli/src/commands/p2p/join.rs`, `crates/defra-agent/src/agent/p2p_reconcile/discovery.rs`

- [ ] **Step 1: Write the failing test (extend `cli_p2p.rs`)**

Two-daemon test: admin `create` + `grant <B>` + `invite --member-did <B>`; B `join`s. Assert: join succeeds, B's `NetworkMembership(member_did=B,status=active)` is durably written, and the nonce is single-use (a replayed join is rejected — extend the existing replay test). Add a negative: a token whose `grant.member_did != B` (wrong grantee) is rejected; a token whose `issuer_did != network.admin_did` is rejected.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent-cli --test cli_p2p join_membership_arm`
Expected: FAIL — join doesn't yet verify/persist the grant.

- [ ] **Step 3: Implement the gate**

In `join.rs`, after the existing signature/freshness/network-match steps and BEFORE the nonce burn, add the membership-arm checks (D9): verify `remote.network.sig` (admin) and `remote.grant.sig` (admin) via `identity.verify`; require `remote.issuer_did == remote.network.admin_did` (admin-issued, v1); require `remote.grant.member_did == identity.did()` (this node is the grantee) and `remote.grant.status == "active"` and `remote.grant.network_id == remote.network.network_id == remote.network_id`. On success, write the durable `NetworkMembership` (the grant) + `AgentNetwork` (the network record) locally so the reconciler can backfill. In `discovery.rs`, extend `decide_join_admission` (or add `decide_join_admission_membership`) to express the `admittedMember` arm as a pure fn over the carried signed grant, conformance-fenced against `NetworkMembership.lean`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p defra-agent-cli --test cli_p2p join_membership_arm -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Conformance**

Add to `tests/conformance/peer_registry_discovery.rs`: the membership-arm decision matches `admittedMember`; a forged grant is rejected (`forged_membership_not_admitted`); membership growth requires admin sig (`membership_growth_requires_admin_sig`).

Run: `cargo test -p defra-agent --test conformance peer_registry_discovery`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-cli/src/commands/p2p/join.rs crates/defra-agent/src/agent/p2p_reconcile/discovery.rs crates/defra-agent/tests/conformance/
git commit -m "feat: join admits from signed grant (membership arm, admin-issued) + conformance (cut 4)"
```

---

## Cut 5 — Reconciler + runtime fence (two layers, one gate, merged install)

### Task 5.1 — Narrow `network-control` template

**Files:** `crates/defra-agent/src/agent/p2p_reconcile/templates.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn network_control_template_is_control_plane_only() {
    let t = resolve_template("network-control").expect("template exists");
    let cols: std::collections::BTreeSet<_> = t.collections.iter().copied().collect();
    assert_eq!(cols, ["AgentNetwork","NetworkMembership","PeerEndpoint","NetworkJoinRequest"].into_iter().collect());
    // no agent-config collections leak into the mesh
    assert!(!cols.contains("AgentBehavior"));
    assert!(!cols.contains("ToolSelection"));
    assert!(matches!(t.scope, Scope::Unscoped));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent network_control_template`
Expected: FAIL — no such template.

- [ ] **Step 3: Implement**

Add `const NETWORK_CONTROL_COLLECTIONS: &[&str] = &["AgentNetwork","NetworkMembership","PeerEndpoint","NetworkJoinRequest"];` and a `ScopeTemplate { id: "network-control", collections: NETWORK_CONTROL_COLLECTIONS, scope: Scope::Unscoped, delivery: Delivery::Replicate }` entry in `BUILTIN_TEMPLATES`. Leave `discovery` (config-bearing on-ramp) unchanged.

- [ ] **Step 4: Run / commit**

Run: `cargo test -p defra-agent scope_templates network_control_template`
```bash
git add crates/defra-agent/src/agent/p2p_reconcile/templates.rs
git commit -m "feat(p2p): narrow network-control template (mesh carries no agent config) (cut 5)"
```

### Task 5.2 — Signed `PeerEndpoint` heartbeat

**Files:** `crates/defra-agent/src/agent/p2p_reconcile/registry.rs` (or new `endpoint.rs`), `startup.rs`

- [ ] **Step 1: Write conformance/unit test**

Test that the published `PeerEndpoint` row has a `binding_sig` that verifies under the local DID over `EndpointRecord::signing_payload` (mirror Lean `memberSignedEndpoint`), and that freshness uses `intervals::endpoint_interval()`/`stale_after()`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent peer_endpoint_heartbeat`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add a heartbeat that, on the `intervals::endpoint_interval()` cadence, builds an `EndpointRecord { did, node_id, address, updated_at, sig }`, signs it, and upserts the `PeerEndpoint` row. Spawn it in `startup.rs` alongside `run_registry_heartbeat` (read `startup.rs:283-296` for the spawn pattern). Resolve `node_id`/`address` from the runtime's embedded P2P handle / node state, following the existing registry heartbeat boundary; do not depend on the CLI-only `load_live_http_p2p_status` helper from runtime code.

- [ ] **Step 4: Run / commit**

Run: `cargo test -p defra-agent peer_endpoint_heartbeat`
```bash
git add crates/defra-agent/src/agent/p2p_reconcile/ crates/defra-agent/src/agent/startup.rs
git commit -m "feat(p2p): signed PeerEndpoint heartbeat (memberSignedEndpoint) (cut 5)"
```

### Task 5.3 — Lean alignment for the data plane + per-collection filter (Lean-first)

**Files:** `crates/defra-agent/proofs/Proofs/PairingReconcile/State.lean`, `proofs/Proofs/PeerRegistryDiscovery/NetworkMembership.lean`

- [ ] **Step 1: Decide + state the obligations**

Read `State.lean:29-45` (`ReplicatorId = String × Option ScopeFilterKey`). Two questions, resolved IN LEAN before Rust:
  1. **Per-collection filter:** the merged install uses a per-collection filter map (one replicator carries unfiltered control + filtered conversation collections). Confirm the existing `PairingFilters`/per-collection modeling already covers this, or generalize `ReplicatorId`'s filter to a per-collection map and re-prove `filter_change_forces_reinstall`. Keep zero `sorry`.
  2. **Data-plane gate lemma:** add a lemma that a Layer-2 (data-plane) edge to peer `d` is materializable only if `d` is an admitted member — i.e. the data-plane reconciler's desired set ⊆ `deriveNetworkDesired`'s membership domain. Reuse `decideMaterializable`; state and prove `dataPlaneEdge d → admittedMember`-shaped obligation.

- [ ] **Step 2: Build the proofs**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: builds, zero `sorry` (grep `sorry` in changed files → none).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent/proofs/
git commit -m "proof: data-plane edges ⊆ admitted members + per-collection filter identity (cut 5)"
```

### Task 5.4 — `DataPlanePairingDesired` schema + registration

**Files:** Create `crates/defra-agent-schemas/schemas/agent/data_plane_pairing_desired.graphql`; modify `crates/defra-agent-schemas/src/lib.rs`, `crates/defra-agent-protocol/src/schemas.rs`, `crates/defra-agent/src/schema.rs`, `crates/defra-agent/src/migration.rs`

- [ ] **Step 1: Write the schema (mirror `peer_pairing_desired.graphql`)**

```graphql
# Layer-2 (application data plane) pairing intent: operator-owned conversation/
# delegation edges (star). Separate from PeerPairingDesired (Layer-1 substrate)
# so the two never collide on the unique peer_id; merged into one replicator per
# peer at install behind the decideMaterializable gate.
type DataPlanePairingDesired {
    peer_id: String @index(unique: true)
    agent_did: String @index
    collections: [String!]!
    replicator_addresses: [String!]!
    template: String @index
    source: String @index
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
}
```

- [ ] **Step 2: Register it**

In `schemas/src/lib.rs`: add `pub const DATA_PLANE_PAIRING_DESIRED_NAME: &str = "DataPlanePairingDesired";` + `pub const DATA_PLANE_PAIRING_DESIRED: &str = include_str!("../schemas/agent/data_plane_pairing_desired.graphql");` and add both to the schema-body list (~line 85) and name list (~line 115). In `defra-agent-protocol/src/schemas.rs`, re-export the new schema/name and include it in `ALL` / `ALL_COLLECTION_NAMES`. In `schema.rs`, add the `... as ..._SCHEMA` import and include it in the runtime schema set used by `ensure_runtime_schemas`. In `migration.rs`, add the idempotent runtime migration/ensure hook for upgraded DBs and wire it into `ensure_all_runtime_migrations`.

- [ ] **Step 3: Write/Run a schema-load test**

Add a test that `ensure_runtime_schemas` succeeds and a trivial `DataPlanePairingDesired` upsert + query round-trips on an `EmbeddedNode`.

Run: `cargo test -p defra-agent data_plane_pairing_desired_schema`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-schemas/ crates/defra-agent-protocol/src/schemas.rs crates/defra-agent/src/schema.rs crates/defra-agent/src/migration.rs
git commit -m "feat(schema): DataPlanePairingDesired collection (Layer-2 desired) (cut 5)"
```

### Task 5.5 — `deriveNetworkDesired` in Rust → `source="network"` mesh rows

**Files:** `crates/defra-agent/src/agent/p2p_reconcile/discovery.rs`

- [ ] **Step 1: Write the failing conformance test**

In `tests/conformance/peer_registry_discovery.rs`: given a `NetworkState` (admin + N active members each with a fresh signed `PeerEndpoint`), the Rust `derive_network_desired` returns exactly the admitted members minus self (matches Lean `deriveNetworkDesired` / `mem_deriveNetworkDesired`), each carrying the `network-control` template; a revoked member is excluded.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent --test conformance derive_network_desired`
Expected: FAIL — function not present.

- [ ] **Step 3: Implement**

Add `derive_network_desired` reading `NetworkMembership` (active, admin-signed) ∩ fresh `PeerEndpoint`, excluding self, materializing `source="network"` `PeerPairingDesired` rows with `template="network-control"`. Mirror the existing `derive_registry_desired` partition discipline (only touch `source="network"` rows; never operator rows). Read `discovery.rs:252-258` (existing derive) + `:659-681` (upsert render).

- [ ] **Step 4: Run / commit**

Run: `cargo test -p defra-agent --test conformance derive_network_desired`
```bash
git add crates/defra-agent/src/agent/p2p_reconcile/discovery.rs crates/defra-agent/tests/conformance/
git commit -m "feat(p2p): derive_network_desired → source=network mesh rows (cut 5)"
```

### Task 5.6 — Merged install + `decideMaterializable` master gate

**Files:** `crates/defra-agent/src/agent/p2p_reconcile/engine.rs`, `diff.rs`

- [ ] **Step 1: Write the failing conformance test**

In `tests/conformance/pairing_reconcile.rs`: for a coordinator↔subagent peer that has BOTH a `source="network"` `PeerPairingDesired` (control-plane, unfiltered) AND a `DataPlanePairingDesired` (conversation, peer-DID-filtered), the reconciler installs ONE replicator whose `replicator_collections` = control ∪ conversation, whose subscription `collections` stay limited to network-control collections, and whose filter map scopes ONLY the conversation collections. Then: revoking the subagent's membership (so `decideMaterializable` is false) retracts the WHOLE replicator (both layers).

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p defra-agent --test conformance merged_install_membership_gate`
Expected: FAIL.

- [ ] **Step 3: Implement**

In `engine.rs reconcile_peer_tick`: (a) load BOTH desired collections for the peer; (b) gate on `decide_materializable(peer)` — if not an active admitted member, treat desired as empty (retract); (c) else compute the merged desired carefully:

- `collections` = Layer-1 network-control subscription collections only.
- `replicator_collections` = Layer-1 network-control collections ∪ Layer-2 conversation/delegation collections.
- `replicator_filter` = per-collection map with entries only for the Layer-2 conversation/delegation collections via `scope_filter`; leave network-control collections unfiltered.

Then (d) diff against the single `PeerPairingApplied` row + actual; (e) install via the existing `add_replicator` path with the merged `replicator_collections` (full replace). Use `remove_replicator_collections` semantics for collection-scoped teardown when only one layer's carried collections change (read embedded_impl `delete_replicator`). No `PeerPairingApplied`/`diff.rs` schema change (per §6a).

- [ ] **Step 4: Run / commit**

Run: `cargo test -p defra-agent --test conformance pairing_reconcile`
```bash
git add crates/defra-agent/src/agent/p2p_reconcile/engine.rs crates/defra-agent/src/agent/p2p_reconcile/diff.rs crates/defra-agent/tests/conformance/
git commit -m "feat(p2p): merged per-peer install + decideMaterializable master gate (cut 5)"
```

### Task 5.7 — Full p2p_reconcile + conformance gate

- [ ] **Step 1: Run the whole suite**

Run: `cargo test -p defra-agent` (full package — NOT `--lib`)
Expected: PASS. Then `cd crates/defra-agent/proofs && lake build` → zero `sorry`.

- [ ] **Step 2: Commit any fixups**

```bash
git add -A && git commit -m "test: full defra-agent suite green after cut 5"
```

---

## Cut 6 — 5-process fleet e2e (the capstone)

**Files:** Create `crates/defra-agent-cli/tests/cli_fleet_delegation_live.rs`. Reuse `tests/support/{process,ports,waits,graphql}.rs`.

### Task 6.1 — Fleet bring-up harness helper

- [ ] **Step 1: Write a helper that spawns N daemons**

```rust
const P2P_LOOPBACK_ARGS: &[&str] = &[
    "--p2p-bind-addr", "127.0.0.1",
    "--p2p-port", "0",
    "--p2p-relay-mode", "disabled",
    "--p2p-discovery", "disabled",
];

struct FleetNode { home: std::path::PathBuf, port: u16, serve: ServeProcess, gql: String, agent_did: String }

async fn bring_up_fleet(tmp: &std::path::Path, n: usize, model_endpoint: &str, model: &str)
    -> anyhow::Result<Vec<FleetNode>>
{
    let mut nodes = Vec::new();
    for i in 0..n {
        let home = tmp.join(format!("node-{i}"));
        std::fs::create_dir_all(&home)?;
        // init each daemon with its own behavior + the shared live backend (copy the init
        // preamble from cli_live.rs; set the InferenceBackend endpoint/model to the live server).
        let port = allocate_port()?;
        let envs = [
            ("DEFRA_AGENT_REGISTRY_HEARTBEAT_MS", "1000"),
            ("DEFRA_AGENT_PAIRING_SWEEP_MS", "1000"),
            ("DEFRA_AGENT_REGISTRY_STALE_MS", "5000"),
            ("DEFRA_AGENT_ENDPOINT_HEARTBEAT_MS", "1000"),
        ];
        let (mut serve, _ready) = spawn_server_with_ready_json(&home, port, P2P_LOOPBACK_ARGS, &envs)?;
        wait_for_port(port, &mut serve)?;
        let gql = graphql_url(port);
        let agent_did = read_agent_did(&home)?; // from init json / runtime, as cli_live.rs does
        wait_for_runtime_ready(&gql, &agent_did, std::time::Duration::from_secs(30)).await?;
        nodes.push(FleetNode { home, port, serve, gql, agent_did });
    }
    Ok(nodes)
}
```

- [ ] **Step 2: Compile-check**

Run: `cargo test -p defra-agent-cli --test cli_fleet_delegation_live -- --list`
Expected: compiles (no tests run yet).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent-cli/tests/cli_fleet_delegation_live.rs
git commit -m "test(e2e): fleet bring-up harness helper (cut 6)"
```

### Task 6.2 — The full fleet test (gated live)

- [ ] **Step 1: Write the test (one `#[ignore]` + env-gated test)**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: set DEFRA_AGENT_LIVE_OPENAI=1 + endpoint/model, run with --ignored"]
async fn five_process_fleet_discovery_join_pairing_delegation() -> anyhow::Result<()> {
    if std::env::var("DEFRA_AGENT_LIVE_OPENAI").as_deref() != Ok("1") {
        tracing::info!("DEFRA_AGENT_LIVE_OPENAI != 1; skipping fleet live e2e");
        return Ok(());
    }
    let endpoint = std::env::var("DEFRA_AGENT_LIVE_OPENAI_ENDPOINT")?; // DeepSeek server
    let model = std::env::var("DEFRA_AGENT_LIVE_OPENAI_MODEL")?;
    let tmp = tempfile::tempdir()?;
    let fleet = bring_up_fleet(tmp.path(), 5, &endpoint, &model).await?;
    let (coord, subs) = fleet.split_first().unwrap();

    // 1) genesis
    let created = run_cli_json(&coord.home, &["p2p","network","create","--name","FleetE2E","--output","json"])?;
    let network_id = created["network_id"].as_str().unwrap().to_string();

    // 2) serial grant + invite + join for each subagent (Layer-1 substrate)
    for s in subs {
        run_cli_json(&coord.home, &["p2p","network","grant", &s.agent_did, "--output","json"])?;
        let tok = run_cli_text(&coord.home, &["p2p","pairings","invite","--member-did",&s.agent_did,"--template","network-control"])?;
        let token = extract_token(&tok); // dapair1-... line
        run_cli_json(&s.home, &["p2p","pairings","join", &token, "--wait"])?;
    }

    // Do not depend on the legacy reciprocal-token path here. A member's
    // network-control join pushes its signed membership/endpoint to the admin;
    // the admin then materializes the reverse Layer-1 edge through
    // deriveNetworkDesired. Poll for that reverse desired/applied edge below.

    // ASSERT discovery/membership convergence (poll, bounded ~30s with fast intervals)
    for n in &fleet {
        wait_until(std::time::Duration::from_secs(30), || async {
            let m = graphql_query(&n.gql, r#"query { NetworkMembership(filter:{status:{_eq:"active"}}) { member_did } }"#).await.ok()?;
            let count = m["data"]["NetworkMembership"].as_array()?.len();
            (count >= 5).then_some(())
        }).await.expect("all 5 memberships converge on every node");
        // every node sees fresh PeerEndpoint for all members
        // (similar poll on PeerEndpoint count >= 5)
    }

    // 3) Layer-2 scoped star: coordinator↔each subagent conversation pairing; NO subagent↔subagent.
    //    v1 writes DataPlanePairingDesired rows by GraphQL upsert (operator-owned, source="operator")
    //    — same approach as the legacy write_pairing helper, but on the new collection. No new CLI.
    for s in subs {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let pid = escape_graphql_string(&peer_id_of(s));
        let did = escape_graphql_string(&s.agent_did);
        let addr = escape_graphql_string(&address_of(s));
        let now = escape_graphql_string(&now);
        let m = format!(r#"mutation {{ upsert_DataPlanePairingDesired(
            filter: {{ peer_id: {{ _eq: "{pid}" }} }},
            add: {{ peer_id: "{pid}", agent_did: "{did}", template: "conversation",
                    source: "operator", collections: null,
                    replicator_addresses: ["{addr}"],
                    created_at: "{now}", updated_at: "{now}" }},
            update: {{ agent_did: "{did}", template: "conversation",
                       source: "operator", collections: null,
                       replicator_addresses: ["{addr}"],
                       updated_at: "{now}" }}
        ) {{ _docID }} }}"#,
            pid = pid, did = did, addr = addr, now = now);
        graphql_query(&coord.gql, &m).await?;
    }
    // ASSERT: a conversation replicator is present for each coord↔sub edge; assert NO
    // DataPlanePairingDesired row exists between any two subagents (query each sub's node).

    // 4) authorization config: coordinator gets 4 SubagentTargets + allow_cross_deployment=true;
    //    subagents get none + false. (Upsert ToolSelection via GraphQL — copy the shape from
    //    conformance r5_cross_deployment.rs:263-286.)

    // 5) delegation under real inference (background await_mode)
    let request_id = "fleet-parent-1";
    create_runtime_request(&coord.gql, &coord.agent_did, /*behavior*/, request_id,
        "Use your research subagents to answer two sub-questions IN PARALLEL via spawn_subagent (background).").await?;

    // ASSERT round-trip:
    //  - >=2 child AgentRequests materialize on their owning subagent nodes with agent_did=that sub + correct behavior_id
    //  - lineage stamped (caused_by_parent_request_id / _tool_call_id / caused_by_trigger_kind=subagent)
    //  - >=2 children produce non-empty AgentResponse; background bridge/await semantics hold
    //  - terminals replicate back; coordinator parent terminalizes cleanly, no orphaned bridges
    let parent_terminal = wait_for_request_terminal(&coord.gql, request_id, std::time::Duration::from_secs(180)).await;
    assert!(is_terminal(&parent_terminal));

    // 6) no-crosswise: a subagent's onward spawn_subagent is denied (no allowed target / allow_cross_deployment=false)
    //    AND no subagent↔subagent data-plane replicator exists.

    // 7) revoke fence (D11): coordinator revokes one subagent; assert BOTH its Layer-1 mesh edge
    //    and Layer-2 conversation replicator retract within ~30s.
    Ok(())
}
```

(Fill the assertion helpers by porting `wait_for_request_terminal` / `wait_for_assistant_answer` / `fetch_request_on_node` from `crates/defra-agent/tests/e2e_live/subagent_delegation_live.rs:703-1085` into HTTP-GraphQL form against each node's `gql`.)

- [ ] **Step 2: Run it live**

Run:
```bash
DEFRA_AGENT_LIVE_OPENAI=1 \
  DEFRA_AGENT_LIVE_OPENAI_ENDPOINT="<deepseek-endpoint>" \
  DEFRA_AGENT_LIVE_OPENAI_MODEL="<model>" \
  cargo test -p defra-agent-cli --test cli_fleet_delegation_live -- --ignored --nocapture
```
Expected: PASS — all stage assertions hold; wall-clock dominated by inference, convergence in seconds (fast intervals).

- [ ] **Step 3: Verify it's hermetic-skipped by default**

Run: `cargo test -p defra-agent-cli --test cli_fleet_delegation_live`
Expected: the test is skipped (ignored) — default CI stays green without the live endpoint.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-cli/tests/cli_fleet_delegation_live.rs
git commit -m "test(e2e): 5-process fleet — discovery→join→scoped star→fan-out delegation (cut 6, #511)"
```

---

## Final gate (before PR)

- [ ] `cargo test -p defra-agent` (full package, not `--lib`) → green
- [ ] `cargo test -p defra-agent-cli` → green (live fleet test skipped by default)
- [ ] `cargo test -p defra-agent-protocol` → green
- [ ] `cd crates/defra-agent/proofs && lake build` → zero `sorry` (`grep -rn sorry Proofs/` on changed files → none)
- [ ] `cargo fmt --all` + `cargo clippy --workspace` clean
- [ ] Live fleet run passes against the DeepSeek endpoint (manual, once)
- [ ] Open PR against `main` from `fleet-discovery-e2e`, referencing #511; PR body lists cuts 0/3/4/5/6 and the D1–D11 decisions.

## Notes / discovery dependencies

- Several Rust internal signatures (exact helper names in `network.rs`, the live-backend init shape in `cli_live.rs`, the `ToolSelection` upsert in `r5_cross_deployment.rs`, the `create_runtime_request` helper) must be read from the cited files at implementation time — they are named with file:line so the implementer reads the real signature rather than guessing.
- The Lean obligations in Task 5.3 are the only genuinely *new* proof work; everything else in cuts 3/4/5 is conformance + Rust satisfying the existing `NetworkMembership.lean`. If Task 5.3 surfaces that the model needs more than a per-collection-filter generalization, STOP and resolve in Lean before writing the cut-5 Rust (foundation-first).
- Verify at impl time whether `DataPlanePairingDesired` must be added to any `Collection` enum / config-apply walker; `PeerPairingDesired` was not (it is not config-apply-managed), so `DataPlanePairingDesired` likely isn't either — but grep `config_import.rs` to be sure.
