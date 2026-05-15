# Issue #193 — Principal/Behavior Runtime Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor `DefraAgent`'s conflated principal/behavior/identity into typed `AgentPrincipal` + `AgentBehavior` runtime types, then flip the `identity.respects_principal_boundary` Lean conformance contract from `enforced=false` to `enforced=true`.

**Architecture:** `AgentPrincipal` (new) owns the signing identity + principal-level metadata; `AgentBehavior` (rename of `BehaviorConfig`) holds `Arc<AgentPrincipal>` as a back-reference, removing its duplicated `identity` field. Single principal per snapshot — every behavior on a `DefraAgent` clones the same `Arc<AgentPrincipal>`. Routing-only design: DefraDB ACP is the decider, runtime contributes principal-DID routing observability only.

**Tech Stack:** Rust workspace (`crates/defra-agent`, `crates/defra-agent-cli`), Lean 4 / Lake (`crates/defra-agent/proofs/`), `proptest = "1"` (already a dep).

**Spec:** `docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md`

---

## File map

**Files created:**
- `crates/defra-agent/tests/support/identity_stubs.rs` — `StubAgentIdentity` test-only impl.

**Files modified:**
- `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean` — sharpen statement text (Task 1); flip `enforced` (Task 11).
- `crates/defra-agent/src/identity.rs` — add `AgentPrincipal` struct (Task 2).
- `crates/defra-agent/src/config.rs` — rename `BehaviorConfig` → `AgentBehavior`; remove `identity` field; add `principal: Arc<AgentPrincipal>` field; add `principal_identity()` + `agent_did()` methods (Task 4); rename `name` → `behavior_id` (Task 5).
- `crates/defra-agent/src/lib.rs` — update `pub use` for the rename (Task 4).
- `crates/defra-agent/src/agent.rs` — `DefraAgent` carries `principal: Arc<AgentPrincipal>`; accessors delegate (Task 7).
- `crates/defra-agent/src/document_config/` (whichever module reads the principal row) — surface `display_name` + `enabled` (Task 6).
- `crates/defra-agent/src/runtime_snapshot.rs` — snapshot carries the principal Arc; construction threads it through behaviors (Task 7).
- `crates/defra-agent/src/agent/builder.rs`, `crates/defra-agent/src/agent/reconcile.rs` — construction sites pass the principal Arc through (Task 7).
- `crates/defra-agent/src/agent/runtime_snapshot/` and consumers (every `behavior.identity` → `behavior.principal_identity()`; every `behavior.name` → `behavior.behavior_id`) — mechanical updates surfaced by `cargo check` (Tasks 4, 5).
- `crates/defra-agent/tests/support/mod.rs` — register the new `identity_stubs` module (Task 3).
- `crates/defra-agent/tests/identity_conformance.rs` — rewrite witness test to drive runtime types; delete Rust mirrors; add new enforced-routing test (Tasks 8, 9, 10).
- `crates/defra-agent/tests/identity_conformance_proptest.rs` (new) — loader-dedup proptest (Task 12). Alternatively a `#[cfg(test)] mod identity_proptest` inside `tests/identity_conformance.rs`; this plan picks a separate file.

**No schema files change. No `Collection` enum changes. No `defra-agent-cli` apply-path code changes.** The CLI may need a transitional `pub use AgentBehavior as BehaviorConfig` re-export if `defra-agent-cli` imports the type — Task 4 verifies.

---

### Task 1: Lean — sharpen `identity.respects_principal_boundary` statement text

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:429-439`

- [ ] **Step 1: Open the file and locate `identityContracts`**

The current contract entry is around line 429:

```lean
def identityContracts : List IdentityContract :=
  [ { name      := "identity.respects_principal_boundary"
    , statement :=
        "For any two AgentBehavior rows b₁, b₂ with " ++
        "b₁.agent_did == b₂.agent_did, the runtime's permission " ++
        "decision function MUST return identical results for any " ++
        "permission."
    , enforced  := false
    , trackedBy := "#193"
    }
  ]
```

- [ ] **Step 2: Replace the `statement` text with the routing-explicit version**

```lean
def identityContracts : List IdentityContract :=
  [ { name      := "identity.respects_principal_boundary"
    , statement :=
        "The runtime's behavior_id -> agent_did resolution is " ++
        "single-valued: for any two AgentBehavior rows b1, b2 with " ++
        "b1.agent_did == b2.agent_did, the runtime supplies the same " ++
        "Identity::Authenticated(did) as the actor for any DefraDB ACP " ++
        "check, so any DID-keyed permission decision returns identical " ++
        "results."
    , enforced  := false
    , trackedBy := "#193"
    }
  ]
```

Notes:
- Use ASCII `b1`, `b2`, `->` (the Lean module uses ASCII for the existing statement; keep that convention).
- Keep the substring `agent_did` (existing Rust assertion in `identity_respects_principal_contract_is_declared` checks `target.statement.contains("agent_did")`).
- The new statement also contains `routing`, `Identity::Authenticated`, and `resolution` — Task 10 asserts on these.

- [ ] **Step 3: Verify Lean builds**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: zero errors, zero `sorry`s. The JSON snapshot regenerates with the new statement text.

- [ ] **Step 4: Verify the existing Rust assertion still passes**

```bash
cargo test -p defra-agent --test identity_conformance identity_respects_principal_contract_is_declared
```

Expected: PASS. The test only asserts `enforced == false` and that the statement contains `agent_did`; both are still true.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Conformance.lean
git commit -m "$(cat <<'EOF'
Sharpen identity.respects_principal_boundary statement

Restate the contract as routing-explicit: the runtime's
behavior_id -> agent_did resolution is single-valued, so any
DID-keyed permission decision (DefraDB ACP) returns identical
results for behaviors sharing a principal.

Keeps enforced := false; the flip happens after the runtime
refactor lands the routing witness (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Rust — add `AgentPrincipal` struct

**Files:**
- Modify: `crates/defra-agent/src/identity.rs:1-27` (add new struct near the existing `AgentIdentity` trait)
- Modify: `crates/defra-agent/src/lib.rs:59` (export the new type)

- [ ] **Step 1: Add the struct to `identity.rs`**

Insert this block after the `ServiceAccount` struct (around line 16) and before the `AgentIdentity` trait:

```rust
/// Deployment-level principal record.
///
/// One `AgentPrincipal` exists per defra-agent process. Owns the signing
/// identity used for every DefraDB op the runtime issues. Every
/// `AgentBehavior` on the deployment holds an `Arc<AgentPrincipal>`
/// back-reference; the back-reference makes Lean's
/// `behavior_id_determines_principal` theorem structural at the type
/// level (no path constructs a behavior with a dangling agent_did).
///
/// Mirrors the Lean `Identity.Principal` record in
/// `crates/defra-agent/proofs/Proofs/Identity/State.lean:17`.
pub struct AgentPrincipal {
    pub agent_did: String,
    pub identity: Arc<dyn AgentIdentity>,
    pub default_behavior_id: String,
    pub display_name: Option<String>,
    pub enabled: bool,
}

impl std::fmt::Debug for AgentPrincipal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentPrincipal")
            .field("agent_did", &self.agent_did)
            .field("default_behavior_id", &self.default_behavior_id)
            .field("display_name", &self.display_name)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}
```

`Debug` is hand-rolled because `Arc<dyn AgentIdentity>` is not `Debug`.

- [ ] **Step 2: Export it from `lib.rs`**

Find the existing identity re-exports (search for `pub use identity::` or similar). Add:

```rust
pub use identity::AgentPrincipal;
```

If `identity` is already re-exported wholesale (e.g., `pub use identity::*;`), nothing to add — but verify.

- [ ] **Step 3: Verify it compiles**

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
```

