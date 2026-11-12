import Proofs.Automation.Triggers.Durable
import Proofs.Automation.EventDelivery.Durable
import Proofs.Conformance.ContractTypes
import Proofs.Automation.Triggers.SessionKey
import Proofs.Automation.Triggers.FireBudget
import Proofs.Conformance.Family

namespace Conformance.TriggerDelivery

open Triggers.Durable Conformance.Contracts

structure IdentityWire where
  ownerDid : String
  triggerId : String
  sourceCollection : String
  sourceDocId : String
  deriving Lean.ToJson

def IdentityWire.ofModel (id : Identity) : IdentityWire :=
  { ownerDid := id.owner, triggerId := id.trigger, sourceCollection := id.collection,
    sourceDocId := id.document }

def IdentityWire.toModel (id : IdentityWire) : Identity :=
  { owner := id.ownerDid, trigger := id.triggerId, collection := id.sourceCollection,
    document := id.sourceDocId }

structure FireWire where
  identity : IdentityWire
  session : String
  serial : Bool
  emitOutcome : Bool
  goalBacked : Bool
  deriving Lean.ToJson

def FireWire.ofModel (f : Fire) : FireWire :=
  { identity := .ofModel f.identity, session := f.session, serial := f.serial,
    emitOutcome := f.emitOutcome, goalBacked := f.goalBacked }

def FireWire.toModel (f : FireWire) : Fire :=
  { identity := f.identity.toModel, session := f.session, serial := f.serial,
    emitOutcome := f.emitOutcome, goalBacked := f.goalBacked }

structure RequestWire where
  fire : FireWire
  running : Bool
  terminal : Bool
  assignmentReplaced : Bool
  goalAssignmentApplied : Bool
  deriving Lean.ToJson

def RequestWire.ofModel (r : Request) : RequestWire :=
  { fire := .ofModel r.fire, running := r.running, terminal := r.terminal,
    assignmentReplaced := r.assignmentReplaced, goalAssignmentApplied := r.goalAssignmentApplied }

def RequestWire.toModel (r : RequestWire) : Request :=
  { fire := r.fire.toModel, running := r.running, terminal := r.terminal,
    assignmentReplaced := r.assignmentReplaced, goalAssignmentApplied := r.goalAssignmentApplied }

structure GoalBindingWire where
  owner : String
  session : String
  assignment : IdentityWire
  status : String
  deriving Lean.ToJson

def GoalBindingWire.ofModel (g : GoalBinding) : GoalBindingWire :=
  { owner := g.owner, session := g.session, assignment := .ofModel g.assignment,
    status := g.status.toDefraDB }

def GoalBindingWire.toModel (g : GoalBindingWire) : GoalBinding :=
  { owner := g.owner, session := g.session, assignment := g.assignment.toModel,
    status := (Goals.Status.fromDefraDB? g.status).getD .active }

/-- Production keeps no publication order between outcome rows, so the wire
lists published outcomes in receipt order. -/
structure StateWire where
  receipts : List IdentityWire
  requests : List RequestWire
  outcomes : List IdentityWire
  goals : List GoalBindingWire
  deriving Lean.ToJson

def StateWire.ofModel (s : State) : StateWire :=
  { receipts := s.receipts.map .ofModel, requests := s.requests.map .ofModel,
    outcomes := (s.receipts.filter fun id => decide (id ∈ s.outcomes)).map .ofModel,
    goals := s.goals.map .ofModel }

def StateWire.toModel (s : StateWire) : State :=
  { receipts := s.receipts.map (·.toModel), requests := s.requests.map (·.toModel),
    outcomes := s.outcomes.map (·.toModel), goals := s.goals.map (·.toModel) }

def callbackVisibilityCases : List String := do
  let visible ← [false, true]
  let linked ← [false, true]
  pure <| "{\"visible\":" ++ toString visible ++ ",\"linked\":" ++ toString linked ++
    ",\"ready\":" ++ toString (EventDelivery.Durable.callbackResultReady visible linked) ++
    ",\"after\":" ++ toString (EventDelivery.Durable.callbackResultCursor 4 5 visible linked) ++ "}"

def object (fields : List (String × String)) : String :=
  "{" ++ String.intercalate "," (fields.map fun (k, v) => jsonString k ++ ":" ++ v) ++ "}"

def identity (owner document : String) : Identity :=
  { owner, document, trigger := "handoff", collection := "Work" }

def fire (document : String) : Fire :=
  { identity := identity "owner-a" document, session := "lead-session",
    serial := true, emitOutcome := true, goalBacked := false }

def identityJson (id : Identity) : String := object [
  ("owner_did", jsonString id.owner), ("trigger_id", jsonString id.trigger),
  ("source_collection", jsonString id.collection), ("source_doc_id", jsonString id.document)]

def fireJson (f : Fire) : String := object [
  ("identity", identityJson f.identity), ("session", jsonString f.session),
  ("serial", toString f.serial), ("emit_outcome", toString f.emitOutcome),
  ("goal_backed", toString f.goalBacked)]

def requestJson (r : Request) : String := object [
  ("fire", fireJson r.fire), ("running", toString r.running),
  ("terminal", toString r.terminal), ("assignment_replaced", toString r.assignmentReplaced),
  ("goal_assignment_applied", toString r.goalAssignmentApplied)]

def goalBindingJson (goal : GoalBinding) : String := object [
  ("owner", jsonString goal.owner), ("session", jsonString goal.session),
  ("assignment", identityJson goal.assignment), ("status", jsonString goal.status.toDefraDB)]

