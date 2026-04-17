# Phase A1 Implementation Plan: Split `src/admission/mod.rs`

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split `crates/defra-agent/src/admission/mod.rs` (1456 lines, inline tests) into a dispatch shell plus six focused submodules and a sibling tests file, with zero behavior change.

**Architecture:** Break the monolith along responsibility seams: config types, admission-scoped client/context, registry, controller, permit, persistence helpers. Keep all items at their current visibility (`pub(crate)` for externals, private otherwise) via re-exports from the new `mod.rs` shell. Tests land in a single `src/admission/tests.rs` sibling file. Cross-module visibility bumps are limited to `pub(super)` where sibling modules need access.

**Tech Stack:** Rust 2021, Cargo workspace, `tokio`, `anyhow`. Tests use `defra_node::EmbeddedNode` (integration-style).

**Spec reference:** `docs/issues/2026-04-17-agent-readability-refactor-plan.md` — Phase A1.

---

## Import-path evolution during the refactor

The file contents shown in each task reflect **final-state imports** (references to sibling modules like `super::persistence::persist_*`). During intermediate tasks, a sibling may still be an empty placeholder. If `cargo check` reports "unresolved import" on a `super::<sibling>::<item>` path for an item that has not moved yet, rewrite the import as `super::<item>` — this resolves to `mod.rs`, where the item still lives, and works because child modules can access their parent's private items.

When a later task moves that item into the sibling, update the import back to the sibling path. The commands in each task's compile-and-test step will catch any missed updates.

Example: during Task 5 (move controller), `use super::persistence::persist_call_started;` won't resolve because persistence.rs is still a placeholder. Use `use super::persist_call_started;` instead. Task 8 (move persistence) then updates this to `use super::persistence::persist_call_started;` as part of Step 8.2.

Applies specifically to these sibling paths until their owning task runs:

| Sibling import                           | Populated by | Until then use          |
|------------------------------------------|--------------|-------------------------|
| `super::config::...`                     | Task 3       | `super::...`            |
| `super::client::...`                     | Task 4       | `super::...`            |
| `super::controller::...`                 | Task 5       | `super::...`            |
| `super::permit::AdmissionPermit`         | Task 6       | `super::AdmissionPermit`|
| `super::registry::AdmissionRegistryInner`| Task 7       | `super::AdmissionRegistryInner` |
| `super::persistence::...`                | Task 8       | `super::...`            |

---

## Prerequisites

- Parent branch: `main` at commit `db094e0` or later.
- Worktree: created via `superpowers:using-git-worktrees` at `../defra-agent-agent-a1-admission` (branch `refactor/admission-split`).
- Lean-spec boundary applies: this phase touches `admission/`, so `tests/state_machine_conformance.rs`, `tests/lifecycle_regression.rs`, and `cargo test -p defra-agent` must all stay green unchanged.

---

## File Structure (end state)

```
crates/defra-agent/src/admission/
├── mod.rs              # shell: module decls + pub(crate) re-exports (~25 lines)
├── stream_guard.rs     # unchanged (242 lines)
├── config.rs           # BackendAdmissionConfig + helper
├── client.rs           # AdmittedCompletionClient/Model + CallKind + context + scope
├── controller.rs       # BackendAdmissionController + QueuedCallGuard + records
├── permit.rs           # AdmissionPermit + PermitTerminal
├── registry.rs         # AdmissionRegistry + inner state + reconcile
├── persistence.rs      # persist_* + add_call_mutation + helpers
└── tests.rs            # 4 async integration tests + shared helpers
```

External callers (unchanged imports):

- `crate::admission::AdmissionRegistry` — `completion_factory.rs`, `scheduler.rs`, `scheduler/tests.rs`, `agent/runtime.rs`, `agent/reconcile.rs`, `agent/reconcile/tests.rs`
- `crate::admission::AdmittedCompletionClient` — `completion_factory.rs`
- `crate::admission::BackendAdmissionConfig` — `runtime_snapshot.rs`, `agent/runtime.rs`, `agent/builder.rs`, `scheduler/tests.rs`
- `crate::admission::backend_admission_configs_from_backends` — `agent/document_view.rs`
- `crate::admission::{AdmissionCallContext, CallKind}` — `scheduler/execution.rs`, `agent/daemon/request.rs`, `agent/daemon/inference.rs`
- `crate::admission::{scope_request, scope_call}` — `scheduler/execution.rs`, `agent/daemon/request.rs`, `agent/daemon/inference.rs`

Re-exports in the new `mod.rs` must cover every item in this list.

---

## Task 1: Establish baseline

**Files:** none modified.

- [ ] **Step 1.1: Verify worktree + branch**

Run:
```bash
git rev-parse --abbrev-ref HEAD
git status --short
```
Expected: branch `refactor/admission-split`, clean working tree.

- [ ] **Step 1.2: Record baseline admission test list**

Run:
```bash
cargo test -p defra-agent admission::tests --no-run 2>&1 | tail -5
cargo test -p defra-agent admission::tests -- --list 2>&1 | grep ': test$'
```
Expected output (record for later comparison):
```
admission::tests::compaction_calls_share_backend_capacity_with_inference_calls: test
admission::tests::max_queue_depth_zero_allows_immediate_permit_and_rejects_saturated_backend: test
admission::tests::queued_calls_start_in_tokio_registration_order_after_permit_release: test
admission::tests::scoped_scheduled_calls_are_persisted_with_scheduled_kind: test
```

- [ ] **Step 1.3: Confirm green baseline**

Run:
```bash
cargo test -p defra-agent admission::tests
```
Expected: `test result: ok. 4 passed; 0 failed`.

Also run:
```bash
cargo test -p defra-agent --test state_machine_conformance
cargo test -p defra-agent --test lifecycle_regression
```
Expected: all green.

If any of the above fails on a clean baseline, STOP — fix or report before continuing; this plan is not the source of the failure.

- [ ] **Step 1.4: Commit a checkpoint marker (empty)**

```bash
git commit --allow-empty -m "checkpoint: baseline green before admission split"
```

---

## Task 2: Scaffold submodule files

Create empty submodules so each later task can move code into a file that already exists and compiles.

**Files:**
- Create: `crates/defra-agent/src/admission/config.rs`
- Create: `crates/defra-agent/src/admission/client.rs`
- Create: `crates/defra-agent/src/admission/controller.rs`
- Create: `crates/defra-agent/src/admission/permit.rs`
- Create: `crates/defra-agent/src/admission/registry.rs`
- Create: `crates/defra-agent/src/admission/persistence.rs`
- Modify: `crates/defra-agent/src/admission/mod.rs:1` — add module declarations

- [ ] **Step 2.1: Create each submodule file with a single module-level placeholder**

Each file starts as:
```rust
// Temporary placeholder; contents move from admission/mod.rs in subsequent tasks.
```

Create all six files with that one line.

- [ ] **Step 2.2: Add `mod` declarations to `admission/mod.rs`**

Edit `crates/defra-agent/src/admission/mod.rs` line 1 region so the top becomes:

```rust
pub(crate) mod stream_guard;
mod config;
mod client;
mod controller;
mod permit;
mod registry;
mod persistence;

use std::collections::{HashMap, HashSet};
use std::future::Future;
// ... rest of the existing imports unchanged
```

Keep every other line of the file unchanged.

- [ ] **Step 2.3: Compile**

Run:
```bash
cargo check -p defra-agent
```
Expected: clean build (warnings about unused mods acceptable if any; otherwise silent).

- [ ] **Step 2.4: Commit**

```bash
git add crates/defra-agent/src/admission/
git commit -m "Scaffold admission submodule files"
```

---

## Task 3: Move `BackendAdmissionConfig` to `config.rs`

Smallest self-contained chunk; no admission internal deps.

**Files:**
- Modify: `crates/defra-agent/src/admission/mod.rs:22-75` — remove content, add re-export
- Modify: `crates/defra-agent/src/admission/config.rs` — receive content

- [ ] **Step 3.1: Write `config.rs` contents**

Replace the placeholder with:

