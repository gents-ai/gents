# Event-Driven Tasks — PR 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split `ScheduledTask` into `Task` + `Schedule`, introduce the `TriggerEngine` + `TriggerSource` abstraction, wire the new collections through the existing reconcile loop (control watcher → DocumentRuntimeView → ActiveRuntimeSnapshot), and retarget today's `Scheduler::run()` to drive the engine with a `ScheduleSource`. Cron behavior is unchanged from the outside.

**Architecture:** Each new collection joins the seven existing operator-controlled collections following the same patterns (CLI `Desired*` struct + mixed apply/runtime-owned fields in one GraphQL type + inclusion in DocumentRuntimeView and ResolvedRuntimeSnapshot). The runtime no longer polls `ScheduledTask` directly — the `ScheduleSource` consumes `snapshot.active_schedules()` and reads runtime-owned `next_run_at` per tick. Templates render via MiniJinja with strict-undefined behavior. Lineage fields on `AgentRequest` (`caused_by_trigger_id`, `caused_by_trigger_kind`) enable tuple-matched in-flight queries for concurrency modes.

**Tech Stack:** Rust (workspace), DefraDB (embedded + GraphQL), MiniJinja (new dep), Lean 4 (proofs), tokio async, existing tracing / serde stack.

**Scope:** This plan covers PR 1 only. PR 2 (EventTrigger + EventSource) and PR 3 (ManualSource + operator ergonomics) get separate plans once PR 1 is landing.

**Related:**
- Spec: `docs/superpowers/specs/2026-04-21-event-driven-tasks-design.md`
- Issue: sourcenetwork/defra-agent#49
- Spec dependency (informational, not blocking): `docs/superpowers/specs/2026-04-14-apply-reconcile-lean.md` and #53

---

## Worktree setup

- [ ] **Create worktree for PR 1 work**

```bash
git worktree add ../defra-agent-event-driven-tasks-pr1 -b event-driven-tasks-pr1
cd ../defra-agent-event-driven-tasks-pr1
```

Execute every subsequent task in that worktree.

---

## Phase 1 — Schemas

### Task 1: Add `Task` schema

**Files:**
- Create: `crates/defra-agent-protocol/schemas/agent/task.graphql`

- [ ] **Step 1: Write the schema**

```graphql
type Task @branchable {
    task_id: String @index(unique: true)
    name: String @index
    description: String
    behavior_id: String @index
    prompt_template: String
    enabled: Boolean @index
    output_schema_ref: String
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
}
```

- [ ] **Step 2: Register the schema in the protocol bundle**

Find the file that `include_str!`s schema files (grep for `scheduled_task.graphql` in `crates/defra-agent-protocol/src/`). Add `task.graphql` following the same pattern.

- [ ] **Step 3: Build to verify schema loads**

Run: `cargo check -p defra-agent-protocol`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-protocol/
git commit -m "feat(protocol): add Task schema"
```

### Task 2: Add `Schedule` schema

**Files:**
- Create: `crates/defra-agent-protocol/schemas/agent/schedule.graphql`

- [ ] **Step 1: Write the schema**

```graphql
type Schedule @branchable {
    schedule_id: String @index(unique: true)
    task_id: String @index
    interval_secs: Int
    enabled: Boolean @index
    concurrency: String @index
    next_run_at: DateTime @index(direction: ASC)
    last_attempt_at: DateTime @index(direction: DESC)
    last_status: String @index
    last_error: String
    fire_count: Int
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
}
```

- [ ] **Step 2: Register in protocol bundle** (same file as Task 1 step 2)

- [ ] **Step 3: Build**: `cargo check -p defra-agent-protocol` → clean.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-protocol/
git commit -m "feat(protocol): add Schedule schema"
```

### Task 3: Remove `ScheduledTask` schema

**Files:**
- Delete: `crates/defra-agent-protocol/schemas/agent/scheduled_task.graphql`
- Modify: the protocol bundle registration file from Task 1

- [ ] **Step 1: Grep for all refs** to `ScheduledTask`, `scheduled_task`, `ScheduledTaskDoc`:

```bash
rg -l "ScheduledTask|scheduled_task" crates/
```

Expected matches: scheduler module, desired_state (possibly), document_view, CLI, desktop. Capture the list — subsequent tasks touch them.

- [ ] **Step 2: Remove the registration line** from the protocol bundle.

- [ ] **Step 3: Delete the .graphql file**:

```bash
git rm crates/defra-agent-protocol/schemas/agent/scheduled_task.graphql
```

- [ ] **Step 4: Commit (build will still break — fixed by Phases 2–6)**

```bash
git add crates/defra-agent-protocol/
git commit -m "feat(protocol): remove ScheduledTask schema (replaced by Task + Schedule)"
```

The workspace build will fail until the runtime is retargeted. That's expected and drives the remaining phases.

---

## Phase 2 — `AgentRequest` lineage fields

### Task 4: Add lineage fields to AgentRequest schema

**Files:**
- Modify: `crates/defra-agent-protocol/schemas/agent/agent_request.graphql`

- [ ] **Step 1: Read the current schema** to see existing field layout.

- [ ] **Step 2: Add the two fields** near the other observability fields:

```graphql
caused_by_trigger_id: String @index
caused_by_trigger_kind: String @index
```

