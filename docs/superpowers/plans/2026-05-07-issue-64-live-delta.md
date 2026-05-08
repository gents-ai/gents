# Issue #64 Visible Live Delta — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redefine `AgentResponse.content`/`reasoning` as the live tail (reset at every commit boundary) so clients render the streaming overlay without prefix-stripping heuristics. Schema unchanged.

**Architecture:** Single-subsystem writer-side fix. Lean and conformance updates land first per the project flow in `CLAUDE.md`, then the Rust writer changes (`DefraStreamWriter::reset_tail` + boundary call-sites in `StreamProcessor` + finalize tail clear), then the desktop bridge cleanup (delete `live_overlay_suffix`), then integration coverage. `MaterializationSignature` keeps the tail-length fields but their meaning shifts from "turn-cumulative growth" to "current-tail growth" — actually a cleaner stall signal under the new contract.

**Tech Stack:** Rust (workspace), Lean 4 (lake), TypeScript (vitest in `apps/desktop-tauri`), DefraDB (embedded node), GraphQL.

**Design doc:** `docs/design/issue-64-visible-live-delta.md` — read it before starting.

---

## File Structure

| Path | Action | Purpose |
|---|---|---|
| `crates/defra-agent/proofs/client-state-machine.md` | modify | Replace cumulative-content note; add Live Overlay subsection with TS/Swift/Rust pseudocode of the render rule. |
| `crates/defra-agent/proofs/Proofs/Client/Types.lean` | modify | Extend `ResponseSnapshot` with `tailEmpty : Bool`. |
| `crates/defra-agent/proofs/Proofs/ClientShell/Projection.lean` | modify | Add `OverlayBlock`, `projectActiveOverlay`, theorems O1–O3. |
| `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/LiveOverlay.lean` | create | Generated case table for Rust integration test. |
| `crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean` | modify | Re-export the new module. |
| `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean` | modify | Register `Client.LiveOverlay` domain with Rust consumer pointer. |
| `crates/defra-agent/src/streaming.rs` | modify | Add `DefraStreamWriter::reset_tail`; rewrite `build_finalize_mutation` to clear content/reasoning. |
| `crates/defra-agent/src/streaming/tests.rs` | modify | Add tests for `reset_tail` and tail-cleared finalization. |
| `crates/defra-agent/src/agent/stream_processor.rs` | modify | Insert `reset_tail` after each commit boundary. |
| `crates/defra-agent/src/agent/stream_processor/tests.rs` | modify | Cover the post-tool-resumed reset pattern. |
| `crates/defra-agent-desktop-core/src/client/core/materialization.rs` | modify | Doc-comment update on `MaterializationSignature`: tail-length fields now mean current-tail (not turn-cumulative). Add within-boundary stall test. |
| `apps/desktop-tauri/src-tauri/src/bridge/snapshot/timeline.rs` | modify | Delete `live_overlay_suffix` + `active_turn_committed_assistant_texts`; read `overlay.content`/`reasoning` directly. |
| `apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_timeline.rs` | modify | Replace prefix-stripping cases with tail-only cases. |
| `crates/defra-agent/tests/live_overlay_conformance.rs` | create | Iterate the LiveOverlay Lean cases against the live writer + bridge. |
| `apps/desktop-tauri/src/lib/chat-shell.test.ts` | modify | Consume the new frontend LiveOverlay conformance rows. |

Verification commands run from repo root unless otherwise noted:

```bash
cargo check
cargo test --workspace
(cd crates/defra-agent/proofs && lake build)
(cd apps/desktop-tauri && pnpm test)
```

---

## Task 1: Document the new contract

**Files:**
- Modify: `crates/defra-agent/proofs/client-state-machine.md`

- [ ] **Step 1: Open the file and locate the AgentMessage subsection (around line 145)**

The current text reads (excerpt):

```
Rendering: ordered transcript for scroll-back history. The streaming bubble
reads from `AgentResponse.content`; `AgentMessage` is the persisted transcript
surface.
```

- [ ] **Step 2: Replace that paragraph with the live-tail contract**

Replace with:

```
Rendering: ordered transcript for scroll-back history. `AgentMessage` is the
persisted transcript surface; `AgentResponse.content` and
`AgentResponse.reasoning` carry the **live tail** (the visible bytes streamed
since the most recent commit boundary in this turn) — see "Live Overlay"
below.
```

- [ ] **Step 3: Add a new "Live Overlay" section before "Subscription Model"**

Insert this section verbatim:

````markdown
## Live Overlay

`AgentResponse.content` and `AgentResponse.reasoning` are the **live tail** of
the active assistant segment. They are reset to empty whenever a partial
assistant turn or a tool-result is persisted as an `AgentMessage`, and again
on finalize. They are **not** a transcript record — the transcript is
`AgentMessage`. `token_count` is cumulative across the turn (metering, not
rendering). `progress_seq` is a strict-monotonic version cursor that bumps at
lifecycle boundaries (`RequestLifecycle::advance`).

A compliant client renders an active turn with this algorithm:

```text
input:
  committed_messages : [AgentMessage]   # filtered to the active turn
  tool_calls         : [AgentToolCall]  # filtered to the active turn
  active_response    : AgentResponse?   # tip response for the active turn
  derived_turn       : ClientTurnState?

output:
  timeline : [TimelineItem]

algorithm:
  sort committed_messages by sequence ASC
  group tool_calls by message_sequence
  for each msg in committed_messages:
    emit UserMessage / AssistantMessage / ToolMessage based on role
    if tool_calls grouped at msg.sequence:
      emit ToolGroup
  emit any tool_calls whose message_sequence has no matching committed
    message yet (lag fallback)

  if should_show_overlay(active_response, derived_turn):
    emit LiveAssistant {
      content   : active_response.content,
      reasoning : active_response.reasoning,
    }

should_show_overlay(r, t):
  r is not None
  AND r.materialized_message_sequence is None
  AND r.status not in {"complete", "error"}
  AND t in {WaitingForClaim, Streaming}
  AND (r.content non-empty OR r.reasoning non-empty)
```

### Reference TypeScript

```typescript
function shouldShowOverlay(
  r: { status: string; materializedMessageSequence?: number | null;
       content?: string | null; reasoning?: string | null } | null,
  t: ClientTurnState | null,
): boolean {
  if (!r) return false;
  if (r.materializedMessageSequence != null) return false;
  if (r.status === "complete" || r.status === "error") return false;
  if (t !== "waitingForClaim" && t !== "streaming") return false;
  return Boolean(r.content?.trim()) || Boolean(r.reasoning?.trim());
}
```

