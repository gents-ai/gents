import Proofs.Automation.Triggers.Durable
import Proofs.Automation.EventDelivery.Durable
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family
import Proofs.Automation.Triggers.SessionKey
import Proofs.Automation.Triggers.FireBudget
import Proofs.Conformance.Family
import Proofs.Conformance.GoalRequestAssignment

deriving instance Lean.ToJson for Triggers.Durable.ClaimObservation

namespace Conformance.TriggerDelivery

open Triggers.Durable Conformance.Contracts

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

def admissionCase (name : String) (pre : State) (f : Fire) (commit : Bool) : String :=
  object [("name", jsonString name), ("pre", stateJson pre), ("fire", fireJson f),
    ("commit", toString commit),
    ("source_allowed", toString (Triggers.outcomeSourceAllowed f.identity.collection f.emitOutcome)),
    ("post", stateJson (admitTransaction pre f commit))]

def admissionCases : List String := [
  admissionCase "outcome_consumer_cannot_opt_in" {}
    { fire "outcome" with identity := { (fire "outcome").identity with collection := "FireOutcome" } } true,
  admissionCase "outcome_consumer_opted_out_admitted" {}
    { fire "outcome" with emitOutcome := false, identity := { (fire "outcome").identity with collection := "FireOutcome" } } true,
  admissionCase "crash_before_fire_commit" {} (fire "a") false,
  admissionCase "fire_commit" {} (fire "a") true,
  admissionCase "crash_after_fire_commit_retry" (admit {} (fire "a")) (fire "a") true,
  admissionCase "same_trigger_under_two_owners" (admit {} (fire "a"))
    { fire "a" with identity := identity "owner-b" "a" } true,
  admissionCase "same_document_other_collection" (admit {} (fire "a"))
    { fire "a" with identity := { identity "owner-a" "a" with collection := "Other" } } true,
  admissionCase "serial_burst_is_persisted" (claim (admit {} (fire "a")) (fire "a").identity)
    { fire "b" with session := "other-session" } true]

def claimObservationJson (row : ClaimObservation) : String := object [
  ("document", jsonString row.document), ("owner", jsonString row.owner),
  ("session", jsonString row.session), ("trigger", jsonString row.trigger),
  ("serial", toString row.serial), ("receipt", toString row.receipt),
  ("arrival", row.arrival.map toString |>.getD "null"),
  ("running", toString row.running), ("terminal", toString row.terminal)]

def queueCase (name : String) (pre : State) (id : Identity) : String :=
  object [("name", jsonString name), ("pre", stateJson pre),
    ("identity", identityJson id),
    ("observations", jsonArray ((requestObservations pre).map claimObservationJson)),
    ("can_claim", toString (canClaim pre id))]

def queueCases : List String :=
  let a := fire "a"
  let b := { fire "b" with session := "other-session" }
  let queued := admit (admit {} a) b
  let busy := claim queued a.identity
  let sameSession := { b with identity := { b.identity with trigger := "other-trigger" }, session := a.session, serial := false }
  [queueCase "serial_first_claims" queued a.identity,
   queueCase "serial_second_waits" queued b.identity,
   queueCase "serial_running_blocks_next_session" busy b.identity,
   queueCase "serial_terminal_releases_next" (terminalize busy a.identity) b.identity,
   queueCase "busy_session_queues_another_trigger" (admit busy sameSession) sameSession.identity,
   queueCase "owner_is_part_of_queue_scope"
     (admit busy { b with identity := identity "owner-b" "b" }) (identity "owner-b" "b")]

def reasonString : OutcomeReason → String
  | .requestTerminal => "request_terminal"
  | .goalTerminal status => status.toDefraDB
  | .superseded => "superseded"

def outcomeCase (name : String) (r : Request) (status : Option Goals.Status) (already : Bool) : String :=
  let bindings := status.toList.map fun value => ({
    owner := r.fire.identity.owner
    session := r.fire.session
    assignment := r.fire.identity
    status := value } : GoalBinding)
  let pre : State := {
    receipts := [r.fire.identity]
    requests := [r]
    outcomes := if already then [r.fire.identity] else []
    goals := bindings }
  object [("name", jsonString name), ("pre", stateJson pre),
    ("due", toString (outcomeDue pre r)),
    ("reason", jsonOptionalString ((outcomeReason pre r).map reasonString)),
    ("post", stateJson (recoverOutcomes pre))]