def stateJson (s : State) : String := object [
  ("receipts", jsonArray (s.receipts.map identityJson)),
  ("requests", jsonArray (s.requests.map requestJson)),
  ("outcomes", jsonArray (s.outcomes.map identityJson)),
  ("goals", jsonArray (s.goals.map goalBindingJson))]

structure AdmissionInput where
  name : String
  pre : StateWire
  fire : FireWire
  commit : Bool
  deriving Lean.ToJson

structure AdmissionOutput where
  sourceAllowed : Bool
  committed : Bool
  post : StateWire
  deriving Lean.ToJson

def admissionInput (name : String) (pre : State) (f : Fire) (commit : Bool) : AdmissionInput :=
  { name, pre := .ofModel pre, fire := .ofModel f, commit }

@[conformance_family]
def fireAdmission : Family AdmissionInput AdmissionOutput where
  inputs := [
    admissionInput "outcome_consumer_cannot_opt_in" {}
      { fire "outcome" with identity := { (fire "outcome").identity with collection := "FireOutcome" } } true,
    admissionInput "outcome_consumer_opted_out_admitted" {}
      { fire "outcome" with emitOutcome := false, identity := { (fire "outcome").identity with collection := "FireOutcome" } } true,
    admissionInput "crash_before_fire_commit" {} (fire "a") false,
    admissionInput "fire_commit" {} (fire "a") true,
    admissionInput "crash_after_fire_commit_retry" (admit {} (fire "a")) (fire "a") true,
    admissionInput "same_trigger_under_two_owners" (admit {} (fire "a"))
      { fire "a" with identity := identity "owner-b" "a" } true,
    admissionInput "same_document_other_collection" (admit {} (fire "a"))
      { fire "a" with identity := { identity "owner-a" "a" with collection := "Other" } } true,
    admissionInput "serial_burst_is_persisted" (claim (admit {} (fire "a")) (fire "a").identity)
      { fire "b" with session := "other-session" } true]
  model := fun i =>
    let f := i.fire.toModel
    let allowed := Triggers.outcomeSourceAllowed f.identity.collection f.emitOutcome
    { sourceAllowed := allowed, committed := allowed && i.commit,
      post := .ofModel (admitTransaction i.pre.toModel f i.commit) }

def claimObservationJson (row : ClaimObservation) : String := object [
  ("document", jsonString row.document), ("owner", jsonString row.owner),
  ("session", jsonString row.session), ("trigger", jsonString row.trigger),
  ("serial", toString row.serial), ("receipt", toString row.receipt),
  ("arrival", row.arrival.map toString |>.getD "null"),
  ("running", toString row.running), ("terminal", toString row.terminal)]

structure ClaimObservationWire where
  document : String
  owner : String
  session : String
  trigger : String
  serial : Bool
  receipt : Bool
  arrival : Option Nat
  running : Bool
  terminal : Bool
  deriving Lean.ToJson

def ClaimObservationWire.ofModel (row : ClaimObservation) : ClaimObservationWire :=
  { document := row.document, owner := row.owner, session := row.session, trigger := row.trigger,
    serial := row.serial, receipt := row.receipt, arrival := row.arrival, running := row.running,
    terminal := row.terminal }

structure QueueInput where
  name : String
  pre : StateWire
  identity : IdentityWire
  /-- The claim observations production reads for the requests of `pre`
  (`requestObservations`). -/
  observations : List ClaimObservationWire
  deriving Lean.ToJson

def queueInput (name : String) (pre : State) (id : Identity) : QueueInput :=
  { name, pre := .ofModel pre, identity := .ofModel id,
    observations := (requestObservations pre).map .ofModel }

def queueInputs : List QueueInput :=
  let a := fire "a"
  let b := { fire "b" with session := "other-session" }
  let queued := admit (admit {} a) b
  let busy := claim queued a.identity
  let sameSession := { b with identity := { b.identity with trigger := "other-trigger" }, session := a.session, serial := false }
  [queueInput "serial_first_claims" queued a.identity,
   queueInput "serial_second_waits" queued b.identity,
   queueInput "serial_running_blocks_next_session" busy b.identity,
   queueInput "serial_terminal_releases_next" (terminalize busy a.identity) b.identity,
   queueInput "busy_session_queues_another_trigger" (admit busy sameSession) sameSession.identity,
   queueInput "owner_is_part_of_queue_scope"
     (admit busy { b with identity := identity "owner-b" "b" }) (identity "owner-b" "b")]

structure QueueClaimOutput where
  canClaim : Bool
  deriving Lean.ToJson

def queueClaim (i : QueueInput) : QueueClaimOutput :=
  { canClaim := canClaim i.pre.toModel i.identity.toModel }

@[conformance_family]
def queueClaims : Family QueueInput QueueClaimOutput where
  inputs := queueInputs
  model := queueClaim

/-- A served node stages one serial Trigger's first two fires in order, before
either runs, so its claim queue builds only the first fire and the second one
waiting behind it. -/
def serialPairInputs : List QueueInput :=
  queueInputs.filter fun i => i.name ∈ ["serial_first_claims", "serial_second_waits"]

@[conformance_family]
def queuePagedClaims : Family QueueInput QueueClaimOutput where
  inputs := serialPairInputs
  model := queueClaim

@[conformance_family]
def queueDispatchClaims : Family QueueInput QueueClaimOutput where
  inputs := serialPairInputs
  model := queueClaim

def reasonString : OutcomeReason → String
  | .requestTerminal => "request_terminal"
  | .goalTerminal status => status.toDefraDB
  | .superseded => "superseded"

