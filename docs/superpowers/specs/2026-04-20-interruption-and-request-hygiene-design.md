# Request Interruption and Freshness TTL Design

Adds two tightly-coupled production-hardening features to the request
lifecycle: **user-initiated mid-turn interruption** and **submitter-driven
freshness TTL**. Both are expressed as desired-state fields on `AgentRequest`;
the runtime observes them and drives matching lifecycle transitions.

This spec also closes the *external-request-driven cancellation* open item
reserved in `2026-04-15-concurrent-inference-admission-design.md` — the
`InferenceCall.cancelled` terminal that spec reserved gets its delivery
mechanism here.

## Problem

Two gaps in the current request lifecycle show up as soon as the system is
used in real, sometimes-offline, human-in-the-loop conditions:

1. **There is no way to interrupt a turn mid-flight.** All four existing
   terminals (`completed`, `failed`, `superseded`, `dead`) model outcomes
   the *runtime* decides on. `superseded` looks like a candidate but is
   reachable only from `pending` (via the `dedup_lose` transition — a newer
   request displaces an unclaimed older one). Once a request reaches
   `claimed`, `processing`, or `inputRequired`, nothing short of failure or
   timeout takes it to a terminal. Users that want to steer mid-stream
   ("stop, I meant something else") have no lever.

2. **Stale offline submissions cause thundering herds on reconnect.** When
   a client has been offline and comes back online, DefraDB replicates its
   locally-written requests into the runtime. The runtime currently treats
   each one as fresh and eligible for claim, regardless of how old the
   submitter's intent is. A user who queued up (accidentally or otherwise)
   twenty requests before rejoining the network gets twenty concurrent
   inference turns, none of which they may still want.

Both problems share a substrate: **submitter intent over time is not part
of the current request model.** The submitter's "I want this to run" and
"I no longer want this to run" are both missing. Adding them is the
feature.