def outcomeCases : List String :=
  let ordinary : Request := { fire := fire "a", terminal := true }
  let goal : Request := { ordinary with fire := { ordinary.fire with goalBacked := true }, goalAssignmentApplied := true }
  [outcomeCase "ordinary_terminal_outcome" ordinary none false,
   outcomeCase "crash_after_terminal_before_outcome" ordinary none false,
   outcomeCase "crash_after_outcome_retry" ordinary none true,
   outcomeCase "opted_out" { ordinary with fire := { ordinary.fire with emitOutcome := false } } none false,
   outcomeCase "outcome_consumer_ends_chain"
     { ordinary with fire := { ordinary.fire with emitOutcome := false, identity := { ordinary.fire.identity with collection := "FireOutcome" } } } none false,
   outcomeCase "continuing_goal_has_no_outcome" goal (some .active) false,
   outcomeCase "queued_goal_ignores_previous_complete" { goal with terminal := false, goalAssignmentApplied := false } (some .complete) false,
   outcomeCase "preclaim_cancelled_goal" { goal with goalAssignmentApplied := false } none false,
   outcomeCase "replaced_applied_goal" { goal with assignmentReplaced := true } none false,
   outcomeCase "completed_goal" goal (some .complete) false,
   outcomeCase "stopped_goal" goal (some .stopped) false,
   outcomeCase "blocked_goal" goal (some .blocked) false,
   outcomeCase "budget_exhausted_goal" goal (some .budgetLimited) false,
   outcomeCase "paused_goal" goal (some .paused) false,
   outcomeCase "usage_limited_goal" goal (some .usageLimited) false]

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

def arrivalJson (entry : EventDelivery.Durable.Arrival) : String := object [
  ("position", jsonString (toString entry.position)), ("identity", identityJson entry.identity)]

def cursorJson (cursor : EventDelivery.Durable.Cursor) : String := object [
  ("seeded", toString cursor.seeded), ("after", jsonString (toString cursor.after))]

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

def cursorCase (scenario : CursorScenario) : String :=
  let source := [arrival 1 "a", arrival 2 "b", arrival 3 "c"]
  let journal : EventDelivery.Durable.Journal := { head := 3, entries := source }
  let first := EventDelivery.Durable.seed {} scenario.seedHead
  let saved := match scenario.priorAfter with
    | none => first
    | some position => { first with after := position }
  let before := if scenario.restart then EventDelivery.Durable.seed saved journal.head else saved
  let f := { fire scenario.entry.identity.document with identity := scenario.entry.identity }
  let afterState := match scenario.admissionCommit with
    | none => scenario.committed
    | some commit => admitTransaction scenario.committed f commit
  let eligible := fun entry : EventDelivery.Durable.Arrival =>
    scenario.matchesFilter && !scenario.excludedDocuments.contains entry.identity.document
  let accepted := EventDelivery.Durable.checkpointAccepted before afterState journal
    scenario.entry.position scenario.mode scenario.busy eligible
  let afterCursor := match scenario.checkpointCommit with
    | none => before
    | some commit => EventDelivery.Durable.acknowledge before afterState journal
        scenario.entry.position scenario.mode scenario.busy eligible commit
  let optionalBool := fun value : Option Bool => match value with
    | none => "null"
    | some value => toString value
  let mode := match scenario.mode with
    | .serial => "serial"
    | .queuedSerial => "queued_serial"
    | .parallel => "parallel"
    | .latestOnly => "latest_only"
  object [
    ("name", jsonString scenario.name), ("seed_head", jsonString (toString scenario.seedHead)),
    ("restart", toString scenario.restart), ("enabled", toString scenario.enabled),
    ("pre_cursor", cursorJson before), ("post_cursor", cursorJson afterCursor),
    ("pre", stateJson scenario.committed), ("post", stateJson afterState),
    ("source", jsonArray (source.map arrivalJson)), ("entry", arrivalJson scenario.entry),
    ("source_eligible", jsonArray (source.map fun entry => toString (eligible entry))),
    ("journal_head", jsonString (toString journal.head)),
    ("mode", jsonString mode), ("busy", toString scenario.busy),
    ("fire", fireJson f), ("matches_filter", toString scenario.matchesFilter),
    ("admission_commit", optionalBool scenario.admissionCommit),
    ("checkpoint_commit", optionalBool scenario.checkpointCommit),
    ("checkpoint_succeeds", optionalBool (scenario.checkpointCommit.map accepted)),
    ("acknowledgment_allowed", toString (accepted true)),
    ("pending", jsonArray ((EventDelivery.Durable.pending afterCursor source scenario.enabled).map arrivalJson)),
    ("journal_after", jsonArray ((EventDelivery.Durable.pending afterCursor source true).map arrivalJson))]

