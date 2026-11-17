import Proofs.Session.Compaction.State
import Proofs.Session.PromptAssembly.State

namespace Compaction

open Transcript (MessageRow MessageKind ToolResultKey)

/-- Compare the bytes of the actual candidate with the original content.
Equal-size replacements are retained, not stubbed. -/
def stubEligible (originalBytes candidateBytes : Nat) : Bool :=
  candidateBytes < originalBytes

@[simp] theorem stubEligible_equal (bytes : Nat) : stubEligible bytes bytes = false := by
  simp [stubEligible]

theorem stubEligible_no_expansion (originalBytes candidateBytes : Nat)
    (h : stubEligible originalBytes candidateBytes = true) : candidateBytes < originalBytes := by
  simpa [stubEligible] using h

theorem stubEligible_iff (originalBytes candidateBytes : Nat) :
    stubEligible originalBytes candidateBytes = true ↔ candidateBytes < originalBytes := by
  simp [stubEligible]

/-- What the reducer measures for one tool result: the UTF-8 bytes of the
payload it would replace (non-text blocks by their JSON encoding) and of the
stub that would replace it. A separate input, not derived from the
`ToolResultKey` hash.

External premise: both sizes are a function of the result itself, its payload
and its own call, so they are the same on every pass and whatever other rows
the sanitizer drops. The model states it by taking sizes as a function of the
key. Rust enforces it in
`gents-inference` `compaction::tests::stub_sizes_depend_only_on_the_result`. -/
structure StubSizes where
  originalBytes : Nat
  stubBytes : Nat
  deriving DecidableEq, Repr

/-- The sizes measured for each tool result, by its identity. -/
abbrev ResultSizes := ToolResultKey → StubSizes

/-- A sizes table given as rows; a result missing from the table measures
`0` bytes and so is never stubbed. -/
def sizesOf (table : List (ToolResultKey × StubSizes)) : ResultSizes := fun key =>
  (table.lookup key).getD ⟨0, 0⟩

def stubKey (key : ToolResultKey) : ToolResultKey := { key with payloadHash := 0 }

@[simp] theorem stubKey_idempotent (key : ToolResultKey) :
    stubKey (stubKey key) = stubKey key := rfl

/-- Whether a tool result is replaced by its stub: only when the stub is
strictly smaller than the output it replaces (the no-expansion floor). -/
def stubs (sizes : ResultSizes) (key : ToolResultKey) : Bool :=
  stubEligible (sizes key).originalBytes (sizes key).stubBytes

/-- The identity of a tool result after stripping: the stub's identity when
the stub saves bytes, otherwise the result unchanged. -/
def stripKey (sizes : ResultSizes) (key : ToolResultKey) : ToolResultKey :=
  if stubs sizes key then stubKey key else key

theorem stripKey_of_stubs {sizes : ResultSizes} {key : ToolResultKey}
    (h : stubs sizes key = true) : stripKey sizes key = stubKey key := by
  simp [stripKey, h]

theorem stripKey_of_not_stubs {sizes : ResultSizes} {key : ToolResultKey}
    (h : stubs sizes key = false) : stripKey sizes key = key := by
  simp [stripKey, h]

/-- A stub is a fixpoint and a kept result is kept again: its key, and so its
measured sizes, are unchanged. No hypothesis beyond `ResultSizes` being a
function of the key. -/
@[simp] theorem stripKey_idempotent (sizes : ResultSizes) (key : ToolResultKey) :
    stripKey sizes (stripKey sizes key) = stripKey sizes key := by
  cases h : stubs sizes key
  · rw [stripKey_of_not_stubs h, stripKey_of_not_stubs h]
  · rw [stripKey_of_stubs h]
    unfold stripKey
    split <;> rfl

/-- A result whose identity is already a stub's is unchanged by stripping,
whatever its measured sizes. -/
theorem stripKey_of_stub_identity (sizes : ResultSizes) {key : ToolResultKey}
    (h : stubKey key = key) : stripKey sizes key = key := by
  unfold stripKey
  split
  · exact h
  · rfl

/-- The floor: a stub that is not strictly smaller than the output it would
replace (in particular one of equal size) leaves the result as it was. -/
theorem stripKey_keeps_unless_smaller {sizes : ResultSizes} {key : ToolResultKey}
    (h : ¬ (sizes key).stubBytes < (sizes key).originalBytes) : stripKey sizes key = key := by
  apply stripKey_of_not_stubs
  cases hs : stubs sizes key
  · rfl
  · exact absurd ((stubEligible_iff _ _).mp hs) h

def stripKind (sizes : ResultSizes) : MessageKind → MessageKind
  | .toolResult callId key => .toolResult callId (stripKey sizes key)
  | k => k

