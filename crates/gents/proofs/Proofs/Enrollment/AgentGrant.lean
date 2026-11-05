namespace Enrollment

/-- A signed enrollment decision may grant all agents or only the named agents.
An empty named set grants no agent; it never means unrestricted access. -/
inductive AgentGrant where
  | all
  | selected (agents : List String)
  deriving DecidableEq, Repr

def AgentGrant.allows (grant : AgentGrant) (agent : String) : Bool :=
  match grant with
  | .all => true
  | .selected agents => agents.contains agent

theorem AgentGrant.all_allows (agent : String) :
    AgentGrant.all.allows agent = true := by
  rfl

theorem AgentGrant.empty_denies (agent : String) :
    (AgentGrant.selected []).allows agent = false := by
  rfl

theorem AgentGrant.selected_allows_only_member
    (agents : List String) (agent : String)
    (h : (AgentGrant.selected agents).allows agent = true) : agent ∈ agents := by
  simpa [AgentGrant.allows] using h

end Enrollment
