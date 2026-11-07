import Proofs.Enrollment.State

namespace Enrollment.AutoRenewal

def unansweredRetrySeconds (remaining attempts : Nat) : Nat :=
  min (max 1 (remaining / 3)) (min 86400 (3600 * 2 ^ min attempts 5))

theorem unanswered_retry_is_bounded (remaining attempts : Nat) :
    unansweredRetrySeconds remaining attempts ≤ 86400 := by
  simp [unansweredRetrySeconds]

structure Generation where
  network : String
  member : String
  peer : String
  owner : String
  profile : String
  expiresAt : Nat
  leaseSeconds : Nat
  renewalLeaseLimit : Option Nat
  sequence : Nat
  nextSequence : Nat
  authorityConflict : Bool
  revoked : Bool
  enabled : Bool
  verified : Bool
  deriving DecidableEq, Repr

structure Proposal where
  network : String
  member : String
  peer : String
  owner : String
  profile : String
  leaseSeconds : Nat
  memberSigned : Bool
  offerFresh : Bool
  /-- Bound by the authenticated renewal offer; an unauthenticated request
  field is not an authorization fence. -/
  predecessorSequence : Nat
  deriving DecidableEq, Repr

structure Intent where
  network : String
  member : String
  peer : String
  predecessorSequence : Nat
  issuedAt : Nat
  expiresAt : Nat
  memberSigned : Bool
  transportAuthenticated : Bool
  deriving DecidableEq, Repr

/-- A request for a renewal offer grants no authority. It must come from the
current member on its authenticated route and cannot outlive its signed window. -/
def mayOffer (current : Generation) (intent : Intent) (now window maxValidity : Nat) : Bool :=
  current.enabled && current.verified && !current.revoked && !current.authorityConflict &&
    decide (current.nextSequence = current.sequence + 1) &&
    decide (now < current.expiresAt ∧ current.expiresAt ≤ now + window) &&
    intent.memberSigned && intent.transportAuthenticated &&
    decide (intent.issuedAt ≤ now ∧ now < intent.expiresAt ∧
      intent.expiresAt ≤ intent.issuedAt + maxValidity) &&
    intent.network == current.network && intent.member == current.member &&
    intent.peer == current.peer && intent.predecessorSequence == current.sequence

theorem unauthenticated_cannot_request_offer (current : Generation) (intent : Intent)
    (now window maxValidity : Nat) (h : intent.transportAuthenticated = false) :
    mayOffer current intent now window maxValidity = false := by
  simp [mayOffer, h]

theorem unsigned_cannot_request_offer (current : Generation) (intent : Intent)
    (now window maxValidity : Nat) (h : intent.memberSigned = false) :
    mayOffer current intent now window maxValidity = false := by
  simp [mayOffer, h]

theorem expired_intent_cannot_request_offer (current : Generation) (intent : Intent)
    (now window maxValidity : Nat) (h : intent.expiresAt ≤ now) :
    mayOffer current intent now window maxValidity = false := by
  simp [mayOffer, Nat.not_lt.mpr h]

/-- Automatic approval never replaces the member's signature or exceeds the
operator's renewal limit. Expired or revoked generations require an operator. -/
def allowed (current : Generation) (proposal : Proposal) (now window protocolMax : Nat) : Bool :=
  current.enabled && current.verified && !current.revoked && !current.authorityConflict &&
    decide (current.nextSequence = current.sequence + 1) && decide (now < current.expiresAt) &&
    decide (current.expiresAt ≤ now + window) && proposal.memberSigned && proposal.offerFresh &&
    decide (proposal.leaseSeconds > 0 ∧
      proposal.leaseSeconds ≤ current.renewalLeaseLimit.getD current.leaseSeconds ∧
      proposal.leaseSeconds ≤ protocolMax) &&
    proposal.predecessorSequence == current.sequence &&
    proposal.network == current.network && proposal.member == current.member &&
    proposal.peer == current.peer && proposal.owner == current.owner &&
    proposal.profile == current.profile

