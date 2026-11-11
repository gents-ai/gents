import Proofs.Conformance.Family
import Proofs.Tools.ToolPolicy.AccessReview
import Proofs.Conformance.Contracts.Json.ToolPolicy
import Proofs.Runtime.CanonicalOutput.Execution.Examples
import Proofs.Runtime.CanonicalOutput.Execution.Gate

namespace Conformance.AccessReview
open Lean ToolPolicy ToolPolicy.WorkspacePermissions

private def reply (action decision reason : String) : String :=
  (Json.mkObj [("actionId", toJson action), ("decision", toJson decision),
    ("reason", toJson reason)]).compress

private def replyRaws : List String :=
  let action := "action-1"
  let base := reply action "allow" ""
  [ reply action "allow" "Within the requested change.",
    reply action "deny" "Outside the requested work.",
    reply action "ask" "The intended destination is unclear.",
    reply "another-action" "allow" "Wrong invocation.",
    reply action "allow" "", reply action "unknown" "No decision.",
    reply "" "allow" "Unbound invocation.",
    "", "null", "true", "[]", "{}", "{", "\"allow\"",
    "{\"actionId\":\"action-1\",\"decision\":\"allow\"}",
    "{\"actionId\":\"action-1\",\"decision\":\"allow\",\"reason\":false}",
    "{\"actionId\":\"action-1\",\"decision\":\"allow\",\"reason\":\"ok\",\"extra\":true}",
    reply action "allow" (String.mk (List.replicate (8192 - base.utf8ByteSize) 'a')),
    reply action "allow" (String.mk (List.replicate (8193 - base.utf8ByteSize) 'a')),
    reply action "allow" (String.mk (List.replicate 4097 'λ'))]

private def reviewOf (wire : String) : Review :=
  (([.allow, .ask, .deny, .unavailable] : List Review).find? (·.wireName == wire)).getD .unavailable

private def modeOf (wire : String) : ToolPolicy.WorkspacePermissions.Mode :=
  (([.ask, .autoReview, .fullAccess] : List ToolPolicy.WorkspacePermissions.Mode).find?
    (·.wireName == wire)).getD .ask

structure ReplyInput where
  action : String
  raw : String
  deriving ToJson

@[conformance_family]
def replyDecision : Conformance.Family ReplyInput String where
  inputs := replyRaws.map fun raw => { action := "action-1", raw }
  model := fun i => (ToolPolicy.AccessReview.decodeReply i.action i.raw).wireName

private def histories : List (List Review) := [
  [], [.deny, .deny, .deny], [.deny, .deny, .allow, .deny, .deny],
  [.deny, .unavailable, .deny, .ask, .deny],
  List.replicate 10 .deny,
  (List.replicate 10 [.deny, .allow]).flatten,
  [.deny, .deny, .allow, .deny, .deny, .deny],
  List.replicate 51 .allow,
  List.replicate 10 .deny ++ List.replicate 50 .allow,
  List.replicate 2 .deny ++ [.allow] ++ List.replicate 2 .deny]

structure TraceInput where
  verdicts : List String
  deriving ToJson

structure TraceStep where
  history : List String
  blocked : Bool
  deriving ToJson

private def steps : List Review → List Review → List TraceStep
  | _, [] => []
  | history, verdict :: rest =>
      let next := ToolPolicy.AccessReview.record history verdict
      ({ history := next.map Review.wireName
         blocked := ToolPolicy.AccessReview.blocked next } : TraceStep) :: steps next rest

@[conformance_family]
def denialTrace : Conformance.Family TraceInput (List TraceStep) where
  inputs := histories.map fun history => { verdicts := history.map Review.wireName }
  model := fun i => steps [] (i.verdicts.map reviewOf)

structure WireScope where
  workspace : Nat
  revision : Nat
  deriving ToJson

structure WireGrant where
  scope : WireScope
  action : Nat
  deriving ToJson

structure WireInput where
  current : WireScope
  configuredWorkspace : Nat
  mode : String
  action : Nat
  ceilingAllows : Bool
  crossesBoundary : Bool
  grant : Option WireGrant
  deriving ToJson

structure RouteInput where
  input : WireInput
  history : List String
  verdict : String
  grant : WireGrant
  deriving ToJson

structure RouteOutput where
  evaluated : String
  applied : String
  history : List String
  deriving ToJson

private def wireScope (scope : ToolPolicy.WorkspacePermissions.Scope) : WireScope :=
  ⟨scope.workspace, scope.revision⟩

private def wireGrant (grant : ToolPolicy.WorkspacePermissions.Grant) : WireGrant :=
  ⟨wireScope grant.scope, grant.action⟩

private def wireInput (input : ToolPolicy.WorkspacePermissions.Input) : WireInput :=
  ⟨wireScope input.current, input.configuredWorkspace, input.mode.wireName, input.action,
    input.ceilingAllows, input.crossesBoundary, input.grant.map wireGrant⟩

private def scopeOf (scope : WireScope) : ToolPolicy.WorkspacePermissions.Scope :=
  ⟨scope.workspace, scope.revision⟩

private def grantOf (grant : WireGrant) : ToolPolicy.WorkspacePermissions.Grant :=
  ⟨scopeOf grant.scope, grant.action⟩

private def inputOf (input : WireInput) : ToolPolicy.WorkspacePermissions.Input :=
  ⟨scopeOf input.current, input.configuredWorkspace, modeOf input.mode, input.action,
    input.ceilingAllows, input.crossesBoundary, input.grant.map grantOf⟩

