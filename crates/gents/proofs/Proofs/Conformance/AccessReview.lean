import Proofs.ToolPolicy.AccessReview
import Proofs.Conformance.Contracts.Json.ToolPolicy
import Proofs.CanonicalOutput.Execution.Examples
import Proofs.CanonicalOutput.Execution.Gate

namespace Conformance.AccessReview
open Lean ToolPolicy ToolPolicy.WorkspacePermissions

private def reply (action decision reason : String) : String :=
  (Json.mkObj [("actionId", toJson action), ("decision", toJson decision),
    ("reason", toJson reason)]).compress

private def replyCases : List Json :=
  let action := "action-1"
  let base := reply action "allow" ""
  let rawValues := [
    reply action "allow" "Within the requested change.",
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
  rawValues.map fun raw => Json.mkObj [
    ("kind", toJson "reply"), ("action", toJson action), ("raw", toJson raw),
    ("expected", toJson (ToolPolicy.AccessReview.decodeReply action raw).wireName)]

private def histories : List (List Review) := [
  [], [.deny, .deny, .deny], [.deny, .deny, .allow, .deny, .deny],
  [.deny, .unavailable, .deny, .ask, .deny],
  List.replicate 10 .deny,
  (List.replicate 10 [.deny, .allow]).flatten,
  [.deny, .deny, .allow, .deny, .deny, .deny],
  List.replicate 51 .allow,
  List.replicate 10 .deny ++ List.replicate 50 .allow,
  List.replicate 2 .deny ++ [.allow] ++ List.replicate 2 .deny]

private def steps : List Review → List Review → List Json
  | _, [] => []
  | history, verdict :: rest =>
      let next := ToolPolicy.AccessReview.record history verdict
      Json.mkObj [
        ("verdict", toJson verdict.wireName),
        ("history", toJson (next.map Review.wireName)),
        ("blocked", toJson (ToolPolicy.AccessReview.blocked next))] :: steps next rest

private def traceCases : List Json := histories.map fun history => Json.mkObj [
  ("kind", toJson "trace"), ("steps", toJson (steps [] history))]

private def routeCases : List Json :=
  histories.flatMap fun history =>
  let history := history.foldl ToolPolicy.AccessReview.record []
  ToolPolicy.WorkspacePermissions.cases.map fun case => Json.mkObj [
    ("kind", toJson "route"),
    ("input", Contracts.permissionInputJson case.input),
    ("history", toJson (history.map Review.wireName)),
    ("verdict", toJson case.review.wireName),
    ("grant", Contracts.permissionGrantJson case.reviewedGrant),
    ("expected", toJson (ToolPolicy.AccessReview.evaluate
      case.input history case.review case.reviewedGrant).wireName),
    ("expectedHistory", toJson ((ToolPolicy.AccessReview.apply
      case.input history case.review case.reviewedGrant).2.map Review.wireName))]

private def completionCases : List Json :=
  ([.normal, .titleAudit, .accessReview] : List RequestPurpose).flatMap fun purpose =>
  [false, true].map fun hasTools =>
    let world := { CanonicalOutput.Execution.Examples.world 5 with purpose }
    let permitsIntent := CanonicalOutput.Execution.Gate.purposeAllows world
      (.dispatch 7 ⟨1, true, true⟩)
    Json.mkObj [
      ("kind", toJson "completion"), ("purpose", toJson purpose.toWire),
      ("hasTools", toJson hasTools), ("expected", toJson (!hasTools || permitsIntent))]

def casesJson : Json := toJson (replyCases ++ traceCases ++ routeCases ++ completionCases)

private def taskCaseJson (policy : ToolPolicy.AccessReview.TaskPolicy)
    (budgetOnly : Bool := false) (approvedPrompt : Option String := none) : Json :=
  let selected := approvedPrompt.getD policy.prompt
  Json.mkObj [
  ("approvedPrompt", toJson selected),
  ("selectedEligible", toJson (ToolPolicy.AccessReview.selectedTaskEligible "node" "review" "agent" selected policy)),
  ("node", toJson "node"), ("taskId", toJson "review"), ("agent", toJson "agent"),
  ("document", Json.mkObj [
    ("node_did", toJson policy.node), ("task_id", toJson policy.task),
    ("agent_id", toJson policy.agent), ("prompt_template", toJson policy.prompt),
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
    GoalAutomation.objectiveWhitespaceCodepoints.map (fun value =>
      taskCaseJson { base with prompt := String.singleton (Char.ofNat value) }))

end Conformance.AccessReview