def stripRow (sizes : ResultSizes) (row : MessageRow) : MessageRow :=
  { row with kind := stripKind sizes row.kind }

def strip (sizes : ResultSizes) (msgs : List MessageRow) : List MessageRow :=
  msgs.map (stripRow sizes)

section
variable {sizes : ResultSizes}

@[simp] theorem stripRow_kind (row : MessageRow) :
    (stripRow sizes row).kind = stripKind sizes row.kind := rfl

@[simp] theorem stripRow_sequence (row : MessageRow) :
    (stripRow sizes row).sequence = row.sequence := rfl

@[simp] theorem stripRow_role (row : MessageRow) :
    (stripRow sizes row).role = row.role := rfl

@[simp] theorem stripRow_messageId (row : MessageRow) :
    (stripRow sizes row).messageId = row.messageId := rfl

@[simp] theorem stripRow_sessionId (row : MessageRow) :
    (stripRow sizes row).sessionId = row.sessionId := rfl

@[simp] theorem strip_nil : strip sizes [] = [] := rfl

@[simp] theorem strip_cons (row : MessageRow) (rest : List MessageRow) :
    strip sizes (row :: rest) = stripRow sizes row :: strip sizes rest := rfl

@[simp] theorem strip_length (msgs : List MessageRow) :
    (strip sizes msgs).length = msgs.length := List.length_map _ _

theorem strip_append (a b : List MessageRow) :
    strip sizes (a ++ b) = strip sizes a ++ strip sizes b :=
  List.map_append _ _ _

theorem strip_take (msgs : List MessageRow) (n : Nat) :
    strip sizes (msgs.take n) = (strip sizes msgs).take n := List.map_take _ _ _

theorem strip_drop (msgs : List MessageRow) (n : Nat) :
    strip sizes (msgs.drop n) = (strip sizes msgs).drop n := List.map_drop _ _ _

theorem stripKind_idempotent (k : MessageKind) :
    stripKind sizes (stripKind sizes k) = stripKind sizes k := by
  cases k <;> simp [stripKind]

theorem stripRow_idempotent (row : MessageRow) :
    stripRow sizes (stripRow sizes row) = stripRow sizes row := by
  simp only [stripRow, stripKind_idempotent]

theorem strip_idempotent (msgs : List MessageRow) :
    strip sizes (strip sizes msgs) = strip sizes msgs := by
  induction msgs with
  | nil => rfl
  | cons row rest ih =>
      rw [strip_cons, strip_cons, stripRow_idempotent, ih]

theorem strip_kind_ordinary (row : MessageRow) (h : row.kind = .ordinary) :
    (stripRow sizes row).kind = .ordinary := by
  rw [stripRow_kind, h]; rfl

theorem strip_kind_assistant (row : MessageRow) (callIds : Finset ToolExecution.ToolCallId)
    (h : row.kind = .assistantToolCalls callIds) :
    (stripRow sizes row).kind = .assistantToolCalls callIds := by
  rw [stripRow_kind, h]; rfl

theorem strip_kind_result (row : MessageRow) (callId : ToolExecution.ToolCallId)
    (key : ToolResultKey) (h : row.kind = .toolResult callId key) :
    (stripRow sizes row).kind = .toolResult callId (stripKey sizes key) := by
  rw [stripRow_kind, h]; rfl

theorem callsIn_strip (l : List MessageRow) :
    PromptAssembly.callsIn (strip sizes l) = PromptAssembly.callsIn l := by
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
          rw [PromptAssembly.callsIn_cons_result _ _ callId (stripKey sizes key)
              (strip_kind_result row callId key hk),
            PromptAssembly.callsIn_cons_result row rest callId key hk, ih]

theorem resolvedIn_strip (l : List MessageRow) :
    PromptAssembly.resolvedIn (strip sizes l) = PromptAssembly.resolvedIn l := by
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
          rw [PromptAssembly.resolvedIn_cons_result _ _ callId (stripKey sizes key)
              (strip_kind_result row callId key hk),
            PromptAssembly.resolvedIn_cons_result row rest callId key hk, ih]

theorem strip_preserves_uniqueCallIds :
    ∀ {l : List MessageRow},
      PromptAssembly.UniqueCallIds l → PromptAssembly.UniqueCallIds (strip sizes l) := by
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
          rw [PromptAssembly.uniqueCallIds_cons_result _ _ callId (stripKey sizes key)
              (strip_kind_result row callId key hk)]
          exact ih ((PromptAssembly.uniqueCallIds_cons_result row rest callId key hk).mp h)

theorem strip_preserves_strictlyIncreasing :
    ∀ {l : List MessageRow},
      Transcript.StrictlyIncreasingMessages l →
        Transcript.StrictlyIncreasingMessages (strip sizes l) := by
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

end

end Compaction