The state machine is already the project's source of truth for lifecycle
behavior (`CLAUDE.md`: *"The Lean proofs are the source of truth for all
state machine behavior"*), so the design is Lean-first.

## Scope

In scope:

- A ninth `RequestState` value, `interrupted`, terminal. Reachable from
  every non-terminal state (`pending`, `claimed`, `processing`,
  `inputRequired`).
- A new terminal reason `Stale` on the existing `dead` state, reached by
  a new `expire` transition when `currentTime > validUntil` on a pending
  request.
- Two new desired-state fields on `AgentRequest`: `interrupt_requested_at`
  and `valid_until`. Submitter-owned, runtime-read-only. Monotonic — once
  set, never cleared or shortened.
- One new runtime-owned field on `Message`: `interrupted_at`. Used to mark
  a streamed assistant message that was persisted before the turn ended in
  an `interrupted` terminal.
- One new runtime-owned field on `ToolResult`:
  `discarded_because_interrupted`. Written when a tool ran to completion
  but its result never reached the model because the turn was interrupted.
- A `tokio_util::sync::CancellationToken` hierarchy rooted at the daemon,
  child-scoped to inference calls and cancellable tool calls. Graceful
  window of 100ms before force-abort on cancellable work.
- A new `CancellableTool` trait wrapping `rig::tool::Tool` so individual
  tools can opt into observing cancellation. Default is non-cancellable —
  tools run to completion and their results are discarded.
- The delivery mechanism closing the admission spec's reserved
  `InferenceCall.cancelled` terminal: `PermitGuard::mark_interrupted`,
  discharge the reserved Lean axioms in `Composed.lean`.
- New Lean transitions, invariants S7/S8, extended L1 bounded termination.
- Client-side submission API (`interrupt_request` mutation), Chat UI
  interrupt button with `Esc` shortcut, transcript rendering for both
  terminals, stale-request resend flow.

Out of scope (tracked or deferred):

- **Authority model beyond DefraDB ACL.** v1 accepts any principal with
  write access to the `AgentRequest` doc as a valid interrupter. A future
  spec may narrow this (submitter-only, runtime-operator, DID-policy).
- **Auto-resend on reconnect.** Stale terminals are final; the submitter
  manually resends if they still want the work. No runtime-side retry.
- **Scheduler fairness changes.** Pending → claimed remains FIFO on
  non-stale, non-interrupted requests. No newest-first or priority
  reordering.
- **Cross-request cancellation** (e.g., "interrupt all requests for agent
  X"). A central supervisor could add this later; v1 is per-request.
- **Partial-response recovery after crash.** The admission spec already
  specifies that crash loses partial tokens; interrupt specifically
  preserves them. These remain distinct — crash ≠ interrupt.
- **TTL on non-pending states.** `valid_until` only gates pending → claim;
  it does not force expiry on a claimed or processing request. Once the
  runtime has committed to work on a request, the request's own
  `deadline` is the authority.
- **Mid-tool cancellation for non-cancellable tools.** The runtime does
  not force-kill tool subprocesses or abort tool stdio. Non-cancellable
  tools always run to completion, possibly well after the user's
  interrupt intent.

## Decisions at a glance

| Decision | Choice | Rationale |
|---|---|---|
| Unit of interruption | `AgentRequest` | The request is the user-visible unit of intent. Tool-level or message-level interrupts don't map to anything the user points at. |
| New terminal for user-driven abort | `interrupted` | Distinct from `failed` (runtime decided something went wrong) and `superseded` (another request displaced this one); matches codex's `TurnAbortReason::Interrupted`. |
| Stale-request terminal | `dead` + `failure_reason = Stale` | Reuses existing terminal rather than adding a tenth state. Telemetry via `failure_reason`. |
| Signal shape | Desired fields on `AgentRequest` | Preserves the CLAUDE.md field-ownership split: submitter owns intent, runtime owns live lifecycle state. No new doc type. |
| `valid_until` source | Submitter-supplied; client helpers default to `submitted_at + 5min` | Interactive chat wants a short TTL, scheduled tasks want long or none; server cannot pick the right default for all callers. |
| `valid_until` enforcement | Only on `pending → claimed` | Once claimed, the existing `deadline` is authoritative. Simpler mental model; no mid-stream expiry contention with `deadline_expire`. |
| Stale on reconnect | Strict TTL; no auto-retry | Client comes back online → stale requests transition to `dead/Stale` as cheap writes, no backend load; user resends if still wanted. |
| Tie-break when both interrupt + expiry hold on pending | Prefer `interrupted` | Explicit user intent is more informative than timeout. |
| Observer location | Per-request daemon + scheduler claim check | Daemon already subscribes to its own request doc; scheduler already evaluates pending claim eligibility. No new architectural layer. |
| Cancellation plumbing | `tokio_util::sync::CancellationToken` hierarchy | Codex-proven pattern; composable; drops cleanly with tasks. |
| Graceful window | 100ms before force-abort on cancellable work | Codex-aligned. Long enough for HTTP futures to observe, short enough that Esc feels instant. |
| Tool cancellation policy | Per-tool, default non-cancellable | Side-effectful tools (writes, exec) stay safe by default; cancellable tools opt in explicitly. |
| Partial-response handling | Preserve already-streamed tokens, flag with `Message.interrupted_at` | Option C from brainstorming. Lets the user "continue to steer" with context the model can see. Matches codex's history marker. |
| Cross-layer delivery | `PermitGuard::mark_interrupted`; `InferenceCall.cancelled` | Closes the admission spec's reserved terminal. No schema change on `InferenceCall`. |
| `ToolResult` of a non-cancellable tool after interrupt | Written normally, `discarded_because_interrupted = true` | Preserves audit trail. |
| Proof responsibility for cross-layer cancel | This spec | Discharges the admission spec's reserved axioms in `Composed.lean`. |
| State machine source of truth | Lean 4 (project norm) | CLAUDE.md rule. |

## Architecture

### State machine (Lean)

`RequestState` goes from 8 to 9 values. Visually:

```
pending ──► claimed ──► processing ⇄ inputRequired
   │           │              │              │
   │           ▼              ▼              ▼
   ├──►  interrupted    (existing)      (existing)
   │
   ├──►  dead (failure_reason = Stale)   via expire
   │
   └──► (existing transitions)
```

**New `RequestContext` fields:**

```lean
structure RequestContext where
  -- existing fields ...
  interruptRequestedAt : Option Time   -- desired; submitter sets, runtime reads
  validUntil           : Option Time   -- desired; submitter sets, runtime reads
```

Both are `Option` because pre-interrupt and no-TTL are legitimate states.
Both are monotonic under the transition relation (see S7, S8 below).

**New transitions** (five, all preserve `origin`, `backend`, and the
terminal `admission = released` invariant once the admission-refactor
lands):

| Name | From | Precondition | To |
|---|---|---|---|
| `expire` | `pending` | `validUntil.isSome ∧ currentTime > validUntil.get` | `dead` |
| `interrupt_before_claim` | `pending` | `interruptRequestedAt.isSome` | `interrupted` |
| `interrupt_claimed` | `claimed` | `interruptRequestedAt.isSome` | `interrupted` |
| `interrupt_processing` | `processing` | `interruptRequestedAt.isSome` | `interrupted` |
| `interrupt_input_required` | `inputRequired` | `interruptRequestedAt.isSome` | `interrupted` |

**Tie-breaking (runtime policy, not a state-machine rule):** if a pending
request satisfies both `interrupt_before_claim` and `expire` preconditions,
the scheduler prefers `interrupted`. The Lean model permits either; the
scheduler's implementation pins the order.

**Properties:**

- **S1 terminal irreversibility** — extended. `interrupted` joins the
  terminal set. `isTerminal_dec` decidable instance extended; no outbound
  transitions from `interrupted`.
- **S3 monotonic progress** — preserved. All new transitions advance out
  of non-terminals to either a more-advanced non-terminal or a terminal.
- **S4 deadline bounding** — preserved. New transitions don't alter the
  `deadline` field; `valid_until` is a separate concept (submission TTL)
  that does not interact with the claim-deadline machinery.
- **S7 interrupt monotonicity (new)** — if
  `pre.interruptRequestedAt = some t`, then for every `post` reachable from
  `pre`, `post.interruptRequestedAt = some t`. The field is a latch, not a
  toggle. Proved by induction over the transition relation; each
  constructor preserves the field.
- **S8 valid-until monotonicity (new)** — same shape as S7 for
  `validUntil`. Once set at submission time, the runtime never rewrites
  it. (If a submitter wants a longer TTL, they must resubmit.)
- **L1 bounded termination** — extended. Under the assumption that either
  `interruptRequestedAt` is set or `currentTime` eventually exceeds
  `validUntil`, every non-terminal request reaches a terminal in bounded
  steps via the new transitions.

### Schema changes

**`AgentRequest`** — two new desired fields, one new `lifecycle_state`
value, one new `failure_reason` value:

```graphql
type AgentRequest {
  # ... existing fields ...

  # DESIRED — submitter writes, runtime reads
  interrupt_requested_at: String   # RFC3339; null until interrupt requested
  valid_until: String              # RFC3339; null = no TTL

  # LIVE (existing) — gains values
  lifecycle_state: String          # gains "interrupted"
  failure_reason: String           # gains "Stale", "Interrupted"
}
```

**`Message`** — one new runtime-owned field:

```graphql
type Message {
  # ... existing fields ...
  interrupted_at: String   # RFC3339; null for complete messages, non-null ⇒ truncated
}
```

**`ToolResult`** — one new runtime-owned field:

```graphql
type ToolResult {
  # ... existing fields ...
  discarded_because_interrupted: Boolean   # default false
}
```

**`schemas/README.md`** — update the `AgentRequest`, `Message`, and
`ToolResult` rows to list the new fields; extend the `lifecycle_state` and
`failure_reason` enum lists.

**Protocol-crate row mirrors** (`defra-agent-protocol/src/row.rs`) — the
four new fields flow into the serde mirrors. No new collection, no new row
type, no new client-protocol turn-state value beyond `Interrupted` being
added to the terminal-classifier's set (the client protocol already
classifies terminal states; `interrupted` joins `completed`/`failed`/
`superseded`/`dead`).

### Runtime architecture

Two interrupt observation paths, both reusing existing subscription
machinery. No new architectural layer.

**Scheduler claim check** (`defra-agent/src/scheduler/*.rs`):

```rust
// Pseudocode — actual impl follows existing scheduler conventions
fn evaluate_pending(request: &AgentRequest, now: Time) -> ClaimDecision {
    if request.interrupt_requested_at.is_some() {
        return ClaimDecision::Transition(Lifecycle::Interrupted);
    }
    if let Some(valid_until) = request.valid_until {
        if now > valid_until {
            return ClaimDecision::Transition(Lifecycle::Dead { reason: Stale });
        }
    }
    ClaimDecision::Claim
}
```

Neither branch touches the admission controller or the backend. A
thousand stale pending requests replicating in after an offline gap
become a thousand cheap lifecycle writes, bounded by the scheduler's own
throughput.

**Per-request daemon watcher** (`defra-agent/src/agent/daemon/*.rs`):

The daemon already subscribes to its own `AgentRequest` doc for state
transitions. Extend the subscription handler to react to
`interrupt_requested_at` flipping from null to non-null:

1. Call `request_token.cancel()` on the daemon's root `CancellationToken`.
2. Await a bounded grace window (100ms) for cancellable work to observe
   the cancel and drain.
3. Force-abort any still-running cancellable work whose JoinHandle hasn't
   completed (`tokio::task::JoinHandle::abort()`).
4. Wait for any non-cancellable tool to complete naturally; its
   `ToolResult` is written with `discarded_because_interrupted = true`.
5. If an assistant message has been partially streamed into DefraDB, flip
   its `interrupted_at` field to the interrupt timestamp. No new message
   write — the partial is already persisted by the streaming writer; the
   daemon just marks it.
6. Write the terminal `lifecycle_state = interrupted` on the request doc.

**Cancellation token hierarchy:**

```
request_token (per AgentRequest, owned by daemon)
├── inference_token (one per HTTP call through AdmittedCompletionModel)
│   └── scoped to CompletionResponse / StreamingCompletionResponse future
└── tool_token (one per tool invocation)
    └── passed to Tool::call_cancellable() if the tool opts in
```

Children are created via `request_token.child_token()`. When the root
cancels, all children observe it; when a child is dropped cleanly, it
doesn't affect the root or its siblings. This is the codex-proven pattern
from `core/src/tasks/mod.rs`.

**Pending-interrupt races:** if an interrupt doc write lands in the narrow
window between scheduler claim-check and daemon startup, the scheduler
may already have promoted `pending → claimed`. The daemon's watcher picks
up the interrupt on its first subscription read and drives
`claimed → interrupted` via the `interrupt_claimed` transition. No race;
both transitions are legal.

### Tool trait

Rig's `Tool` trait is upstream and not modifiable. Wrap it:

```rust
pub trait CancellableTool: rig::tool::Tool {
    fn supports_cancellation(&self) -> bool { false }

    async fn call_cancellable(
        &self,
        args: Self::Args,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        let _ = cancel;
        self.call(args).await
    }
}

impl<T: rig::tool::Tool> CancellableTool for T {}
```

Blanket impl means every existing tool compiles unchanged as
non-cancellable. Tools opting in override both methods.

**Dispatch path** (in the daemon's tool-call loop): `spawn`s the future
under the correct shape based on `supports_cancellation()`. Non-cancellable
tools are spawned with no token; cancellable tools receive
`request_token.child_token()`.

**Opt-in inventory** (this spec's concrete work):

| Tool category | Policy |
|---|---|
| HTTP fetch, web search | Opt in (cancellable). `tokio::select!` against the reqwest future. |
| Filesystem read | Opt in. Side-effect-free. |
| Filesystem write | Non-cancellable. Half-written files are worse than a slow interrupt. |
| Shell exec | Non-cancellable. Killing mid-build leaves arbitrary state. |
| MCP over HTTP | Opt in if the tool author confirms the MCP server supports cancel semantics. |
| MCP over stdio | Non-cancellable by default. Stdio protocol lacks cancel. |
| Delegate / subagent | Non-cancellable in v1. A subagent's own interrupt path is a future extension. |

### Cross-layer propagation (closes admission spec's open item)

The admission design reserved `InferenceCall.cancelled` but deferred the
delivery mechanism. This spec closes it:

1. The daemon's `request_token` cancels on interrupt (or the daemon task
   is dropped for any other reason).
2. The `inference_token`, a child of `request_token`, signals the
   `AdmittedCompletionModel` wrapper's cancellation-select arm.
3. The arm calls `permit.mark_interrupted()` on the admission layer's
   `PermitGuard` before dropping it.
4. `PermitGuard::Drop` writes the terminal `InferenceCall` document with
   `call_state = cancelled, failure_reason = Cancelled` — these are the
   values the admission design reserved.

**New method on `PermitGuard`:**

```rust
impl PermitGuard {
    pub fn mark_interrupted(&mut self);
    // Sets terminal to (call_state: cancelled, failure_reason: Cancelled).
    // Idempotent with the admission design's existing "already-written" guard.
}
```

**Token accounting:** tokens streamed before cancellation are recorded on
the `InferenceCall`. If the provider sent 400 completion tokens before the
stream drop, `InferenceCall.completion_tokens = 400`. Matches the
admission design's own recovery model ("partial tokens recorded, not
deduplicated").

**Crash vs. interrupt distinction stays sharp:**

- **Crash** (admission spec, §Recovery): partial response lost, parent
  request retries from scratch.
- **Interrupt** (this spec): partial response preserved, parent request
  reaches `interrupted` terminal, *no retry*.

**Lean cross-module theorem** (new, in `Proofs/Composed.lean`):

```
theorem interrupted_request_cancels_calls :
    RequestContext.state r = .interrupted →
    ∀ c : InferenceCall, c.request_id = r.id →
      c.state ∈ {.running, .queued} →
      ∃ steps, InferenceCall.afterSteps c steps ∈ terminalCancelled
```

Discharges the admission spec's two reserved axioms (parent-driven
`queued → cancelled` and `running → cancelled`).

### Client-side + UI

**Submission API** (`defra-agent-desktop/src/client/mutations/chat/`):

```rust
pub async fn submit_request(
    client: &ClientCore,
    conversation_id: ConversationId,
    prompt: String,
    valid_until: Option<DateTime<Utc>>,   // NEW; client default = now + 5min
) -> Result<SubmittedRequest, ClientError>;

pub async fn interrupt_request(
    client: &ClientCore,
    request_id: RequestId,
) -> Result<(), ClientError>;
```

`interrupt_request` is idempotent: a second call on the same request is a
no-op from the submitter's perspective (the first write already latched
the field).