structure OutcomeInput where
  name : String
  pre : StateWire
  deriving Lean.ToJson

def outcomeInput (name : String) (r : Request) (status : Option Goals.Status) (already : Bool) :
    OutcomeInput :=
  let bindings := status.toList.map fun value => ({
    owner := r.fire.identity.owner
    session := r.fire.session
    assignment := r.fire.identity
    status := value } : GoalBinding)
  { name, pre := .ofModel {
    receipts := [r.fire.identity]
    requests := [r]
    outcomes := if already then [r.fire.identity] else []
    goals := bindings } }

def outcomeInputs : List OutcomeInput :=
  let ordinary : Request := { fire := fire "a", terminal := true }
  let goal : Request := { ordinary with fire := { ordinary.fire with goalBacked := true }, goalAssignmentApplied := true }
  [outcomeInput "ordinary_terminal_outcome" ordinary none false,
   outcomeInput "crash_after_terminal_before_outcome" ordinary none false,
   outcomeInput "crash_after_outcome_retry" ordinary none true,
   outcomeInput "opted_out" { ordinary with fire := { ordinary.fire with emitOutcome := false } } none false,
   outcomeInput "outcome_consumer_ends_chain"
     { ordinary with fire := { ordinary.fire with emitOutcome := false, identity := { ordinary.fire.identity with collection := "FireOutcome" } } } none false,
   outcomeInput "continuing_goal_has_no_outcome" goal (some .active) false,
   outcomeInput "queued_goal_ignores_previous_complete" { goal with terminal := false, goalAssignmentApplied := false } (some .complete) false,
   outcomeInput "preclaim_cancelled_goal" { goal with goalAssignmentApplied := false } none false,
   outcomeInput "replaced_applied_goal" { goal with assignmentReplaced := true } none false,
   outcomeInput "completed_goal" goal (some .complete) false,
   outcomeInput "stopped_goal" goal (some .stopped) false,
   outcomeInput "blocked_goal" goal (some .blocked) false,
   outcomeInput "budget_exhausted_goal" goal (some .budgetLimited) false,
   outcomeInput "paused_goal" goal (some .paused) false,
   outcomeInput "usage_limited_goal" goal (some .usageLimited) false]

structure OutcomeReasonOutput where
  due : Bool
  reason : Option String
  deriving Lean.ToJson

@[conformance_family]
def outcomeReasons : Family OutcomeInput OutcomeReasonOutput where
  inputs := outcomeInputs
  model := fun i =>
    let pre := i.pre.toModel
    match pre.requests.head? with
    | none => { due := false, reason := none }
    | some r => { due := outcomeDue pre r, reason := (outcomeReason pre r).map reasonString }

structure OutcomeRecoveryOutput where
  post : StateWire
  recovered : Nat
  recoveredAgain : Nat
  deriving Lean.ToJson

@[conformance_family]
def outcomeRecovery : Family OutcomeInput OutcomeRecoveryOutput where
  inputs := outcomeInputs
  model := fun i =>
    let pre := i.pre.toModel
    let post := recoverOutcomes pre
    { post := .ofModel post, recovered := post.outcomes.length - pre.outcomes.length,
      recoveredAgain := (recoverOutcomes post).outcomes.length - post.outcomes.length }

def retainedOutcomeCase (name : String) (request : Request)
    (retained current : Option GoalBinding) (settled : Bool := false) : String :=
  let reason := retainedOutcomeReason request retained current
  object [("name", jsonString name), ("request", requestJson request),
    ("retained", (retained.map goalBindingJson).getD "null"),
    ("current", (current.map goalBindingJson).getD "null"),
    ("settled", toString settled),
    ("classification", jsonOptionalString (reason.map reasonString)),
    ("recover_reason", jsonOptionalString (if settled then none else reason.map reasonString))]

def retainedOutcomeCases : List String :=
  let f := { fire "retained" with goalBacked := true }
  let request : Request := { fire := f, terminal := true, goalAssignmentApplied := true }
  let old : GoalBinding := { owner := request.fire.identity.owner, session := request.fire.session, assignment := request.fire.identity }
  let next : GoalBinding := { old with assignment := identity "owner-a" "fresh" }
  [retainedOutcomeCase "retained_stopped_after_new_goal" request (some { old with status := .stopped }) (some next),
   retainedOutcomeCase "retained_complete_after_new_goal" request (some { old with status := .complete }) (some next),
   retainedOutcomeCase "retained_blocked_after_new_goal" request (some { old with status := .blocked }) (some next),
   retainedOutcomeCase "retained_budget_after_new_goal" request (some { old with status := .budgetLimited }) (some next),
   retainedOutcomeCase "retained_active_superseded" request (some old) (some next),
   retainedOutcomeCase "retained_paused_superseded" request (some { old with status := .paused }) (some next),
   retainedOutcomeCase "retained_usage_limited_superseded" request (some { old with status := .usageLimited }) (some next),
   retainedOutcomeCase "retained_active_without_current" request (some old) none,
   retainedOutcomeCase "retained_stopped_without_current" request (some { old with status := .stopped }) none,
   retainedOutcomeCase "missing_retained_cannot_borrow_current" request none (some next),
   retainedOutcomeCase "foreign_retained_owner" request (some { old with owner := "owner-b" }) (some next),
   retainedOutcomeCase "foreign_retained_session" request (some { old with session := "other-session" }) (some next),
   retainedOutcomeCase "foreign_retained_root" request (some { old with assignment := next.assignment }) (some next),
   retainedOutcomeCase "foreign_current_cannot_supersede" request (some old) (some { next with owner := "owner-b" }),
   retainedOutcomeCase "settled_retained_complete_after_new_goal" request (some { old with status := .complete }) (some next) true]

