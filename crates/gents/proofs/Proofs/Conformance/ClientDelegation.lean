import Proofs.Store.Identity.ClientDelegation
import Proofs.Clients.RuntimeAccess
import Proofs.Clients.AuthoredOutbox
import Proofs.Conformance.Family

namespace Conformance.ClientDelegation

open Lean Identity.ClientDelegation

structure Input where
  issuerMatches : Bool
  workerMatches : Bool
  deploymentMatches : Bool
  fresh : Bool
  verified : Bool
  deriving ToJson

@[conformance_family]
def authentication : Family Input Bool where
  inputs := [false, true].flatMap fun issuerMatches =>
    [false, true].flatMap fun workerMatches =>
      [false, true].flatMap fun deploymentMatches =>
        [false, true].flatMap fun fresh =>
          [false, true].map fun verified =>
            { issuerMatches, workerMatches, deploymentMatches, fresh, verified }
  model := fun i => authenticates "client" ⟨"runtime", "worker", 1⟩ 10
    { issuer := if i.issuerMatches then "client" else "another-client"
      worker := if i.workerMatches then "worker" else "another-worker"
      deployment := if i.deploymentMatches then 1 else 2
      expires := if i.fresh then 20 else 10
      verified := i.verified }

structure GraphInput where
  remote : Bool
  presented : Bool
  actorMatches : Bool
  deriving ToJson

@[conformance_family]
def clientGraph : Family GraphInput Bool where
  inputs := [false, true].flatMap fun remote =>
    [false, true].flatMap fun presented =>
      [false, true].map fun actorMatches => { remote, presented, actorMatches }
  model := fun i => clientGraphReady i.remote i.presented i.actorMatches

structure AccessDecisionInput where
  before : String
  decision : String
  nativeAuthority : Bool
  effectsComplete : Bool
  deriving ToJson

@[conformance_family]
def accessDecision : Family AccessDecisionInput String where
  inputs := ["pending", "approved", "rejected", "revoked"].flatMap fun before =>
    ["approve", "reject", "revoke"].flatMap fun decision =>
      [false, true].flatMap fun nativeAuthority =>
        [false, true].map fun effectsComplete => { before, decision, nativeAuthority, effectsComplete }
  model := fun i =>
    let before := match i.before with
      | "approved" => ClientRuntimeAccess.AccessRequestState.approved
      | "rejected" => .rejected | "revoked" => .revoked | _ => .pending
    let decision := match i.decision with
      | "approve" => ClientRuntimeAccess.AccessDecision.approve
      | "reject" => .reject | _ => .revoke
    ClientRuntimeAccess.requestStateToken <|
      ClientRuntimeAccess.settleRequest before decision i.nativeAuthority i.effectsComplete


structure OutboxReservationInput where
  localSigner : Bool
  bindingValid : Bool
  retainedExists : Bool
  same : Bool
  deriving ToJson

@[conformance_family]
def outboxReservation : Family OutboxReservationInput String where
  inputs := [false, true].flatMap fun localSigner =>
    [false, true].flatMap fun bindingValid =>
        [false, true].flatMap fun retainedExists =>
            [false, true].map fun same =>
                { localSigner, bindingValid, retainedExists, same }
  model := fun i => ClientAuthoredOutbox.reservationToken <|
    ClientAuthoredOutbox.reserve i.localSigner i.bindingValid i.retainedExists i.same

structure OutboxPreparationInput where
  bindingMatches : Bool
  pending : Bool
  retainedExists : Bool
  same : Bool
  deriving ToJson

@[conformance_family]
def outboxPreparation : Family OutboxPreparationInput String where
  inputs := [false, true].flatMap fun bindingMatches =>
    [false, true].flatMap fun pending =>
        [false, true].flatMap fun retainedExists =>
            [false, true].map fun same =>
                { bindingMatches, pending, retainedExists, same }
  model := fun i => ClientAuthoredOutbox.reservationToken <|
    ClientAuthoredOutbox.prepare i.bindingMatches i.pending i.retainedExists i.same

structure OutboxSelectionInput where
  pending : List String
  steer : Option String
  deriving ToJson

@[conformance_family]
def outboxSelection : Family OutboxSelectionInput (Option String) where
  inputs := [[], ["first"], ["first", "second"]].flatMap fun pending =>
    [none, some "first", some "second", some "missing"].map fun steer => { pending, steer }
  model := fun i => ClientAuthoredOutbox.select i.pending i.steer

structure OutboxSessionInput where
  submission : String
  session : Option String
  deriving ToJson

@[conformance_family]
def outboxSession : Family OutboxSessionInput String where
  inputs := ["first", "second"].flatMap fun submission =>
    [none, some "existing", some ""].map fun session => { submission, session }
  model := fun i => ClientAuthoredOutbox.sessionBinding i.submission i.session

end Conformance.ClientDelegation