def cursorCases : List String :=
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
   { name := "latest_only_later_receipt_cannot_skip_gap", mode := .latestOnly, entry := arrival 3 "c", admissionCommit := some true, checkpointCommit := some true }].map cursorCase




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

def callbackCursorCase (scenario : CallbackCursorScenario) : String :=
  let source := [arrival 1 "a", arrival 2 "b", arrival 3 "c"]
  let journal : EventDelivery.Durable.Journal := { head := 3, entries := source }
  let consumer := EventDelivery.Durable.Consumer.callbackBinding
    scenario.bindingEnabled scenario.callbackEnabled
  let admission := fun (document version : String) =>
    ({ identity := identity "owner-a" document, version } : EventDelivery.Durable.CallbackAdmission)
  let pre := scenario.committed.foldl
    (fun state document => EventDelivery.Durable.admitCallback state (admission document "v1")) {}
  let first := EventDelivery.Durable.seed {} scenario.seedHead
  let saved := match scenario.priorAfter with
    | none => first
    | some position => { first with after := position }
  let before := if scenario.restart then EventDelivery.Durable.seed saved journal.head else saved
  let document := scenario.entry.identity.document
  let admitted := match scenario.admissionCommit with
    | some true => EventDelivery.Durable.admitCallback pre (admission document "v1")
    | _ => pre
  let afterState := if scenario.editAfterAdmission
    then EventDelivery.Durable.admitCallback admitted (admission document "v2") else admitted
  let filterEligible := fun _ : EventDelivery.Durable.Arrival => scenario.matchesFilter
  let eligible := consumer.eligible filterEligible
  let accepted := EventDelivery.Durable.checkpointAccepted before afterState journal
    scenario.entry.position consumer.mode false eligible
  let afterCursor := match scenario.checkpointCommit with
    | none => before
    | some commit => EventDelivery.Durable.acknowledge before afterState journal
        scenario.entry.position consumer.mode false eligible commit
  let optionalBool := fun value : Option Bool => match value with
    | none => "null"
    | some value => toString value
  object [
    ("name", jsonString scenario.name), ("seed_head", jsonString (toString scenario.seedHead)),
    ("restart", toString scenario.restart),
    ("binding_enabled", toString scenario.bindingEnabled),
    ("callback_enabled", toString scenario.callbackEnabled),
    ("pre_cursor", cursorJson before), ("post_cursor", cursorJson afterCursor),
    ("pre_receipts", jsonArray (pre.receipts.map identityJson)),
    ("post_receipts", jsonArray (afterState.receipts.map identityJson)),
    ("source", jsonArray (source.map arrivalJson)), ("entry", arrivalJson scenario.entry),
    ("matches_filter", toString scenario.matchesFilter),
    ("admission_commit", optionalBool scenario.admissionCommit),
    ("edit_after_admission", toString scenario.editAfterAdmission),
    ("checkpoint_commit", optionalBool scenario.checkpointCommit),
    ("checkpoint_succeeds", optionalBool (scenario.checkpointCommit.map accepted)),
    ("journal_after", jsonArray ((EventDelivery.Durable.pending afterCursor source true).map arrivalJson))]

def callbackCursorCases : List String :=
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
   { name := "callback_disabled_passes_receipts", bindingEnabled := false, committed := ["b"], checkpointCommit := some true }].map callbackCursorCase

