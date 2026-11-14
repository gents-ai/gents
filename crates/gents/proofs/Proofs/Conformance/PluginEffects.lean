import Proofs.Runtime.CanonicalOutput.Execution.Examples
import Proofs.Runtime.CanonicalOutput.Execution.Gate
import Proofs.Runtime.CanonicalOutput.Execution.ToolDeliveryCases
import Proofs.Conformance.Contracts.Json.ClientRuntime
import Proofs.Conformance.Family
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
    arguments := arguments }

private def parentState (f : OwnedTool → OwnedTool) : World :=
  { baseline with toolContexts := baseline.toolContexts.map f }

private def admitted : World := match admitPluginEffect baseline 7 admission with
  | .ok post => post | .error _ => baseline

private def backgroundParent : World := match changeToolControl baseline 7 600 .background with
  | .ok post => post | .error _ => baseline

private def scenarios : List (String × World × Nat × PluginEffectAdmission) :=
  [ ("valid", baseline, 7, admission)
  , ("replay", admitted, 7, admission)
  , ("terminal_parent_replay", { admitted with toolContexts := admitted.toolContexts.map (fun p => if p.document == 600 then { p with context := { p.context with state := .completed } } else p) }, 7, admission)
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
  , ("extended_deadline", baseline, 7,
       { admission with context := { admission.context with deadline := 21 } })
  , ("background_parent", backgroundParent, 7, admission)
  , ("background_child", baseline, 7,
       { admission with context := { admission.context with awaitMode := .background } }) ]

theorem admission_regressions :
    (scenarios.map fun (_, before, generation, call) =>
      succeeds (admitPluginEffect before generation call)) =
      [true, true, false, false, false, false, false, false, false, false, false,
       true, false, false, true, true] := by native_decide

structure ToolWire where
  id : String
  call_id : Option String
  name : String
  deriving ToJson, FromJson, Inhabited

structure DeclarationWire where
  block : Nat
  part : Nat
  kind : String
  tool : Option ToolWire
  media_kind : Option String
  deriving ToJson, FromJson, Inhabited

structure RunWire where
  stream : Nat
  bytes : Nat
  declaration : Option DeclarationWire
  deriving ToJson, FromJson, Inhabited

structure FlushWire where
  ordinal : Nat
  runs : List RunWire
  payload : List Nat
  deriving ToJson, FromJson, Inhabited

/-- A segment source or writer as its tag and the coordinates that tag carries. -/
structure TagWire where
  kind : String
  call : Option Nat
  generation : Option Nat
  key : Option Nat
  scope : Option Nat
  turn : Option Nat
  attempt : Option Nat
  auxiliary_kind : Option String
  deriving ToJson, FromJson, Inhabited

structure CoordinateWire where
  request : Nat
  source : TagWire
  deriving ToJson, FromJson, Inhabited

structure CloseWire where
  kind : String
  outcome : Option String
  segments : Option Nat
  stream_bytes : Option (List Nat)
  deriving ToJson, FromJson, Inhabited

/-- A canonical segment in the shape `canonicalSegmentJson` writes. -/
structure SegmentWire where
  id : Nat
  coordinate : CoordinateWire
  writer : TagWire
  flush : Option FlushWire
  close : Option CloseWire
  created_at : Nat
  deriving ToJson, FromJson, Inhabited

def segmentWire (segment : Segment) : SegmentWire :=
  match (Json.parse (Conformance.Contracts.canonicalSegmentJson segment)).bind fromJson? with
  | .ok wire => wire
  | .error _ => default

/-- The parent the admission names, as the execution owner reads it. -/
structure ParentWire where
  present : Bool
  provenance : Option String
  state : Option String
  request : Option Nat
  session : Option Nat
  deadline : Option Nat
  awaitMode : Option String
  deriving ToJson

structure ChildWire where
  document : Nat
  deadline : Nat
  awaitMode : String
  deriving ToJson

structure AdmissionInput where
  name : String
  generation : Nat
  ordinal : Nat
  parent : ParentWire
  child : ChildWire
  arguments : SegmentWire
  prior : Option SegmentWire
  deriving ToJson

private def awaitSpelling : ToolExecution.AwaitMode → String
  | .foreground => "foreground" | .background => "background"