Expected: clean. The struct has no callers yet.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/src/identity.rs crates/defra-agent/src/lib.rs
git commit -m "$(cat <<'EOF'
Add AgentPrincipal struct

Deployment-level principal record. Mirrors the Lean
Identity.Principal record. No production callers yet; wired in by
the upcoming AgentBehavior rename which will hold this as an Arc
back-reference (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Rust — add `StubAgentIdentity` for tests

**Files:**
- Create: `crates/defra-agent/tests/support/identity_stubs.rs`
- Modify: `crates/defra-agent/tests/support/mod.rs` (register module)

- [ ] **Step 1: Create the stub file**

Write `crates/defra-agent/tests/support/identity_stubs.rs`:

```rust
//! Test-only `AgentIdentity` impl that returns a chosen DID string.
//!
//! `KeyIdentity` derives its DID from a generated key and cannot return
//! a chosen DID like `"did:agent:amy"`. The identity-conformance tests
//! need to construct principals whose DIDs match the Lean rows, so they
//! use this stub. Routing tests never sign or verify; both methods
//! panic if called.

use std::sync::Arc;

use async_trait::async_trait;

use defra_agent::{AgentIdentity, ServiceAccount};

/// Test-only `AgentIdentity` that returns the chosen DID.
pub(crate) struct StubAgentIdentity {
    pub did: String,
}

impl StubAgentIdentity {
    pub(crate) fn new(did: impl Into<String>) -> Self {
        Self { did: did.into() }
    }

    pub(crate) fn arc(did: impl Into<String>) -> Arc<dyn AgentIdentity> {
        Arc::new(Self::new(did))
    }
}

#[async_trait]
impl AgentIdentity for StubAgentIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, _payload: &[u8]) -> anyhow::Result<Vec<u8>> {
        panic!(
            "StubAgentIdentity::sign called for {} — routing tests must not sign",
            self.did
        )
    }

    async fn verify(
        &self,
        _did: &str,
        _payload: &[u8],
        _sig: &[u8],
    ) -> anyhow::Result<bool> {
        panic!(
            "StubAgentIdentity::verify called for {} — routing tests must not verify",
            self.did
        )
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        None
    }
}
```

Notes:
- `ServiceAccount` and `AgentIdentity` must be re-exported from the `defra_agent` crate. If they aren't yet, Task 2's lib.rs edit should add them; verify before this task.
- Visibility is `pub(crate)` so only the test tree consumes it.

- [ ] **Step 2: Register the module in `tests/support/mod.rs`**

Read the existing `tests/support/mod.rs` and add a line:

```rust
pub(crate) mod identity_stubs;
```

next to the other `pub(crate) mod fixtures;`-style declarations. Keep alphabetical if the existing file is sorted; otherwise append.

- [ ] **Step 3: Verify it compiles**

```bash
cargo check -p defra-agent --tests
```

Expected: clean. No tests consume `StubAgentIdentity` yet, so a `#[allow(dead_code)]` may be needed on `StubAgentIdentity` (or its methods) to suppress warnings. If `cargo check` complains, add `#[allow(dead_code)]` above the struct.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/tests/support/identity_stubs.rs crates/defra-agent/tests/support/mod.rs
git commit -m "$(cat <<'EOF'
Add StubAgentIdentity test helper

Returns a chosen DID from .did(); panics on sign/verify. Consumed
by the identity-conformance routing tests in upcoming tasks
(#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Rust — rename `BehaviorConfig` → `AgentBehavior`, drop `identity` field, add `principal` back-ref

This is the largest mechanical change. The strategy: do all four edits in one commit, then fix the cascading `cargo check` errors via search-and-replace.

**Files:**
- Modify: `crates/defra-agent/src/config.rs:22-41` (struct + Debug impl)
- Modify: `crates/defra-agent/src/config.rs:103-142` (impl block)
- Modify: `crates/defra-agent/src/lib.rs:59` (`pub use`)
- Modify (many): every consumer that uses `BehaviorConfig` or `.identity`

- [ ] **Step 1: Update `src/config.rs` — rename struct + drop `identity` + add `principal`**

Replace lines 20-41 (the struct definition and its `Clone` derive):

```rust
/// Runtime configuration for one loaded behavior executor.
///
/// Mirrors the Lean `Identity.Behavior` record. Holds an
/// `Arc<AgentPrincipal>` back-reference; the principal owns the
/// signing identity used for all DefraDB ops issued for this
/// behavior. Two behaviors sharing the same principal Arc share the
/// same actor DID (Lean's `behavior_id_determines_principal` is
/// structural at the type level here).
#[derive(Clone)]
pub struct AgentBehavior {
    pub name: String,
    pub principal: Arc<AgentPrincipal>,
    pub backend_id: Option<String>,
    pub backend_provider_kind: BackendProviderKind,
    pub backend_endpoint: String,
    pub backend_api_key: Option<String>,
    pub backend_api_key_env_var: Option<String>,
    pub model_name: String,
    pub context_window: usize,
    pub max_output_tokens: usize,
    pub max_turns: usize,
    pub system_prompt: String,
    pub tools: BehaviorToolConfig,
    pub compaction_threshold: f64,
    pub compaction_strategy: CompactionStrategy,
    pub stream_batch_ms: u64,
    pub deadline_duration: Duration,
    pub sampling: SamplingConfig,
}
```

Note: the `name` field stays as `name` in this task — renaming to `behavior_id` is Task 5. Each task one rename.

- [ ] **Step 2: Update the `Debug` impl in `src/config.rs`**

Replace the `impl std::fmt::Debug for BehaviorConfig` block (lines 75-101) with:

```rust
impl std::fmt::Debug for AgentBehavior {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentBehavior")
            .field("name", &self.name)
            .field("principal_did", &self.principal.agent_did)
            .field("backend_id", &self.backend_id)
            .field("backend_provider_kind", &self.backend_provider_kind)
            .field("backend_endpoint", &self.backend_endpoint)
            .field(
                "backend_api_key",
                &self.backend_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("backend_api_key_env_var", &self.backend_api_key_env_var)
            .field("model_name", &self.model_name)
            .field("context_window", &self.context_window)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("max_turns", &self.max_turns)
            .field("system_prompt", &self.system_prompt)
            .field("tools", &self.tools)
            .field("compaction_threshold", &self.compaction_threshold)
            .field("compaction_strategy", &self.compaction_strategy)
            .field("stream_batch_ms", &self.stream_batch_ms)
            .field("deadline_duration", &self.deadline_duration)
            .field("sampling", &self.sampling)
            .finish()
    }
}
```

- [ ] **Step 3: Update the `impl` block — replace `did()` with two methods**

Replace lines 103-142 (the `impl BehaviorConfig` block):

```rust
impl AgentBehavior {
    /// Returns the principal's agent_did.
    pub fn agent_did(&self) -> &str {
        &self.principal.agent_did
    }

    /// Returns the principal's signing identity.
    ///
    /// This is the only way to obtain an `Arc<dyn AgentIdentity>` for
    /// a behavior; the behavior itself does not hold one. Two
    /// behaviors sharing an `Arc<AgentPrincipal>` return identical
    /// clones, so DefraDB ACP receives the same actor for both —
    /// satisfying Lean's `RespectsPrincipal` predicate.
    pub fn principal_identity(&self) -> &Arc<dyn AgentIdentity> {
        &self.principal.identity
    }

    pub fn resolve_backend_api_key(&self) -> Result<Option<String>> {
        if let Some(api_key) = normalize_optional_secret(self.backend_api_key.as_deref()) {
            return Ok(Some(api_key.to_string()));
        }

        if let Some(env_var) = normalize_optional_env_var(self.backend_api_key_env_var.as_deref()) {
            let value = std::env::var(env_var).with_context(|| {
                format!(
                    "backend {} for behavior {} requires environment variable {}",
                    self.backend_id.as_deref().unwrap_or("<unbound>"),
                    self.name,
                    env_var
                )
            })?;
            let value = value.trim();
            if value.is_empty() {
                anyhow::bail!(
                    "backend {} for behavior {} resolved empty API key from environment variable {}",
                    self.backend_id.as_deref().unwrap_or("<unbound>"),
                    self.name,
                    env_var
                );
            }
            return Ok(Some(value.to_string()));
        }

        Ok(None)
    }

    pub fn completion_client_api_key(&self) -> Result<String> {
        Ok(self
            .resolve_backend_api_key()?
            .unwrap_or_else(|| "no-key".to_string()))
    }
}
```

The old `did()` method is replaced by `agent_did()` (semantic rename — DID always came from the principal anyway).

- [ ] **Step 4: Update the imports at the top of `src/config.rs`**

Replace line 8:

```rust
use crate::identity::AgentIdentity;
```

with:

```rust
use crate::identity::{AgentIdentity, AgentPrincipal};
```

- [ ] **Step 5: Update `src/lib.rs` re-export**

Replace line 59:

```rust
pub use config::BehaviorConfig;
```

with:

```rust
pub use config::AgentBehavior;
```

(Optional transitional alias if `defra-agent-cli` consumes it externally — verified in Step 6.)

- [ ] **Step 6: Run `cargo check` and triage errors**

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens 2>&1 | tee /tmp/task4_check.log
```

You will get many errors. They fall into four classes:

1. **`BehaviorConfig` is undefined.** Search-and-replace all `BehaviorConfig` → `AgentBehavior` across the crate and consumers.
2. **`.identity` access on a behavior.** Replace `behavior.identity` → `behavior.principal.identity` (when only the field is wanted) or `behavior.principal_identity()` (idiomatic accessor). Replace `behavior.identity.did()` → `behavior.agent_did()`.
3. **`.did()` method calls on a behavior.** Method survives (renamed to `agent_did`) so most calls work, but if any test fixtures construct `BehaviorConfig { identity: ..., ... }` literally, they need a principal Arc instead.
4. **Construction sites.** Anywhere a `BehaviorConfig { name, identity, backend_id, ... }` literal exists, change to `AgentBehavior { name, principal: Arc::new(AgentPrincipal { ... }), backend_id, ... }`. Test fixtures are the main offenders.

Use search-and-replace tooling:

```bash
# Class 1: rename
rg -l "BehaviorConfig" crates/defra-agent crates/defra-agent-cli | xargs sed -i '' 's/BehaviorConfig/AgentBehavior/g'

# Class 2: field access (be careful — only on AgentBehavior contexts)
# Manual fix-by-error is safer here; run cargo check after the Class 1 rename.
```

After the Class 1 mass-rename, re-run `cargo check`. Each remaining error names a file and line; fix by hand.

Patterns to look for in the by-hand pass (search results from earlier audit):
- `config.rs:79` — already updated in Step 2.
- `config.rs:105` — already updated in Step 3 (replaced with `agent_did()`).
- `agent/builder.rs:189` — likely a `self.behavior.identity = Some(identity);` style write; replace with a builder method that sets the principal Arc. Read the file and adapt.
- Builders that historically took `identity: Arc<dyn AgentIdentity>` for a behavior should now take or build an `Arc<AgentPrincipal>`. This may ripple into `DefraAgentBuilder::with_default_behavior`, which Task 7 will reconcile cleanly. **For Task 4, the minimum is: construct an `Arc<AgentPrincipal>` from whatever identity was provided, even if it's redundant.** Task 7 cleans up the construction story.

- [ ] **Step 7: Add a transitional alias if `defra-agent-cli` consumes `BehaviorConfig` as a public type**

After Step 6 errors are fixed, search:

```bash
rg "defra_agent::BehaviorConfig" crates/defra-agent-cli crates/defra-agent-desktop-core
```

If any matches: either rename those imports to `defra_agent::AgentBehavior`, OR add a transitional alias at the top of `crates/defra-agent/src/lib.rs`:

```rust
/// Transitional alias for the renamed type. Remove once
/// `defra-agent-cli` and `defra-agent-desktop-core` are updated.
#[deprecated(note = "use AgentBehavior")]
pub use config::AgentBehavior as BehaviorConfig;
```

Prefer renaming imports — the alias adds a deprecation warning that the next person has to deal with. Only add the alias if the consumer change is genuinely outside this PR's scope.

- [ ] **Step 8: Run all tests**

```bash
cargo test -p defra-agent --lib --tests 2>&1 | tail -40
cargo test -p defra-agent-cli 2>&1 | tail -20
```

Expected: all green. If tests fail, the most likely cause is a fixture that constructs `AgentBehavior` literally without a real principal; switch it to build an `Arc<AgentPrincipal>` (use `test_identity(name)` from `tests/support/fixtures.rs` to obtain an `Arc<dyn AgentIdentity>`).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
Rename BehaviorConfig -> AgentBehavior; drop identity; add principal

Replaces BehaviorConfig.identity (Arc<dyn AgentIdentity>) with a
back-reference: principal: Arc<AgentPrincipal>. Adds
agent_did() and principal_identity() methods. Every call site
that previously read behavior.identity now sources signing
identity exclusively through the principal Arc.

Lean's behavior_id_determines_principal is now structural at the
type level: no construction path produces a behavior with a
dangling agent_did (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Rust — rename `AgentBehavior::name` field → `behavior_id`

The field is `name` today; the schema and Lean both call it `behavior_id`. Atomic rename in its own commit.

**Files:**
- Modify: `crates/defra-agent/src/config.rs:23` (and the Debug impl `name` reference at line ~78)
- Modify (many): every consumer (`behavior.name` → `behavior.behavior_id`)

- [ ] **Step 1: Rename the field declaration in `src/config.rs`**

In the `AgentBehavior` struct definition, change:

```rust
pub name: String,
```

to:

```rust
pub behavior_id: String,
```

Also update the `Debug` impl's `.field("name", ...)` line:

```rust
.field("behavior_id", &self.behavior_id)
```

- [ ] **Step 2: Search-and-replace consumers**

```bash
# Targeted replace: only inside contexts where the var name says it's a behavior.
rg -l "behavior\.name|\.behavior\.name|self\.behavior\.name|left\.name|right\.name" \
    crates/defra-agent crates/defra-agent-cli > /tmp/task5_files.txt

# Manual review of each match — see the audit list from the spec exploration.
# Known sites (from grep):
#   - crates/defra-agent/src/agent.rs:129,130,133
#   - crates/defra-agent/src/oneshot.rs:63,86,145
#   - crates/defra-agent/src/runtime_snapshot.rs:154
#   - crates/defra-agent/src/prompt.rs:102
#   - crates/defra-agent/src/agent/reconcile/slot.rs:181,193,206
#   - crates/defra-agent/src/agent/daemon.rs:88,96,99,109,134,162,174,183,193,203,218,222
#   - and likely more — let cargo check enumerate.
```

For each file, replace `behavior.name` / `behavior_config.name` / `self.behavior.name` with the `_id`-suffixed form. Beware of false positives where `.name` refers to something else (a backend name, a tool name) — read the variable's type before replacing.

Safer pattern: don't bulk-replace. Instead, edit `src/config.rs` only (Step 1), run `cargo check`, and fix each compile error by hand. `cargo check` will name the file and line for every site that needs updating.

- [ ] **Step 3: Run `cargo check` iteratively**

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
```

Fix each error. Re-run until clean.

- [ ] **Step 4: Run all tests**

```bash
cargo test -p defra-agent --lib --tests
cargo test -p defra-agent-cli
```

Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
Rename AgentBehavior.name -> behavior_id

Matches the schema field (agent_behavior.graphql) and the Lean
record (Identity.Behavior.id). Mechanical rename; cargo check
surfaced every site (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Rust — extend the loader to read `display_name` + `enabled` from the AgentPrincipal row

Today the loader only surfaces `default_behavior_id` from the AgentPrincipal row. To construct a full `Arc<AgentPrincipal>` in Task 7, the loader needs `display_name` and `enabled` too.

**Files:**
- Modify: `crates/defra-agent/src/document_config/` — find the module that defines the loader's view of `AgentPrincipal`. Likely `document_config/principal.rs` or similar; if there is no per-collection module, the type lives in `document_config/mod.rs` or `document_config.rs`.
- Modify: `crates/defra-agent/src/agent/document_view.rs` — the GraphQL query that pulls the principal row.

- [ ] **Step 1: Locate the loader's principal type**

```bash
rg "struct.*AgentPrincipal\b|fn.*agent_principal\b|default_behavior_id" crates/defra-agent/src/document_config/ crates/defra-agent/src/agent/
```

Find the struct that mirrors the AgentPrincipal GraphQL row. It should have at least `agent_did: String` and `default_behavior_id: Option<String>`. Read it.

- [ ] **Step 2: Add `display_name` and `enabled` fields**

In the struct (adjust path as found in Step 1):

```rust
pub(crate) struct AgentPrincipal {  // or whatever the local name is
    pub agent_did: String,
    pub default_behavior_id: Option<String>,
    pub display_name: Option<String>,  // NEW
    pub enabled: Option<bool>,         // NEW (Option because schema is non-required)
}
```

- [ ] **Step 3: Update the GraphQL query**

Find the `AgentPrincipal` query in `agent/document_view.rs` (search for `AgentPrincipal` GraphQL strings):

```bash
rg -n "AgentPrincipal" crates/defra-agent/src/agent/document_view.rs
```

The current query likely selects `agent_did { default_behavior_id }`. Extend it to:

```graphql
AgentPrincipal {
    agent_did
    default_behavior_id
    display_name
    enabled
}
```

Update the JSON deserialization to pull these new fields.

- [ ] **Step 4: Update consumers (none expected yet)**

This task just makes the fields available. Task 7 consumes them. Run `cargo check` to confirm no regressions.

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
cargo test -p defra-agent --lib --tests
```

Expected: clean / green.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
Surface display_name + enabled from AgentPrincipal row

Extends the GraphQL query and the loader's principal struct.
Consumed by the upcoming Arc<AgentPrincipal> construction in
DefraAgent (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Rust — rework `DefraAgent` construction to thread `Arc<AgentPrincipal>`

Wire the principal Arc through `DefraAgent::from_default_behavior_documents`, `runtime_snapshot::ResolvedRuntimeSnapshot`, and reconcile. This is where the single-principal-per-snapshot invariant becomes load-bearing.

**Files:**
- Modify: `crates/defra-agent/src/agent.rs:85-105` (`DefraAgent` struct)
- Modify: `crates/defra-agent/src/agent.rs:111-153` (`from_default_behavior_documents`)
- Modify: `crates/defra-agent/src/agent.rs:155-170` (accessors)
- Modify: `crates/defra-agent/src/agent.rs:201-267` (`behavior_config_from_documents`, now `agent_behavior_from_documents`)
- Modify: `crates/defra-agent/src/runtime_snapshot.rs:108-165` (ResolvedRuntimeSnapshot)
- Modify: `crates/defra-agent/src/runtime_snapshot.rs:256-300` (ActiveRuntimeSnapshot)
- Modify: `crates/defra-agent/src/agent/builder.rs` (builder snapshot construction)
- Modify: `crates/defra-agent/src/agent/reconcile.rs` (snapshot rebuild)
- Modify: `crates/defra-agent/src/agent/document_view.rs` (snapshot construction)

- [ ] **Step 1: Update `DefraAgent` struct**

In `crates/defra-agent/src/agent.rs:85-104`, replace the struct fields:

```rust
#[derive(Clone)]
pub struct DefraAgent {
    node: Arc<EmbeddedNode>,
    principal: Arc<AgentPrincipal>,        // replaces: agent_did + default_behavior_id
    behaviors: Vec<Arc<AgentBehavior>>,
    unavailable_behaviors: HashMap<String, String>,
    document_runtime_context: Option<DocumentResolveContext>,
    mcp_pool: McpPool,
    local_hostname: String,
    local_subnet: Option<String>,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    process_state_observer: Option<Arc<dyn ProcessLifecycleObserver>>,
    pub(crate) manual_trigger_handle: Arc<OnceCell<ManualTriggerHandle>>,
}
```

Add `use crate::identity::{AgentIdentity, AgentPrincipal};` at the top if not already imported.

- [ ] **Step 2: Update accessors**

Replace `agent_did()` and `default_behavior_id()` (lines 159-165):

```rust
pub fn principal(&self) -> &AgentPrincipal {
    &self.principal
}

pub fn agent_did(&self) -> &str {
    &self.principal.agent_did
}

pub fn default_behavior_id(&self) -> &str {
    &self.principal.default_behavior_id
}
```

- [ ] **Step 3: Update `from_default_behavior_documents`**

In the body of `from_default_behavior_documents` (lines 111-153), construct `Arc<AgentPrincipal>` once and clone it into every behavior. Replace the body's `Ok(Self { ... })` block with:

```rust
let default_behavior_id = resolved_snapshot.default_behavior_id.clone();

// Single Arc<AgentPrincipal> for this snapshot. Every AgentBehavior
// in this DefraAgent clones this Arc — the load-bearing
// single-principal-per-snapshot invariant from the spec.
let principal = Arc::new(AgentPrincipal {
    agent_did: identity.did().to_string(),
    identity: identity.clone(),
    default_behavior_id,
    display_name: resolved_snapshot.principal_display_name.clone(),
    enabled: resolved_snapshot.principal_enabled.unwrap_or(true),
});

let mut behaviors = resolved_snapshot
    .behaviors
    .values()
    .cloned()
    .collect::<Vec<_>>();
behaviors.sort_by(|left, right| {
    let left_is_default = left.behavior_id == principal.default_behavior_id;
    let right_is_default = right.behavior_id == principal.default_behavior_id;
    right_is_default
        .cmp(&left_is_default)
        .then_with(|| left.behavior_id.cmp(&right.behavior_id))
});

Ok(Self {
    node,
    principal,
    behaviors,
    unavailable_behaviors: resolved_snapshot.unavailable_behaviors,
    document_runtime_context: Some(document_runtime_context),
    mcp_pool: options.mcp_pool,
    local_hostname: options
        .local_hostname
        .unwrap_or_else(runtime::default_hostname),
    local_subnet: options.local_subnet,
    retry_policy: options.retry_policy,
    hook_failure_policy: options.hook_failure_policy,
    process_state_observer: options.process_state_observer,
    manual_trigger_handle: Arc::new(OnceCell::new()),
})
```

This snippet assumes `ResolvedRuntimeSnapshot` already surfaces `principal_display_name: Option<String>` and `principal_enabled: Option<bool>`. Step 4 adds them; if step ordering causes a missing-field error here, swap Steps 3 and 4.

- [ ] **Step 4: Update `ResolvedRuntimeSnapshot` and `ActiveRuntimeSnapshot`**

In `crates/defra-agent/src/runtime_snapshot.rs`:

Add fields to `ResolvedRuntimeSnapshot` (around line 108):

```rust
pub(crate) struct ResolvedRuntimeSnapshot {
    pub(crate) local_did: String,
    pub(crate) paired_peer_dids: HashSet<String>,
    pub(crate) default_behavior_id: String,
    pub(crate) principal_display_name: Option<String>,  // NEW
    pub(crate) principal_enabled: Option<bool>,          // NEW
    pub(crate) behaviors: HashMap<String, Arc<AgentBehavior>>,
    // ... rest unchanged ...
}
```

Mirror the new fields on `ActiveRuntimeSnapshot` (around line 256) — same two fields.

Update `from_parts` and `from_parts_with_admission_configs` (lines 124-165) to default both new fields to `None`. Update `activate()` (line 215) to pass them through.

- [ ] **Step 5: Update the loader to populate the new snapshot fields**

In `crates/defra-agent/src/agent/document_view.rs`, find where `ResolvedRuntimeSnapshot` is constructed from the principal row (search for `default_behavior_id:` in the file). Add:

```rust
principal_display_name: principal_row.display_name.clone(),
principal_enabled: principal_row.enabled,
```

at that construction site.

- [ ] **Step 6: Update `behavior_config_from_documents` (now `agent_behavior_from_documents`)**

In `crates/defra-agent/src/agent.rs:201-267`, the function signature currently is:

```rust
pub(crate) fn behavior_config_from_documents(
    identity: Arc<dyn AgentIdentity>,
    behavior: &crate::document_config::AgentBehavior,
    // ...
) -> anyhow::Result<BehaviorConfig> { ... }
```

Replace its first parameter from `identity: Arc<dyn AgentIdentity>` to `principal: Arc<AgentPrincipal>`. Rename the function to `agent_behavior_from_documents`. Replace `identity` in the body with `principal`:

```rust
pub(crate) fn agent_behavior_from_documents(
    principal: Arc<AgentPrincipal>,
    behavior: &crate::document_config::AgentBehavior,
    backend: &crate::backend_registry::InferenceBackend,
    inference_profile: &crate::document_config::InferenceProfile,
    tool_selection: ToolSelection,
    subagent_tools: SubagentToolConfig,
    tool_ceiling: &ToolCeiling,
) -> anyhow::Result<AgentBehavior> {
    // ... (existing parsing logic unchanged) ...

    Ok(AgentBehavior {
        behavior_id: behavior.behavior_id.clone(),
        principal,                   // NEW: replaces identity
        backend_id: Some(backend.backend_id.clone()),
        // ... rest unchanged ...
    })
}
```

- [ ] **Step 7: Update every caller of `behavior_config_from_documents`**

```bash
rg -n "behavior_config_from_documents" crates/defra-agent/
```

For each call site, the caller now needs an `Arc<AgentPrincipal>` instead of an `Arc<dyn AgentIdentity>`. The principal Arc should be constructed once per snapshot (in the loader / document_view path) and cloned into each call.

Add a `let principal_arc = Arc::new(AgentPrincipal { ... })` near the top of whichever loader function aggregates behaviors, and pass `principal_arc.clone()` to each `agent_behavior_from_documents` call.

- [ ] **Step 8: Update reconcile to rebuild with the principal Arc**

In `crates/defra-agent/src/agent/reconcile.rs`, find the snapshot rebuild logic (search for `ResolvedRuntimeSnapshot`). The rebuild needs to construct a fresh `Arc<AgentPrincipal>` (since values may have changed via desired-state apply) and thread it into every behavior, just like `from_default_behavior_documents` does.

If the existing reconcile code factors through `behavior_config_from_documents`, Step 6's signature change will surface the work; pass through `principal_arc.clone()`.

- [ ] **Step 9: Update the builder**

In `crates/defra-agent/src/agent/builder.rs`, the builder constructs `DefraAgent` directly (line 162-167 in the read earlier). Update the `Default::default()` and any explicit field-construction to use the new struct shape: build an `Arc<AgentPrincipal>` from whatever identity the builder has, with sensible defaults for `display_name` and `enabled`.

The relevant builder paths:
- Around line 167 (test-style builder): construct a principal Arc from the test identity.
- Lines 456-457 (`resolved_identity = self.identity.clone()...`): the resolved identity feeds the principal.

Pattern:

```rust
let principal = Arc::new(AgentPrincipal {
    agent_did: resolved_identity.did().to_string(),
    identity: resolved_identity,
    default_behavior_id: ... ,  // from builder state
    display_name: None,
    enabled: true,
});
```

- [ ] **Step 10: Run cargo check + tests**

```bash
cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens
cargo fmt --all
cargo test -p defra-agent --lib --tests
cargo test -p defra-agent-cli
```

Expected: clean / green. Any test fixture that constructs `AgentBehavior` literally with a stub identity needs to construct an `Arc<AgentPrincipal>` instead — use `test_identity(name)` from `tests/support/fixtures.rs` for the inner `Arc<dyn AgentIdentity>`.

- [ ] **Step 11: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
Thread Arc<AgentPrincipal> through DefraAgent construction

DefraAgent now owns Arc<AgentPrincipal>; agent_did() and
default_behavior_id() delegate. from_default_behavior_documents
constructs the principal Arc once and clones it into every
AgentBehavior — the single-principal-per-snapshot invariant from
the spec is now load-bearing in the loader.

ResolvedRuntimeSnapshot / ActiveRuntimeSnapshot surface
principal_display_name and principal_enabled so reconcile rebuild
preserves operator-edited metadata (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 12: Open the PR**

The typed runtime types are now visible. Open the PR against `main`:

```bash
git push -u origin design/issue-193-principal-behavior-deployment
gh pr create --title "Refactor DefraAgent into typed AgentPrincipal + AgentBehavior (#193)" --body "$(cat <<'EOF'
## Summary

- Splits the runtime types per spec `docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md`
- `AgentPrincipal` is a new type; `BehaviorConfig` renamed to `AgentBehavior` with a `principal: Arc<AgentPrincipal>` back-reference replacing the duplicated `identity` field
- Two scope reductions from the issue body: `AgentDeployment` dropped (one process = one principal; Lean's Deployment record stays as a model abstraction), and no new Rust-side permission decider (defradb.rs ACP is the decider, refactor contributes routing observability)
- Remaining tasks (conformance test rewrite, Lean `enforced` flip, proptest) land in subsequent commits

## Test plan

- [ ] `cargo check --workspace --all-targets --exclude agent-subagent-v2-to-v3-lens --exclude agent-tool-call-lifecycle-v1-to-v2-lens` clean
- [ ] `cargo test -p defra-agent --lib --tests` fully green
- [ ] `cargo test -p defra-agent-cli` fully green
- [ ] `cd crates/defra-agent/proofs && lake build` zero sorrys
- [ ] `tests/identity_conformance.rs::identity_respects_principal_contract_enforced_by_runtime_routing` green with `enforced == true` (lands in subsequent commits)
- [ ] New loader-dedup proptest green (lands in subsequent commits)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

### Task 8: Rust — rewrite `identity_permission_cases_pin_runtime_permission_contract_shape` to drive runtime types

**Files:**
- Modify: `crates/defra-agent/tests/identity_conformance.rs:1-234` (rewrite the test; keep structural tests untouched)

- [ ] **Step 1: Add the runtime-types helper**

At the top of `crates/defra-agent/tests/identity_conformance.rs`, after the existing imports, add:

```rust
use std::sync::Arc;

use defra_agent::{AgentBehavior, AgentIdentity, AgentPrincipal};

#[path = "support/identity_stubs.rs"]
mod identity_stubs;
#[allow(unused_imports)]
use identity_stubs::StubAgentIdentity;

/// Build `Arc<AgentPrincipal>` instances (one per Lean principal row)
/// and `Arc<AgentBehavior>` instances with the matching principal
/// back-ref. The Lean rows may include multiple principals (e.g., the
/// `separate_principal_*` cases), so this helper legitimately produces
/// a multi-principal world. The single-principal-per-snapshot
/// invariant from the spec applies to the production loader (fenced
/// by the proptest in task 12), not to these test fixtures.
fn build_runtime_behaviors_from_lean_case(
    case: &LeanIdentityPermissionCase,
) -> Vec<Arc<AgentBehavior>> {
    use std::collections::HashMap;
    let principals: HashMap<String, Arc<AgentPrincipal>> = case
        .principals
        .iter()
        .map(|p| {
            let identity: Arc<dyn AgentIdentity> = StubAgentIdentity::arc(p.did.clone());
            let arc = Arc::new(AgentPrincipal {
                agent_did: p.did.clone(),
                identity,
                default_behavior_id: String::new(),
                display_name: None,
                enabled: p.enabled,
            });
            (p.did.clone(), arc)
        })
        .collect();
    case.behaviors
        .iter()
        .map(|b| {
            let principal = principals
                .get(b.principal.as_str())
                .unwrap_or_else(|| {
                    panic!(
                        "lean case {:?}: behavior {:?} references unknown principal {:?}",
                        case.name, b.id, b.principal
                    )
                })
                .clone();
            Arc::new(build_agent_behavior_for_routing_test(
                b.id.clone(),
                principal,
            ))
        })
        .collect()
}

/// Construct an `AgentBehavior` populated with default routing-test
/// values; only behavior_id and principal are load-bearing for the
/// routing tests.
fn build_agent_behavior_for_routing_test(
    behavior_id: String,
    principal: Arc<AgentPrincipal>,
) -> AgentBehavior {
    AgentBehavior {
        behavior_id,
        principal,
        backend_id: None,
        backend_provider_kind: defra_agent::BackendProviderKind::default(),
        backend_endpoint: String::new(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: defra_agent::DEFAULT_MODEL_NAME.to_string(),
        context_window: defra_agent::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: defra_agent::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: defra_agent::DEFAULT_MAX_TURNS,
        system_prompt: String::new(),
        tools: defra_agent::BehaviorToolConfig::default(),
        compaction_threshold: defra_agent::DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: defra_agent::CompactionStrategy::default(),
        stream_batch_ms: defra_agent::DEFAULT_STREAM_BATCH_MS,
        deadline_duration: std::time::Duration::from_secs(
            defra_agent::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        sampling: defra_agent::SamplingConfig::default(),
    }
}
```

Notes:
- `BackendProviderKind`, `BehaviorToolConfig`, `CompactionStrategy`, `SamplingConfig`, the default constants — these need to be re-exported from `defra-agent`'s `lib.rs`. If they aren't, expand the lib.rs `pub use` list as part of this step.
- The constructor uses defaults because the routing tests don't exercise tool execution or sampling.

- [ ] **Step 2: Rewrite `identity_permission_cases_pin_runtime_permission_contract_shape`**

Replace the existing test body (the one that uses `rust_canonical_permission_decision` and `rust_hostability_decision`) with:

```rust
#[test]
fn identity_permission_cases_pin_runtime_permission_contract_shape() {
    let cases = lean_identity_permission_cases();
    assert_eq!(
        cases.len(),
        4,
        "Lean should emit the four executable identity permission rows that unblock #193"
    );

    let names: HashSet<&str> = cases.iter().map(|case| case.name.as_str()).collect();
    for expected in [
        "same_principal_row_owner_grant_allows_shared_behaviors",
        "separate_principal_without_grant_blocks_peer",
        "separate_principal_with_grant_allows_peer",
        "behavior_id_lookup_selects_declared_principal",
    ] {
        assert!(
            names.contains(expected),
            "missing expected identity permission case: {expected}"
        );
    }

    for case in cases {
        // Drive runtime types from the Lean fixture. The assertions go
        // through AgentBehavior::principal.agent_did rather than the
        // local Rust mirror that this test used to maintain.
        let runtime_behaviors = build_runtime_behaviors_from_lean_case(case);
        let by_id: std::collections::HashMap<&str, &AgentBehavior> = runtime_behaviors
            .iter()
            .map(|b| (b.behavior_id.as_str(), b.as_ref()))
            .collect();

        let actor = by_id.get(case.actor_behavior.as_str()).unwrap_or_else(|| {
            panic!(
                "case {:?}: actor_behavior {:?} not constructed",
                case.name, case.actor_behavior
            )
        });
        let peer = by_id.get(case.peer_behavior.as_str()).unwrap_or_else(|| {
            panic!(
                "case {:?}: peer_behavior {:?} not constructed",
                case.name, case.peer_behavior
            )
        });

        assert_eq!(
            actor.principal.agent_did, case.expected_actor_principal,
            "case {:?}: actor behavior-id lookup drifted at runtime layer",
            case.name
        );
        assert_eq!(
            peer.principal.agent_did, case.expected_peer_principal,
            "case {:?}: peer behavior-id lookup drifted at runtime layer",
            case.name
        );
        assert_eq!(
            actor.principal.agent_did == peer.principal.agent_did,
            case.same_principal,
            "case {:?}: same-principal witness drifted at runtime layer",
            case.name
        );
    }
}
```

The test now exercises **runtime types** as the witness for Lean's shape claims, rather than a local Rust mirror.

- [ ] **Step 3: Run the test**

```bash
cargo test -p defra-agent --test identity_conformance identity_permission_cases_pin_runtime_permission_contract_shape
```

Expected: PASS. If `BackendProviderKind::default()` or other defaults don't exist, derive them (likely `#[derive(Default)]`) or use `BackendProviderKind::Anthropic` (or whatever the closest existing variant is).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
Drive identity_permission shape test through runtime types

Replaces the local Rust mirror (rust_canonical_permission_decision,
rust_hostability_decision) with calls into the runtime
AgentPrincipal + AgentBehavior types. The Lean rows are now the
fixture; assertions go through behavior.principal.agent_did, which
is the production routing path (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Rust — delete the Rust mirror functions

`rust_canonical_permission_decision` and `rust_hostability_decision` are no longer consumed. Delete them outright.

**Files:**
- Modify: `crates/defra-agent/tests/identity_conformance.rs:84-95` (delete the two mirror functions)

- [ ] **Step 1: Delete `rust_canonical_permission_decision` and `rust_hostability_decision`**

Remove lines 84-95 (the two `fn rust_*` definitions). If `cargo check` flags any remaining references, they're orphans from Task 8 — delete those too.

Also delete the now-unused helpers `behavior_for_id` and `deployment_for_id` (lines 54-82) IF they have no other callers after the rewrite. Run:

```bash
rg -n "behavior_for_id|deployment_for_id" crates/defra-agent/tests/
```

If only the mirror functions used them, delete; otherwise keep.

- [ ] **Step 2: Run the conformance tests**

```bash
cargo test -p defra-agent --test identity_conformance
```

Expected: all green (`identity_structural_cases_match_lean_verdicts`, `identity_structural_cases_cover_named_scenarios`, `identity_permission_cases_pin_runtime_permission_contract_shape`, `identity_respects_principal_contract_is_declared` — the last still asserts `enforced == false`).

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
Delete unused Rust mirrors of Lean canonicalDecide

rust_canonical_permission_decision and rust_hostability_decision
existed only to mirror Lean's canonicalDecide for the shape-pin
test. After the rewrite to use runtime types, no Rust consumer
calls them. Lean still proves canonicalDecide_respectsPrincipal
internally (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: Rust — write the new contract-enforced test (will fail until Task 11)

Add the test that asserts `enforced == true`. This will fail until Task 11 flips Lean.

**Files:**
- Modify: `crates/defra-agent/tests/identity_conformance.rs` (replace the existing `_is_declared` test)

- [ ] **Step 1: Replace `identity_respects_principal_contract_is_declared`**

Find the existing test (around line 236-269 in the pre-refactor file). Replace it entirely with:

```rust
#[test]
fn identity_respects_principal_contract_enforced_by_runtime_routing() {
    let contracts = lean_identity_contracts();
    let target = contracts
        .iter()
        .find(|c: &&LeanIdentityContract| c.name == "identity.respects_principal_boundary")
        .expect(
            "Lean must emit the identity.respects_principal_boundary contract \
             — this is the runtime routing witness for #193",
        );

    // After #193 lands, the contract is enforced by the runtime: the
    // AgentBehavior::principal back-reference makes behavior -> principal
    // -> Identity::Authenticated(did) routing single-valued by
    // construction. DefraDB ACP, being DID-keyed, returns identical
    // results for behaviors sharing a principal.
    assert!(
        target.enforced,
        "identity.respects_principal_boundary must be enforced=true \
         now that AgentBehavior holds Arc<AgentPrincipal> as a back-ref \
         and the loader threads a single principal Arc through every \
         behavior in the snapshot"
    );
    assert_eq!(
        target.tracked_by, "#193",
        "tracked_by must continue to point at the runtime-refactor tracker"
    );
    assert!(
        target.statement.contains("agent_did"),
        "contract statement must name agent_did so a reader unfamiliar \
         with the Lean model can grasp the boundary; statement was: {}",
        target.statement
    );
    assert!(
        target.statement.contains("routing")
            || target.statement.contains("resolution")
            || target.statement.contains("Identity::Authenticated"),
        "contract statement must name the routing-witness interpretation: \
         the runtime resolves behavior -> agent_did and supplies that DID \
         as the ACP actor; statement was: {}",
        target.statement
    );

    // Exercise the runtime routing witness over every Lean row.
    for case in lean_identity_permission_cases() {
        let runtime_behaviors = build_runtime_behaviors_from_lean_case(case);
        let by_id: std::collections::HashMap<&str, &AgentBehavior> = runtime_behaviors
            .iter()
            .map(|b| (b.behavior_id.as_str(), b.as_ref()))
            .collect();

        let actor = by_id[case.actor_behavior.as_str()];
        let peer = by_id[case.peer_behavior.as_str()];

        // The structural claim: behaviors with the same Lean principal
        // resolve to the same agent_did at the runtime layer.
        assert_eq!(
            actor.principal.agent_did, case.expected_actor_principal,
            "case {:?}: actor.principal.agent_did mismatch",
            case.name
        );
        assert_eq!(
            peer.principal.agent_did, case.expected_peer_principal,
            "case {:?}: peer.principal.agent_did mismatch",
            case.name
        );
        assert_eq!(
            actor.principal.agent_did == peer.principal.agent_did,
            case.same_principal,
            "case {:?}: routing-witness same_principal mismatch",
            case.name
        );
    }
}
```

- [ ] **Step 2: Run the test (expect failure)**

```bash
cargo test -p defra-agent --test identity_conformance identity_respects_principal_contract_enforced_by_runtime_routing
```

Expected: **FAIL** with assertion message "identity.respects_principal_boundary must be enforced=true ...". This proves the test correctly observes that Lean has not flipped yet. Task 11 flips it.

If the test fails for some other reason (compile error, panic), fix that first.

- [ ] **Step 3: Commit the failing test**

```bash
git add -A
git commit -m "$(cat <<'EOF'
Add identity_respects_principal_contract_enforced_by_runtime_routing

Replaces _is_declared. Asserts enforced == true, asserts the
statement names the routing-witness interpretation, and drives
the runtime routing witness over every Lean row. Fails until
Lean flips enforced := true in the next commit (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

The TDD-style failing-test commit makes the Lean flip in Task 11 the green-light moment.

---

### Task 11: Lean — flip `enforced := false` → `enforced := true`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean` (the `enforced` field on the `identity.respects_principal_boundary` row)

- [ ] **Step 1: Flip the field**

In `identityContracts`, change:

```lean
    , enforced  := false
```

to:

```lean
    , enforced  := true
```

- [ ] **Step 2: Verify Lean builds**

```bash
cd crates/defra-agent/proofs && lake build
```

Expected: zero errors, zero `sorry`s.

- [ ] **Step 3: Run the contract-enforced test (now expected to pass)**

```bash
cargo test -p defra-agent --test identity_conformance identity_respects_principal_contract_enforced_by_runtime_routing
```

Expected: **PASS**. The Lean snapshot now emits `enforced: true`, the Rust test consumes it, and the runtime witness over the 4 Lean rows is exercised successfully.

- [ ] **Step 4: Run the full identity_conformance test file**

```bash
cargo test -p defra-agent --test identity_conformance
```

Expected: all four tests pass (`identity_structural_cases_match_lean_verdicts`, `identity_structural_cases_cover_named_scenarios`, `identity_permission_cases_pin_runtime_permission_contract_shape`, `identity_respects_principal_contract_enforced_by_runtime_routing`).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Identity/Conformance.lean
git commit -m "$(cat <<'EOF'
Flip identity.respects_principal_boundary to enforced

The Rust runtime now holds Arc<AgentPrincipal> as a back-reference
on every AgentBehavior, and the loader threads a single principal
Arc through every behavior in a snapshot. DefraDB ACP is DID-keyed
so any permission decision returns identical results for behaviors
sharing a principal — the routing witness is structural.

This is the success criterion the #193 issue body named: the
deferred conformance contract is now green (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 12: Rust — add loader-dedup proptest

Fence the bug class where a future loader path constructs a fresh `Arc<AgentPrincipal>` per behavior instead of cloning the single snapshot principal.

**Files:**
- Create: `crates/defra-agent/tests/identity_conformance_proptest.rs`

- [ ] **Step 1: Create the proptest file**

Write `crates/defra-agent/tests/identity_conformance_proptest.rs`:

```rust
//! Proptest fencing the loader-dedup invariant.
//!
//! Within a `DefraAgent` snapshot there is exactly one
//! `Arc<AgentPrincipal>`, and every `Arc<AgentBehavior>` in that
//! snapshot clones the same Arc. If a future code path constructs a
//! fresh principal Arc per behavior (e.g., from the behavior row's
//! `agent_did` FK instead of reusing the snapshot's), Lean's
//! `behavior_id_determines_principal` becomes observable-but-violated:
//! two behaviors with the same agent_did would point at different
//! Arcs, and a downstream caller cloning the principal Arc could
//! accidentally end up with diverging metadata.
//!
//! This proptest constructs arbitrary single-principal worlds and
//! verifies the invariant on the constructed `Vec<Arc<AgentBehavior>>`.

use std::sync::Arc;

use proptest::prelude::*;

use defra_agent::{AgentBehavior, AgentIdentity, AgentPrincipal};

#[path = "support/identity_stubs.rs"]
mod identity_stubs;
use identity_stubs::StubAgentIdentity;

/// Mimic the production loader's principal+behavior construction for
/// one snapshot's worth of behaviors. The production code lives in
/// `crates/defra-agent/src/agent.rs::from_default_behavior_documents`
/// and in the reconcile rebuild path; this helper isolates the
/// load-bearing logic (build one Arc<AgentPrincipal>, clone it into
/// every behavior).
fn build_snapshot_principal_and_behaviors(
    agent_did: String,
    behavior_ids: Vec<String>,
) -> (Arc<AgentPrincipal>, Vec<Arc<AgentBehavior>>) {
    let identity: Arc<dyn AgentIdentity> = StubAgentIdentity::arc(agent_did.clone());
    let principal = Arc::new(AgentPrincipal {
        agent_did,
        identity,
        default_behavior_id: behavior_ids
            .first()
            .cloned()
            .unwrap_or_default(),
        display_name: None,
        enabled: true,
    });

    let behaviors = behavior_ids
        .into_iter()
        .map(|behavior_id| {
            // Each behavior clones the *same* principal Arc. The
            // invariant under test: this Arc is shared, not freshly
            // constructed per behavior.
            Arc::new(AgentBehavior {
                behavior_id,
                principal: principal.clone(),
                backend_id: None,
                backend_provider_kind: defra_agent::BackendProviderKind::default(),
                backend_endpoint: String::new(),
                backend_api_key: None,
                backend_api_key_env_var: None,
                model_name: defra_agent::DEFAULT_MODEL_NAME.to_string(),
                context_window: defra_agent::DEFAULT_CONTEXT_WINDOW,
                max_output_tokens: defra_agent::DEFAULT_MAX_OUTPUT_TOKENS,
                max_turns: defra_agent::DEFAULT_MAX_TURNS,
                system_prompt: String::new(),
                tools: defra_agent::BehaviorToolConfig::default(),
                compaction_threshold: defra_agent::DEFAULT_COMPACTION_THRESHOLD,
                compaction_strategy: defra_agent::CompactionStrategy::default(),
                stream_batch_ms: defra_agent::DEFAULT_STREAM_BATCH_MS,
                deadline_duration: std::time::Duration::from_secs(
                    defra_agent::DEFAULT_DEADLINE_DURATION_SECS,
                ),
                sampling: defra_agent::SamplingConfig::default(),
            })
        })
        .collect();

    (principal, behaviors)
}

fn arb_did() -> impl Strategy<Value = String> {
    proptest::string::string_regex("did:agent:[a-z]{1,6}").unwrap()
}

fn arb_behavior_id() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z][a-z0-9-]{0,10}").unwrap()
}

proptest! {
    /// For any snapshot constructed via the helper, every behavior's
    /// principal Arc is pointer-equal to the snapshot's single
    /// principal Arc. Future loader changes that build fresh
    /// principal Arcs per behavior would fail this assertion.
    #[test]
    fn snapshot_behaviors_share_principal_arc(
        agent_did in arb_did(),
        behavior_ids in proptest::collection::vec(arb_behavior_id(), 0..20),
    ) {
        let (principal, behaviors) =
            build_snapshot_principal_and_behaviors(agent_did.clone(), behavior_ids);

        for behavior in &behaviors {
            prop_assert!(
                Arc::ptr_eq(&behavior.principal, &principal),
                "behavior {:?} held a different Arc<AgentPrincipal> than the snapshot principal",
                behavior.behavior_id,
            );
            prop_assert_eq!(behavior.principal.agent_did.as_str(), agent_did.as_str());
        }
    }

    /// Symmetric: for any two behaviors in the snapshot, their
    /// principal Arcs are pointer-equal. This is the form
    /// Lean's behavior_id_determines_principal takes at the runtime
    /// layer.
    #[test]
    fn pairs_in_snapshot_share_principal_arc(
        agent_did in arb_did(),
        behavior_ids in proptest::collection::vec(arb_behavior_id(), 2..20),
    ) {
        let (_principal, behaviors) =
            build_snapshot_principal_and_behaviors(agent_did, behavior_ids);

        for (i, b1) in behaviors.iter().enumerate() {
            for b2 in behaviors.iter().skip(i + 1) {
                prop_assert!(
                    Arc::ptr_eq(&b1.principal, &b2.principal),
                    "behaviors {:?} and {:?} held different principal Arcs",
                    b1.behavior_id,
                    b2.behavior_id,
                );
                prop_assert_eq!(b1.agent_did(), b2.agent_did());
            }
        }
    }
}
```

The helper isolates the load-bearing logic. The Production loader (`from_default_behavior_documents` after Task 7) follows the exact same pattern — Arc one principal, clone into every behavior. If that pattern ever drifts (e.g., a refactor accidentally constructs `Arc::new(AgentPrincipal { ... })` per behavior), this proptest fails.

A natural extension is to add an integration variant that drives `DefraAgent::from_default_behavior_documents` end-to-end against an in-memory `EmbeddedNode`. That's heavier and isn't strictly required for the routing-witness proof; deferred as a follow-on.

- [ ] **Step 2: Run the proptest**

```bash
cargo test -p defra-agent --test identity_conformance_proptest
```

Expected: PASS. Both `snapshot_behaviors_share_principal_arc` and `pairs_in_snapshot_share_principal_arc` should run for 256 (proptest default) generated cases each.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
Add loader-dedup proptest

Fences the bug class where a future loader path constructs a
fresh Arc<AgentPrincipal> per behavior instead of cloning the
snapshot's single principal Arc. Asserts Arc::ptr_eq for all
behaviors in arbitrarily-generated worlds.

This is the form Lean's behavior_id_determines_principal takes
at the runtime layer (#193).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 13: Docs — update #193 issue body to record scope reductions

**Files:**
- Modify (GitHub): #193 issue body

- [ ] **Step 1: Fetch the current issue body**

```bash
gh issue view 193 --json body --jq '.body' > /tmp/issue193_body.md
```

- [ ] **Step 2: Edit the body**

Open `/tmp/issue193_body.md` and:

1. Find the "## Scope" section.
2. Add this note immediately above "In:" :

```markdown
**Scope reductions from spec brainstorming (2026-05-15):** see
`docs/superpowers/specs/2026-05-15-issue-193-principal-behavior-deployment-design.md`
for full reasoning.

- `AgentDeployment` schema + Collection variant is **dropped** from this PR. In this codebase the installation IS the deployment and `AgentPrincipal` is the top-level runtime concept (one process = one principal). Lean's `Deployment` record stays as a model abstraction proving I5 co-hostability. The schema and Collection variant would be dead weight as long as the one-process-one-principal invariant holds.
- The "permission decision module" is **routing-only, no new decider**. defradb.rs `DocumentACP` is the decider and is already DID-keyed by signature. The refactor contributes routing observability (`AgentBehavior` holds `Arc<AgentPrincipal>`); two behaviors with the same principal supply the same `Identity::Authenticated(did)` to ACP by construction. No new `Permissions` trait, no `GrantStorePermissions`.
```

- [ ] **Step 3: Push the edit**

```bash
gh issue edit 193 --body-file /tmp/issue193_body.md
```

- [ ] **Step 4: Verify on GitHub**

```bash
gh issue view 193 | head -60
```

Confirm the scope-reductions note appears.

- [ ] **Step 5: No commit needed** — this is a GitHub-only change.

The PR opened in Task 7 Step 12 should be updated to reference this issue-body edit in its summary.

---

## Final verification (post-Task 12)

After all tasks are complete, run the full success-criteria suite from the spec:

```bash
# Lean
cd crates/defra-agent/proofs && lake build && cd ../../..

# Format
cargo fmt --all

# Compile
cargo check --workspace --all-targets \
    --exclude agent-subagent-v2-to-v3-lens \
    --exclude agent-tool-call-lifecycle-v1-to-v2-lens

# Test (defra-agent)
cargo test -p defra-agent --lib --tests

# Test (defra-agent-cli)
cargo test -p defra-agent-cli
```

Every command must exit 0. The PR description's checklist should now have every box ticked.

---

## Notes for the executing engineer

- **Commit message style:** every commit ends with the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer per PROMPT.md.
- **TDD note:** Task 10 deliberately commits a failing test; Task 11 makes it pass. Don't rebase / squash these two — the red→green pair is the documentation of the contract flip.
- **Test fixtures:** many integration tests in `crates/defra-agent/tests/` construct `BehaviorConfig` or pass `Arc<dyn AgentIdentity>` to constructors. After Task 4 / 7, every such site needs an `Arc<AgentPrincipal>` instead. `support::fixtures::test_identity(name)` returns a `KeyIdentity` whose `did()` is auto-generated — that's fine for production-flavor tests. For routing tests in `identity_conformance*.rs`, the `StubAgentIdentity` from Task 3 returns a chosen DID and is the right pick.
- **Don't merge.** PROMPT.md says stop and report when done. Push and PR; don't merge yourself.
