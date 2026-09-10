import Proofs.Compaction.Summarize
import Proofs.Compaction.Prefix

namespace Compaction

open Transcript (MessageRow StrictlyIncreasingMessages)

/-- The fixtures the Rust conformance driver builds, keyed by row count.

`tests/conformance/streaming_compaction.rs::compaction_messages_for_case`
constructs the same shapes out of `gents::llm::message::Message`, so the
`safeBoundary` values below are computed from the model and checked against
production's `compaction::pair_safe_boundary` on the corresponding Rust
fixture. -/
def caseFixture : Nat → List Transcript.MessageRow
  | 1 => [⟨0, 0, 0, .user, .toolResult 1 ⟨0, 0, 37⟩⟩]
  | 2 => [⟨0, 0, 0, .assistant, .assistantToolCalls {1}⟩,
          ⟨1, 0, 1, .user, .toolResult 1 ⟨0, 0, 37⟩⟩]
  | 3 => [⟨0, 0, 0, .user, .ordinary⟩,
          ⟨1, 0, 1, .assistant, .assistantToolCalls {1}⟩,
          ⟨2, 0, 2, .user, .toolResult 1 ⟨0, 0, 37⟩⟩]
  | _ => []

/-- The straddling split: a budget index of 2 lands between the assistant
announcement and its result, and the boundary retreats to 1 so the turn stays
whole in the retained tail. -/
theorem caseFixture_boundaries_pinned :
    pairSafeBoundary (caseFixture 3) 2 = 1 ∧
      pairSafeBoundary (caseFixture 3) 1 = 1 ∧
      pairSafeBoundary (caseFixture 3) 3 = 3 ∧
      pairSafeBoundary (caseFixture 2) 1 = 0 := by
  refine ⟨?_, ?_, ?_, ?_⟩ <;> decide

structure CompactionReducerCase where
  name                : String
  group               : String
  reducer             : String
  legal               : Bool
  preMessageCount     : Nat
  postMessageCount    : Nat
  preservesPairs      : Bool
  preservesOrder      : Bool
  /-- Only reducers with a modeled gate report it; raw strip/sanitize have none. -/
  gateOpen            : Option Bool
  /-- Input to the uniform response resolver, independent of the expected gate. -/
  responseStatus      : StreamingResponse.Status
  safeToReduce        : Bool
  reducerIsIdentity   : Bool
  reducerIsIdempotent : Bool
  /-- The raw token-budget index production computes before any adjustment. -/
  splitIndex          : Nat
  /-- Where the boundary lands after retreating to a turn boundary. Computed by
  `pairSafeBoundary`, not asserted. -/
  safeBoundary        : Nat
  /-- Rows surviving the reduction. -/
  retainedCount       : Nat
  deriving Repr

private def pairsClosed (msgs : List MessageRow) : Bool :=
  msgs.all fun row => match row.kind with
    | .toolResult callId _ => msgs.any fun caller =>
        caller.role == .assistant && match caller.kind with
          | .assistantToolCalls ids => decide (callId ∈ ids)
          | _ => false
    | _ => true
private theorem pairsClosed_iff (msgs : List MessageRow) :
    pairsClosed msgs = true ↔ PromptView.PairsClosedInMessages msgs := by
  simp only [pairsClosed, List.all_eq_true, PromptView.PairsClosedInMessages]
  constructor
  · intro h row hr callId key hk
    have hh := h row hr
    simp only [hk, List.any_eq_true, Bool.and_eq_true, beq_iff_eq] at hh
    obtain ⟨caller, hc, role, hk⟩ := hh
    refine ⟨caller, hc, role, ?_⟩
    cases he : caller.kind <;> simp_all
  · intro h row hr
    cases hk : row.kind with
    | ordinary => rfl
    | assistantToolCalls ids => rfl
    | toolResult callId key =>
        obtain ⟨caller, hc, role, ids, hi, hm⟩ := h row hr callId key hk
        exact List.any_eq_true.mpr ⟨caller, hc, by simp [role, hi, hm]⟩
private instance (msgs : List MessageRow) : Decidable (StrictlyIncreasingMessages msgs) := by
  induction msgs with
  | nil => exact isTrue trivial
  | cons row rest ih =>
      unfold StrictlyIncreasingMessages
      exact instDecidableAnd

private inductive Reducer where
  | identity | strip | summarize | providerView

private def Reducer.name : Reducer → String
  | .identity => "identity"
  | .strip => "strip"
  | .summarize => "summarize"
  | .providerView => "provider_view"