@[conformance_family]
def reviewRoute : Conformance.Family RouteInput RouteOutput where
  inputs :=
    histories.flatMap fun history =>
    let history := history.foldl ToolPolicy.AccessReview.record []
    ToolPolicy.WorkspacePermissions.cases.map fun case =>
      { input := wireInput case.input, history := history.map Review.wireName
        verdict := case.review.wireName, grant := wireGrant case.reviewedGrant }
  model := fun i =>
    let input := inputOf i.input
    let history := i.history.map reviewOf
    let verdict := reviewOf i.verdict
    let grant := grantOf i.grant
    { evaluated := (ToolPolicy.AccessReview.evaluate input history verdict grant).wireName
      applied := (ToolPolicy.AccessReview.apply input history verdict grant).1.wireName
      history := (ToolPolicy.AccessReview.apply input history verdict grant).2.map
        Review.wireName }

structure CompletionInput where
  purpose : String
  hasTools : Bool
  deriving ToJson

@[conformance_family]
def completionAcceptance : Conformance.Family CompletionInput Bool where
  inputs :=
    ([.normal, .titleAudit, .accessReview] : List RequestPurpose).flatMap fun purpose =>
    [false, true].map fun hasTools => { purpose := purpose.toWire, hasTools }
  model := fun i =>
    let purpose := (RequestPurpose.fromWire? i.purpose).getD .normal
    let world := { CanonicalOutput.Execution.Examples.world 5 with purpose }
    let permitsIntent := CanonicalOutput.Execution.Gate.purposeAllows world
      (.dispatch 7 ⟨1, true, true⟩)
    !i.hasTools || permitsIntent

structure ContextInput where
  required : Nat
  limit : Nat
  costs : List Nat
  deriving ToJson

@[conformance_family]
def contextSelection : Conformance.Family ContextInput (Option Nat) where
  inputs :=
    let maxWord := 2^64 - 1
    ([0, 32, 65500, 65536, 65537, maxWord] : List Nat).flatMap fun required =>
    ([0, 64, 65536, maxWord] : List Nat).flatMap fun limit =>
    ([[], [0], [1], [32, 32], [33, 32, 1], [65536], [65537],
      [maxWord, 1], [1, maxWord], [maxWord, 0], [65500, 32],
      [4, 4, 4, 4, 4]] : List (List Nat)).map fun costs => { required, limit, costs }
  model := fun i => ToolPolicy.AccessReview.selectContext i.required i.limit i.costs

private def taskCaseJson (policy : ToolPolicy.AccessReview.TaskPolicy)
    (budgetOnly : Bool := false) (approvedPrompt : Option String := none) : Json :=
  let selected := approvedPrompt.getD policy.prompt
  Json.mkObj [
  ("approvedPrompt", toJson selected),
  ("selectedEligible", toJson (ToolPolicy.AccessReview.selectedTaskEligible "node" "review" "agent" selected policy)),
  ("node", toJson "node"), ("taskId", toJson "review"), ("agent", toJson "agent"),
  ("document", Json.mkObj [
    ("node_did", toJson policy.node), ("task_id", toJson policy.task),
    ("agent_did", toJson policy.agent), ("prompt_template", toJson policy.prompt),
    ("enabled", toJson policy.enabled), ("emit_outcome", toJson policy.outcome),
    ("hooks", if policy.hooks then toJson [Json.mkObj [
      ("hook_id", toJson "setup"), ("phase", toJson "before"),
      ("command", toJson ["fixture"])]] else toJson ([] : List Json)),
    ("goal_objective_template", if policy.goal && !budgetOnly then toJson "goal" else Json.null),
    ("goal_token_budget", if policy.goal && budgetOnly then toJson (500 : Nat) else Json.null),
    ("output_schema_ref", if policy.schema then toJson "schema" else Json.null)]),
  ("eligible", toJson (ToolPolicy.AccessReview.taskEligible "node" "review" "agent" policy))]

def taskCasesJson : Json :=
  let base : ToolPolicy.AccessReview.TaskPolicy :=
    ⟨"node", "review", "agent", "Review workspace access.", true, false, false, false, false⟩
  let combinations := [false, true].flatMap fun sameNode =>
    [false, true].flatMap fun sameTask =>
    [false, true].flatMap fun sameAgent =>
    [false, true].flatMap fun enabled =>
    [false, true].flatMap fun hooks =>
    [false, true].flatMap fun outcome =>
    [false, true].flatMap fun goal =>
    [false, true].map fun schema => taskCaseJson
      { base with
        node := if sameNode then "node" else "other-node"
        task := if sameTask then "review" else "other-task"
        agent := if sameAgent then "agent" else "other-agent"
        enabled := enabled
        hooks := hooks
        outcome := outcome
        goal := goal
        schema := schema }
  toJson (combinations ++ [taskCaseJson { base with prompt := "" },
    taskCaseJson { base with prompt := " \n\t" },
    taskCaseJson { base with goal := true } true,
    taskCaseJson base false (some "Earlier policy."),
    taskCaseJson base false (some (base.prompt ++ " "))] ++
    TextNormalization.objectiveWhitespaceCodepoints.map (fun value =>
      taskCaseJson { base with prompt := String.singleton (Char.ofNat value) }))

end Conformance.AccessReview