```rust
use std::collections::HashMap;

use anyhow::Result;

use crate::backend_registry::InferenceBackend;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BackendAdmissionConfig {
    pub(crate) backend_id: String,
    pub(crate) max_concurrent: usize,
    pub(crate) max_queue_depth: usize,
    pub(crate) enabled: bool,
    pub(crate) probe_status: String,
    pub(crate) config_fingerprint: String,
}

impl BackendAdmissionConfig {
    pub(crate) fn from_backend(backend: &InferenceBackend) -> Result<Self> {
        if backend.max_concurrent < 1 {
            anyhow::bail!(
                "backend {} has invalid max_concurrent {}; expected >= 1",
                backend.backend_id,
                backend.max_concurrent
            );
        }
        if backend.max_queue_depth < 0 {
            anyhow::bail!(
                "backend {} has invalid max_queue_depth {}; expected >= 0",
                backend.backend_id,
                backend.max_queue_depth
            );
        }

        Ok(Self {
            backend_id: backend.backend_id.clone(),
            max_concurrent: backend.max_concurrent as usize,
            max_queue_depth: backend.max_queue_depth as usize,
            enabled: backend.enabled,
            probe_status: backend.probe_status.clone(),
            config_fingerprint: format!("{backend:?}"),
        })
    }

    pub(crate) fn is_available(&self) -> bool {
        self.enabled && self.probe_status == "healthy"
    }
}

pub(crate) fn backend_admission_configs_from_backends<'a>(
    backends: impl IntoIterator<Item = &'a InferenceBackend>,
) -> Result<HashMap<String, BackendAdmissionConfig>> {
    let mut configs = HashMap::new();
    for backend in backends {
        configs.insert(
            backend.backend_id.clone(),
            BackendAdmissionConfig::from_backend(backend)?,
        );
    }
    Ok(configs)
}
```

- [ ] **Step 3.2: Remove the moved lines from `admission/mod.rs`**

Delete lines that currently hold `BackendAdmissionConfig`, its impl, and `backend_admission_configs_from_backends` (originally lines 22-75 of `admission/mod.rs`, identifiable by the `#[derive(Clone, Debug, PartialEq, Eq)]\npub(crate) struct BackendAdmissionConfig {` block through the closing `Ok(configs)\n}` of the helper).

- [ ] **Step 3.3: Add re-export in `admission/mod.rs`**

Add, after the `mod persistence;` line and before the existing `use std::...` imports:

```rust
pub(crate) use config::{backend_admission_configs_from_backends, BackendAdmissionConfig};
```

- [ ] **Step 3.4: Compile and test**

Run:
```bash
cargo check -p defra-agent
cargo test -p defra-agent admission::
```
Expected: clean build; all 4 admission tests pass.

- [ ] **Step 3.5: Commit**

```bash
git add crates/defra-agent/src/admission/
git commit -m "Extract BackendAdmissionConfig into admission/config.rs"
```

---

## Task 4: Move client / context / scope helpers to `client.rs`

Moves `AdmittedCompletionClient`, `AdmittedCompletionModel`, `CallKind`, `AdmissionCallContext`, the task-local context, and `scope_request`/`scope_call`/`current_context`. `CallKind::as_str` becomes `pub(super)` so `persistence.rs` can format it.

**Files:**
- Modify: `crates/defra-agent/src/admission/mod.rs` — remove lines 77-241, add re-export
- Modify: `crates/defra-agent/src/admission/client.rs` — receive content

Note: `AdmissionCallContext::next_call` returns `PendingCallMetadata` which will live in `controller.rs` (Task 5). At the end of this task, `PendingCallMetadata` still lives in `mod.rs`, so `client.rs` imports it via `use super::PendingCallMetadata;`. After Task 5 we adjust the path.

- [ ] **Step 4.1: Write `client.rs` contents**

Replace the placeholder with:

```rust
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
};
use rig::streaming::StreamingCompletionResponse;

use super::stream_guard::hold_stream_guard;
use super::{AdmissionRegistry, PendingCallMetadata};
use crate::watcher::AgentRequest;

#[derive(Clone)]
pub(crate) struct AdmittedCompletionClient<C> {
    inner: C,
    admission: AdmissionRegistry,
}

impl<C> AdmittedCompletionClient<C> {
    pub(crate) fn new(inner: C, admission: AdmissionRegistry) -> Self {
        Self { inner, admission }
    }
}

impl<C> CompletionClient for AdmittedCompletionClient<C>
where
    C: CompletionClient,
    C::CompletionModel: 'static,
    <C::CompletionModel as CompletionModel>::Response: 'static,
    <C::CompletionModel as CompletionModel>::StreamingResponse: 'static,
{
    type CompletionModel = AdmittedCompletionModel<C::CompletionModel>;
}

#[derive(Clone)]
pub(crate) struct AdmittedCompletionModel<M> {
    inner: M,
    admission: AdmissionRegistry,
}

impl<M> CompletionModel for AdmittedCompletionModel<M>
where
    M: CompletionModel + 'static,
    M::Response: 'static,
    M::StreamingResponse: 'static,
{
    type Response = M::Response;
    type StreamingResponse = M::StreamingResponse;
    type Client = AdmittedCompletionClient<M::Client>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self {
            inner: M::make(&client.inner, model),
            admission: client.admission.clone(),
        }
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let mut permit = self.admission.acquire_current_call().await?;
        match self.inner.completion(request).await {
            Ok(response) => {
                permit.finish_success(Some(response.usage)).await;
                Ok(response)
            }
            Err(error) => {
                permit.finish_failure(&error.to_string()).await;
                Err(error)
            }
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let mut permit = self.admission.acquire_current_call().await?;
        match self.inner.stream(request).await {
            Ok(stream) => Ok(hold_stream_guard(stream, permit)),
            Err(error) => {
                permit.finish_failure(&error.to_string()).await;
                Err(error)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallKind {
    Inference,
    Compaction,
    Scheduled,
}

impl CallKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Inference => "inference",
            Self::Compaction => "compaction",
            Self::Scheduled => "scheduled",
        }
    }
}

#[derive(Clone)]
pub(crate) struct AdmissionCallContext {
    pub(super) request_id: String,
    pub(super) backend_id: String,
    pub(super) behavior_id: String,
    pub(super) agent_did: String,
    pub(super) call_kind: CallKind,
    pub(super) attempt: i64,
    pub(super) call_seq: Arc<AtomicU64>,
}

impl AdmissionCallContext {
    pub(crate) fn for_request(
        request: &AgentRequest,
        behavior_id: impl Into<String>,
        backend_id: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request.request_id.clone(),
            backend_id: backend_id.into(),
            behavior_id: behavior_id.into(),
            agent_did: request.agent_did.clone(),
            call_kind: CallKind::Inference,
            attempt: 1,
            call_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(super) fn next_call(&self, runtime_instance_id: &str) -> PendingCallMetadata {
        let call_seq = self.call_seq.fetch_add(1, Ordering::SeqCst) + 1;
        PendingCallMetadata {
            call_id: uuid::Uuid::new_v4().to_string(),
            runtime_instance_id: runtime_instance_id.to_string(),
            request_id: self.request_id.clone(),
            call_seq,
            backend_id: self.backend_id.clone(),
            behavior_id: self.behavior_id.clone(),
            agent_did: self.agent_did.clone(),
            call_kind: self.call_kind,
            attempt: self.attempt,
        }
    }
}

tokio::task_local! {
    static ADMISSION_CALL_CONTEXT: AdmissionCallContext;
}

pub(crate) async fn scope_request<T>(
    context: AdmissionCallContext,
    future: impl Future<Output = T>,
) -> T {
    ADMISSION_CALL_CONTEXT.scope(context, future).await
}

pub(crate) async fn scope_call<T>(
    call_kind: CallKind,
    attempt: i64,
    future: impl Future<Output = T>,
) -> T {
    let mut context = current_context().expect("admission call scope requires request context");
    context.call_kind = call_kind;
    context.attempt = attempt;
    ADMISSION_CALL_CONTEXT.scope(context, future).await
}

pub(super) fn current_context() -> Result<AdmissionCallContext, CompletionError> {
    ADMISSION_CALL_CONTEXT
        .try_with(Clone::clone)
        .map_err(|_| CompletionError::ProviderError("missing inference admission context".into()))
}
```

Note: `AdmissionCallContext` fields become `pub(super)` because `AdmissionRegistry::acquire_for_test` (still in `mod.rs` during this task, moves to `registry.rs` in Task 7) constructs one directly. Original was private; the visibility bump is limited to sibling modules.

- [ ] **Step 4.2: Remove moved lines from `mod.rs`**

Delete lines 77-241 (the `AdmittedCompletionClient` block through the `current_context` helper). Also delete the now-orphan imports in `mod.rs` that were only used by the moved code: `use rig::client::CompletionClient;`, the `use rig::completion::{...}` line, `use rig::streaming::StreamingCompletionResponse;`, `use self::stream_guard::{hold_stream_guard, StreamGuardLifecycle};` — but **keep** `StreamGuardLifecycle` import if still referenced by `AdmissionPermit` (it is; leave the import intact by splitting: keep `use self::stream_guard::StreamGuardLifecycle;`). Also drop unused atomic imports in `mod.rs` if any remain.

Verify by running `cargo check -p defra-agent` after the removal; the compiler will flag unused imports.

- [ ] **Step 4.3: Add client re-export to `mod.rs`**

Add below the `pub(crate) use config::...` line:

```rust
pub(crate) use client::{
    scope_call, scope_request, AdmissionCallContext, AdmittedCompletionClient, CallKind,
};
```

- [ ] **Step 4.4: Compile and test**