CLI surface parallels: `defra-agent request interrupt <id>` and
`defra-agent request submit --valid-until <duration>`.

**Chat activity — composer**
(`defra-agent-desktop/src/views/chat/composer.rs`):

The Send button replaces with a Stop button whenever turn state is
non-terminal (`streaming` or `tool_executing`). Clicking invokes
`interrupt_request(current_request_id)`. Keyboard shortcut: `Esc` while
focused on the composer. Disabled once turn state is terminal.

**Chat transcript**
(`defra-agent-desktop/src/views/chat/transcript.rs`):

- **Interrupted turn:** the truncated assistant message renders with a
  horizontal rule and a muted "Interrupted" label pulled from
  `Message.interrupted_at`. Matches codex's visual break.
- **Stale request:** a small muted system message — "Request expired
  (offline too long). [Resend]". The Resend action calls `submit_request`
  with the original prompt and a fresh `valid_until`. The stale request
  stays visible for audit; it never un-terminates.

**Manage / Operator activity:** the Requests list view gains two optional
columns — `Valid Until` (only shown when non-null) and `Interrupted At`
(only shown when terminal is `interrupted`). No new view; surface the new
fields where requests already appear.

## Lean proofs

`proofs/Proofs/Request.lean`:

- Extend `RequestState` enum with `.interrupted`; extend `isTerminal` and
  its decidable instance.