inductive OutcomeAction where
  | admitFire (fire : Fire)
  | claimFire (id : Identity)
  | requestTerminal (id : Identity) (terminalState : String) (commit : Bool)
  | goalStatus (id : Identity) (status : Goals.Status)
  | recover (commit : Bool)

def outcomeActionJson : OutcomeAction → String
  | .admitFire f => object [("kind", jsonString "admit"), ("fire", fireJson f)]
  | .claimFire id => object [("kind", jsonString "claim"), ("identity", identityJson id)]
  | .requestTerminal id terminalState commit => object [("kind", jsonString "request_terminal"),
      ("identity", identityJson id), ("terminal_state", jsonString terminalState), ("commit", toString commit)]
  | .goalStatus id status => object [("kind", jsonString "goal_status"),
      ("identity", identityJson id), ("status", jsonString status.toDefraDB)]
  | .recover commit => object [("kind", jsonString "recover"), ("commit", toString commit)]

def applyOutcomeAction (state : State) : OutcomeAction → State
  | .admitFire f => admit state f
  | .claimFire id => claim state id
  | .requestTerminal id _ commit => if commit then terminalize state id else state
  | .goalStatus id status => setGoal state id status
  | .recover commit => if commit then recoverOutcomes state else state

def outcomeSteps (state : State) : List OutcomeAction → List String
  | [] => []
  | action :: actions =>
    let post := applyOutcomeAction state action
    let reasons := (post.outcomes.filter fun id => !decide (id ∈ state.outcomes)).map fun id =>
      let reason := ((state.requests.find? (·.fire.identity == id)).bind (outcomeReason state)).orElse
        (fun _ => (post.requests.find? (·.fire.identity == id)).bind (outcomeReason post))
      object [("identity", identityJson id), ("reason", jsonOptionalString (reason.map reasonString))]
    object [("action", outcomeActionJson action), ("pre", stateJson state),
      ("post", stateJson post), ("published", jsonArray reasons)] :: outcomeSteps post actions

def outcomeTrace (name : String) (actions : List OutcomeAction) : String :=
  object [("name", jsonString name), ("steps", jsonArray (outcomeSteps {} actions))]

def outcomeTraces : List String :=
  let a := fire "a"
  let goal := { a with goalBacked := true }
  let next := { goal with identity := identity "owner-a" "b" }
  let ended := [.requestTerminal a.identity "completed" true, .recover true]
  let replacementPrefix := [.admitFire goal, .claimFire goal.identity, .requestTerminal goal.identity "completed" true]
  [outcomeTrace "terminal_and_publication_crash_boundaries"
      ([.admitFire a, .requestTerminal a.identity "completed" false, .recover true,
        .requestTerminal a.identity "completed" true, .recover false, .recover true, .recover true]),
   outcomeTrace "goal_request_boundary_then_complete"
      ([.admitFire goal, .claimFire goal.identity] ++ ended ++ [.goalStatus goal.identity .complete, .recover true]),
   outcomeTrace "goal_preclaim_cancellation" [.admitFire goal, .requestTerminal goal.identity "interrupted" true, .recover true],
   outcomeTrace "paused_usage_limited_preserve_outcome"
      (replacementPrefix ++ [.goalStatus goal.identity .paused, .recover true,
        .goalStatus goal.identity .usageLimited, .recover true, .goalStatus goal.identity .budgetLimited, .recover true]),
   outcomeTrace "replacement_supersedes_previous_assignment"
      (replacementPrefix ++ [.admitFire next, .claimFire next.identity, .requestTerminal next.identity "completed" true,
        .goalStatus next.identity .complete, .recover true, .recover true]),
   outcomeTrace "replacement_preserves_prior_complete_boundary"
      (replacementPrefix ++ [.goalStatus goal.identity .complete, .admitFire next, .claimFire next.identity, .recover true]),
   outcomeTrace "outcome_consumer_rejects_recursive_emission"
      [.admitFire { a with identity := { a.identity with collection := "FireOutcome" } }, .recover true],
   outcomeTrace "outcome_consumer_stops_chain"
      ([.admitFire { a with emitOutcome := false, identity := { a.identity with collection := "FireOutcome" } },
        .requestTerminal { a.identity with collection := "FireOutcome" } "completed" true, .recover true])]

def arrival (position : Nat) (document : String) : EventDelivery.Durable.Arrival :=
  { position, identity := identity "owner-a" document }

structure CursorScenario where
  name : String
  seedHead : Nat := 1
  priorAfter : Option Nat := none
  restart : Bool := false
  enabled : Bool := true
  committed : State := {}
  entry : EventDelivery.Durable.Arrival := arrival 2 "b"
  matchesFilter : Bool := true
  excludedDocuments : List String := []
  mode : ConcurrencyMode := .queuedSerial
  busy : Bool := false
  admissionCommit : Option Bool := none
  checkpointCommit : Option Bool := none

structure ArrivalWire where
  position : String
  identity : IdentityWire
  deriving Lean.ToJson

def ArrivalWire.ofModel (entry : EventDelivery.Durable.Arrival) : ArrivalWire :=
  { position := toString entry.position, identity := .ofModel entry.identity }

def ArrivalWire.toModel (entry : ArrivalWire) : EventDelivery.Durable.Arrival :=
  { position := entry.position.toNat!, identity := entry.identity.toModel }

structure CursorWire where
  seeded : Bool
  after : String
  deriving Lean.ToJson

