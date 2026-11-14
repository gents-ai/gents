import Proofs.Session.Hooks.Delivery

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

/-- The two classes of owed delivery. Decision 2026-11-11, maintainer-approved.

`staging`: the dropped staging of a must-commit transition,
`request.terminal.stage` and `control.session.halt`. Core authorized and
committed the transition itself; what is owed is only the plugin's own
bookkeeping of it. The runtime redrives it as the Agent that owns the subject
session (a runtime-managed DID the runtime signs as), whoever the transition's
actor was: a person interrupting from the CLI or the desktop is exactly when a
Goal pause is owed, and the runtime cannot sign as that person.

`onBehalf`: a post-commit delivery that asks core to act for someone, the
safety monitor's stop (`Hooks.Safety.perform`), `request.output.recorded` and
the `*.committed` delivery points. It runs under the recorded actor's own
authority, re-checked at redrive; an actor the runtime cannot sign as is
refused when the delivery would be owed (`owe`), never left pending. -/
inductive Kind where
  | staging
  | onBehalf
  deriving DecidableEq, Repr

def Kind.ofPoint : String → Kind
  | "request.terminal.stage" => .staging
  | "control.session.halt" => .staging
  | _ => .onBehalf

inductive Settlement where
  | delivered
  | dropped
  deriving DecidableEq, Repr

/-- `actor` is the actor of the owing transaction, kept for audit; a
transaction can have none. `runner` is the identity the redrive runs as and
under nothing else (`owe`): the owning Agent for a staging, the recorded actor
for an `onBehalf` delivery. The node key only keeps the record. -/
structure Record where
  key : Key
  kind : Kind
  actor : Option String
  runner : String
  attempts : Nat
  lastError : Option String
  settled : Option Settlement
  deriving DecidableEq, Repr

/-- Who a delivery would run as when it is owed, or `none` when it is not
recorded. A staging runs as the Agent owning the subject session, with or
without an actor; an `onBehalf` delivery runs as its recorded actor only when
the runtime can sign as that actor. -/
def owe (kind : Kind) (actor owner : Option String) (signable : String → Bool) :
    Option String :=
  match kind with
  | .staging => owner
  | .onBehalf => actor.filter signable

theorem staging_runs_under_owning_agent (actor : Option String) (owner : String)
    (signable : String → Bool) : owe .staging actor (some owner) signable = some owner := rfl

theorem on_behalf_runs_under_recorded_actor (actor : String) (owner : Option String)
    (signable : String → Bool) (signs : signable actor = true) :
    owe .onBehalf (some actor) owner signable = some actor := by
  simp [owe, Option.filter, signs]

/-- An `onBehalf` delivery whose actor the runtime cannot sign as is refused at
owe time rather than left pending forever. -/
theorem unsignable_on_behalf_is_refused_at_owe (actor : String) (owner : Option String)
    (signable : String → Bool) (unsignable : signable actor = false) :
    owe .onBehalf (some actor) owner signable = none := by
  simp [owe, Option.filter, unsignable]

def Record.fresh (kind : Kind) (actor : Option String) (runner : String) (key : Key) :
    Record :=
  ⟨key, kind, actor, runner, 0, none, none⟩

def Record.pending (record : Record) : Bool := record.settled.isNone

/-- What the redrive observes in its own transaction, which runs as `runner`:
`authorized`, for a staging that the subject still exists and is still owned by
the runner, for an `onBehalf` delivery that the recorded actor still holds the
authority under native ACP now; `current`, that core's rows still show the
owing transition and nothing has superseded it (the request is terminal in the
recorded state; the session's stop latch is as the halt left it and no later
request was created). -/
structure Observation where
  authorized : Bool
  current : Bool
  deriving DecidableEq, Repr

/-- One write a plugin's staging asks for: the document and the subject it
belongs to. -/
structure Write where
  subject : String
  doc : String
  deriving DecidableEq, Repr

/-- The plugin's hook run for that one plugin as a refusable `stage`, in the
redrive's transaction: `applied` with the writes it asked for, or `failed`. -/
inductive Attempt where
  | applied (writes : List Write)
  | failed (error : String)
  deriving DecidableEq, Repr

/-- Writes the redrive may make: each on the subject the delivery was owed
for, and each one `mayWrite` (native document ACP for the runner) admits.