theorem revoked_cannot_auto_renew (current : Generation) (proposal : Proposal)
    (now window protocolMax : Nat) (h : current.revoked = true) :
    allowed current proposal now window protocolMax = false := by
  simp [allowed, h]

theorem disabled_cannot_auto_renew (current : Generation) (proposal : Proposal)
    (now window protocolMax : Nat) (h : current.enabled = false) :
    allowed current proposal now window protocolMax = false := by
  simp [allowed, h]

theorem unsigned_cannot_auto_renew (current : Generation) (proposal : Proposal)
    (now window protocolMax : Nat) (h : proposal.memberSigned = false) :
    allowed current proposal now window protocolMax = false := by
  simp [allowed, h]

theorem expired_cannot_auto_renew (current : Generation) (proposal : Proposal)
    (now window protocolMax : Nat) (h : current.expiresAt ≤ now) :
    allowed current proposal now window protocolMax = false := by
  simp [allowed, Nat.not_lt.mpr h]

theorem superseded_cannot_auto_renew (current : Generation) (proposal : Proposal)
    (now window protocolMax : Nat) (h : proposal.predecessorSequence ≠ current.sequence) :
    allowed current proposal now window protocolMax = false := by
  simp [allowed, h]

theorem premature_cannot_auto_renew (current : Generation) (proposal : Proposal)
    (now window protocolMax : Nat) (h : now + window < current.expiresAt) :
    allowed current proposal now window protocolMax = false := by
  simp [allowed, Nat.not_le.mpr h]

theorem excessive_lease_cannot_auto_renew (current : Generation) (proposal : Proposal)
    (now window protocolMax : Nat)
    (h : current.renewalLeaseLimit.getD current.leaseSeconds < proposal.leaseSeconds) :
    allowed current proposal now window protocolMax = false := by
  simp [allowed, Nat.not_le.mpr h]

theorem changed_member_cannot_auto_renew (current : Generation) (proposal : Proposal)
    (now window protocolMax : Nat) (h : proposal.member ≠ current.member) :
    allowed current proposal now window protocolMax = false := by
  simp [allowed, h]

theorem approved_renewal_is_bound_to_current_generation
    (current : Generation) (proposal : Proposal) (now window protocolMax : Nat)
    (h : allowed current proposal now window protocolMax = true) :
    proposal.predecessorSequence = current.sequence := by
  by_contra different
  have refused := superseded_cannot_auto_renew current proposal now window protocolMax different
  rw [refused] at h
  contradiction

def exampleGeneration : Generation :=
  { network := "network", member := "member", peer := "peer", owner := "owner",
    profile := "client", expiresAt := 100, leaseSeconds := 30,
    renewalLeaseLimit := none, sequence := 1, nextSequence := 2, authorityConflict := false,
    revoked := false, enabled := true, verified := true }

def exampleProposal : Proposal :=
  { network := "network", member := "member", peer := "peer", owner := "owner",
    profile := "client", leaseSeconds := 30, memberSigned := true, offerFresh := true,
    predecessorSequence := 1 }

def exampleIntent : Intent :=
  { network := "network", member := "member", peer := "peer", predecessorSequence := 1,
    issuedAt := 90, expiresAt := 99, memberSigned := true, transportAuthenticated := true }

def intentBoundaryCases : List (String × Generation × Intent × Nat × Nat × Nat) :=
  [ ("valid", exampleGeneration, exampleIntent, 95, 10, 10)
  , ("unsigned", exampleGeneration, { exampleIntent with memberSigned := false }, 95, 10, 10)
  , ("wrong_transport", exampleGeneration, { exampleIntent with transportAuthenticated := false }, 95, 10, 10)
  , ("wrong_peer", exampleGeneration, { exampleIntent with peer := "other" }, 95, 10, 10)
  , ("wrong_member", exampleGeneration, { exampleIntent with member := "other" }, 95, 10, 10)
  , ("wrong_network", exampleGeneration, { exampleIntent with network := "other" }, 95, 10, 10)
  , ("stale_generation", exampleGeneration, { exampleIntent with predecessorSequence := 2 }, 95, 10, 10)
  , ("expired_request", exampleGeneration, { exampleIntent with expiresAt := 95 }, 95, 10, 10)
  , ("too_long", exampleGeneration, { exampleIntent with expiresAt := 101 }, 95, 10, 10)
  , ("future_request", exampleGeneration, { exampleIntent with issuedAt := 96 }, 95, 10, 10)
  , ("expired_generation", exampleGeneration, exampleIntent, 100, 10, 10)
  , ("disabled", { exampleGeneration with enabled := false }, exampleIntent, 95, 10, 10)
  , ("conflicted", { exampleGeneration with authorityConflict := true }, exampleIntent, 95, 10, 10)
  , ("revoked", { exampleGeneration with revoked := true }, exampleIntent, 95, 10, 10) ]

