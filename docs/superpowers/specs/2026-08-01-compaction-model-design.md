# Compaction: model the real reducer, fix what it exposes (#993)

Status: approved design, not yet implemented.
Branch: `compaction-model`. Single PR against `main`.

## Problem

`Proofs/Compaction/Transition.lean` proves preservation properties about
**identity functions**. `stubMessageKind` is literally
`| .toolResult callId key => .toolResult callId key`, and
`stripToolResultsReducer_id` proves the reducer is `id`. Every theorem in
`Proofs/Compaction/Properties.lean` is therefore vacuous with respect to the
production summarize path in `crates/gents/src/compaction/`, which really does
drop and rewrite transcript rows.

Three production defects live in the gap:

1. **The summarize split can orphan a tool-call/result pair.**
   `split_messages_for_summary` cuts on a token budget at an arbitrary index.
   When the boundary lands between an assistant message carrying a `ToolCall`
   and the user message carrying its `ToolResult`, the call is summarized away
   while the result stays in the retained tail. `sanitize_history_for_provider`
   then drops the orphaned result at loop entry, so the tool's output is lost
   from the provider view entirely while the summary describes only the call.

2. **`safeToReduce` has no runtime counterpart.** The gate exists in Lean. The
   conformance case in `tests/conformance/streaming_compaction.rs:1219`
   implements the gate *inside the test* (`"any_valid" if case.safe_to_reduce`),
   so the test cannot detect its absence from production.

3. **The compacted-prefix accounting mismatch.** `agent/daemon/request.rs:105`
   hands `compact()` the sanitized history and `compaction.rs:181` records
   `messages_compacted = old_messages.len()` — a count indexing
   `strip(sanitize(strip(H)))`. On the next request, `request.rs:72` applies
   that count to `stripped_history` *before* sanitization runs — i.e. to
   `strip(H)`. Whenever `sanitize_history_for_provider` removes anything at or
   before the boundary the two indexings diverge: either summarized messages
   survive verbatim alongside their own summary, or messages that were never
   summarized are silently dropped from the provider view. The second is the
   dangerous direction, and the skew compounds because `total_compacted_messages`
   sums across entries.

## Context: what compaction is here

Two distinct mechanisms, and the defects live in the seam between them.

**Tool-result stripping** (`strip_tool_results`) runs unconditionally on every
request. `agent/daemon/request.rs:50` strips the entire loaded history before
prompt assembly; each persisted `ToolResult`'s content becomes a pointer stub.
No rows are dropped — same rows, same order, same call ids, payload text
swapped. The current turn's own results are not in loaded history yet (they
live in `new_messages` inside the owned loop), so in practice prior rounds
become pointers while the live round stays full.

**Summarize compaction** fires only when the assembled prompt crosses
`context_window * threshold`. `compact()` splits history into `old` + `recent`
on a token budget, summarizes `old` through a sub-completion, persists an
`AgentCompactionEntry { summary, messages_compacted }`, and returns `recent`.
Later requests inject the summary as a prompt layer and skip the first
`Σ messages_compacted` rows. The durable transcript is never mutated;
compaction is a projection.

The provider view is:

```
[preamble] + [summaries] + drop_prefix(sanitize(strip(H))) + [prompt]
```

## Decisions

**Split-boundary direction: move earlier.** When the budget index straddles a
turn, the retained tail grows to include the whole assistant turn and its
results. Cost is at most one turn of over-retention; nothing is lost. Moving
the boundary later would summarize away a turn the budget wanted kept, which is
information loss in exchange for staying under budget. For provider-input
assembly, over-retaining is the correct failure direction. Documented at the
call site.

**`safeToReduce` counterpart: pure predicate + session-level resolver.**
`compaction::safe_to_reduce` mirrors the Lean predicate exactly and is what the
conformance test drives. The daemon backs it with a single "is any
`AgentResponse` in this session non-terminal?" query rather than per-message
`request_id` → response linkage. This is a sound over-approximation: if every
response in the session is terminal then every row's owning response is
terminal, so the modelled predicate holds. It can only err toward *unsafe*,
whose cost is a skipped compaction retried on the next request.