Premise: the subject restriction holds in the native host because the rebuilt
event names only the subject and a plugin writes only rows it was shown
(`Hooks.Named`); native ACP for the runner decides `mayWrite` as each write
executes, so an inadmissible write fails the redrive's transaction. -/
def admissible (record : Record) (mayWrite : Write → Bool) (writes : List Write) : Bool :=
  writes.all fun write => write.subject == record.key.subject && mayWrite write

/-- One redrive of one record. A settled record is inert. A record whose subject
is gone or no longer the runner's (or whose actor lost authority), or whose
transition is no longer current, settles as dropped and its hook is not run.
Otherwise an admissible staging settles it as delivered, and a failure,
including a refused write, counts an attempt and keeps it owed. -/
def redrive (record : Record) (observed : Observation) (attempt : Attempt)
    (mayWrite : Write → Bool) : Record :=
  if record.settled.isSome then record
  else if !observed.authorized || !observed.current then
    { record with settled := some .dropped }
  else
    match attempt with
    | .applied writes =>
        if admissible record mayWrite writes then { record with settled := some .delivered }
        else { record with attempts := record.attempts + 1,
                            lastError := some "a write was refused" }
    | .failed error => { record with attempts := record.attempts + 1, lastError := some error }

/-- Whether a redrive performs the plugin's writes. -/
def performs (record : Record) (observed : Observation) (attempt : Attempt)
    (mayWrite : Write → Bool) : Bool :=
  record.pending && observed.authorized && observed.current &&
    match attempt with
    | .applied writes => admissible record mayWrite writes
    | .failed _ => false

/-- Publication after the owing transaction commits: a record per owed key,
unless one with that key exists, so publishing twice is inert. A rolled-back
transaction publishes nothing.

Premise (decision 2026-11-11, maintainer-approved), the publication window:
publication is not atomic with the transition. The record is node-owned while
the owing transaction runs as its own actor, and an embedded transaction
carries one identity and cannot open another inside it, so the runtime
publishes the record right after the commit; a process that stops in between
loses it. `deriveHalts` closes that window for halts from durable core state;
a request terminal's triggers outcome is republished by its own recovery
(`recover_outcomes`). -/
def publishOne (records : List Record) (fresh : Key → Record) (key : Key) : List Record :=
  if records.any (·.key == key) then records else records ++ [fresh key]

def publish (records : List Record) (fresh : Key → Record) (keys : List Key) (commit : Bool) :
    List Record :=
  if commit then keys.foldl (fun acc key => publishOne acc fresh key) records else records

def lookup (records : List Record) (key : Key) : Option Record :=
  records.find? (·.key == key)

/-- The publication window: the transition committed and its record was not
published. -/
structure Publication where
  committed : Bool
  published : Bool
  deriving DecidableEq, Repr

def Publication.lost (publication : Publication) : Bool :=
  publication.committed && !publication.published

theorem rollback_publishes_nothing (records : List Record) (fresh : Key → Record)
    (keys : List Key) : publish records fresh keys false = records := by
  simp [publish]

private theorem publishOne_keeps (records : List Record) (fresh : Key → Record) (key : Key)
    (present : records.any (·.key == key) = true) : publishOne records fresh key = records := by
  simp [publishOne, present]

private theorem publishOne_present (records : List Record) (fresh : Key → Record)
    (key other : Key) (present : records.any (·.key == other) = true) :
    (publishOne records fresh key).any (·.key == other) = true := by
  unfold publishOne
  split
  · exact present
  · simp only [List.any_append, present, Bool.true_or]

private theorem publishOne_adds (records : List Record) (fresh : Key → Record) (key : Key)
    (keyed : ∀ key, (fresh key).key = key) :
    (publishOne records fresh key).any (·.key == key) = true := by
  unfold publishOne
  split
  · assumption
  · simp [keyed]

private theorem fold_present (keys : List Key) (fresh : Key → Record) (records : List Record)
    (other : Key) (present : records.any (·.key == other) = true) :
    (keys.foldl (fun acc key => publishOne acc fresh key) records).any (·.key == other) =
      true := by
  induction keys generalizing records with
  | nil => exact present
  | cons key rest ih =>
    exact ih _ (publishOne_present records fresh key other present)

