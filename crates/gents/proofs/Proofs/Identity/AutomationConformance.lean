import Proofs.Identity.Automation
import Lean

namespace Identity.AutomationConformance
open Lean

def callbackParentCases : List (Option String × String) :=
  [(some "selected", "selected"), (some "foreign", "selected"), (none, "selected")]

def callbackParentCasesJson : String :=
  (toJson (callbackParentCases.map fun (actor, selected) => Json.mkObj
    [("actor", toJson actor), ("selected", toJson selected),
     ("allowed", toJson (Identity.Automation.callbackParentAllowed actor selected))])).compress

end Identity.AutomationConformance
