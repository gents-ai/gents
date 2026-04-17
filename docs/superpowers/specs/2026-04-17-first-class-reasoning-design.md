# First-class reasoning support — design

**Status:** draft
**Date:** 2026-04-17
**Scope:** `crates/defra-agent`, `crates/defra-agent-protocol`, `crates/defra-agent-desktop`, `crates/defra-agent-cli` (schema registration only)

## Problem

The runtime already observes typed reasoning blocks from rig's completion stream (`StreamProcessor` merges `Reasoning` / `ReasoningDelta` items into `AssistantTurnAccumulator` and passes them into the persisted `CompletionMessage`). But the document surfaces clients watch — `AgentResponse.reasoning` and the projection helpers in `defra_agent_protocol::transcript` — flatten those typed blocks into a single rendered string. Encrypted reasoning payloads become the literal placeholder `"[encrypted reasoning]"`; signatures disappear; block ordering collapses.

That's fine for displaying thinking text, but it means:

- P2P peers and remote clients that walk the conversation off `AgentRequest` + `AgentResponse` documents cannot reconstruct the turn at fidelity sufficient to replay it into the model.
- The desktop has no structural handle to render reasoning distinctly (collapsible sections, encrypted-preserved indicator, interleaved thinking ↔ tool calls).
- "Tool calls inside thinking" (Anthropic interleaved extended thinking) has no first-class representation on the document surface.

Two related gaps compound the above: `AgentConversation` and `AgentSession` are duplicate session collections, and sub-rows (`AgentMessage`, `AgentToolCall`) do not link back to the `AgentRequest` that produced them — so scoping conversation-reconstruction queries across retries or supersedes is ambiguous.

## Goals

1. **Lossless reasoning in the authoritative store.** `AgentMessage.content` JSON faithfully round-trips every `ReasoningContent` variant (Text, Summary, Encrypted, Redacted) plus `Reasoning.signature` and `.id` — proven by conformance tests.
2. **Structured client visibility.** Clients can render reasoning per-block without parsing opaque JSON out-of-band; desktop shows collapsible thinking sections and distinguishes encrypted/redacted blocks.
3. **Conversation reconstruction on the response surface.** `AgentRequest` + `AgentResponse` + linked `AgentMessage` rows are sufficient to walk a session end-to-end; any reader can pick its fidelity level.
4. **No state-machine churn.** Streaming / request / persistence lifecycles are unchanged.

## Non-goals

- Retiring `AgentToolResult`. Deferred.
- Reasoning-specific compaction. Reasoning already passes through the existing compactor as opaque rig `Message` content; no change.
- New Lean proofs. Streaming-to-completion lifecycle is unchanged.

## Target document model

```
AgentSession  (absorbs AgentConversation; one row per session)
    session_id (unique), agent_did, agent_name, behavior_id,
    title, preview_text, status,
    started, ended, created_at, updated_at, latest_request_id

AgentRequest  (unchanged; one row per user prompt)

AgentResponse  (per request — client-facing header)
    response_key, request_id, agent_did, behavior_id, session_id,
    content              (rendered final text projection)
    reasoning            (rendered thinking projection)
    status, progress_seq, token_count,
    error_message, created_at, completed_at,
    first_message_sequence, last_message_sequence   [new]

AgentMessage  (authoritative per-turn)
    message_key (unique), session_id, request_id [new],
    sequence, role,
    content              (rig Message JSON — source of truth for typed blocks)
    reasoning            [new — rendered projection, derived at write time]
    timestamp

AgentToolCall  (projection)
    tool_call_key, session_id, request_id [new],
    message_sequence, tool_name, tool_call_id,
    args, result, status, started_at, completed_at

AgentToolResult  (unchanged)
```

### Sources of truth

- `AgentMessage.content` JSON is the authoritative record of typed assistant content (text, reasoning blocks with all variants, tool calls). This is what replays into the model on the next turn via the existing `session::load_history` path.
- `AgentResponse` is the per-request client-facing header. Rendered summaries (`content`, `reasoning`) are convenience projections; range linkage (`first_message_sequence`, `last_message_sequence`) lets any reader walk the authoritative messages for that request.
- `AgentMessage.reasoning` is a rendered projection derived from `transcript::extract_message_reasoning(&decoded_message)` at write time — queryable without decoding the content JSON.

