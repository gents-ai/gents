import Proofs.Triggers.Durable
import Proofs.Triggers.Reachability

namespace Triggers.Durable

def dispatchProjection (request : Request) : AgentRequest := {
  id := request.fire.identity.requestId
  causedBy := some request.fire.identity.trigger
  concurrency := if request.fire.serial then .queuedSerial else .parallel
  isTerminal := request.terminal
  executionOrigin := .scheduled }

def dispatchShape (request : AgentRequest) : Option String × ConcurrencyMode × Bool :=
  (request.causedBy, request.concurrency, request.isTerminal)

theorem fresh_queued_admission_refines_dispatch (state : State) (fire : Fire)
    (snapshot : TriggerSnapshot) (intent : FireIntent) (seed : RequestSeed)
    (hv : Triggers.outcomeSourceAllowed fire.identity.collection fire.emitOutcome = true)
    (hf : admitted state fire.identity = false) (hs : fire.serial = true)
    (hd : dispatch snapshot intent = some seed)
    (hc : intent.concurrency = .queuedSerial)
    (ht : seed.causedByTriggerId = some fire.identity.trigger) :
    ((admit state fire).requests.map dispatchProjection).map dispatchShape =
      (dispatchStep { requests := state.requests.map dispatchProjection } snapshot intent).requests.map dispatchShape := by
  simp [admit, hf, hv, dispatchStep, hd, hc, ht, List.map_append,
    dispatchProjection, dispatchShape, hs]

end Triggers.Durable