The one asymmetry: a row whose request has no `AgentResponse` row at all
(crashed run) reads unsafe in the model but safe under the session check. In
practice `sanitize` already deletes half-turns from crashed runs — an unpaired
call or an orphaned result never reaches compaction's input — so what survives
is a complete turn, which is safe to summarize. Recorded as a
`Boundaries.lean` entry carrying that argument, not left in a comment.

**`strip_tool_results` becomes idempotent.** Today the path strips twice
(`request.rs`, then again inside `compact()`), and the second pass re-stubs the
stub, so the pointer reports the *stub's* byte count instead of the real
output's. Recognizing an existing stub fixes the misleading count and makes
`provider_view = sanitize ∘ strip` a genuine idempotent reduction, which is
what the canonical-reduction argument in Part 1.2 needs.

## Part 1 — Lean

### 1.1 A real `strip`

Replace the identity `stubMessageKind` with a strip that rewrites the payload:

```lean
def stubKey (key : ToolResultKey) : ToolResultKey := { key with payloadHash := 0 }

def stripKind : MessageKind → MessageKind
  | .toolResult callId key => .toolResult callId (stubKey key)
  | k => k

def strip (msgs : List MessageRow) : List MessageRow := msgs.map stripRow
```

Theorems: `strip_idempotent`, `strip_length`, and the shape lemmas
`callsIn (strip l) = callsIn l` and `resolvedIn (strip l) = resolvedIn l`.
Strip touches payload; never constructors, never call ids. `stubKey` collapsing
distinct payload hashes is deliberate and harmless — `ViewCoherent` does not
require `UniqueToolResultKeys`.

### 1.2 One canonical reduction

```lean
def providerView (msgs : List MessageRow) : List MessageRow :=
  PromptAssembly.sanitize (strip msgs)
```

- `strip_sanitize_commute : strip (sanitize m) = sanitize (strip m)`.
  Both `dropOrphanedFrom` and `filterCallsBy` branch only on the constructor
  and call ids of `row.kind`, which `strip` preserves; `stripRow` commutes with
  `withKind` on assistant rows because `stripKind` fixes them.
- `providerView_idempotent` — from `strip_idempotent`, `strip_sanitize_commute`,
  and the existing `sanitize_idempotent` (needs `UniqueCallIds`).
- `providerView_sound : ProviderValid (providerView m)` — from `sanitize_sound`
  plus `strip` preserving `UniqueCallIds`.

This settles the question issue #993 raised as unproven, and settles it
affirmatively, so reordering the drop past sanitization is licensed rather than
assumed.

`normalize_assistant_content_order` (the third stage of the Rust `sanitize`)
reorders content *within* an assistant message and has no counterpart in the
Lean model, which has no content ordering. Explicit model boundary.

### 1.3 Prefix stability — the #3 obligation

Define `pendingAfter` — the pending-call set `dropOrphanedFrom` threads.

```lean
theorem providerView_append_of_turn_boundary
    (huniq : UniqueCallIds (a ++ b)) (hb : pendingAfter ∅ a = ∅) :
    providerView (a ++ b) = providerView a ++ tailView a b
```

With the production-shaped corollary: `b` begins with an ordinary row, because
every new request appends its user prompt before anything else. Then the
correspondence the fix actually rests on — the count the compaction writer
records names exactly the rows the next request's reader drops:

```lean
theorem compacted_prefix_correspondence
    (hstable : providerView (H ++ new) = providerView H ++ tail)
    (hsplit  : providerView H = dropped ++ old ++ recent) :
    (providerView (H ++ new)).drop (dropped.length + old.length) = recent ++ tail
```

Plus `drop_at_turn_boundary_preserves_ProviderValid`, which is what makes it
safe to drop the compacted prefix *after* sanitization without re-sanitizing:
because `total_compacted_messages` is always produced by `pairSafeBoundary`, the
drop lands on a turn boundary and the tail is still provider-valid.

### 1.4 `summarize` — the #1 obligation, and the modelled gate behind #2