Run:
```bash
cargo check -p defra-agent
cargo test -p defra-agent admission::
```
Expected: clean build; 4 admission tests pass.

Note: tests in `admission/mod.rs` reference `CallKind`, `AdmissionCallContext`, `scope_request`, `scope_call` directly. Because tests live inside `mod tests` in `mod.rs` with `use super::*`, and these items are now re-exported via `pub(crate) use client::{...}` at the `mod.rs` level, `use super::*` will still pick them up.

- [ ] **Step 4.5: Commit**

```bash
git add crates/defra-agent/src/admission/
git commit -m "Extract admission client and call context into admission/client.rs"
```

---

## Task 5: Move controller + records to `controller.rs`

Moves `BackendAdmissionController`, `QueuedCallGuard`, `PendingCallMetadata`, `InferenceCallRecord`, and their impls. Makes record fields `pub(super)` so `persistence.rs` can read them. After this task, `client.rs`'s `use super::PendingCallMetadata` still resolves because `mod.rs` re-exports controller items internally.

**Files:**
- Modify: `crates/defra-agent/src/admission/mod.rs` — remove lines 505-798
- Modify: `crates/defra-agent/src/admission/controller.rs` — receive content

- [ ] **Step 5.1: Write `controller.rs` contents**

Replace the placeholder with:

```rust
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use defra_node::EmbeddedNode;
use rig::completion::CompletionError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::client::CallKind;
use super::config::BackendAdmissionConfig;
use super::permit::AdmissionPermit;
use super::persistence::{
    persist_call_started, persist_existing_call_running, persist_existing_call_terminal,
    persist_terminal_call,
};
use super::registry::AdmissionRegistryInner;
use super::spawn_persistence;

pub(super) struct BackendAdmissionController {
    pub(super) backend_id: String,
    pub(super) generation: u64,
    pub(super) config: BackendAdmissionConfig,
    semaphore: Arc<Semaphore>,
    waiters: AtomicUsize,
    running: AtomicUsize,
    closed: AtomicBool,
    registry: Weak<AdmissionRegistryInner>,
}

impl BackendAdmissionController {
    pub(super) fn new(
        generation: u64,
        config: BackendAdmissionConfig,
        registry: Weak<AdmissionRegistryInner>,
    ) -> Arc<Self> {
        let max_concurrent = config.max_concurrent;
        Arc::new(Self {
            backend_id: config.backend_id.clone(),
            generation,
            config,
            // BackendAdmissionConfig validation guarantees this is >= 1.
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            waiters: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            registry,
        })
    }

    pub(super) fn matches(&self, config: &BackendAdmissionConfig) -> bool {
        self.config == *config && !self.is_closed()
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.semaphore.close();
    }

    pub(super) fn is_drained(&self) -> bool {
        self.running.load(Ordering::SeqCst) == 0
    }

    pub(super) async fn acquire(
        self: Arc<Self>,
        node: Arc<EmbeddedNode>,
        pending: PendingCallMetadata,
    ) -> Result<AdmissionPermit, CompletionError> {
        if self.is_closed() {
            let call = self.call_record(pending, 0);
            if let Err(error) =
                persist_terminal_call(node, call, "cancelled", Some("BackendGone"), None).await
            {
                tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist closed-controller inference call");
            }
            return Err(CompletionError::ProviderError(
                "BackendGone: backend admission controller is draining".into(),
            ));
        }

        match self.semaphore.clone().try_acquire_owned() {
            Ok(permit) => {
                let call = self.call_record(pending, 0);
                return self.start_permit(node, permit, call).await;
            }
            Err(tokio::sync::TryAcquireError::Closed) => {
                let call = self.call_record(pending, 0);
                if let Err(error) =
                    persist_terminal_call(node, call, "cancelled", Some("BackendGone"), None).await
                {
                    tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist closed-controller inference call");
                }
                return Err(CompletionError::ProviderError(
                    "BackendGone: backend admission controller is draining".into(),
                ));
            }
            Err(tokio::sync::TryAcquireError::NoPermits) => {}
        }

        let queue_depth = match self.try_enter_queue() {
            Some(queue_depth) => queue_depth,
            None => {
                let queue_depth = self.waiters.load(Ordering::SeqCst);
                match self.semaphore.clone().try_acquire_owned() {
                    Ok(permit) => {
                        let call = self.call_record(pending, queue_depth);
                        return self.start_permit(node, permit, call).await;
                    }
                    Err(tokio::sync::TryAcquireError::Closed) => {
                        let call = self.call_record(pending, queue_depth);
                        if let Err(error) = persist_terminal_call(
                            node,
                            call,
                            "cancelled",
                            Some("BackendGone"),
                            None,
                        )
                        .await
                        {
                            tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist backend-gone inference call");
                        }
                        return Err(CompletionError::ProviderError(
                            "BackendGone: backend admission controller is draining".into(),
                        ));
                    }
                    Err(tokio::sync::TryAcquireError::NoPermits) => {
                        let call = self.call_record(pending, queue_depth);
                        if let Err(error) =
                            persist_terminal_call(node, call, "failed", Some("QueueFull"), None)
                                .await
                        {
                            tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist queue-full inference call");
                        }
                        return Err(CompletionError::ProviderError(format!(
                            "QueueFull: backend {} admission queue is full",
                            self.backend_id
                        )));
                    }
                }
            }
        };

        let call = self.call_record(pending, queue_depth);
        let doc_id = super::persistence::persist_call_queued(node.clone(), &call)
            .await
            .map_err(super::persistence::completion_persistence_error)?;
        let queued_guard = QueuedCallGuard {
            node: node.clone(),
            controller: self.clone(),
            call: call.clone(),
            persist_on_drop: true,
        };
        let permit = match self.semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                drop(queued_guard.disarm());
                if let Err(error) = persist_existing_call_terminal(
                    node,
                    &call,
                    "cancelled",
                    Some("BackendGone"),
                    None,
                )
                .await
                {
                    tracing::warn!(backend_id = %self.backend_id, call_id = %call.call_id, error = %error, "failed to persist backend-gone queued inference call");
                }
                return Err(CompletionError::ProviderError(
                    "BackendGone: backend admission controller is draining".into(),
                ));
            }
        };
        drop(queued_guard.disarm());
        self.running.fetch_add(1, Ordering::SeqCst);
        if let Err(error) = persist_existing_call_running(node.clone(), &call).await {
            self.release_running();
            return Err(super::persistence::completion_persistence_error(error));
        }
        Ok(AdmissionPermit::new(node, self, permit, call, doc_id))
    }

    async fn start_permit(
        self: Arc<Self>,
        node: Arc<EmbeddedNode>,
        permit: OwnedSemaphorePermit,
        call: InferenceCallRecord,
    ) -> Result<AdmissionPermit, CompletionError> {
        self.running.fetch_add(1, Ordering::SeqCst);
        match persist_call_started(node.clone(), &call).await {
            Ok(doc_id) => Ok(AdmissionPermit::new(node, self, permit, call, doc_id)),
            Err(error) => {
                self.release_running();
                Err(error)
            }
        }
    }

    fn try_enter_queue(&self) -> Option<usize> {
        loop {
            let current = self.waiters.load(Ordering::SeqCst);
            if current >= self.config.max_queue_depth {
                return None;
            }
            if self
                .waiters
                .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Some(current + 1);
            }
        }
    }

    pub(super) fn leave_queue(&self) {
        self.waiters.fetch_sub(1, Ordering::SeqCst);
    }

    pub(super) fn release_running(&self) {
        let previous = self.running.fetch_sub(1, Ordering::SeqCst);
        if previous == 1 && self.is_closed() {
            if let Some(registry) = self.registry.upgrade() {
                let backend_id = self.backend_id.clone();
                registry.controller_drained(backend_id);
            }
        }
    }

    fn call_record(
        &self,
        pending: PendingCallMetadata,
        queue_depth_at_enqueue: usize,
    ) -> InferenceCallRecord {
        InferenceCallRecord {
            call_id: pending.call_id,
            runtime_instance_id: pending.runtime_instance_id,
            request_id: pending.request_id,
            call_seq: pending.call_seq,
            backend_id: pending.backend_id,
            behavior_id: pending.behavior_id,
            agent_did: pending.agent_did,
            call_kind: pending.call_kind,
            attempt: pending.attempt,
            queue_depth_at_enqueue,
            controller_generation: self.generation,
            backend_config_fingerprint: self.config.config_fingerprint.clone(),
        }
    }
}

pub(super) struct QueuedCallGuard {
    node: Arc<EmbeddedNode>,
    controller: Arc<BackendAdmissionController>,
    call: InferenceCallRecord,
    persist_on_drop: bool,
}

impl QueuedCallGuard {
    pub(super) fn disarm(mut self) -> Self {
        self.persist_on_drop = false;
        self
    }
}

impl Drop for QueuedCallGuard {
    fn drop(&mut self) {
        self.controller.leave_queue();
        if !self.persist_on_drop {
            return;
        }
        let node = self.node.clone();
        let call = self.call.clone();
        spawn_persistence(async move {
            if let Err(error) =
                persist_existing_call_terminal(node, &call, "cancelled", Some("Cancelled"), None)
                    .await
            {
                tracing::warn!(call_id = %call.call_id, error = %error, "failed to persist cancelled queued inference call");
            }
        });
    }
}

pub(crate) struct PendingCallMetadata {
    pub(super) call_id: String,
    pub(super) runtime_instance_id: String,
    pub(super) request_id: String,
    pub(super) call_seq: u64,
    pub(super) backend_id: String,
    pub(super) behavior_id: String,
    pub(super) agent_did: String,
    pub(super) call_kind: CallKind,
    pub(super) attempt: i64,
}

#[derive(Clone)]
pub(crate) struct InferenceCallRecord {
    pub(super) call_id: String,
    pub(super) runtime_instance_id: String,
    pub(super) request_id: String,
    pub(super) call_seq: u64,
    pub(super) backend_id: String,
    pub(super) behavior_id: String,
    pub(super) agent_did: String,
    pub(super) call_kind: CallKind,
    pub(super) attempt: i64,
    pub(super) queue_depth_at_enqueue: usize,
    pub(super) controller_generation: u64,
    pub(super) backend_config_fingerprint: String,
}

impl InferenceCallRecord {
    pub(super) fn without_controller(pending: PendingCallMetadata) -> Self {
        Self {
            call_id: pending.call_id,
            runtime_instance_id: pending.runtime_instance_id,
            request_id: pending.request_id,
            call_seq: pending.call_seq,
            backend_id: pending.backend_id,
            behavior_id: pending.behavior_id,
            agent_did: pending.agent_did,
            call_kind: pending.call_kind,
            attempt: pending.attempt,
            queue_depth_at_enqueue: 0,
            controller_generation: 0,
            backend_config_fingerprint: String::new(),
        }
    }
}
```