def identityCases : List String :=
  [identity "owner-a" "a", identity "owner-b" "a", identity "a:b" "é",
   { identity "a" "b:é" with trigger := "b:handoff" }].map fun id => object [
    ("identity", identityJson id), ("key", jsonString id.key),
    ("request_id", jsonString id.requestId), ("session_id", jsonString id.sessionId),
    ("outcome_id", jsonString id.outcomeId)]

structure SessionConfigInput where
  template : Option String
  createSession : Bool
  deriving Lean.ToJson

structure SessionConfigOutput where
  allowed : Bool
  deriving Lean.ToJson

@[conformance_family]
def sessionConfig : Family SessionConfigInput SessionConfigOutput where
  inputs := [
    { template := none, createSession := false },
    { template := none, createSession := true },
    { template := some "game:{{ doc.id }}", createSession := false },
    { template := some "game:{{ doc.id }}", createSession := true }]
  model := fun i => { allowed := Triggers.SessionKey.configValid i.template i.createSession }

structure FireBudgetInput where
  name : String
  configured : Option Nat
  priorAges : List Nat
  replay : Bool
  deriving Lean.ToJson

structure FireBudgetOutput where
  budget : Nat
  admitted : Bool
  exhausted : Bool
  stopped : Bool
  status : Option String
  deriving Lean.ToJson

structure FireBudgetConfigInput where
  configured : Option Nat
  receipted : Bool
  deriving Lean.ToJson

structure NoInput where
  deriving Lean.ToJson

/-- Ages are seconds before the fire; the window boundary is exercised in both
directions so the receipt query's strictness is checked against the model. -/
def fireBudgetOutcome (scenario : FireBudgetInput) : FireBudgetOutput :=
  let now := 7200
  let times := scenario.priorAges.map (now - ·)
  let trigger : Triggers.FireBudget.Trigger := { admittedAt := times }
  let f := fire "budget-fire"
  let before : State := if scenario.replay then admit {} f else {}
  let (after, afterTrigger) := Triggers.FireBudget.admitBudgeted before trigger scenario.configured
    "over budget" now f
  { budget := Triggers.FireBudget.effective scenario.configured
    admitted := decide (after.receipts.length > before.receipts.length)
    exhausted := !scenario.replay && decide (Triggers.FireBudget.decideFire
      (Triggers.FireBudget.effective scenario.configured)
      (Triggers.FireBudget.windowCount times now) = .exhausted)
    stopped := !afterTrigger.enabled
    status := afterTrigger.lastStatus }

@[conformance_family]
def fireBudgetAdmission : Family FireBudgetInput FireBudgetOutput where
  inputs := [
    { name := "empty_window_admits", configured := some 2, priorAges := [], replay := false },
    { name := "under_budget_admits", configured := some 2, priorAges := [10], replay := false },
    { name := "at_budget_stops", configured := some 2, priorAges := [10, 20], replay := false },
    { name := "boundary_receipt_is_outside_window", configured := some 2, priorAges := [10, 3600], replay := false },
    { name := "inside_boundary_counts", configured := some 2, priorAges := [10, 3599], replay := false },
    { name := "expired_receipts_do_not_count", configured := some 2, priorAges := [7200, 3601], replay := false },
    { name := "default_budget_applies_without_configuration", configured := none, priorAges := [10], replay := false },
    { name := "replay_spends_no_budget", configured := some 1, priorAges := [10], replay := true }]
  model := fireBudgetOutcome

@[conformance_family]
def fireBudgetConfig : Family FireBudgetConfigInput Bool where
  inputs := [
    { configured := none, receipted := false }, { configured := none, receipted := true },
    { configured := some 1, receipted := true }, { configured := some 1, receipted := false },
    { configured := some 5000, receipted := true }, { configured := some 0, receipted := true }]
  model := fun i => Triggers.FireBudget.configValid i.configured i.receipted

@[conformance_family]
def fireBudgetDefault : Family NoInput Nat where
  inputs := [{}]
  model := fun _ => Triggers.FireBudget.defaultMaxPerHour