def CursorWire.ofModel (cursor : EventDelivery.Durable.Cursor) : CursorWire :=
  { seeded := cursor.seeded, after := toString cursor.after }

def modeWire : ConcurrencyMode → String
  | .serial => "serial"
  | .queuedSerial => "queued_serial"
  | .parallel => "parallel"
  | .latestOnly => "latest_only"

def modeOfWire : String → ConcurrencyMode
  | "serial" => .serial
  | "parallel" => .parallel
  | "latest_only" => .latestOnly
  | _ => .queuedSerial

def cursorSource : List EventDelivery.Durable.Arrival := [arrival 1 "a", arrival 2 "b", arrival 3 "c"]

/-- `sourceEligible` is the filter's verdict on each source document, in source
order; `fire` is the fire the entry's admission stages. -/
structure CursorInput where
  name : String
  seedHead : String
  priorAfter : Option String
  restart : Bool
  enabled : Bool
  pre : StateWire
  source : List ArrivalWire
  entry : ArrivalWire
  sourceEligible : List Bool
  journalHead : String
  mode : String
  busy : Bool
  fire : FireWire
  matchesFilter : Bool
  admissionCommit : Option Bool
  checkpointCommit : Option Bool
  deriving Lean.ToJson

def cursorInput (scenario : CursorScenario) : CursorInput :=
  let eligible := fun entry : EventDelivery.Durable.Arrival =>
    scenario.matchesFilter && !scenario.excludedDocuments.contains entry.identity.document
  { name := scenario.name, seedHead := toString scenario.seedHead,
    priorAfter := scenario.priorAfter.map toString, restart := scenario.restart,
    enabled := scenario.enabled, pre := .ofModel scenario.committed,
    source := cursorSource.map .ofModel, entry := .ofModel scenario.entry,
    sourceEligible := cursorSource.map eligible, journalHead := "3", mode := modeWire scenario.mode,
    busy := scenario.busy,
    fire := .ofModel { fire scenario.entry.identity.document with identity := scenario.entry.identity },
    matchesFilter := scenario.matchesFilter, admissionCommit := scenario.admissionCommit,
    checkpointCommit := scenario.checkpointCommit }

structure CursorOutput where
  preCursor : CursorWire
  postCursor : CursorWire
  post : StateWire
  checkpointSucceeds : Option Bool
  acknowledgmentAllowed : Bool
  pending : List ArrivalWire
  journalAfter : List ArrivalWire
  deriving Lean.ToJson

def cursorOutput (i : CursorInput) : CursorOutput :=
  let source := i.source.map (·.toModel)
  let journal : EventDelivery.Durable.Journal := { head := i.journalHead.toNat!, entries := source }
  let first := EventDelivery.Durable.seed {} i.seedHead.toNat!
  let saved := match i.priorAfter with
    | none => first
    | some position => { first with after := position.toNat! }
  let before := if i.restart then EventDelivery.Durable.seed saved journal.head else saved
  let committed := i.pre.toModel
  let afterState := match i.admissionCommit with
    | none => committed
    | some commit => admitTransaction committed i.fire.toModel commit
  let eligible := fun entry : EventDelivery.Durable.Arrival =>
    (i.sourceEligible.getD (entry.position - 1) false)
  let mode := modeOfWire i.mode
  let position := i.entry.position.toNat!
  let accepted := EventDelivery.Durable.checkpointAccepted before afterState journal
    position mode i.busy eligible
  let afterCursor := match i.checkpointCommit with
    | none => before
    | some commit => EventDelivery.Durable.acknowledge before afterState journal
        position mode i.busy eligible commit
  { preCursor := .ofModel before, postCursor := .ofModel afterCursor, post := .ofModel afterState,
    checkpointSucceeds := i.checkpointCommit.map accepted,
    acknowledgmentAllowed := accepted true,
    pending := (EventDelivery.Durable.pending afterCursor source i.enabled).map .ofModel,
    journalAfter := (EventDelivery.Durable.pending afterCursor source true).map .ofModel }

def cursorScenarios : List CursorScenario :=
  [({ name := "first_seed_excludes_existing", seedHead := 3 } : CursorScenario),
   { name := "first_seed_disabled_retains_later_arrivals", enabled := false },
   { name := "reenabled_delivers_receiving_order" },
   { name := "restart_does_not_reseed", restart := true },
   { name := "crash_before_fire_commit", admissionCommit := some false, checkpointCommit := some true },
   { name := "crash_after_fire_commit_before_checkpoint", admissionCommit := some true },
   { name := "checkpoint_transaction_crashes", admissionCommit := some true, checkpointCommit := some false },
   { name := "replay_after_receipt_commit", committed := admit {} (fire "b"), admissionCommit := some true, checkpointCommit := some true },
   { name := "checkpoint_commits", admissionCommit := some true, checkpointCommit := some true },
   { name := "replay_after_checkpoint_commit", priorAfter := some 2, committed := admit {} (fire "b"), admissionCommit := some true, checkpointCommit := some true },
   { name := "unmatched_filter_checkpoint", matchesFilter := false, checkpointCommit := some true },
   { name := "stale_checkpoint_keeps_progress", priorAfter := some 3, committed := admit {} (fire "b"), checkpointCommit := some true },
   { name := "unadmitted_match_cannot_checkpoint", checkpointCommit := some true },
   { name := "later_receipt_cannot_skip_gap", entry := arrival 3 "c", admissionCommit := some true, checkpointCommit := some true },
   { name := "complete_prefix_checkpoint", entry := arrival 3 "c", committed := admit {} (fire "b"), admissionCommit := some true, checkpointCommit := some true },
   { name := "excluded_earlier_arrival_allows_prefix", entry := arrival 3 "c", excludedDocuments := ["b"], admissionCommit := some true, checkpointCommit := some true },
   { name := "legacy_serial_busy_exclusion", mode := .serial, busy := true, checkpointCommit := some true },
   { name := "queued_serial_busy_requires_receipt", busy := true, checkpointCommit := some true },
   { name := "latest_only_busy_requires_receipt", mode := .latestOnly, busy := true, checkpointCommit := some true },
   { name := "latest_only_later_receipt_cannot_skip_gap", mode := .latestOnly, entry := arrival 3 "c", admissionCommit := some true, checkpointCommit := some true }]

