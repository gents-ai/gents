# Forkable conversations — design

**Status:** draft
**Date:** 2026-04-21
**Scope:** `crates/defra-agent`, `crates/defra-agent-protocol`, `crates/defra-agent-cli`
**Desktop UI changes:** out of scope (follow-up plan)

## Problem

A conversation in defra-agent today is a linear sequence of `AgentMessage` rows keyed by a single `AgentSession`. There is no way to say "start a new conversation seeded with the first N turns of an existing one." Users who want to redo a turn with a different prompt, branch into two parallel comparisons, spawn a child conversation from partial context, or replay history under a different behavior have no primitive to lean on.

Codex solves this with a `thread/fork` operation that copies a prefix of a thread's rollout into a new thread and records a `forked_from_id` provenance link. Defra-agent's document-driven model is a natural fit for the same idea: each session is a set of documents keyed by `session_id`; forking is copying a prefix of those documents under a new `session_id` and recording the parent link.

## Goals

1. A `session::fork` primitive that materializes a new `AgentSession` + `AgentConversation` seeded with a prefix of a parent session's transcript, tool activity, and compactions.
2. Fork provenance is queryable: given a session, find its parent and its children.
3. Parent sessions are immutable under fork — no row in the parent changes when it is forked.
4. The fork operation is safe under the existing Request / Process / Persistence state machines — no Lean spec changes, no invariants at risk.
5. Retry and fork remain orthogonal concepts. The Request-lifecycle retry graph (`retry_parent_request`, `retry_root_request`, `superseded_by_request`) and the Conversation-level fork graph do not share fields or interact.
6. Same-principal-only access: a fork's source must share `agent_did` with the caller's identity.

## Non-goals