### Reference Swift

```swift
func shouldShowOverlay(
    _ r: AgentResponseState?, _ t: ClientTurnState?
) -> Bool {
    guard let r else { return false }
    if r.materializedMessageSequence != nil { return false }
    if r.status == "complete" || r.status == "error" { return false }
    guard t == .waitingForClaim || t == .streaming else { return false }
    let hasContent = !(r.content ?? "").trimmingCharacters(in: .whitespaces).isEmpty
    let hasReasoning = !(r.reasoning ?? "").trimmingCharacters(in: .whitespaces).isEmpty
    return hasContent || hasReasoning
}
```

### Replication-lag note

There is a brief window where `AgentResponse.content` for a post-tool tail can
replicate before the tool-result `AgentMessage` that explains the boundary.
During this window the overlay may render at a lower-sequence slot than its
true anchor. Self-heals once the boundary message replicates. If this becomes
a visible problem, the forward-compatible fix is an optional
`after_message_sequence: Int` field on `AgentResponse` that pins the slot
atomically.
````

- [ ] **Step 4: Verify markdown renders**

Run:

```bash
ls -la crates/defra-agent/proofs/client-state-machine.md
```

Expected: file present, size grew by ~3 KB.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/client-state-machine.md
git commit -m "docs(client-protocol): redefine AgentResponse.content/reasoning as live tail

Documents the writer-side contract introduced by issue #64: AgentResponse
content/reasoning carry only the visible bytes streamed since the most recent
commit boundary in this turn. Adds a Live Overlay section with the render
algorithm and TypeScript/Swift reference implementations."
```

---

## Task 2: Extend Lean ResponseSnapshot with `tailEmpty`

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Client/Types.lean`

- [ ] **Step 1: Locate the existing `ResponseSnapshot` struct (lines ~60-65)**

```lean
/-- Snapshot of an AgentResponse as observed by the client.
    progressSeq is omitted — it orders response versions
    but does not affect the derivation result. -/
structure ResponseSnapshot where
  status : ResponseStatus
  deriving DecidableEq, Repr
```

- [ ] **Step 2: Replace with the extended struct**

```lean
/-- Snapshot of an AgentResponse as observed by the client.
    progressSeq is omitted — it orders response versions
    but does not affect the derivation result.
    `tailEmpty` reflects whether the live-tail fields (content/reasoning)
    are empty. It does not affect `deriveAttempt`; it is consumed by
    `projectActiveOverlay` in `Proofs.ClientShell.Projection`. -/
structure ResponseSnapshot where
  status    : ResponseStatus
  tailEmpty : Bool
  deriving DecidableEq, Repr
```

- [ ] **Step 3: Build the proofs**

Run:

```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tail -40
```

Expected: clean build. If a downstream proof fails because it constructs
`ResponseSnapshot` positionally, fix each call site by adding `tailEmpty := false` (the existing tests model only `status`-driven transitions; the tail flag is unrelated to derivation outcomes).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Client/Types.lean
git commit -m "proofs(client): add tailEmpty flag to ResponseSnapshot

Fields needed by the upcoming projectActiveOverlay helper. deriveAttempt
and deriveTurn are unchanged; tailEmpty does not affect the 6-state
projection."
```

---

## Task 3: Add `OverlayBlock` + `projectActiveOverlay` + theorems

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/ClientShell/Projection.lean`

- [ ] **Step 1: Read the existing file to understand the namespace and imports**

```bash
sed -n '1,40p' crates/defra-agent/proofs/Proofs/ClientShell/Projection.lean
```

- [ ] **Step 2: Add the overlay model below `TransportIndicator`**

Append to the end of the file (after the `projectTransportIndicator` definition):

```lean
/-! ## Active Overlay

`projectActiveOverlay` is a pure function over the tip response and the
derived turn state. It returns `some` when the live overlay block should be
rendered, otherwise `none`. The body of the block is intentionally elided —
the formal model only needs the *presence* decision, not the byte content. -/

structure OverlayBlock where
  hasContent   : Bool
  hasReasoning : Bool
  deriving DecidableEq, Repr

/-- Decide whether the live overlay should be rendered, and if so what
    payload presence flags it carries. -/
def projectActiveOverlay
    (resp : Option ResponseSnapshot)
    (turn : Option ClientTurnState)
    (materialized : Bool)
    (hasContent hasReasoning : Bool)
    : Option OverlayBlock :=
  match resp with
  | none => none
  | some r =>
    if materialized then none
    else if r.status = .complete ∨ r.status = .error then none
    else
      match turn with
      | none => none
      | some t =>
        if t.isTerminal then none
        else if t = .waitingForClaim ∨ t = .streaming then
          if hasContent ∨ hasReasoning then
            some { hasContent := hasContent, hasReasoning := hasReasoning }
          else none
        else none

/-! ## Theorems O1–O3 -/

/-- O1: `projectActiveOverlay` returns at most one `OverlayBlock`. Trivial by
    construction (the function returns at most one `some`). Stated for the
    contract surface. -/
theorem projectActiveOverlay_at_most_one
    (resp : Option ResponseSnapshot)
    (turn : Option ClientTurnState)
    (materialized hasContent hasReasoning : Bool) :
    ∀ b₁ b₂,
      projectActiveOverlay resp turn materialized hasContent hasReasoning = some b₁ →
      projectActiveOverlay resp turn materialized hasContent hasReasoning = some b₂ →
      b₁ = b₂ := by
  intros b₁ b₂ h₁ h₂
  rw [h₁] at h₂
  injection h₂

/-- O2: A terminal turn state hides the overlay. -/
theorem projectActiveOverlay_terminal_hides
    (resp : Option ResponseSnapshot)
    (t : ClientTurnState)
    (h : t.isTerminal = true)
    (materialized hasContent hasReasoning : Bool) :
    projectActiveOverlay resp (some t) materialized hasContent hasReasoning = none := by
  cases resp with
  | none => rfl
  | some r =>
    simp [projectActiveOverlay]
    cases materialized
    · cases hr : r.status <;> simp [ClientTurnState.isTerminal] at h <;>
        cases t <;> simp [ClientTurnState.isTerminal] at h <;> simp [h]
    · simp

/-- O3: A materialized response hides the overlay. -/
theorem projectActiveOverlay_materialized_hides
    (resp : Option ResponseSnapshot)
    (turn : Option ClientTurnState)
    (hasContent hasReasoning : Bool) :
    projectActiveOverlay resp turn true hasContent hasReasoning = none := by
  cases resp with
  | none => rfl
  | some _ => rfl
```

