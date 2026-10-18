import Proofs.ToolPolicy.AccessReview
import Proofs.Conformance.Contracts.Json.ToolPolicy

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

def casesJson : Json := toJson (replyCases ++ traceCases ++ routeCases)

end Conformance.AccessReview
