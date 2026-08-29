import Proofs.Enrollment.Properties

namespace Enrollment

/-- Unordered durable documents restored from DefraDB. -/
structure DurableDocuments where
  offers : Finset Offer := ∅
  adminPins : Finset NetworkAdminPin := ∅
  requests : Finset Request := ∅
  decisions : Finset Decision := ∅
  revisions : Finset AuthorizationRevision := ∅
  deriving DecidableEq

def requestIdentityUnique (docs : DurableDocuments) (request : Request) : Prop :=
  request ∈ docs.requests ∧
  ∀ other ∈ docs.requests,
    (other.requestId = request.requestId ∨ other.challenge = request.challenge) →
      other = request

instance (docs : DurableDocuments) (request : Request) :
    Decidable (requestIdentityUnique docs request) := by
  unfold requestIdentityUnique; infer_instance

def decisionIdentityUnique (docs : DurableDocuments) (decision : Decision) : Prop :=
  decision ∈ docs.decisions ∧
  ∀ other ∈ docs.decisions, other.requestId = decision.requestId → other = decision

instance (docs : DurableDocuments) (decision : Decision) :
    Decidable (decisionIdentityUnique docs decision) := by
  unfold decisionIdentityUnique; infer_instance

def durableRequestAdmissible
    (docs : DurableDocuments) (offer : Offer) (request : Request) : Prop :=
  requestIdentityUnique docs request ∧
  requestAdmissible
    ({ observedOffers := docs.offers, adminPins := docs.adminPins } : State)
    offer request

instance (docs : DurableDocuments) (offer : Offer) (request : Request) :
    Decidable (durableRequestAdmissible docs offer request) := by
  unfold durableRequestAdmissible; infer_instance

def durableUniqueMaximumRevision
    (docs : DurableDocuments) (revision : AuthorizationRevision) : Prop :=
  uniqueMaximumRevision ({ authorizations := docs.revisions } : State) revision

instance (docs : DurableDocuments) (revision : AuthorizationRevision) :
    Decidable (durableUniqueMaximumRevision docs revision) := by
  unfold durableUniqueMaximumRevision; infer_instance

/--
An approval restored from unordered documents is authoritative only when every
identity boundary is unique and its exact active revision is the unique scoped
maximum. No replay order or GraphQL row order participates in this predicate.
-/
def durableCurrentApproval
    (docs : DurableDocuments) (offer : Offer) (request : Request) (decision : Decision) : Prop :=
  durableRequestAdmissible docs offer request ∧
  decisionIdentityUnique docs decision ∧
  decisionMatchesRequest request decision ∧
  decision.kind = .approved ∧ decision.adminSigned = true ∧ decision.fresh = true ∧
  durableUniqueMaximumRevision docs (revisionForApproval request decision)

instance (docs : DurableDocuments) (offer : Offer) (request : Request) (decision : Decision) :
    Decidable (durableCurrentApproval docs offer request decision) := by
  unfold durableCurrentApproval; infer_instance

theorem conflicting_request_id_fails_closed
    {docs : DurableDocuments} {first second : Request}
    (hfirst : first ∈ docs.requests) (hsecond : second ∈ docs.requests)
    (hid : first.requestId = second.requestId) (hne : first ≠ second) :
    ¬ requestIdentityUnique docs first ∧ ¬ requestIdentityUnique docs second := by
  constructor
  · rintro ⟨_, hunique⟩
    exact hne (hunique second hsecond (Or.inl hid.symm)).symm
  · rintro ⟨_, hunique⟩
    exact hne (hunique first hfirst (Or.inl hid))

theorem conflicting_challenge_fails_closed
    {docs : DurableDocuments} {first second : Request}
    (hfirst : first ∈ docs.requests) (hsecond : second ∈ docs.requests)
    (hchallenge : first.challenge = second.challenge) (hne : first ≠ second) :
    ¬ requestIdentityUnique docs first ∧ ¬ requestIdentityUnique docs second := by
  constructor
  · rintro ⟨_, hunique⟩
    exact hne (hunique second hsecond (Or.inr hchallenge.symm)).symm
  · rintro ⟨_, hunique⟩
    exact hne (hunique first hfirst (Or.inr hchallenge))

theorem conflicting_terminal_decision_fails_closed
    {docs : DurableDocuments} {first second : Decision}
    (hfirst : first ∈ docs.decisions) (hsecond : second ∈ docs.decisions)
    (hid : first.requestId = second.requestId) (hne : first ≠ second) :
    ¬ decisionIdentityUnique docs first ∧ ¬ decisionIdentityUnique docs second := by
  constructor
  · rintro ⟨_, hunique⟩
    exact hne (hunique second hsecond hid.symm).symm
  · rintro ⟨_, hunique⟩
    exact hne (hunique first hfirst hid)

theorem durable_current_approval_requires_unique_request
    {docs : DurableDocuments} {offer : Offer} {request : Request} {decision : Decision}
    (hcurrent : durableCurrentApproval docs offer request decision) :
    requestIdentityUnique docs request := hcurrent.1.1

theorem durable_current_approval_requires_unique_decision
    {docs : DurableDocuments} {offer : Offer} {request : Request} {decision : Decision}
    (hcurrent : durableCurrentApproval docs offer request decision) :
    decisionIdentityUnique docs decision := hcurrent.2.1

theorem durable_equal_revision_conflict_retracts_approval
    {docs : DurableDocuments} {offer : Offer} {request : Request} {decision : Decision}
    {conflict : AuthorizationRevision}
    (hbase : revisionForApproval request decision ∈ docs.revisions)
    (hconflict : conflict ∈ docs.revisions)
    (hsame : sameMember conflict (revisionForApproval request decision))
    (hseq : conflict.sequence = (revisionForApproval request decision).sequence)
    (hne : conflict ≠ revisionForApproval request decision) :
    ¬ durableCurrentApproval docs offer request decision := by
  intro hcurrent
  exact (equal_sequence_conflict_has_no_unique_maximum hconflict hbase hsame hseq hne).2
    hcurrent.2.2.2.2.2.2

end Enrollment