private def admissionInput (item : String × World × Nat × PluginEffectAdmission) : AdmissionInput :=
  let (name, before, generation, call) := item
  let parent : ParentWire := match ownedToolByDocument? before call.parentToolDoc with
    | none => ⟨false, none, none, none, none, none, none⟩
    | some p => ⟨true,
        some (match p.provenance with
          | .acceptedIntent => "accepted" | .spawnedBackground _ => "spawned"
          | .pluginEffect _ _ => "plugin_effect"),
        some (match p.context.state with
          | .pending => "pending" | .running => "running" | .completed => "completed"
          | .failed => "failed" | .cancelled => "cancelled" | .timedOut => "timed_out"),
        some p.requestDoc, some p.session, some p.context.deadline,
        some (awaitSpelling p.context.awaitMode)⟩
  { name, generation, ordinal := call.ordinal, parent
    child := ⟨call.document, call.context.deadline, awaitSpelling call.context.awaitMode⟩
    arguments := segmentWire call.arguments
    prior := if (ownedToolByDocument? before call.document).isSome then
      some (segmentWire arguments) else none }

@[conformance_family]
def admissions : Family AdmissionInput Bool where
  inputs := scenarios.map admissionInput
  model := fun i => match scenarios.find? (·.1 == i.name) with
    | some (_, before, generation, call) => succeeds (admitPluginEffect before generation call)
    | none => false

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

structure TerminalInput where
  replay : Bool
  conflict : Bool
  deriving ToJson

@[conformance_family]
def terminalPresentation : Family TerminalInput Bool where
  inputs := [⟨false, false⟩, ⟨true, false⟩, ⟨true, true⟩]
  model := fun i => terminalCase i.replay i.conflict

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

private def completeParent (world : World) : World :=
  match ToolDelivery.completeAndDeliver world 600 (.native .complete)
      ToolDelivery.Cases.toolOutputClose (ToolDelivery.Cases.foregroundResultMessage 1) with
  | .ok post => post
  | .error _ => world

private def shortChild : World :=
  match admitPluginEffect baseline 7
      { admission with context := { admission.context with deadline := 4 } } with
  | .ok admitted => match dispatch admitted 7 ⟨601, true, true⟩ with
    | .ok post => post | .error _ => admitted
  | .error _ => baseline

private def closeChild (before : World) (action : ToolExecution.ToolCallContext.Action) : World :=
  match Gate.evaluate (.pluginEffectClose 601 (.native action) output terminalPayload) before with
  | .ok post => post
  | .error _ => before

private def timedOutChild : World := closeChild shortChild .timeout
private def completedChild : World := closeChild running .complete

theorem child_timeout_is_child_terminal_and_parent_continues :
    ((ownedToolByDocument? timedOutChild 601).any fun child =>
      child.context.state == .timedOut && child.terminalOutput == some terminalPayload) = true ∧
    ((ownedToolByDocument? timedOutChild 600).any fun parent =>
      parent.context.state == .running) = true ∧
    ((ownedToolByDocument? (completeParent timedOutChild) 600).any fun parent =>
      parent.context.state == .completed) = true ∧
    normalCompletionToolsReady (completeParent timedOutChild) 7 = true := by native_decide

theorem parent_interruption_retains_completed_children :
    ((ownedToolByDocument? (accountOwnedTools completedChild 7 true) 601).any fun child =>
      child.context.state == .completed && child.terminalOutput == some terminalPayload) = true := by
  native_decide

private def backgroundAdmitted : World := match admitPluginEffect backgroundParent 7 admission with
  | .ok post => post | .error _ => backgroundParent

private def backgroundRunningChild : World :=
  match dispatch backgroundAdmitted 7 ⟨601, true, true⟩ with
  | .ok post => post | .error _ => backgroundAdmitted

