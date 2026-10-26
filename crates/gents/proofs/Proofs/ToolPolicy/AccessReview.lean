import Lean
import Proofs.ToolPolicy.WorkspacePermissions
import Proofs.GoalAutomation

namespace ToolPolicy.AccessReview
open Lean WorkspacePermissions

structure TaskPolicy where
  node : String
  task : String
  agent : String
  prompt : String
  enabled : Bool
  hooks : Bool
  outcome : Bool
  goal : Bool
  schema : Bool

/-- A review borrows a Task's policy text, not its execution side effects.
Tasks requiring hooks, goals, outcomes, or a different output contract cannot
silently lose those requirements when selected as a reviewer. -/
def taskEligible (node task agent : String) (policy : TaskPolicy) : Bool :=
  policy.node == node && policy.task == task && policy.agent == agent &&
    !(GoalAutomation.canonicalObjective policy.prompt).isEmpty && policy.enabled &&
    !policy.hooks && !policy.outcome && !policy.goal && !policy.schema

theorem task_hooks_cannot_run_as_a_review (node task agent : String) (policy : TaskPolicy)
    (hooks : policy.hooks = true) : taskEligible node task agent policy = false := by
  simp [taskEligible, hooks]

theorem disabled_task_cannot_review (node task agent : String) (policy : TaskPolicy)
    (disabled : policy.enabled = false) : taskEligible node task agent policy = false := by
  simp [taskEligible, disabled]

theorem another_agent_cannot_supply_review_policy (node task agent : String)
    (policy : TaskPolicy) (different : policy.agent ≠ agent) :
    taskEligible node task agent policy = false := by
  simp [taskEligible, different]

/-- Selection approves the exact policy template, not every future revision
of a mutable Task. Native adapters identify its bytes with SHA-256; collision
resistance is an external premise. Display metadata does not select policy,
and eligibility independently rejects changes to execution requirements. -/
def selectedTaskEligible (node task agent selectedPrompt : String) (policy : TaskPolicy) : Bool :=
  taskEligible node task agent policy && policy.prompt == selectedPrompt

theorem changed_policy_requires_selection (node task agent selectedPrompt : String)
    (policy : TaskPolicy) (changed : policy.prompt ≠ selectedPrompt) :
    selectedTaskEligible node task agent selectedPrompt policy = false := by
  simp [selectedTaskEligible, changed]

theorem selected_policy_preserves_eligibility (node task agent : String) (policy : TaskPolicy) :
    selectedTaskEligible node task agent policy.prompt policy = taskEligible node task agent policy := by
  simp [selectedTaskEligible]

/-- The native encoder supplies complete incremental wire costs, including
JSON escaping and separators. Required bytes retain the full policy, action
and current request, with an upper bound for omission metadata. Entries are
newest first; only whole entries from that prefix may be supplied. Correct
wire costs are a transport premise, independently checked by native adapters. -/
def contextCount (remaining : Nat) : List Nat → Nat
  | [] => 0
  | cost :: rest =>
      if cost ≤ remaining then 1 + contextCount (remaining - cost) rest else 0

def selectContext (required limit : Nat) (costs : List Nat) : Option Nat :=
  if required ≤ limit then some (contextCount (limit - required) costs) else none

theorem context_count_is_bounded (remaining : Nat) (costs : List Nat) :
    contextCount remaining costs ≤ costs.length := by
  induction costs generalizing remaining with
  | nil => simp [contextCount]
  | cons cost rest ih =>
      simp only [contextCount, List.length_cons]
      split
      · have := ih (remaining - cost)
        omega
      · omega

theorem context_cost_is_bounded (remaining : Nat) (costs : List Nat) :
    (costs.take (contextCount remaining costs)).sum ≤ remaining := by
  induction costs generalizing remaining with
  | nil => simp [contextCount]
  | cons cost rest ih =>
      simp only [contextCount]
      split
      · simp only [Nat.add_comm 1, List.take_succ_cons, List.sum_cons]
        have := ih (remaining - cost)
        omega
      · simp

theorem required_overflow_needs_a_person (required limit : Nat) (costs : List Nat)
    (large : limit < required) : selectContext required limit costs = none := by
  simp [selectContext, Nat.not_le_of_gt large]

theorem selected_context_fits (required limit : Nat) (costs : List Nat) (count : Nat)
    (selected : selectContext required limit costs = some count) :
    required + (costs.take count).sum ≤ limit := by
  by_cases fits : required ≤ limit
  · simp only [selectContext, if_pos fits, Option.some.injEq] at selected
    subst count
    have := context_cost_is_bounded (limit - required) costs
    omega
  · simp [selectContext, fits] at selected

