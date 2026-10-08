import Proofs.SelfConfig.Apply

namespace SelfConfig

/-- Read-only projection of the node's Agent documents for create, edit and
disable decisions. `protectedIds` are product-owned agents (the Engineer): they
may be cloned but never edited or disabled through agent management, so a
recovery/configuration agent stays available. `defaultId` is the node's default
agent; publication rejects a disabled default, so disabling it is refused here
rather than admitted and left to fail at every reconcile. -/
structure AgentCatalog where
  agents : List (String × Bool)
  protectedIds : List String
  defaultId : Option String
  deriving DecidableEq, Repr

inductive AgentOp where
  | create
  | edit
  | disable
  deriving DecidableEq, Repr

def AgentCatalog.present (c : AgentCatalog) (id : String) : Bool :=
  c.agents.any (fun a => a.1 == id)

def AgentCatalog.mutable (c : AgentCatalog) (id : String) : Bool :=
  !c.protectedIds.contains id

/-- A new agent takes an unused id. -/
def agentCreateDecision (c : AgentCatalog) (target : String) : Bool :=
  !c.present target

def agentEditDecision (c : AgentCatalog) (target : String) : Bool :=
  c.present target && c.mutable target

/-- `makeDefault` is the same request designating the target as default. -/
def agentDisableDecision (c : AgentCatalog) (target : String)
    (makeDefault : Bool) : Bool :=
  c.present target && c.mutable target && !makeDefault
    && decide (some target ≠ c.defaultId)

def agentDecision (c : AgentCatalog) (op : AgentOp) (target : String)
    (makeDefault : Bool) : Bool :=
  match op with
  | .create => agentCreateDecision c target
  | .edit => agentEditDecision c target
  | .disable => agentDisableDecision c target makeDefault

theorem default_disable_rejected (c : AgentCatalog) (target : String)
    (makeDefault : Bool) (h : c.defaultId = some target) :
    agentDisableDecision c target makeDefault = false := by
  simp [agentDisableDecision, h]

theorem protected_edit_or_disable_rejected (c : AgentCatalog) (op : AgentOp)
    (target : String) (makeDefault : Bool) (hop : op ≠ .create)
    (hp : target ∈ c.protectedIds) :
    agentDecision c op target makeDefault = false := by
  cases op <;> simp_all [agentDecision, agentEditDecision, agentDisableDecision,
    AgentCatalog.mutable]

theorem create_requires_unused_id (c : AgentCatalog) (target : String)
    (h : c.present target = true) : agentCreateDecision c target = false := by
  simp [agentCreateDecision, h]

theorem unprotected_edit_accepted (c : AgentCatalog) (target : String)
    (hpresent : c.present target = true) (hp : target ∉ c.protectedIds) :
    agentEditDecision c target = true := by
  simp [agentEditDecision, AgentCatalog.mutable, hpresent, hp]

end SelfConfig
