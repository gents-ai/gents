import Proofs.Session.AgentSession
import Proofs.Conformance.Family
import Proofs.Conformance.Functions.Session

namespace Conformance.SessionStaleCompletion

open Lean AgentSession

structure StaleInput where
  staleState : RequestState
  newerState : RequestState
  deriving ToJson

structure StaleOutput where
  latestRequestId : Option Nat
  latestState : Option String
  deriving ToJson

@[conformance_family]
def observation : Family StaleInput StaleOutput where
  inputs := [RequestState.completed, RequestState.failed].flatMap fun stale =>
    [RequestState.pending, RequestState.processing].map fun newer =>
      ({ staleState := stale, newerState := newer } : StaleInput)
  model := fun i =>
    let observed := staleCompletionLatest i.newerState i.staleState
    { latestRequestId := observed.map (·.requestId)
      latestState := observed.map (fun o => o.state.toDefraDB) }

end Conformance.SessionStaleCompletion