- Extend `RequestContext` with `interruptRequestedAt` and `validUntil`.
- Extend `Transition` relation with `expire`, `interrupt_before_claim`,
  `interrupt_claimed`, `interrupt_processing`,
  `interrupt_input_required`.
- Extend `Action` enum and `step?` function mechanically.
- Reprove the five invariant lemmas (`step_sound`, `transition_complete`,
  `replay_sound`, `trace_complete`, `transition_produces_coherent`) —
  each arm follows the existing pattern.
- Add S7 and S8 invariants.

`proofs/Proofs/Properties/Safety.lean`:

- Extend terminal-irreversibility to cover `interrupted`.
- Add S7 and S8 as theorems, proved by induction over the transition
  relation.

`proofs/Proofs/Properties/Liveness.lean`:

- Extend L1 bounded termination proof to cover the `interrupt_*` and
  `expire` transitions.

`proofs/Proofs/Composed.lean`:

- New theorem `interrupted_request_cancels_calls`, discharging the two
  admission-spec axioms.

`proofs/Proofs/Conformance/DefraAgent.lean`:

- Fixture extensions for the new transitions and invariants, matching the
  existing conformance style.

## Testing strategy

**Conformance tests** (`tests/state_machine_conformance.rs`):

- One assertion per new Lean transition: the Rust implementation's state
  transition matches the spec.