Key changes from original: `new_controller` (originally a method on `AdmissionRegistryInner`) becomes `BackendAdmissionController::new` (associated fn). All fields on records are `pub(super)`. `call_record` now reads `self.generation` / `self.config.config_fingerprint` via `pub(super)` fields.

Dependency note: `AdmissionRegistryInner` is referenced via `super::registry::AdmissionRegistryInner` — this path is valid as soon as `registry.rs` is scaffolded (Task 2) because Rust allows forward references between sibling modules. Until Task 7 actually moves `AdmissionRegistryInner` into `registry.rs`, the import won't resolve. So we add a temporary shim in Step 5.2.

- [ ] **Step 5.2: Add temporary shim in `registry.rs`**

During this task, `AdmissionRegistryInner` still lives in `mod.rs`. Add to `crates/defra-agent/src/admission/registry.rs`:

```rust
pub(super) use super::AdmissionRegistryInner;
```

Overwrite the placeholder. This one line will be replaced entirely in Task 7.

- [ ] **Step 5.3: Remove moved lines from `mod.rs`**

Delete the span covering `struct BackendAdmissionController { ... }` through the last line of `impl InferenceCallRecord { ... }` (original lines 505-798). Also delete the `impl AdmissionRegistryInner { fn new_controller ... }` block at lines 435-462 — except `fn controller_drained`, which `registry.rs` will eventually own but which `mod.rs` still needs for the existing `AdmissionRegistry::acquire_current_call` path. Keep `controller_drained` where it is for now; it moves in Task 7.