@[conformance_family]
def arrivalCursor : Family CursorInput CursorOutput where
  inputs := cursorScenarios.map cursorInput
  model := cursorOutput

/-- An unadmitted matching arrival keeps the cursor, stays in the journal and
leaves no receipt. -/
example :
    let o := cursorOutput (cursorInput { name := "unadmitted_match_cannot_checkpoint", checkpointCommit := some true })
    o.postCursor.after = o.preCursor.after ∧ o.postCursor.seeded = o.preCursor.seeded ∧
      o.journalAfter.any (fun entry => entry.position == "2" && entry.identity.sourceDocId == "b") ∧
      o.post.receipts.isEmpty := by
  native_decide

structure CallbackCursorScenario where
  name : String
  seedHead : Nat := 1
  priorAfter : Option Nat := none
  restart : Bool := false
  bindingEnabled : Bool := true
  callbackEnabled : Bool := true
  committed : List String := []
  entry : EventDelivery.Durable.Arrival := arrival 2 "b"
  matchesFilter : Bool := true
  admissionCommit : Option Bool := none
  editAfterAdmission : Bool := false
  checkpointCommit : Option Bool := none

structure CallbackCursorInput where
  name : String
  seedHead : String
  priorAfter : Option String
  restart : Bool
  bindingEnabled : Bool
  callbackEnabled : Bool
  preReceipts : List IdentityWire
  source : List ArrivalWire
  entry : ArrivalWire
  matchesFilter : Bool
  admissionCommit : Option Bool
  editAfterAdmission : Bool
  checkpointCommit : Option Bool
  deriving Lean.ToJson

def callbackAdmission (id : Identity) (version : String) : EventDelivery.Durable.CallbackAdmission :=
  { identity := id, version }

def callbackCursorInput (scenario : CallbackCursorScenario) : CallbackCursorInput :=
  let pre := scenario.committed.foldl
    (fun state document => EventDelivery.Durable.admitCallback state
      (callbackAdmission (identity "owner-a" document) "v1")) {}
  { name := scenario.name, seedHead := toString scenario.seedHead,
    priorAfter := scenario.priorAfter.map toString, restart := scenario.restart,
    bindingEnabled := scenario.bindingEnabled, callbackEnabled := scenario.callbackEnabled,
    preReceipts := pre.receipts.map .ofModel, source := cursorSource.map .ofModel,
    entry := .ofModel scenario.entry, matchesFilter := scenario.matchesFilter,
    admissionCommit := scenario.admissionCommit, editAfterAdmission := scenario.editAfterAdmission,
    checkpointCommit := scenario.checkpointCommit }

structure CallbackCursorOutput where
  preCursor : CursorWire
  postCursor : CursorWire
  postReceipts : List IdentityWire
  checkpointSucceeds : Option Bool
  journalAfter : List ArrivalWire
  deriving Lean.ToJson

def callbackCursorOutput (i : CallbackCursorInput) : CallbackCursorOutput :=
  let source := i.source.map (·.toModel)
  let journal : EventDelivery.Durable.Journal := { head := 3, entries := source }
  let consumer := EventDelivery.Durable.Consumer.callbackBinding i.bindingEnabled i.callbackEnabled
  let pre := i.preReceipts.foldl
    (fun state id => EventDelivery.Durable.admitCallback state (callbackAdmission id.toModel "v1")) {}
  let first := EventDelivery.Durable.seed {} i.seedHead.toNat!
  let saved := match i.priorAfter with
    | none => first
    | some position => { first with after := position.toNat! }
  let before := if i.restart then EventDelivery.Durable.seed saved journal.head else saved
  let entry := i.entry.toModel
  let admitted := match i.admissionCommit with
    | some true => EventDelivery.Durable.admitCallback pre (callbackAdmission entry.identity "v1")
    | _ => pre
  let afterState := if i.editAfterAdmission
    then EventDelivery.Durable.admitCallback admitted (callbackAdmission entry.identity "v2")
    else admitted
  let eligible := consumer.eligible fun _ => i.matchesFilter
  let accepted := EventDelivery.Durable.checkpointAccepted before afterState journal
    entry.position consumer.mode false eligible
  let afterCursor := match i.checkpointCommit with
    | none => before
    | some commit => EventDelivery.Durable.acknowledge before afterState journal
        entry.position consumer.mode false eligible commit
  { preCursor := .ofModel before, postCursor := .ofModel afterCursor,
    postReceipts := afterState.receipts.map .ofModel,
    checkpointSucceeds := i.checkpointCommit.map accepted,
    journalAfter := (EventDelivery.Durable.pending afterCursor source true).map .ofModel }