- S7 / S8 monotonicity assertions: the runtime never rewrites
  `interrupt_requested_at` or `valid_until` on a request.

**Lifecycle regression tests** (`tests/lifecycle_regression.rs`):

- `pending → interrupted` (submit, then interrupt before claim).
- `pending → dead/Stale` (submit with near-past `valid_until`, tick time
  forward).
- `claimed → interrupted` (interrupt after claim, before first backend
  call).
- `processing → interrupted` (interrupt mid-stream; assert partial
  message has `interrupted_at`).
- `inputRequired → interrupted`.
- `processing → interrupted` with cancellable tool in flight: tool
  returns promptly with cancellation error; `ToolResult` normal.
- `processing → interrupted` with non-cancellable tool in flight: tool
  completes; `ToolResult.discarded_because_interrupted = true`.
- Tie-break: pending with both interrupt + expired `valid_until` →
  `interrupted`, not `dead/Stale`.
- S7 / S8 enforcement is the submission helpers' responsibility —
  `submit_request` / `interrupt_request` reject attempts to clear or
  shorten either field. The Lean invariants S7 / S8 are proved over the
  modeled transition relation; the runtime reads raw field values and
  does not re-validate. A client that bypasses the helpers and writes
  inconsistent field values directly produces a runtime that observes
  what it observes — the state machine's legality is unaffected (no
  transition "un-sets" either field). Test: exercise the helpers, assert
  rejection; separately assert that a directly-written invalid doc does
  not crash or corrupt the runtime.