/-- Request accounting leaves the children of a running background invocation
to that invocation, and still hands the background invocation itself off. -/
theorem live_background_parent_keeps_its_children :
    ((ownedToolByDocument? (accountOwnedTools backgroundAdmitted 7 true) 601).any fun child =>
      child.context.state == .pending) = true ∧
    ((ownedToolByDocument? (accountOwnedTools backgroundRunningChild 7 true) 601).any fun child =>
      child.context.state == .running && child.stuckSince.isNone) = true ∧
    ((ownedToolByDocument? (accountOwnedTools backgroundRunningChild 7 true) 600).any fun parent =>
      parent.context.state == .running && parent.stuckSince.isSome) = true := by native_decide

private def accountingWorld : String → World
  | "pending_cancel" => admitted
  | "running_normal" => runningWithCompletedParent
  | "running_exceptional" => running
  | "timed_out_normal" => completeParent timedOutChild
  | "completed_exceptional" => completedChild
  | "background_parent_pending" => backgroundAdmitted
  | "background_parent_running" => backgroundRunningChild
  | _ => baseline

structure AccountingInput where
  name : String
  exceptional : Bool
  deriving ToJson

structure AccountingOutput where
  state : String
  started : Bool
  stuck : Bool
  terminalOutput : Bool
  normalReady : Bool
  deriving ToJson

private def accountingObservation (i : AccountingInput) : AccountingOutput :=
  let before := accountingWorld i.name
  let post := if i.exceptional then accountOwnedTools before 7 i.exceptional else before
  let child := (ownedToolByDocument? post 601).getD
    { document := 601, requestDoc := 10, session := 1, acceptedSequence := 0,
      context := admission.context }
  { state := match child.context.state with
      | .pending => "pending" | .running => "running" | .completed => "completed"
      | .failed => "failed" | .cancelled => "cancelled" | .timedOut => "timedOut"
    started := child.context.startedAt.isSome
    stuck := child.stuckSince.isSome
    terminalOutput := child.terminalOutput.isSome
    normalReady := normalCompletionToolsReady post 7 }

/-- Exceptional accounting runs first; `normalReady` is then whether normal
completion of the request would be admitted. -/
@[conformance_family]
def childAccounting : Family AccountingInput AccountingOutput where
  inputs :=
    [ ⟨"pending_cancel", true⟩, ⟨"running_normal", false⟩, ⟨"running_exceptional", true⟩
    , ⟨"timed_out_normal", false⟩, ⟨"completed_exceptional", true⟩
    , ⟨"background_parent_pending", true⟩, ⟨"background_parent_running", true⟩ ]
  model := accountingObservation

private def backgroundRunning : World := Id.run do
  let admission := { admission with context := { admission.context with awaitMode := .background } }
  let .ok accepted := admitPluginEffect baseline 7 admission | return baseline
  let .ok running := dispatch accepted 7 ⟨601, true, true⟩ | return accepted
  return running

private def receipt : Segment :=
  { id := 604, coordinate := ⟨10, .authored 91⟩, writer := .tool 601
    flush := some ⟨0, [⟨0, 2, some { block := 0, part := 0, kind := .toolOutput }⟩], [123, 125]⟩
    close := some (.closed .complete 1 [2]), createdAt := 5 }

private def receiptAccepted (replay changed terminal : Bool) : Bool := Id.run do
  let before := if terminal then closeChild backgroundRunning .complete else backgroundRunning
  let before := if replay then
      (ToolDelivery.publishPluginReceipt before 601 receipt).toOption.getD before
    else before
  let record := if changed then { receipt with
      flush := some ⟨0, [⟨0, 4, some { block := 0, part := 0, kind := .toolOutput }⟩], [110, 117, 108, 108]⟩
      close := some (.closed .complete 1 [4]) } else receipt
  return succeeds (ToolDelivery.publishPluginReceipt before 601 record)

theorem asynchronous_receipt_regressions :
    [receiptAccepted false false false, receiptAccepted true false false,
      receiptAccepted true true false, receiptAccepted false false true] =
    [true, true, false, false] := by native_decide

structure ReceiptInput where
  replay : Bool
  changed : Bool
  terminal : Bool
  deriving ToJson

@[conformance_family]
def receipts : Family ReceiptInput Bool where
  inputs := [(false, false, false), (true, false, false), (true, true, false), (false, false, true)].map
    fun (replay, changed, terminal) => { replay, changed, terminal }
  model := fun i => receiptAccepted i.replay i.changed i.terminal

end Conformance.PluginEffects
