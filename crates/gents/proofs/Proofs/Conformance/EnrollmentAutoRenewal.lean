import Proofs.P2P.Enrollment.AutoRenewal
import Proofs.Conformance.Family

namespace Conformance.EnrollmentAutoRenewal

open Lean Enrollment.AutoRenewal

deriving instance ToJson for Intent, Proposal

structure IntentCurrent where
  network : String
  member : String
  peer : String
  expiresAt : Nat
  sequence : Nat
  nextSequence : Nat
  conflict : Bool
  revoked : Bool
  enabled : Bool
  verified : Bool
  deriving ToJson

def IntentCurrent.ofModel (g : Generation) : IntentCurrent :=
  { network := g.network, member := g.member, peer := g.peer, expiresAt := g.expiresAt
    sequence := g.sequence, nextSequence := g.nextSequence, conflict := g.authorityConflict
    revoked := g.revoked, enabled := g.enabled, verified := g.verified }

/-- `mayOffer` reads none of the fields an intent case leaves out; they are
taken from the example generation every intent case starts from. -/
def IntentCurrent.toModel (c : IntentCurrent) : Generation :=
  { exampleGeneration with
    network := c.network, member := c.member, peer := c.peer, expiresAt := c.expiresAt
    sequence := c.sequence, nextSequence := c.nextSequence, authorityConflict := c.conflict
    revoked := c.revoked, enabled := c.enabled, verified := c.verified }

structure IntentInput where
  name : String
  current : IntentCurrent
  intent : Intent
  now : Nat
  window : Nat
  maxValidity : Nat
  deriving ToJson

def intentInputs : List IntentInput :=
  intentBoundaryCases.map fun (name, g, i, now, window, maxValidity) =>
    { name, current := IntentCurrent.ofModel g, intent := i, now, window, maxValidity }

structure IntentProjection where
  projectionAllowed : Bool
  deriving ToJson

@[conformance_family]
def intentProjection : Family IntentInput IntentProjection where
  inputs := intentInputs
  model := fun i =>
    { projectionAllowed := offerProjection i.current.toModel i.intent i.now i.window i.maxValidity }

structure Offer where
  allowed : Bool
  deriving ToJson

/-- The served offer scenario builds one enabled current generation on two
live nodes and varies only the policy switch, the intent signature and its
peer, so it reaches exactly these cases. -/
def offerScenarioNames : List String := ["disabled", "unsigned", "wrong_peer", "valid"]

@[conformance_family]
def offer : Family IntentInput Offer where
  inputs := intentInputs.filter fun i => i.name ∈ offerScenarioNames
  model := fun i => { allowed := mayOffer i.current.toModel i.intent i.now i.window i.maxValidity }

/-- model-production-gap: `issue_renewal_offer` is reached only through a
served two-node offer exchange whose fixture holds one enabled current
generation; no production scenario builds a conflicted, revoked or expired
generation, a transport identity mismatch, or a re-signed intent with another
network, member, sequence or validity window per case. `intentProjection`
replays the generation and window checks of every case through
`current_for_renewal_intent`. -/
@[conformance_family]
def offerUnreached : Family IntentInput Offer where
  inputs := intentInputs.filter fun i => i.name ∉ offerScenarioNames
  model := fun i => { allowed := mayOffer i.current.toModel i.intent i.now i.window i.maxValidity }

structure ApprovalCurrent where
  network : String
  member : String
  peer : String
  owner : String
  profile : String
  expiresAt : Nat
  leaseSeconds : Nat
  renewalLimit : Option Nat
  sequence : Nat
  revoked : Bool
  nextSequence : Nat
  authorityConflict : Bool
  enabled : Bool
  verified : Bool
  deriving ToJson

def ApprovalCurrent.ofModel (g : Generation) : ApprovalCurrent :=
  { network := g.network, member := g.member, peer := g.peer, owner := g.owner
    profile := g.profile, expiresAt := g.expiresAt, leaseSeconds := g.leaseSeconds
    renewalLimit := g.renewalLeaseLimit, sequence := g.sequence, revoked := g.revoked
    nextSequence := g.nextSequence, authorityConflict := g.authorityConflict
    enabled := g.enabled, verified := g.verified }

def ApprovalCurrent.toModel (c : ApprovalCurrent) : Generation :=
  { network := c.network, member := c.member, peer := c.peer, owner := c.owner
    profile := c.profile, expiresAt := c.expiresAt, leaseSeconds := c.leaseSeconds
    renewalLeaseLimit := c.renewalLimit, sequence := c.sequence, nextSequence := c.nextSequence
    authorityConflict := c.authorityConflict, revoked := c.revoked, enabled := c.enabled
    verified := c.verified }

structure ApprovalInput where
  name : String
  current : ApprovalCurrent
  proposal : Proposal
  now : Nat
  window : Nat
  protocolMax : Nat
  deriving ToJson

/-- An approved renewal leases the operator's renewal limit, or the current
lease when none is configured: the value `allowed` bounds by the protocol
maximum. -/
structure Approval where
  projectionAllowed : Bool
  lease : Option Nat
  deriving ToJson

@[conformance_family]
def approval : Family ApprovalInput Approval where
  inputs := boundaryInputCases.map fun c =>
    { name := c.name, current := ApprovalCurrent.ofModel c.current, proposal := c.proposal
      now := c.now, window := c.window, protocolMax := c.protocolMax }
  model := fun i =>
    let current := i.current.toModel
    let approved := approvalProjection current i.proposal i.now i.window i.protocolMax
    { projectionAllowed := approved
      lease := if approved then some (current.renewalLeaseLimit.getD current.leaseSeconds) else none }

end Conformance.EnrollmentAutoRenewal
