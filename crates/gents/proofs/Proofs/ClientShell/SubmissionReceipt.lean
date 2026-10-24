namespace ClientSubmissionReceipt

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
