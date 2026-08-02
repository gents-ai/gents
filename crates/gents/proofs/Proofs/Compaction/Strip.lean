import Proofs.Compaction.State
import Proofs.PromptAssembly.State

/-!
# The real tool-result strip

The previous model of this reducer (`stubMessageKind` in
`Proofs/Compaction/Transition.lean`) was literally
`| .toolResult callId key => .toolResult callId key`, so every preservation
property in `Proofs/Compaction/Properties.lean` quantified over `id`. It proved
that doing nothing preserves meaning — true, and useless (#993).

Production's `strip_tool_results` replaces each tool result's payload with a
pointer stub and touches nothing else: same rows, same order, same call ids.
That last part is the whole reason `strip` commutes with `sanitize`
(`Proofs/Compaction/ProviderView.lean`), which is what licenses the production
reordering of the compacted-prefix drop.
-/

namespace Compaction

open Transcript (MessageRow MessageKind ToolResultKey)

/-- The canonical pointer payload a stripped tool result carries.

Production writes `[tool: NAME(ARG), call_id: ID, N bytes — see DefraDB
AgentToolCall for full output]`; the model abstracts that to a single canonical
payload hash. Collapsing distinct payloads is deliberate — `ViewCoherent` does
not require `UniqueToolResultKeys`. -/
def stubKey (key : ToolResultKey) : ToolResultKey := { key with payloadHash := 0 }

@[simp] theorem stubKey_idempotent (key : ToolResultKey) :
    stubKey (stubKey key) = stubKey key := rfl

/-- Strip rewrites the *payload* of a tool result and nothing else. It never
changes a constructor and never changes a call id. -/
def stripKind : MessageKind → MessageKind
  | .toolResult callId key => .toolResult callId (stubKey key)
  | k => k

def stripRow (row : MessageRow) : MessageRow := { row with kind := stripKind row.kind }

def strip (msgs : List MessageRow) : List MessageRow := msgs.map stripRow

@[simp] theorem stripRow_kind (row : MessageRow) :
    (stripRow row).kind = stripKind row.kind := rfl

@[simp] theorem stripRow_sequence (row : MessageRow) :
    (stripRow row).sequence = row.sequence := rfl

@[simp] theorem stripRow_role (row : MessageRow) :
    (stripRow row).role = row.role := rfl

@[simp] theorem stripRow_messageId (row : MessageRow) :
    (stripRow row).messageId = row.messageId := rfl

@[simp] theorem stripRow_sessionId (row : MessageRow) :
    (stripRow row).sessionId = row.sessionId := rfl

@[simp] theorem strip_nil : strip [] = [] := rfl

@[simp] theorem strip_cons (row : MessageRow) (rest : List MessageRow) :
    strip (row :: rest) = stripRow row :: strip rest := rfl

@[simp] theorem strip_length (msgs : List MessageRow) :
    (strip msgs).length = msgs.length := List.length_map _ _

theorem strip_append (a b : List MessageRow) : strip (a ++ b) = strip a ++ strip b :=
  List.map_append _ _ _

theorem strip_take (msgs : List MessageRow) (n : Nat) :
    strip (msgs.take n) = (strip msgs).take n := List.map_take _ _ _

theorem strip_drop (msgs : List MessageRow) (n : Nat) :
    strip (msgs.drop n) = (strip msgs).drop n := List.map_drop _ _ _

theorem stripKind_idempotent (k : MessageKind) : stripKind (stripKind k) = stripKind k := by
  cases k <;> rfl

theorem stripRow_idempotent (row : MessageRow) : stripRow (stripRow row) = stripRow row := by
  simp only [stripRow, stripRow_kind, stripKind_idempotent]

/-- The honest replacement for the old `strip_tool_results_is_strictly_idempotent`,
which was true only because the modelled strip was `id`. Production earns this
by recognizing an existing stub instead of re-stubbing it. -/
theorem strip_idempotent (msgs : List MessageRow) : strip (strip msgs) = strip msgs := by
  induction msgs with
  | nil => rfl
  | cons row rest ih =>
      rw [strip_cons, strip_cons, stripRow_idempotent, ih]

theorem strip_kind_ordinary (row : MessageRow) (h : row.kind = .ordinary) :
    (stripRow row).kind = .ordinary := by
  rw [stripRow_kind, h]; rfl

theorem strip_kind_assistant (row : MessageRow) (callIds : Finset ToolExecution.ToolCallId)
    (h : row.kind = .assistantToolCalls callIds) :
    (stripRow row).kind = .assistantToolCalls callIds := by
  rw [stripRow_kind, h]; rfl

theorem strip_kind_result (row : MessageRow) (callId : ToolExecution.ToolCallId)
    (key : ToolResultKey) (h : row.kind = .toolResult callId key) :
    (stripRow row).kind = .toolResult callId (stubKey key) := by
  rw [stripRow_kind, h]; rfl

theorem callsIn_strip (l : List MessageRow) :
    PromptAssembly.callsIn (strip l) = PromptAssembly.callsIn l := by
  induction l with
  | nil => rfl
  | cons row rest ih =>
      rw [strip_cons]
      cases hk : row.kind with
      | ordinary =>
          rw [PromptAssembly.callsIn_cons_ordinary _ _ (strip_kind_ordinary row hk),
            PromptAssembly.callsIn_cons_ordinary row rest hk, ih]
      | assistantToolCalls callIds =>
          rw [PromptAssembly.callsIn_cons_assistant _ _ callIds
              (strip_kind_assistant row callIds hk),
            PromptAssembly.callsIn_cons_assistant row rest callIds hk, ih]
      | toolResult callId key =>
          rw [PromptAssembly.callsIn_cons_result _ _ callId (stubKey key)
              (strip_kind_result row callId key hk),
            PromptAssembly.callsIn_cons_result row rest callId key hk, ih]

theorem resolvedIn_strip (l : List MessageRow) :
    PromptAssembly.resolvedIn (strip l) = PromptAssembly.resolvedIn l := by
  induction l with
  | nil => rfl
  | cons row rest ih =>
      rw [strip_cons]
      cases hk : row.kind with
      | ordinary =>
          rw [PromptAssembly.resolvedIn_cons_ordinary _ _ (strip_kind_ordinary row hk),
            PromptAssembly.resolvedIn_cons_ordinary row rest hk, ih]
      | assistantToolCalls callIds =>
          rw [PromptAssembly.resolvedIn_cons_assistant _ _ callIds
              (strip_kind_assistant row callIds hk),
            PromptAssembly.resolvedIn_cons_assistant row rest callIds hk, ih]
      | toolResult callId key =>
          rw [PromptAssembly.resolvedIn_cons_result _ _ callId (stubKey key)
              (strip_kind_result row callId key hk),
            PromptAssembly.resolvedIn_cons_result row rest callId key hk, ih]

theorem strip_preserves_uniqueCallIds :
    ∀ {l : List MessageRow},
      PromptAssembly.UniqueCallIds l → PromptAssembly.UniqueCallIds (strip l) := by
  intro l
  induction l with
  | nil => intro _; trivial
  | cons row rest ih =>
      intro h
      rw [strip_cons]
      cases hk : row.kind with
      | ordinary =>
          rw [PromptAssembly.uniqueCallIds_cons_ordinary _ _ (strip_kind_ordinary row hk)]
          exact ih ((PromptAssembly.uniqueCallIds_cons_ordinary row rest hk).mp h)
      | assistantToolCalls callIds =>
          have hsplit := (PromptAssembly.uniqueCallIds_cons_assistant row rest callIds hk).mp h
          rw [PromptAssembly.uniqueCallIds_cons_assistant _ _ callIds
              (strip_kind_assistant row callIds hk), callsIn_strip]
          exact ⟨hsplit.1, ih hsplit.2⟩
      | toolResult callId key =>
          rw [PromptAssembly.uniqueCallIds_cons_result _ _ callId (stubKey key)
              (strip_kind_result row callId key hk)]
          exact ih ((PromptAssembly.uniqueCallIds_cons_result row rest callId key hk).mp h)

theorem strip_preserves_strictlyIncreasing :
    ∀ {l : List MessageRow},
      Transcript.StrictlyIncreasingMessages l →
        Transcript.StrictlyIncreasingMessages (strip l) := by
  intro l
  induction l with
  | nil => intro _; trivial
  | cons row rest ih =>
      intro h
      refine ⟨?_, ih h.2⟩
      intro other hmem
      obtain ⟨source, hsource, hEq⟩ := List.mem_map.mp hmem
      subst hEq
      simpa using h.1 source hsource

end Compaction