/-- Every published key has a record. -/
theorem published_key_is_recorded (records : List Record) (fresh : Key → Record)
    (keyed : ∀ key, (fresh key).key = key) (keys : List Key) (key : Key) (named : key ∈ keys) :
    (publish records fresh keys true).any (·.key == key) = true := by
  simp only [publish, if_true]
  induction keys generalizing records with
  | nil => cases named
  | cons head rest ih =>
    rcases List.mem_cons.mp named with rfl | later
    · exact fold_present rest fresh _ _ (publishOne_adds records fresh _ keyed)
    · exact ih _ later

private theorem fold_all_present (keys : List Key) (fresh : Key → Record) (records : List Record)
    (present : ∀ key ∈ keys, records.any (·.key == key) = true) :
    keys.foldl (fun acc key => publishOne acc fresh key) records = records := by
  induction keys with
  | nil => rfl
  | cons key rest ih =>
    simp only [List.foldl_cons]
    rw [publishOne_keeps records fresh key (present key (by simp))]
    exact ih (fun other member => present other (by simp [member]))

/-- The key is deterministic, so publishing the same keys again, whoever
publishes them, is inert: an owed delivery is published once. -/
theorem publish_twice_is_inert (records : List Record) (fresh again : Key → Record)
    (keyed : ∀ key, (fresh key).key = key) (keys : List Key) :
    publish (publish records fresh keys true) again keys true =
      publish records fresh keys true := by
  conv => lhs; simp only [publish, if_true]
  exact fold_all_present keys again _
    (fun key member => published_key_is_recorded records fresh keyed keys key member)

/-- A record carries its key, kind, recorded actor and runner through every
redrive. -/
theorem redrive_keeps_identity (record : Record) (observed : Observation) (attempt : Attempt)
    (mayWrite : Write → Bool) :
    (redrive record observed attempt mayWrite).key = record.key ∧
      (redrive record observed attempt mayWrite).kind = record.kind ∧
      (redrive record observed attempt mayWrite).actor = record.actor ∧
      (redrive record observed attempt mayWrite).runner = record.runner := by
  unfold redrive
  split
  · exact ⟨rfl, rfl, rfl, rfl⟩
  · split
    · exact ⟨rfl, rfl, rfl, rfl⟩
    · cases attempt with
      | applied writes =>
        dsimp only
        split <;> exact ⟨rfl, rfl, rfl, rfl⟩
      | failed error => exact ⟨rfl, rfl, rfl, rfl⟩

/-- Why redriving a staging as the owning Agent does not widen authority
(decision 2026-11-11, maintainer-approved): core already authorized and
committed the transition, so the plugin gains nothing it would not have had if
its staging had succeeded inside that transition; and the redrive acts only on
the owning Agent's own rows, writes on the subject the delivery was owed for,
each admitted by native ACP for that Agent. -/
theorem redrive_performs_only_owning_agent_writes (record : Record) (owner : String)
    (observed : Observation) (writes : List Write) (mayWrite : String → Write → Bool)
    (staging : record.kind = .staging) (ownedBy : record.runner = owner)
    (performed : performs record observed (.applied writes) (mayWrite record.runner) = true) :
    record.kind = .staging ∧
      ∀ write ∈ writes, write.subject = record.key.subject ∧ mayWrite owner write = true := by
  refine ⟨staging, fun write member => ?_⟩
  simp only [performs, admissible, Bool.and_eq_true, List.all_eq_true] at performed
  have each := performed.2 write member
  simp only [Bool.and_eq_true, beq_iff_eq] at each
  exact ⟨each.1, ownedBy ▸ each.2⟩

/-- A subject that no longer exists or is no longer owned by the runner (for an
`onBehalf` delivery: an actor whose authority was revoked) is never performed
and stops being pending. -/
theorem unowned_subject_is_never_performed (record : Record) (observed : Observation)
    (attempt : Attempt) (mayWrite : Write → Bool) (unowned : observed.authorized = false) :
    performs record observed attempt mayWrite = false ∧
      (record.pending → (redrive record observed attempt mayWrite).settled = some .dropped) := by
  refine ⟨by simp [performs, unowned], fun pending => ?_⟩
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  simp [redrive, unsettled, unowned]