def callbackCursorScenarios : List CallbackCursorScenario :=
  [({ name := "callback_first_seed_excludes_existing", seedHead := 3 } : CallbackCursorScenario),
   { name := "callback_restart_does_not_reseed", restart := true },
   { name := "callback_crash_before_invocation_commit", admissionCommit := some false, checkpointCommit := some true },
   { name := "callback_crash_after_invocation_before_checkpoint", admissionCommit := some true },
   { name := "callback_checkpoint_commits", admissionCommit := some true, checkpointCommit := some true },
   { name := "callback_checkpoint_transaction_crashes", admissionCommit := some true, checkpointCommit := some false },
   { name := "callback_edit_after_admission_keeps_one_receipt", admissionCommit := some true, editAfterAdmission := true, checkpointCommit := some true },
   { name := "callback_unmatched_filter_checkpoint", matchesFilter := false, checkpointCommit := some true },
   { name := "callback_unadmitted_match_cannot_checkpoint", checkpointCommit := some true },
   { name := "callback_later_receipt_cannot_skip_gap", entry := arrival 3 "c", admissionCommit := some true, checkpointCommit := some true },
   { name := "callback_complete_prefix_checkpoint", entry := arrival 3 "c", committed := ["b"], admissionCommit := some true, checkpointCommit := some true },
   { name := "callback_disabled_binding_holds_unmatched", bindingEnabled := false, matchesFilter := false, checkpointCommit := some true },
   { name := "callback_disabled_callback_holds_match", callbackEnabled := false, checkpointCommit := some true },
   { name := "callback_disabled_passes_receipts", bindingEnabled := false, committed := ["b"], checkpointCommit := some true }]

@[conformance_family]
def callbackCursor : Family CallbackCursorInput CallbackCursorOutput where
  inputs := callbackCursorScenarios.map callbackCursorInput
  model := callbackCursorOutput


structure FireIdentityInput where
  identity : IdentityWire
  deriving Lean.ToJson

structure FireIdentityKeys where
  key : String
  requestId : String
  sessionId : String
  outcomeId : String
  deriving Lean.ToJson

@[conformance_family]
def fireIdentity : Family FireIdentityInput FireIdentityKeys where
  inputs := [identity "owner-a" "a", identity "owner-b" "a", identity "a:b" "é",
    { identity "a" "b:é" with trigger := "b:handoff" }].map fun id =>
      { identity := IdentityWire.ofModel id }
  model := fun i =>
    let id := i.identity.toModel
    { key := id.key, requestId := id.requestId, sessionId := id.sessionId,
      outcomeId := id.outcomeId }

def sessionCases : List String :=
  let id := identity "owner-a" "a"
  [(none, true, false), (some "lead-one", true, false), (some "lead-two", true, false),
   (some "", true, false), (some "foreign", false, false),
   (some "game:a:red", false, true), (some "game:a:red", true, true),
   (some "", false, true)].map fun (target, owned, create) => object [
     ("identity", identityJson id), ("target", jsonOptionalString target),
     ("owned", toString owned), ("create_missing", toString create),
     ("resolved", jsonOptionalString (resolveSession id target owned create))]

def sessionConfigCases : List String :=
  [(none, false), (none, true), (some "game:{{ doc.id }}", false),
   (some "game:{{ doc.id }}", true)].map fun (template, create) => object [
     ("template", jsonOptionalString template), ("create_session", toString create),
     ("allowed", toString (Triggers.SessionKey.configValid template create))]

structure BudgetScenario where
  name : String
  configured : Option Nat := none
  priorAges : List Nat := []
  replay : Bool := false

def jsonOptionNat : Option Nat → String
  | none => "null"
  | some value => toString value

/-- Ages are seconds before the fire; the window boundary is exercised in both
directions so the receipt query's strictness is checked against the model. -/
def budgetCase (scenario : BudgetScenario) : String :=
  let now := 7200
  let times := scenario.priorAges.map (now - ·)
  let trigger : Triggers.FireBudget.Trigger := { admittedAt := times }
  let f := fire "budget-fire"
  let before : State := if scenario.replay then admit {} f else {}
  let (after, stopped) := Triggers.FireBudget.admitBudgeted before trigger scenario.configured
    "over budget" now f
  object [
    ("name", jsonString scenario.name), ("configured", jsonOptionNat scenario.configured),
    ("prior_ages", jsonArray (scenario.priorAges.map toString)),
    ("replay", toString scenario.replay),
    ("budget", toString (Triggers.FireBudget.effective scenario.configured)),
    ("window_count", toString (Triggers.FireBudget.windowCount times now)),
    ("admitted", toString (decide (after.receipts.length > before.receipts.length))),
    ("stopped", toString (!stopped.enabled)),
    ("status", jsonOptionalString stopped.lastStatus)]

def budgetCases : List String := [
  ({ name := "empty_window_admits", configured := some 2 } : BudgetScenario),
  { name := "under_budget_admits", configured := some 2, priorAges := [10] },
  { name := "at_budget_stops", configured := some 2, priorAges := [10, 20] },
  { name := "boundary_receipt_is_outside_window", configured := some 2, priorAges := [10, 3600] },
  { name := "inside_boundary_counts", configured := some 2, priorAges := [10, 3599] },
  { name := "expired_receipts_do_not_count", configured := some 2, priorAges := [7200, 3601] },
  { name := "default_budget_applies_without_configuration", priorAges := [10] },
  { name := "replay_spends_no_budget", configured := some 1, priorAges := [10], replay := true }
].map budgetCase

def budgetConfigCases : List String :=
  [(none, false), (none, true), (some 1, true), (some 1, false), (some 5000, true),
   (some 0, true)].map fun (configured, receipted) =>
    object [("configured", jsonOptionNat configured), ("receipted", toString receipted),
      ("allowed", toString (Triggers.FireBudget.configValid configured receipted))]

