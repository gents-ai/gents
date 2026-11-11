import Proofs.Hooks.Delivery

namespace Hooks.OwedDelivery

/-- The durable record of one owed delivery (`Hooks.owed`), beside the
abstract record of `Hooks.Delivery`. Decision 2026-11-11, maintainer-approved:
an owed delivery is a document, node-owned operational state
(`Identity.OwedDeliveryAccess`), because a delivery dropped at a must-commit
point would otherwise be lost: a Goal whose pause was dropped stays active
after the interrupt that should have paused it.

The key is the point, the plugin and the physical subject: the request for
`request.terminal.stage`, which becomes terminal once, and the session together
with the halt's own time for `control.session.halt`, which recurs. A record
holds identifiers and the event's scalars (state, reason, time), never payload
rows: the redrive rebuilds the event from core's durable rows. -/
structure Key where
  point : String
  plugin : String
  subject : String
  deriving DecidableEq, Repr

inductive Settlement where
  | delivered
  | dropped
  deriving DecidableEq, Repr

/-- `actor` is the actor of the owing transaction. A delivery runs under it
and under nothing else (`Hooks.Safety.perform`); the node key only keeps the
record. -/
structure Record where
  key : Key
  actor : String
  attempts : Nat
  lastError : Option String
  settled : Option Settlement
  deriving DecidableEq, Repr

def Record.fresh (actor : String) (key : Key) : Record :=
  ⟨key, actor, 0, none, none⟩

def Record.pending (record : Record) : Bool := record.settled.isNone

/-- What the redrive observes in its own transaction, which runs as the
recorded actor: `authorized`, that the actor still reads the subject under
native ACP now, not when the delivery was owed; `current`, that core's rows
still show the owing transition and nothing has superseded it (the request is
terminal in the recorded state; the session's stop latch is as the halt left it
and no later request was created). -/
structure Observation where
  authorized : Bool
  current : Bool
  deriving DecidableEq, Repr

/-- The plugin's hook run for that one plugin as a refusable `stage`, in the
redrive's transaction: `applied` when that transaction committed. -/
inductive Attempt where
  | applied
  | failed (error : String)
  deriving DecidableEq, Repr

/-- One redrive of one record. A settled record is inert. A record whose actor
lost its authority, or whose transition is no longer current, settles as
dropped and its hook is not run. Otherwise the hook's committed staging
settles it as delivered, and a failure counts an attempt and keeps it owed. -/
def redrive (record : Record) (observed : Observation) (attempt : Attempt) : Record :=
  if record.settled.isSome then record
  else if !observed.authorized || !observed.current then
    { record with settled := some .dropped }
  else
    match attempt with
    | .applied => { record with settled := some .delivered }
    | .failed error => { record with attempts := record.attempts + 1, lastError := some error }

/-- Whether a redrive performs the plugin's effects. -/
def performs (record : Record) (observed : Observation) (attempt : Attempt) : Bool :=
  record.pending && observed.authorized && observed.current && attempt == .applied

/-- Publication with the owing transaction: a record per owed key, unless one
with that key exists, so publishing twice is inert. A rolled-back transaction
publishes nothing. -/
def publishOne (records : List Record) (actor : String) (key : Key) : List Record :=
  if records.any (·.key == key) then records else records ++ [Record.fresh actor key]

def publish (records : List Record) (actor : String) (keys : List Key) (commit : Bool) :
    List Record :=
  if commit then keys.foldl (fun acc key => publishOne acc actor key) records else records

def lookup (records : List Record) (key : Key) : Option Record :=
  records.find? (·.key == key)

theorem rollback_publishes_nothing (records : List Record) (actor : String) (keys : List Key) :
    publish records actor keys false = records := by
  simp [publish]

private theorem publishOne_keeps (records : List Record) (actor : String) (key : Key)
    (present : records.any (·.key == key) = true) : publishOne records actor key = records := by
  simp [publishOne, present]

private theorem publishOne_present (records : List Record) (actor : String) (key other : Key)
    (present : records.any (·.key == other) = true) :
    (publishOne records actor key).any (·.key == other) = true := by
  unfold publishOne
  split
  · exact present
  · simp only [List.any_append, present, Bool.true_or]

private theorem publishOne_adds (records : List Record) (actor : String) (key : Key) :
    (publishOne records actor key).any (·.key == key) = true := by
  unfold publishOne
  split
  · assumption
  · simp [Record.fresh]

private theorem fold_present (keys : List Key) (actor : String) (records : List Record)
    (other : Key) (present : records.any (·.key == other) = true) :
    (keys.foldl (fun acc key => publishOne acc actor key) records).any (·.key == other) =
      true := by
  induction keys generalizing records with
  | nil => exact present
  | cons key rest ih =>
    exact ih _ (publishOne_present records actor key other present)

