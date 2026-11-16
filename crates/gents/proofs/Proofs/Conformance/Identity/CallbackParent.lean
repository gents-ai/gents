import Proofs.Store.Identity.Automation
import Proofs.Conformance.Family

namespace Conformance.CallbackParent

open Lean

structure ActorInput where
  actor : Option String
  selected : String
  deriving ToJson

@[conformance_family]
def actor : Family ActorInput Bool where
  inputs := [(some "selected", "selected"), (some "foreign", "selected"), (none, "selected")].map
    fun (actor, selected) => { actor, selected }
  model := fun i => Identity.Automation.callbackParentAllowed i.actor i.selected

end Conformance.CallbackParent
