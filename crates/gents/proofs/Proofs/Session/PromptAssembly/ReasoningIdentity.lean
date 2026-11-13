/-! Replaying a persisted assistant row's reasoning needs the row's physical
identity: the provider response header and the block indices it carried.
Evidence of an accepted, complete provider response restores the identity of a
row persisted without it. Evidence recovered from an incomplete response is
replayable for a row that kept its identity, but never supplies one. A row
whose reasoning is not replayed keeps its ordinary blocks. -/

namespace PromptAssembly.ReasoningIdentity

inductive EvidenceOrigin where
  | acceptedProvider
  | recoveredProvider
  deriving DecidableEq, Repr

inductive Block where
  | reasoning
  | text
  deriving DecidableEq, Repr

def restoresIdentity (origin : EvidenceOrigin) (complete : Bool) : Bool :=
  origin == .acceptedProvider && complete

def replayableEvidence (origin : EvidenceOrigin) (complete : Bool) : Bool :=
  (origin == .acceptedProvider && complete) || (origin == .recoveredProvider && !complete)

def replaysReasoning (origin : EvidenceOrigin) (complete hasIdentity : Bool) : Bool :=
  (hasIdentity || restoresIdentity origin complete) && replayableEvidence origin complete

def retained (replayed : Bool) (content : List Block) : List Block :=
  if replayed then content else content.filter (· != .reasoning)

theorem recovered_reasoning_does_not_regain_missing_physical_identity :
    replaysReasoning .recoveredProvider false false = false ∧
      retained (replaysReasoning .recoveredProvider false false) [.reasoning, .text] = [.text] := by
  decide

theorem only_accepted_complete_evidence_restores_identity (origin : EvidenceOrigin) (complete : Bool) :
    replaysReasoning origin complete false = (origin == .acceptedProvider && complete) := by
  cases origin <;> cases complete <;> decide

end PromptAssembly.ReasoningIdentity
