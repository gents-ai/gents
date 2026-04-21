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
  and `valid_until`. Desired-state (runtime reads, never writes); v1
  inherits DefraDB ACL without narrowing, so any principal with write
  access to the `AgentRequest` doc can set either field. Monotonic in the
  Lean model (S7/S8). The runtime enforces the spirit of S8 operationally
  by reading `valid_until` exactly once at the scheduler claim check and
  caching the result for the lifetime of the request; later rewrites to
  the field have no runtime effect.
- Two new runtime-owned fields on `AgentMessage`: `request_id` (the
  producing request, so partial messages can be joined to the request
  that produced them without a positional lookup) and `interrupted_at`
  (marks a streamed assistant message that was persisted before the
  turn ended in an `interrupted` terminal). `request_id` is a new index
  and ships on every new message regardless of interrupt — it is a
  latent requirement the interrupt work surfaces, not interrupt-specific.
- One new runtime-owned field on `AgentToolResult`:
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
- Client-side submission API: `interrupt_request` mutation,
  `submit_request` gaining a `valid_until` and `retry_parent_request`
  parameter, and a `request resend` helper that preserves the
  `retry_parent_request` / `retry_root_request` audit chain.
- Exact UI presentation (Stop button placement, keyboard shortcuts,
  transcript rendering of the two new terminals) is **out of scope**
  for this spec — it is follow-up UX work that can land against the
  state machine once it is in place.

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
| Partial-response handling | Preserve already-streamed tokens, flag with `AgentMessage.interrupted_at` | Option C from brainstorming. Lets the user "continue to steer" with context the model can see. Matches codex's history marker. |
| Cross-layer delivery | `PermitGuard::mark_interrupted`; `InferenceCall.cancelled` | Closes the admission spec's reserved terminal. No schema change on `InferenceCall`. |
| `AgentToolResult` of a non-cancellable tool after interrupt | Written normally, `discarded_because_interrupted = true` | Preserves audit trail. |
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

**Tie-breaking (runtime policy, not a state-machine rule):** when
multiple transitions are simultaneously enabled for a request, the
runtime applies a fixed preference order so that explicit user intent
always wins over timeouts:

- Pending with both `interrupt_before_claim` and `expire` enabled →
  scheduler fires `interrupt_before_claim`.
- Claimed / processing / inputRequired with both `interrupt_*` and
  `deadline_expire` (or `input_timeout`) enabled → daemon fires the
  `interrupt_*` transition.

The Lean model permits either ordering at each fork; only the runtime
implementation pins the choice. Tests assert the policy at every fork.

**Properties:**

- **S1 terminal irreversibility** — extended. `interrupted` joins the
  terminal set. `isTerminal_dec` decidable instance extended; no outbound
  transitions from `interrupted`.
- **S3 monotonic progress** — preserved. All new transitions advance out
  of non-terminals to either a more-advanced non-terminal or a terminal.
- **S4 deadline bounding** — preserved. New transitions don't alter the
  `deadline` field; `valid_until` is a separate concept (submission TTL)
  that does not interact with the claim-deadline machinery.
