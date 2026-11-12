import Lean
import Proofs.Shared.CompletionRetry.Executable
import Proofs.Session.PromptAssembly.AggregateBudget

namespace CompletionLoop
open Lean CompletionRetry

structure Usage where
  input : Nat
  output : Nat
  total : Nat
  deriving ToJson

def Usage.native (usage : Usage) : PromptAssembly.AggregateBudget.Usage :=
  ⟨usage.input, usage.output, usage.total⟩

structure Call where
  failure : Option String := none
  text : List String := []
  reasoning : List String := []
  tool : Bool := false
  firstPoll : Bool := false
  poison : Bool := true
  usage : Option Usage := none
  deriving ToJson

structure Input where
  calls : List Call
  transport : Nat := 3
  resample : Nat := 2
  repair : Bool := true
  deadlineFits : Bool := true
  maxTurns : Nat := 0
  context : Bool := false
  initialDirty : Bool := false
  missingCapture : Bool := false
  limit : Option Nat := none
  nestedUsage : Option Usage := none
  structured : Bool := false
  recoveredPair : Bool := false
  deriving ToJson

structure Coordinate where
  turn : Nat
  attempt : Nat
  deriving ToJson

structure Retry where
  turn : Nat
  attempt : Nat
  retry : Bool
  delayed : Bool
  deriving ToJson

structure Capture where
  turn : Nat
  attempt : Nat
  path : String
  effective : Bool
  results : List String
  matchesTransport : Bool
  deriving ToJson

structure Observation where
  text : List String := []
  reasoning : List String := []
  tools : List String := []
  retractions : List Coordinate := []
  retries : List Retry := []
  finalText : Option String := none
  failed : Bool := false
  dirtyInputs : List Bool := []
  contextInputs : List Bool := []
  captures : List Capture := []
  allowances : List Nat := []
  ledgerUsed : Option Nat := none
  deriving ToJson

def failureClass : String → FailureClass
  | "parse" => .parseBadRequest
  | "reasoning" => .reasoningRejected
  | "permanent" => .permanent
  | _ => .transport

def structuredValid (text : List String) : Bool :=
  match Json.parse (String.join text) with
  | .ok value => (value.getObjValAs? String "answer").isOk
  | .error _ => false

def transition (state : State) (action : Action) : State :=
  (step? state action).getD state

/-- A provider failure before output uses the classified retry policy. Once
preview bytes or an unaccepted tool intent exist, transport recovery first
retracts that attempt. Only accepted tool turns enter the next request. -/
def run (input : Input) : Observation := Id.run do
  let budget : Budget := ⟨input.transport, input.resample, 1, input.repair, 0⟩
  let mut state := { baseState with phase := .issuing, now := 0, budget := budget, deadline := if input.deadlineFits then none else some 0 }
  let mut result : Observation := {}
  let mut dirty := input.initialDirty
  let mut repaired := input.nestedUsage.isSome
  let mut repairAttempt := false
  let mut ledger := input.limit.map fun limit =>
    (⟨limit, (input.nestedUsage.map fun usage => usage.input + usage.output).getD 0⟩ : PromptAssembly.AggregateBudget.Ledger)
  result := { result with ledgerUsed := ledger.map (·.used) }
  for call in input.calls do
    if result.failed || result.finalText.isSome then break
    if input.maxTurns > 0 && state.turn ≥ input.maxTurns then
      result := { result with failed := true }
      break
    if ledger.isSome && (ledger.map (·.remaining)).getD 0 == 0 then
      result := { result with failed := true }
      break
    result := { result with allowances := result.allowances ++ (ledger.map (·.remaining)).toList }
    result := { result with dirtyInputs := result.dirtyInputs ++ [dirty], contextInputs := result.contextInputs ++ [input.context], text := result.text ++ call.text, reasoning := result.reasoning ++ call.reasoning }
    result := { result with captures := result.captures ++ [⟨state.turn, state.attempt, if repairAttempt then "repair" else "budgeted", input.context || repaired, (if input.initialDirty then ["ok"] else if input.recoveredPair then ["restored"] else []) ++ result.tools, true⟩] }
    if input.missingCapture then
      result := { result with failed := true, text := [], reasoning := [] }
      break
    state := transition state .issue
    let preview := !call.text.isEmpty || !call.reasoning.isEmpty || call.tool
    if input.limit.isSome && (call.failure.isNone || preview) then
      let charged := ledger.map fun before => PromptAssembly.AggregateBudget.chargeReported before
        (if call.failure.isNone then call.usage.map Usage.native else none)
      ledger := PromptAssembly.AggregateBudget.sharedCharge ledger
        (if call.failure.isNone then call.usage.map Usage.native else none)
      result := { result with ledgerUsed := ledger.map (·.used) }
      let terminal := !call.tool && !call.text.isEmpty && (!input.structured || structuredValid call.text)
      let failed := charged.any fun charged =>
        PromptAssembly.AggregateBudget.postChargeAction charged terminal == .fail
      if failed then
        result := { result with failed := true }
        break
    if call.failure.isNone && call.tool then
      result := { result with tools := result.tools ++ ["ECHOED"] }
      dirty := dirty || call.poison
      repairAttempt := false
      state := { state with phase := .issuing, turn := state.turn + 1, attempt := 0, emptyUsed := 0, compactUsed := false }
    else if call.failure.isNone && !call.text.isEmpty && (!input.structured || structuredValid call.text) then
      result := { result with finalText := some (String.join call.text) }
    else
      let preview := !call.text.isEmpty || !call.reasoning.isEmpty || call.tool
      let empty := call.failure.isNone && !input.structured
      let failure := if empty then FailureClass.emptySample else
        if preview then .transport else failureClass (call.failure.getD "transport")
      let before := state
      state := transition state (.observeFailure failure (call.failure.getD "empty") (state.now + 1))
      state := transition state (.confirmRetraction true)
      state := transition state .schedule
      let retry := match state.phase with
        | .backingOff _ | .repairing => true
        | _ => false
      let delayed := match state.phase with | .backingOff _ => true | _ => false
      if !preview && !empty then
        result := { result with retries := result.retries ++ [⟨before.turn, before.attempt, retry, delayed⟩] }
      if retry then
        if preview || empty then result := { result with retractions := result.retractions ++ [⟨before.turn, before.attempt⟩] }
        let repairing := match state.phase with | .repairing => true | _ => false
        state := match state.phase with
          | .backingOff wake => transition state (.wake wake)
          | .repairing => transition state .repairIssue
          | _ => state
        if repairing then
          dirty := false
          repaired := true
          repairAttempt := true
      else result := { result with failed := true }
  return result

structure TextObservation where
  text : Option String
  failed : Bool
  deriving ToJson

def toText (input : Input) : TextObservation :=
  let result := run input
  ⟨result.finalText, result.failed⟩

end CompletionLoop
