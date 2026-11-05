import Lean

namespace ClientSubmissionReceipt
open Lean

/-- The caller retains the canonical request identity before transmission. The
existing local journal's unique request index is the preparation reservation. -/
structure Caller where
  requester : Nat
  request : Nat
  deriving DecidableEq, Repr

/-- The envelope denotes the exact authored input, captures, requested session
and configuration. Equality is structural binding, never matching display text.
Goal assignment and the generated session are frozen in the prepared request. -/
structure Intent where
  caller : Caller
  node : Nat
  envelope : Nat
  deriving DecidableEq, Repr

structure Prepared where
  session : Nat
  request : Nat
  signedBytes : Nat
  deriving DecidableEq, Repr

inductive Outcome where
  | unresolved
  | accepted
  | notDelivered
  deriving DecidableEq, Repr

/-- Cleanup reports retain the original delivery observation and request
identity. A failed local cleanup cannot convert a confirmed delivery into an
unresolved or rejected send, and must not authorize a new request identity. -/
structure Completion where
  prepared : Prepared
  outcome : Outcome
  cleanupFailed : Bool
  deriving DecidableEq, Repr

def completion (prepared : Prepared) (outcome : Outcome) (cleanupSucceeded : Bool) : Completion :=
  ⟨prepared, outcome, outcome != .unresolved && !cleanupSucceeded⟩

theorem cleanup_preserves_delivery (prepared : Prepared) (outcome : Outcome)
    (cleanupSucceeded : Bool) :
    (completion prepared outcome cleanupSucceeded).prepared = prepared ∧
      (completion prepared outcome cleanupSucceeded).outcome = outcome := by
  simp [completion]

theorem unknown_never_claims_cleanup_failure (prepared : Prepared) (cleanupSucceeded : Bool) :
    (completion prepared .unresolved cleanupSucceeded).cleanupFailed = false := by
  simp [completion]

/-- A definite native rejection remains attached to the original request
identity. An ambiguous observation remains unresolved; an absent filtered row
alone is not a rejection. -/
structure RejectionObservation where
  prepared : Prepared
  outcome : Outcome
  nativeError : Option Nat
  deriving DecidableEq, Repr

def rejectionObservation (prepared : Prepared) (outcome : Outcome)
    (nativeError : Option Nat) : RejectionObservation :=
  ⟨prepared, outcome, if outcome == .notDelivered then nativeError else none⟩

theorem rejection_preserves_request (prepared : Prepared) (outcome : Outcome) (nativeError : Option Nat) :
    (rejectionObservation prepared outcome nativeError).prepared = prepared := by rfl

theorem rejection_preserves_native_error (prepared : Prepared) (nativeError : Option Nat) :
    (rejectionObservation prepared .notDelivered nativeError).nativeError = nativeError := by rfl

theorem unresolved_does_not_report_rejection (prepared : Prepared) (nativeError : Option Nat) :
    (rejectionObservation prepared .unresolved nativeError).nativeError = none := by rfl

def rejectionCasesJson : String :=
  let prepared : Prepared := ⟨20, 30, 300⟩
  (toJson (([Outcome.unresolved, .accepted, .notDelivered]).flatMap fun outcome =>
    [none, some 71].map fun nativeError =>
      let observed := rejectionObservation prepared outcome nativeError
      Json.mkObj [("request", toJson prepared.request),
        ("outcome", toJson (match outcome with
          | .unresolved => "unresolved" | .accepted => "accepted" | .notDelivered => "notDelivered")),
        ("native_error", toJson nativeError), ("expected_request", toJson observed.prepared.request),
        ("expected_native_error", toJson observed.nativeError)])).compress

/-- An explicit native error while reading a receipt is evidence about that
read, not proof of non-delivery. The caller retains both the original request
identity and the error while the outcome stays unresolved. -/
structure UncertainObservation where
  prepared : Prepared
  outcome : Outcome
  accessError : Option Nat
  deriving DecidableEq, Repr

def uncertainObservation (prepared : Prepared) (accessError : Option Nat) : UncertainObservation :=
  ⟨prepared, .unresolved, accessError⟩

theorem uncertain_access_preserves_error_and_identity (prepared : Prepared) (accessError : Option Nat) :
    (uncertainObservation prepared accessError).prepared = prepared ∧
      (uncertainObservation prepared accessError).accessError = accessError ∧
      (uncertainObservation prepared accessError).outcome = .unresolved := by
  simp [uncertainObservation]

def uncertainErrorCasesJson : String :=
  let prepared : Prepared := ⟨20, 30, 300⟩
  (toJson ([none, some 71].map fun error =>
    let observed := uncertainObservation prepared error
    Json.mkObj [("request", toJson prepared.request), ("access_error", toJson error),
      ("expected_request", toJson observed.prepared.request),
      ("expected_access_error", toJson observed.accessError),
      ("expected_outcome", toJson "unresolved")])).compress

def cleanupCasesJson : String :=
  let prepared : Prepared := ⟨20, 30, 300⟩
  let preparedJson := fun (value : Prepared) => Json.mkObj
    [("session", toJson value.session), ("request", toJson value.request),
     ("signedBytes", toJson value.signedBytes)]
  let outcomeJson := fun (outcome : Outcome) => toJson <| match outcome with
    | .unresolved => "unresolved" | .accepted => "accepted" | .notDelivered => "notDelivered"
  (toJson (([Outcome.unresolved, .accepted, .notDelivered]).flatMap fun outcome =>
    [true, false].map fun success =>
      let observed := completion prepared outcome success
      Json.mkObj [("prepared", preparedJson prepared), ("outcome", outcomeJson outcome),
        ("cleanup_succeeded", toJson success), ("expected_prepared", preparedJson observed.prepared),
        ("expected_outcome", outcomeJson observed.outcome), ("cleanup_failed", toJson observed.cleanupFailed)])).compress

