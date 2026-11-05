import Proofs.Identity.Household

namespace Identity.Automation

/-- A fresh Task request and its physical Agent and session relationships
commit together. The caller must verify both targets and authenticate the
selected Agent before entering the native transaction. A failed transaction
leaves neither document nor grants. Duplicate deliveries are observed rather
than regranted, preserving later revocations. -/
def commitTaskRequest (state : Household.State)
    (requestDoc actor selectedAgent agentDoc sessionDoc : String)
    (targetsVerified fresh committed : Bool) : Household.State :=
  if actor == selectedAgent && targetsVerified && fresh && committed then
    { state with
      objects := ⟨"delivered_requests", requestDoc, actor⟩ :: state.objects
      relations :=
        ⟨"delivered_requests", requestDoc, "parent", .object "agents" agentDoc⟩ ::
        ⟨"delivered_requests", requestDoc, "session", .object "sessions" sessionDoc⟩ ::
        state.relations }
  else state

theorem task_request_aborted (state : Household.State) (request actor selected agent session : String)
    (verified fresh : Bool) :
    commitTaskRequest state request actor selected agent session verified fresh false = state := by
  simp [commitTaskRequest]

theorem task_request_duplicate_preserves_revocation (state : Household.State)
    (request actor selected agent session : String) (verified committed : Bool) :
    commitTaskRequest state request actor selected agent session verified false committed = state := by
  simp [commitTaskRequest]

theorem task_request_rejects_foreign_creator (state : Household.State)
    (request actor selected agent session : String) (verified fresh committed : Bool)
    (different : actor ≠ selected) :
    commitTaskRequest state request actor selected agent session verified fresh committed = state := by
  simp [commitTaskRequest, different]

theorem task_request_commits_exact_parents (state : Household.State)
    (request actor agent session : String) :
    (commitTaskRequest state request actor actor agent session true true true).relations =
      ⟨"delivered_requests", request, "parent", .object "agents" agent⟩ ::
      ⟨"delivered_requests", request, "session", .object "sessions" session⟩ ::
      state.relations := by simp [commitTaskRequest]

/-- Trigger combines configuration and runtime observations in one document.
ACP1 update authorizes the document, rather than individual fields. -/
structure Trigger where
  task : String
  enabled : Bool
  nextRun : Nat
  deriving DecidableEq, Repr

def updateTrigger (current replacement : Trigger) (nativeUpdate : Bool) : Trigger :=
  if nativeUpdate then replacement else current

theorem update_for_observation_also_permits_routing_change :
    updateTrigger ⟨"task-a", true, 0⟩ ⟨"task-b", false, 1⟩ true =
      ⟨"task-b", false, 1⟩ := by rfl

theorem denied_update_preserves_all_fields (current replacement : Trigger) :
    updateTrigger current replacement false = current := by rfl

/-- ACP1 commits a configuration document before detached reader grants.
Seeding an event cursor in the document transaction does not imply that an
agent has permission to read the configuration after that transaction. -/
structure Publication where
  committed : Bool
  cursorSeeded : Bool
  agentReader : Bool
  deriving DecidableEq, Repr

def publishConfig (eventSource : Bool) : Publication :=
  ⟨true, eventSource, false⟩

def grantReader (state : Publication) (succeeded : Bool) : Publication :=
  { state with agentReader := state.agentReader || succeeded }

theorem seeded_cursor_does_not_supply_agent_grant :
    publishConfig true = ⟨true, true, false⟩ := by rfl

theorem failed_grant_leaves_committed_config_unreadable :
    grantReader (publishConfig true) false = ⟨true, true, false⟩ := by rfl

/-- Replacing the selected agent in a configuration document does not itself
remove a previously granted native relationship or grant the new agent access. -/
structure Assignment where
  selected : String
  granted : String → Bool

def reassign (state : Assignment) (agent : String) : Assignment :=
  { state with selected := agent }

theorem reassignment_does_not_revoke (state : Assignment) (oldAgent newAgent : String) :
    (reassign state newAgent).granted oldAgent = state.granted oldAgent := by rfl

theorem reassignment_does_not_grant (state : Assignment) (newAgent : String) :
    (reassign state newAgent).granted newAgent = state.granted newAgent := by rfl

/-- A prospective activation fence is an application protocol, not a native
ACP1 property. It must check the selected actor, the current binding and every
required standing grant before admitting unattended work. Its checks alone do
not make detached grants atomic with activation or revoke stale grants. -/
def mayActivate (actorIsSelected bindingCurrent grantsComplete : Bool) : Bool :=
  actorIsSelected && bindingCurrent && grantsComplete

theorem missing_standing_grant_blocks_fenced_activation (selected current : Bool) :
    mayActivate selected current false = false := by
  cases selected <;> cases current <;> rfl

theorem stale_binding_blocks_fenced_activation (selected granted : Bool) :
    mayActivate selected false granted = false := by
  cases selected <;> rfl

theorem foreign_actor_blocks_fenced_activation (current granted : Bool) :
    mayActivate false current granted = false := by rfl

/-- A callback child belongs to its authenticated agent. The publisher must
bind the physical Agent parent to that same actor even if the actor can read
other Agent documents. ACP1 does not constrain an arbitrary authored parent. -/
def callbackParentAllowed (actor : Option String) (selected : String) : Bool :=
  actor == some selected

theorem callback_parent_requires_actor (selected : String) :
    callbackParentAllowed none selected = false := by rfl

theorem callback_parent_accepts_selected_actor (selected : String) :
    callbackParentAllowed (some selected) selected = true := by simp [callbackParentAllowed]

theorem callback_parent_rejects_foreign_actor (actor selected : String)
    (different : actor ≠ selected) :
    callbackParentAllowed (some actor) selected = false := by
  simp [callbackParentAllowed, different]

end Identity.Automation
