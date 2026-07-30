import Mathlib.Data.Prod.Lex
import Mathlib.Data.List.Perm.Basic
import Mathlib.Data.String.Basic
import Proofs.Recovery.Contract
import Proofs.Recovery.Outcome

namespace Recovery

inductive ConversationStatus where
  | processing
  | error
  | active
  | completed
  deriving DecidableEq, Repr

namespace ConversationStatus

def toContract : ConversationStatus → String
  | .processing => "processing"
  | .error => "error"
  | .active => "active"
  | .completed => "completed"

def isTerminal : ConversationStatus → Bool
  | .processing => false
  | .error => false
  | .active => true
  | .completed => true

end ConversationStatus

inductive ParentOutcome where
  | completed
  | unfinished
  deriving DecidableEq, Repr

namespace ParentOutcome

def recoveredStatus : ParentOutcome → ConversationStatus
  | .completed => .completed
  | .unfinished => .active

theorem recoveredStatus_terminal (outcome : ParentOutcome) :
    ConversationStatus.isTerminal outcome.recoveredStatus = true := by
  cases outcome <;> rfl

end ParentOutcome

structure ConversationDoc where
  docId : String
  status : ConversationStatus
  updatedAt : Nat
  richness : Nat
  deriving DecidableEq, Repr

structure ConversationGroup where
  sessionId : String
  head : ConversationDoc
  rest : List ConversationDoc
  parent : ParentOutcome
  deriving DecidableEq, Repr

namespace ConversationGroup

def docs (group : ConversationGroup) : List ConversationDoc :=
  group.head :: group.rest

@[simp] theorem docs_ne_nil (group : ConversationGroup) : group.docs ≠ [] := by
  simp [docs]

def isDuplicated (group : ConversationGroup) : Bool :=
  !group.rest.isEmpty

end ConversationGroup

abbrev DocRank := Nat ×ₗ Nat ×ₗ String

def docRank (doc : ConversationDoc) : DocRank :=
  toLex (doc.updatedAt, toLex (doc.richness, doc.docId))

theorem docRank_inj_of_docId (left right : ConversationDoc)
    (h_rank : docRank left = docRank right) :
    left.docId = right.docId := by
  unfold docRank at h_rank
  have h := congrArg ofLex h_rank
  simp [ofLex_toLex] at h
  exact h.2.2

def betterDoc (best candidate : ConversationDoc) : ConversationDoc :=
  if docRank best < docRank candidate then candidate else best

def canonicalOf (group : ConversationGroup) : ConversationDoc :=
  group.rest.foldl betterDoc group.head

theorem betterDoc_eq (best candidate : ConversationDoc) :
    betterDoc best candidate = best ∨ betterDoc best candidate = candidate := by
  unfold betterDoc
  by_cases h : docRank best < docRank candidate <;> simp [h]

theorem foldl_betterDoc_greatest (docs : List ConversationDoc) (start : ConversationDoc) :
    docRank start ≤ docRank (docs.foldl betterDoc start) ∧
      ∀ doc ∈ docs, docRank doc ≤ docRank (docs.foldl betterDoc start) := by
  induction docs generalizing start with
  | nil => exact ⟨le_refl _, by simp⟩
  | cons hd tl ih =>
      obtain ⟨h_start, h_mem⟩ := ih (betterDoc start hd)
      rw [List.foldl_cons]
      have h_better_start : docRank start ≤ docRank (betterDoc start hd) := by
        unfold betterDoc
        by_cases h : docRank start < docRank hd
        · simp [h]
          exact le_of_lt h
        · simp [h]
      have h_better_hd : docRank hd ≤ docRank (betterDoc start hd) := by
        unfold betterDoc
        by_cases h : docRank start < docRank hd
        · simp [h]
        · simp [h]
          exact le_of_not_lt h
      refine ⟨le_trans h_better_start h_start, ?_⟩
      intro doc h_doc
      rcases List.mem_cons.mp h_doc with h_is_hd | h_in_tl
      · subst h_is_hd
        exact le_trans h_better_hd h_start
      · exact h_mem doc h_in_tl

theorem foldl_betterDoc_mem (docs : List ConversationDoc) (start : ConversationDoc) :
    docs.foldl betterDoc start = start ∨ docs.foldl betterDoc start ∈ docs := by
  induction docs generalizing start with
  | nil => exact Or.inl rfl
  | cons hd tl ih =>
      rcases ih (betterDoc start hd) with h | h
      · rcases betterDoc_eq start hd with h_keep | h_take
        · exact Or.inl (by rw [List.foldl_cons, h, h_keep])
        · refine Or.inr ?_
          rw [List.foldl_cons, h, h_take]
          simp
      · refine Or.inr ?_
        rw [List.foldl_cons]
        simp [h]

theorem canonicalOf_mem (group : ConversationGroup) :
    canonicalOf group ∈ group.docs := by
  unfold canonicalOf ConversationGroup.docs
  rcases foldl_betterDoc_mem group.rest group.head with h | h
  · simp [h]
  · simp [h]

theorem canonicalOf_greatest (group : ConversationGroup) :
    ∀ doc ∈ group.docs, docRank doc ≤ docRank (canonicalOf group) := by
  intro doc h_mem
  obtain ⟨h_start, h_rest⟩ := foldl_betterDoc_greatest group.rest group.head
  rcases List.mem_cons.mp (by simpa [ConversationGroup.docs] using h_mem) with h_head | h_tail
  · subst h_head
    exact h_start
  · exact h_rest doc h_tail