@[conformance_family]
def fireBudgetStatus : Family NoInput String where
  inputs := [{}]
  model := fun _ => Triggers.FireBudget.exhaustedStatus

structure ObservedClaimInput where
  name : String
  candidate : ClaimObservation
  rows : List ClaimObservation
  deriving Lean.ToJson

def observedClaimCases : List ObservedClaimInput :=
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
  cases.map fun (name, candidate, rows) => ({ name, candidate, rows } : ObservedClaimInput)

@[conformance_family]
def observedClaim : Family ObservedClaimInput Bool where
  inputs := observedClaimCases
  model := fun i => observedClaimAllowed i.candidate i.rows

def assignmentRootCases : List String :=
  [(none, none), (none, some "ordinary"), (some "new", none),
   (some "new", some "new"), (some "new", some "old")].map fun (assigned, observed) => object [
    ("assigned", jsonOptionalString assigned), ("observed", jsonOptionalString observed),
    ("allowed", toString (Goals.assignmentAllows assigned observed))]

structure SelfSessionInput where
  callerOwner : String
  callerSession : String
  listedOwner : String
  listedSession : String
  deriving Lean.ToJson

@[conformance_family]
def selfSession : Family SelfSessionInput Bool where
  inputs :=
    [("owner", "first", "owner", "first"), ("owner", "first", "owner", "second"),
     ("owner", "first", "foreign", "first")].map fun (callerOwner, callerSession, listedOwner, listedSession) =>
      { callerOwner, callerSession, listedOwner, listedSession }
  model := fun i => isCurrent i.callerOwner i.callerSession i.listedOwner i.listedSession

deriving instance Lean.ToJson for TriggerKind, ConcurrencyMode

structure DeliveryOptionInput where
  kind : TriggerKind
  emitOutcome : Bool
  concurrency : ConcurrencyMode
  targetExisting : Bool
  deriving Lean.ToJson

@[conformance_family]
def deliveryOptions : Family DeliveryOptionInput Bool where
  inputs := [TriggerKind.manual, .schedule, .event].flatMap fun kind =>
    [false, true].flatMap fun emitOutcome =>
    [ConcurrencyMode.parallel, .serial, .queuedSerial, .latestOnly].flatMap fun concurrency =>
    [false, true].map fun targetExisting => { kind, emitOutcome, concurrency, targetExisting }
  model := fun i => Triggers.deliveryOptionsAllowed i.kind i.emitOutcome i.concurrency i.targetExisting

def latestAdmissionCases : List String :=
  let old := { fire "old" with emitOutcome := false, identity := { identity "owner-a" "old" with collection := "Schedule" } }
  let next := { old with identity := { old.identity with document := "next" } }
  let pre := admit {} old
  [("invalid_target_preserves_pending", pre, false),
   ("valid_target_replaces_pending", pre, true),
   ("duplicate_preserves_later_work", admit (admit pre next)
      { old with identity := { old.identity with document := "later" } }, true)].map fun (name, before, allowed) =>
    object [("name", jsonString name), ("pre", stateJson before),
      ("fire", fireJson next), ("destination_allowed", toString allowed),
      ("post", stateJson (admitLatest before next allowed))]

def triggerDeliveryAdmissionCasesJson : String :=
  jsonArray admissionCases

def triggerDeliveryQueueCasesJson : String :=
  jsonArray queueCases

def triggerDeliveryAssignmentRootCasesJson : String :=
  jsonArray assignmentRootCases

def triggerDeliveryOutcomeCasesJson : String :=
  jsonArray outcomeCases

def triggerDeliveryOutcomeTraceCasesJson : String :=
  jsonArray outcomeTraces

def triggerDeliveryCursorCasesJson : String :=
  jsonArray cursorCases

def triggerDeliveryCallbackCursorCasesJson : String :=
  jsonArray callbackCursorCases

def triggerDeliveryCallbackVisibilityCasesJson : String :=
  jsonArray callbackVisibilityCases

def triggerDeliveryIdentityCasesJson : String :=
  jsonArray identityCases

def triggerDeliveryLatestAdmissionCasesJson : String :=
  jsonArray latestAdmissionCases

