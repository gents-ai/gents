# Issue #302 — Desktop UI Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the shared scaffolding (Rust bridge types, Tauri command stubs, signature/throttle helpers, and an empty React `OperationsRail`) that the nine operator-surfaces panel PRs build on top of.

**Architecture:** Strictly additive extension. New Rust types live in a new `bridge/types/views/operations.rs` module re-exported through the existing pipeline; new Tauri command stubs live in a new `bridge/tauri_commands/operations.rs` registered through the existing `invoke_handler`; signature/emit-floor logic lives in a new `bridge/snapshot/operations_signature.rs` with pure unit tests; a new `components/operations/` directory holds the empty tabbed `OperationsRail` mounted at the place the spec calls out, but with zero visible tabs until panel PRs populate it.

**Tech Stack:**
- Rust (`defra-agent-desktop-tauri` crate) + `tauri` + `serde`
- `blake3 = "1"` newly declared in `apps/desktop-tauri/src-tauri/Cargo.toml` (already transitively available; the spec permits BLAKE3 or SHA-256 and we lock BLAKE3 here)
- TypeScript + React 19 in `apps/desktop-tauri/src/` (vitest + @testing-library/react for component tests)

**Scope guardrails:**
- Do NOT implement any of the 9 panels (#276, #277, #278, #281, #283, #284, #285, #286, #288). Their Tauri commands stay stubbed.
- Do NOT rename, remove, or refactor existing chat shell components, types, or Tauri commands. New types/files only.
- Do NOT wire a real liveness HTTP probe — that is panel #277's work. We ship the floor + signature logic as pure, testable helpers; panel #277 plugs them into a watcher that fetches `/status`.
- Use `components/operations/` (matches the design spec; all 9 panels co-locate here). PROMPT.md's `operationsRail/` path is superseded — confirmed with user during planning.
- Use BLAKE3 as the hash algorithm — confirmed with user during planning.

**Source-of-truth references:**
- Design spec: `docs/superpowers/specs/2026-05-20-desktop-operator-surfaces-design.md`
  - §"Shared Data Layer" (line 739)
  - §"Liveness Watcher Emit Floor" (line 765)
  - §"Operations Snapshot Type" (line 799)
  - §"New Tauri Commands" (line 897)
  - §"Component Decomposition" (line 972)
  - §"Cascade Cancel UX" preview signature encoding (line 696)
  - `SubagentTreeView` type definition (line 322)
  - `CascadeCancelPreview` type definition (line 644)
- Runtime data sources the snapshot will project (read-only, no changes here):
  - `crates/defra-agent-cli/src/http/liveness.rs:1` — `RuntimeLivenessSnapshot`, `ActiveRequest`, `ActiveToolCall`
  - `crates/defra-agent/src/native_executor_status.rs` — `NativeExecutorStatus`
- Existing bridge to extend:
  - `apps/desktop-tauri/src-tauri/src/bridge/snapshot.rs:1`
  - `apps/desktop-tauri/src-tauri/src/bridge/state.rs:1`
  - `apps/desktop-tauri/src-tauri/src/bridge/mod.rs:1` — `invoke_handler` registration
  - `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands.rs:1` — submodule index
  - `apps/desktop-tauri/src-tauri/src/bridge/types/views.rs:1` — view re-export index

---

## File Structure

### Rust (new files)
- `apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs` — all `*View` structs that mirror the spec's TS types (`DesktopOperationsSnapshot`, `RuntimeLivenessView`, `ActiveRequestView`, `ActiveToolCallView`, `NativeExecutorStatusView`, `BackgroundedToolView`, `StuckWorkDiagnosticView`, `SubagentTreeView`, `SubagentNodeView`, `SubagentEdgeView`, `CascadeCancelPreview`, `CascadeAffectedRequest`, `InterruptRequestResult`).
- `apps/desktop-tauri/src-tauri/src/bridge/types/requests/operations.rs` — request param structs for the four new Tauri commands (`DesktopOperationsSnapshotRequest`, `DesktopListSubagentTreeRequest`, `DesktopPreviewInterruptCascadeRequest`, `DesktopInterruptRequest`).
- `apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_signature.rs` — pure functions:
  - `compute_liveness_signature(input: &LivenessSignatureInput) -> String` (hex-lowercase BLAKE3).
  - `compute_preview_signature(input: &PreviewSignatureInput) -> String` (hex-lowercase BLAKE3).
  - `LivenessEmitFloor` state machine: tracks last-emit instant, pending-coalesce instant, exposes `observe(signature, now) -> EmitDecision`.
- `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs` — four stub commands that compile but `unimplemented!("…")` in body, referencing the panel issue that fills each one in.

### Rust (modified files)
- `apps/desktop-tauri/src-tauri/Cargo.toml` — add `blake3 = "1"` to `[dependencies]`.
- `apps/desktop-tauri/src-tauri/src/bridge/types/views.rs` — add `mod operations` + re-export.
- `apps/desktop-tauri/src-tauri/src/bridge/types/requests.rs` — add `mod operations` + re-export. (Inspect this file first: it currently lives at `bridge/types/requests.rs` per the `#[path = "types/requests.rs"]` indirection — verify before adding the submodule.)
- `apps/desktop-tauri/src-tauri/src/bridge/snapshot.rs` — add `mod operations_signature;` + `pub(crate) use` of the helpers we want callable from siblings.
- `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands.rs` — add `pub(crate) mod operations;`.
- `apps/desktop-tauri/src-tauri/src/bridge/mod.rs` — register the four new commands in `invoke_handler`.

### TypeScript (new files)
- `apps/desktop-tauri/src/lib/types/operations.ts` — 1:1 type mirrors of the Rust view structs (camelCase, `import type` only).
- `apps/desktop-tauri/src/components/operations/OperationsRail.tsx` — top-level tabbed container component. Owns the `activeTab` state. Exposes `setActiveTab(tabId)` via a context provider so panels in their own PRs can call it from a `useOperationsRail()` hook.
- `apps/desktop-tauri/src/components/operations/OperationsRailTabs.tsx` — renders the tab strip from the registered tabs prop.
- `apps/desktop-tauri/src/components/operations/OperationsRailTabPanel.tsx` — renders the active tab's content (mount-at-most-one).
- `apps/desktop-tauri/src/components/operations/operationsRailContext.ts` — React context + `useOperationsRail()` hook + `OperationsRailTabId` type.
- `apps/desktop-tauri/src/components/operations/index.ts` — barrel exports.

### TypeScript (modified files)
- `apps/desktop-tauri/src/lib/types.ts` — re-export operations types.
- `apps/desktop-tauri/src/components/ChatWorkspace.tsx` — mount `<OperationsRail />` inside the existing `section.chat-workspace` per the spec's diagram (line 1003). Initially empty (zero registered tabs).

### Tests (new files)
- `apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_signature_tests.rs` (or inline `#[cfg(test)] mod tests`) — covers the signature determinism, sort independence, and emit-floor scheduling behaviour.
- `apps/desktop-tauri/tests/operations-rail.test.tsx` — vitest + @testing-library: renders `OperationsRail` with no tabs (empty), and with a fake registered tab confirms `setActiveTab` switches.

---

## Self-Review Reminders

After each task, before committing:
- Re-check the spec's exact field names, types, and nullability for any types you wrote.
- `cargo check -p defra_agent_desktop_tauri` from repo root.
- `pnpm test` from `apps/desktop-tauri/` (or `npm test` — check `package.json`; runner is `vitest run`).
- The Lean proofs are unchanged; we should not need to touch `crates/defra-agent/proofs/`.

---

## Task 1: Declare `blake3` dependency in `defra-agent-desktop-tauri`

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/Cargo.toml`

- [ ] **Step 1: Add `blake3` to `[dependencies]`**

Open `apps/desktop-tauri/src-tauri/Cargo.toml` and add after the existing `chrono.workspace = true` line:

```toml
blake3 = "1"
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p defra-agent-desktop-tauri`
Expected: success, possibly with a small download/build of `blake3` on first compile.

- [ ] **Step 3: Commit**

```bash
git add apps/desktop-tauri/src-tauri/Cargo.toml apps/desktop-tauri/src-tauri/Cargo.lock 2>/dev/null || true
# Cargo.lock lives at repo root for workspaces:
git add Cargo.lock
git commit -m "$(cat <<'EOF'
bridge: declare blake3 dep for operator-surfaces signatures

Picks BLAKE3 over SHA-256 per design spec line 725-726. Available
transitively; this just locks the choice.

Refs #302.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add Rust view types for operations snapshot

**Files:**
- Create: `apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/types/views.rs`

- [ ] **Step 1: Inspect current `views.rs` so the new `mod` follows the existing pattern**

```bash
cat apps/desktop-tauri/src-tauri/src/bridge/types/views.rs
```

Expected: each sibling view file is loaded via `#[path = "views/<name>.rs"] mod <name>;` followed by `pub(crate) use <name>::*;`.

- [ ] **Step 2: Create the new view module**

Create `apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs` with the following exact content:

```rust
//! Operator-surfaces view types per
//! docs/superpowers/specs/2026-05-20-desktop-operator-surfaces-design.md
//! "Operations Snapshot Type" (line ~799). Stubs only — the panels in their
//! own PRs (#276/#277/#278/#281/#283/#284/#285/#286/#288) build and populate
//! these structs.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopOperationsSnapshot {
    pub fetched_at: String,
    pub agent_did: Option<String>,
    pub liveness: Option<RuntimeLivenessView>,
    pub liveness_unavailable_reason: Option<String>,
    pub backgrounded_tools: Vec<BackgroundedToolView>,
    pub stuck_diagnostics: Vec<StuckWorkDiagnosticView>,
    pub lineage: Option<SubagentTreeView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeLivenessView {
    pub expired_processing_count: i64,
    pub requests: Vec<ActiveRequestView>,
    pub active_tool_calls: Vec<ActiveToolCallView>,
    pub active_native_executors_available: bool,
    pub active_native_executors: Vec<NativeExecutorStatusView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActiveRequestView {
    pub request_id: String,
    pub claimed_at: Option<String>,
    pub deadline: Option<String>,
    pub deadline_expired: bool,
    pub deadline_age_ms: Option<i64>,
    pub last_progress_age_ms: i64,
    pub subagent_depth: i64,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_trigger_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActiveToolCallView {
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub started_at: Option<String>,
    pub deadline_at: Option<String>,
    pub await_mode: Option<String>,
    pub running_age_ms: i64,
    pub deadline_expired: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeExecutorStatusView {
    pub id: i64,
    pub pid: u32,
    pub argv0: String,
    pub tool_name: Option<String>,
    pub started_at: String,
    pub age_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackgroundedToolView {
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub lifecycle_state: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub age_ms: Option<i64>,
    pub deadline_at: Option<String>,
    pub deadline_expired: bool,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub child_request_id: Option<String>,
    pub native_executor: Option<NativeExecutorStatusView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StuckWorkDiagnosticView {
    pub request_id: String,
    pub session_id: Option<String>,
    pub severity: String,
    pub reason: String,
    pub deadline_age_ms: Option<i64>,
    pub last_progress_age_ms: Option<i64>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub stuck_since: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentTreeView {
    pub root_request_id: String,
    pub nodes: Vec<SubagentNodeView>,
    pub edges: Vec<SubagentEdgeView>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentNodeView {
    pub request_id: String,
    pub session_id: Option<String>,
    pub agent_did: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub status: Option<String>,
    pub subagent_depth: Option<i64>,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentEdgeView {
    pub parent_request_id: String,
    pub child_request_id: String,
    pub parent_tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub lifecycle_state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CascadeCancelPreview {
    pub root_request_id: String,
    pub preview_signature: String,
    pub root_state: Option<String>,
    pub will_interrupt: Vec<CascadeAffectedRequest>,
    pub will_detach: Vec<CascadeAffectedRequest>,
    pub already_terminal: Vec<CascadeAffectedRequest>,
    pub unknown_policy: Vec<CascadeAffectedRequest>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CascadeAffectedRequest {
    pub request_id: String,
    pub session_id: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub parent_request_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
}

/// Result envelope for `desktop_interrupt_request`. Field semantics are
/// normative per the design spec line 922–942:
/// - `accepted = true` iff the bridge latched (or confirmed already-latched)
///   `interrupt_requested_at` for `request_id`.
/// - `already_interrupted = true` iff the field was non-null prior to the
///   call; `accepted` is still `true` in that case.
/// - `stale_preview = true` is mutually exclusive with `accepted = true`.
///   On signature mismatch the bridge returns `accepted: false`,
///   `stale_preview: true`, and a fresh `preview` for the UI to redraw.
/// - `interrupt_requested_at` is the canonical timestamp the bridge observed
///   on the document after the call. Null only on a non-already-interrupted
///   failure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InterruptRequestResult {
    pub request_id: String,
    pub accepted: bool,
    pub interrupt_requested_at: Option<String>,
    pub already_interrupted: bool,
    pub stale_preview: bool,
    pub preview: Option<CascadeCancelPreview>,
}
```

- [ ] **Step 3: Register the new sub-module in `views.rs`**

Open `apps/desktop-tauri/src-tauri/src/bridge/types/views.rs` and add the new module/re-export following the existing pattern. The file looks like:

```rust
#[path = "views/bootstrap.rs"]
mod bootstrap;
#[path = "views/deployment.rs"]
mod deployment;
#[path = "views/events.rs"]
mod events;
#[path = "views/session.rs"]
mod session;

pub(crate) use bootstrap::*;
pub(crate) use deployment::*;
pub(crate) use events::*;
pub(crate) use session::*;
```

Insert (alphabetically) so it becomes:

```rust
#[path = "views/bootstrap.rs"]
mod bootstrap;
#[path = "views/deployment.rs"]
mod deployment;
#[path = "views/events.rs"]
mod events;
#[path = "views/operations.rs"]
mod operations;
#[path = "views/session.rs"]
mod session;

pub(crate) use bootstrap::*;
pub(crate) use deployment::*;
pub(crate) use events::*;
pub(crate) use operations::*;
pub(crate) use session::*;
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p defra-agent-desktop-tauri`
Expected: success. (You will see "unused import" warnings on the new types since nothing consumes them yet — that's fine, they'll be consumed by later tasks within the same PR.)

- [ ] **Step 5: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/types/views/operations.rs \
        apps/desktop-tauri/src-tauri/src/bridge/types/views.rs
git commit -m "$(cat <<'EOF'
bridge: add operator-surfaces view types

Mirrors the design spec "Operations Snapshot Type" section (line 799) and
the SubagentTree / CascadeCancelPreview / InterruptRequestResult shapes
that the nine panel PRs build on. No consumers yet — these are stubs that
panel PRs will populate.

Refs #302.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add Rust request-param types for the new Tauri commands

**Files:**
- Create: `apps/desktop-tauri/src-tauri/src/bridge/types/requests/operations.rs`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/types/requests.rs`

- [ ] **Step 1: Inspect current `requests.rs`**

```bash
cat apps/desktop-tauri/src-tauri/src/bridge/types/requests.rs | head -30
```

If the existing pattern matches `views.rs` (single file holding all request structs, no submodule indirection), prefer extending that file in-place rather than adding a sub-module. If it already does have submodules, follow that pattern.

- [ ] **Step 2: Add the request types**

If `requests.rs` is a single file: append the following to the end of `apps/desktop-tauri/src-tauri/src/bridge/types/requests.rs`:

If the file uses submodules: create `apps/desktop-tauri/src-tauri/src/bridge/types/requests/operations.rs` with the same content and register it in `requests.rs` the same way `views.rs` registers `operations`.

Content:

```rust
// --- operator-surfaces request params (issue #302) ---

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopOperationsSnapshotRequest {
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub root_request_id: Option<String>,
    #[serde(default)]
    pub include_terminal: Option<bool>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopListSubagentTreeRequest {
    pub root_request_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub include_terminal: Option<bool>,
    #[serde(default)]
    pub max_depth: Option<u32>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPreviewInterruptCascadeRequest {
    pub request_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub include_terminal: Option<bool>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopInterruptRequest {
    pub request_id: String,
    /// Currently always `"userCancelled"` per spec line 907. Kept as a String
    /// so future cause variants don't require an enum migration here.
    pub cause: String,
    pub cascade: bool,
    #[serde(default)]
    pub expected_preview_signature: Option<String>,
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p defra-agent-desktop-tauri`
Expected: success with unused-warning on the new structs.

- [ ] **Step 4: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/types/requests.rs \
        apps/desktop-tauri/src-tauri/src/bridge/types/requests/operations.rs 2>/dev/null || true
git commit -m "$(cat <<'EOF'
bridge: add operator-surfaces request param types

Pins the Tauri command parameter shapes from the design spec
"New Tauri Commands" table (line 902). Panels in #276/#277/#278/#281/
#283/#284/#285/#286/#288 build against these directly.

Refs #302.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Implement BLAKE3 signature helpers (preview + liveness)

**Files:**
- Create: `apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_signature.rs`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/snapshot.rs`

The spec defines two signatures:
- **`previewSignature`** over a cascade preview (spec line 696-727): header is `(rootRequestId, rootState, root.interrupt_requested_at)` joined with `0x1F`, then `0x1E`, then the sorted-by-`requestId` rows where each row has fields `(requestId, lifecycleState, awaitMode, cancelPolicy, parentToolCallId)` joined with `0x1D`.
- **Liveness signature** (spec line 775-779): over `(requests[].requestId, requests[].lifecycleState, requests[].deadlineExpired, activeToolCalls[].toolCallId, activeToolCalls[].lifecycleState, expiredProcessingCount, activeNativeExecutorsAvailable)`. The spec says "same kind of signature as `previewSignature`" — we encode it with the same control-byte separators and stable sort.

The emit floor (spec line 765-790) keeps per-watcher state and exposes a `observe(signature, now) -> EmitDecision` API:
- Minimum 250ms inter-emit. If a structural change arrives within 250ms, defer.
- Maximum coalescing window 2s.
- Never silently drop a structural change; trailing emit reflects latest state.

- [ ] **Step 1: Write the failing tests first (TDD)**

Create `apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_signature.rs` with the test module pre-populated. The first version of this file should contain only the *test* module and empty `pub` stubs so we can watch them fail.

```rust
//! BLAKE3 signature helpers and emit-floor state for the operations
//! snapshot watcher. See design spec lines 696-727 (previewSignature) and
//! 765-790 (emit floor).

use std::time::{Duration, Instant};

// --- Preview signature -------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct PreviewSignatureInput {
    pub root_request_id: String,
    pub root_state: Option<String>,
    pub root_interrupt_requested_at: Option<String>,
    pub affected: Vec<PreviewSignatureRow>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PreviewSignatureRow {
    pub request_id: String,
    pub lifecycle_state: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub parent_tool_call_id: Option<String>,
}

pub(crate) fn compute_preview_signature(_input: &PreviewSignatureInput) -> String {
    unimplemented!("Task 4 step 3")
}

// --- Liveness signature ------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct LivenessSignatureInput {
    pub expired_processing_count: i64,
    pub active_native_executors_available: bool,
    pub requests: Vec<LivenessSignatureRequest>,
    pub tool_calls: Vec<LivenessSignatureToolCall>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LivenessSignatureRequest {
    pub request_id: String,
    pub lifecycle_state: Option<String>,
    pub deadline_expired: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LivenessSignatureToolCall {
    pub tool_call_id: String,
    pub lifecycle_state: Option<String>,
}

pub(crate) fn compute_liveness_signature(_input: &LivenessSignatureInput) -> String {
    unimplemented!("Task 4 step 3")
}

// --- Emit floor --------------------------------------------------------

pub(crate) const EMIT_FLOOR_MIN_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const EMIT_FLOOR_MAX_COALESCE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EmitDecision {
    /// Emit the new signature now.
    EmitNow,
    /// No structural change vs. the last observed/emitted signature; do not emit.
    NoChange,
    /// Structural change detected, but we must wait until `at` to emit (250ms floor).
    /// The watcher should arm a timer for `at` and re-call `observe` then.
    Defer { at: Instant },
}

#[derive(Debug, Default)]
pub(crate) struct LivenessEmitFloor {
    last_emitted_signature: Option<String>,
    last_emit_at: Option<Instant>,
    pending_change_first_seen_at: Option<Instant>,
}

impl LivenessEmitFloor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Observe the latest signature at the wall-clock instant `now`.
    /// The watcher is expected to call this both on probe ticks and on
    /// any signal it has that the signature may have changed.
    pub(crate) fn observe(&mut self, signature: &str, now: Instant) -> EmitDecision {
        // Same signature as last emit: nothing to do; clear any pending change.
        if self.last_emitted_signature.as_deref() == Some(signature) {
            self.pending_change_first_seen_at = None;
            return EmitDecision::NoChange;
        }

        // Track when we first observed this changed signature so we can
        // honour the 2-second coalescing ceiling.
        let first_seen = *self
            .pending_change_first_seen_at
            .get_or_insert(now);

        // Inter-emit floor: 250ms minimum.
        if let Some(last) = self.last_emit_at {
            let since_last = now.saturating_duration_since(last);
            if since_last < EMIT_FLOOR_MIN_INTERVAL {
                // We owe an emit, but not yet. If the coalesce ceiling has elapsed,
                // emit anyway — we must not silently drop a structural change.
                if now.saturating_duration_since(first_seen) >= EMIT_FLOOR_MAX_COALESCE {
                    self.commit_emit(signature, now);
                    return EmitDecision::EmitNow;
                }
                return EmitDecision::Defer {
                    at: last + EMIT_FLOOR_MIN_INTERVAL,
                };
            }
        }

        self.commit_emit(signature, now);
        EmitDecision::EmitNow
    }

    fn commit_emit(&mut self, signature: &str, now: Instant) {
        self.last_emitted_signature = Some(signature.to_owned());
        self.last_emit_at = Some(now);
        self.pending_change_first_seen_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        // Use a fixed monotonic anchor; we only care about relative offsets.
        Instant::now()
    }

    #[test]
    fn preview_signature_is_deterministic_under_row_reordering() {
        let row_a = PreviewSignatureRow {
            request_id: "req-a".into(),
            lifecycle_state: Some("processing".into()),
            await_mode: Some("foreground".into()),
            cancel_policy: Some("cascade".into()),
            parent_tool_call_id: Some("tc-1".into()),
        };
        let row_b = PreviewSignatureRow {
            request_id: "req-b".into(),
            lifecycle_state: Some("claimed".into()),
            await_mode: Some("background".into()),
            cancel_policy: Some("detach".into()),
            parent_tool_call_id: None,
        };
        let input_one = PreviewSignatureInput {
            root_request_id: "req-root".into(),
            root_state: Some("processing".into()),
            root_interrupt_requested_at: None,
            affected: vec![row_a.clone(), row_b.clone()],
        };
        let input_two = PreviewSignatureInput {
            root_request_id: "req-root".into(),
            root_state: Some("processing".into()),
            root_interrupt_requested_at: None,
            affected: vec![row_b, row_a],
        };

        assert_eq!(
            compute_preview_signature(&input_one),
            compute_preview_signature(&input_two)
        );
    }

    #[test]
    fn preview_signature_changes_when_root_state_changes() {
        let mut input = PreviewSignatureInput {
            root_request_id: "req-root".into(),
            root_state: Some("processing".into()),
            root_interrupt_requested_at: None,
            affected: vec![],
        };
        let before = compute_preview_signature(&input);
        input.root_state = Some("interrupted".into());
        let after = compute_preview_signature(&input);
        assert_ne!(before, after);
    }

    #[test]
    fn preview_signature_returns_lowercase_hex_64_chars() {
        let sig = compute_preview_signature(&PreviewSignatureInput {
            root_request_id: "req-root".into(),
            ..Default::default()
        });
        assert_eq!(sig.len(), 64, "BLAKE3 hex is 64 chars");
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase())));
    }

    #[test]
    fn liveness_signature_changes_when_expired_processing_count_changes() {
        let base = LivenessSignatureInput {
            expired_processing_count: 0,
            active_native_executors_available: true,
            requests: vec![LivenessSignatureRequest {
                request_id: "req-1".into(),
                lifecycle_state: Some("processing".into()),
                deadline_expired: false,
            }],
            tool_calls: vec![],
        };
        let with_expiry = LivenessSignatureInput {
            expired_processing_count: 1,
            ..base.clone()
        };
        assert_ne!(
            compute_liveness_signature(&base),
            compute_liveness_signature(&with_expiry)
        );
    }

    #[test]
    fn liveness_signature_is_stable_when_only_progress_age_drifts() {
        // The signature spec does NOT include lastProgressAgeMs — drift on age
        // alone must not invalidate the signature.
        let base = LivenessSignatureInput {
            expired_processing_count: 0,
            active_native_executors_available: true,
            requests: vec![LivenessSignatureRequest {
                request_id: "req-1".into(),
                lifecycle_state: Some("processing".into()),
                deadline_expired: false,
            }],
            tool_calls: vec![LivenessSignatureToolCall {
                tool_call_id: "tc-1".into(),
                lifecycle_state: Some("running".into()),
            }],
        };
        assert_eq!(
            compute_liveness_signature(&base),
            compute_liveness_signature(&base.clone())
        );
    }

    #[test]
    fn emit_floor_emits_on_first_observation() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let decision = floor.observe("sig-a", now);
        assert_eq!(decision, EmitDecision::EmitNow);
    }

    #[test]
    fn emit_floor_returns_no_change_when_signature_unchanged() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);
        let decision = floor.observe("sig-a", now + Duration::from_millis(500));
        assert_eq!(decision, EmitDecision::NoChange);
    }

    #[test]
    fn emit_floor_defers_within_250ms_window() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);
        let decision = floor.observe("sig-b", now + Duration::from_millis(100));
        match decision {
            EmitDecision::Defer { at } => {
                assert_eq!(at, now + EMIT_FLOOR_MIN_INTERVAL);
            }
            other => panic!("expected Defer, got {other:?}"),
        }
    }

    #[test]
    fn emit_floor_emits_after_250ms_window() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);
        let decision = floor.observe("sig-b", now + Duration::from_millis(260));
        assert_eq!(decision, EmitDecision::EmitNow);
    }

    #[test]
    fn emit_floor_emits_after_sustained_pending_period() {
        // After a Defer at t=50ms, if the watcher comes back at t=2100ms
        // (long past both the 250ms floor and the 2s ceiling) the call must
        // emit, never silently drop the pending change.
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);

        let defer_decision = floor.observe("sig-b", now + Duration::from_millis(50));
        match defer_decision {
            EmitDecision::Defer { .. } => {}
            other => panic!("expected Defer at t=50ms, got {other:?}"),
        }

        let final_decision = floor.observe("sig-b", now + Duration::from_millis(2100));
        assert_eq!(final_decision, EmitDecision::EmitNow);
    }

    #[test]
    fn emit_floor_uses_latest_signature_in_trailing_emit() {
        let mut floor = LivenessEmitFloor::new();
        let now = t0();
        let _ = floor.observe("sig-a", now);
        let _ = floor.observe("sig-b", now + Duration::from_millis(50));
        let _ = floor.observe("sig-c", now + Duration::from_millis(200));
        let final_decision = floor.observe("sig-d", now + Duration::from_millis(260));
        assert_eq!(final_decision, EmitDecision::EmitNow);
        // After emit, the most recent signature ("sig-d") should be what's
        // recorded as last_emitted; observing it again should NoChange.
        assert_eq!(
            floor.observe("sig-d", now + Duration::from_millis(600)),
            EmitDecision::NoChange
        );
    }
}
```

- [ ] **Step 2: Run the tests; confirm they fail with `unimplemented`**

Add the module to `snapshot.rs`. Open `apps/desktop-tauri/src-tauri/src/bridge/snapshot.rs` and after the line:

```rust
#[path = "snapshot/runtime.rs"]
mod runtime;
pub(crate) use runtime::build_runtime_snapshot;
```

insert:

```rust
#[path = "snapshot/operations_signature.rs"]
mod operations_signature;
#[cfg(test)]
pub(crate) use operations_signature::*;
#[allow(unused_imports)]
pub(crate) use operations_signature::{
    compute_liveness_signature, compute_preview_signature, EmitDecision, LivenessEmitFloor,
    LivenessSignatureInput, LivenessSignatureRequest, LivenessSignatureToolCall,
    PreviewSignatureInput, PreviewSignatureRow,
};
```

Run: `cargo test -p defra-agent-desktop-tauri operations_signature -- --nocapture`
Expected: each test panics with "not implemented: Task 4 step 3" — that's the red bar.

- [ ] **Step 3: Implement the signature functions**

Replace the two `unimplemented!()` bodies with real implementations. Replace the placeholder `compute_preview_signature` body with:

```rust
pub(crate) fn compute_preview_signature(input: &PreviewSignatureInput) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(input.root_request_id.as_bytes());
    hasher.update(&[0x1F]);
    hasher.update(input.root_state.as_deref().unwrap_or("").as_bytes());
    hasher.update(&[0x1F]);
    hasher.update(
        input
            .root_interrupt_requested_at
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    hasher.update(&[0x1E]);

    let mut sorted: Vec<&PreviewSignatureRow> = input.affected.iter().collect();
    sorted.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    for (idx, row) in sorted.iter().enumerate() {
        if idx > 0 {
            hasher.update(&[0x1F]);
        }
        hasher.update(row.request_id.as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.lifecycle_state.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.await_mode.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.cancel_policy.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(
            row.parent_tool_call_id
                .as_deref()
                .unwrap_or("")
                .as_bytes(),
        );
    }

    hasher.finalize().to_hex().to_string()
}
```

And replace `compute_liveness_signature`:

```rust
pub(crate) fn compute_liveness_signature(input: &LivenessSignatureInput) -> String {
    let mut hasher = blake3::Hasher::new();
    // Header: scalar fields.
    hasher.update(&input.expired_processing_count.to_le_bytes());
    hasher.update(&[0x1F]);
    hasher.update(&[input.active_native_executors_available as u8]);
    hasher.update(&[0x1E]);

    // Requests, sorted by request_id.
    let mut requests: Vec<&LivenessSignatureRequest> = input.requests.iter().collect();
    requests.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    for (idx, row) in requests.iter().enumerate() {
        if idx > 0 {
            hasher.update(&[0x1F]);
        }
        hasher.update(row.request_id.as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.lifecycle_state.as_deref().unwrap_or("").as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(&[row.deadline_expired as u8]);
    }
    hasher.update(&[0x1E]);

    // Tool calls, sorted by tool_call_id.
    let mut tool_calls: Vec<&LivenessSignatureToolCall> = input.tool_calls.iter().collect();
    tool_calls.sort_by(|a, b| a.tool_call_id.cmp(&b.tool_call_id));
    for (idx, row) in tool_calls.iter().enumerate() {
        if idx > 0 {
            hasher.update(&[0x1F]);
        }
        hasher.update(row.tool_call_id.as_bytes());
        hasher.update(&[0x1D]);
        hasher.update(row.lifecycle_state.as_deref().unwrap_or("").as_bytes());
    }

    hasher.finalize().to_hex().to_string()
}
```

- [ ] **Step 4: Run tests; confirm they pass**

Run: `cargo test -p defra-agent-desktop-tauri operations_signature`
Expected: all 9 tests pass.

- [ ] **Step 5: Run the full crate test suite to verify no regression**

Run: `cargo test -p defra-agent-desktop-tauri`
Expected: pass (we haven't touched any prior tests).

- [ ] **Step 6: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/snapshot.rs \
        apps/desktop-tauri/src-tauri/src/bridge/snapshot/operations_signature.rs
git commit -m "$(cat <<'EOF'
bridge: BLAKE3 preview/liveness signatures and 250ms/2s emit floor

Implements the two normative signatures from the design spec — the
cascade-preview signature (line 696) and the liveness signature
(line 775) — plus the LivenessEmitFloor state machine that enforces
the 250ms minimum inter-emit window and 2s coalescing ceiling without
silently dropping structural changes.

Pure helpers with 9 unit tests; not wired into a watcher yet. The
watcher that consumes these is panel #277's responsibility once the
liveness HTTP probe lands.

Refs #302.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Stub the four Tauri commands

**Files:**
- Create: `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands.rs`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/mod.rs`

The four commands per spec line 902:

| Command | Implemented by |
|---|---|
| `desktop_operations_snapshot` | panel #277 (operations projection) |
| `desktop_list_subagent_tree` | panel #285 (subagent lineage) |
| `desktop_preview_interrupt_cascade` | panel #286 (cascade cancel UX) |
| `desktop_interrupt_request` | panel #283 (interrupt button) |

Per PROMPT.md the stub bodies use `unimplemented!()` with explicit issue references; they compile and register, but cannot succeed at runtime. Since no panel yet calls them, the panics are unreachable.

- [ ] **Step 1: Create the stubs file**

Create `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs`:

```rust
//! Tauri command stubs for operator-surfaces panels. Each command's body
//! is `unimplemented!()` until the named panel issue replaces it with the
//! real implementation. Until then no panel UI calls these — the stubs
//! exist so the panel PRs can be reviewed as additive replacements rather
//! than additive surface area + replacement combined.

use tauri::State;

use super::super::state::DesktopAppState;
use super::super::types::{
    CascadeCancelPreview, DesktopInterruptRequest, DesktopListSubagentTreeRequest,
    DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest,
    DesktopPreviewInterruptCascadeRequest, InterruptRequestResult, SubagentTreeView,
};

#[tauri::command]
pub(crate) async fn desktop_operations_snapshot(
    _state: State<'_, DesktopAppState>,
    _request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot, String> {
    unimplemented!(
        "desktop_operations_snapshot is implemented by panel #277 (backgrounded tools) / \
         operations projection follow-up"
    )
}

#[tauri::command]
pub(crate) async fn desktop_list_subagent_tree(
    _state: State<'_, DesktopAppState>,
    _request: DesktopListSubagentTreeRequest,
) -> Result<SubagentTreeView, String> {
    unimplemented!(
        "desktop_list_subagent_tree is implemented by panel #285 (subagent lineage view)"
    )
}

#[tauri::command]
pub(crate) async fn desktop_preview_interrupt_cascade(
    _state: State<'_, DesktopAppState>,
    _request: DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, String> {
    unimplemented!(
        "desktop_preview_interrupt_cascade is implemented by panel #286 (cascade cancel UX)"
    )
}

#[tauri::command]
pub(crate) async fn desktop_interrupt_request(
    _state: State<'_, DesktopAppState>,
    _request: DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    unimplemented!(
        "desktop_interrupt_request is implemented by panel #283 (interrupt button)"
    )
}
```

- [ ] **Step 2: Register the new submodule**

Open `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands.rs` and add `pub(crate) mod operations;` to the module list:

```rust
pub(crate) mod chat;
pub(crate) mod config;
pub(crate) mod lifecycle;
pub(crate) mod operations;
pub(crate) mod peers;
pub(crate) mod tasks;
```

- [ ] **Step 3: Register the four commands in `invoke_handler`**

Open `apps/desktop-tauri/src-tauri/src/bridge/mod.rs` and add the four new commands after the existing `tauri_commands::tasks::*` entries inside the `tauri::generate_handler![ ... ]` block:

```rust
tauri_commands::tasks::desktop_task_run,
tauri_commands::operations::desktop_operations_snapshot,
tauri_commands::operations::desktop_list_subagent_tree,
tauri_commands::operations::desktop_preview_interrupt_cascade,
tauri_commands::operations::desktop_interrupt_request
```

Note: existing entries end with `desktop_task_run` (no trailing comma). Add a trailing comma to that line so the four new lines append cleanly.

- [ ] **Step 4: Verify compile**

Run: `cargo check -p defra-agent-desktop-tauri`
Expected: success. Unused-import warnings in the new file are OK if any.

- [ ] **Step 5: Add a sanity test that confirms commands register**

The Tauri builder consumes the handler list at runtime. Add a focused compile-time test in a new file `apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations_tests.rs` that exercises the function signatures via type assertions:

```rust
//! Compile-only assertion that each operations command has the
//! parameter and return-type shape downstream panels rely on. These tests
//! never call the underlying functions (which would panic via
//! unimplemented!()); they only assert types at compile time.

#![cfg(test)]

use super::operations::{
    desktop_interrupt_request, desktop_list_subagent_tree, desktop_operations_snapshot,
    desktop_preview_interrupt_cascade,
};

#[allow(dead_code)]
fn _assert_command_signatures() {
    // These let bindings only check the function items exist and are
    // visible. The Tauri `#[tauri::command]` macro wraps the real function
    // in a synthetic one we don't reference here.
    let _ = desktop_operations_snapshot;
    let _ = desktop_list_subagent_tree;
    let _ = desktop_preview_interrupt_cascade;
    let _ = desktop_interrupt_request;
}
```

Register it inside `tauri_commands.rs`:

```rust
#[cfg(test)]
#[path = "tauri_commands/operations_tests.rs"]
mod operations_tests;
```

- [ ] **Step 6: Verify tests pass**

Run: `cargo test -p defra-agent-desktop-tauri`
Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/tauri_commands.rs \
        apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations.rs \
        apps/desktop-tauri/src-tauri/src/bridge/tauri_commands/operations_tests.rs \
        apps/desktop-tauri/src-tauri/src/bridge/mod.rs
git commit -m "$(cat <<'EOF'
bridge: stub Tauri commands for upcoming operator panels

Adds desktop_operations_snapshot, desktop_list_subagent_tree,
desktop_preview_interrupt_cascade, desktop_interrupt_request as
unimplemented!() stubs registered in invoke_handler. Panel PRs
#277/#283/#285/#286 replace each body with a real implementation;
until then no panel calls these so the panics stay unreachable.

Refs #302.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Add TypeScript type mirrors

**Files:**
- Create: `apps/desktop-tauri/src/lib/types/operations.ts`
- Modify: `apps/desktop-tauri/src/lib/types.ts`

- [ ] **Step 1: Create the operations types file**

Create `apps/desktop-tauri/src/lib/types/operations.ts`:

```typescript
// 1:1 mirror of bridge/types/views/operations.rs. Keep these in sync.
// Panels in their own PRs import from "../lib/types" (re-exported below).

export type DesktopOperationsSnapshot = {
  fetchedAt: string;
  agentDid?: string | null;
  liveness?: RuntimeLivenessView | null;
  livenessUnavailableReason?: string | null;
  backgroundedTools: BackgroundedToolView[];
  stuckDiagnostics: StuckWorkDiagnosticView[];
  lineage?: SubagentTreeView | null;
};

export type RuntimeLivenessView = {
  expiredProcessingCount: number;
  requests: ActiveRequestView[];
  activeToolCalls: ActiveToolCallView[];
  activeNativeExecutorsAvailable: boolean;
  activeNativeExecutors: NativeExecutorStatusView[];
};

export type ActiveRequestView = {
  requestId: string;
  claimedAt?: string | null;
  deadline?: string | null;
  deadlineExpired: boolean;
  deadlineAgeMs?: number | null;
  lastProgressAgeMs: number;
  subagentDepth: number;
  causedByParentRequestId?: string | null;
  causedByTriggerKind?: string | null;
};

export type ActiveToolCallView = {
  requestId: string;
  toolCallId: string;
  toolName: string;
  startedAt?: string | null;
  deadlineAt?: string | null;
  awaitMode?: string | null;
  runningAgeMs: number;
  deadlineExpired: boolean;
};

export type NativeExecutorStatusView = {
  id: number;
  pid: number;
  argv0: string;
  toolName?: string | null;
  startedAt: string;
  ageMs: number;
};

export type BackgroundedToolView = {
  requestId: string;
  toolCallId: string;
  toolName: string;
  lifecycleState?: string | null;
  status?: string | null;
  startedAt?: string | null;
  ageMs?: number | null;
  deadlineAt?: string | null;
  deadlineExpired: boolean;
  awaitMode?: string | null;
  cancelPolicy?: string | null;
  childRequestId?: string | null;
  nativeExecutor?: NativeExecutorStatusView | null;
};

export type StuckWorkDiagnosticView = {
  requestId: string;
  sessionId?: string | null;
  severity: "warning" | "critical";
  reason:
    | "expiredProcessing"
    | "expiredTool"
    | "stuckTool"
    | "pendingRemoteCancelAck";
  deadlineAgeMs?: number | null;
  lastProgressAgeMs?: number | null;
  toolCallId?: string | null;
  toolName?: string | null;
  stuckSince?: string | null;
};

export type SubagentTreeView = {
  rootRequestId: string;
  nodes: SubagentNodeView[];
  edges: SubagentEdgeView[];
  truncated: boolean;
};

export type SubagentNodeView = {
  requestId: string;
  sessionId?: string | null;
  agentDid?: string | null;
  behaviorId?: string | null;
  lifecycleState?: string | null;
  status?: string | null;
  subagentDepth?: number | null;
  causedByParentRequestId?: string | null;
  causedByParentToolCallId?: string | null;
};

export type SubagentEdgeView = {
  parentRequestId: string;
  childRequestId: string;
  parentToolCallId?: string | null;
  toolName?: string | null;
  awaitMode?: "foreground" | "background" | string | null;
  cancelPolicy?: "cascade" | "detach" | string | null;
  lifecycleState?: string | null;
};

export type CascadeCancelPreview = {
  rootRequestId: string;
  previewSignature: string;
  rootState?: string | null;
  willInterrupt: CascadeAffectedRequest[];
  willDetach: CascadeAffectedRequest[];
  alreadyTerminal: CascadeAffectedRequest[];
  unknownPolicy: CascadeAffectedRequest[];
};

export type CascadeAffectedRequest = {
  requestId: string;
  sessionId?: string | null;
  behaviorId?: string | null;
  lifecycleState?: string | null;
  parentRequestId?: string | null;
  parentToolCallId?: string | null;
  toolName?: string | null;
  awaitMode?: string | null;
  cancelPolicy?: string | null;
};

export type InterruptRequestResult = {
  requestId: string;
  accepted: boolean;
  interruptRequestedAt?: string | null;
  alreadyInterrupted: boolean;
  stalePreview: boolean;
  preview?: CascadeCancelPreview | null;
};

// Command request shapes (mirror bridge/types/requests/operations.rs).

export type DesktopOperationsSnapshotRequest = {
  agentDid?: string | null;
  rootRequestId?: string | null;
  includeTerminal?: boolean;
};

export type DesktopListSubagentTreeRequest = {
  rootRequestId: string;
  agentDid?: string | null;
  includeTerminal?: boolean;
  maxDepth?: number;
};

export type DesktopPreviewInterruptCascadeRequest = {
  requestId: string;
  agentDid?: string | null;
  includeTerminal?: boolean;
};

export type DesktopInterruptRequestRequest = {
  requestId: string;
  cause: "userCancelled";
  cascade: boolean;
  expectedPreviewSignature?: string | null;
};
```

- [ ] **Step 2: Re-export from `types.ts`**

Open `apps/desktop-tauri/src/lib/types.ts` and add at the end:

```typescript
export type {
  ActiveRequestView,
  ActiveToolCallView,
  BackgroundedToolView,
  CascadeAffectedRequest,
  CascadeCancelPreview,
  DesktopInterruptRequestRequest,
  DesktopListSubagentTreeRequest,
  DesktopOperationsSnapshot,
  DesktopOperationsSnapshotRequest,
  DesktopPreviewInterruptCascadeRequest,
  InterruptRequestResult,
  NativeExecutorStatusView,
  RuntimeLivenessView,
  StuckWorkDiagnosticView,
  SubagentEdgeView,
  SubagentNodeView,
  SubagentTreeView,
} from "./types/operations";
```

- [ ] **Step 3: Run typecheck**

Run: `cd apps/desktop-tauri && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Run frontend tests**

Run: `cd apps/desktop-tauri && pnpm test` (or `npm test` — check `package.json`; runner is `vitest run`)
Expected: existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop-tauri/src/lib/types/operations.ts \
        apps/desktop-tauri/src/lib/types.ts
git commit -m "$(cat <<'EOF'
desktop: TypeScript mirrors of operator-surfaces view types

1:1 mirror of bridge/types/views/operations.rs. Panel PRs
#276/#277/#278/#281/#283/#284/#285/#286/#288 import from "../lib/types".

Refs #302.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Add the `OperationsRail` React component

**Files:**
- Create: `apps/desktop-tauri/src/components/operations/operationsRailContext.ts`
- Create: `apps/desktop-tauri/src/components/operations/OperationsRailTabs.tsx`
- Create: `apps/desktop-tauri/src/components/operations/OperationsRailTabPanel.tsx`
- Create: `apps/desktop-tauri/src/components/operations/OperationsRail.tsx`
- Create: `apps/desktop-tauri/src/components/operations/index.ts`
- Test: `apps/desktop-tauri/tests/operations-rail.test.tsx`

Per spec line 1003 / 1021:
- `OperationsRail` is a tabbed container, not a vertical stack.
- At most one panel mounted at a time.
- Selecting "Open lineage" / "Open background" sets active tab.
- Initially zero tabs — populated by panel PRs via the registration API.

Design choice for the foundation: expose tab registration through a React context so panel PRs do not need to modify `OperationsRail.tsx` itself — they declare a tab descriptor and the rail picks it up via `useOperationsRail()`. This avoids churn in this file when each panel lands. The first panel PR will both register its tab descriptor and wire `setActiveTab` calls from its "Open X" buttons.

- [ ] **Step 1: Write the failing test first**

Create `apps/desktop-tauri/tests/operations-rail.test.tsx`:

```tsx
import { describe, expect, it } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";

import {
  OperationsRail,
  OperationsRailProvider,
  type OperationsRailTabDescriptor,
  useOperationsRail,
} from "../src/components/operations";

function HarnessOpenLineageButton() {
  const rail = useOperationsRail();
  return (
    <button onClick={() => rail.setActiveTab("lineage")}>
      open-lineage-button
    </button>
  );
}

function HarnessWithTabs({ tabs }: { tabs: OperationsRailTabDescriptor[] }) {
  return (
    <OperationsRailProvider tabs={tabs}>
      <HarnessOpenLineageButton />
      <OperationsRail />
    </OperationsRailProvider>
  );
}

describe("OperationsRail", () => {
  it("renders empty when no tabs are registered", () => {
    render(
      <OperationsRailProvider tabs={[]}>
        <OperationsRail />
      </OperationsRailProvider>,
    );
    expect(
      screen.queryByRole("tablist", { name: /operations/i }),
    ).not.toBeInTheDocument();
  });

  it("renders the registered tabs and mounts only the active one", () => {
    const tabs: OperationsRailTabDescriptor[] = [
      {
        id: "background",
        label: "Background",
        render: () => <div data-testid="background-panel">bg</div>,
      },
      {
        id: "lineage",
        label: "Lineage",
        render: () => <div data-testid="lineage-panel">lin</div>,
      },
    ];
    render(<HarnessWithTabs tabs={tabs} />);

    // First tab is active by default.
    expect(screen.getByTestId("background-panel")).toBeInTheDocument();
    expect(screen.queryByTestId("lineage-panel")).not.toBeInTheDocument();

    // setActiveTab via external caller switches the active tab.
    fireEvent.click(screen.getByText("open-lineage-button"));
    expect(screen.getByTestId("lineage-panel")).toBeInTheDocument();
    expect(screen.queryByTestId("background-panel")).not.toBeInTheDocument();
  });

  it("clicking a tab button activates that tab", () => {
    const tabs: OperationsRailTabDescriptor[] = [
      {
        id: "background",
        label: "Background",
        render: () => <div data-testid="background-panel">bg</div>,
      },
      {
        id: "lineage",
        label: "Lineage",
        render: () => <div data-testid="lineage-panel">lin</div>,
      },
    ];
    render(<HarnessWithTabs tabs={tabs} />);
    fireEvent.click(screen.getByRole("tab", { name: "Lineage" }));
    expect(screen.getByTestId("lineage-panel")).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run the test; confirm it fails because the module doesn't exist**

Run: `cd apps/desktop-tauri && npx vitest run tests/operations-rail.test.tsx`
Expected: FAIL with "Cannot find module '../src/components/operations'".

- [ ] **Step 3: Create the context module**

Create `apps/desktop-tauri/src/components/operations/operationsRailContext.ts`:

```typescript
import {
  createContext,
  useContext,
  type ReactNode,
} from "react";

export type OperationsRailTabId = string;

export type OperationsRailTabDescriptor = {
  id: OperationsRailTabId;
  label: string;
  /** Optional badge text (e.g. count). */
  badge?: string | null;
  render: () => ReactNode;
};

export type OperationsRailContextValue = {
  tabs: OperationsRailTabDescriptor[];
  activeTabId: OperationsRailTabId | null;
  setActiveTab: (id: OperationsRailTabId) => void;
};

export const OperationsRailContext =
  createContext<OperationsRailContextValue | null>(null);

export function useOperationsRail(): OperationsRailContextValue {
  const value = useContext(OperationsRailContext);
  if (!value) {
    throw new Error(
      "useOperationsRail must be used inside <OperationsRailProvider>",
    );
  }
  return value;
}
```

- [ ] **Step 4: Create the Tabs view component**

Create `apps/desktop-tauri/src/components/operations/OperationsRailTabs.tsx`:

```tsx
import type { OperationsRailContextValue } from "./operationsRailContext";

export type OperationsRailTabsProps = Pick<
  OperationsRailContextValue,
  "tabs" | "activeTabId" | "setActiveTab"
>;

export function OperationsRailTabs({
  tabs,
  activeTabId,
  setActiveTab,
}: OperationsRailTabsProps) {
  if (tabs.length === 0) {
    return null;
  }
  return (
    <div role="tablist" aria-label="Operations" className="operations-rail-tabs">
      {tabs.map((tab) => {
        const selected = tab.id === activeTabId;
        return (
          <button
            key={tab.id}
            type="button"
            role="tab"
            aria-selected={selected}
            aria-controls={`operations-rail-panel-${tab.id}`}
            id={`operations-rail-tab-${tab.id}`}
            className={selected ? "is-active" : undefined}
            onClick={() => setActiveTab(tab.id)}
          >
            <span className="operations-rail-tab-label">{tab.label}</span>
            {tab.badge ? (
              <span className="operations-rail-tab-badge">{tab.badge}</span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}
```

- [ ] **Step 5: Create the TabPanel view component**

Create `apps/desktop-tauri/src/components/operations/OperationsRailTabPanel.tsx`:

```tsx
import type { OperationsRailTabDescriptor } from "./operationsRailContext";

export type OperationsRailTabPanelProps = {
  tab: OperationsRailTabDescriptor;
};

export function OperationsRailTabPanel({ tab }: OperationsRailTabPanelProps) {
  return (
    <div
      role="tabpanel"
      id={`operations-rail-panel-${tab.id}`}
      aria-labelledby={`operations-rail-tab-${tab.id}`}
      className="operations-rail-tab-panel"
    >
      {tab.render()}
    </div>
  );
}
```

- [ ] **Step 6: Create the top-level `OperationsRail` + provider**

Create `apps/desktop-tauri/src/components/operations/OperationsRail.tsx`:

```tsx
import {
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import {
  OperationsRailContext,
  type OperationsRailContextValue,
  type OperationsRailTabDescriptor,
  type OperationsRailTabId,
} from "./operationsRailContext";
import { OperationsRailTabPanel } from "./OperationsRailTabPanel";
import { OperationsRailTabs } from "./OperationsRailTabs";

export type OperationsRailProviderProps = {
  tabs: OperationsRailTabDescriptor[];
  /** Initial active tab id. Defaults to the first registered tab. */
  initialActiveTabId?: OperationsRailTabId | null;
  children: ReactNode;
};

export function OperationsRailProvider({
  tabs,
  initialActiveTabId,
  children,
}: OperationsRailProviderProps) {
  const [activeTabId, setActiveTabId] = useState<OperationsRailTabId | null>(
    initialActiveTabId ?? tabs[0]?.id ?? null,
  );

  const setActiveTab = useCallback((id: OperationsRailTabId) => {
    setActiveTabId(id);
  }, []);

  const value: OperationsRailContextValue = useMemo(
    () => ({
      tabs,
      activeTabId:
        activeTabId !== null && tabs.some((tab) => tab.id === activeTabId)
          ? activeTabId
          : (tabs[0]?.id ?? null),
      setActiveTab,
    }),
    [tabs, activeTabId, setActiveTab],
  );

  return (
    <OperationsRailContext.Provider value={value}>
      {children}
    </OperationsRailContext.Provider>
  );
}

export function OperationsRail() {
  const value = useContext(OperationsRailContext);
  if (!value || value.tabs.length === 0) {
    // Either no provider (foundation default) or no registered tabs:
    // render nothing so the chat shell layout doesn't get a phantom column.
    return null;
  }
  const activeTab =
    value.tabs.find((tab) => tab.id === value.activeTabId) ?? value.tabs[0];
  return (
    <aside className="operations-rail" aria-label="Operations">
      <OperationsRailTabs
        tabs={value.tabs}
        activeTabId={value.activeTabId}
        setActiveTab={value.setActiveTab}
      />
      <OperationsRailTabPanel tab={activeTab} />
    </aside>
  );
}
```

- [ ] **Step 7: Create the barrel export**

Create `apps/desktop-tauri/src/components/operations/index.ts`:

```typescript
export {
  OperationsRail,
  OperationsRailProvider,
  type OperationsRailProviderProps,
} from "./OperationsRail";
export {
  type OperationsRailTabDescriptor,
  type OperationsRailTabId,
  type OperationsRailContextValue,
  useOperationsRail,
} from "./operationsRailContext";
export { OperationsRailTabs } from "./OperationsRailTabs";
export { OperationsRailTabPanel } from "./OperationsRailTabPanel";
```

- [ ] **Step 8: Run the tests; confirm they pass**

Run: `cd apps/desktop-tauri && npx vitest run tests/operations-rail.test.tsx`
Expected: all three tests pass.

- [ ] **Step 9: Run the full frontend test suite to check for regressions**

Run: `cd apps/desktop-tauri && pnpm test` (or `npm test`)
Expected: all tests pass.

- [ ] **Step 10: Commit**

```bash
git add apps/desktop-tauri/src/components/operations/ \
        apps/desktop-tauri/tests/operations-rail.test.tsx
git commit -m "$(cat <<'EOF'
desktop: add OperationsRail tabbed container

Empty tabbed shell per design spec component decomposition (line 972).
Initially renders nothing because no panels register tabs; panel PRs
will register descriptors via OperationsRailProvider props and route
"Open X" buttons through useOperationsRail().setActiveTab.

Refs #302.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Mount `OperationsRail` in the chat shell

**Files:**
- Modify: `apps/desktop-tauri/src/components/ChatWorkspace.tsx`

Per spec line 1003, the rail mounts as a sibling of `div.chat-main` inside `section.chat-workspace`. Initially we wrap the entire workspace in an `OperationsRailProvider` with zero tabs so the rail renders nothing and existing layout is unaffected.

- [ ] **Step 1: Inspect current `ChatWorkspace.tsx` layout**

```bash
sed -n '80,120p' apps/desktop-tauri/src/components/ChatWorkspace.tsx
```

Expected: a `<section className="chat-workspace">` containing `<div className="chat-main">` with `ChatTranscriptPanel` and `ChatComposer`.

- [ ] **Step 2: Edit `ChatWorkspace.tsx`**

Wrap the `<section className="chat-workspace">` in `<OperationsRailProvider tabs={[]}>` and mount `<OperationsRail />` after `<div className="chat-main">` per the spec diagram (line 1003).

Add the import at the top:

```tsx
import {
  OperationsRail,
  OperationsRailProvider,
} from "./operations";
```

Modify the JSX returned by `ActiveChatWorkspace` to:

```tsx
return (
  <OperationsRailProvider tabs={[]}>
    <ChatHeader
      behaviorLabel={behaviorLabel}
      runtimeHealth={runtimeHealth}
      selectedConversationTitle={selectedConversationTitle}
      selectedSessionId={selectedSessionId}
      onRenameConversationTitle={onRenameConversationTitle}
    />

    <section className="chat-workspace">
      <div className="chat-main">
        <ChatTranscriptPanel
          selectedSessionId={selectedSessionId}
          session={session}
        />

        <ChatComposer
          approxSerializedBytes={approxSerializedBytes}
          behaviorLabel={behaviorLabel}
          canSend={canSend}
          configuredPeerCount={configuredPeerCount}
          dialedPeerCount={dialedPeerCount}
          draft={draft}
          rowCount={rowCount}
          sendHint={sendHint}
          sending={sending}
          turnState={session?.turnState ?? null}
          onDraftChange={onDraftChange}
          onSend={onSend}
        />
      </div>
      <OperationsRail />
    </section>
  </OperationsRailProvider>
);
```

Replace the `<>` fragment wrapper accordingly (the provider replaces it).

- [ ] **Step 3: Verify typecheck**

Run: `cd apps/desktop-tauri && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 4: Run the full frontend test suite**

Run: `cd apps/desktop-tauri && pnpm test`
Expected: existing tests including `chat-transcript-panel.test.tsx` and any chat-shell tests still pass. The rail renders nothing because tabs is `[]`, so layout snapshots / DOM queries that don't mention "operations" should be unaffected.

If a snapshot test breaks due to the added `OperationsRailProvider` wrapper, do not blindly update snapshots — re-examine whether the wrapper introduced a real DOM change. Since `OperationsRail` returns `null` when no tabs are registered and `OperationsRailProvider` only adds a context (no DOM node), the DOM should be identical.

- [ ] **Step 5: Smoke-build the production bundle to catch Vite-only issues**

Run: `cd apps/desktop-tauri && npm run build` (or `pnpm build`)
Expected: tsc + vite build pass.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop-tauri/src/components/ChatWorkspace.tsx
git commit -m "$(cat <<'EOF'
desktop: wire OperationsRail mount point into chat shell

Mounts OperationsRail as a sibling of div.chat-main per design spec
line 1003. Provider declares tabs=[] so the rail renders null; panel
PRs (#276/#277/#278/#281/#283/#284/#285/#286/#288) add their tab
descriptors and the rail becomes visible.

Refs #302.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Full verification

This is the gate before opening the PR.

- [ ] **Step 1: Lean proofs (sanity — should be unchanged)**

Run: `cd crates/defra-agent/proofs && lake build`
Expected: success, no diff in proof status.

- [ ] **Step 2: Rust workspace check**

Run: `cargo check`
Expected: success across the workspace.

- [ ] **Step 3: Rust crate-specific test**

Run: `cargo test -p defra-agent-desktop-tauri`
Expected: success including the new `operations_signature` test suite.

- [ ] **Step 4: Frontend tests**

Run: `cd apps/desktop-tauri && pnpm test`
Expected: success including the new `operations-rail.test.tsx`.

- [ ] **Step 5: Frontend production build (catches type-only + Vite issues)**

Run: `cd apps/desktop-tauri && npm run build`
Expected: tsc passes, Vite bundles, no errors.

- [ ] **Step 6: Visual sanity — desktop dev server**

Run: `cd apps/desktop-tauri && npm run dev` (background)
Open the app. Confirm:
- App shell still loads
- Chat workspace still renders, transcript still works
- No new "Operations" panel visible (zero tabs registered)
- No console errors

Kill the dev server when satisfied.

- [ ] **Step 7: Stub command unreachable verification**

Search the codebase to confirm none of the four stub commands are called from React:

```bash
git grep -nE "desktop_operations_snapshot|desktop_list_subagent_tree|desktop_preview_interrupt_cascade|desktop_interrupt_request" -- apps/desktop-tauri/src/
```

Expected: zero hits. (TS file with the types is fine to match, but no `invoke("desktop_*")` call.)

- [ ] **Step 8: Push branch and open PR**

```bash
git push -u origin design/issue-302-ui-foundation
gh pr create --title "Desktop UI foundation: OperationsRail + bridge contracts (#302)" --body "$(cat <<'EOF'
## Summary
- Adds the shared scaffolding nine panel PRs depend on: empty tabbed `OperationsRail`, Rust view types, Tauri command stubs, and BLAKE3 signature/emit-floor helpers.
- Strictly additive: existing chat shell, types, and commands are untouched. The rail renders nothing until panel PRs register tab descriptors.

## Scope
- Task 1: `OperationsRail` tabbed container (`apps/desktop-tauri/src/components/operations/`).
- Task 2: Bridge snapshot contract types (`DesktopOperationsSnapshot`, `RuntimeLivenessView`, `CascadeCancelPreview`, `InterruptRequestResult`, etc.) — additive, no schema rewrite.
- Task 3: Four `unimplemented!()` Tauri command stubs (`desktop_operations_snapshot`, `desktop_list_subagent_tree`, `desktop_preview_interrupt_cascade`, `desktop_interrupt_request`) registered in `invoke_handler`. No panel calls them yet so panics are unreachable.
- Task 4: BLAKE3 preview + liveness signatures plus the `LivenessEmitFloor` (250ms min, 2s coalesce ceiling) as pure, fully-tested helpers. Wiring into a live watcher with an HTTP `/status` probe is panel #277's responsibility.
- Task 5: `OperationsRail` mount point in `ChatWorkspace`.

## Out of scope
- All nine panel implementations: #276, #277, #278, #281, #283, #284, #285, #286, #288.

## Closes
Closes #302.

## Test plan
- [x] `cargo check` workspace-wide
- [x] `cargo test -p defra-agent-desktop-tauri` (9 new signature + emit-floor tests pass)
- [x] `pnpm test` in `apps/desktop-tauri/` (3 new OperationsRail tests pass; existing chat tests unchanged)
- [x] `npm run build` in `apps/desktop-tauri/` (tsc + Vite)
- [x] `lake build` in `crates/defra-agent/proofs/` (unchanged — no Lean change)
- [x] Desktop dev server launches and existing chat shell renders without regression
- [x] No call sites for the four stub commands exist in `apps/desktop-tauri/src/`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 9: Return the PR URL to the user**

---

## Risks and Open Questions

- **Liveness watcher integration not yet wired.** The PROMPT's Task 4 phrasing — "Implement in the bridge watcher that produces snapshots" — implies the floor should run inside an actual Tokio task. We deliver the floor as a pure, tested helper rather than starting a watcher with no data source. Rationale: there is no `RuntimeLivenessSnapshot` source available to `ClientCore` today; the production source is HTTP `/status` on each peer, which panel #277 wires up as part of `desktop_operations_snapshot`. Starting a watcher now that polls nothing costs runtime cycles for no observable benefit. If reviewers prefer the watcher to exist with an empty probe so panel #277 only swaps the probe, that is a one-task follow-up that does not change the public surface.
- **`unimplemented!()` vs typed error return.** PROMPT.md asks for `unimplemented!()`. That panics inside Tauri's invoke handler and surfaces as an opaque IPC error. If the rail ever accidentally calls one, the desktop process panics; mitigated by Step 7 of Task 9 confirming no call sites. Alternative is `Err("unimplemented; see #277")` but PROMPT.md is explicit so we follow it.
- **`previewSignature` separator collision.** Spec line 720 asserts that the `0x1D`/`0x1E`/`0x1F` control bytes do not appear in DefraDB document ids or RFC3339 timestamps. If a panel later adds free-form text into the signature (tool names, error strings), revisit; for the rows shipped in Task 4 this constraint is intact.