```lean
abbrev SplitPolicy := List MessageRow → Nat        -- the token-budget index

def pairSafeBoundary (msgs : List MessageRow) (k : Nat) : Nat
  -- greatest j ≤ k with pendingAfter ∅ (msgs.take j) = ∅

def summarize (policy : SplitPolicy) (h : SummaryHandle) : TranscriptReducer
```

`pairSafeBoundary_le` and `pairSafeBoundary_pending_empty` establish the
adjustment is a retreat to a turn boundary. An `IsValidReducer` instance for
`summarize` then discharges:

- `preservesPairs` — the real pair-closure theorem, requiring
  `ActiveBlockValid v.messages`. That hypothesis holds precisely because the
  input is a `providerView`, so #1's fix *depends on* #3's fix; they compose
  rather than sitting side by side.
- `preservesOrder` — `drop` preserves `StrictlyIncreasingMessages`.
- `preservesSession`, `identityBelowGate`, `identityUnlessSafe`,
  `reapplyPreservesCoh`. `identityUnlessSafe` is the modelled `safeToReduce`
  gate that Part 2.4 gives a runtime counterpart.
- `summarize_messages_suffix : (summarize … v).messages <:+ v.messages` — the
  link between the recorded count and the retained rows.

And the negative theorem, without which `pairSafeBoundary` is decoration:

```lean
theorem raw_split_can_orphan :
    ∃ msgs k, ActiveBlockValid msgs ∧ ¬ PairsClosedInMessages (msgs.drop k)
```

a concrete witness discharged by `decide`.

`stripToolResultsReducer` and its vacuous `IsValidReducer` instance are retired
in favour of the real `strip`-based reducer. `identityReducer` stays — it is the
legitimate degenerate reducer for the below-gate case.

### 1.5 Contract JSON and coverage ledger

- Extend `CompactionReducerCase` with `split_index`, `safe_boundary`, and
  `retained_count`.
- New case groups: `summarize` (including a case where the raw budget index
  straddles a pair and the safe boundary retreats), and `provider_view_prefix`.
- `CoverageLedger.lean`: rows for the new groups; a `boundaryCoverage` entry for
  the session-level `safeToReduce` refinement; consumer registry entries in
  `crates/gents/tests/support/conformance_consumers.rs` in the same change.

## Part 2 — Rust

### 2.1 `provider_view`: one reduction, both call sites

```rust
pub fn provider_view(messages: Vec<Message>) -> (Vec<Message>, FileActivity)
// = sanitize_history_for_provider(strip_tool_results(messages))
```

`agent/daemon/request.rs` becomes view-then-drop; the post-drop `sanitize` call
disappears, licensed by `drop_at_turn_boundary_preserves_ProviderValid`.
`compact()` normalizes its own input through `provider_view` so
`messages_compacted` indexes the canonical space regardless of caller — free,
by idempotence.

`sanitize_history_for_provider` stays public: `loop_stream.rs` and
`tests/conformance/prompt_assembly.rs` both consume it directly.

### 2.2 `strip_tool_results` idempotent

Recognize an existing stub by exact shape (`[tool: ` prefix and the
`see DefraDB AgentToolCall for full output]` suffix) and pass it through
untouched, preserving the original byte count. This also replaces the
`tool_result_was_truncated` substring sniffing (defect 3 below).

### 2.3 `split_messages_for_summary` pair-safe

After picking `split_index` on the token budget, retreat to the greatest
`j ≤ split_index` where the pending-call set is empty, mirroring
`pairSafeBoundary`. The pending discipline matches `drop_orphaned_tool_results`:
an assistant message sets pending to its call ids, a tool result erases one, any
other message clears it.

### 2.4 `safe_to_reduce`

```rust
pub trait ResponseStatusIndex {
    fn status_of(&self, message: &Message) -> Option<ResponseStatus>;
}
pub fn safe_to_reduce(messages: &[Message], statuses: &impl ResponseStatusIndex) -> bool;
```

`ResponseStatus` is a small enum in `compaction` mirroring Lean's
`StreamingResponse.Status` (`streaming` / `complete` / `error`) with the same
terminal partition, so the conformance case can feed Lean's values straight in.