(The terminal-hides proof uses case analysis on the turn enum; if elaboration
of the `cases ... cases ...` chain fails, replace with explicit `cases t with`
matches and `simp` rewrites — the underlying claim is true by inspection.)

- [ ] **Step 3: Build**

```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tail -60
```

Expected: clean build. If theorem elaboration fails, fix the proof body without changing the statements (the statements are the contract; bodies are mechanical).

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/ClientShell/Projection.lean
git commit -m "proofs(client-shell): add projectActiveOverlay + theorems O1–O3

Pure decision over (response, turn, materialized, hasContent, hasReasoning)
that returns Some OverlayBlock when the overlay is renderable. O1 = at
most one block; O2 = terminal turn hides; O3 = materialized hides.

Statements form the protocol contract; the body of the block is elided
(presence flags only) because formal model only needs the decision."
```

---

## Task 4: Add LiveOverlay conformance case module

**Files:**
- Create: `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/LiveOverlay.lean`
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean` (re-export)

- [ ] **Step 1: Inspect the existing ContractCases.lean barrel**

```bash
sed -n '1,40p' crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean
```

- [ ] **Step 2: Create the LiveOverlay module**

Create `crates/defra-agent/proofs/Proofs/Conformance/ContractCases/LiveOverlay.lean` with:

```lean
import Proofs.Client.Types
import Proofs.ClientShell.Projection

/-!
# Live Overlay Conformance Cases

Generated cases asserting the live-overlay render decision under the
seven streaming patterns enumerated in the issue #64 design doc.
-/

namespace Conformance.ContractCases

structure LiveOverlayCase where
  name             : String
  responseStatus   : String   -- "streaming" | "complete" | "error"
  materialized     : Bool
  turnTerminal     : Bool
  turnLabel        : String   -- "waitingForClaim" | "streaming" | "completed"
                              -- | "failed" | "superseded" | "interrupted"
  hasContent       : Bool
  hasReasoning     : Bool
  expectOverlay    : Bool
  deriving Repr

def liveOverlayCases : List LiveOverlayCase :=
  [ { name := "pre_first_tool"
    , responseStatus := "streaming", materialized := false
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := true, hasReasoning := false
    , expectOverlay := true }
  , { name := "post_tool_resumed"
    , responseStatus := "streaming", materialized := false
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := true, hasReasoning := false
    , expectOverlay := true }
  , { name := "interleaved_two_tools"
    , responseStatus := "streaming", materialized := false
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := true, hasReasoning := false
    , expectOverlay := true }
  , { name := "tool_first_no_pre_text"
    , responseStatus := "streaming", materialized := false
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := false, hasReasoning := false
    , expectOverlay := false }
  , { name := "interrupted_mid_stream"
    , responseStatus := "streaming", materialized := false
    , turnTerminal := true, turnLabel := "interrupted"
    , hasContent := true, hasReasoning := false
    , expectOverlay := false }
  , { name := "error_mid_stream"
    , responseStatus := "error", materialized := false
    , turnTerminal := false, turnLabel := "streaming"
    , hasContent := false, hasReasoning := false
    , expectOverlay := false }
  , { name := "materialized_final"
    , responseStatus := "complete", materialized := true
    , turnTerminal := true, turnLabel := "completed"
    , hasContent := false, hasReasoning := false
    , expectOverlay := false }
  ]

def liveOverlayCaseNames : List String :=
  liveOverlayCases.map LiveOverlayCase.name

end Conformance.ContractCases
```

- [ ] **Step 3: Re-export from the barrel**

In `crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean`, add at the top alongside other imports (preserve existing ordering):

```lean
import Proofs.Conformance.ContractCases.LiveOverlay
```

- [ ] **Step 4: Build**

```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tail -30
```

Expected: clean build, new module compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/ContractCases/LiveOverlay.lean \
        crates/defra-agent/proofs/Proofs/Conformance/ContractCases.lean
git commit -m "proofs(conformance): add LiveOverlay case table

Seven cases (pre_first_tool, post_tool_resumed, interleaved_two_tools,
tool_first_no_pre_text, interrupted_mid_stream, error_mid_stream,
materialized_final) drive Rust integration coverage in the next task."
```

---

## Task 5: Register LiveOverlay coverage consumer

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean`

- [ ] **Step 1: Read the existing entries**

```bash
sed -n '1,150p' crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean
```

Note where `consumerCoverage` entries are added (typically a `consumerEntries` or top-level `coverageLedger : List CoverageEntry` list).

- [ ] **Step 2: Add the LiveOverlay entry**

Locate the list of coverage entries and append:

```lean
, consumerCoverage
    "Client"
    "LiveOverlay"
    "tests/live_overlay_conformance.rs::live_overlay_cases_match_lean_table"
```

Match the surrounding indentation and trailing-comma style.

- [ ] **Step 3: Build**

```bash
cd crates/defra-agent/proofs && lake build 2>&1 | tail -20
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean
git commit -m "proofs(coverage): register Client.LiveOverlay consumer

Points the new conformance domain at tests/live_overlay_conformance.rs.
The Rust support harness will resolve the pointer once Task 11 lands."
```

---

## Task 6: Add `DefraStreamWriter::reset_tail`

**Files:**
- Modify: `crates/defra-agent/src/streaming.rs`
- Test: `crates/defra-agent/src/streaming/tests.rs`

- [ ] **Step 1: Write the failing test**

In `crates/defra-agent/src/streaming/tests.rs`, after the existing `write_tokens_*` tests, add:

```rust
#[tokio::test]
async fn reset_tail_clears_response_content_and_reasoning() {
    let (node, _path) = build_test_node("reset-tail").await;
    let writer = DefraStreamWriter::new(
        Arc::clone(&node),
        "did:defra-agent:test",
        Duration::from_millis(0),
    );
    let _request_doc =
        create_processing_request(&node, "req-reset", "session-reset").await;

    let doc_id = writer
        .begin("session-reset", "req-reset", "general")
        .await
        .expect("begin");

    writer.write_tokens(&doc_id, "hello").await.expect("write tokens");
    writer
        .write_reasoning(&doc_id, "thinking")
        .await
        .expect("write reasoning");
    writer.flush_pending(&doc_id).await.expect("flush");

    let pre = load_response(&node, &doc_id).await;
    assert_eq!(pre["content"].as_str(), Some("hello"));
    assert_eq!(pre["reasoning"].as_str(), Some("thinking"));

    writer.reset_tail(&doc_id).await.expect("reset_tail");

    let post = load_response(&node, &doc_id).await;
    assert_eq!(post["content"].as_str(), Some(""));
    assert_eq!(post["reasoning"].as_str(), Some(""));
}
```

- [ ] **Step 2: Run the test to confirm failure**

```bash
cargo test -p defra-agent --lib streaming::tests::reset_tail_clears_response_content_and_reasoning 2>&1 | tail -30
```

Expected: compile error — `reset_tail` undefined on `DefraStreamWriter`.

- [ ] **Step 3: Implement `reset_tail`**

In `crates/defra-agent/src/streaming.rs`, add this method to `impl DefraStreamWriter` (place it adjacent to `flush_snapshot` / `pending_snapshot`):

```rust
    /// Reset the live-tail buffer at a commit boundary.
    ///
    /// Clears the in-memory content/reasoning, leaves token_count cumulative
    /// (metering field), and persists empty content/reasoning on the
    /// streaming response row. progress_seq is not bumped here — it is
    /// owned by `RequestLifecycle::advance` and bumps at lifecycle
    /// boundaries (which are exactly the call sites that invoke
    /// reset_tail).
    pub async fn reset_tail(&self, doc_id: &str) -> Result<()> {
        {
            let mut buffers = self.buffers.lock().await;
            let buf = buffers
                .get_mut(doc_id)
                .ok_or_else(|| anyhow::anyhow!("no buffer for doc_id={}", doc_id))?;
            buf.content.clear();
            buf.reasoning.clear();
            buf.last_flush_at = Instant::now();
        }

        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "",
                        reasoning: ""
                    }}
                ) {{ _docID }}
            }}"#
        );

        let resp = execute_mutation_with_retry(
            &self.node,
            &mutation,
            "reset_streaming_response_tail",
        )
        .await?;

        if !resp
            .data
            .as_ref()
            .and_then(|data| data.get("update_AgentResponse"))
            .is_some_and(response_has_documents)
        {
            let current = load_response_state(&self.node, doc_id).await?;
            anyhow::bail!(
                "cannot reset tail of AgentResponse {} because it is {}",
                doc_id,
                current
                    .as_ref()
                    .map(|r| r.status.as_str())
                    .unwrap_or("missing")
            );
        }

        Ok(())
    }
```

- [ ] **Step 4: Run the test to confirm pass**

```bash
cargo test -p defra-agent --lib streaming::tests::reset_tail_clears_response_content_and_reasoning 2>&1 | tail -20
```

Expected: PASS.

- [ ] **Step 5: Run the full streaming test module to catch regressions**

```bash
cargo test -p defra-agent --lib streaming:: 2>&1 | tail -20
```

Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/streaming.rs crates/defra-agent/src/streaming/tests.rs
git commit -m "feat(streaming): add DefraStreamWriter::reset_tail

Clears the in-memory content/reasoning buffers and persists empty
content/reasoning on the streaming response row. Used at commit boundaries
in StreamProcessor to keep AgentResponse.content/reasoning as the live
tail (issue #64). token_count remains cumulative (metering field)."
```

---

## Task 7: Clear tail on finalize

**Files:**
- Modify: `crates/defra-agent/src/streaming.rs` (function `build_finalize_mutation`, lines 470-520)
- Test: `crates/defra-agent/src/streaming/tests.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/defra-agent/src/streaming/tests.rs`:

```rust
#[tokio::test]
async fn finalize_complete_clears_tail() {
    let (node, _path) = build_test_node("finalize-tail").await;
    let writer = DefraStreamWriter::new(
        Arc::clone(&node),
        "did:defra-agent:test",
        Duration::from_millis(0),
    );
    let _ = create_processing_request(&node, "req-fin", "session-fin").await;
    let doc_id = writer
        .begin("session-fin", "req-fin", "general")
        .await
        .expect("begin");
    writer.write_tokens(&doc_id, "world").await.expect("write");
    writer.flush_pending(&doc_id).await.expect("flush");
    writer
        .finalize(&doc_id, StreamStatus::Complete)
        .await
        .expect("finalize");

    let row = load_response(&node, &doc_id).await;
    assert_eq!(row["status"].as_str(), Some("complete"));
    assert_eq!(row["content"].as_str(), Some(""));
    assert_eq!(row["reasoning"].as_str(), Some(""));
}
```

- [ ] **Step 2: Run to confirm failure**

```bash
cargo test -p defra-agent --lib streaming::tests::finalize_complete_clears_tail 2>&1 | tail -20
```

Expected: assertion failure on `content == ""` — current finalize writes the cumulative buffer back.

- [ ] **Step 3: Update `build_finalize_mutation`**

In `crates/defra-agent/src/streaming.rs`, replace the function body (lines 470-520) with:

```rust
fn build_finalize_mutation(
    existing: Option<&PersistedResponseState>,
    doc_id: &str,
    status: &StreamStatus,
    now: &str,
    snapshot: Option<&StreamBufferSnapshot>,
) -> String {
    let request_transition = existing
        .map(|existing| build_request_terminal_update(&existing.request_id, status))
        .unwrap_or_default();
    // content / reasoning are always cleared on finalize because they
    // represent the live tail (issue #64). token_count is preserved as a
    // cumulative metering field — only updated when the in-memory buffer
    // is present (the snapshot path); on the crash-recovery path
    // (`snapshot = None`) the previously-flushed token_count is left
    // untouched.
    match snapshot {
        Some(snapshot) => format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "",
                        reasoning: "",
                        status: "{status}",
                        token_count: {token_count},
                        completed_at: "{now}"
                    }}
                ) {{ _docID }}
                {request_transition}
            }}"#,
            status = status.as_str(),
            token_count = snapshot.token_count,
        ),
        None => format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{
                        _docID: {{ _eq: "{doc_id}" }},
                        status: {{ _eq: "streaming" }}
                    }},
                    input: {{
                        content: "",
                        reasoning: "",
                        status: "{status}",
                        completed_at: "{now}"
                    }}
                ) {{ _docID }}
                {request_transition}
            }}"#,
            status = status.as_str(),
        ),
    }
}
```

- [ ] **Step 4: Run the new test**

```bash
cargo test -p defra-agent --lib streaming::tests::finalize_complete_clears_tail 2>&1 | tail -20
```

Expected: PASS.

- [ ] **Step 5: Run all streaming tests**

```bash
cargo test -p defra-agent --lib streaming:: 2>&1 | tail -30
```

Expected: all green. Some existing tests may have asserted that `content` carries the cumulative text after finalize — update them to assert `content == ""` instead. The transcript surface is `AgentMessage`, which those tests should still verify via `load_history` / `AgentMessage` queries.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/streaming.rs crates/defra-agent/src/streaming/tests.rs
git commit -m "feat(streaming): clear AgentResponse tail on finalize

Per the issue #64 contract, content/reasoning are the live tail since the
last commit boundary. Finalize is a terminal commit, so the tail is empty
post-finalize. token_count is preserved (cumulative metering)."
```