structure Record where
  intent : Intent
  prepared : Prepared
  outcome : Outcome := .unresolved
  retainedBytes : Bool := true
  acknowledged : Bool := false
  deriving DecidableEq, Repr

abbrev Journal := List Record

inductive Result where
  | refused
  | reconcile (prepared : Prepared)
  | settled (prepared : Prepared) (outcome : Outcome)
  deriving DecidableEq, Repr

def lookup (journal : Journal) (caller : Caller) : Option Record :=
  journal.find? fun record => record.intent.caller == caller

def bound (record : Record) : Bool :=
  record.prepared.request == record.intent.caller.request &&
    record.retainedBytes != record.acknowledged

def result (record : Record) : Result :=
  if !bound record then .refused else
    match record.outcome with
    | .unresolved => if record.retainedBytes then .reconcile record.prepared else .refused
    | outcome => .settled record.prepared outcome

/-- `durable` is the existing journal owner's successful durable preparation,
not an RPC reply or database admission receipt. A failed or ambiguous local
write cannot authorize an owner effect. Retrying an existing caller operation
returns its original signed preparation even if a new proposal was allocated.
The lookup and reservation are one atomic journal decision; an independently
racing read followed by insertion does not implement this transition. -/
def prepare (journal : Journal) (intent : Intent) (proposal : Prepared)
    (durable : Bool) : Journal × Result :=
  if (journal.filter fun record => record.intent.caller == intent.caller).length > 1 then
    (journal, .refused)
  else match lookup journal intent.caller with
  | some record =>
    if record.intent == intent then (journal, result record) else (journal, .refused)
  | none =>
    if durable && proposal.request == intent.caller.request &&
        !(journal.any fun record => record.prepared.request == proposal.request) then
      let record : Record := ⟨intent, proposal, .unresolved, true, false⟩
      (journal ++ [record], result record)
    else (journal, .refused)

/-- This observation is supplied only by the existing signed owner-receipt
reconciler. Transport absence and elapsed time cannot establish non-delivery. -/
def observe (journal : Journal) (caller : Caller) (prepared : Prepared)
    (outcome : Outcome) : Journal :=
  if (journal.filter fun record => record.intent.caller == caller).length != 1 then journal
  else journal.map fun record =>
    if bound record && record.intent.caller == caller && record.prepared == prepared &&
        record.outcome == .unresolved && outcome != .unresolved then
      { record with outcome := outcome }
    else record

/-- Acknowledgement may retire replay bytes after a confirmed outcome. The
caller-to-request binding survives it: a stale restored view must receive the
same result rather than create a second signed request after an acknowledged
reply. Unresolved work cannot be acknowledged away. -/
def acknowledge (journal : Journal) (caller : Caller) (prepared : Prepared) : Journal :=
  if (journal.filter fun record => record.intent.caller == caller).length != 1 then journal
  else journal.map fun record =>
    if bound record && record.intent.caller == caller && record.prepared == prepared &&
        record.outcome != .unresolved then
      { record with retainedBytes := false, acknowledged := true }
    else record

theorem prepare_existing_binding (journal : Journal) (intent : Intent)
    (record : Record) (proposal : Prepared) (durable : Bool)
    (found : lookup journal intent.caller = some record)
    (same : record.intent = intent)
    (unique : (journal.filter fun row => row.intent.caller == intent.caller).length ≤ 1) :
    prepare journal intent proposal durable = (journal, result record) := by
  simp [prepare, found, same, Nat.not_lt.mpr unique]

theorem prepare_scope_mismatch_refused (journal : Journal) (intent : Intent)
    (record : Record) (proposal : Prepared) (durable : Bool)
    (found : lookup journal intent.caller = some record)
    (different : record.intent ≠ intent) :
    prepare journal intent proposal durable = (journal, .refused) := by
  simp only [prepare]
  split <;> simp [found, different]

theorem failed_preparation_cannot_authorize (journal : Journal) (intent : Intent)
    (proposal : Prepared) (absent : lookup journal intent.caller = none) :
    prepare journal intent proposal false = (journal, .refused) := by
  simp only [prepare]
  split <;> simp [absent]

theorem acknowledgement_keeps_bindings (journal : Journal) (caller : Caller)
    (prepared : Prepared) :
    (acknowledge journal caller prepared).map (fun record => (record.intent, record.prepared)) =
      journal.map (fun record => (record.intent, record.prepared)) := by
  unfold acknowledge
  split
  · rfl
  · simp only [List.map_map]
    congr 1
    funext record
    dsimp only [Function.comp_def]
    split <;> rfl

theorem unknown_cannot_be_retired (intent : Intent) (prepared : Prepared) :
    acknowledge [⟨intent, prepared, .unresolved, true, false⟩] intent.caller prepared =
      [⟨intent, prepared, .unresolved, true, false⟩] := by
  simp [acknowledge]

theorem acknowledged_outcome_never_replays (intent : Intent) (prepared : Prepared)
    (outcome : Outcome) (confirmed : outcome ≠ .unresolved)
    (identity : prepared.request = intent.caller.request) :
    result ⟨intent, prepared, outcome, false, true⟩ = .settled prepared outcome := by
  cases outcome <;> simp_all [result, bound]

end ClientSubmissionReceipt