### Conversation reconstruction (any reader)

```
for request in AgentRequest where session_id = X ordered by created_at:
    response = AgentResponse where request_id = request.request_id
    messages = AgentMessage where request_id = request.request_id ordered by sequence
    # messages[i].content is rig Message JSON with typed blocks
```

### Model replay (next turn)

Unchanged: `session::load_history(&node, session_id)` reads `AgentMessage` by session, ordered by sequence, decodes each row's `content` into a rig `Message`. Typed reasoning blocks (including encrypted payloads and signatures) flow through untouched.

## Schema changes

### `AgentSession` (absorbs `AgentConversation`)

```graphql
type AgentSession @branchable {
    session_id: String @index(unique: true)
    agent_did: String @index
    agent_name: String @index
    behavior_id: String @index
    title: String
    preview_text: String
    status: String @index
    started: DateTime
    ended: DateTime
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
    latest_request_id: String @index
}
```

Delete `crates/defra-agent-protocol/schemas/agent/agent_conversation.graphql`. All `AgentConversation`-specific fields migrate here.

### `AgentMessage`

```graphql
type AgentMessage @branchable {
    message_key: String @index(unique: true)
    session_id: String @index
    request_id: String @index
    sequence: Int @index
    role: String
    content: String
    reasoning: String
    timestamp: DateTime
}
```

- `content` remains rig `Message` JSON (source of truth).
- `reasoning` is empty-string for user/tool rows and for assistant rows without reasoning blocks.

### `AgentToolCall`

```graphql
type AgentToolCall @branchable {
    tool_call_key: String @index(unique: true)
    session_id: String @index
    request_id: String @index
    message_sequence: Int
    tool_name: String @index
    tool_call_id: String @index
    args: String
    result: String
    status: String
    started_at: DateTime
    completed_at: DateTime
}
```

### `AgentResponse`

```graphql
type AgentResponse @branchable {
    response_key: String @index(unique: true)
    request_id: String @index
    agent_did: String @index
    behavior_id: String @index
    session_id: String @index
    content: String
    reasoning: String
    first_message_sequence: Int
    last_message_sequence: Int
    status: String @index
    error_message: String
    token_count: Int
    progress_seq: Int
    created_at: String @index
    completed_at: String
}
```

Range linkage (not a list) — every request produces a contiguous block of `AgentMessage` rows ordered by `sequence`.

### Migration

Clean schema reset. No production data to preserve. Dev environments must wipe DefraDB volumes when pulling this change. Documented in PR body.

## Runtime changes

### A. Consolidate session writes

- Delete `crates/defra-agent/src/session/conversation.rs`.
- Merge its three upsert paths (session create on chat start, title/preview updates, `latest_request_id` update) into the existing session module that handles `AgentSession`.
- Callers updated: `session/query.rs:76` (reads), `lifecycle/recovery.rs:281` (recovery snapshot), `session` and `hook` tests, plus test-support scaffolding (`tests/support/mod.rs`, `tests/support/snapshots.rs`). Exact call-site inventory finalized during implementation.
- `resolve_behavior_id` helper stays, operates against `AgentSession`.

### B. `save_message` signature and derived `reasoning`

`crates/defra-agent/src/session/history.rs`:

```rust
pub(crate) async fn save_message(
    node: &EmbeddedNode,
    session_id: &str,
    request_id: &str,
    sequence: u32,
    role: &str,
    content: &str,
) -> Result<u32>
```

- Decodes the rig `Message` from `content`, calls `transcript::extract_message_reasoning`, includes the result in the upsert alongside `request_id` and the existing fields.
- Returns the sequence written (used by the stream writer to track the response range).
- Sole caller `hook/persistence.rs:52` updated; `request_id` is available via the admission/lifecycle scope already present in that code path.

### C. `AgentToolCall` writes gain `request_id`

`crates/defra-agent/src/session/tool_calls.rs` — upsert and update mutations include `request_id`. Callers have it in scope (tool execution happens inside request scope).

### D. Stream writer tracks message-range linkage

`crates/defra-agent/src/streaming.rs`:

- Per-`doc_id` buffer state gains `first_message_sequence: Option<u32>`, `last_message_sequence: Option<u32>`.
- New method `record_message_sequence(&self, doc_id: &str, sequence: u32)`. Sets `first` on the first call for a doc_id; sets `last` on every call. Cheap and idempotent within a request.
- Every flush and finalize mutation on `AgentResponse` includes `first_message_sequence` / `last_message_sequence` when present.
- `hook/persistence.rs` calls `stream_writer.record_message_sequence` after each successful `save_message`.

### E. Recovery

`lifecycle/recovery.rs:281` — update the GraphQL query that hydrates session snapshot during startup recovery from `AgentConversation` to `AgentSession`.

### F. Schema registration

- `crates/defra-agent-protocol/src/schemas.rs` — remove `AGENT_CONVERSATION_NAME`, ensure `AgentSession` appears in the registration list.
- `crates/defra-agent-cli/src/main.rs:182` and `crates/defra-agent-cli/src/commands/p2p/collections.rs:42` — update P2P collection lists similarly.

### G. Protocol crate row types (`crates/defra-agent-protocol/src/row.rs`)

- Delete `AgentConversationRow`.
- Expand `AgentSessionRow`: title, preview_text, status, created_at, updated_at, latest_request_id.
- Expand `AgentMessageRow`: `request_id`, `reasoning`.
- Expand `AgentToolCallRow`: `request_id`.
- Expand `AgentResponseRow`: `first_message_sequence`, `last_message_sequence`.

### H. Schema README

`crates/defra-agent-protocol/schemas/README.md` — reflect the collapse, linkage fields, and reasoning projection conventions. Add a "reasoning blocks" conformance section (see §4.6).

## Fidelity guarantees

### 4.1 rig `Message` serde round-trip (unit)

Location: `crates/defra-agent-protocol/src/transcript.rs` tests module.

Cases:

- `ReasoningContent::Text { text, id }` with `id = Some(_)` and `id = None`.
- `ReasoningContent::Summary(text)`.
- `ReasoningContent::Encrypted(bytes)` with non-empty opaque payload.
- `ReasoningContent::Redacted { data }`.
- `Reasoning` with `signature` set and unset.
- `Reasoning` with multiple `content` items of mixed kinds.
- Assistant message containing `[Reasoning, ToolCall, Text, Reasoning, ToolCall]` in that order.

For each: `serde_json::to_string` → `serde_json::from_str::<Message>` → assert decoded structure equality (typed, field-by-field).

Pins rig's serde behavior — a rig upgrade that silently drops a field breaks this test.

### 4.2 DefraDB persistence round-trip (integration)

Location: new `crates/defra-agent/tests/reasoning_fidelity.rs`.

Cases:

- Save assistant message with `[Text, Reasoning(Text), Reasoning(Encrypted), ToolCall]`, load via `load_history`, assert reconstruction.
- Save user + tool result + assistant-with-reasoning sequence within one session; load and assert ordering preserved by `sequence`.
- Load an `AgentMessage` row directly; assert `reasoning` column matches `extract_message_reasoning` of the decoded message.

### 4.3 Streaming interleave order (unit)

Location: extend `crates/defra-agent/src/agent/stream_processor/tests.rs`.

Scope: accumulator only; rendered streaming text is not re-tested here.

Cases:

- Feed fake stream: `ReasoningDelta("thinking about X")` → `Reasoning(complete block with id)` → `ToolCall` → `ReasoningDelta(continuation)` → `Reasoning(with signature)` → `Text("final")`.
- Assert `AssistantTurnAccumulator::take_message` produces content in order: `Reasoning, ToolCall, Reasoning, Text`.
- Delta merging: two `ReasoningDelta`s with the same `id` collapse into one block; two with different `id`s stay separate.
- Encrypted/Redacted variants received mid-stream round-trip into the final message.

### 4.4 `AgentResponse` ↔ `AgentMessage` linkage (integration)

Location: `crates/defra-agent/tests/reasoning_fidelity.rs`.

Cases:

- Mocked multi-turn request produces 3 assistant turns with interleaved tool calls; after finalize, read `AgentResponse` and assert `first_message_sequence` = first assistant turn's sequence, `last_message_sequence` = last assistant turn's sequence.
- Query `AgentMessage` filtered by `request_id`; assert rows match the response range.
- Edge case: single-turn request (first == last).

