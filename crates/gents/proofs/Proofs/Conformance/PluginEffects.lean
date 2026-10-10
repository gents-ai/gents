import Proofs.CanonicalOutput.Execution.Examples
import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.ToolDeliveryCases
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Lean

namespace Conformance.PluginEffects
open CanonicalOutput CanonicalOutput.Execution CanonicalOutput.Execution.Examples Lean

def baseline : World := match foregroundAcceptedAndDispatched with
  | .ok post => post | .error _ => world 5

def arguments : Segment :=
  { id := 602, coordinate := ⟨10, .toolEffectArguments 601⟩, writer := .request 7
    flush := some ⟨0, [⟨0, 2, some { block := 0, part := 0, kind := .arguments, tool := some ⟨"plugin-effect", none, "query"⟩ }⟩], [123, 125]⟩
    close := some (.closed .complete 1 [2]), createdAt := 5 }

def admission : PluginEffectAdmission :=
  { document := 601, parentToolDoc := 600, ordinal := 1, name := "query"
    context := { foregroundToolContext with callId := 601 }
    arguments := arguments, delegationGranted := true }

private def parentState (f : OwnedTool → OwnedTool) : World :=
  { baseline with toolContexts := baseline.toolContexts.map f }

private def admitted : World := match admitPluginEffect baseline 7 admission with
  | .ok post => post | .error _ => baseline

private def scenarios : List (String × World × Nat × PluginEffectAdmission) :=
  [ ("valid", baseline, 7, admission)
  , ("replay", admitted, 7, admission)
  , ("terminal_parent_replay", { admitted with toolContexts := admitted.toolContexts.map (fun p => if p.document == 600 then { p with context := { p.context with state := .completed } } else p) }, 7, admission)
  , ("no_grant_replay", admitted, 7, { admission with delegationGranted := false })
  , ("conflicting_arguments", admitted, 7,
      { admission with arguments := { arguments with
          flush := some ⟨0, [⟨0, 3, some { block := 0, part := 0, kind := .arguments, tool := some ⟨"plugin-effect", none, "query"⟩ }⟩], [123, 32, 125]⟩
          close := some (.closed .complete 1 [3]) } })
  , ("foreign_request", parentState (fun p => { p with requestDoc := 11 }), 7, admission)
  , ("foreign_session", parentState (fun p => { p with session := 2 }), 7, admission)
  , ("missing_parent", { baseline with toolContexts := [] }, 7, admission)
  , ("stale_generation", baseline, 8, admission)
  , ("terminal_parent", parentState (fun p =>
        { p with context := { p.context with state := .completed } }), 7, admission)
  , ("nested", parentState (fun p => { p with provenance := .pluginEffect 599 1 }), 7, admission)
  , ("ordinal_zero", baseline, 7, { admission with ordinal := 0 })
  , ("ordinal_last", baseline, 7, { admission with ordinal := 64 })
  , ("ordinal_exceeded", baseline, 7, { admission with ordinal := 65 })
  , ("no_grant", baseline, 7, { admission with delegationGranted := false })
  , ("extended_deadline", baseline, 7,
       { admission with context := { admission.context with deadline := 21 } }) ]

theorem admission_regressions :
    (scenarios.map fun (_, before, generation, call) =>
      succeeds (admitPluginEffect before generation call)) =
      [true, true, false, false, false, false, false, false, false, false, false,
       false, true, false, false, false] := by native_decide

private def render (item : String × World × Nat × PluginEffectAdmission) : Json :=
  let (name, before, generation, call) := item
  let parent := ownedToolByDocument? before call.parentToolDoc
  let parentJson := match parent with
    | none => Json.mkObj [("present", toJson false)]
    | some p => Json.mkObj [("present", toJson true),
        ("provenance", toJson (match p.provenance with
          | .acceptedIntent => "accepted" | .spawnedBackground _ => "spawned"
          | .pluginEffect _ _ => "plugin_effect")),
        ("state", toJson (match p.context.state with
          | .pending => "pending" | .running => "running" | .completed => "completed"
          | .failed => "failed" | .cancelled => "cancelled" | .timedOut => "timed_out")), ("request", toJson p.requestDoc),
        ("session", toJson p.session), ("deadline", toJson p.context.deadline)]
  Json.mkObj [("name", toJson name), ("generation", toJson generation),
    ("grant", toJson call.delegationGranted), ("ordinal", toJson call.ordinal),
    ("parent", parentJson), ("child", Json.mkObj [("document", toJson call.document),
      ("deadline", toJson call.context.deadline)]),
    ("arguments", (Json.parse (Conformance.Contracts.canonicalSegmentJson call.arguments)).toOption.getD Json.null),
    ("prior", if (ownedToolByDocument? before call.document).isSome then
      (Json.parse (Conformance.Contracts.canonicalSegmentJson arguments)).toOption.getD Json.null else Json.null),
    ("expected", toJson (succeeds (admitPluginEffect before generation call)))]