/-- Fixtures invoke the real reducers, including the same finite response gate
used by summarize. Nonzero result payloads make a missing strip observable. -/
private def reducerCase (name group : String) (reducer : Reducer)
    (count splitIndex : Nat) (status : StreamingResponse.Status := .completed) :
    CompactionReducerCase :=
  let source := caseFixture count
  let view : PromptView := ⟨0, source, none, fun _ => some status⟩
  let reduce := fun (input : PromptView) => match reducer with
    | .identity => identityReducer input
    | .strip => { input with messages := strip input.messages }
    | .summarize => summarize (fun _ => splitIndex) ⟨1⟩ input
    | .providerView => { input with messages := providerViewTurn input.messages }
  let reduced := reduce view
  let reapplied := reduce reduced
  { name, group, reducer := reducer.name, legal := true
  , preMessageCount := source.length
  , postMessageCount := reduced.messages.length
  , preservesPairs := !pairsClosed source || pairsClosed reduced.messages
  , preservesOrder := decide
      (Transcript.StrictlyIncreasingMessages source →
       Transcript.StrictlyIncreasingMessages reduced.messages)
  , gateOpen := match reducer with
      | .identity => some (@decide _ (IsValidReducer.decGate (r := identityReducer) view))
      | .summarize => some (@decide _ (IsValidReducer.decGate
          (r := summarize (fun _ => splitIndex) ⟨1⟩) view))
      | .strip | .providerView => none
  , responseStatus := status
  , safeToReduce := decide (PromptView.safeToReduce view)
  , reducerIsIdentity := decide (reduced.messages = source ∧ reduced.summary = view.summary)
  , reducerIsIdempotent := decide
      (reapplied.messages = reduced.messages ∧ reapplied.summary = reduced.summary)
  , splitIndex
  , safeBoundary := pairSafeBoundary source splitIndex
  , retainedCount := reduced.messages.length }

def compactionReducerCases : List CompactionReducerCase :=
  [ reducerCase "identity_reducer_is_no_op" "witness" .identity 0 0
  , reducerCase "identity_preserves_pair_atomicity" "witness" .identity 2 0
  , reducerCase "identity_preserves_message_order" "witness" .identity 3 0
  , reducerCase "strip_preserves_pair_atomicity" "witness" .strip 2 0
  , reducerCase "strip_preserves_message_order" "witness" .strip 3 0
  , reducerCase "strip_is_strictly_idempotent" "witness" .strip 2 0
  , reducerCase "reduction_blocked_when_response_streaming" "streaming" .summarize 2 2 .streaming
  , reducerCase "reduction_allowed_when_response_terminal" "streaming" .summarize 2 2
  , reducerCase "no_orphaned_tool_results_after_strip" "contract" .strip 2 0
  , reducerCase "reapply_preserves_view_coherent" "contract" .strip 2 0
  , reducerCase "summarize_retains_straddling_turn" "summarize" .summarize 3 2
  , reducerCase "summarize_drops_whole_turns" "summarize" .summarize 3 1
  , reducerCase "summarize_oversized_complete_turn" "summarize" .summarize 3 3
  , reducerCase "summarize_blocked_when_response_streaming" "summarize" .summarize 3 2 .streaming
  , reducerCase "summarize_cannot_split_a_leading_turn" "summarize" .summarize 2 1
  , reducerCase "provider_view_is_idempotent" "provider_view" .providerView 3 0
  , reducerCase "provider_view_drops_orphaned_result" "provider_view" .providerView 1 0 ]

theorem compactionReducerCases_count : compactionReducerCases.length = 17 := by decide

theorem strip_fixture_changes_payload :
    (reducerCase "strip" "witness" .strip 2 0).reducerIsIdentity = false := by decide

theorem strip_and_provider_fixtures_idempotent :
    (reducerCase "strip" "witness" .strip 2 0).reducerIsIdempotent = true ∧
    (reducerCase "provider" "provider_view" .providerView 3 0).reducerIsIdempotent = true := by
  decide

/-! ## Conditional global-view active-history cursor cases

These fixtures exercise the row-level global proof model for the database
optimization. Transferring them to production requires the unique-id alignment
premise and a separate content-bearing per-turn model. The cursor is a raw-row
split, while `compacted` is measured in the sanitized provider view; the model
therefore searches only splits satisfying `CursorDenotes` rather than treating
the provider count as a raw offset. -/

def cursorFixture : List Transcript.MessageRow :=
  [ ⟨0, 0, 0, .user, .toolResult 99 ⟨0, 0, 0⟩⟩
  , ⟨1, 0, 1, .user, .ordinary⟩
  , ⟨2, 0, 2, .assistant, .assistantToolCalls {1}⟩
  , ⟨3, 0, 3, .user, .toolResult 1 ⟨0, 0, 37⟩⟩
  , ⟨4, 0, 4, .assistant, .ordinary⟩ ]

def cursorCandidates (msgs : List Transcript.MessageRow) : List Nat :=
  (List.range msgs.length).map fun offset => msgs.length - offset

def findCursor (msgs : List Transcript.MessageRow) (compacted : Nat) : Option Nat :=
  (cursorCandidates msgs).find? fun cursor => decide (
    providerViewGlobal msgs =
        providerViewGlobal (msgs.take cursor) ++ providerViewGlobal (msgs.drop cursor) ∧
      (providerViewGlobal (msgs.take cursor)).length = compacted)

structure CompactionCursorCase where
  name           : String
  compacted      : Nat
  expectedCursor : Option Nat
  deriving Repr

def compactionCursorCases : List CompactionCursorCase :=
  [ { name := "cursor_skips_orphan_and_compacted_turn"
    , compacted := 1
    , expectedCursor := findCursor cursorFixture 1 }
  , { name := "cursor_rejects_split_inside_tool_pair"
    , compacted := 2
    , expectedCursor := findCursor cursorFixture 2 }
  , { name := "cursor_accepts_complete_tool_pair"
    , compacted := 3
    , expectedCursor := findCursor cursorFixture 3 } ]

theorem compactionCursorCases_pinned :
    compactionCursorCases.map (fun c => c.expectedCursor) = [some 2, none, some 4] := by
  native_decide

end Compaction