### 4.6 Conformance documentation

New section in `crates/defra-agent-protocol/schemas/README.md`:

- `AgentMessage.content` JSON is the source of truth for typed assistant blocks.
- Rendered projections (`reasoning` on `AgentMessage` and `AgentResponse`) are convenience views and may drop information (encrypted payloads render as placeholder; signatures are not surfaced).
- Consumers needing replay fidelity must decode `content` with `transcript::decode_persisted_message`.
- Do not upgrade rig without running the live-model conformance test (§4.7).

### 4.7 Live-model replay conformance (gated integration)

Location: `crates/defra-agent/tests/reasoning_fidelity_live.rs`.

Gating: matches the existing codebase convention for network-dependent tests (`#[ignore]` by default or a `live-inference-tests` cargo feature — whichever convention is in place at implementation time). Requires `ANTHROPIC_API_KEY`.

Cases:

- **Replay safety (signature round-trip).** Send a prompt eliciting extended thinking; save via `save_message`; load via `load_history`; submit a follow-up turn on the same session. Assert the provider accepts the replayed history without a signature-validation error.
- **Interleaved thinking + tools.** Prompt that triggers think → call a trivial mock tool → think → respond. Verify the interleaved assistant message round-trips and the follow-up turn succeeds.

### Explicit non-scope

- No Lean proof changes (no state-machine invariants change).

## Desktop changes (structured reasoning rendering)

### A. Row types

`AgentConversationRow` removed. `AgentSessionRow` gains the fields per §G. `AgentMessageRow` gains `request_id` and `reasoning`. `AgentToolCallRow` gains `request_id`. `AgentResponseRow` gains `first_message_sequence`, `last_message_sequence`.

### B. Structured block access helper

New in `defra_agent_protocol::transcript`:

```rust
pub enum RenderedBlock {
    Text(String),
    Reasoning(RenderedReasoningBlock),
    ToolCall(RenderedToolCall),
}

pub struct RenderedReasoningBlock {
    pub kind: ReasoningKind,     // Text | Summary | Encrypted | Redacted
    pub text: Option<String>,    // rendered text if available; None for Encrypted/Redacted
    pub has_signature: bool,
    pub id: Option<String>,
}

pub fn render_blocks(message: &Message) -> Vec<RenderedBlock>
```

Desktop (and CLI) call `render_blocks` on a decoded message to iterate typed blocks in order.

### C. Chat view rendering (`views/chat/transcript.rs`)

- For each assistant message, iterate `Vec<RenderedBlock>`.
- Text blocks: rendered inline as today.
- Reasoning blocks:
    - `Text` / `Summary`: collapsible "Thinking" section, collapsed by default, visually distinct (muted).
    - `Encrypted`: inline pill "Encrypted thinking preserved" (grayed, not expandable).
    - `Redacted`: inline pill "Redacted by provider" (grayed).
- Tool-call blocks: rendered as today.
- `has_signature = true` is not surfaced visually in v1 — it is a correctness property.

### D. Live streaming preview

During streaming (`AgentResponse.status == "streaming"`) the UI uses `AgentResponse.reasoning` directly (rendered text) to show thinking as it arrives. On finalization, the transcript view switches to block-structured rendering from `AgentMessage`. Two-phase render matches existing content handling (stream preview → final transcript).

### E. Controller / projection changes

`chat::projection` updated:

- Consumes the expanded `AgentSessionRow` instead of `AgentConversationRow`.
- `PersistedMessagePresentation` replaces `body_markdown` and `reasoning_markdown` with `blocks: Vec<RenderedBlock>`. All consumers migrate to iterating blocks.

### F. Audit

Existing `chat.reasoning` audit key retained; fires on reasoning updates during streaming.

### G. Desktop tests

`chat/projection/tests.rs` gains cases for reasoning-block rendering (text, encrypted pill, redacted pill, mixed with tool calls). Collapse/expand visual behavior is not automated.

## Implementation phasing

Single worktree, single PR, commits grouped per phase.

### Phase 0 — rig ecosystem review and update

