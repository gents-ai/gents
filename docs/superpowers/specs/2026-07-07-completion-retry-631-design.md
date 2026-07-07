# Completion Retry (#631) — Design

**Status:** approved-in-brainstorm, pending spec review
**Issue:** #631 (runtime never retries failed completion requests)
**Related:** #638 (tool-replay hazard, closed by this work), #589/#590/#601 (tool-args
boundary), #442 (tool-result backfill), #438/#509 (owned provider layer), #544
(InferenceProfile design)

## Problem

A completion request that fails mid-run — HTTP 400, transport error, mid-stream
decode failure — terminates the whole agentic run. The 48h fleet quantification
(#631 comment): 26 retryable run-kills out of ~670 scheduled runs (~4% loss),
16 of them clustered on two ~3-minute backend restarts, 10 vLLM 400s, 4
mid-stream decode failures.

### What main already has (and why it didn't help)

- `RetryPolicy` (`retry.rs`): 3 retries, 1s base, 30s cap, jitter.
- A daemon-level attempt loop (`agent/daemon/inference.rs`, `max_attempts=4` in
  the prod spans) with deadline-bounded backoff, already tested.
- `classify_completion_error` (`error.rs`): typed `InferenceError` verdicts;
  already classifies the vLLM parse-signature 400
  (`BadRequestError` + `line/column/(char N)`) as transient.
- `InferenceCall` rows already persist `attempt: Int` and `failure_reason`.

The retry gate requires `!processor.has_observable_activity()`, and
`streamed_text` accumulates across **all turns** of the run — so any run that
ever printed visible text is permanently unretryable. All 26 production
failures died at attempt=1 for this reason. The granularity is wrong: one
"attempt" is the whole multi-turn run, and re-driving it would re-run tools,
hence the conservative gate.

**Latent hazard (#638):** for a run whose turns emitted only tool calls and no
visible text, the gate is *false* at failure time (the turn accumulator drains
at every turn boundary), so the outer retry re-invokes `run_loop_stream` from
scratch and re-executes every tool in the run. The redesign removes this path
structurally.

### Triage: the deterministic 400 sub-shape is pre-#601

The 3× identical `Expecting value: line 1 column 28 (char 27)` failures all
occurred on a fleet binary installed 2026-07-01 14:20Z — nine hours **before**
PR #601 merged (23:18Z). The post-#601 binary (installed 07-06 17:32Z) shows
zero failures since. Mechanism: the vLLM parse-400 arrives as an HTTP status at
request time — vLLM `json.loads`-es tool-call args in the *input* transcript
during chat-template render. Within-run resampling of the same input is
therefore deterministic for this class; operator re-kicks succeeded because a
fresh run reloads history through #601's ingest sanitization. Post-#601, egress
normalization covers every completion input (including same-run threaded
messages), so the class should be dead. **No #601 gap is filed.** The repair
path below remains as a regression fence, with the exact prod payload as its
test.

## Design

One per-request retry budget, three mechanisms, all at or below the owned-loop
seam (`agent/loop_stream.rs`) — preserving the CLAUDE.md property that every
completion is born in the loop, and adding: every completion *retry* is decided
in the loop.

### 1. In-loop completion retry (covers 22/26 prod failures)

In `run_loop_stream`, around `model.stream(request)` and the first item of the
turn: a completion that fails **before yielding any item of the current turn**
is re-issued with the *same assembled request* after deadline-aware backoff.
Prior turns' content stands untouched; no tool re-runs by construction (no
effects exist for the current turn yet).

Classification ladder (consuming the existing typed `InferenceError`):

| Class | Policy |
|---|---|
| transport / 5xx / timeout / rate-limit | full ladder, default **5s → 30s → 2m** (3 retries) |
| parse-signature 400 (vLLM json.loads) | 1 resample; **identical error text twice → skip remaining resamples, go to repair** |
| repair (at most once per request) | aggressive re-sanitize + escape-salvage of the assembled input (the #589-style sanitizer run in its most aggressive mode), then one final attempt |
| everything else permanent | fail as today |

`RateLimited { retry_after_secs }` uses the provider hint when it exceeds the
ladder step.

### 2. Mid-stream failure (the 4 decode cases), keyed on tool effects

- **No tool executed this turn → retract-and-resample.** The loop emits a
  `TurnRetracted` control event; the consumer resets the turn accumulator and
  truncates `streamed_text` to the turn mark; the stream writer rewinds its
  un-persisted tail. The fresh sample renders as the one true turn. The durable
  transcript is untouched (nothing of the turn was persisted yet). Property:
  the materialized response renders **exactly one** turn per turn index.
- **Tools already executed this turn → close-and-continue.** Thread the
  partial assistant turn and yield the executed ToolResults through the normal
  path (persists pair-closed, reusing the #442 backfill-adjacent machinery),
  then continue the loop: the tool results are the next turn's prompt and the
  model continues naturally. No re-execution; no retraction of real effects.
  Consumes a retry token and backoff like any other retry.

Retraction is **forbidden** once any tool has executed in the turn; the two
rules partition mid-stream failures exactly.

### 3. Loop item type becomes native

`run_loop_stream` yields a native enum instead of the bare rig item:

```rust
enum LoopStreamItem<R> {
    Item(MultiTurnStreamItem<R>),
    TurnRetracted { turn: usize, attempt: u32 },
    AttemptFailed { turn: usize, attempt: u32, error: InferenceError, will_retry: bool, backoff: Duration },
}
```

Consumers: the daemon `StreamProcessor` (handles retraction + records attempt
events), `run_loop_to_text` (ignores control events, keeps last-error). This
also fixes the rendered-request capture key: `on_rendered_request` is keyed by
`(turn_index, attempt)` instead of colliding on turn index across retries.

### 4. Daemon outer retry removed

With retry owned by the loop, the outer attempt loop's only remaining retry
path is exactly the #638 hazard — delete it. The daemon keeps: partial-turn
persistence on terminal failure, tool-result backfill, terminal error
finalization, interrupt/shutdown handling. `InferenceAttemptOutcome::Retry`
and the `!had_observable_activity` gate go away.

### Budget and configuration

- One budget per request, shared across all mechanisms:
  `transport_retries` (ladder), `resample_retries`, `repair_retries` (0/1).
- Keyed by `execution_origin`: **scheduled** → 3 transport + 1 resample + 1
  repair with the 5s/30s/2m ladder; **interactive** → 1 quick retry (~2s) + 1
  repair, no minutes-scale sleeps.
- Config fields on the **`InferenceProfile`** document (next to `max_turns`,
  `stream_liveness_timeout_secs`, `deadline_duration_secs`):
  `retry_max_transport`, `retry_backoff_ms` (Int list — **emit `null`, never
  `[]`**, per the DefraDB sharp edge), `retry_max_resample`,
  `retry_allow_repair`, `retry_interactive_max`. Scheduled/task origins use the
  full fields; interactive origins use `retry_interactive_max` quick retries at
  a fixed ~2s delay plus repair-if-allowed, ignoring the ladder. Process
  defaults apply when absent. Reconcile threads the resolved policy into
  `LoopConfig`.
- **Deadline is a hard ceiling, fail-fast:** if the next delay exceeds the
  remaining claimed deadline, fail *now* (do not sleep a truncated delay into
  certain death). Requests without deadlines (today's scheduled runs have
  `has_deadline=false`) are bounded by the ladder total (~3m).
- The `InferenceCall` row stays `running` (slot held) through backoff sleeps —
  documented as an accepted boundary; during backend outages the backend is
  down anyway, and per-completion sleeps are short.

### Observability

- `InferenceCall` gains `completion_retry_count: Int` and
  `last_transient_error: String` (nillable). Recovered-vs-dead is derivable:
  terminal `completed` × `completion_retry_count > 0` = recovered. These
  persisted fields are the durable operator surface — `run_timeline.rs`
  projects them so retries are readable from run history without log
  archaeology (the issue's operator ask). Per-attempt detail beyond
  count + last error stays in `tracing` spans (`AttemptFailed` events carry
  it in-process).

## Lean scope

Request lifecycle unchanged: all retrying happens inside `processing`; budget
exhaustion reaches the existing `failed` transition. No new request states.

New executable model `Proofs/CompletionRetry.lean` (+ `Executable`,
`Properties` submodules per house structure):

- State: turn index, attempt counter, per-class budget, effects-this-turn flag,
  turn-closed flag, repair-used flag, deadline, clock.
- Obligations:
  - **N1 no-tool-re-execution:** a re-issue transition requires
    `effectsThisTurn = ∅ ∨ turnClosed`.
  - **N2 retract-only-before-effects:** `TurnRetracted` requires
    `effectsThisTurn = ∅`.
  - **N3 bounded progress:** attempt counters monotone and bounded by budget;
    repair fires at most once.
  - **N4 backoff-fits-deadline:** a backoff transition requires
    `now + delay ≤ deadline` (fail-fast otherwise); retry never extends the
    deadline.
  - **N5 render-exactly-once:** each turn index contributes at most one
    retained rendered turn (retraction removes, close-and-continue retains and
    freezes). Stated in Lean; discharged as a conformance obligation in
    `stream_processor` durable-fence tests (the #589/#590 lesson: fences live
    at the processor, not the bare generator).
- Contract emission in `Proofs/Conformance/Contracts.lean`, coverage-ledger
  entry, and consumer registration in
  `tests/support/conformance_consumers.rs` — same change, per proofs README.
- Boundary note: slot-held-during-backoff recorded in
  `Proofs/Conformance/Boundaries.lean`.

Zero `sorry`s, as always. Spec lands before Rust.

## Testing (the 48h tape as fixtures)

Mock-model / time-paused tokio where timing matters; full package gate
(`cargo test -p defra-agent`, never `--lib`).

1. **Backend-restart cluster:** N concurrent completions hit connect-refused;
   the mock backend recovers at ~3 simulated minutes; all N runs complete,
   `completion_retry_count > 0`, zero lost runs.
2. **Sampling 400:** first attempt 400s with parse signature, resample
   succeeds.
3. **Deterministic 400 (regression fence):** the exact prod
   `Expecting value: line 1 column 28 (char 27)` payload twice → repair path →
   success-or-clean-failure, provably no infinite resample.
4. **Mid-stream decode at a delta boundary:** retract-and-resample → exactly
   one rendered turn in the materialized response (stream_processor durable
   fence).
5. **Deadline-tight:** next backoff exceeds remaining deadline → immediate
   clean failure with today's terminal semantics.
6. **No re-execution (closes #638):** spy tool counts dispatches; mid-stream
   failure after a tool ran → close-and-continue → each tool dispatched exactly
   once; text-less multi-turn run + retryable failure → no whole-run re-drive.
7. **Interactive budget:** interactive-origin request gets the quick-retry
   policy, no minutes-scale sleep.
8. Lean conformance: CompletionRetry contract cases consumed by a Rust
   conformance test; ledger accounts for the new domain.

## Out of scope

- Request-level reissue (SessionRecovery model) — unchanged, distinct layer.
- Tool retries (`ToolExecution` model gates those separately).
- #438 native-provider classification: `classify_completion_error` remains the
  verdict seam; typed transport classification tightens underneath when #438
  lands, without changing the loop contract.
- MaxTurn-cap failures (the 27th failure in the window) — different class.