theorem offer_boundaries :
    intentBoundaryCases.map (fun (_, g, i, now, window, valid) =>
      mayOffer g i now window valid) =
      [true, false, false, false, false, false, false, false, false, false,
       false, false, false, false] := by
  rfl

structure BoundaryCase where
  name : String
  current : Generation
  proposal : Proposal
  now : Nat
  window : Nat := 10
  protocolMax : Nat := 90

def boundaryInputCases : List BoundaryCase :=
  [ ⟨"allowed", exampleGeneration, exampleProposal, 95, 10, 90⟩
  , ⟨"outside_window", exampleGeneration, exampleProposal, 89, 10, 90⟩
  , ⟨"expired", exampleGeneration, exampleProposal, 100, 10, 90⟩
  , ⟨"revoked", { exampleGeneration with revoked := true }, exampleProposal, 95, 10, 90⟩
  , ⟨"disabled", { exampleGeneration with enabled := false }, exampleProposal, 95, 10, 90⟩
  , ⟨"unverified", { exampleGeneration with verified := false }, exampleProposal, 95, 10, 90⟩
  , ⟨"conflicted", { exampleGeneration with authorityConflict := true }, exampleProposal, 95, 10, 90⟩
  , ⟨"newer_authority", { exampleGeneration with nextSequence := 3 }, exampleProposal, 95, 10, 90⟩
  , ⟨"superseded", { exampleGeneration with sequence := 2 }, exampleProposal, 95, 10, 90⟩
  , ⟨"unsigned", exampleGeneration, { exampleProposal with memberSigned := false }, 95, 10, 90⟩
  , ⟨"stale_offer", exampleGeneration, { exampleProposal with offerFresh := false }, 95, 10, 90⟩
  , ⟨"changed_member", exampleGeneration, { exampleProposal with member := "other" }, 95, 10, 90⟩
  , ⟨"changed_peer", exampleGeneration, { exampleProposal with peer := "other" }, 95, 10, 90⟩
  , ⟨"changed_profile", exampleGeneration, { exampleProposal with profile := "other" }, 95, 10, 90⟩
  , ⟨"changed_network", exampleGeneration, { exampleProposal with network := "other" }, 95, 10, 90⟩
  , ⟨"changed_owner", exampleGeneration, { exampleProposal with owner := "other" }, 95, 10, 90⟩
  , ⟨"zero_lease", exampleGeneration, { exampleProposal with leaseSeconds := 0 }, 95, 10, 90⟩
  , ⟨"extended_without_limit", exampleGeneration, { exampleProposal with leaseSeconds := 31 }, 95, 10, 90⟩
  , ⟨"configured_limit", { exampleGeneration with renewalLeaseLimit := some 60 },
      { exampleProposal with leaseSeconds := 60 }, 95, 10, 90⟩
  , ⟨"protocol_limit", { exampleGeneration with renewalLeaseLimit := some 100 },
      { exampleProposal with leaseSeconds := 91 }, 95, 10, 90⟩ ]

def boundaryCases : List (String × Bool) :=
  boundaryInputCases.map fun c => (c.name, allowed c.current c.proposal c.now c.window c.protocolMax)

theorem boundary_cases_preserve_renewal_limits :
    boundaryCases.map Prod.snd =
      [true, false, false, false, false, false, false, false, false, false, false, false,
       false, false, false, false, false, false, true, false] := by
  rfl

end Enrollment.AutoRenewal