/-- The `onBehalf` reading of the same rule: a revoked actor's delivery is never
performed. -/
theorem revoked_on_behalf_actor_is_never_performed (record : Record) (observed : Observation)
    (attempt : Attempt) (mayWrite : Write → Bool) (_onBehalf : record.kind = .onBehalf)
    (revoked : observed.authorized = false) :
    performs record observed attempt mayWrite = false :=
  (unowned_subject_is_never_performed record observed attempt mayWrite revoked).1

/-- A transition that is no longer current (never committed, or superseded)
is not performed either. -/
theorem stale_transition_is_never_performed (record : Record) (observed : Observation)
    (attempt : Attempt) (mayWrite : Write → Bool) (stale : observed.current = false) :
    performs record observed attempt mayWrite = false ∧
      (record.pending → (redrive record observed attempt mayWrite).settled = some .dropped) := by
  refine ⟨by simp [performs, stale], fun pending => ?_⟩
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  simp [redrive, unsettled, stale]

theorem performs_only_while_authorized (record : Record) (observed : Observation)
    (attempt : Attempt) (mayWrite : Write → Bool)
    (performed : performs record observed attempt mayWrite = true) :
    observed.authorized = true ∧ observed.current = true := by
  simp only [performs, Bool.and_eq_true] at performed
  exact ⟨performed.1.1.2, performed.1.2⟩

/-- Redelivery is inert: a settled record is unchanged by any redrive. -/
theorem redelivery_is_inert (record : Record) (observed : Observation) (attempt : Attempt)
    (mayWrite : Write → Bool) (settled : record.pending = false) :
    redrive record observed attempt mayWrite = record := by
  have some : record.settled.isSome = true := by
    simpa [Record.pending, Option.isNone_iff_eq_none, Option.isSome_iff_ne_none] using settled
  simp [redrive, some]

theorem delivered_record_is_settled (record : Record) (observed : Observation)
    (writes : List Write) (mayWrite : Write → Bool)
    (current : observed.authorized = true ∧ observed.current = true)
    (pending : record.pending = true) (allowed : admissible record mayWrite writes = true) :
    (redrive record observed (.applied writes) mayWrite).settled = some .delivered := by
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  simp [redrive, unsettled, current.1, current.2, allowed]

/-- Nothing owed is lost while the subject stays owned and the transition
current: a failed attempt keeps the record owed and counts the attempt. -/
theorem failed_redrive_stays_pending (record : Record) (observed : Observation)
    (error : String) (mayWrite : Write → Bool)
    (current : observed.authorized = true ∧ observed.current = true)
    (pending : record.pending = true) :
    (redrive record observed (.failed error) mayWrite).pending = true ∧
      (redrive record observed (.failed error) mayWrite).attempts = record.attempts + 1 := by
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  simp [redrive, unsettled, current.1, current.2, Record.pending]

/-- While the subject stays owned and the transition current, a record leaves
the owed set only by its delivery being performed. -/
theorem settles_only_by_delivery_while_authorized (record : Record) (observed : Observation)
    (attempt : Attempt) (mayWrite : Write → Bool)
    (current : observed.authorized = true ∧ observed.current = true)
    (pending : record.pending = true)
    (settled : (redrive record observed attempt mayWrite).pending = false) :
    performs record observed attempt mayWrite = true := by
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  cases attempt with
  | applied writes =>
    by_cases allowed : admissible record mayWrite writes = true
    · simp [performs, pending, current.1, current.2, allowed]
    · simp [redrive, unsettled, current.1, current.2, allowed, Record.pending] at settled
  | failed error =>
    simp [redrive, unsettled, current.1, current.2, Record.pending] at settled

theorem fresh_record_is_pending (kind : Kind) (actor : Option String) (runner : String)
    (key : Key) : (Record.fresh kind actor runner key).pending = true := rfl

/-! Re-deriving owed halts from durable core state. -/

/-- A session as the recovery sweep sees it through core's rows: the Agent that
owns it, its halt latch (the stop latch, or the interrupt latch of its newest
request) and whether that halt is still current (no resume cleared the stop
latch and no later request was created). -/
structure HaltedSession where
  session : String
  owner : Option String
  latchedAt : Option String
  current : Bool
  deriving DecidableEq, Repr