example : (admit (admit {} (fire "a")) (fire "a")).requests.length = 1 := by native_decide
example : outcomeDue {} { fire := { fire "a" with goalBacked := true }, terminal := false } = false := by
  native_decide

structure SessionIdentity where
  ownerDid : String
  triggerId : String
  sourceCollection : String
  sourceDocId : String
  deriving Lean.ToJson

def SessionIdentity.toModel (id : SessionIdentity) : Identity :=
  { owner := id.ownerDid, trigger := id.triggerId,
    collection := id.sourceCollection, document := id.sourceDocId }

structure SessionInput where
  identity : SessionIdentity
  target : Option String
  owned : Bool
  createMissing : Bool
  deriving Lean.ToJson

def sessionOwnerA : SessionIdentity :=
  { ownerDid := "owner-a", triggerId := "handoff", sourceCollection := "Work", sourceDocId := "a" }

@[conformance_family]
def sessionResolution : Family SessionInput (Option String) where
  inputs :=
    [ { identity := sessionOwnerA, target := none, owned := true, createMissing := false }
    , { identity := sessionOwnerA, target := some "lead-one", owned := true, createMissing := false }
    , { identity := sessionOwnerA, target := some "lead-two", owned := true, createMissing := false }
    , { identity := sessionOwnerA, target := some "", owned := true, createMissing := false }
    , { identity := sessionOwnerA, target := some "foreign", owned := false, createMissing := false }
    , { identity := sessionOwnerA, target := some "game:a:red", owned := false, createMissing := true }
    , { identity := sessionOwnerA, target := some "game:a:red", owned := true, createMissing := true }
    , { identity := sessionOwnerA, target := some "", owned := false, createMissing := true } ]
  model := fun i => resolveSession i.identity.toModel i.target i.owned i.createMissing

structure IdentityWire where
  ownerDid : String
  triggerId : String
  sourceCollection : String
  sourceDocId : String
  deriving Lean.ToJson

namespace IdentityWire

def ofModel (id : Identity) : IdentityWire :=
  { ownerDid := id.owner, triggerId := id.trigger, sourceCollection := id.collection,
    sourceDocId := id.document }

def toModel (w : IdentityWire) : Identity :=
  { owner := w.ownerDid, document := w.sourceDocId, trigger := w.triggerId,
    collection := w.sourceCollection }

end IdentityWire

structure FireWire where
  identity : IdentityWire
  session : String
  serial : Bool
  emitOutcome : Bool
  goalBacked : Bool
  deriving Lean.ToJson

namespace FireWire

def ofModel (f : Fire) : FireWire :=
  { identity := IdentityWire.ofModel f.identity, session := f.session, serial := f.serial,
    emitOutcome := f.emitOutcome, goalBacked := f.goalBacked }

def toModel (w : FireWire) : Fire :=
  { identity := w.identity.toModel, session := w.session, serial := w.serial,
    emitOutcome := w.emitOutcome, goalBacked := w.goalBacked }

end FireWire

structure RequestWire where
  fire : FireWire
  running : Bool
  terminal : Bool
  assignmentReplaced : Bool
  goalAssignmentApplied : Bool
  deriving Lean.ToJson

namespace RequestWire

def ofModel (r : Request) : RequestWire :=
  { fire := FireWire.ofModel r.fire, running := r.running, terminal := r.terminal,
    assignmentReplaced := r.assignmentReplaced, goalAssignmentApplied := r.goalAssignmentApplied }

def toModel (w : RequestWire) : Request :=
  { fire := w.fire.toModel, running := w.running, terminal := w.terminal,
    assignmentReplaced := w.assignmentReplaced, goalAssignmentApplied := w.goalAssignmentApplied }

end RequestWire

structure GoalBindingWire where
  owner : String
  session : String
  assignment : IdentityWire
  status : Goals.Status
  deriving Lean.ToJson

namespace GoalBindingWire

def ofModel (g : GoalBinding) : GoalBindingWire :=
  { owner := g.owner, session := g.session, assignment := IdentityWire.ofModel g.assignment,
    status := g.status }

