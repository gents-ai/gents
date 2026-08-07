/-!
# Request ingest provenance

This model captures the minimum provenance checks required before a request is
processed. Access policy and encryption are intentionally outside its scope:
they can be layered on after the signed source and the target agent's claim are
known to be authentic and bound to the same immutable request fact.

The source fact is admitted only when its signature comes from the expected
author, its composite view has exactly one head, and that head is the exact CID
selected for processing. A claim is admitted only when the target agent signs
it, names that source CID as its parent, and preserves the source payload.

Internal requests keep requester attribution separate from authorship. Their
expected source signer is the creating agent, so a request can carry a distinct
requester without pretending that requester signed the materialized document.
-/

namespace RequestIngest

abbrev Did := Nat
abbrev Cid := Nat
abbrev Payload := Nat

inductive Origin where
  | external
  | internal
  deriving DecidableEq, Repr

structure SourceEvidence where
  origin : Origin
  requesterDid : Did
  sourceAuthorDid : Did
  targetAgentDid : Did
  sourceSignerDid : Did
  sourceSignatureValid : Bool
  sourceHeadCount : Nat
  observedSourceCid : Cid
  sourceCid : Cid
  payload : Payload
  deriving DecidableEq, Repr

namespace SourceEvidence

/-- External sources are authored by the requester. Internal materializations
are authored by the agent that created the document. -/
def expectedSigner (source : SourceEvidence) : Did :=
  match source.origin with
  | .external => source.requesterDid
  | .internal => source.sourceAuthorDid

/-- The complete source-side provenance gate. -/
def admitted (source : SourceEvidence) : Bool :=
  source.sourceSignatureValid &&
    source.sourceSignerDid == source.expectedSigner &&
    source.sourceHeadCount == 1 &&
    source.observedSourceCid == source.sourceCid

theorem admitted_iff (source : SourceEvidence) :
    source.admitted = true ↔
      source.sourceSignatureValid = true ∧
      source.sourceSignerDid = source.expectedSigner ∧
      source.sourceHeadCount = 1 ∧
      source.observedSourceCid = source.sourceCid := by
  constructor
  · intro h
    have grouped :
        ((source.sourceSignatureValid = true ∧
          source.sourceSignerDid = source.expectedSigner) ∧
          source.sourceHeadCount = 1) ∧
          source.observedSourceCid = source.sourceCid := by
      simpa [admitted] using h
    exact ⟨grouped.1.1.1, grouped.1.1.2, grouped.1.2, grouped.2⟩
  · rintro ⟨hsig, hsigner, hhead, hcid⟩
    simp [admitted, hsig, hsigner, hhead, hcid]

theorem admitted_has_valid_signature (source : SourceEvidence)
    (h : source.admitted = true) : source.sourceSignatureValid = true := by
  exact (source.admitted_iff.mp h).1

theorem admitted_has_expected_signer (source : SourceEvidence)
    (h : source.admitted = true) :
    source.sourceSignerDid = source.expectedSigner := by
  exact (source.admitted_iff.mp h).2.1

theorem admitted_has_unique_head (source : SourceEvidence)
    (h : source.admitted = true) : source.sourceHeadCount = 1 := by
  exact (source.admitted_iff.mp h).2.2.1

theorem admitted_pins_exact_cid (source : SourceEvidence)
    (h : source.admitted = true) :
    source.observedSourceCid = source.sourceCid := by
  exact (source.admitted_iff.mp h).2.2.2

end SourceEvidence

structure ClaimEvidence where
  source : SourceEvidence
  claimSignerDid : Did
  claimSignatureValid : Bool
  claimParentCid : Cid
  claimPayload : Payload
  deriving DecidableEq, Repr

namespace ClaimEvidence

/-- The complete claim-side gate, including prior source admission. -/
def admitted (claim : ClaimEvidence) : Bool :=
  claim.source.admitted &&
    claim.claimSignatureValid &&
    claim.claimSignerDid == claim.source.targetAgentDid &&
    claim.claimParentCid == claim.source.sourceCid &&
    claim.claimPayload == claim.source.payload

