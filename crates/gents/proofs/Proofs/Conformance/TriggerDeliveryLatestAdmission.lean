import Proofs.Session.Request.State
import Proofs.Conformance.TriggerDelivery
import Proofs.Conformance.Triggers.Materializer

namespace Conformance.TriggerDelivery

open Triggers.Durable

/-- `seeded` is the `lifecycle_state` token every prior request is stored with. -/
structure LatestAdmissionInput where
  name : String
  seeded : String
  pre : StateWire
  fire : FireWire
  destinationAllowed : Bool
  deriving Lean.ToJson

/-- One admitted request: its fire identity and its stored `lifecycle_state` token. -/
structure LatestRowWire where
  identity : IdentityWire
  lifecycle : String
  deriving Lean.ToJson

structure LatestAdmissionOutput where
  outcome : String
  receiptCount : Nat
  requestCount : Nat
  requests : List LatestRowWire
  deriving Lean.ToJson

/-- `admitLatest` ends a request only by supersession. A request it leaves open
keeps the unclaimed lifecycle it was stored with, and the request it admits
starts pending. -/
def latestLifecycle (seeded : RequestState) (pre : Triggers.Durable.State)
    (request : Triggers.Durable.Request) : RequestState :=
  if request.terminal then .superseded
  else if decide (request.fire.identity ∈ pre.requests.map (·.fire.identity)) then seeded
  else .pending

/-- A destination the admission transaction refuses rejects the fire as a
template that does not render does: the transaction rolls back before any
write. -/
def latestDispatch (pre : Triggers.Durable.State) (f : Triggers.Durable.Fire)
    (destinationAllowed : Bool) : DispatchOutcome :=
  replayDispatch pre f.identity
    (destinationAllowed && Triggers.outcomeSourceAllowed f.identity.collection f.emitOutcome)

/-- Every case runs once for each unclaimed lifecycle latest-only supersession
replaces. The fixture's owner `owner-a` stands for the node DID. -/
@[conformance_family]
def latestAdmission : Family LatestAdmissionInput LatestAdmissionOutput where
  inputs :=
    let old := { fire "old" with emitOutcome := false, identity := { identity "owner-a" "old" with collection := "Schedule" } }
    let next := { old with identity := { old.identity with document := "next" } }
    let pre := admit {} old
    [RequestState.pending, .workspaceBindingPending].flatMap fun seeded =>
      [("invalid_target_preserves_pending", pre, false),
       ("valid_target_replaces_pending", pre, true),
       ("duplicate_preserves_later_work", admit (admit pre next)
          { old with identity := { old.identity with document := "later" } }, true)].map
        fun (name, before, destinationAllowed) =>
          ({ name, seeded := seeded.toDefraDB, pre := .ofModel before, fire := .ofModel next,
             destinationAllowed } : LatestAdmissionInput)
  model := fun i =>
    let pre := i.pre.toModel
    let f := i.fire.toModel
    let seeded := (RequestState.fromDefraDB? i.seeded).getD .pending
    let post := admitLatest pre f i.destinationAllowed
    { outcome := Conformance.TriggerMaterializer.outcomeWire (latestDispatch pre f i.destinationAllowed)
      receiptCount := post.receipts.length
      requestCount := post.requests.length
      requests := post.requests.map fun request =>
        ({ identity := .ofModel request.fire.identity
           lifecycle := (latestLifecycle seeded pre request).toDefraDB } : LatestRowWire) }

end Conformance.TriggerDelivery