def HaltedSession.derivable (session : HaltedSession) : Bool :=
  session.owner.isSome && session.latchedAt.isSome && session.current

/-- The halts one sweep redrives from core state alone: each derivable session,
at most `bound` of them, as (session, halt time, owning Agent). The halt
listeners then run as a redrive under that Agent; the Goals listener is inert
for a Goal that is not active, so a halt whose staging did commit costs only
the reads. -/
def deriveHalts (bound : Nat) (sessions : List HaltedSession) :
    List (String × String × String) :=
  ((sessions.filter HaltedSession.derivable).take bound).filterMap fun session =>
    match session.owner, session.latchedAt with
    | some owner, some time => some (session.session, time, owner)
    | _, _ => none

theorem derived_halts_are_bounded (bound : Nat) (sessions : List HaltedSession) :
    (deriveHalts bound sessions).length ≤ bound := by
  unfold deriveHalts
  exact (List.length_filterMap_le _ _).trans (List.length_take_le _ _)

/-- A halt a resume or a later request superseded is never derived. -/
theorem superseded_halt_is_not_derived (bound : Nat) (sessions : List HaltedSession)
    (session : HaltedSession) (superseded : session.current = false) (time owner : String)
    (derived : (session.session, time, owner) ∈ deriveHalts bound sessions)
    (unique : ∀ other ∈ sessions, other.session = session.session → other = session) :
    False := by
  unfold deriveHalts at derived
  obtain ⟨candidate, member, mapped⟩ := List.mem_filterMap.mp derived
  have inFilter := List.mem_of_mem_take member
  obtain ⟨original, kept⟩ := List.mem_filter.mp inFilter
  cases owned : candidate.owner <;> cases latched : candidate.latchedAt <;>
    simp [owned, latched] at mapped
  have same := unique candidate original (by rw [← mapped.1])
  subst same
  simp [HaltedSession.derivable, superseded] at kept

/-- A halt whose record the publication window lost is re-derived while it is
current, its session's owner and latch are visible, and it is among the first
`bound` derivable sessions (derivable includes current). -/
theorem lost_halt_is_rederived (bound : Nat) (sessions : List HaltedSession)
    (session : HaltedSession) (owner time : String) (owned : session.owner = some owner)
    (latched : session.latchedAt = some time)
    (within : session ∈ (sessions.filter HaltedSession.derivable).take bound) :
    (session.session, time, owner) ∈ deriveHalts bound sessions := by
  unfold deriveHalts
  exact List.mem_filterMap.mpr ⟨session, within, by simp [owned, latched]⟩

/-! Pruning settled records. -/

/-- A record the sweep deletes, as the node key: settled for longer than the
retention. A pending record is never pruned. Within the retention a settled
record still answers a duplicate publication of its key, which happens right
after the owing commit, long before the retention ends. -/
def prunable (record : Record) (age retention : Nat) : Bool :=
  record.settled.isSome && decide (retention < age)

def prune (records : List (Record × Nat)) (retention : Nat) : List (Record × Nat) :=
  records.filter fun (record, age) => !prunable record age retention

theorem prune_keeps_pending (records : List (Record × Nat)) (retention : Nat)
    (record : Record) (age : Nat) (member : (record, age) ∈ records)
    (pending : record.pending = true) : (record, age) ∈ prune records retention := by
  have unsettled : record.settled = none := by
    simpa [Record.pending, Option.isNone_iff_eq_none] using pending
  simp [prune, prunable, unsettled, member]

theorem prune_removes_only_expired_settled (records : List (Record × Nat)) (retention : Nat)
    (record : Record) (age : Nat) (member : (record, age) ∈ records)
    (removed : (record, age) ∉ prune records retention) :
    record.settled.isSome = true ∧ retention < age := by
  by_contra kept
  apply removed
  simp only [prune, List.mem_filter]
  refine ⟨member, ?_⟩
  simp only [prunable, Bool.not_and, Bool.or_eq_true, Bool.not_eq_true', decide_eq_false_iff_not]
  by_cases settled : record.settled.isSome = true
  · exact Or.inr (fun later => kept ⟨settled, later⟩)
  · exact Or.inl (by simpa using settled)

end Hooks.OwedDelivery
