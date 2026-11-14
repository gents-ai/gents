import Proofs.Session.Request
import Proofs.Conformance.Contracts.Machines.Request
import Proofs.Conformance.Contracts.Machines.Process
import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases


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

end Conformance.ContractCases