def output : Segment :=
  { id := 603, coordinate := ⟨10, .tool 601⟩, writer := .tool 601
    flush := some ⟨0, [⟨0, 3, some { block := 0, part := 0, kind := .toolOutput }⟩], [97, 98, 99]⟩
    close := some (.closed .complete 1 [3]), createdAt := 5 }

def terminalPayload : PayloadSpec :=
  ⟨⟨603, 0⟩, .composed [.literal [101, 114, 114, 111, 114, 58, 32], .range 1 3]⟩

private def terminalCase (replay conflict : Bool) : Bool :=
  match dispatch admitted 7 ⟨601, true, true⟩ with
  | .error _ => false
  | .ok running =>
      match Gate.evaluate (.pluginEffectClose 601 (.native .complete) output terminalPayload) running with
      | .error _ => false
      | .ok closed =>
          if replay then
            succeeds (Gate.evaluate (.pluginEffectClose 601 (.native .complete) output
              (if conflict then ⟨⟨603, 0⟩, .full⟩ else terminalPayload)) closed)
          else (ownedToolByDocument? closed 601).any (fun tool => tool.terminalOutput == some terminalPayload)

theorem terminal_presentation_regressions :
    [terminalCase false false, terminalCase true false, terminalCase true true] =
      [true, true, false] := by native_decide

def terminalCasesJson : Json := toJson <| [(false, false), (true, false), (true, true)].map
  fun (replay, conflict) => Json.mkObj [("replay", toJson replay), ("conflict", toJson conflict),
    ("expected", toJson (terminalCase replay conflict))]

private def running : World := match dispatch admitted 7 ⟨601, true, true⟩ with
  | .ok post => post | .error _ => admitted

theorem pending_effect_cancellation_has_no_execution_output :
    ((ownedToolByDocument? (accountOwnedTools admitted 7 true) 601).any fun child =>
      child.context.state == .cancelled && child.context.startedAt.isNone &&
      child.terminalOutput.isNone) = true := by native_decide

theorem exceptional_effect_is_handed_to_recovery :
    ((ownedToolByDocument? (accountOwnedTools running 7 true) 601).any fun child =>
      child.context.state == .running && child.stuckSince.isSome) = true := by native_decide

private def runningWithCompletedParent : World :=
  match ToolDelivery.completeAndDeliver running 600 (.native .complete)
      ToolDelivery.Cases.toolOutputClose (ToolDelivery.Cases.foregroundResultMessage 1) with
  | .ok post => post
  | .error _ => running

theorem running_effect_alone_blocks_normal_completion :
    ((ownedToolByDocument? runningWithCompletedParent 600).any fun parent =>
      parent.context.state == .completed) = true ∧
      normalCompletionToolsReady runningWithCompletedParent 7 = false := by native_decide

def accountingCasesJson : Json := toJson <|
  [("pending_cancel", admitted, true), ("running_normal", runningWithCompletedParent, false),
   ("running_exceptional", running, true)].map fun (name, before, exceptional) =>
    let post := if name == "running_normal" then before else accountOwnedTools before 7 exceptional
    let child := (ownedToolByDocument? post 601).getD
      { document := 601, requestDoc := 10, session := 1, acceptedSequence := 0,
        context := admission.context }
    Json.mkObj [("name", toJson name),
      ("expected_state", toJson (match child.context.state with
        | .pending => "pending" | .running => "running" | .completed => "completed"
        | .failed => "failed" | .cancelled => "cancelled" | .timedOut => "timed_out")),
      ("expected_started", toJson child.context.startedAt.isSome),
      ("expected_stuck", toJson child.stuckSince.isSome),
      ("expected_terminal_output", toJson child.terminalOutput.isSome),
      ("expected_normal_ready", toJson (normalCompletionToolsReady post 7))]

def casesJson : Json := toJson (scenarios.map render)
end Conformance.PluginEffects