theorem canonical_perm_invariant (left right : ConversationGroup)
    (h_perm : left.docs.Perm right.docs)
    (h_distinct : ∀ a ∈ left.docs, ∀ b ∈ left.docs, a.docId = b.docId → a = b) :
    canonicalOf left = canonicalOf right := by
  have h_left_mem := canonicalOf_mem left
  have h_right_mem := canonicalOf_mem right
  have h_right_in_left : canonicalOf right ∈ left.docs :=
    (List.Perm.mem_iff h_perm).mpr h_right_mem
  have h_left_in_right : canonicalOf left ∈ right.docs :=
    (List.Perm.mem_iff h_perm).mp h_left_mem
  have h_le₁ : docRank (canonicalOf right) ≤ docRank (canonicalOf left) :=
    canonicalOf_greatest left (canonicalOf right) h_right_in_left
  have h_le₂ : docRank (canonicalOf left) ≤ docRank (canonicalOf right) :=
    canonicalOf_greatest right (canonicalOf left) h_left_in_right
  have h_rank : docRank (canonicalOf left) = docRank (canonicalOf right) :=
    le_antisymm h_le₂ h_le₁
  exact h_distinct (canonicalOf left) h_left_mem (canonicalOf right) h_right_in_left
    (docRank_inj_of_docId _ _ h_rank)

def conversationStale (group : ConversationGroup) : Prop :=
  ∃ doc ∈ group.docs, ConversationStatus.isTerminal doc.status = false

instance (group : ConversationGroup) : Decidable (conversationStale group) := by
  unfold conversationStale
  infer_instance

def conversationRecover (group : ConversationGroup) : ConversationGroup :=
  let settle := fun doc : ConversationDoc =>
    { doc with status := group.parent.recoveredStatus }
  { group with
      head := settle group.head
      rest := group.rest.map settle }

def conversationMeasure (group : ConversationGroup) : Nat :=
  (group.docs.filter (fun doc => !ConversationStatus.isTerminal doc.status)).length

def conversationTerminal (group : ConversationGroup) : Prop :=
  ∀ doc ∈ group.docs, ConversationStatus.isTerminal doc.status = true

def conversationUninterrupted (group : ConversationGroup) : ConversationGroup :=
  conversationRecover group

@[simp] theorem recover_docs (group : ConversationGroup) :
    (conversationRecover group).docs =
      group.docs.map (fun doc => { doc with status := group.parent.recoveredStatus }) := by
  simp [conversationRecover, ConversationGroup.docs]

theorem conversation_stale_positive :
    ∀ group, conversationStale group → conversationMeasure group > 0 := by
  intro group h_stale
  obtain ⟨doc, h_mem, h_open⟩ := h_stale
  unfold conversationMeasure
  have h_mem_filter :
      doc ∈ group.docs.filter (fun doc => !ConversationStatus.isTerminal doc.status) := by
    simp [List.mem_filter, h_mem, h_open]
  exact List.length_pos_of_mem h_mem_filter

theorem conversation_recover_terminal :
    ∀ group, conversationStale group → conversationTerminal (conversationRecover group) := by
  intro group _h_stale
  unfold conversationTerminal
  intro doc h_mem
  rw [recover_docs] at h_mem
  obtain ⟨pre, _h_pre, h_doc⟩ := List.mem_map.mp h_mem
  rw [← h_doc]
  exact ParentOutcome.recoveredStatus_terminal group.parent

theorem conversation_recover_zero :
    ∀ group, conversationStale group → conversationMeasure (conversationRecover group) = 0 := by
  intro group h_stale
  have h_terminal := conversation_recover_terminal group h_stale
  unfold conversationMeasure
  apply List.length_eq_zero_iff.mpr
  apply List.filter_eq_nil_iff.mpr
  intro doc h_mem
  simp [h_terminal doc h_mem]

theorem conversation_recover_matches_uninterrupted :
    ∀ group, conversationStale group →
      conversationRecover group = conversationUninterrupted group := by
  intro group _h_stale
  rfl

def conversationRecoverySweep : RecoverySweep :=
  { Row := ConversationGroup
  , collection := .agentConversation
  , sweepId := "request_lifecycle_recover_all_conversations"
  , rustFunction := "RequestLifecycle::recover_all"
  , cadence := .startup
  , implementationStatus := .implemented
  , stale := conversationStale
  , recover := conversationRecover
  , terminal := conversationTerminal
  , measure := conversationMeasure
  , h_stale_positive := conversation_stale_positive
  , h_recover_terminal := conversation_recover_terminal
  , h_recover_zero := conversation_recover_zero
  }

def conversationRecoveryEquivalence : RecoveryEquivalence conversationRecoverySweep :=
  { uninterrupted := conversationUninterrupted
  , h_recover_eq_uninterrupted := conversation_recover_matches_uninterrupted
  }

theorem conversation_recovered_not_stale (group : ConversationGroup)
    (h_stale : conversationStale group) :
    ¬ conversationStale (conversationRecover group) := by
  intro h_still
  obtain ⟨doc, h_mem, h_open⟩ := h_still
  have h_terminal := conversation_recover_terminal group h_stale doc h_mem
  rw [h_terminal] at h_open
  exact absurd h_open (by simp)

theorem conversation_recover_idempotent (group : ConversationGroup)
    (_h_stale : conversationStale group) :
    conversationRecover (conversationRecover group) = conversationRecover group := by
  unfold conversationRecover
  simp

theorem duplicate_group_recovers (group : ConversationGroup)
    (h_stale : conversationStale group) :
    conversationTerminal (conversationRecover group) ∧
      conversationMeasure (conversationRecover group) = 0 :=
  ⟨conversation_recover_terminal group h_stale, conversation_recover_zero group h_stale⟩

end Recovery