Update the `state.active.insert` call site in `AdmissionRegistry::reconcile` (original lines 302-310, 359-363, and `install_pending_if_ready`'s equivalent at 493-500) from:

```rust
self.inner.new_controller(generation, config.clone(), Arc::downgrade(&self.inner))
```

to:

```rust
BackendAdmissionController::new(generation, config.clone(), Arc::downgrade(&self.inner))
```

Same change in `RegistryState::install_pending_if_ready`.

Add `use self::controller::{BackendAdmissionController, InferenceCallRecord, PendingCallMetadata};` to the top of `mod.rs` so these three types resolve in the remaining registry code.

- [ ] **Step 5.4: Compile and test**

Run:
```bash
cargo check -p defra-agent
cargo test -p defra-agent admission::
```
Expected: clean build; 4 admission tests pass.

- [ ] **Step 5.5: Commit**

```bash
git add crates/defra-agent/src/admission/
git commit -m "Extract admission controller and call records into admission/controller.rs"
```

---

## Task 6: Move permit to `permit.rs`

Moves `AdmissionPermit`, `PermitTerminal`, and the `Drop` / `StreamGuardLifecycle` impls.

**Files:**
- Modify: `crates/defra-agent/src/admission/mod.rs` — remove lines 800-927
- Modify: `crates/defra-agent/src/admission/permit.rs` — receive content

- [ ] **Step 6.1: Write `permit.rs` contents**

Replace the placeholder with:

```rust
use std::sync::Arc;

use defra_node::EmbeddedNode;
use rig::completion::{CompletionError, Usage};
use tokio::sync::OwnedSemaphorePermit;

use super::controller::{BackendAdmissionController, InferenceCallRecord};
use super::persistence::persist_existing_call_terminal;
use super::spawn_persistence;
use super::stream_guard::StreamGuardLifecycle;

pub(crate) struct AdmissionPermit {
    node: Arc<EmbeddedNode>,
    controller: Arc<BackendAdmissionController>,
    _permit: OwnedSemaphorePermit,
    call: InferenceCallRecord,
    _doc_id: String,
    terminal: Option<PermitTerminal>,
    finished: bool,
}

#[derive(Clone, Debug)]
struct PermitTerminal {
    call_state: &'static str,
    failure_reason: Option<String>,
    usage: Option<Usage>,
}

impl AdmissionPermit {
    pub(super) fn new(
        node: Arc<EmbeddedNode>,
        controller: Arc<BackendAdmissionController>,
        permit: OwnedSemaphorePermit,
        call: InferenceCallRecord,
        doc_id: String,
    ) -> Self {
        Self {
            node,
            controller,
            _permit: permit,
            call,
            _doc_id: doc_id,
            terminal: None,
            finished: false,
        }
    }

    pub(crate) async fn finish_success(&mut self, usage: Option<Usage>) {
        self.terminal = Some(PermitTerminal {
            call_state: "completed",
            failure_reason: None,
            usage,
        });
        self.finish().await;
    }

    pub(crate) async fn finish_failure(&mut self, reason: &str) {
        self.terminal = Some(PermitTerminal {
            call_state: "failed",
            failure_reason: Some(reason.to_string()),
            usage: None,
        });
        self.finish().await;
    }

    async fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let terminal = self.terminal.clone().unwrap_or(PermitTerminal {
            call_state: "completed",
            failure_reason: None,
            usage: None,
        });
        if let Err(error) = persist_existing_call_terminal(
            self.node.clone(),
            &self.call,
            terminal.call_state,
            terminal.failure_reason.as_deref(),
            terminal.usage,
        )
        .await
        {
            tracing::warn!(call_id = %self.call.call_id, error = %error, "failed to persist terminal inference call state");
        }
    }
}

impl StreamGuardLifecycle for AdmissionPermit {
    fn mark_stream_success(&mut self, usage: Option<Usage>) {
        if self.terminal.is_none() {
            self.terminal = Some(PermitTerminal {
                call_state: "completed",
                failure_reason: None,
                usage,
            });
        }
    }

    fn mark_stream_error(&mut self, error: &CompletionError) {
        self.terminal = Some(PermitTerminal {
            call_state: "failed",
            failure_reason: Some(error.to_string()),
            usage: None,
        });
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        self.controller.release_running();
        if self.finished {
            return;
        }
        self.finished = true;
        let terminal = self.terminal.clone().unwrap_or(PermitTerminal {
            call_state: "failed",
            failure_reason: Some("StreamDroppedBeforeTerminalResponse".to_string()),
            usage: None,
        });
        let node = self.node.clone();
        let call_id = self.call.call_id.clone();
        let call = self.call.clone();
        spawn_persistence(async move {
            if let Err(error) = persist_existing_call_terminal(
                node,
                &call,
                terminal.call_state,
                terminal.failure_reason.as_deref(),
                terminal.usage,
            )
            .await
            {
                tracing::warn!(call_id = %call_id, error = %error, "failed to persist dropped inference call state");
            }
        });
    }
}
```

Change notes: `AdmissionPermit::new` becomes `pub(super)` (it's called from `controller.rs`). `finish_success`/`finish_failure` become `pub(crate)` since they're called from `client.rs` (the admitted completion model) — originally they were private inside the same module. This is the minimum visibility to keep existing callers working.

- [ ] **Step 6.2: Remove moved lines from `mod.rs`**

Delete the `pub(crate) struct AdmissionPermit { ... }` block through the end of `impl Drop for AdmissionPermit` (original lines 800-927).

Remove now-unused imports from `mod.rs`: `use rig::completion::{... Usage};` → drop `Usage`. `use tokio::sync::{OwnedSemaphorePermit, Semaphore};` → drop `OwnedSemaphorePermit`. `use self::stream_guard::StreamGuardLifecycle;` can stay if needed by `client.rs`'s re-export path; drop if `cargo check` flags it unused.

Add to `mod.rs`:
```rust
pub(crate) use permit::AdmissionPermit;
```

- [ ] **Step 6.3: Compile and test**

Run:
```bash
cargo check -p defra-agent
cargo test -p defra-agent admission::
```
Expected: clean build; 4 admission tests pass.

- [ ] **Step 6.4: Commit**

```bash
git add crates/defra-agent/src/admission/
git commit -m "Extract admission permit into admission/permit.rs"
```

---

## Task 7: Move registry to `registry.rs`

Moves `AdmissionRegistry`, `AdmissionRegistryInner`, `RegistryState`, `PendingControllerConfig`, and their impls. This deletes the temporary shim from Task 5.2.

**Files:**
- Modify: `crates/defra-agent/src/admission/mod.rs` — remove remaining registry code (lines 243-503 originally, minus already-moved)
- Modify: `crates/defra-agent/src/admission/registry.rs` — replace shim with full content

- [ ] **Step 7.1: Write `registry.rs` contents**

Overwrite `registry.rs` completely (replacing the Task 5 shim) with:

```rust
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use defra_node::EmbeddedNode;
use rig::completion::CompletionError;

use super::client::{current_context, scope_request, AdmissionCallContext, CallKind};
use super::config::BackendAdmissionConfig;
use super::controller::{BackendAdmissionController, InferenceCallRecord};
use super::permit::AdmissionPermit;
use super::persistence::persist_terminal_call;

#[derive(Clone)]
pub(crate) struct AdmissionRegistry {
    inner: Arc<AdmissionRegistryInner>,
}

pub(super) struct AdmissionRegistryInner {
    node: Arc<EmbeddedNode>,
    pub(super) runtime_instance_id: String,
    state: Mutex<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
    active: HashMap<String, Arc<BackendAdmissionController>>,
    draining: HashMap<String, Vec<Arc<BackendAdmissionController>>>,
    pending: HashMap<String, PendingControllerConfig>,
}

#[derive(Clone)]
struct PendingControllerConfig {
    generation: u64,
    config: BackendAdmissionConfig,
}

impl AdmissionRegistry {
    pub(crate) fn new(node: Arc<EmbeddedNode>) -> Self {
        Self {
            inner: Arc::new(AdmissionRegistryInner {
                node,
                runtime_instance_id: uuid::Uuid::new_v4().to_string(),
                state: Mutex::new(RegistryState::default()),
            }),
        }
    }

    pub(crate) fn reconcile(
        &self,
        generation: u64,
        configs: &HashMap<String, BackendAdmissionConfig>,
    ) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("AdmissionRegistry state lock poisoned");
        state.prune_drained();

        let desired_ids = configs.keys().cloned().collect::<HashSet<_>>();
        let active_ids = state.active.keys().cloned().collect::<Vec<_>>();
        for backend_id in active_ids {
            let desired = configs
                .get(&backend_id)
                .filter(|config| config.is_available());
            match (state.active.remove(&backend_id), desired) {
                (Some(active), Some(config)) if active.matches(config) => {
                    state.active.insert(backend_id, active);
                }
                (Some(active), Some(config)) => {
                    active.close();
                    if active.is_drained() {
                        state.active.insert(
                            backend_id.clone(),
                            BackendAdmissionController::new(
                                generation,
                                config.clone(),
                                Arc::downgrade(&self.inner),
                            ),
                        );
                    } else {
                        state
                            .draining
                            .entry(backend_id.clone())
                            .or_default()
                            .push(active);
                        state.pending.insert(
                            backend_id,
                            PendingControllerConfig {
                                generation,
                                config: config.clone(),
                            },
                        );
                    }
                }
                (Some(active), None) => {
                    active.close();
                    if !active.is_drained() {
                        state
                            .draining
                            .entry(backend_id.clone())
                            .or_default()
                            .push(active);
                    }
                    state.pending.remove(&backend_id);
                }
                (None, _) => {}
            }
        }

        for (backend_id, config) in configs {
            if !config.is_available() || !desired_ids.contains(backend_id) {
                state.pending.remove(backend_id);
                continue;
            }
            if state.active.contains_key(backend_id) {
                continue;
            }
            if state.has_draining(backend_id) {
                state.pending.insert(
                    backend_id.clone(),
                    PendingControllerConfig {
                        generation,
                        config: config.clone(),
                    },
                );
                continue;
            }
            state.active.insert(
                backend_id.clone(),
                BackendAdmissionController::new(
                    generation,
                    config.clone(),
                    Arc::downgrade(&self.inner),
                ),
            );
        }

        let pending_ids = state.pending.keys().cloned().collect::<Vec<_>>();
        for backend_id in pending_ids {
            state.install_pending_if_ready(&self.inner, &backend_id);
        }
    }

    #[cfg(test)]
    pub(crate) async fn acquire_for_test(
        &self,
        request_id: impl Into<String>,
        backend_id: impl Into<String>,
        behavior_id: impl Into<String>,
        agent_did: impl Into<String>,
        call_kind: CallKind,
    ) -> Result<AdmissionPermit, CompletionError> {
        use std::sync::atomic::AtomicU64;
        let context = AdmissionCallContext {
            request_id: request_id.into(),
            backend_id: backend_id.into(),
            behavior_id: behavior_id.into(),
            agent_did: agent_did.into(),
            call_kind,
            attempt: 1,
            call_seq: Arc::new(AtomicU64::new(0)),
        };
        scope_request(context, async { self.acquire_current_call().await }).await
    }

    pub(super) async fn acquire_current_call(&self) -> Result<AdmissionPermit, CompletionError> {
        let context = current_context()?;
        let pending = context.next_call(&self.inner.runtime_instance_id);
        if pending.backend_id.trim().is_empty() {
            return Err(CompletionError::ProviderError(format!(
                "behavior {} has no backend binding",
                pending.behavior_id
            )));
        }

        let controller = {
            let state = self
                .inner
                .state
                .lock()
                .expect("AdmissionRegistry state lock poisoned");
            state.active.get(&pending.backend_id).cloned()
        };

        match controller {
            Some(controller) => controller.acquire(self.inner.node.clone(), pending).await,
            None => {
                let call = InferenceCallRecord::without_controller(pending);
                if let Err(error) = persist_terminal_call(
                    self.inner.node.clone(),
                    call,
                    "cancelled",
                    Some("BackendGone"),
                    None,
                )
                .await
                {
                    tracing::warn!(error = %error, "failed to persist backend-gone inference call");
                }
                Err(CompletionError::ProviderError(
                    "BackendGone: backend admission controller is not active".into(),
                ))
            }
        }
    }
}

impl AdmissionRegistryInner {
    pub(super) fn controller_drained(self: Arc<Self>, backend_id: String) {
        let mut state = self
            .state
            .lock()
            .expect("AdmissionRegistry state lock poisoned");
        state.install_pending_if_ready(&self, &backend_id);
    }
}

impl RegistryState {
    fn prune_drained(&mut self) {
        self.draining.retain(|_, controllers| {
            controllers.retain(|controller| !controller.is_drained());
            !controllers.is_empty()
        });
    }

    fn has_draining(&mut self, backend_id: &str) -> bool {
        self.prune_drained();
        self.draining
            .get(backend_id)
            .is_some_and(|controllers| !controllers.is_empty())
    }

    fn install_pending_if_ready(
        &mut self,
        registry: &Arc<AdmissionRegistryInner>,
        backend_id: &str,
    ) {
        self.prune_drained();
        if self.active.contains_key(backend_id) || self.has_draining(backend_id) {
            return;
        }
        let Some(pending) = self.pending.remove(backend_id) else {
            return;
        };
        if pending.config.is_available() {
            self.active.insert(
                backend_id.to_string(),
                BackendAdmissionController::new(
                    pending.generation,
                    pending.config,
                    Arc::downgrade(registry),
                ),
            );
        }
    }
}
```

Note: `AdmissionRegistryInner` is now `pub(super)` (sibling controller.rs holds a `Weak<AdmissionRegistryInner>`). `runtime_instance_id` is `pub(super)` (controller reads it via registry weak-upgrade? actually no — controller uses it only via the closure in Drop → `controller_drained` which is on `AdmissionRegistryInner`, so OK). Actually `runtime_instance_id` is read only inside `AdmissionRegistry::acquire_current_call` which lives in `registry.rs` itself, so it can be private — drop the `pub(super)`. Adjust if compiler complains.

- [ ] **Step 7.2: Remove moved lines + inner helpers from `mod.rs`**

Delete remaining registry code from `mod.rs`: the `pub(crate) struct AdmissionRegistry`, `struct AdmissionRegistryInner`, `struct RegistryState`, `struct PendingControllerConfig`, their impl blocks including `impl AdmissionRegistry`, `impl AdmissionRegistryInner` (`controller_drained` method only — `new_controller` already moved in Task 5), and `impl RegistryState`.

Remove now-orphan imports: `use std::collections::{HashMap, HashSet};`, `use std::sync::{Arc, Mutex, Weak};`, `use defra_node::EmbeddedNode;`.

Add to `mod.rs`:
```rust
pub(crate) use registry::AdmissionRegistry;
```

- [ ] **Step 7.3: Compile and test**

Run:
```bash
cargo check -p defra-agent
cargo test -p defra-agent admission::
```
Expected: clean build; 4 admission tests pass.

- [ ] **Step 7.4: Commit**

```bash
git add crates/defra-agent/src/admission/
git commit -m "Extract admission registry into admission/registry.rs"
```

---

## Task 8: Move persistence to `persistence.rs`

Moves `persist_*` functions, `add_call_mutation`, `upsert_*_mutation`, `optional_graphql_string`, `usage_fields`, `spawn_persistence`, `completion_persistence_error`, `extract_inference_call_doc_id`.

**Files:**
- Modify: `crates/defra-agent/src/admission/mod.rs` — remove lines 929-1231 originally
- Modify: `crates/defra-agent/src/admission/persistence.rs` — receive content

- [ ] **Step 8.1: Write `persistence.rs` contents**

Replace the placeholder with:

```rust
use std::future::Future;
use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use rig::completion::{CompletionError, Usage};

use super::controller::InferenceCallRecord;
use crate::graphql::escape_graphql_string;

pub(super) fn spawn_persistence<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(future);
    }
}

pub(super) fn completion_persistence_error(error: anyhow::Error) -> CompletionError {
    CompletionError::ProviderError(format!("persisting InferenceCall failed: {error:#}"))
}

fn extract_inference_call_doc_id(data: Option<&serde_json::Value>) -> Result<String> {
    data.and_then(|data| data.get("add_InferenceCall"))
        .and_then(|value| {
            value
                .get("_docID")
                .and_then(|doc_id| doc_id.as_str())
                .or_else(|| {
                    value
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("_docID"))
                        .and_then(|doc_id| doc_id.as_str())
                })
        })
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("add_InferenceCall returned no _docID"))
}

pub(super) async fn persist_call_queued(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "queued", None, Some(&now), None, None, None);
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("persisting queued InferenceCall failed: {:?}", resp.errors);
    }
    extract_inference_call_doc_id(resp.data.as_ref())
}

pub(super) async fn persist_call_started(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<String, CompletionError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(call, "running", None, Some(&now), Some(&now), None, None);
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        return Err(CompletionError::ProviderError(format!(
            "persisting running InferenceCall failed: {:?}",
            resp.errors
        )));
    }
    extract_inference_call_doc_id(resp.data.as_ref()).map_err(completion_persistence_error)
}

pub(super) async fn persist_existing_call_running(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = upsert_call_running_mutation(call, &now);
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("persisting running InferenceCall failed: {:?}", resp.errors);
    }
    Ok(())
}

pub(super) async fn persist_terminal_call(
    node: Arc<EmbeddedNode>,
    call: InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    usage: Option<Usage>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = add_call_mutation(
        &call,
        call_state,
        failure_reason,
        Some(&now),
        None,
        Some(&now),
        usage,
    );
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!(
            "persisting terminal InferenceCall failed: {:?}",
            resp.errors
        );
    }
    Ok(())
}

pub(super) async fn persist_existing_call_terminal(
    node: Arc<EmbeddedNode>,
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    usage: Option<Usage>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = upsert_call_terminal_mutation(call, call_state, failure_reason, &now, usage);
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!(
            "persisting terminal InferenceCall failed: {:?}",
            resp.errors
        );
    }
    Ok(())
}

fn add_call_mutation(
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    queued_at: Option<&str>,
    started_at: Option<&str>,
    ended_at: Option<&str>,
    usage: Option<Usage>,
) -> String {
    let queued_at = optional_graphql_string("queued_at", queued_at);
    let started_at = optional_graphql_string("started_at", started_at);
    let ended_at = optional_graphql_string("ended_at", ended_at);
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens) = usage_fields(usage);
    format!(
        r#"mutation {{
            add_InferenceCall(input: {{
                call_id: "{call_id}",
                runtime_instance_id: "{runtime_instance_id}",
                request_id: "{request_id}",
                call_seq: {call_seq},
                backend_id: "{backend_id}",
                behavior_id: "{behavior_id}",
                agent_did: "{agent_did}",
                call_kind: "{call_kind}",
                attempt: {attempt},
                call_state: "{call_state}",
                {failure_reason}
                {queued_at}
                {started_at}
                {ended_at}
                priority: 0,
                queue_depth_at_enqueue: {queue_depth_at_enqueue},
                controller_generation: {controller_generation},
                backend_config_fingerprint: "{backend_config_fingerprint}"
                {prompt_tokens}
                {completion_tokens}
            }}) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        call_seq = call.call_seq,
        backend_id = escape_graphql_string(&call.backend_id),
        behavior_id = escape_graphql_string(&call.behavior_id),
        agent_did = escape_graphql_string(&call.agent_did),
        call_kind = call.call_kind.as_str(),
        attempt = call.attempt,
        call_state = call_state,
        failure_reason = failure_reason,
        queued_at = queued_at,
        started_at = started_at,
        ended_at = ended_at,
        queue_depth_at_enqueue = call.queue_depth_at_enqueue,
        controller_generation = call.controller_generation,
        backend_config_fingerprint = escape_graphql_string(&call.backend_config_fingerprint),
        prompt_tokens = prompt_tokens,
        completion_tokens = completion_tokens,
    )
}

fn upsert_call_running_mutation(call: &InferenceCallRecord, started_at: &str) -> String {
    format!(
        r#"mutation {{
            upsert_InferenceCall(
                filter: {{ call_id: {{ _eq: "{call_id}" }} }},
                add: {{
                    call_id: "{call_id}",
                    runtime_instance_id: "{runtime_instance_id}",
                    request_id: "{request_id}",
                    call_seq: {call_seq},
                    backend_id: "{backend_id}",
                    behavior_id: "{behavior_id}",
                    agent_did: "{agent_did}",
                    call_kind: "{call_kind}",
                    attempt: {attempt},
                    call_state: "running",
                    queued_at: "{started_at}",
                    started_at: "{started_at}",
                    priority: 0,
                    queue_depth_at_enqueue: {queue_depth_at_enqueue},
                    controller_generation: {controller_generation},
                    backend_config_fingerprint: "{backend_config_fingerprint}"
                }},
                update: {{
                    call_state: "running",
                    started_at: "{started_at}"
                }}
            ) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        call_seq = call.call_seq,
        backend_id = escape_graphql_string(&call.backend_id),
        behavior_id = escape_graphql_string(&call.behavior_id),
        agent_did = escape_graphql_string(&call.agent_did),
        call_kind = call.call_kind.as_str(),
        attempt = call.attempt,
        started_at = escape_graphql_string(started_at),
        queue_depth_at_enqueue = call.queue_depth_at_enqueue,
        controller_generation = call.controller_generation,
        backend_config_fingerprint = escape_graphql_string(&call.backend_config_fingerprint),
    )
}

fn upsert_call_terminal_mutation(
    call: &InferenceCallRecord,
    call_state: &str,
    failure_reason: Option<&str>,
    ended_at: &str,
    usage: Option<Usage>,
) -> String {
    let failure_reason = optional_graphql_string("failure_reason", failure_reason);
    let (prompt_tokens, completion_tokens) = usage_fields(usage);
    format!(
        r#"mutation {{
            upsert_InferenceCall(
                filter: {{ call_id: {{ _eq: "{call_id}" }} }},
                add: {{
                    call_id: "{call_id}",
                    runtime_instance_id: "{runtime_instance_id}",
                    request_id: "{request_id}",
                    call_seq: {call_seq},
                    backend_id: "{backend_id}",
                    behavior_id: "{behavior_id}",
                    agent_did: "{agent_did}",
                    call_kind: "{call_kind}",
                    attempt: {attempt},
                    call_state: "{call_state}",
                    {failure_reason}
                    queued_at: "{ended_at}",
                    ended_at: "{ended_at}",
                    priority: 0,
                    queue_depth_at_enqueue: {queue_depth_at_enqueue},
                    controller_generation: {controller_generation},
                    backend_config_fingerprint: "{backend_config_fingerprint}"
                    {prompt_tokens}
                    {completion_tokens}
                }},
                update: {{
                    call_state: "{call_state}",
                    {failure_reason}
                    ended_at: "{ended_at}"
                    {prompt_tokens}
                    {completion_tokens}
                }}
            ) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call.call_id),
        runtime_instance_id = escape_graphql_string(&call.runtime_instance_id),
        request_id = escape_graphql_string(&call.request_id),
        call_seq = call.call_seq,
        backend_id = escape_graphql_string(&call.backend_id),
        behavior_id = escape_graphql_string(&call.behavior_id),
        agent_did = escape_graphql_string(&call.agent_did),
        call_kind = call.call_kind.as_str(),
        attempt = call.attempt,
        call_state = call_state,
        failure_reason = failure_reason,
        ended_at = escape_graphql_string(ended_at),
        queue_depth_at_enqueue = call.queue_depth_at_enqueue,
        controller_generation = call.controller_generation,
        backend_config_fingerprint = escape_graphql_string(&call.backend_config_fingerprint),
        prompt_tokens = prompt_tokens,
        completion_tokens = completion_tokens,
    )
}

fn optional_graphql_string(field: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!(r#"{field}: "{}","#, escape_graphql_string(value)))
        .unwrap_or_default()
}

fn usage_fields(usage: Option<Usage>) -> (String, String) {
    match usage {
        Some(usage) => (
            format!("prompt_tokens: {},", usage.input_tokens),
            format!("completion_tokens: {},", usage.output_tokens),
        ),
        None => (String::new(), String::new()),
    }
}
```

`spawn_persistence` moves here and becomes `pub(super)` because `controller.rs` and `permit.rs` both spawn via it. Update those files' `use super::spawn_persistence;` paths — they already import via `super::spawn_persistence` in Tasks 5 and 6, which resolves to `mod.rs`. Since `spawn_persistence` now lives in `persistence.rs`, either:
- add `pub(super) use persistence::spawn_persistence;` to `mod.rs` (simplest), or
- change sibling imports to `use super::persistence::spawn_persistence;`.

Use the latter: cleaner long-term and matches the pattern of every other helper.

- [ ] **Step 8.2: Update `controller.rs` and `permit.rs` imports**

In both `crates/defra-agent/src/admission/controller.rs` and `crates/defra-agent/src/admission/permit.rs`, change:
```rust
use super::spawn_persistence;
```
to:
```rust
use super::persistence::spawn_persistence;
```

- [ ] **Step 8.3: Remove moved code from `mod.rs`**

Delete `fn spawn_persistence`, `fn completion_persistence_error`, `fn extract_inference_call_doc_id`, all `persist_*` async fns, `add_call_mutation`, `upsert_call_running_mutation`, `upsert_call_terminal_mutation`, `optional_graphql_string`, `usage_fields` (original lines 929-1231).

Remove now-orphan imports from `mod.rs`: `use crate::graphql::escape_graphql_string;`, any remaining `Usage`/chrono bits.

- [ ] **Step 8.4: Compile and test**

Run:
```bash
cargo check -p defra-agent
cargo test -p defra-agent admission::
```
Expected: clean build; 4 admission tests pass.

- [ ] **Step 8.5: Commit**

```bash
git add crates/defra-agent/src/admission/
git commit -m "Extract admission persistence into admission/persistence.rs"
```

---

## Task 9: Extract tests to `tests.rs`

Moves the inline `#[cfg(test)] mod tests { ... }` block into a sibling `src/admission/tests.rs`. Keeps all four tests in one file — per the refactor spec, single-`tests.rs` is acceptable when test count is small (4 tests, ~220 lines; splitting by sibling-module would produce mostly empty files).

**Files:**
- Create: `crates/defra-agent/src/admission/tests.rs`
- Modify: `crates/defra-agent/src/admission/mod.rs` — delete inline test module, declare sibling

- [ ] **Step 9.1: Create `tests.rs`**

Write `crates/defra-agent/src/admission/tests.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use serde_json::Value;

use super::{
    scope_call, scope_request, AdmissionCallContext, AdmissionRegistry, BackendAdmissionConfig,
    CallKind,
};
use crate::schema::ensure_schemas;
use crate::watcher::AgentRequest;

async fn test_node() -> Arc<EmbeddedNode> {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_schemas(node.as_ref()).await.unwrap();
    node
}

fn config(
    backend_id: &str,
    max_concurrent: usize,
    max_queue_depth: usize,
) -> BackendAdmissionConfig {
    BackendAdmissionConfig {
        backend_id: backend_id.to_string(),
        max_concurrent,
        max_queue_depth,
        enabled: true,
        probe_status: "healthy".to_string(),
        config_fingerprint: format!("{backend_id}:{max_concurrent}:{max_queue_depth}"),
    }
}

fn request(request_id: &str) -> AgentRequest {
    AgentRequest {
        doc_id: format!("doc-{request_id}"),
        request_id: request_id.to_string(),
        agent_did: "did:defra-agent:test".to_string(),
        behavior_id: Some("default".to_string()),
        session_id: format!("session-{request_id}"),
        content: "hello".to_string(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: None,
        created_at: "2026-04-15T00:00:00Z".to_string(),
    }
}

async fn call_rows(node: &EmbeddedNode) -> Vec<Value> {
    let response = node
        .execute(
            r#"{
                InferenceCall(order: { call_seq: ASC }) {
                    request_id
                    call_seq
                    backend_id
                    behavior_id
                    call_kind
                    call_state
                    failure_reason
                    queue_depth_at_enqueue
                }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn max_queue_depth_zero_allows_immediate_permit_and_rejects_saturated_backend() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 0))]),
    );
    let context =
        AdmissionCallContext::for_request(&request("req-zero"), "default", "backend-a");

    scope_request(context, async {
        let mut first = registry.acquire_current_call().await.unwrap();
        let error = match registry.acquire_current_call().await {
            Ok(_) => panic!("saturated backend should reject without queue capacity"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("QueueFull"));
        first.finish_success(None).await;
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["call_state"], "completed");
    assert_eq!(rows[1]["call_state"], "failed");
    assert_eq!(rows[1]["failure_reason"], "QueueFull");
}

#[tokio::test]
async fn queued_calls_start_in_tokio_registration_order_after_permit_release() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 2))]),
    );
    let first_context =
        AdmissionCallContext::for_request(&request("req-ordered"), "default", "backend-a");
    let second_context = first_context.clone();

    scope_request(first_context, async {
        let mut first = registry.acquire_current_call().await.unwrap();
        let second_registry = registry.clone();
        let second = tokio::spawn(async move {
            scope_request(second_context, async move {
                let mut permit = second_registry.acquire_current_call().await.unwrap();
                permit.finish_success(None).await;
            })
            .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        let rows = call_rows(node.as_ref()).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["call_state"], "running");
        assert_eq!(rows[1]["call_state"], "queued");

        first.finish_success(None).await;
        drop(first);
        second.await.unwrap();
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["call_state"], "completed");
    assert_eq!(rows[1]["call_state"], "completed");
    assert_eq!(rows[1]["queue_depth_at_enqueue"], 1);
}

#[tokio::test]
async fn scoped_scheduled_calls_are_persisted_with_scheduled_kind() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
    );
    let context =
        AdmissionCallContext::for_request(&request("req-scheduled"), "default", "backend-a");

    scope_request(context, async {
        scope_call(CallKind::Scheduled, 1, async {
            let mut permit = registry.acquire_current_call().await.unwrap();
            permit.finish_success(None).await;
        })
        .await;
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["call_kind"], "scheduled");
    assert_eq!(rows[0]["call_state"], "completed");
}

#[tokio::test]
async fn compaction_calls_share_backend_capacity_with_inference_calls() {
    let node = test_node().await;
    let registry = AdmissionRegistry::new(node.clone());
    registry.reconcile(
        1,
        &HashMap::from([("backend-a".to_string(), config("backend-a", 1, 1))]),
    );
    let inference_context =
        AdmissionCallContext::for_request(&request("req-compaction"), "default", "backend-a");
    let compaction_context = inference_context.clone();

    scope_request(inference_context, async {
        let mut inference = registry.acquire_current_call().await.unwrap();
        let compaction_registry = registry.clone();
        let compaction = tokio::spawn(async move {
            scope_request(compaction_context, async move {
                scope_call(CallKind::Compaction, 1, async {
                    let mut permit = compaction_registry.acquire_current_call().await.unwrap();
                    permit.finish_success(None).await;
                })
                .await;
            })
            .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        let rows = call_rows(node.as_ref()).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["call_kind"], "inference");
        assert_eq!(rows[0]["call_state"], "running");
        assert_eq!(rows[1]["call_kind"], "compaction");
        assert_eq!(rows[1]["call_state"], "queued");

        inference.finish_success(None).await;
        drop(inference);
        compaction.await.unwrap();
    })
    .await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let rows = call_rows(node.as_ref()).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["call_state"], "completed");
    assert_eq!(rows[1]["call_state"], "completed");
    assert_eq!(rows[1]["queue_depth_at_enqueue"], 1);
}
```

Tests use the `acquire_current_call` method (now `pub(super)` in `registry.rs`). Because `tests.rs` is a sibling module, `pub(super)` is visible. If the compiler objects, bump to `pub(crate)` — but `pub(super)` should suffice.

- [ ] **Step 9.2: Remove inline tests from `mod.rs`**

Delete the `#[cfg(test)] mod tests { ... }` block (original lines 1233-1456).

Add, at the bottom of `mod.rs`:

```rust
#[cfg(test)]
mod tests;
```

- [ ] **Step 9.3: Compile and test**

Run:
```bash
cargo check -p defra-agent
cargo test -p defra-agent admission::tests
```
Expected: clean build; `test result: ok. 4 passed; 0 failed` — same count as Step 1.3 baseline.

Verify the exact same test names are discovered:
```bash
cargo test -p defra-agent admission::tests -- --list 2>&1 | grep ': test$' | sort
```
Expected (sorted):
```
admission::tests::compaction_calls_share_backend_capacity_with_inference_calls: test
admission::tests::max_queue_depth_zero_allows_immediate_permit_and_rejects_saturated_backend: test
admission::tests::queued_calls_start_in_tokio_registration_order_after_permit_release: test
admission::tests::scoped_scheduled_calls_are_persisted_with_scheduled_kind: test
```

- [ ] **Step 9.4: Commit**

```bash
git add crates/defra-agent/src/admission/
git commit -m "Extract admission tests into admission/tests.rs"
```

---

## Task 10: Verify `mod.rs` is a shell and review `stream_guard.rs`

`mod.rs` should now be a dispatch shell only. Verify.

**Files:**
- Modify: `crates/defra-agent/src/admission/mod.rs` — finalize

- [ ] **Step 10.1: Inspect `mod.rs`**

Run:
```bash
wc -l crates/defra-agent/src/admission/mod.rs
cat crates/defra-agent/src/admission/mod.rs
```

Expected: ≤ 30 lines. Content should be exactly (order of `pub(crate) use` lines can vary):

```rust
pub(crate) mod stream_guard;
mod client;
mod config;
mod controller;
mod permit;
mod persistence;
mod registry;

#[cfg(test)]
mod tests;

pub(crate) use client::{
    scope_call, scope_request, AdmissionCallContext, AdmittedCompletionClient, CallKind,
};
pub(crate) use config::{backend_admission_configs_from_backends, BackendAdmissionConfig};
pub(crate) use permit::AdmissionPermit;
pub(crate) use registry::AdmissionRegistry;
```

If any implementation code remains — move it to the appropriate sibling. If imports remain that aren't used by re-exports — delete them.

- [ ] **Step 10.2: Verify no inline test modules remain in admission**

Run:
```bash
grep -rn "^#\[cfg(test)\]$" crates/defra-agent/src/admission/
```
Expected output: two lines only:
```
crates/defra-agent/src/admission/mod.rs:N:#[cfg(test)]
crates/defra-agent/src/admission/stream_guard.rs:159:#[cfg(test)]
```
(The `stream_guard.rs` inline tests are out of scope for Phase A1 — that file was already healthy at 242 lines. They remain inline and are addressed in Phase A9 borderline sweep if applicable.)

- [ ] **Step 10.3: Commit any final shell adjustments**

If you edited `mod.rs` in Step 10.1:

```bash
git add crates/defra-agent/src/admission/mod.rs
git commit -m "Finalize admission mod.rs as dispatch shell"
```

Otherwise skip.

---

## Task 11: Comment cleanup pass

Apply the comment cleanup policy from the refactor spec to all seven new files. `stream_guard.rs` is out of scope (not touched in Phase A1).

**Files:**
- Modify: `crates/defra-agent/src/admission/client.rs`
- Modify: `crates/defra-agent/src/admission/config.rs`
- Modify: `crates/defra-agent/src/admission/controller.rs`
- Modify: `crates/defra-agent/src/admission/permit.rs`
- Modify: `crates/defra-agent/src/admission/persistence.rs`
- Modify: `crates/defra-agent/src/admission/registry.rs`
- Modify: `crates/defra-agent/src/admission/tests.rs`

Policy (from spec):
- Delete comments that restate the code.
- Delete change-log / history comments.
- Delete section banner comments.
- Rewrite `///` doc comments on public items to be terse and current; drop fluff.
- Keep only comments explaining non-obvious *why* — invariants, subtle ordering, workarounds with issue links.

- [ ] **Step 11.1: Audit comments**

Run:
```bash
grep -nE "^\s*(//|///)" crates/defra-agent/src/admission/client.rs crates/defra-agent/src/admission/config.rs crates/defra-agent/src/admission/controller.rs crates/defra-agent/src/admission/permit.rs crates/defra-agent/src/admission/persistence.rs crates/defra-agent/src/admission/registry.rs crates/defra-agent/src/admission/tests.rs
```

For each comment, apply the policy. Known comments to evaluate:

- `controller.rs`: `// BackendAdmissionConfig validation guarantees this is >= 1.` — this is a non-obvious *why* (explains an invariant that justifies using `Semaphore::new(n)` without runtime validation). **Keep.**

Any `// TODO`, `// used by X`, `// fix for #N`, `// added when Y` — delete.

- [ ] **Step 11.2: Apply edits file by file**

Edit the files identified in Step 11.1. For each comment deleted or rewritten, verify the code still compiles and tests still pass after the file is done.

- [ ] **Step 11.3: Compile and test after cleanup**

Run:
```bash
cargo check -p defra-agent
cargo test -p defra-agent admission::
```
Expected: clean build; 4 admission tests pass.

- [ ] **Step 11.4: Commit**

```bash
git add crates/defra-agent/src/admission/
git commit -m "Apply comment cleanup policy to admission submodules"
```

If no comments needed changing, skip the commit — not a failure.

---

## Task 12: Final verification + PR prep

**Files:** none modified.

- [ ] **Step 12.1: Check file sizes are within target**

Run:
```bash
wc -l crates/defra-agent/src/admission/*.rs
```

Expected: all files under 500 lines, most under 400. `controller.rs` is the largest and is allowed up to ~450 lines (a single responsibility: controller + records + queue guard). If any file exceeds 500 and the soft-cap fallback is not justified, reconsider the split with the reviewer before moving on.

- [ ] **Step 12.2: Run the Lean-spec guard tests**

Run:
```bash
cargo test -p defra-agent --test state_machine_conformance
cargo test -p defra-agent --test lifecycle_regression
```
Expected: both green. If either fails, the refactor changed behavior somewhere — investigate before opening a PR (do NOT edit the test expectations).

- [ ] **Step 12.3: Run the full agent library test suite**

Run:
```bash
cargo test -p defra-agent
```
Expected: all tests pass. Note the total pass count for the PR description.

- [ ] **Step 12.4: Confirm no inline test modules were added elsewhere**

Run:
```bash
grep -rn "^#\[cfg(test)\]\nmod tests \{" crates/defra-agent/src/admission/ --include="*.rs"
```
Expected: no matches (stream_guard.rs has its tests inline but that file is out of scope).

- [ ] **Step 12.5: Push and open PR**

Push the branch:
```bash
git push -u origin refactor/admission-split
```

Open the PR using the gh CLI with a body that includes:
- What changed (split `admission/mod.rs` into 6 sibling submodules + tests.rs)
- Line-count before/after (1456 → ~25 shell + 7 files under threshold)
- Green test runs (agent library, state-machine conformance, lifecycle regression)
- Reference to the spec: `docs/issues/2026-04-17-agent-readability-refactor-plan.md#phase-a1`

Do NOT open the PR automatically; surface the command for the reviewer to run manually or ask before pushing if working with an exec-plan skill that requires confirmation.

---

## Rollback

If at any task the test suite goes red and cannot be returned to green within the task, roll back the branch to the last green commit:

```bash
git reset --hard HEAD~1
```

Do NOT attempt to fix behavior on the fly — this is a readability-only refactor. A persistent red test means a move lost information; the safe play is to revert the most recent move and re-approach.

## Notes for the Implementer

- **Do not add `pub(crate)` where `pub(super)` suffices.** Over-widening visibility is a soft way to leak internal structure.
- **Do not change test expectations.** Tests are the safety net for this refactor.
- **Do not merge two submodules "because it's easier".** The split is the goal.
- **Do not add new helpers or clean up unrelated code.** Save it for a separate PR.
- When `cargo check` complains about an unused import, delete it immediately — don't accumulate warnings.
- When a `pub(super)` cycle appears tricky (e.g. controller ↔ registry), Rust sibling modules can resolve it; the types just need to exist at compile time.
