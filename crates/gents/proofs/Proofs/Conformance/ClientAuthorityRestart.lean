import Lean
import Proofs.ClientShell.AuthorityRestart

namespace Conformance.ClientAuthorityRestart
open Lean ClientShell.AuthorityRestart

def refusalJson : Refusal → Json
  | .nodeUnobserved => "nodeUnobserved"
  | .noAgent => "noAgent"

def actionJson : NextAction → Json
  | .keepCurrentFolder => "keepCurrentFolder"
  | .retryWhenLoaded => "retryWhenLoaded"
  | .addAgent => "addAgent"

def planJson : Plan → Json
  | .restart => Json.mkObj [("kind", "restart")]
  | .publishThenRestart => Json.mkObj [("kind", "publishThenRestart")]
  | .refused cause => Json.mkObj [("kind", "refused"), ("cause", refusalJson cause),
      ("nextActions", Json.arr ((nextActions cause).map actionJson).toArray)]

def caseJson (o : Observation) : Json :=
  Json.mkObj [("rootChanged", toJson o.rootChanged), ("nodeObserved", toJson o.nodeObserved),
    ("agentPresent", toJson o.agentPresent), ("plan", planJson (plan o))]

def casesJson : String := (Json.arr (allObservations.map caseJson).toArray).compress

end Conformance.ClientAuthorityRestart

/-- Every access-change observation and its plan, executed from the Lean owner
for the desktop's `authorityRestartPlan`. -/
def main : IO Unit := IO.println Conformance.ClientAuthorityRestart.casesJson