- Rollback / truncation of an existing conversation. Fork is strictly additive. There is no "undo turn N and everything after" operation in this design.
- Shared-prefix storage (child logically shares parent's rows). Copy-on-fork is the chosen model.
- Ephemeral (in-memory only) forks. All forks are persisted.
- Cross-principal forks / session import. The source's `agent_did` must equal the caller's identity.
- A fork meta-tool exposed to LLMs. Agent-initiated forks call `session::fork` via internal Rust API (for example via a future meta-tool), not via a new tool surface in this spec.
- Desktop UI. The schema additions are sufficient for a future desktop plan to land "fork from here" affordances and sibling navigation.
- DefraDB `@branchable` CRDT features. Those are commit-graph branching at the document level (for sync/merge of concurrent writes). Conversation forks are a different, higher-level concept and are not built on `@branchable`.

## Use cases (in scope)

- **Redo from here.** User forks at user-turn N with an edited prompt or different sampling; gets an independent branch without touching the original.
- **Branch and compare.** User forks at user-turn N with a different `behavior_id` to compare two agent configurations side-by-side on the same history.
- **Sub-agent spawn.** An agent, producing an `AgentRequest` targeted at itself or another agent sharing its principal, first calls `session::fork` (for example via a future meta-tool) to obtain a seeded `session_id`, then writes the `AgentRequest` to that session using the existing agent-to-agent mechanism. No new primitive.
- **Replay / debug.** A developer forks at user-turn N and submits a probe request (possibly with a different behavior) to observe divergence from the parent's history.

## Conceptual model

A **fork** is a new `AgentSession` seeded with a prefix of an existing session's transcript. The parent is immutable — nothing about it changes when it is forked. The child owns a fresh session/conversation identity, a copy of the parent's messages / tool calls / tool results / compaction entries up to a user-turn boundary, and a provenance link back to the parent.

Fork is:

- **Additive.** Never destructive. No rollback, no parent truncation.
- **Idle-only.** The parent must have no `AgentRequest` in a non-terminal state (not `Pending`, not `Claimed`, not `Processing`, not `InputRequired`). This avoids racing against an active runtime that is still writing the parent's rows.
- **User-turn-bounded.** The fork cut-point is `fork_at_user_turn: u32`, a 0-based index of the parent's committed user messages. The child's prefix includes every parent message with `sequence` strictly less than the sequence of that Nth user message. Forks at mid-tool-call or mid-turn are not allowed.
- **Copy-on-fork.** The child's copied rows are independent, newly inserted documents. No shared storage with the parent. Parent deletion is safe.
- **Same-principal-only.** `source.agent_did == caller_agent_did`. The child inherits the principal; it cannot escape the audit boundary.
- **Behavior-swappable.** The child may set a different `behavior_id`, which transitively swaps the backend, model, tool selection, and inference profile via existing configuration resolution.
- **Orthogonal to retry.** `retry_parent_request` / `retry_root_request` / `superseded_by_request` are within-session Request-lifecycle concerns. Fork is cross-session and does not set, read, or modify these fields.
- **Out of formal spec.** The Lean state machines (Process, Request, Persistence) do not see fork. Fork's correctness is a structural invariant on document shape, enforced by Rust tests (see "Structural invariant" below).

## Operational considerations

**Behavior swap and tool-availability in replayed history.** When the child forks with a different `behavior_id` whose `ToolSelection` differs from the parent's, the copied `AgentMessage` and `AgentToolCall` rows may reference tools that are not in the child's current tool surface. This is expected and correct: history is a record of what happened under the parent's configuration, not a claim that those tools are currently available. The model sees the historical tool calls in context on its next turn; the current turn's tool surface is determined by the child's `ToolSelection` and `ToolCeiling`. If the model tries to call a tool that is no longer in the surface, the existing tool-surface-resolution path rejects it as it would in any other session.

## Target document model

`AgentConversation` grows three fields. No other collection changes.

```graphql
# crates/defra-agent-protocol/schemas/agent/agent_conversation.graphql
type AgentConversation @branchable {
    session_id: String @index(unique: true)
    agent_name: String @index
    agent_did: String @index
    behavior_id: String @index
    title: String
    preview_text: String
    status: String @index
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
    latest_request_id: String @index

    # NEW — fork provenance (empty/null for root conversations)
    forked_from_session_id: String @index
    fork_at_user_turn: Int
    forked_at: DateTime
}
```

The new fields live on `AgentConversation` rather than `AgentSession` because:

1. `AgentConversation` is the user-facing summary layer (title, preview, status). UIs listing conversations already query this collection; provenance belongs alongside that metadata.
2. `AgentSession` is used in low-level runtime paths and is kept lean on purpose.
3. `forked_from_session_id` is indexed so "children of session X" is one direct query: `AgentConversation(filter: { forked_from_session_id: { _eq: "X" } })`.

**Ancestry.** Each conversation knows only its immediate parent. Full ancestry is computed by walking the chain (child → parent → grandparent → ...). No transitive-closure column; depths are expected to be small.

**Coordination note.** A separate draft spec (`2026-04-17-first-class-reasoning-design.md`) proposes absorbing `AgentConversation` into `AgentSession`. If that lands first, these three fields move to `AgentSession` unchanged — no other design changes needed. If this fork spec lands first, the reasoning spec carries the fields over when it consolidates the two collections.

**Schema migration.** Adding fields to an existing `@branchable` collection is a schema change. Existing `AgentConversation` rows (created before this spec ships) will read back with null/empty values for `forked_from_session_id`, `fork_at_user_turn`, and `forked_at`. That is the correct representation of a root conversation: no parent. No data migration of existing rows is required. Queries that filter by `forked_from_session_id _eq "some_id"` naturally exclude empty-valued rows.

## The `session::fork` function

Single public entry point, new module `crates/defra-agent/src/session/fork.rs`, re-exported from `session.rs` and from the crate root.

```rust
pub struct ForkParams<'a> {
    pub source_session_id: &'a str,
    pub fork_at_user_turn: u32,       // 0-based index into parent's committed user messages
    pub caller_agent_did: &'a str,    // enforces same-principal ACL
    pub target_behavior_id: Option<&'a str>,  // None = inherit parent's behavior
}

pub struct ForkOutcome {
    pub session_id: String,           // new child session_id
    pub copied_messages: u32,
    pub copied_tool_calls: u32,
    pub copied_tool_results: u32,
    pub copied_compaction_entries: u32,
}

pub async fn fork(node: &EmbeddedNode, params: ForkParams<'_>) -> Result<ForkOutcome>;
```

### Execution steps (ordered)

1. **Load and validate source.**
   - Fetch parent `AgentSession` and `AgentConversation` by `session_id`. Both must exist.
   - Verify `parent.agent_did == params.caller_agent_did` (see "ACL enforcement" below for the v1 trust model). Mismatch → `ForkNotSameAgent`.
   - Verify parent has **zero** `AgentRequest` rows with `lifecycle_state` in any non-terminal state. The persisted state string values are lowercase (`PersistedLifecycleState::as_str` in `crates/defra-agent/src/lifecycle.rs`): non-terminal values are `"pending"`, `"claimed"`, `"processing"`, `"inputRequired"`; terminal values are `"completed"`, `"failed"`, `"superseded"`, `"dead"`. The busy-check is a DefraDB filter for `lifecycle_state _in ["pending", "claimed", "processing", "inputRequired"]` returning zero rows. Mismatch → `ForkSourceBusy`.

2. **Compute fork cut sequence and cut timestamp.**
   - Query parent's `AgentMessage` rows ordered by `sequence` ascending.
   - Find the message that is the Nth (0-based) occurrence of `role == "user"`, where N = `fork_at_user_turn`. Call its sequence `cut_seq` and its `timestamp` `cut_ts`.
   - If fewer than `N + 1` user messages exist → `ForkAtUserTurnOutOfRange`.
   - `AgentMessage.sequence` is 1-indexed (confirmed in `crates/defra-agent/src/hook/persistence.rs`: `state.sequence += 1` before use). So `fork_at_user_turn = 0` typically yields `cut_seq = 1`, and the copied prefix `sequence < 1` is empty — a legal "fork before everything" that produces a child inheriting only behavior and provenance.
   - The child's copied prefix includes every `AgentMessage` / `AgentToolCall` with sequence strictly less than `cut_seq`, and every `AgentToolResult` / `CompactionEntry` with `created_at` strictly less than `cut_ts`. Timestamp-based filtering is safe here because fork is idle-only (step 1 guarantees no concurrent writes to the parent), so there is no race between "what's committed at `cut_ts`" and "what a still-running turn is about to commit."

3. **Resolve child behavior.**
   - If `target_behavior_id` is `Some`:
     - Verify an `AgentBehavior` with that id exists. Missing → `ForkBehaviorNotFound`.
     - Verify `target_behavior.agent_did == parent.agent_did`. Mismatch → `ForkBehaviorNotOwnedByPrincipal`. This prevents a caller from escaping the principal boundary by swapping to a behavior owned by a different principal; it reinforces the "principal inherited" guarantee at the point where behavior is resolved.
   - Otherwise inherit `parent.behavior_id` (whose `agent_did` equals `parent.agent_did` by existing config-resolution invariants).
   - Principal (`agent_did`) is always inherited. The child's `AgentConversation.agent_did == parent.agent_did == resolved_behavior.agent_did`.

4. **Copy rows first.** For each collection below, query matching parent rows and insert new rows under the child's generated `session_id`. No `AgentSession` or `AgentConversation` row for the child is written yet.

5. **Create child `AgentSession` and `AgentConversation` last.** Creating these last ensures that a partial failure during step 4 leaves orphan rows that are invisible to any normal query (no session/conversation exists to surface them). A future janitor can GC orphans; not required for v1 correctness.

6. **Return** `ForkOutcome`.

### Per-collection copy rules

| Collection | Filter on parent | Key remap | Field preservation |
|---|---|---|---|
| `AgentMessage` | `sequence < cut_seq` | `message_key = "{child_session_id}:{sequence}"` | `timestamp` preserved from parent |
| `AgentToolCall` | `message_sequence < cut_seq` | `tool_call_key = "{child_session_id}:{tool_call_id}"` | `started_at`, `completed_at` preserved |
| `AgentToolResult` | `created_at < cut_ts` | DefraDB generates a fresh `_docID`; no application-level key | `created_at` preserved; `agent_did` inherited |
| `CompactionEntry` | `created_at < cut_ts` | `compaction_key = "{child_session_id}:{sequence}"` (parent's compaction sequence preserved in the key and in the `sequence` field) | `created_at` preserved |

**Not copied:** `AgentRequest`, `AgentResponse`, `AgentSession` (parent's), `AgentConversation` (parent's). The child's `AgentSession` and `AgentConversation` are created fresh in step 5. `AgentRequest` / `AgentResponse` are per-turn lifecycle records whose states are terminal and owned by the parent; duplicating them under new IDs would produce data without meaning.

**Timestamp preservation rationale.** Copied rows retain their original timestamps (`AgentMessage.timestamp`, `AgentToolCall.started_at`/`completed_at`, `AgentToolResult.created_at`, `CompactionEntry.created_at`). The rows represent a faithful reproduction of historical events, not new documents written at fork time. The fork's own timeline lives on `AgentConversation.forked_at`.

**`AgentToolResult` schema note.** `AgentToolResult` (see `crates/defra-agent-protocol/schemas/agent/agent_tool_result.graphql`) has no `tool_call_id` or `sequence` / `message_sequence` field — it links to a tool call only by `(session_id, tool_name, tool_input)` and is used as a spill store for truncated tool output (written from `crates/defra-agent/src/truncation/spill.rs`; not read during history replay in `session::load_history`). Copying by `created_at < cut_ts` captures every spill that existed before the fork point without needing a join.

**`CompactionEntry` schema note.** `CompactionEntry.sequence` is an independent monotonic counter for compaction events, not a pointer into `AgentMessage.sequence` (see `crates/defra-agent/src/session/compaction_entries.rs`: `sequence = previous.map_or(1, |entry| entry.sequence + 1)`). It has no explicit start/end pointer into the message stream; only a `messages_compacted` count. Filtering by `created_at < cut_ts` is the correct and only reliable rule. Child's compaction sequence continues from the largest copied sequence, so new post-fork compactions extend the sequence naturally without conflict.

### Atomicity

DefraDB mutations are per-document; there is no multi-document transaction. The design's guarantee is weaker than full atomicity and explicit:

- **Happy path:** all rows copied, then session/conversation created. All-or-nothing from the perspective of any query that joins through `AgentSession` or `AgentConversation`.
- **Partial crash during copy:** orphan rows reference a `session_id` with no session/conversation document. These are invisible to normal queries because every existing query filters by a known `session_id` or joins through `AgentSession`.
- **Retry semantics:** the caller may retry `fork` with the same inputs. Each retry generates a new `session_id` UUID, so retries produce independent child sessions rather than idempotent deduplication. Callers that want at-most-once must track the returned `session_id` themselves. An orphan-GC janitor is future work, noted as an open issue.

### Concurrent forks

Two forks of the same parent (same or different `fork_at_user_turn`) execute independently. Both only read from the parent; neither writes to the parent. The children receive disjoint `session_id`s and proceed without coordination.

### ACL enforcement (v1 trust model)

The same-principal check — `parent.agent_did == caller_agent_did` — is enforced at the Rust function boundary, not inside DefraDB's ACL layer. `caller_agent_did` is a parameter the caller passes in; it is not independently verified against the node's signing identity. This means:

- For CLI callers, `caller_agent_did` is read from the local node identity / configuration. The `defra-agent session fork` command must look up the caller's identity the same way other identity-sensitive commands do today and pass it through.
- For runtime callers (a future meta-tool, a scheduler path), the caller must pass the identity of the principal currently executing, which the runtime already has in its request context.
- For in-process library callers, this is a trust-your-caller boundary.

Implication: a hostile Rust caller with direct access to `session::fork` could pass any `caller_agent_did` and bypass the check. A future hardening pass — tracked in Open Issues — derives `caller_agent_did` from the node's signing identity inside `session::fork` itself, or relies on DefraDB ACLs to make reads of a foreign-principal session fail, at which point the parameter becomes redundant. Either path is non-breaking because it only tightens the check.

### DefraDB ACL readiness

The fork primitive is compatible with a future DefraDB ACL policy of the form "actor DID must match row's `agent_did`" without any additional schema reservations. Every identity-sensitive check fork performs goes through a collection that already carries `agent_did`:

| Fork-time check | Collection read | Has `agent_did`? |
|---|---|---|
| "does the source exist and belong to caller?" | `AgentConversation` | ✓ |
| "is the parent idle?" (busy-check) | `AgentRequest` | ✓ |
| "does target behavior belong to the principal?" | `AgentBehavior` | ✓ |

Writes on fork create a new `AgentSession` + `AgentConversation`. `AgentConversation.agent_did` is set to the inherited principal; a DefraDB ACL policy on writes (actor DID must equal the write's `agent_did`) is sufficient to gate that under the same-principal rule.

**Orthogonal schema gap (not introduced by this spec).** `AgentSession`, `AgentMessage`, `AgentToolCall`, and `CompactionEntry` do not currently carry an `agent_did` field. Row-level ACLs on those collections would need either (a) a schema addition to add `agent_did` system-wide, or (b) a DefraDB ACL policy capable of resolving `agent_did` via a join to `AgentConversation` keyed by `session_id`. This spec does not close that gap — it is a broader schema-modernization concern affecting every write path (the persistence hook, tool-call hook, compaction layer), not just fork. The fork implementation is forward-compatible with either resolution: when `agent_did` is added to those collections, fork's copy step will populate the field with the child's (inherited) `agent_did` value in the same mutation — a one-line change. No design element in this spec conflicts with that future addition.

**No new DID fields reserved in this spec.** A `forked_by_did` column on `AgentConversation` was considered and rejected: under the same-principal-only rule, it would always equal `agent_did`, making it redundant. If cross-principal forks are ever added, that spec can introduce the field and populate it on newly-created rows without migration — existing rows correctly represent same-principal forks where `forked_by_did == agent_did` by convention.

## Error taxonomy

| Error | Condition | Class |
|---|---|---|
| `ForkSourceNotFound` | parent `AgentSession` or `AgentConversation` missing | caller error |
| `ForkNotSameAgent` | `source.agent_did != caller_agent_did` | caller error (ACL) |
| `ForkSourceBusy` | parent has any `AgentRequest` with non-terminal `lifecycle_state` | retryable (caller waits or supersedes) |
| `ForkAtUserTurnOutOfRange` | fewer than `fork_at_user_turn + 1` user messages in parent | caller error |
| `ForkBehaviorNotFound` | `target_behavior_id` set but no matching `AgentBehavior` | caller error |
| `ForkBehaviorNotOwnedByPrincipal` | `target_behavior.agent_did != parent.agent_did` | caller error (principal boundary) |
| `ForkCopyFailed(inner)` | a DefraDB mutation during the copy step failed | retryable |

Transient DefraDB errors within each copy mutation are handled by the existing `session::retry::execute_mutation_with_retry` helper. `ForkCopyFailed` surfaces only after those retries are exhausted.

## Surfaces

### CLI (`defra-agent-cli`)

New subcommand under `defra-agent session`:

```text
defra-agent session fork \
  --from <SOURCE_SESSION_ID> \
  --at-user-turn <N> \
  [--behavior <BEHAVIOR_ID>]
```

Prints the new `session_id` on success. Callers compose it with existing subcommands: pipe into `defra-agent request submit --session-id <NEW>` to land a first post-fork prompt.

No `--ephemeral` flag. No path-based source — defra-agent stores everything in DefraDB, not rollout files.

### Rust library

`pub use session::{fork, ForkParams, ForkOutcome}` re-exported from `crates/defra-agent/src/session.rs` and from the crate root. This is the entry point for the desktop, for an eventual agent meta-tool, and for tests.

### Agent-initiated forks (sub-agent spawn)

An agent producing an `AgentRequest` targeting itself or another agent in its principal:

1. Calls `session::fork` (via a future meta-tool or direct Rust helper) to obtain a child `session_id`.
2. Writes an `AgentRequest` bound to that child `session_id` using the existing agent-to-agent request mechanism.

The meta-tool surface — what the agent sees as a tool — is a separate design. This spec commits only to making `session::fork` callable.

### Desktop (noted, not built here)

The schema additions are sufficient for a future desktop plan to land:

- "Fork from here" affordance on each user message in the transcript.
- Fork badge on `AgentConversation` list entries pointing to their parent.
- Behavior picker at fork time for the branch-and-compare use case.

That work is a separate plan.

## Testing strategy

### Structural invariant test — `crates/defra-agent/tests/fork_invariants.rs` (new)

Core invariant: **a forked session's copied rows are a prefix-match of its parent's; post-fork rows do not exist; parent rows are byte-identical before and after the fork.**

The test:

1. Builds a parent session with N user turns + assistant turns + tool calls (some with results) + compaction entries (one window straddling multiple user turns).
2. Forks at several `fork_at_user_turn` values: 0, mid-range, exactly N, out-of-range (expect `ForkAtUserTurnOutOfRange`).
3. For each successful fork, asserts:
   - **Prefix match:** child's `AgentMessage[sequence < cut_seq]` content equals parent's, key-remapped to the child's `session_id`. Timestamps preserved byte-identical.
   - **Tool-call prefix match:** child's `AgentToolCall[message_sequence < cut_seq]` equals parent's, key-remapped.
   - **Timestamp-based spill/compaction cutoff:** child's `AgentToolResult` / `CompactionEntry` rows each have `created_at < cut_ts` (the parent's cut_seq message timestamp); no row with `created_at >= cut_ts` is present.
   - **Post-fork emptiness:** child has zero `AgentMessage` / `AgentToolCall` rows with `sequence >= cut_seq`.
   - **Disjoint keys:** child's `message_key`, `tool_call_key`, `compaction_key` do not overlap the parent's (session_id portion differs).
   - **Conversation provenance:** `child.forked_from_session_id == parent.session_id` and `child.fork_at_user_turn` matches the input; `child.forked_at` is set.
   - **No request/response copied:** child has zero `AgentRequest` and zero `AgentResponse` rows.
   - **`fork_at_user_turn = 0` corner case:** child has zero copied rows in every collection except `AgentSession` and `AgentConversation`; child is a legally empty fork whose provenance still points to the parent.
4. **Parent unchanged:** full row snapshot of the parent before and after each fork, asserted byte-equal.
5. Negative cases covering each error in the taxonomy, including:
   - `ForkBehaviorNotOwnedByPrincipal`: fork attempted with `target_behavior_id` pointing to an `AgentBehavior` whose `agent_did` differs from the parent's. Child must NOT be created; no copied rows must exist.

### State-machine conformance addition

`crates/defra-agent/tests/state_machine_conformance.rs` gets one new case:

- A fork performed while a parent session has only terminal-state `AgentRequest` rows does not transition any state in the Process, Request, or Persistence lifecycle state machines. Snapshot the parent's request/response/conversation state before and after the fork; assert byte-equality. This asserts that fork is orthogonal to the Lean-proven invariants.

### Concurrent fork test

Two forks of the same parent (same and different `fork_at_user_turn`) issued concurrently both succeed, produce disjoint children, and leave the parent unchanged. Verifies the "parent is read-only under fork" claim.

### Not tested in v1

- Fork-of-fork-of-fork ancestry depth beyond two. Fork-of-fork is covered by one case in the invariants test.
- P2P replication of forked sessions. Existing replication treats the child's new rows like any other session creation; no fork-specific replication behavior.
- Orphan-row GC. Partial-crash orphans are tolerable in v1 (invisible to queries); GC is future work.

## Open issues

- **ACL hardening.** v1 enforces same-principal via a Rust function-boundary parameter check (see "ACL enforcement"). A future hardening derives `caller_agent_did` from the node's signing identity inside `session::fork`, or relies on DefraDB ACLs to make foreign-principal reads fail, at which point the parameter is redundant. Non-breaking when it lands (the check only tightens). See "DefraDB ACL readiness" for the collections and fields that policy would pivot on.
- **Row-level ACL on transcript collections.** `AgentSession` / `AgentMessage` / `AgentToolCall` / `CompactionEntry` do not carry `agent_did` today, so row-level DefraDB ACL on those collections is not directly expressible. Resolutions (schema addition or join-capable ACL policy) are a system-wide schema-modernization concern; the fork spec is forward-compatible with either path.
- **Orphan GC.** A janitor that finds `AgentMessage` / `AgentToolCall` / `AgentToolResult` / `CompactionEntry` rows whose `session_id` has no `AgentSession` or `AgentConversation` and removes them. Deferred.
- **Content-addressed dedup for large copied tool outputs.** `AgentToolResult.output_text` may be large; copy-on-fork duplicates it. A later blob-dedup layer could transparently deduplicate without changing this logical model. Deferred.
- **Meta-tool exposure.** A `fork_conversation` meta-tool callable by an LLM is the natural way to wire sub-agent spawn. Its surface, scoping rules, and how it interacts with the existing agent-to-agent `AgentRequest` path are a separate design.

## Success criteria

1. `session::fork` compiles, ships as `pub use session::fork`, and is invoked from the new CLI subcommand.
2. The structural invariant test passes with the scenarios listed above.
3. State-machine conformance tests pass unchanged except for the one new no-op case in `state_machine_conformance.rs`.
4. A human can: create a parent session via `chat`, let it produce a few user + assistant turns, run `defra-agent session fork --from <ID> --at-user-turn 1`, receive a new `session_id`, and submit a new request against that session that begins where the fork cut.