def observedClaimCases : List String :=
  let old : ClaimObservation := { document := "old", owner := "owner", session := "session" }
  let next : ClaimObservation := { old with document := "new", arrival := some 1, receipt := true }
  let cases : List (String × ClaimObservation × List ClaimObservation) := [
    ("historical_pending_unordered", old, [{ old with document := "older" }]),
    ("historical_running_blocks", old, [{ old with document := "older", running := true }]),
    ("historical_precedes_journal", next, [old]),
    ("journal_does_not_precede_historical", old, [next]),
    ("journal_running_blocks_historical", old, [{ next with running := true }]),
    ("unrelated_historical_does_not_block", next, [{ old with session := "elsewhere" }]),
    ("foreign_owner_does_not_block", next, [{ old with owner := "other" }]),
    ("receipt_missing_position_rejected", { next with arrival := none }, []),
    ("receipt_conflict_missing_position_rejected", old, [{ next with arrival := none }]),
    ("native_pending_fifo", { next with arrival := some 2 }, [{ next with document := "prior" }]),
    ("terminal_historical_does_not_block", next, [{ old with terminal := true }])]
  cases.map fun (name, candidate, rows) => object [
    ("name", jsonString name), ("candidate", claimObservationJson candidate),
    ("rows", jsonArray (rows.map claimObservationJson)),
    ("allowed", toString (observedClaimAllowed candidate rows))]

structure AssignmentRootInput where
  assigned : Option String
  observed : Option String
  deriving Lean.ToJson

@[conformance_family]
def assignmentRoot : Family AssignmentRootInput Bool where
  inputs := [(none, none), (none, some "ordinary"), (some "new", none),
    (some "new", some "new"), (some "new", some "old")].map fun (assigned, observed) =>
      { assigned, observed }
  model := fun i => Goals.assignmentAllows i.assigned i.observed

def selfCases : List String :=
  [("owner", "first", "owner", "first"),
   ("owner", "first", "owner", "second"),
   ("owner", "first", "foreign", "first")].map fun (co, cs, lo, ls) => object [
    ("caller_owner", jsonString co), ("caller_session", jsonString cs),
    ("listed_owner", jsonString lo), ("listed_session", jsonString ls),
    ("current", toString (isCurrent co cs lo ls))]

def deliveryOptionCases : List String := do
  let kind ← [.manual, .schedule, .event]
  let emit ← [false, true]
  let concurrency ← [.parallel, .serial, .queuedSerial, .latestOnly]
  let target ← [false, true]
  pure <| object [
    ("kind", jsonString kind.toDefraDB), ("emit_outcome", toString emit),
    ("concurrency", jsonString (match concurrency with
      | .parallel => "parallel" | .serial => "serial"
      | .queuedSerial => "queued_serial" | .latestOnly => "latest_only")),
    ("target_existing", toString target),
    ("allowed", toString (Triggers.deliveryOptionsAllowed kind emit concurrency target))]

structure LatestAdmissionInput where
  name : String
  pre : StateWire
  fire : FireWire
  destinationAllowed : Bool
  deriving Lean.ToJson

structure LatestAdmissionOutput where
  post : StateWire
  deriving Lean.ToJson

/-- A latest-only Trigger's fires are not queued-serial: only `queued_serial`
concurrency marks a receipt serial. -/
@[conformance_family]
def latestAdmission : Family LatestAdmissionInput LatestAdmissionOutput where
  inputs :=
    let old := { fire "old" with serial := false, emitOutcome := false, identity := { identity "owner-a" "old" with collection := "Schedule" } }
    let next := { old with identity := { old.identity with document := "next" } }
    let pre := admit {} old
    [("invalid_target_preserves_pending", pre, false),
     ("valid_target_replaces_pending", pre, true),
     ("duplicate_preserves_later_work", admit (admit pre next)
        { old with identity := { old.identity with document := "later" } }, true)].map
      fun (name, before, destinationAllowed) =>
        { name, pre := .ofModel before, fire := .ofModel next, destinationAllowed }
  model := fun i =>
    { post := .ofModel (admitLatest i.pre.toModel i.fire.toModel i.destinationAllowed) }

def triggerDeliveryObservedClaimCasesJson : String :=
  jsonArray observedClaimCases

def triggerDeliveryOutcomeTraceCasesJson : String :=
  jsonArray outcomeTraces

def triggerDeliveryRetainedOutcomeCasesJson : String :=
  jsonArray retainedOutcomeCases

def triggerDeliveryCallbackVisibilityCasesJson : String :=
  jsonArray callbackVisibilityCases

def triggerDeliverySessionCasesJson : String :=
  jsonArray sessionCases

def triggerDeliverySessionConfigCasesJson : String :=
  jsonArray sessionConfigCases

def triggerFireBudgetCasesJson : String :=
  jsonArray budgetCases

def triggerFireBudgetConfigCasesJson : String :=
  jsonArray budgetConfigCases

def triggerFireBudgetDefaultJson : String :=
  toString Triggers.FireBudget.defaultMaxPerHour

def triggerFireBudgetStatusJson : String :=
  jsonString Triggers.FireBudget.exhaustedStatus

def triggerDeliveryOptionCasesJson : String :=
  jsonArray deliveryOptionCases

def triggerDeliverySelfSessionCasesJson : String :=
  jsonArray selfCases

example : (admit (admit {} (fire "a")) (fire "a")).requests.length = 1 := by native_decide
example : outcomeDue {} { fire := { fire "a" with goalBacked := true }, terminal := false } = false := by
  native_decide

end Conformance.TriggerDelivery