- [ ] **Step 3: Build**: `cargo check -p defra-agent-protocol` → clean.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-protocol/
git commit -m "feat(protocol): add caused_by_trigger_{id,kind} to AgentRequest"
```

### Task 5: Plumb lineage fields through the Rust AgentRequest struct

**Files:**
- Modify: `crates/defra-agent/src/lifecycle/materialize.rs` (at minimum)
- Modify: any struct mirroring AgentRequest fields (grep for a `struct AgentRequest` or similar Rust mirror)

- [ ] **Step 1: Grep for Rust definitions** mirroring the AgentRequest schema:

```bash
rg "caused_by|execution_origin" crates/defra-agent/src/ -l
```

- [ ] **Step 2: Add `caused_by_trigger_id: Option<String>` and `caused_by_trigger_kind: Option<String>`** to every struct representing an AgentRequest insert payload.

- [ ] **Step 3: Plumb them into the materialize call path.** `materialize_claimed_with_execution_binding` takes parameters describing the request shape; add two parameters (or a single `TriggerLineage` struct) carrying these values. Default to `None` at every existing call site for now.

- [ ] **Step 4: Build**: `cargo check -p defra-agent` (ignore scheduler errors from Task 3).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/
git commit -m "feat(lifecycle): accept trigger lineage at materialize time"
```

### Task 6: Unit test — materialized request carries lineage

