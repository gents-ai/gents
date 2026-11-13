import Proofs.Store.Identity.AgentBlockSigning
import Proofs.Conformance.Family

namespace Conformance.AgentBlockSigning

open Lean Identity.AgentBlockSigning

deriving instance ToJson for Route

structure SigningInput where
  name : String
  managed : Bool
  valid : Bool
  rootHeld : Bool
  node : String
  actor : String
  signer : Option String
  bindingNode : Option String
  deriving ToJson

def SigningInput.ofCase (c : Case) : SigningInput :=
  { name := c.name, managed := c.managed, valid := c.valid, rootHeld := c.rootHeld
    node := c.node, actor := c.actor, signer := c.signer, bindingNode := c.bindingNode }

@[conformance_family]
def managedRoute : Family SigningInput Route where
  inputs := cases.map SigningInput.ofCase
  model := fun i => route i.managed i.valid i.rootHeld i.node i.actor i.signer i.bindingNode

@[conformance_family]
def configTool : Family SigningInput Route where
  inputs := cases.map SigningInput.ofCase
  model := fun i => configToolRoute i.valid i.rootHeld i.node i.actor i.signer i.bindingNode

end Conformance.AgentBlockSigning