Daemon resolver: one query for non-terminal `AgentResponse` rows in the session;
any hit resolves every status to unknown, so the gate closes and compaction is
skipped with a `tracing::info`. The conformance case calls this function with
Lean's synthetic statuses instead of branching on `case.safe_to_reduce` itself.

### 2.5 Tool-stripping defect fixes

Found while auditing `compaction/history.rs`; in-module and low risk. Lettered
to keep them distinct from the three headline defects above.

**(a)** **`files_modified` is always empty.** `ToolCallInfo::from` classifies by tool
   name against `"write" | "edit" | "replace" | "apply_patch"`; the real tools
   are `write_file` and `edit_file` (`toolset/file_tools.rs:422,492`). Nothing
   matches, ever. `is_read` (`"read" | "cat" | "grep" | "search" | "find" |
   "query"`) matches exactly one real tool, `grep` — `read_file`, `list_files`,
   and `glob` are invisible. Masked to date because the LLM's own
   `files_read`/`files_modified` are merged on top. Fix: classify against the
   actual registered tool names, and add a unit test that fails if a file tool
   is added without being classified.

**(b)** **UTF-8 panic.** `truncate_tool_result_content` guards on
   `text.text.len() > max_chars` (bytes) then slices `&text.text[..max_chars]`,
   which panics when that index lands mid-codepoint. Unreachable under
   `StripThenSummarize` (stubs are short ASCII) but live under
   `CompactionStrategy::Summarize`, where raw tool output flows through. Fix:
   floor to a char boundary, as `toolset/edit_match.rs:484` already does.

**(c)** **`tool_result_was_truncated` substring sniffing.**
   `contains("truncated")` fires on any tool output mentioning the word.
   Subsumed by the stub marker from 2.2.

**(d)** **The stub carries no arguments.** Becomes
   `[tool: read_file(/src/compaction/history.rs), call_id: call-7, 4213 bytes
   — see DefraDB AgentToolCall for full output]`. The path is already extracted
   for `FileActivity`. Turns the stub from "a file was read" into "this file was
   read", which is most of what a pointer stub exists to do.

**(e)** **The `tool_calls` map is never scoped.** It accumulates across the whole
   history, so a reused call id from an earlier turn can label a later stub with
   the wrong tool name. Scope it per assistant turn.

## Part 3 — Tests

1. **Pair-closure regression** — history where the budget index lands between an
   assistant `ToolCall` and its `ToolResult`; assert the retained tail contains
   both.
2. **Sanitize-shrinks-inside-the-compacted-region regression (#3)** — build `H`
   where sanitize removes a row at or before the boundary, run two compaction
   rounds, assert the provider view after the second is exactly `recent ++ new`:
   no row both summarized and retained, no row dropped that was never
   summarized.
3. **Strip idempotence** — reapplication is a no-op and the byte count still
   describes the original output.
4. **`safe_to_reduce`** — unit tests; the conformance case calls production.
5. **Tool classification** — `write_file`/`edit_file` land in `files_modified`,
   `read_file`/`grep`/`glob`/`list_files` in `files_read`.
6. **UTF-8 pre-truncation** — multibyte content at the cut index does not panic.
7. **`run_timeline` round-trip after compaction** — reconstruct a request's
   event stream from persisted rows with a compaction entry present and assert
   it is unchanged. Compaction is a projection; it must not perturb the
   timeline.

**Acceptance demonstration.** Revert `pairSafeBoundary`, confirm both the Lean
proof and the conformance test fail, restore. Recorded in the PR body.

## Deployment note

Existing `AgentCompactionEntry` rows carry counts measured in
`sanitize(drop(strip H))` space; the new reader applies them in
`drop(sanitize(strip H))` space. These coincide unless sanitize removes a row
before the boundary — the exact case that is already broken today. No
migration. Called out in the PR body.

## Risk

`providerView_append` is the one proof not yet seen all the way through. If it
resists, the work stops and says so rather than shipping a `sorry` or a
plausible-looking reorder. This is provider-input assembly: a wrong fix here
corrupts context silently instead of failing loudly.

## Gates

```
cd crates/gents/proofs && lake build     # zero sorries
cargo test -p gents                      # full package suite, not --lib
cargo check --workspace --all-targets    # catches desktop/example crates
```
