import Proofs.Identity.Configuration

namespace Identity.TaskTarget
open Household

/-- Task targets provide a native reader edge to the configured Agent's
executor. Other callers require a separate explicit native reader grant. -/
def fixture : State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "a", "user"⟩,
      ⟨"agents", "b", "user"⟩, ⟨"configuration", "task", "user"⟩,
      ⟨"configuration", "context", "user"⟩, ⟨"configuration", "tools", "user"⟩,
      ⟨"configuration", "target", "user"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"agents", "a", "executor", .actor "agent-a"⟩,
      ⟨"agents", "b", "executor", .actor "agent-b"⟩,
      ⟨"configuration", "task", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "task", "selected_by", .entitySet "agents" "a" "executor"⟩,
      ⟨"configuration", "context", "selected_by", .entitySet "agents" "a" "executor"⟩,
      ⟨"configuration", "tools", "selected_by", .entitySet "configuration" "context" "selected_by"⟩,
      ⟨"configuration", "target", "selected_by", .entitySet "configuration" "tools" "selected_by"⟩],
    enrolled := [] }

def taskEdge (agentDoc : String) : Relation :=
  ⟨"configuration", "task", "selected_by", .entitySet "agents" agentDoc "executor"⟩

/-- Rebinding removes only the old target edge and adds the new one as part of
one authorized transaction; unchanged selection does not regrant revoked access. -/
def rebindTask (state : State) (manager old next : String) (committed : Bool) : State :=
  if old == next || !committed then state
  else if mayManage state manager (taskEdge old) && mayManage state manager (taskEdge next) then
    apply (apply state (.revokeAs manager (taskEdge old))) (.grantAs manager (taskEdge next))
  else state

theorem task_access_separates_target_and_caller :
    [allowed 8 fixture "user" "configuration" "task" "read",
     allowed 8 fixture "runtime" "configuration" "task" "read",
     allowed 8 fixture "agent-a" "configuration" "task" "read",
     allowed 8 fixture "agent-b" "configuration" "task" "read"] = [true, true, true, false] := by native_decide

theorem rebinding_moves_only_selected_access :
    let state := rebindTask fixture "user" "a" "b" true
    [allowed 8 state "agent-a" "configuration" "task" "read",
     allowed 8 state "agent-b" "configuration" "task" "read"] = [false, true] := by native_decide

theorem explicit_reader_survives_rebinding :
    let shared := apply fixture (.grantAs "user" ⟨"configuration", "task", "reader", .actor "agent-a"⟩)
    allowed 8 (rebindTask shared "user" "a" "b" true) "agent-a" "configuration" "task" "read" = true := by native_decide

theorem denied_or_uncommitted_rebind_is_unchanged :
    rebindTask fixture "other" "a" "b" true = fixture ∧
    rebindTask fixture "user" "a" "b" false = fixture := by
  constructor <;> rfl

theorem replay_does_not_restore_revoked_target :
    let rebound := rebindTask fixture "user" "a" "b" true
    let revoked := apply rebound (.revoke (taskEdge "b"))
    allowed 8 (rebindTask revoked "user" "b" "b" true) "agent-b" "configuration" "task" "read" = false := by native_decide

theorem selected_target_is_not_blanket_access :
    [allowed 8 fixture "agent-a" "configuration" "target" "read",
     allowed 8 fixture "agent-b" "configuration" "target" "read"] = [true, false] := by native_decide

theorem revoking_tools_selection_removes_target_access :
    let state := apply fixture (.revoke ⟨"configuration", "target", "selected_by",
      .entitySet "configuration" "tools" "selected_by"⟩)
    allowed 8 state "agent-a" "configuration" "target" "read" = false := by native_decide
end Identity.TaskTarget