theorem admitted_iff (claim : ClaimEvidence) :
    claim.admitted = true ↔
      claim.source.admitted = true ∧
      claim.claimSignatureValid = true ∧
      claim.claimSignerDid = claim.source.targetAgentDid ∧
      claim.claimParentCid = claim.source.sourceCid ∧
      claim.claimPayload = claim.source.payload := by
  constructor
  · intro h
    have grouped :
        (((claim.source.admitted = true ∧
          claim.claimSignatureValid = true) ∧
          claim.claimSignerDid = claim.source.targetAgentDid) ∧
          claim.claimParentCid = claim.source.sourceCid) ∧
          claim.claimPayload = claim.source.payload := by
      simpa [admitted] using h
    exact ⟨grouped.1.1.1.1, grouped.1.1.1.2, grouped.1.1.2,
      grouped.1.2, grouped.2⟩
  · rintro ⟨hsource, hsig, hsigner, hparent, hpayload⟩
    simp [admitted, hsource, hsig, hsigner, hparent, hpayload]

theorem admitted_has_valid_source (claim : ClaimEvidence)
    (h : claim.admitted = true) : claim.source.admitted = true := by
  exact (claim.admitted_iff.mp h).1

theorem admitted_has_target_agent_signer (claim : ClaimEvidence)
    (h : claim.admitted = true) :
    claim.claimSignerDid = claim.source.targetAgentDid := by
  exact (claim.admitted_iff.mp h).2.2.1

theorem admitted_binds_source_parent (claim : ClaimEvidence)
    (h : claim.admitted = true) :
    claim.claimParentCid = claim.source.sourceCid := by
  exact (claim.admitted_iff.mp h).2.2.2.1

theorem admitted_preserves_payload (claim : ClaimEvidence)
    (h : claim.admitted = true) :
    claim.claimPayload = claim.source.payload := by
  exact (claim.admitted_iff.mp h).2.2.2.2

end ClaimEvidence

inductive Outcome where
  | sourceRejected
  | claimRejected
  | admitted
  deriving DecidableEq, Repr

namespace Outcome

def toContract : Outcome → String
  | .sourceRejected => "sourceRejected"
  | .claimRejected => "claimRejected"
  | .admitted => "admitted"

end Outcome

/-- Source failures are distinguished from claim failures so production can
reject before creating or processing a target-agent claim. -/
def evaluate (claim : ClaimEvidence) : Outcome :=
  if claim.admitted then
    .admitted
  else if claim.source.admitted then
    .claimRejected
  else
    .sourceRejected

theorem evaluate_admitted_iff (claim : ClaimEvidence) :
    evaluate claim = .admitted ↔ claim.admitted = true := by
  cases hclaim : claim.admitted <;>
    cases hsource : claim.source.admitted <;>
    simp [evaluate, hclaim, hsource]

theorem admitted_provenance (claim : ClaimEvidence)
    (h : evaluate claim = .admitted) :
    claim.source.sourceSignatureValid = true ∧
    claim.source.sourceSignerDid = claim.source.expectedSigner ∧
    claim.source.sourceHeadCount = 1 ∧
    claim.source.observedSourceCid = claim.source.sourceCid ∧
    claim.claimSignatureValid = true ∧
    claim.claimSignerDid = claim.source.targetAgentDid ∧
    claim.claimParentCid = claim.source.sourceCid ∧
    claim.claimPayload = claim.source.payload := by
  have admitted :=
    (ClaimEvidence.admitted_iff claim).mp ((evaluate_admitted_iff claim).mp h)
  have source := (SourceEvidence.admitted_iff claim.source).mp admitted.1
  exact ⟨source.1, source.2.1, source.2.2.1, source.2.2.2,
    admitted.2.1, admitted.2.2.1, admitted.2.2.2.1, admitted.2.2.2.2⟩

/-- A distinct requester on an internal request remains attribution, not proof
of authorship: the target agent's source signature is the accepted author. -/
theorem internal_request_with_distinct_requester_is_admitted :
    let source : SourceEvidence :=
      { origin := .internal
      , requesterDid := 7
      , sourceAuthorDid := 13
      , targetAgentDid := 11
      , sourceSignerDid := 13
      , sourceSignatureValid := true
      , sourceHeadCount := 1
      , observedSourceCid := 101
      , sourceCid := 101
      , payload := 303
      }
    let claim : ClaimEvidence :=
      { source := source
      , claimSignerDid := 11
      , claimSignatureValid := true
      , claimParentCid := 101
      , claimPayload := 303
      }
    source.requesterDid ≠ source.sourceAuthorDid ∧ evaluate claim = .admitted := by
  decide

end RequestIngest