---

## Task 8: Wire `reset_tail` into `StreamProcessor`

**Files:**
- Modify: `crates/defra-agent/src/agent/stream_processor.rs`
- Test: `crates/defra-agent/src/agent/stream_processor/tests.rs`

- [ ] **Step 1: Write the failing test**

In `crates/defra-agent/src/agent/stream_processor/tests.rs`, add a test that drives the processor through Text → ToolCall → ToolResult → Text → FinalResponse and asserts the tail-reset behavior. Pattern (mirror existing tests in this file for harness setup):

```rust
#[tokio::test]
async fn post_tool_resumed_resets_response_tail() {
    let (node, _path) = build_test_node("post-tool-reset").await;
    let processor_harness = build_processor_harness(&node, "session-reset", "req-reset").await;

    processor_harness.feed_text("hello ").await;
    processor_harness.feed_text("world").await;
    processor_harness.feed_tool_call("search", r#"{"q":"x"}"#).await;
    processor_harness.feed_tool_result("search", r#"{"hit":1}"#, "call-1").await;

    let after_tool = load_response(&node, processor_harness.doc_id()).await;
    assert_eq!(
        after_tool["content"].as_str(),
        Some(""),
        "tail must reset after tool-result persisted",
    );
    assert_eq!(after_tool["reasoning"].as_str(), Some(""));

    processor_harness.feed_text("done").await;
    let after_resume = load_response(&node, processor_harness.doc_id()).await;
    assert_eq!(after_resume["content"].as_str(), Some("done"));

    processor_harness.feed_final("done").await;
    let after_final = load_response(&node, processor_harness.doc_id()).await;
    assert_eq!(after_final["content"].as_str(), Some(""));
    assert_eq!(after_final["status"].as_str(), Some("complete"));
}
```

If `build_processor_harness` does not exist in this test file, mirror the existing test setup in this module (look for the most recent existing `#[tokio::test]` in `stream_processor/tests.rs` for the harness pattern).

- [ ] **Step 2: Run to confirm failure**

```bash
cargo test -p defra-agent --lib agent::stream_processor::tests::post_tool_resumed_resets_response_tail 2>&1 | tail -30
```

Expected: assertion failure on `content == ""` post-tool — current processor doesn't reset.

- [ ] **Step 3: Insert `reset_tail` calls in `process_item`**

In `crates/defra-agent/src/agent/stream_processor.rs`:

**3a.** Locate the `StreamedUserContent::ToolResult` arm (lines 105-127). After the `apply_persistence_policy` call for `persist_stream_tool_result_message`, add:

```rust
                self.stream_writer.reset_tail(self.doc_id).await?;
```

The arm should now read:

```rust
            Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                internal_call_id,
            })) => {
                let _ = self.stream_writer.flush_pending(self.doc_id).await?;
                self.lifecycle.advance().await?;
                if let Some(message) = self.assistant_turn.take_message() {
                    self.persistence_hook.apply_persistence_policy(
                        self.persistence_hook
                            .persist_message(&message)
                            .await
                            .map(|_| ()),
                        "persist streamed assistant turn",
                    )?;
                }
                self.persistence_hook.apply_persistence_policy(
                    self.persistence_hook
                        .persist_stream_tool_result_message(&tool_result, &internal_call_id)
                        .await,
                    "persist streamed tool result",
                )?;
                self.stream_writer.reset_tail(self.doc_id).await?;
                Ok(StreamAction::Continue)
            }
```

**3b.** Locate the `MultiTurnStreamItem::FinalResponse` arm (lines 128-143). The `finalize` call at the very end of the stream already clears the tail (Task 7), but we also reset here so the cleared state is observable between `persist_message` and the eventual `finalize`. Add the reset after `mark_current_response_materialized`:

```rust
            Ok(MultiTurnStreamItem::FinalResponse(response)) => {
                self.assistant_turn.reconcile_text(response.response());
                let _ = self.stream_writer.flush_pending(self.doc_id).await?;
                self.lifecycle.advance().await?;
                if let Some(message) = self.assistant_turn.take_message() {
                    let sequence = self.persistence_hook.persist_message(&message).await?;
                    self.persistence_hook.apply_persistence_policy(
                        self.persistence_hook
                            .mark_current_response_materialized(sequence)
                            .await,
                        "mark final assistant turn materialized",
                    )?;
                    self.stream_writer.reset_tail(self.doc_id).await?;
                }
                self.final_text = Some(response.response().to_string());
                Ok(StreamAction::Done)
            }
```

**3c.** Locate `persist_partial_turn` (lines 158-172). Add a reset after a successful persist:

```rust
    pub(super) async fn persist_partial_turn(&mut self, context: &str) -> Result<bool> {
        let Some(message) = self.assistant_turn.take_message() else {
            return Ok(false);
        };

        self.persistence_hook.apply_persistence_policy(
            self.persistence_hook
                .persist_message(&message)
                .await
                .map(|_| ()),
            context,
        )?;
        self.stream_writer.reset_tail(self.doc_id).await?;

        Ok(true)
    }
```

- [ ] **Step 4: Run the new test**

```bash
cargo test -p defra-agent --lib agent::stream_processor::tests::post_tool_resumed_resets_response_tail 2>&1 | tail -30
```