def toModel (w : GoalBindingWire) : GoalBinding :=
  { owner := w.owner, session := w.session, assignment := w.assignment.toModel, status := w.status }

end GoalBindingWire

structure RetainedInput where
  name : String
  request : RequestWire
  retained : Option GoalBindingWire
  current : Option GoalBindingWire
  settled : Bool
  reassignedInPlace : Bool
  deriving Lean.ToJson

structure RetainedClassification where
  classification : Option String
  deriving Lean.ToJson

structure RetainedRecovery where
  recovered : Nat
  rowsChanged : Bool
  recoverReason : Option String
  requestMatches : Bool
  deriving Lean.ToJson

/-- The Goal row a retained outcome is read through: the retained binding, or the
current one when the retained Goal is still running and was reassigned. -/
def retainedClassification (i : RetainedInput) : RetainedClassification :=
  { classification := (retainedOutcomeReason i.request.toModel
      (i.retained.map GoalBindingWire.toModel) (i.current.map GoalBindingWire.toModel)).map reasonString }

/-- A settled outcome was already published, so recovery publishes nothing. -/
def retainedRecoverReason (i : RetainedInput) : Option OutcomeReason :=
  if i.settled then none
  else retainedOutcomeReason i.request.toModel
    (i.retained.map GoalBindingWire.toModel) (i.current.map GoalBindingWire.toModel)

def retainedRecovery (i : RetainedInput) : RetainedRecovery :=
  let reason := retainedRecoverReason i
  { recovered := if reason.isSome then 1 else 0, rowsChanged := reason.isSome,
    recoverReason := reason.map reasonString, requestMatches := reason.isSome }

def retainedInput (name : String) (request : Request) (retained current : Option GoalBinding)
    (settled : Bool := false) (reassigned : Bool := false) : RetainedInput :=
  { name := name, request := RequestWire.ofModel request,
    retained := retained.map GoalBindingWire.ofModel, current := current.map GoalBindingWire.ofModel,
    settled := settled, reassignedInPlace := reassigned }

def retainedInputs : List RetainedInput :=
  let f := { fire "retained" with goalBacked := true }
  let request : Request := { fire := f, terminal := true, goalAssignmentApplied := true }
  let old : GoalBinding := { owner := request.fire.identity.owner, session := request.fire.session, assignment := request.fire.identity }
  let next : GoalBinding := { old with assignment := identity "owner-a" "fresh" }
  [retainedInput "retained_stopped_after_new_goal" request (some { old with status := .stopped }) (some next),
   retainedInput "retained_complete_after_new_goal" request (some { old with status := .complete }) (some next),
   retainedInput "retained_blocked_after_new_goal" request (some { old with status := .blocked }) (some next),
   retainedInput "retained_budget_after_new_goal" request (some { old with status := .budgetLimited }) (some next),
   retainedInput "retained_active_superseded" request (some old) (some next) (reassigned := true),
   retainedInput "retained_paused_superseded" request (some { old with status := .paused }) (some next) (reassigned := true),
   retainedInput "retained_usage_limited_superseded" request (some { old with status := .usageLimited }) (some next) (reassigned := true),
   retainedInput "retained_active_without_current" request (some old) none,
   retainedInput "retained_stopped_without_current" request (some { old with status := .stopped }) none,
   retainedInput "missing_retained_cannot_borrow_current" request none (some next),
   retainedInput "foreign_retained_owner" request (some { old with owner := "owner-b" }) (some next),
   retainedInput "foreign_retained_session" request (some { old with session := "other-session" }) (some next),
   retainedInput "foreign_retained_root" request (some { old with assignment := next.assignment }) (some next),
   retainedInput "foreign_current_cannot_supersede" request (some old) (some { next with owner := "owner-b" }),
   retainedInput "settled_retained_complete_after_new_goal" request (some { old with status := .complete }) (some next) (settled := true)]

@[conformance_family]
def retainedOutcomeClassification : Family RetainedInput RetainedClassification where
  inputs := retainedInputs
  model := retainedClassification

@[conformance_family]
def retainedOutcomeRecovery : Family RetainedInput RetainedRecovery where
  inputs := retainedInputs
  model := retainedRecovery

end Conformance.TriggerDelivery
