import Lean

namespace Identity.AgentBlockSigning

/-- Managed Agent signing authority requires the held runtime root, a validated
Agent binding, and the same originating node and actor at the write owner.
DefraDB separately authorizes the actor. A registered DID alone does not carry
root or Agent signing authority. -/
inductive Route where
  | native | agent | refuse
  deriving BEq, DecidableEq, Repr

def route (managed valid rootHeld : Bool) (node actor : String)
    (signer bindingNode : Option String) : Route :=
  if !managed then
    if actor.isEmpty || (!node.isEmpty && actor == node) then .native else .refuse
  else if rootHeld && valid && !node.isEmpty && bindingNode == some node &&
      !actor.isEmpty && signer == some actor then .agent
  else .refuse

theorem node_keeps_native (valid held : Bool) (node : String)
    (signer bindingNode : Option String) :
    route false valid held node node signer bindingNode = .native := by
  simp [route]

theorem invalid_binding_never_signs (held : Bool) (node actor : String)
    (signer bindingNode : Option String) :
    route true false held node actor signer bindingNode = .refuse := by
  simp [route]

theorem missing_root_never_signs (valid : Bool) (node actor : String)
    (signer bindingNode : Option String) :
    route true valid false node actor signer bindingNode = .refuse := by
  simp [route]

theorem managed_never_falls_back (valid held : Bool) (node actor : String)
    (signer bindingNode : Option String) :
    route true valid held node actor signer bindingNode ≠ .native := by
  simp [route]
  split <;> decide

theorem matched_agent_signs (node actor : String)
    (nonemptyNode : node.isEmpty = false) (nonemptyActor : actor.isEmpty = false) :
    route true true true node actor (some actor) (some node) = .agent := by
  simp [route, nonemptyNode, nonemptyActor]

structure Case where
  name : String
  managed : Bool
  valid : Bool
  rootHeld : Bool
  node : String
  actor : String
  signer : Option String
  bindingNode : Option String

def cases : List Case :=
  [ ⟨"managed-agent", true, true, true, "node", "agent-a", some "agent-a", some "node"⟩
  , ⟨"missing-key", true, true, true, "node", "agent-a", none, some "node"⟩
  , ⟨"sibling-key", true, true, true, "node", "agent-a", some "agent-b", some "node"⟩
  , ⟨"invalid-binding", true, false, true, "node", "agent-a", some "agent-a", some "node"⟩
  , ⟨"spoofed-root", true, true, false, "node", "agent-a", some "agent-a", some "node"⟩
  , ⟨"foreign-runtime", true, true, true, "foreign-node", "agent-a", some "agent-a", some "node"⟩
  , ⟨"empty-actor", true, true, true, "node", "", some "agent-a", some "node"⟩
  , ⟨"ordinary-node", false, false, false, "node", "node", none, none⟩
  , ⟨"ordinary-default-node", false, false, false, "node", "", none, none⟩
  , ⟨"unconfigured-explicit-actor", false, false, false, "", "agent-a", none, none⟩
  , ⟨"ordinary-user-key-is-not-adopted", false, true, false, "node", "user", some "user", none⟩ ]

def routeString : Route → String
  | .native => "native" | .agent => "agent" | .refuse => "refuse"

def casesJson : String :=
  (Lean.toJson (cases.map fun c => Lean.Json.mkObj [
    ("name", Lean.toJson c.name), ("managed", Lean.toJson c.managed),
    ("valid", Lean.toJson c.valid), ("root_held", Lean.toJson c.rootHeld),
    ("node", Lean.toJson c.node), ("actor", Lean.toJson c.actor),
    ("signer", Lean.toJson c.signer), ("binding_node", Lean.toJson c.bindingNode),
    ("expected", Lean.toJson (routeString
      (route c.managed c.valid c.rootHeld c.node c.actor c.signer c.bindingNode)))])).compress

end Identity.AgentBlockSigning