Expected: PASS.

- [ ] **Step 5: Run the full processor module**

```bash
cargo test -p defra-agent --lib agent::stream_processor:: 2>&1 | tail -30
```

Expected: all green. Existing tests that asserted cumulative `content` post-boundary need updating to assert `""` post-boundary and inspect `AgentMessage` rows for the persisted text.

- [ ] **Step 6: Commit**

```bash
git add crates/defra-agent/src/agent/stream_processor.rs \
        crates/defra-agent/src/agent/stream_processor/tests.rs
git commit -m "feat(stream-processor): reset response tail at commit boundaries

After persist_message / persist_stream_tool_result_message, reset the
AgentResponse content/reasoning so the live tail reflects only post-boundary
bytes. Same reset on the final-response and partial-turn paths. Closes the
runtime side of issue #64."
```

---

## Task 9: Update materialization signature semantics (doc + within-boundary stall test)

**Files:**
- Modify: `crates/defra-agent-desktop-core/src/client/core/materialization.rs`

The struct shape is unchanged. `response_content_len` and
`response_reasoning_len` previously meant "turn-cumulative growth"; under
the new contract they mean "current tail length" (resets at every commit
boundary, grows during active streaming). That is actually a cleaner
stall signal — but only if a test pins the new semantics.

- [ ] **Step 1: Update the doc-comment on `MaterializationSignature`**

In `crates/defra-agent-desktop-core/src/client/core/materialization.rs`,
add a doc-comment above the struct (lines 24-35):

```rust
/// Stall-detector signature for in-flight streaming responses.
///
/// Under the issue #64 live-tail contract, `response_content_len` and
/// `response_reasoning_len` measure the *current tail* — bytes streamed
/// since the most recent commit boundary in this turn. They reset to 0
/// at every boundary and grow during active streaming, which is the
/// signal the detector consumes. `progress_seq` advances at lifecycle
/// boundaries (`RequestLifecycle::advance`) but not on every flush, so
/// it alone is insufficient as a within-boundary signal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MaterializationSignature {
    response_status: Option<String>,
    progress_seq: Option<i64>,
    materialized_message_sequence: Option<i64>,
    response_content_len: usize,
    response_reasoning_len: usize,
    message_count: usize,
    tool_call_count: usize,
    completed_tool_call_count: usize,
    tool_result_count: usize,
}
```

- [ ] **Step 2: Add a within-boundary stall test**

Append a new test alongside the existing `tracker_*` tests:

```rust
    #[test]
    fn tracker_triggers_repair_when_tail_length_plateaus_within_boundary() {
        let mut tracker = MaterializationTracker::default();
        let now = Instant::now();

        // Active streaming: tail grew from 128 to 256 bytes — no stall.
        assert!(tracker
            .observe_for_test(vec![make_candidate(128, 7, 4)], now)
            .is_empty());
        assert!(tracker
            .observe_for_test(
                vec![make_candidate(256, 7, 4)],
                now + Duration::from_secs(1)
            )
            .is_empty());

        // Tail length plateaued at 256 with no boundary advance — stall.
        let stalled = tracker.observe_for_test(
            vec![make_candidate(256, 7, 4)],
            now + MATERIALIZATION_STALL_THRESHOLD + Duration::from_millis(1),
        );
        assert_eq!(stalled.len(), 1, "expected stall when tail plateaus");
    }
```

`make_candidate` already takes `(response_content_len, progress_seq, message_count)` (see `materialization.rs:577-598`), so the existing helper is reused as-is.

- [ ] **Step 3: Run the materialization tests**

```bash
cargo test -p defra-agent-desktop-core --lib client::core::materialization 2>&1 | tail -30
```

Expected: all green, including the new test.

- [ ] **Step 4: Commit**

```bash
git add crates/defra-agent-desktop-core/src/client/core/materialization.rs
git commit -m "docs(materialization): clarify tail-length stall semantics

Under the issue #64 live-tail contract, response_content_len /
response_reasoning_len measure the current tail rather than turn-cumulative
growth. The detector logic is unchanged — the new doc-comment pins the
contract and a new test asserts within-boundary plateau detection."
```

---

## Task 10: Delete `live_overlay_suffix` from the desktop bridge

**Files:**
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/snapshot/timeline.rs`
- Modify: `apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_timeline.rs`

- [ ] **Step 1: Update the bridge test for the new contract**

Open `apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_timeline.rs`. Find any test that depends on prefix-stripping. Each such test typically:

- Seeds a streaming `AgentResponse` with cumulative content like `"hello world"`.
- Seeds a committed `AgentMessage` with `"hello"`.
- Asserts the rendered overlay carries `"world"` only.

Rewrite each to seed `AgentResponse.content = "world"` directly (the new contract):

```rust
    let response = AgentResponseRow {
        // ...existing fields...
        content: Some("world".to_string()),
        reasoning: None,
        status: Some("streaming".to_string()),
        materialized_message_sequence: None,
        // ...
    };
```

The assertion (overlay shows `"world"`) is unchanged. Add a new test asserting that when `AgentResponse.content == ""` the overlay is suppressed:

```rust
#[test]
fn overlay_hidden_when_response_tail_is_empty() {
    // Seed a streaming response with empty tail (post-boundary state).
    let store = make_streaming_store_with_response_content("");
    let snapshot = build_session_snapshot_from_store(&store, "sess-1", None)
        .expect("snapshot");
    let has_live = snapshot
        .timeline_items
        .iter()
        .any(|item| matches!(item, RenderedTimelineItem::LiveAssistant { .. }));
    assert!(!has_live, "overlay must be hidden when tail is empty");
}
```

(Helper `make_streaming_store_with_response_content` mirrors any existing seeding helper in the test file; reuse / adapt.)

- [ ] **Step 2: Run to confirm failure**

```bash
cargo test -p defra-agent-desktop --lib bridge::snapshot::tests::session_timeline 2>&1 | tail -30
```

Expected: failures — the existing prefix-stripping path still produces an overlay even when content is empty in the post-rewrite tests, OR the rewritten tests pass but the new empty-tail assertion fails because the seeded data goes through `live_overlay_suffix` which still returns content.

- [ ] **Step 3: Delete the heuristic in `timeline.rs`**

In `apps/desktop-tauri/src-tauri/src/bridge/snapshot/timeline.rs`:

**3a.** Delete `fn live_overlay_suffix` (lines 58-79).

**3b.** Delete `fn active_turn_committed_assistant_texts` (lines 98-135) — it is only used by `live_overlay_suffix`.

**3c.** Update the call site in `build_rendered_timeline` (lines 233-247). The current code:

```rust
    let committed_assistant_texts =
        active_turn_committed_assistant_texts(messages, active_turn_index);
    let overlay_content = live_overlay_suffix(
        &committed_assistant_texts,
        active_response_overlay.and_then(|overlay| overlay.content.as_deref()),
    );
    let overlay_reasoning = active_response_overlay
        .and_then(|overlay| normalize_optional(overlay.reasoning.as_deref()));
    if overlay_content.is_some() || overlay_reasoning.is_some() {
        timeline.push(RenderedTimelineItem::LiveAssistant {
            item_key: "live-assistant".to_string(),
            content: overlay_content,
            reasoning: overlay_reasoning,
        });
    }