theorem an_oversized_recent_entry_stops_older_context (remaining cost : Nat) (rest : List Nat)
    (large : remaining < cost) : contextCount remaining (cost :: rest) = 0 := by
  simp [contextCount, Nat.not_le_of_gt large]

/-- The reply names the invocation supplied by the native caller. A model
cannot select an action, widen its scope, or authorize a later invocation.
Malformed, oversized, or incomplete replies require a person. JSON transport
adapters additionally reject duplicate object fields before supplying a value. -/
def decodeReply (action : String) (raw : String) : Review := Id.run do
  if raw.utf8ByteSize > 8192 then return .unavailable
  let .ok value := Json.parse raw | return .unavailable
  let .obj fields := value | return .unavailable
  if fields.size != 3 then return .unavailable
  let .str bound := value.getObjValD "actionId" | return .unavailable
  if bound != action || action.isEmpty then return .unavailable
  let .str reason := value.getObjValD "reason" | return .unavailable
  if reason.isEmpty then return .unavailable
  match value.getObjValD "decision" with
  | .str "allow" => return .allow
  | .str "ask" => return .ask
  | .str "deny" => return .deny
  | _ => return .unavailable

/-- Only completed allow/deny verdicts affect the circuit breaker. An
unavailable reviewer is not a finding that an action is unsafe. History is
newest first and belongs to one physical caller request, not its workspace. -/
def record (history : List Review) (verdict : Review) : List Review :=
  match verdict with
  | .allow | .deny => (verdict :: history).take 50
  | .ask | .unavailable => history

/-- Repeated denial returns routing to a person instead of asking the reviewer
to reconsider equivalent actions indefinitely. These limits are product
policy: three consecutive denials or ten among the last fifty verdicts. -/
def blocked (history : List Review) : Bool :=
  decide ((history.takeWhile (· == .deny)).length ≥ 3 ||
    (history.filter (· == .deny)).length ≥ 10)

def evaluate (input : Input) (history : List Review) (verdict : Review)
    (grant : Grant) : Route :=
  if route input = .review && blocked history then .ask
  else reviewedRoute input verdict grant

/-- A result affects the caller's denial history only when that caller was
eligible for automatic review and the delivered result names its exact action
and revision. A stale result cannot reset or exhaust a newer caller's budget. -/
def apply (input : Input) (history : List Review) (verdict : Review)
    (grant : Grant) : Route × List Review :=
  let result := evaluate input history verdict grant
  if route input = .review && !blocked history &&
      grant.scope == input.current && grant.action == input.action then
    (result, record history verdict)
  else (result, history)

theorem another_scope_cannot_change_review_history (input : Input) (history : List Review)
    (verdict : Review) (grant : Grant) (different : grant.scope ≠ input.current) :
    (apply input history verdict grant).2 = history := by
  simp [apply, different]

theorem another_action_cannot_change_review_history (input : Input) (history : List Review)
    (verdict : Review) (grant : Grant) (different : grant.action ≠ input.action) :
    (apply input history verdict grant).2 = history := by
  simp [apply, different]

theorem blocked_review_requires_a_person (input : Input) (history : List Review)
    (verdict : Review) (grant : Grant) (review : route input = .review)
    (stop : blocked history = true) : evaluate input history verdict grant = .ask := by
  simp [evaluate, review, stop]

theorem routine_actions_ignore_review_history (input : Input) (history : List Review)
    (verdict : Review) (grant : Grant) (ceiling : input.ceilingAllows = true)
    (mode : effectiveMode input ≠ .ask) (routine : input.crossesBoundary = false) :
    evaluate input history verdict grant = .allow := by
  simp [evaluate, reviewedRoute, route, ceiling, mode, routine]

theorem review_history_cannot_widen_the_ceiling (input : Input) (history : List Review)
    (verdict : Review) (grant : Grant) (ceiling : input.ceilingAllows = false) :
    evaluate input history verdict grant = .deny := by
  simp [evaluate, reviewedRoute, route, ceiling]

theorem unavailable_is_not_a_denial (history : List Review) :
    record history .unavailable = history := by rfl

theorem recording_a_verdict_is_bounded (history : List Review) (verdict : Review)
    (bounded : history.length ≤ 50) : (record history verdict).length ≤ 50 := by
  cases verdict <;> simp [record, List.length_take, Nat.min_le_left, bounded]

end ToolPolicy.AccessReview