- **S6 persistence before completion** — extended. The `interrupted`
  terminal joins the set of terminals that imply persistence. Each new
  `interrupt_*` transition must fire *after* the daemon has written
  the `AgentMessage.interrupted_at` mark on any partial assistant
  message, so that "terminal observed ⇒ persisted state is coherent"
  holds. Proof shape matches the existing `finish`/`fail` cases.
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
  failure_reason: String           # gains "Stale" (for dead/Stale expire path)
}
```

**`AgentMessage`** — two new runtime-owned fields:

```graphql
type AgentMessage {
  # ... existing fields ...
  request_id: String @index   # producing request; indexed for filtering
  interrupted_at: DateTime    # null for complete messages, non-null ⇒ truncated
}
```

`request_id` is the backing field for the "messages produced by this
request" join and is what makes the interrupt invariant (below) have a
clean foreign-key formulation. The streaming writer sets `request_id`
on every new message regardless of interrupt; the field is not
interrupt-specific.

`interrupted_at` uses `DateTime` to match the existing `timestamp` field
on `AgentMessage` rather than the `String` RFC3339 style used on
`AgentRequest` (the type is per-collection in the current schema).

Note: `AgentToolResult` already has a `truncated: Boolean` +
`truncation_metadata: String` pair for a different concern
(output-size truncation). `AgentMessage` uses the single-field
`interrupted_at` shape because the timestamp itself is the metadata —
there is nothing structured to spill into a second field. If future use
cases require non-interrupt truncation on messages (e.g., context
window cap), revisit and align on the `AgentToolResult` pattern at that
time.

**`AgentToolResult`** — one new runtime-owned field:

```graphql
type AgentToolResult {
  # ... existing fields ...
  discarded_because_interrupted: Boolean   # default false
}
```

**`schemas/README.md`** — update the `AgentRequest`, `AgentMessage`, and
`AgentToolResult` rows to list the new fields; extend the
`lifecycle_state` and `failure_reason` enum lists.

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

The scheduler reads `valid_until` **exactly once per request** at claim
check and does not observe subsequent rewrites of the field on the doc.
Same rationale applies to the daemon's cached per-request deadline on
claim: once the runtime has committed, the TTL the submitter signed up
for is frozen. This gives us the S8-monotonicity guarantee operationally
without requiring the runtime to police every doc write — a malicious
or buggy writer who tries to extend `valid_until` after claim simply has
no effect. Before claim, the scheduler re-evaluates on each pass, so a
pre-claim extension is observed but can only shorten the window to
expiry (any shortening past `now` triggers `expire`, any lengthening
leaves the request eligible longer — both are safe).

Neither branch touches the admission controller or the backend. A
thousand stale pending requests replicating in after an offline gap
become a thousand cheap lifecycle writes, bounded by the scheduler's own
throughput.

**Per-request daemon watcher** (`defra-agent/src/agent/daemon/*.rs`):

The daemon already subscribes to its own `AgentRequest` doc for state
transitions. Extend the subscription handler to react to
`interrupt_requested_at` flipping from null to non-null:

1. Call `request_token.cancel()` on the daemon's root `CancellationToken`.
2. If at least one cancellable-work child token is live, await a bounded
   grace window (100ms) for it to observe the cancel and drain. Skip the
   wait entirely if no children are outstanding (common when the
   interrupt lands on a freshly `claimed` request with no in-flight
   inference or tool call yet).
3. Force-abort any still-running cancellable work whose JoinHandle hasn't
   completed (`tokio::task::JoinHandle::abort()`).
4. If an assistant message has been partially streamed into DefraDB, flip
   its `interrupted_at` field to the interrupt timestamp. No new message
   write — the partial is already persisted by the streaming writer; the
   daemon just marks it. **This write is sequenced before step 5 so any
   subscriber that observes the terminal also observes the marked
   partial.** The invariant is:

   > For any `AgentRequest r` with `r.lifecycle_state = interrupted`,
   > every `AgentMessage m` with `m.request_id = r.request_id`,
   > `m.role = "assistant"`, and `m.content ≠ ""` has `m.interrupted_at`
   > set.

   The `AgentMessage.request_id` field (new this spec) is what makes
   the invariant a straight foreign-key query rather than a positional
   lookup on session + sequence + timestamps.
5. Write the terminal `lifecycle_state = interrupted` on the request doc.
6. Any non-cancellable tool still running continues to completion.
   When it finishes, its `AgentToolResult` is written with
   `discarded_because_interrupted = true`. This write may land after
   the terminal in step 5 — clients must render the transcript as
   eventually consistent (see Open Items).

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

**Interrupt arrives on an already-terminal request:** the submitter's
`interrupt_request` helper is idempotent, but a submitter *could* write
`interrupt_requested_at` on a request that has already reached
`completed`, `failed`, `dead`, or `superseded`. By S1 (terminal
irreversibility), no transition fires. The runtime observes the field
change, logs at debug level, and takes no action. The client-facing
behavior is the same as interrupting twice: the second call is a no-op.
This case is legal in the Lean model (no transition is enabled from a
terminal state) and requires no special handling beyond the observation
that "observing a change" ≠ "taking a transition."

### Tool trait

Rig's `Tool` trait is upstream and not modifiable. Wrap it:

```rust
pub trait CancellableTool: rig::tool::Tool {
    /// Return true only if `call_cancellable` is also overridden.
    /// The dispatch path asserts this pairing in debug builds.
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
non-cancellable. Tools opting in **must** override both methods;
overriding only `supports_cancellation() -> true` while leaving the
default `call_cancellable` in place silently ignores the token and is
a footgun.

**Dispatch path** (in the daemon's tool-call loop): `spawn`s the future
under the correct shape based on `supports_cancellation()`. Non-cancellable
tools are spawned with no token; cancellable tools receive
`request_token.child_token()`. The dispatch site wraps the cancellable
branch with a lightweight witness to catch the footgun:

```rust
if tool.supports_cancellation() {
    // Debug-only: panic if the tool returns the same result on a pre-
    // cancelled token as on a fresh one for a canary input. In release
    // builds the check is compiled out; in debug builds it surfaces the
    // "forgot to override call_cancellable" mistake as a clear panic
    // during the first tool-integration test run.
    debug_assert!(
        cancellation_is_observed(&tool),
        "{} declares supports_cancellation() = true but its \
         call_cancellable ignores the token — override both methods",
        tool.name()
    );
    // ... dispatch with child token ...
}
```

A tool-author-facing checklist lives next to the trait definition in
doc-comments: "to opt in, override (a) `supports_cancellation`, (b)
`call_cancellable`, and (c) add a unit test that cancels mid-call."

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

### Client-side surfaces

This section defines the non-UI surfaces (submission API, CLI, resend
semantics) needed to drive the state machine from a client. Concrete
UX — button placement, keyboard shortcuts, transcript rendering — is
follow-up work and intentionally out of scope here.

**Submission API** (`defra-agent-desktop/src/client/mutations/chat/`):

```rust
pub async fn submit_request(
    client: &ClientCore,
    conversation_id: ConversationId,
    prompt: String,
    valid_until: Option<DateTime<Utc>>,   // NEW; client default = now + 5min
    retry_parent_request: Option<RequestId>,   // NEW; set when this is a resend
) -> Result<SubmittedRequest, ClientError>;

pub async fn interrupt_request(
    client: &ClientCore,
    request_id: RequestId,
) -> Result<(), ClientError>;
```

`interrupt_request` is idempotent: a second call on the same request is a
no-op from the submitter's perspective (the first write already latched
the field). The client observes the lifecycle transition to `interrupted`
through the existing per-request subscription machinery — no new
acknowledgement channel. `interrupt_request` returning `Ok(())` only
confirms the doc write landed locally; the terminal transition is
observed on the subscription, same as every other lifecycle transition.

**CLI surface** parallels:

```
defra-agent request interrupt <request-id>
defra-agent request submit --valid-until <duration>
defra-agent request resend <stale-request-id>
```

`request resend` is a one-shot helper that copies the original prompt
and submits a new request with `retry_parent_request` set to the
stale request's id and `retry_root_request` following the usual chain
(same as existing retry paths).

**Resend semantics (stale requests):** creating a new request from a
stale terminal *must* populate `retry_parent_request` with the stale
request's id, and `retry_root_request` with the chain root (same rule
the runtime already uses for retry-on-failure). The stale request
stays visible in its `dead/Stale` terminal; it never un-terminates.
This is the audit linkage between submissions the user perceives as
"the same request, resent." Without it, a client that loses the link
cannot reconstruct resend chains from the DB.

**Manage / Operator surface:** the new fields (`valid_until`,
`interrupt_requested_at`, `interrupted_at`, `discarded_because_interrupted`)
appear on the existing Requests and Tool Results views where their
parent rows already appear. Exact column-presentation decisions are UI
follow-up.

## Lean proofs

`proofs/Proofs/Request.lean`:

- Extend `RequestState` enum with `.interrupted`; extend `isTerminal` and
  its decidable instance.
- Extend `RequestContext` with `interruptRequestedAt` and `validUntil`.
- Extend `Transition` relation with `expire`, `interrupt_before_claim`,
  `interrupt_claimed`, `interrupt_processing`,
  `interrupt_input_required`.
- Extend `Action` enum and `step?` function mechanically.
- Extend every existing lemma in `Request.lean` with new case arms for
  the five new transitions — the list today includes `step_sound`,
  `transition_complete`, `replay_sound`, `trace_complete`,
  `transition_produces_coherent`, `backend_binding_preserved`,
  `origin_preserved`, and `terminal_implies_released_local`
  (plus the three `releaseToTerminal_*` helpers and `claimed_coherent_cases`).
  Each new arm follows the existing pattern; most are one-liners.
- Add S7 and S8 invariants.

`proofs/Proofs/Properties/Safety.lean`:

- Extend terminal-irreversibility (S1) to cover `interrupted`.
- Extend persistence-before-completion (S6) to cover `interrupted` — the
  `interrupt_*` transitions must be sequenced after any in-flight
  partial-message write, matching the existing `finish`/`fail` shape.
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
  `interrupt_requested_at` or `valid_until` on a request. The
  scheduler reads `valid_until` exactly once per request (at claim
  check) and caches the result; later doc rewrites have no runtime
  effect.

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
  returns promptly with cancellation error; `AgentToolResult` normal.
- `processing → interrupted` with non-cancellable tool in flight: tool
  completes; `AgentToolResult.discarded_because_interrupted = true`.
- Tie-break (pending): both interrupt + expired `valid_until` →
  `interrupted`, not `dead/Stale`.
- Tie-break (processing): both interrupt + deadline exceeded →
  `interrupted`, not `failed` via `deadline_expire`.
- Idempotency: two calls to `interrupt_request(id)` produce exactly
  one `interrupt_requested_at` write; the second is a no-op. The
  request reaches `interrupted` exactly once and does not re-transition.
- Interrupt-on-already-terminal: calling `interrupt_request` on a
  request in `completed`, `failed`, `dead`, or `superseded` leaves the
  lifecycle unchanged (S1). The daemon observes and logs at debug level;
  no backend or admission activity.
- S8 runtime enforcement: submit a request with `valid_until = now + 10s`,
  claim it, then rewrite `valid_until = now + 1h` via a direct doc
  write; assert the cached-at-claim value is still used and the request
  behaves as if the extension didn't happen. (S7 has no analogous
  runtime gate — it's a latch that fires once; a second rewrite is
  observed as idempotent.)

**Integration tests** (`tests/interruption_integration.rs`, new):

- End-to-end with mock backend: submit → streaming starts → interrupt →
  partial assistant message preserved + truncated + InferenceCall
  terminal is `cancelled`.
- Offline replay: 20 requests submitted with past-now `valid_until` while
  the runtime is paused (simulated offline), runtime resumes, all 20
  transition to `dead/Stale` as a burst of cheap writes without any
  backend interaction.
- Resend from stale: stale request + `request resend` helper creates a
  new request doc with fresh `valid_until`, populated
  `retry_parent_request = <stale request id>` and `retry_root_request`
  following the existing retry-chain rules. Assert the original stays
  as `dead/Stale`, the new request is `pending`, and the audit chain is
  queryable.
- Concurrent requests: interrupt one of two in-flight requests; the
  other's state machine is unaffected. Confirms token hierarchies are
  per-request-isolated.

**Live tests** (`tests/live/interrupt_live.rs`, env-gated):

- Real MiniMax backend; submit, observe streaming, call
  `interrupt_request` mid-stream; assert the request reaches
  `interrupted` in ≤2s and no more tokens written to the partial
  message after `interrupted_at`.

## Implementation order

Each step leaves `cargo test --workspace` green. New conformance cases
that describe not-yet-implemented runtime behavior land behind
`#[ignore]` and graduate to active as the corresponding runtime code
arrives. The Lean `lake build` stays green after step 1.

1. **Lean first.** Extend `Request.lean` with new state, fields,
   transitions; prove S7, S8; extend L1. Extend `Composed.lean` with the
   cross-layer cancellation theorem (discharges admission axioms).
2. **Schema + protocol.** Add new fields to `AgentRequest`
   (`interrupt_requested_at`, `valid_until`), `AgentMessage`
   (`request_id`, `interrupted_at`), and `AgentToolResult`
   (`discarded_because_interrupted`); update `schemas/README.md`;
   extend `defra-agent-protocol/src/row.rs` and the turn-state terminal
   classifier. Update the streaming writer so that every new
   `AgentMessage` row ships with its producing `request_id` —
   backfilling existing rows is out of scope.
3. **Conformance scaffolding.** Update
   `tests/state_machine_conformance.rs` with the new transitions and
   S7/S8 cases; each new case gated behind `#[ignore]` with a comment
   pointing at the step that unblocks it. The green-tree invariant holds
   because ignored tests don't run under the default cargo test
   invocation.
4. **Scheduler claim check.** Add the pre-claim interrupt + stale
   branch. Exercise via conformance fixtures.
5. **CancellationToken plumbing.** Add `request_token` ownership to the
   daemon; thread child tokens through the inference path and the tool
   dispatch path. No behavior change in this step — just wiring.
6. **Daemon watcher.** Extend the existing subscription handler to
   observe `interrupt_requested_at`; wire to `request_token.cancel()`.
   Partial-message `interrupted_at` flip (before terminal write, per
   S6). Terminal write. `AgentToolResult.discarded_because_interrupted`
   path for non-cancellable tools that complete after the terminal.
7. **Tool trait.** Add `CancellableTool`; opt in HTTP / filesystem-read
   tools; wire the dispatch branch.
8. **Admission-layer bridge.** Add `PermitGuard::mark_interrupted`;
   `AdmittedCompletionModel` wires the cancellation-select arm to call
   it. The Composed.lean theorem now matches the runtime.
9. **Submission API + resend.** `interrupt_request` mutation,
   `submit_request` gains `valid_until` and `retry_parent_request`
   parameters, `request resend` CLI helper, Manage/Operator surface
   changes. UX — button placement, transcript rendering, keyboard
   shortcuts — is explicitly out of scope for this spec and tracked
   separately.
10. **Integration + live tests.**

## Open items

- **Authority for interrupt.** v1 inherits DefraDB ACL (any principal
  with write on the request doc can interrupt). A follow-up spec may
  narrow to submitter-only or add a DID-policy layer.
- **`AgentMessage.interrupted_at` vs. `truncated: Boolean` +
  `truncation_metadata: String` (the existing `AgentToolResult`
  pattern).** Chose single-field shape for `AgentMessage` (non-null
  `interrupted_at` ⇒ truncated) because the timestamp is the only
  metadata. If future use cases need a truncated message that is not
  from an interrupt (e.g., length cap mid-stream with structured
  reason), revisit and align on the two-field `AgentToolResult` shape.
- **Non-cancellable tool completion semantics.** A non-cancellable tool
  running for minutes after an interrupt creates a transcript where the
  `interrupted` terminal appeared before the `AgentToolResult` with
  `discarded_because_interrupted = true` did. Clients must render this
  as eventually consistent (the terminal is not a "closed file" — later
  audit rows can still arrive). Exact UI presentation is follow-up work,
  intentionally not pinned here.
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
  `AgentMessage.interrupted_at` and handle truncated messages appropriately.
  Out of scope here — documented as a consumer of the new field.
- **Lean proofs as source of truth** (CLAUDE.md). Every behavior change
  is driven from Lean first; the implementation satisfies the updated
  proofs.