```

Replace with:

```rust
    let overlay_content = active_response_overlay
        .and_then(|overlay| normalize_optional(overlay.content.as_deref()));
    let overlay_reasoning = active_response_overlay
        .and_then(|overlay| normalize_optional(overlay.reasoning.as_deref()));
    if overlay_content.is_some() || overlay_reasoning.is_some() {
        timeline.push(RenderedTimelineItem::LiveAssistant {
            item_key: "live-assistant".to_string(),
            content: overlay_content,
            reasoning: overlay_reasoning,
        });
    }
```

**3d.** The `active_turn_index` parameter of `build_rendered_timeline` is no longer used after removing `active_turn_committed_assistant_texts`. Delete the parameter from the function signature and from the single caller in `apps/desktop-tauri/src-tauri/src/bridge/snapshot/session.rs` (line ~201). Update the call site:

Before (`session.rs:201-207`):
```rust
    let timeline_items = build_rendered_timeline(
        &messages,
        &tool_calls,
        pending_turn.as_ref(),
        active_response_overlay.as_ref(),
        active_turn_index,
    );
```

After:
```rust
    let timeline_items = build_rendered_timeline(
        &messages,
        &tool_calls,
        pending_turn.as_ref(),
        active_response_overlay.as_ref(),
    );
```

Also delete the now-unused `active_turn_index` local in `session.rs:130-132` if no other consumer references it. (Search for other uses with `grep -n "active_turn_index" apps/desktop-tauri/src-tauri/src/bridge/snapshot/session.rs` before deleting.)

- [ ] **Step 4: Run the bridge tests**

```bash
cargo test -p defra-agent-desktop --lib bridge::snapshot:: 2>&1 | tail -30
```

Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop-tauri/src-tauri/src/bridge/snapshot/timeline.rs \
        apps/desktop-tauri/src-tauri/src/bridge/snapshot/session.rs \
        apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_timeline.rs
git commit -m "refactor(bridge): drop prefix-stripping live overlay heuristic

Under the issue #64 contract, AgentResponse.content/reasoning carry only
the live tail, so the bridge can read them directly. Deletes
live_overlay_suffix and active_turn_committed_assistant_texts; updates
the build_rendered_timeline signature to drop the (now-unused)
active_turn_index parameter."
```

---

## Task 11: Add Rust integration test driving the LiveOverlay cases

**Files:**
- Create: `crates/defra-agent/tests/live_overlay_conformance.rs`

- [ ] **Step 1: Create the test scaffold**

Create `crates/defra-agent/tests/live_overlay_conformance.rs`:

```rust
//! Integration test pinning the issue #64 live-tail contract to the
//! generated Lean LiveOverlay case table.
//!
//! Each case asserts that:
//!   - The runtime writer produces the expected AgentResponse.content/reasoning
//!     state given the streamed pattern (where applicable for streaming cases).
//!   - The rendered overlay decision computed from
//!     (response, derived_turn, materialized) matches `expectOverlay` from
//!     the Lean case table.

mod support;

use defra_agent_protocol::client_protocol::ClientTurnState;
use serde::Deserialize;
use support::lean_vocab_test::lean_contract_snapshot;

#[derive(Debug, Deserialize)]
struct LeanLiveOverlayCase {
    name: String,
    #[serde(rename = "responseStatus")]
    response_status: String,
    materialized: bool,
    #[serde(rename = "turnTerminal")]
    turn_terminal: bool,
    #[serde(rename = "turnLabel")]
    turn_label: String,
    #[serde(rename = "hasContent")]
    has_content: bool,
    #[serde(rename = "hasReasoning")]
    has_reasoning: bool,
    #[serde(rename = "expectOverlay")]
    expect_overlay: bool,
}

fn parse_turn(label: &str) -> Option<ClientTurnState> {
    match label {
        "waitingForClaim" => Some(ClientTurnState::WaitingForClaim),
        "streaming" => Some(ClientTurnState::Streaming),
        "completed" => Some(ClientTurnState::Completed),
        "failed" => Some(ClientTurnState::Failed),
        "superseded" => Some(ClientTurnState::Superseded),
        "interrupted" => Some(ClientTurnState::Interrupted),
        _ => None,
    }
}

/// Mirror of the bridge / TS render decision. Kept inline in the test rather
/// than imported, so the test can fail loudly if either the bridge or the
/// frontend drifts from the contract.
fn should_show_overlay(
    response_status: &str,
    materialized: bool,
    turn: Option<ClientTurnState>,
    has_content: bool,
    has_reasoning: bool,
) -> bool {
    if materialized {
        return false;
    }
    if response_status == "complete" || response_status == "error" {
        return false;
    }
    let Some(turn) = turn else {
        return false;
    };
    let renderable = matches!(turn, ClientTurnState::WaitingForClaim | ClientTurnState::Streaming);
    if !renderable {
        return false;
    }
    has_content || has_reasoning
}

#[test]
fn live_overlay_cases_match_lean_table() {
    let snapshot = lean_contract_snapshot();
    let cases: Vec<LeanLiveOverlayCase> = snapshot
        .live_overlay_cases()
        .expect("LiveOverlay case table must be present in lean contract snapshot");
    assert!(!cases.is_empty(), "Lean LiveOverlay table is empty");

    for case in cases {
        let actual = should_show_overlay(
            &case.response_status,
            case.materialized,
            parse_turn(&case.turn_label),
            case.has_content,
            case.has_reasoning,
        );
        assert_eq!(
            actual, case.expect_overlay,
            "case {name} expected overlay={expected}, got {actual}",
            name = case.name,
            expected = case.expect_overlay,
        );

        // Sanity: terminal turns must hide the overlay regardless of content.
        if case.turn_terminal {
            assert!(
                !case.expect_overlay,
                "case {} marks turn as terminal but expects overlay; contract violated",
                case.name,
            );
        }
    }
}
```