/-- Every published key has a record. -/
theorem published_key_is_recorded (records : List Record) (actor : String) (keys : List Key)
    (key : Key) (named : key ∈ keys) :
    (publish records actor keys true).any (·.key == key) = true := by
  simp only [publish, if_true]
  induction keys generalizing records with
  | nil => cases named
  | cons head rest ih =>
    rcases List.mem_cons.mp named with rfl | later
    · exact fold_present rest actor _ _ (publishOne_adds records actor _)
    · exact ih _ later

private theorem fold_all_present (keys : List Key) (actor : String) (records : List Record)
    (present : ∀ key ∈ keys, records.any (·.key == key) = true) :
    keys.foldl (fun acc key => publishOne acc actor key) records = records := by
  induction keys with
  | nil => rfl
  | cons key rest ih =>
    simp only [List.foldl_cons]
    rw [publishOne_keeps records actor key (present key (by simp))]
    exact ih (fun other member => present other (by simp [member]))

/-- The key is deterministic, so publishing the same keys again, by any
actor, is inert: an owed delivery is published once. -/
theorem publish_twice_is_inert (records : List Record) (actor again : String)
    (keys : List Key) :
    publish (publish records actor keys true) again keys true =
      publish records actor keys true := by
  conv => lhs; simp only [publish, if_true]
  exact fold_all_present keys again _
    (fun key member => published_key_is_recorded records actor keys key member)

/-- A record carries its key and recorded actor through every redrive. -/
theorem redrive_keeps_key_and_actor (record : Record) (observed : Observation)
    (attempt : Attempt) :
    (redrive record observed attempt).key = record.key ∧
      (redrive record observed attempt).actor = record.actor := by
  unfold redrive
  split
  · exact ⟨rfl, rfl⟩
  · split
    · exact ⟨rfl, rfl⟩
    · cases attempt <;> exact ⟨rfl, rfl⟩

/-- A revoked actor's delivery is never performed and stops being pending. -/
theorem revoked_actor_is_never_performed (record : Record) (observed : Observation)
    (attempt : Attempt) (revoked : observed.authorized = false) :
    performs record observed attempt = false ∧
      (record.pending → (redrive record observed attempt).settled = some .dropped) := by
  refine ⟨by simp [performs, revoked], fun pending => ?_⟩
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  simp [redrive, unsettled, revoked]

/-- A transition that is no longer current (never committed, or superseded)
is not performed either. -/
theorem stale_transition_is_never_performed (record : Record) (observed : Observation)
    (attempt : Attempt) (stale : observed.current = false) :
    performs record observed attempt = false ∧
      (record.pending → (redrive record observed attempt).settled = some .dropped) := by
  refine ⟨by simp [performs, stale], fun pending => ?_⟩
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  simp [redrive, unsettled, stale]

/-- A delivery is performed only under its recorded actor's current
authority. -/
theorem performs_only_while_authorized (record : Record) (observed : Observation)
    (attempt : Attempt) (performed : performs record observed attempt = true) :
    observed.authorized = true ∧ observed.current = true := by
  simp only [performs, Bool.and_eq_true] at performed
  exact ⟨performed.1.1.2, performed.1.2⟩

/-- Redelivery is inert: a settled record is unchanged by any redrive. -/
theorem redelivery_is_inert (record : Record) (observed : Observation) (attempt : Attempt)
    (settled : record.pending = false) : redrive record observed attempt = record := by
  have some : record.settled.isSome = true := by
    simpa [Record.pending, Option.isNone_iff_eq_none, Option.isSome_iff_ne_none] using settled
  simp [redrive, some]

theorem delivered_record_is_settled (record : Record) (observed : Observation)
    (current : observed.authorized = true ∧ observed.current = true)
    (pending : record.pending = true) :
    (redrive record observed .applied).settled = some .delivered := by
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  simp [redrive, unsettled, current.1, current.2]

/-- Nothing owed is lost while the actor remains authorized and the transition
current: a failed attempt keeps the record owed and counts the attempt. -/
theorem failed_redrive_stays_pending (record : Record) (observed : Observation)
    (error : String) (current : observed.authorized = true ∧ observed.current = true)
    (pending : record.pending = true) :
    (redrive record observed (.failed error)).pending = true ∧
      (redrive record observed (.failed error)).attempts = record.attempts + 1 := by
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  simp [redrive, unsettled, current.1, current.2, Record.pending]

/-- While the actor is authorized and the transition current, a record leaves
the owed set only by its delivery committing. -/
theorem settles_only_by_delivery_while_authorized (record : Record) (observed : Observation)
    (attempt : Attempt) (current : observed.authorized = true ∧ observed.current = true)
    (pending : record.pending = true)
    (settled : (redrive record observed attempt).pending = false) :
    performs record observed attempt = true := by
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  cases attempt with
  | applied => simp [performs, pending, current.1, current.2]
  | failed error =>
    simp [redrive, unsettled, current.1, current.2, Record.pending] at settled

/-- A freshly published record is owed until a redrive settles it. -/
theorem fresh_record_is_pending (actor : String) (key : Key) :
    (Record.fresh actor key).pending = true := rfl

end Hooks.OwedDelivery