**Integration tests** (`tests/interruption_integration.rs`, new):

- End-to-end with mock backend: submit → streaming starts → interrupt →
  partial assistant message preserved + truncated + InferenceCall
  terminal is `cancelled`.
- Offline replay: 20 requests submitted with past-now `valid_until` while
  the runtime is paused (simulated offline), runtime resumes, all 20
  transition to `dead/Stale` as a burst of cheap writes without any
  backend interaction.
- Resend from stale: stale request + resend action creates a new request
  doc with fresh `valid_until`; original stays as `dead/Stale`.
- Concurrent requests: interrupt one of two in-flight requests; the
  other's state machine is unaffected. Confirms token hierarchies are
  per-request-isolated.

**Live tests** (`tests/live/interrupt_live.rs`, env-gated):

- Real MiniMax backend; submit, observe streaming, Esc mid-stream;
  assert interrupt lands in ≤2s and no more tokens written after
  `interrupted_at`.

## Implementation order

Each step leaves the tree green.

1. **Lean first.** Extend `Request.lean` with new state, fields,
   transitions; prove S7, S8; extend L1. Extend `Composed.lean` with the
   cross-layer cancellation theorem (discharges admission axioms).
2. **Schema + protocol.** Add new fields to `AgentRequest`, `Message`,
   `ToolResult`; update `schemas/README.md`; extend
   `defra-agent-protocol/src/row.rs` and the turn-state terminal
   classifier.