**Files:**
- Modify: `crates/defra-agent/src/lifecycle/materialize.rs` (add a `tests.rs` module if one doesn't exist) OR the nearest existing lifecycle test module.

- [ ] **Step 1: Write a failing test** asserting that when `materialize_claimed_with_execution_binding` is called with `trigger_id = Some("sched-1")`, `trigger_kind = Some("schedule")`, the returned/persisted AgentRequest has those values.

- [ ] **Step 2: Run to verify it fails** (the plumbing drops the values on the floor today because Task 5 defaulted everything to None). Fix by threading the values through to the insert payload.

- [ ] **Step 3: Re-run; commit.**

---

## Phase 3 — CLI `Desired*` structs and apply/diff plumbing

For every task in Phase 3, **mirror the pattern of `DesiredAgentBehavior`** (see `crates/defra-agent-cli/src/desired_state/mod.rs:36-48` for the struct and grep for `DesiredAgentBehavior` to find all the diff/convert/normalize/validate sites that need parallel branches).

### Task 7: `DesiredTask` struct

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/mod.rs`

- [ ] **Step 1: Add the struct**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesiredTask {
    pub(crate) task_id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) behavior_id: String,
    pub(crate) prompt_template: String,
    pub(crate) enabled: bool,
    pub(crate) output_schema_ref: Option<String>,
}
```

- [ ] **Step 2: Build**: `cargo check -p defra-agent-cli` → clean (struct unused warning is OK for now).

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent-cli/
git commit -m "feat(cli): add DesiredTask manifest struct"
```

### Task 8: `DesiredSchedule` struct

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/mod.rs`

- [ ] **Step 1: Add**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesiredSchedule {
    pub(crate) schedule_id: String,
    pub(crate) task_id: String,
    pub(crate) interval_secs: i64,
    pub(crate) enabled: bool,
    pub(crate) concurrency: String,  // "parallel" | "serial" | "latest_only"
}
```

- [ ] **Step 2: Build + commit**

```bash
cargo check -p defra-agent-cli
git add crates/defra-agent-cli/
git commit -m "feat(cli): add DesiredSchedule manifest struct"
```

### Task 9: Remove `DesiredScheduledTask` (if present) and any references

- [ ] **Step 1: Grep**: `rg DesiredScheduledTask crates/defra-agent-cli/`

- [ ] **Step 2: Delete the struct and every reference.** Callers that produced `ApplyStep::CreateScheduledTask` / `UpdateScheduledTask` are replaced in subsequent tasks.

- [ ] **Step 3: Commit**

```bash
git add crates/defra-agent-cli/
git commit -m "refactor(cli): remove DesiredScheduledTask (superseded by Task + Schedule)"
```

### Task 10: Add load branches for Task and Schedule

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/load.rs` (and/or wherever manifest parsing is keyed off collection name)

- [ ] **Step 1: Test first** — write a test that loads a manifest containing a `task:` section and a `schedule:` section and returns populated `Vec<DesiredTask>` / `Vec<DesiredSchedule>`. Place it in `desired_state/tests.rs` next to the existing manifest-load tests (grep `tests.rs` for an existing behavior-manifest test as a template).

- [ ] **Step 2: Run to verify failure** — expected: unknown-key or missing-branch error.

- [ ] **Step 3: Add the load branches** following the existing behavior-load pattern.

- [ ] **Step 4: Re-run: pass. Commit.**

### Task 11: Add diff branches for Task and Schedule

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/diff.rs`

- [ ] **Step 1: Write a test** — a manifest with one `DesiredTask` against an empty live state should produce exactly one `create` step for collection `Task`. Mirror the test style already in `desired_state/tests.rs`.

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Add diff branches** following behavior-diff pattern. Remember diff produces `create | update | unchanged | live_only` buckets.

- [ ] **Step 4: Write a second test** for `DesiredSchedule` diff.

- [ ] **Step 5: Implement.**

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent-cli/
git commit -m "feat(cli): diff-manifests support for Task and Schedule"
```

### Task 12: Add convert / normalize / validate branches

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/convert.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/normalize.rs`
- Modify: `crates/defra-agent-cli/src/desired_state/validate.rs`

- [ ] **Step 1: Read the existing AgentBehavior branches** in each file to see the pattern (export_bundle → manifest, field normalization defaults, validation rules like "behavior_id is non-empty").

- [ ] **Step 2: Write validation tests** for Task and Schedule — a bad manifest (empty `task_id`, `interval_secs < 1`, unknown `concurrency` enum value) should error with a clear message.

- [ ] **Step 3: Implement** the convert + normalize + validate branches mirroring the behavior pattern.

- [ ] **Step 4: Tests pass. Commit.**

### Task 13: Add apply-step write branches (DefraDB GraphQL `create`/`update` mutations for Task and Schedule)

**Files:**
- Modify: wherever `ApplyStep` variants are turned into DefraDB writes (grep for `ApplyStep` in `crates/defra-agent-cli/src/`).

- [ ] **Step 1: Trace ScheduledTask's existing apply path** (if still present) to understand the pattern. If removed by Task 3, trace AgentBehavior's apply path.

- [ ] **Step 2: Add branches** for `ApplyStep::CreateTask`, `UpdateTask`, `CreateSchedule`, `UpdateSchedule`. Apply writes only apply-owned fields — Schedule's runtime-owned fields (`next_run_at`, `last_attempt_at`, `last_status`, `last_error`, `fire_count`) are never touched by apply.

- [ ] **Step 3: Integration smoke test** — CLI e2e test `apply` on a manifest with one task + one schedule, then query Task and Schedule collections, assert docs present with correct fields.

- [ ] **Step 4: Commit.**

---

## Phase 4 — DocumentRuntimeView + snapshot extensions

### Task 14: Read the existing DocumentRuntimeView

- [ ] **Step 1:** Read `crates/defra-agent/src/agent/document_view/mod.rs` and `load.rs` end to end. Note how AgentBehavior is loaded — you will mirror the pattern.

### Task 15: Extend `DocumentRuntimeView` struct with `tasks` and `schedules` fields

**Files:**
- Modify: `crates/defra-agent/src/agent/document_view/mod.rs` (or wherever the struct is defined)

- [ ] **Step 1:** Add two fields of the form `HashMap<String, Task>` and `HashMap<String, Schedule>` (using whatever concrete doc type the existing code uses for AgentBehavior).

- [ ] **Step 2:** Add a stub `event_triggers: HashMap<String, EventTrigger>` — empty map, populated in PR 2. This avoids a breaking-change diff in PR 2.

- [ ] **Step 3: Build + commit** (unused field warning is expected; next task uses them).

### Task 16: Extend `load_document_runtime_view()` to populate Task and Schedule

**Files:**
- Modify: `crates/defra-agent/src/agent/document_view/load.rs`

- [ ] **Step 1: Test first** — given a DefraDB state with 2 Task docs and 1 Schedule doc, `load_document_runtime_view()` returns a view with `tasks.len() == 2` and `schedules.len() == 1`. Place in `document_view/tests.rs`.

- [ ] **Step 2: Implement** the query + map-build mirroring the AgentBehavior load block.

- [ ] **Step 3: Test passes. Commit.**

### Task 17: Add `ResolvedTask` + `ResolvedSchedule` types

**Files:**
- Modify: `crates/defra-agent/src/agent/document_view/snapshot.rs` (or wherever `ResolvedRuntimeSnapshot` lives — grep `ResolvedRuntimeSnapshot`)

- [ ] **Step 1:** Define:

```rust
#[derive(Debug, Clone)]
pub struct ResolvedTask {
    pub task_id: String,
    pub behavior_id: String,
    pub prompt_template: String,
    pub output_schema_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedSchedule {
    pub schedule_id: String,
    pub task_id: String,
    pub task: ResolvedTask,
    pub interval_secs: i64,
    pub enabled: bool,
    pub concurrency: ConcurrencyMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConcurrencyMode { Parallel, Serial, LatestOnly }

impl ConcurrencyMode {
    pub fn parse(s: &str) -> Option<Self> { /* match on "parallel"/"serial"/"latest_only" */ }
}
```

- [ ] **Step 2:** Extend `ResolvedRuntimeSnapshot` with `active_schedules: HashMap<String, ResolvedSchedule>` and `unavailable_schedules: HashSet<String>`. Also stub `active_event_triggers: HashMap<_, _>` as empty for PR 2.

- [ ] **Step 3: Build + commit.**

### Task 18: Extend `resolve_document_runtime_snapshot_from_view` to build schedule maps

**Files:**
- Modify: `crates/defra-agent/src/agent/document_view/snapshot.rs`

- [ ] **Step 1: Test first** — given a view with 1 Task and 1 Schedule referencing it, resolve produces `active_schedules` of size 1. Given a Schedule whose `task_id` doesn't resolve (task missing or disabled), the schedule lands in `unavailable_schedules`.

- [ ] **Step 2: Implement.** Mirror existing behavior-resolution logic for the "referenced entity disabled/missing → unavailable" decision.

- [ ] **Step 3: Commit.**

### Task 19: Extend `ActiveRuntimeSnapshot` with `active_schedules()` accessor

**Files:**
- Modify: `crates/defra-agent/src/runtime_snapshot.rs` (or wherever `ActiveRuntimeSnapshot` is defined)

- [ ] **Step 1:** Add a public method `pub fn active_schedules(&self) -> &HashMap<String, ResolvedSchedule>`. Also stub `active_event_triggers(&self) -> &HashMap<_, _>` for PR 2.

- [ ] **Step 2: Build + commit.**

### Task 20: Integration test — snapshot generation bump on Schedule change

**Files:**
- Modify: whichever existing integration test exercises generation bumps (grep for `generation` in `crates/defra-agent/tests/`)

- [ ] **Step 1:** Following the existing pattern for behavior-change generation bumps, write a test that:
  1. Stands up a DefraDB node, initializes an empty snapshot (generation 0).
  2. Applies a manifest with one Task + one Schedule.
  3. Waits for the control watcher to debounce + publish.
  4. Asserts the new `ActiveRuntimeSnapshot` has `active_schedules.len() == 1` and `generation > 0`.

- [ ] **Step 2: Run — expected failure** (control watcher doesn't subscribe to Task/Schedule yet).

- [ ] **Step 3:** Implement in Task 21 and re-run.

### Task 21: Extend `control_watcher.rs` subscription set

**Files:**
- Modify: `crates/defra-agent/src/agent/runtime/control_watcher.rs`

- [ ] **Step 1: Read the current file** — note the existing subscription set (behaviors, backends, etc.).

- [ ] **Step 2:** Add `Task` and `Schedule` to the subscribed collection set. Also stub `EventTrigger` for PR 2 (the stub empty-map from Task 15 means subscribing to it today is a no-op; alternatively, defer the EventTrigger subscription to PR 2).

- [ ] **Step 3: Re-run Task 20's integration test — should pass.**

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/
git commit -m "feat(runtime): wire Task + Schedule into reconcile loop"
```

---

## Phase 5 — Template engine (MiniJinja)

### Task 22: Add MiniJinja dependency

**Files:**
- Modify: workspace root `Cargo.toml` (`[workspace.dependencies]`)
- Modify: `crates/defra-agent/Cargo.toml`

- [ ] **Step 1:** Add:

```toml
# workspace root Cargo.toml
minijinja = { version = "2", default-features = false, features = ["builtins"] }
```

- [ ] **Step 2:** Reference it in `crates/defra-agent/Cargo.toml` under `[dependencies]`:

```toml
minijinja = { workspace = true }
```

- [ ] **Step 3: Build**: `cargo check` at workspace root → clean.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/defra-agent/Cargo.toml Cargo.lock
git commit -m "build: add minijinja template engine dependency"
```

### Task 23: Create template module skeleton

**Files:**
- Create: `crates/defra-agent/src/template/mod.rs`
- Create: `crates/defra-agent/src/template/tests.rs`
- Modify: `crates/defra-agent/src/lib.rs` (or whichever mod declares siblings)

- [ ] **Step 1:** Declare the module. Define:

```rust
pub struct TemplateScope {
    pub event: serde_json::Value,
    pub doc: Option<serde_json::Value>,
    pub args: Option<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("template parse error: {0}")] Parse(String),
    #[error("template render error: {0}")] Render(String),
    #[error("rendered output exceeds size cap ({0} bytes)")] SizeCap(usize),
}

pub const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
pub const MAX_RENDERED_BYTES: usize = 1024 * 1024;
```

- [ ] **Step 2: Commit.**

### Task 24: Implement `render_template(template, scope) -> Result<String, TemplateError>`

**Files:**
- Modify: `crates/defra-agent/src/template/mod.rs`
- Modify: `crates/defra-agent/src/template/tests.rs`

- [ ] **Step 1: Test first:**

```rust
#[test]
fn renders_event_var() {
    let scope = TemplateScope {
        event: serde_json::json!({"fired_at": "2026-04-21T00:00:00Z", "trigger_kind": "schedule"}),
        doc: None, args: None,
    };
    let out = render_template("fired at {{ event.fired_at }}", &scope).unwrap();
    assert_eq!(out, "fired at 2026-04-21T00:00:00Z");
}

#[test]
fn strict_undefined_errors_on_missing_var() {
    let scope = TemplateScope { event: serde_json::json!({}), doc: None, args: None };
    let err = render_template("{{ event.missing }}", &scope).unwrap_err();
    assert!(matches!(err, TemplateError::Render(_)));
}

#[test]
fn enforces_rendered_size_cap() { /* construct a template whose output exceeds MAX_RENDERED_BYTES */ }
```

- [ ] **Step 2: Run to verify failures.**

- [ ] **Step 3: Implement** using `minijinja::Environment` with `set_undefined_behavior(UndefinedBehavior::Strict)` and `set_auto_escape_callback(|_| AutoEscape::None)`. Merge the scope into a context object keyed on `event`/`doc`/`args`. Enforce `MAX_TEMPLATE_BYTES` on input and `MAX_RENDERED_BYTES` on output.

- [ ] **Step 4: Tests pass. Commit.**

```bash
git add crates/defra-agent/src/template/ crates/defra-agent/src/lib.rs
git commit -m "feat(template): minijinja-based renderer with strict undefined + size caps"
```

### Task 25: Add `parse_template_for_validation(template) -> Result<Vec<VariableRef>, TemplateError>`

**Files:**
- Modify: `crates/defra-agent/src/template/mod.rs`

- [ ] **Step 1: Test first** — given template `"{{ event.fired_at }} {{ doc.customer.name }}"`, returns two `VariableRef` values with paths `["event", "fired_at"]` and `["doc", "customer", "name"]`.

- [ ] **Step 2: Implement** using MiniJinja's parser + visitor API (walk the AST collecting `Expr::GetAttr` chains). If MiniJinja's public API doesn't expose enough, fall back to rendering with a probe scope that records every path accessed.

- [ ] **Step 3: Commit.**

### Task 26: Apply-time template validation for Schedule

**Files:**
- Modify: `crates/defra-agent-cli/src/desired_state/validate.rs`

- [ ] **Step 1: Test** — a manifest with a Task whose template references `{{ doc.foo }}`, linked to a Schedule, fails validation with a message saying schedule scope does not permit `doc.*`.

- [ ] **Step 2: Implement** — when validating a Schedule, load its referenced Task, call `parse_template_for_validation`, reject any `doc.*` or `args.*` root.

- [ ] **Step 3: Commit.**

---

## Phase 6 — `TriggerEngine` + `TriggerSource`

### Task 27: Create trigger-engine module skeleton

**Files:**
- Create: `crates/defra-agent/src/trigger_engine/mod.rs`
- Create: `crates/defra-agent/src/trigger_engine/tests.rs`

- [ ] **Step 1:** Define public types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind { Schedule, Event, Manual }

impl TriggerKind {
    pub fn as_str(self) -> &'static str { /* "schedule" | "event" | "manual" */ }
}

pub struct FireIntent {
    pub trigger_id: Option<String>,
    pub trigger_kind: TriggerKind,
    pub task: crate::agent::document_view::snapshot::ResolvedTask,
    pub concurrency: crate::agent::document_view::snapshot::ConcurrencyMode,
    pub event_vars: serde_json::Value,
    pub doc_vars: Option<serde_json::Value>,
    pub args_vars: Option<serde_json::Value>,
    pub on_result: Box<dyn FnOnce(FireResult) + Send>,
}

#[derive(Debug, Clone)]
pub enum FireResult {
    Fired { request_id: String },
    Skipped { reason: String },
    Errored { error: String },
}

pub trait TriggerSource: Send + Sync {
    fn next_fire(&mut self)
        -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>>;
}
```

- [ ] **Step 2: Build + commit.**

### Task 28: Fire-attempt-status enum

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/mod.rs`

- [ ] **Step 1: Add**:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireAttemptStatus { Fired, Skipped, Errored }

impl FireAttemptStatus {
    pub fn as_str(self) -> &'static str { /* "fired" | "skipped" | "error" */ }
}
```

- [ ] **Step 2: Commit.**

### Task 29: `TriggerEngine` struct + `run()` scaffold

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/mod.rs`

- [ ] **Step 1:** Define:

```rust
pub struct TriggerEngine {
    snapshot: Arc<tokio::sync::RwLock<ActiveRuntimeSnapshot>>,
    materializer: MaterializerHandle, // whatever the existing materialize-and-claim handle type is
    per_trigger_locks: Mutex<HashMap<(String, TriggerKind), Arc<tokio::sync::Mutex<()>>>>,
}

impl TriggerEngine {
    pub fn new(snapshot: Arc<_>, materializer: MaterializerHandle) -> Self { /* */ }

    pub async fn run(self, mut sources: Vec<Box<dyn TriggerSource>>, cancel: CancellationToken) {
        // use FuturesUnordered over sources, funnel into one dispatch method
    }

    async fn dispatch(&self, intent: FireIntent) -> FireResult { /* see Task 30-32 */ }
}
```

Implementation details (write stubs; implement in later tasks):
- `run()` drives sources via `FuturesUnordered` or `select_all`.
- `dispatch()` performs steps 1–5 from the spec: enabled gate, render, concurrency check, materialize, callback.

- [ ] **Step 2: Build + commit.**

### Task 30: `dispatch` — enabled gate + render

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/mod.rs`
- Modify: `crates/defra-agent/src/trigger_engine/tests.rs`

- [ ] **Step 1: Test** — a FireIntent whose trigger_id is not in `snapshot.active_schedules()` (e.g., disabled) returns `FireResult::Skipped { reason: "trigger disabled" }`. Use a mock `MaterializerHandle` and hand-built snapshot.

- [ ] **Step 2: Test** — a FireIntent whose task template references `{{ event.fired_at }}` with `event_vars = {"fired_at": "..."}` produces the rendered prompt (capture it via a spy materializer).

- [ ] **Step 3: Implement** — load from snapshot, check enabled, call `render_template`, pass rendered content to materializer.

- [ ] **Step 4: Tests pass. Commit.**

### Task 31: `dispatch` — concurrency: parallel + serial

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/mod.rs`
- Modify: `crates/defra-agent/src/trigger_engine/tests.rs`

- [ ] **Step 1: Test (parallel)** — two `FireIntent`s in parallel concurrency, no in-flight, both materialize. Assert two requests created.

- [ ] **Step 2: Test (serial — no in-flight)** — one FireIntent, serial mode, no in-flight. Fire materializes; returns `Fired`.

- [ ] **Step 3: Test (serial — in-flight exists)** — spy materializer pre-populated with a non-terminal AgentRequest having `(caused_by_trigger_id = "sched-1", caused_by_trigger_kind = "schedule")`. FireIntent for the same trigger returns `Skipped { reason: "serial: prior fire still in-flight" }`; materializer called zero additional times.

- [ ] **Step 4: Implement** the in-flight query via the materializer handle (new method: `has_nonterminal_request_for_trigger(trigger_id, trigger_kind) -> bool`). Remember: match on the TUPLE, not trigger_id alone.

- [ ] **Step 5: Tests pass. Commit.**

### Task 32: `dispatch` — concurrency: latest_only with per-trigger lock

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/mod.rs`
- Modify: `crates/defra-agent/src/trigger_engine/tests.rs`

- [ ] **Step 1: Test** — latest_only mode, one in-flight prior AgentRequest for the same trigger. FireIntent fires: materializer receives a supersede call for the prior request, then a new materialization. Both happen inside a held lock (verified by injecting a delay between supersede and materialize and asserting a parallel second fire for the same trigger waits).

- [ ] **Step 2: Implement** — acquire the per-trigger `Arc<Mutex<()>>` (create on-demand in `per_trigger_locks`); inside the lock: supersede all non-terminal matches, then materialize.

- [ ] **Step 3: Commit.**

### Task 33: `dispatch` — handle template render failure

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/mod.rs`
- Modify: `crates/defra-agent/src/trigger_engine/tests.rs`

- [ ] **Step 1: Test** — FireIntent with a template that references an undefined variable. Dispatch returns `FireResult::Errored { error: <engine message> }`; no materialization happens; callback is invoked with Errored.

- [ ] **Step 2: Implement.** Wrap the `render_template` call; on Err, build `FireResult::Errored` and invoke `on_result`.

- [ ] **Step 3: Commit.**

---

## Phase 7 — `ScheduleSource`

### Task 34: `ScheduleSource` skeleton

**Files:**
- Create: `crates/defra-agent/src/trigger_engine/schedule_source.rs`

- [ ] **Step 1:** Define:

```rust
pub struct ScheduleSource {
    snapshot: Arc<tokio::sync::RwLock<ActiveRuntimeSnapshot>>,
    node: EmbeddedNodeHandle,      // whatever the existing DefraDB handle type is
    tick_every: Duration,          // 1s default
    cancel: CancellationToken,
}

impl ScheduleSource {
    pub fn new(snapshot: Arc<_>, node: _, cancel: CancellationToken) -> Self { /* default 1s */ }
}

impl TriggerSource for ScheduleSource {
    fn next_fire(&mut self) -> Pin<Box<dyn Future<Output = Option<FireIntent>> + Send + '_>> { /* Task 35 */ }
}
```

- [ ] **Step 2: Commit.**

### Task 35: `ScheduleSource::next_fire` — tick + query due + emit intent

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/schedule_source.rs`
- Modify: `crates/defra-agent/src/trigger_engine/tests.rs`

- [ ] **Step 1: Test** — snapshot contains one active schedule `sched-1` referencing task `task-1`; the schedule's DB row has `next_run_at` 1 second in the past. Call `next_fire()`. Assert it returns `Some(FireIntent { trigger_id: Some("sched-1"), trigger_kind: Schedule, ... })` within 2 seconds.

- [ ] **Step 2: Implement**:
  1. Sleep for `tick_every` (or until cancelled).
  2. For each schedule in `snapshot.read().await.active_schedules()`:
     - Query the Schedule doc by `schedule_id` to get the current `next_run_at` (runtime-owned field; snapshot's copy is stale).
     - If `next_run_at <= now`, build FireIntent:
       - `trigger_id = Some(schedule_id)`, `trigger_kind = Schedule`
       - `task = resolved_schedule.task.clone()`
       - `concurrency = resolved_schedule.concurrency`
       - `event_vars = { fired_at: now, trigger_id: schedule_id, trigger_kind: "schedule" }`
       - `doc_vars = None`, `args_vars = None`
       - `on_result = Box::new(move |result| { /* write runtime fields; see Task 36 */ })`
     - Return the first one. Emit others on subsequent ticks (don't batch).

- [ ] **Step 3: Commit.**

### Task 36: `on_result` callback — writes Schedule runtime fields

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/schedule_source.rs`

- [ ] **Step 1: Test** — after a successful fire, the Schedule doc has `last_status = "fired"`, `fire_count` incremented, `last_attempt_at` updated, `next_run_at += interval_secs`. After a skipped fire, `last_status = "skipped"`, `fire_count` unchanged, `next_run_at` still advances.

- [ ] **Step 2: Implement** the callback as an update mutation on the Schedule document. Writes only runtime-owned fields — never `enabled`, `interval_secs`, `task_id`, `concurrency`.

- [ ] **Step 3: Commit.**

### Task 37: Cancellation + graceful shutdown

**Files:**
- Modify: `crates/defra-agent/src/trigger_engine/schedule_source.rs`

- [ ] **Step 1: Test** — starting a ScheduleSource, dropping its `cancel` token, and then calling `next_fire()` returns `None` promptly.

- [ ] **Step 2: Implement** — check the token at every tick boundary.

- [ ] **Step 3: Commit.**

---

## Phase 8 — Retarget `Scheduler::run` call site

### Task 38: Survey current call site

- [ ] **Step 1:** Open `crates/defra-agent/src/scheduler/loop_impl.rs` and its callers (grep `Scheduler::run` / `scheduler::run`). Identify the function that spawns the scheduler at agent bootstrap — likely in `agent/reconcile.rs` or a bootstrap module.

- [ ] **Step 2: Write down** the existing signature + what it reads (snapshot, node, cancel token).

### Task 39: Replace Scheduler::run with TriggerEngine + ScheduleSource

**Files:**
- Modify: the bootstrap site identified in Task 38
- Delete (or stub): `crates/defra-agent/src/scheduler/loop_impl.rs`, `execution.rs` (retain tests that still apply; consolidate under the new engine)

- [ ] **Step 1: Write an integration test** asserting that, given a live snapshot with one Schedule whose `next_run_at` is due, an `AgentRequest` with `caused_by_trigger_id = <schedule_id>` and `caused_by_trigger_kind = "schedule"` materializes within N seconds. Place in `crates/defra-agent/tests/`.

- [ ] **Step 2: At the bootstrap site**, replace `Scheduler::run(snapshot, node, cancel).await` with:

```rust
let source = Box::new(ScheduleSource::new(snapshot.clone(), node.clone(), cancel.clone()));
let engine = TriggerEngine::new(snapshot, materializer);
engine.run(vec![source], cancel).await;
```

- [ ] **Step 3: Delete the old scheduler modules** (or gut them to the point of being no-ops). Retain test cases that cover behaviors now living under the engine; move them.

- [ ] **Step 4: Integration test passes. Commit.**

```bash
git add crates/defra-agent/
git commit -m "feat(runtime): retarget scheduler bootstrap to TriggerEngine + ScheduleSource"
```

### Task 40: Clean up `ScheduledTask`-shaped CLI & desktop references

- [ ] **Step 1:** Grep `rg ScheduledTask crates/`. Anything remaining is cleanup debt.

- [ ] **Step 2:** For each hit: rename / retarget to `Task` + `Schedule` as appropriate. Desktop UI refs are deferred to Phase 11; everything else lands here.

- [ ] **Step 3: `cargo check` across the whole workspace → clean.**

- [ ] **Step 4: Commit.**

---

## Phase 9 — Lean proofs

### Task 41: Read existing scheduler proof

- [ ] **Step 1:** Open `crates/defra-agent/proofs/Proofs/Scheduling.lean` and `RuntimeReconcile.lean`. Note the vocabulary: `ExecutionOrigin`, `AdmissionState`, `SchedulerState`, snapshot shape.

### Task 42: Create `Triggers.lean` module skeleton

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Triggers.lean`
- Modify: `crates/defra-agent/proofs/Proofs.lean` (register the new module)

- [ ] **Step 1:** Import Basic, Scheduling, RuntimeReconcile. Define:

```lean
inductive TriggerKind | schedule | event | manual

inductive ConcurrencyMode | parallel | serial | latestOnly

structure FireIntent where
  triggerId : Option String
  triggerKind : TriggerKind
  taskId : String
  concurrency : ConcurrencyMode
  -- render inputs abstracted away

structure RequestSeed where
  causedByTriggerId : Option String
  causedByTriggerKind : TriggerKind

def dispatch (snap : ActiveRuntimeSnapshot) (intent : FireIntent) : Option RequestSeed :=
  sorry -- filled in subsequent tasks
```

- [ ] **Step 2: `lake build` in `proofs/` — expected: clean (sorry allowed).**

- [ ] **Step 3: Commit.**

### Task 43: Theorem T1 (enabled gate)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Triggers.lean`

- [ ] **Step 1:** State:

```lean
theorem T1_enabled_gate (snap : ActiveRuntimeSnapshot) (intent : FireIntent) :
    dispatch snap intent = some seed →
    (intent.triggerKind = .schedule →
      ∃ sched, snap.activeSchedules.contains intent.triggerId ∧ sched.enabled = true) ∧
    (intent.triggerKind = .event →
      ∃ trig, snap.activeEventTriggers.contains intent.triggerId ∧ trig.enabled = true)
```

- [ ] **Step 2: Prove** — unfold `dispatch`, case on `triggerKind`, use `enabled` check.

- [ ] **Step 3: Commit.**

### Task 44: Theorem T2 (serial at-most-one)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Triggers.lean`

- [ ] **Step 1:** State T2 over a `SystemState` that carries a set of AgentRequests:

```lean
theorem T2_serial_at_most_one (s : SystemState) (t : (String × TriggerKind)) :
    (∀ r ∈ s.requests, r.causedBy = some t → r.concurrency = .serial) →
    (s.requests.filter (fun r =>
        r.causedBy = some t ∧ ¬r.isTerminal)).card ≤ 1
```

- [ ] **Step 2: Prove** — relies on the in-flight check preventing simultaneous non-terminal pairs + S1 irreversibility.

- [ ] **Step 3: Commit.**

### Task 45: Theorem T3 (latest_only convergence)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Triggers.lean`

- [ ] **Step 1:** State T3 — after a `latestOnly` fire materializes `r_new`, all prior `r_prior` with matching `causedBy` reach `Superseded` (terminal).

- [ ] **Step 2: Prove** — uses the per-trigger lock invariant (modeled as serialization of supersede-then-materialize pairs).

- [ ] **Step 3: Commit.**

### Task 46: Theorem T4 (lineage completeness)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Triggers.lean`

- [ ] **Step 1:** State T4 — every materialized `RequestSeed.causedBy` tuple is consistent with its `executionOrigin`.

- [ ] **Step 2: Prove** — follows from `dispatch`'s construction.

- [ ] **Step 3: Commit.**

---

## Phase 10 — Conformance tests

### Task 47: `tests/schedule_conformance.rs`

**Files:**
- Create: `crates/defra-agent/tests/schedule_conformance.rs`

- [ ] **Step 1:** Write the test file. Cases to cover, each as its own `#[tokio::test]`:
  - `fires_at_next_run_at`
  - `enabled_false_does_not_fire`
  - `template_render_failure_records_error_status`
  - `serial_skips_when_prior_non_terminal`
  - `serial_advances_next_run_at_on_skip`
  - `latest_only_supersedes_prior_fire`
  - `generation_bump_reconfigures_active_schedules`

- [ ] **Step 2: Follow** the existing `state_machine_conformance.rs` test-harness pattern (grep for `support::test_db()`).

- [ ] **Step 3: Commit.**

### Task 48: Extend `tests/state_machine_conformance.rs`

**Files:**
- Modify: `crates/defra-agent/tests/state_machine_conformance.rs`

- [ ] **Step 1:** Add cases covering the new transitions:
  - `serial_skip_does_not_create_request`
  - `latest_only_transition_to_superseded`
  - `fire_errored_does_not_create_request`

- [ ] **Step 2: Commit.**

### Task 49: Soak-test extension (optional but recommended)

**Files:**
- Modify: wherever the existing soak test lives (grep `soak` in `tests/`)

- [ ] **Step 1:** Extend the soak driver to exercise K schedules concurrently; assert `fire_count` monotone, no stuck non-terminal requests, snapshot generation bumps handled cleanly under edit-under-load.

- [ ] **Step 2: Commit.**

---

## Phase 11 — Desktop UI retargeting

### Task 50: Survey desktop ScheduledTask usage

- [ ] **Step 1:** `rg ScheduledTask crates/defra-agent-desktop/`. Expected: views/manage, state, manage/documents, manage/actions.

- [ ] **Step 2: Note** each file + what it does (list view, detail form, create form, etc.).

### Task 51: Retarget the manage-list view

**Files:**
- Modify: `crates/defra-agent-desktop/src/views/manage/entity_list.rs` (per git status, this file has pending changes; understand the state first)
- Modify: `crates/defra-agent-desktop/src/manage/documents.rs`

- [ ] **Step 1:** Replace ScheduledTask list queries with TWO parallel queries (Tasks and Schedules). Render them as two sections in the manage view.

- [ ] **Step 2:** Manual smoke test: run the desktop app with `cargo run -p defra-agent-desktop`, create a Task + Schedule via CLI apply, see both in the UI.

- [ ] **Step 3: Commit.**

### Task 52: Retarget the detail form

**Files:**
- Modify: `crates/defra-agent-desktop/src/manage/actions.rs`, `documents.rs`, and relevant view files

- [ ] **Step 1:** Replace single ScheduledTask edit form with one Task form and one Schedule form. Schedule form references task_id.

- [ ] **Step 2: Smoke test: edit a Schedule, observe the doc update.**

- [ ] **Step 3: Commit.**

### Task 53: Observability — surface fire bookkeeping

**Files:**
- Modify: desktop view(s) for Schedule detail

- [ ] **Step 1:** Show `last_attempt_at`, `last_status`, `last_error`, `fire_count` on the Schedule detail view.

- [ ] **Step 2: Smoke test.**

- [ ] **Step 3: Commit.**

---

## Phase 12 — Wrap up

### Task 54: Full workspace build + test

- [ ] **Step 1:** `cargo build --workspace` → clean.

- [ ] **Step 2:** `cargo test --workspace` → all pass.

- [ ] **Step 3:** `cd crates/defra-agent/proofs && lake build` → clean.

### Task 55: Docs

**Files:**
- Modify: `CLAUDE.md` (brief note under "Architecture") and/or `README.md`

- [ ] **Step 1:** Add 3–5 lines describing the Task/Schedule split + TriggerEngine, with a pointer to the spec.

- [ ] **Step 2: Commit.**

### Task 56: Open the PR

- [ ] **Step 1:** Push: `git push -u origin event-driven-tasks-pr1`.

- [ ] **Step 2:** `gh pr create` with title `feat: split ScheduledTask into Task + Schedule via TriggerEngine` and a body pointing at the spec + issue #49. Note in the body: "PR 2 (EventTrigger) and PR 3 (manual runs) are follow-ups."

---

## Non-goals for PR 1 (explicit)

These land in PR 2 or PR 3. Don't let scope creep drag them in:
- `EventTrigger` schema and `EventSource` — PR 2.
- `ManualSource` / `run_task_now` helper — PR 3.
- `args.*` template scope runtime usage (only the syntactic root is accepted today) — PR 3.
- Desktop polish for EventTrigger visibility — PR 2.
- `ApplyReconcile.lean` / `Collection` enum — tracked in #53.
