import Proofs.Triggers.Durable
import Proofs.EventDelivery.Durable
import Proofs.Conformance.ContractTypes

namespace Conformance.TriggerDelivery

open Triggers.Durable Conformance.Contracts

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

/-- Drives a callback-binding consumer through the shared cursor owner. Its
receipts are `CallbackInvocation`s; `editAfterAdmission` edits the admitted
document and admits it again under the new source version. -/
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

def sessionCases : List String :=
  let id := identity "owner-a" "a"
  [(none, true), (some "lead-one", true), (some "lead-two", true),
   (some "", true), (some "foreign", false)].map fun (target, owned) => object [
     ("identity", identityJson id), ("target", jsonOptionalString target),
     ("owned", toString owned), ("resolved", jsonOptionalString (resolveSession id target owned))]

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

def assignmentRootCases : List String :=
  [(none, none), (none, some "ordinary"), (some "new", none),
   (some "new", some "new"), (some "new", some "old")].map fun (assigned, observed) => object [
    ("assigned", jsonOptionalString assigned), ("observed", jsonOptionalString observed),
    ("allowed", toString (Goals.assignmentAllows assigned observed))]

def selfCases : List String :=
  [("owner", "first", "owner", "first"),
   ("owner", "first", "owner", "second"),
   ("owner", "first", "foreign", "first")].map fun (co, cs, lo, ls) => object [
    ("caller_owner", jsonString co), ("caller_session", jsonString cs),
    ("listed_owner", jsonString lo), ("listed_session", jsonString ls),
    ("current", toString (isCurrent co cs lo ls))]

def casesJson : String := object [
  ("admissions", jsonArray admissionCases), ("queues", jsonArray queueCases),
  ("observed_claims", jsonArray observedClaimCases),
  ("assignment_roots", jsonArray assignmentRootCases),
  ("outcomes", jsonArray outcomeCases), ("outcome_traces", jsonArray outcomeTraces),
  ("cursors", jsonArray cursorCases),
  ("callback_cursors", jsonArray callbackCursorCases),
  ("identities", jsonArray identityCases), ("sessions", jsonArray sessionCases),
  ("self_sessions", jsonArray selfCases)]

example : (admit (admit {} (fire "a")) (fire "a")).requests.length = 1 := by native_decide
example : outcomeDue {} { fire := { fire "a" with goalBacked := true }, terminal := false } = false := by
  native_decide

end Conformance.TriggerDelivery