3. **Conformance tests land first.** Update
   `tests/state_machine_conformance.rs` with the new transitions and
   S7/S8 — these should *fail* until the runtime code is added, driving
   the remaining steps.
4. **Scheduler claim check.** Add the pre-claim interrupt + stale
   branch. Exercise via conformance fixtures.
5. **CancellationToken plumbing.** Add `request_token` ownership to the
   daemon; thread child tokens through the inference path and the tool
   dispatch path. No behavior change in this step — just wiring.
6. **Daemon watcher.** Extend the existing subscription handler to
   observe `interrupt_requested_at`; wire to `request_token.cancel()`.
   Partial-message flip. Terminal write. ToolResult discard path.
7. **Tool trait.** Add `CancellableTool`; opt in HTTP / filesystem-read
   tools; wire the dispatch branch.
8. **Admission-layer bridge.** Add `PermitGuard::mark_interrupted`;
   `AdmittedCompletionModel` wires the cancellation-select arm to call
   it. The Composed.lean theorem now matches the runtime.
9. **Submission API + UI.** `interrupt_request` mutation, Stop button /
   Esc shortcut in composer, transcript rendering for interrupted +
   stale, resend flow, Manage activity columns.
10. **Integration + live tests.**

## Open items

- **Authority for interrupt.** v1 inherits DefraDB ACL (any principal
  with write on the request doc can interrupt). A follow-up spec may
  narrow to submitter-only or add a DID-policy layer.
- **Interrupt on a claimed request that has not yet started streaming.**
  This spec's `interrupt_claimed` transition handles the
  state-machine side, but the runtime side has a small nuance: the
  admission layer's `inference_token` doesn't exist until the first
  backend call starts. The daemon's grace window needs to handle "cancel
  before any child token exists" (trivial — nothing to cancel, go
  straight to terminal).
- **`Message.interrupted_at` vs. `truncated: Boolean`.** Chose
  single-field shape (non-null `interrupted_at` ⇒ truncated). If future
  use cases need a truncated message that is not from an interrupt
  (e.g., length cap mid-stream), revisit.
- **Non-cancellable tool completion semantics.** A non-cancellable tool
  running for minutes after an interrupt creates a transcript where the
  `interrupted` terminal appeared before the `ToolResult` with
  `discarded_because_interrupted = true` did. Clients should render this
  coherently; the Chat UI spec (Section 6) doesn't cover the exact
  presentation (small — addressed in UI follow-up).
- **TTL default duration.** The 5-minute client-side default is a guess.
  Revisit with real telemetry once the feature lands; per-behavior or
  per-client-kind defaults may turn out to be warranted.

## Relationship to wider architecture

- **Closes the admission spec's reserved terminal.** The open item in
  `2026-04-15-concurrent-inference-admission-design.md` — the delivery
  mechanism for `InferenceCall.cancelled` — is resolved here. No schema
  changes needed on `InferenceCall`; the reserved values are used as-is.
- **Consistent with the document-driven control plane** (#8). Interrupt
  and TTL are both desired-state signals written by the submitter; the
  runtime observes and drives lifecycle transitions. Same pattern as
  apply/reconcile.
- **Preserves the principal/behavior/deployment split** (#9). Interrupt
  authority today is inherited from the AgentRequest doc's ACL; once the
  split lands, the principal boundary is the natural narrowing axis.
- **Tangential to context management** (#17). A long-running
  conversation may accumulate interrupted turns whose partial assistant
  messages become part of the transcript; compaction needs to recognize
  `Message.interrupted_at` and handle truncated messages appropriately.
  Out of scope here — documented as a consumer of the new field.
- **Lean proofs as source of truth** (CLAUDE.md). Every behavior change
  is driven from Lean first; the implementation satisfies the updated
  proofs.
