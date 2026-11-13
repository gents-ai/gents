import Proofs.Session.Request
import Proofs.Conformance.Contracts.Machines.Request
import Proofs.Conformance.Contracts.Machines.Process
import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases

open Conformance.Contracts (requestStates requestActions requestSamples)

inductive LifecycleTransitionClassification where
  | legal
  | illegal
  | recoveryReachable
  deriving DecidableEq, Repr

namespace LifecycleTransitionClassification

def toContract : LifecycleTransitionClassification → String
  | .legal => "legal"
  | .illegal => "illegal"
  | .recoveryReachable => "recoveryReachable"

end LifecycleTransitionClassification

def lifecycleTransitionCaseName (domain source target : String) : String :=
  domain ++ ":" ++ source ++ "->" ++ target

def actionForPairFromSamples {σ α : Type}
    (samples : List σ)
    (actions : List (String × α))
    (step : σ → α → Option σ)
    (stateName : σ → String)
    (source target : String) : Option String :=
  let candidates :=
    samples.flatMap fun pre =>
      actions.filterMap fun action =>
        match step pre action.snd with
        | some post =>
            if stateName pre = source ∧ stateName post = target then
              some action.fst
            else
              none
        | none => none
  candidates.head?

def requestTransitionAction? (source target : String) : Option String :=
  actionForPairFromSamples
    requestSamples
    requestActions
    RequestContext.step?
    (fun ctx => ctx.state.toDefraDB)
    source
    target

def requestRecoverySweepReachable : RequestState → RequestState → Bool
  | .claimed, .dead => true
  | .processing, .dead => true
  | _, _ => false

def requestTransitionClassification
    (source target : RequestState)
    (action : Option String) : LifecycleTransitionClassification :=
  match action with
  | some _ => .legal
  | none =>
      if requestRecoverySweepReachable source target then
        .recoveryReachable
      else
        .illegal

def requestTransitionCase (source target : RequestState) : LifecycleTransitionCase :=
  let sourceName := source.toDefraDB
  let targetName := target.toDefraDB
  let action := requestTransitionAction? sourceName targetName
  let classification := requestTransitionClassification source target action
  { stoppedBeforeRunning := Request.Projection.stoppedBeforeRunning true source target
  , name := lifecycleTransitionCaseName "Request" sourceName targetName
  , domain := "Request"
  , fromState := sourceName
  , toState := targetName
  , classification := classification.toContract
  , action := action
  , boundary :=
      match classification with
      | .recoveryReachable =>
          some "boundary.request.recovery-sweep-reachable"
      | _ => none
  }

def requestTransitionCases : List LifecycleTransitionCase :=
  requestStates.flatMap fun source =>
    requestStates.map fun target =>
      requestTransitionCase source target

end Conformance.ContractCases