1. Identify current rig pin in workspace `Cargo.toml`.
2. Review upstream: changelog since pin, open PRs touching `completion::message` or provider adapters (Anthropic, OpenAI), open issues on reasoning fidelity / signatures / interleaved thinking.
3. Read-through of `rig-core/src/completion/message.rs` reasoning types and the Anthropic/OpenAI provider adapters — confirm reasoning blocks with signatures and encrypted payloads are serialized correctly on the request side.
4. Decide update target: latest release, or cherry-pick an imminent PR via git rev with a note to move to the tag once merged.
5. Execute the bump; `cargo check` / `cargo test`; triage breakage.
6. Document the pinned rig rev in the PR body; note any upstream PRs being watched; note any known rig limitations affecting reasoning fidelity.

Output of Phase 0: a baseline rig version that Phases A–D build on.

### Phase A — Schema + runtime plumbing

1. GraphQL schema changes per §2.
2. Protocol crate row types per §3G.
3. Delete `session/conversation.rs`; merge into `session/sessions.rs`.
4. `save_message` new signature + derives `reasoning`; callers updated.
5. `AgentToolCall` writes gain `request_id`.
6. `StreamWriter` gains `record_message_sequence`; hook wires it after each save. `AgentResponse` writes include first/last sequence.
7. `lifecycle/recovery.rs` query updated.
8. CLI P2P collection lists updated.
9. Test fixtures migrated from `AgentConversationRow` to expanded `AgentSessionRow`.
10. Schema README updated.

End of Phase A: `cargo test` green.

### Phase B — Offline fidelity tests

11. §4.1 serde round-trip.
12. §4.2 DefraDB persistence round-trip in new `tests/reasoning_fidelity.rs`.
13. §4.3 stream interleave accumulator test.
14. §4.4 response↔message linkage test.
15. §4.6 conformance doc section.

### Phase C — Desktop structured rendering

16. `transcript::render_blocks` + `RenderedBlock` types + unit tests.
17. `chat::projection` rewritten to produce `Vec<RenderedBlock>`; `PersistedMessagePresentation` flat fields removed.
18. `views/chat/transcript.rs` renders blocks with collapsible reasoning, encrypted/redacted pills, interleaved tool calls.
19. Projection tests updated.

### Phase D — Live-model conformance (gated)

20. `tests/reasoning_fidelity_live.rs` per §4.7.

## Risks

- **Schema reset affects every dev environment.** PR body must call out the wipe requirement and, if there's reset tooling, reference it.
- **rig upgrade silently changing reasoning serde format.** Mitigated by §4.1 test; conformance doc warns against rig upgrades without §4.7 re-run.
- **Phase 0 upgrade itself surfaces breakage.** Mitigation: small test exercise of interleaved thinking against the bumped rig before committing.
- **Multi-turn + interleaved-thinking + tools is not heavily exercised today.** §4.3 may surface ordering bugs. Budget for a second pass on `stream_processor` if tests fail.
- **Desktop Phase C is larger than it looks.** Every consumer of `PersistedMessagePresentation.body_markdown` / `.reasoning_markdown` migrates to `blocks`. Expect a non-trivial desktop diff.

## Error handling

No structural changes. `save_message` retains retry semantics via `execute_mutation_with_retry`. `StreamWriter::record_message_sequence` is in-memory state and cannot fail in ways that affect persistence; the next flush to `AgentResponse` writes whatever range is currently recorded. Failures to decode a rig `Message` at save time are bugs (the decoder runs in-process on a value we just emitted) — logged and fail the save, preserving current behavior.

## Open questions (noted, not blocking)

- `AgentRuntime` vs `AgentSession` overlap is out of scope here but worth tracking for future consolidation.
- OpenAI o-series reasoning shape differs from Anthropic's; we rely on rig's normalization. If upstream divergence appears, §4.1 gains per-provider cases.
- Whether to surface `has_signature` visually in a later UX iteration (diagnostics panel, not chat).

## Dependencies

- Phase 0 pins a rig rev; all subsequent phases assume that pin.
- No other spec or PR is a hard prerequisite.

## Out of scope (explicit)

- Retiring `AgentToolResult`.
- Lean proof updates.
- Reasoning compaction strategies.
- OpenAI / non-Anthropic live-model fidelity tests (§4.7 is Anthropic-specific).