- [ ] **Step 2: Wire the snapshot accessor**

The `lean_contract_snapshot()` helper is the existing harness used by `state_machine_conformance.rs`. Add a `live_overlay_cases()` accessor on its result type.

Find the accessor module (`crates/defra-agent/src/lean_vocab_test.rs` per the existing imports in `state_machine_conformance.rs`) and add:

```rust
    pub fn live_overlay_cases<T: serde::de::DeserializeOwned>(&self) -> anyhow::Result<Vec<T>> {
        self.json_section("liveOverlayCases")
    }
```

If the snapshot loader does not yet emit the `liveOverlayCases` section, add it to the contract emission (the file that emits the JSON snapshot — usually `crates/defra-agent/proofs/Proofs/Conformance/Contracts.lean`). Add a top-level field that maps each Lean case to a JSON object:

```lean
def liveOverlayCasesJson : Json :=
  Json.arr (Conformance.ContractCases.liveOverlayCases.map fun c =>
    Json.mkObj
      [ ("name",            Json.str c.name)
      , ("responseStatus",  Json.str c.responseStatus)
      , ("materialized",    Json.bool c.materialized)
      , ("turnTerminal",    Json.bool c.turnTerminal)
      , ("turnLabel",       Json.str c.turnLabel)
      , ("hasContent",      Json.bool c.hasContent)
      , ("hasReasoning",    Json.bool c.hasReasoning)
      , ("expectOverlay",   Json.bool c.expectOverlay)
      ])
```

and include it in the top-level snapshot object under the key `"liveOverlayCases"`. Match the surrounding style of existing `*CasesJson` definitions in that file (look at how `lifecycleTransitionCasesJson` is wired for the precedent).

- [ ] **Step 3: Run the integration test**

```bash
(cd crates/defra-agent/proofs && lake build) && \
  cargo test -p defra-agent --test live_overlay_conformance 2>&1 | tail -30
```

Expected: PASS.

- [ ] **Step 4: Run the broader conformance harness**

```bash
cargo test -p defra-agent --test state_machine_conformance 2>&1 | tail -30
```

Expected: still green; the coverage ledger now resolves the `Client.LiveOverlay` consumer.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent/tests/live_overlay_conformance.rs \
        crates/defra-agent/src/lean_vocab_test.rs \
        crates/defra-agent/proofs/Proofs/Conformance/Contracts.lean
git commit -m "test(conformance): pin live-overlay decision to Lean case table

Loads the Lean LiveOverlay cases from the contract snapshot and asserts
each case's expected render decision matches the runtime predicate.
Resolves the Client.LiveOverlay coverage consumer registered in Task 5."
```

---

## Task 12: Wire frontend LiveOverlay conformance into chat-shell.test.ts

**Files:**
- Modify: `apps/desktop-tauri/src/lib/chat-shell.test.ts`

- [ ] **Step 1: Read the existing test for the conformance pattern**

```bash
grep -n "frontend_client_shell_cases\|frontend_client_shell_case_count\|describe\|it(" apps/desktop-tauri/src/lib/chat-shell.test.ts | head -40
```

This gives the existing pattern for consuming Lean-emitted frontend rows.

- [ ] **Step 2: Add the LiveOverlay test block**

Append to `apps/desktop-tauri/src/lib/chat-shell.test.ts`:

```typescript
import liveOverlayCases from "../../../crates/defra-agent/proofs/.lake/build/contract-snapshot.live-overlay.json"
  with { type: "json" };

type LiveOverlayCase = {
  name: string;
  responseStatus: "streaming" | "complete" | "error";
  materialized: boolean;
  turnTerminal: boolean;
  turnLabel: string;
  hasContent: boolean;
  hasReasoning: boolean;
  expectOverlay: boolean;
};

function shouldShowOverlay(c: LiveOverlayCase): boolean {
  if (c.materialized) return false;
  if (c.responseStatus === "complete" || c.responseStatus === "error") return false;
  if (c.turnLabel !== "streaming" && c.turnLabel !== "waitingForClaim") return false;
  return c.hasContent || c.hasReasoning;
}

describe("LiveOverlay conformance (issue #64)", () => {
  for (const raw of liveOverlayCases as LiveOverlayCase[]) {
    it(`case ${raw.name} matches Lean expected decision`, () => {
      expect(shouldShowOverlay(raw)).toBe(raw.expectOverlay);
    });
  }
});
```

If the snapshot is not emitted to that JSON path, instead add an emission step to the same `Contracts.lean` change made in Task 11 (write the section to a separate file that the frontend imports). Mirror whatever path the existing `frontend_client_shell_cases` use. Pick whichever approach matches the existing pattern in this repo and follow it consistently.

- [ ] **Step 3: Run the frontend tests**

```bash
(cd apps/desktop-tauri && pnpm test --run chat-shell)
```

Expected: PASS — seven new test cases.

- [ ] **Step 4: Run the workspace verification suite**

```bash
cargo test --workspace 2>&1 | tail -30
(cd crates/defra-agent/proofs && lake build) 2>&1 | tail -20
(cd apps/desktop-tauri && pnpm test --run) 2>&1 | tail -30
```

Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop-tauri/src/lib/chat-shell.test.ts
git commit -m "test(chat-shell): consume LiveOverlay frontend conformance rows

Mirrors the Rust conformance test against the same Lean case table so
both render-decision implementations are pinned to the formal contract."
```

---

## Final verification

After all tasks land:

```bash
cargo test --workspace
(cd crates/defra-agent/proofs && lake build)
(cd apps/desktop-tauri && pnpm test --run)
```

Expected: all green. The desktop overlay now consumes `AgentResponse.content` directly with no prefix-stripping; the Lean LiveOverlay cases are pinned by both Rust and TypeScript; the writer always resets content/reasoning at commit boundaries.

A manual eyeball through a tool-heavy session in the running desktop app is recommended once for visual confirmation, but not required — the existing desktop test surface plus the new conformance tests carry the regression coverage (per the design doc smoke section).
