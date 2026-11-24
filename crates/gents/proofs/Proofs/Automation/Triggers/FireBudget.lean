import Proofs.Automation.EventDelivery.Durable
import Proofs.Store.TransactionFence

/-! Rolling fire budget. Admission counts its own receipts in the window, so
budget accounting commits atomically with the receipt it limits. That count is
a filtered read the store does not validate, so two admissions of one Trigger
running at once could each count the other's receipt as absent
(`count_by_scan_over_admits`). Each receipt therefore takes `fire_sequence` one
past the receipts its admission read, and `(owner_did, trigger_id,
fire_sequence)` is a unique index: an admission commits only on a read of every
receipt committed before it (`TransactionFence.uniqueIndex`), and concurrent
admissions keep the bound (`concurrent_admission_preserves_bound`). Exhaustion stops the Trigger through `enabled`,
the existing owner of "no further delivery": the cursor keeps its position and
re-enabling resumes exactly the undelivered arrivals. Requests already admitted
are not this budget's concern; gents-ai/gents#3548 owns stopping an accepted
backlog. The budget bounds that backlog at admission.

Natively the admitting Agent decides exhaustion from its own receipts, and the
owner of the Trigger's runtime fields records the stop: document ACP gives that
Agent no write on the Trigger (`configuration.update` is its writers and its
Node). A stop lost between the two is decided again by the next fire, which
admits nothing either. -/
namespace Triggers.FireBudget

open Triggers.Durable

def windowSeconds : Nat := 3600

/-- One fire every 3.6 s sustained for an hour is more than a single normal
Trigger delivers, and well under the ~2,700 fires per Trigger that took a node
down. A pack that legitimately needs more declares `max_fires_per_hour`. -/
def defaultMaxPerHour : Nat := 1000

def effective (configured : Option Nat) : Nat := configured.getD defaultMaxPerHour

/-- The budget counts admissions, so it is valid only where a fire is counted:
an event source or a targeted schedule writes a receipt, and a command Task's
fire is counted by the engine that runs it. An untargeted schedule of an Agent's
Task is bound by its cadence and cannot amplify itself. A command Task's schedule
takes the same budget as an Agent's: its command runs as trusted Node code, so a
misfiring schedule there is more dangerous, not less (decided on
gents-ai/gents#3955). -/
def configValid (configured : Option Nat) (receipted command : Bool) : Bool :=
  configured.all (0 < ·) && (configured.isNone || receipted || command)

/-- A command Task's schedule may set its budget whether or not it is targeted. -/
theorem command_schedule_takes_a_budget (budget : Nat) (positive : 0 < budget)
    (receipted : Bool) : configValid (some budget) receipted true = true := by
  simp [configValid, positive]

def exhaustedStatus : String := "budget_exhausted"

inductive Decision where
  | admit
  | exhausted
  deriving DecidableEq, Repr

def decideFire (budget admittedInWindow : Nat) : Decision :=
  if admittedInWindow < budget then .admit else .exhausted

/-- A receipt at exactly `now - windowSeconds` is outside the window, matching
the strict `created_at > now - 1h` receipt query. -/
def windowCount (admittedAt : List Nat) (now : Nat) : Nat :=
  (admittedAt.filter fun time => now < time + windowSeconds).length

structure Trigger where
  enabled : Bool := true
  lastStatus : Option String := none
  lastError : Option String := none
  admittedAt : List Nat := []
  deriving DecidableEq, Repr

def stop (trigger : Trigger) (reason : String) : Trigger :=
  { trigger with enabled := false, lastStatus := some exhaustedStatus, lastError := some reason }

def admitBudgeted (state : State) (trigger : Trigger) (configured : Option Nat)
    (reason : String) (now : Nat) (fire : Fire) : State × Trigger :=
  if !trigger.enabled || admitted state fire.identity then (state, trigger)
  else
    match decideFire (effective configured) (windowCount trigger.admittedAt now) with
    | .admit => (admit state fire, { trigger with admittedAt := now :: trigger.admittedAt })
    | .exhausted => (state, stop trigger reason)

theorem replay_spends_no_budget (state : State) (trigger : Trigger) (configured : Option Nat)
    (reason : String) (now : Nat) (fire : Fire) (h : admitted state fire.identity = true) :
    admitBudgeted state trigger configured reason now fire = (state, trigger) := by
  simp [admitBudgeted, h]

theorem within_budget_admits (state : State) (trigger : Trigger) (configured : Option Nat)
    (reason : String) (now : Nat) (fire : Fire) (enabled : trigger.enabled = true)
    (fresh : admitted state fire.identity = false)
    (under : windowCount trigger.admittedAt now < effective configured) :
    (admitBudgeted state trigger configured reason now fire).1 = admit state fire := by
  simp [admitBudgeted, enabled, fresh, decideFire, under]

/-- A normal Trigger, well under its budget, is never stopped. -/
theorem under_budget_keeps_trigger_enabled (state : State) (trigger : Trigger)
    (configured : Option Nat) (reason : String) (now : Nat) (fire : Fire)
    (under : windowCount trigger.admittedAt now < effective configured) :
    (admitBudgeted state trigger configured reason now fire).2.enabled = trigger.enabled := by
  unfold admitBudgeted
  split
  · rfl
  · simp [decideFire, under]

theorem exhaustion_admits_nothing (state : State) (trigger : Trigger) (configured : Option Nat)
    (reason : String) (now : Nat) (fire : Fire)
    (over : effective configured ≤ windowCount trigger.admittedAt now) :
    (admitBudgeted state trigger configured reason now fire).1 = state := by
  unfold admitBudgeted
  split
  · rfl
  · simp [decideFire, Nat.not_lt.mpr over]

theorem exhaustion_records_why (state : State) (trigger : Trigger) (configured : Option Nat)
    (reason : String) (now : Nat) (fire : Fire) (enabled : trigger.enabled = true)
    (fresh : admitted state fire.identity = false)
    (over : effective configured ≤ windowCount trigger.admittedAt now) :
    (admitBudgeted state trigger configured reason now fire).2 =
      stop trigger reason := by
  simp [admitBudgeted, enabled, fresh, decideFire, Nat.not_lt.mpr over]

theorem stop_disables_with_reason (trigger : Trigger) (reason : String) :
    (stop trigger reason).enabled = false ∧
      (stop trigger reason).lastStatus = some exhaustedStatus ∧
      (stop trigger reason).lastError = some reason := ⟨rfl, rfl, rfl⟩

/-- A stopped Trigger receives no further arrivals; its cursor is unchanged. -/
theorem stopped_trigger_delivers_nothing (trigger : Trigger) (reason : String)
    (cursor : EventDelivery.Durable.Cursor) (source : List EventDelivery.Durable.Arrival) :
    EventDelivery.Durable.pending cursor source (stop trigger reason).enabled = [] := by
  simp [stop, EventDelivery.Durable.pending]

/-- Re-enabling delivers exactly what the stop left undelivered. -/
theorem reenable_after_stop_resumes_cursor (cursor : EventDelivery.Durable.Cursor)
    (source : List EventDelivery.Durable.Arrival) (seeded : cursor.seeded = true) :
    EventDelivery.Durable.pending cursor source true =
      source.filter fun entry => cursor.after < entry.position := by
  simp [EventDelivery.Durable.pending, seeded]

theorem windowCount_antitone (times : List Nat) {earlier later : Nat} (h : earlier ≤ later) :
    windowCount times later ≤ windowCount times earlier := by
  apply List.Sublist.length_le
  apply List.monotone_filter_right
  intro time hlater
  simp only [decide_eq_true_eq] at hlater ⊢
  omega

def Bounded (trigger : Trigger) (budget now : Nat) : Prop :=
  (∀ time ∈ trigger.admittedAt, time ≤ now) ∧ windowCount trigger.admittedAt now ≤ budget

theorem admission_preserves_bound (state : State) (trigger : Trigger) (configured : Option Nat)
    (reason : String) (earlier now : Nat) (fire : Fire)
    (bounded : Bounded trigger (effective configured) earlier) (forward : earlier ≤ now) :
    Bounded (admitBudgeted state trigger configured reason now fire).2 (effective configured) now := by
  obtain ⟨past, within⟩ := bounded
  have shifted : windowCount trigger.admittedAt now ≤ effective configured :=
    Nat.le_trans (windowCount_antitone _ forward) within
  have monotone : ∀ time ∈ trigger.admittedAt, time ≤ now :=
    fun time mem => Nat.le_trans (past time mem) forward
  unfold admitBudgeted
  split
  · exact ⟨monotone, shifted⟩
  · cases hdecide : decideFire (effective configured) (windowCount trigger.admittedAt now) with
    | admit =>
        have under : windowCount trigger.admittedAt now < effective configured := by
          by_contra over
          simp [decideFire, over] at hdecide
        refine ⟨?_, ?_⟩
        · intro time mem
          simp only [hdecide, List.mem_cons] at mem
          rcases mem with rfl | old
          · exact Nat.le_refl _
          · exact monotone time old
        · simp only [hdecide]
          have count : windowCount (now :: trigger.admittedAt) now =
              windowCount trigger.admittedAt now + 1 := by
            simp [windowCount, List.filter_cons, windowSeconds]
          simp only [count]
          omega
    | exhausted =>
        simp only [hdecide, stop]
        exact ⟨monotone, shifted⟩

def run (state : State) (trigger : Trigger) (configured : Option Nat) (reason : String) :
    List (Nat × Fire) → State × Trigger
  | [] => (state, trigger)
  | (now, fire) :: rest =>
      let next := admitBudgeted state trigger configured reason now fire
      run next.1 next.2 configured reason rest

/-- However many fires a runaway presents, in time order, a Trigger never holds
more admissions in a rolling window than its budget. -/
theorem trace_stays_within_budget (state : State) (trigger : Trigger) (configured : Option Nat)
    (reason : String) (start : Nat) (trace : List (Nat × Fire))
    (bounded : Bounded trigger (effective configured) start)
    (ordered : (start :: trace.map Prod.fst).Pairwise (· ≤ ·)) :
    let final := run state trigger configured reason trace
    ∀ now, (∀ entry ∈ trace, entry.1 ≤ now) → start ≤ now →
      windowCount final.2.admittedAt now ≤ effective configured := by
  induction trace generalizing state trigger start with
  | nil =>
      intro final now _ forward
      exact Nat.le_trans (windowCount_antitone _ forward) bounded.2
  | cons head tail ih =>
      obtain ⟨time, fire⟩ := head
      intro final now hall forward
      have hpair := List.pairwise_cons.mp ordered
      have step : start ≤ time := hpair.1 time (by simp)
      have next := admission_preserves_bound state trigger configured reason start time fire
        bounded step
      have rest : (time :: tail.map Prod.fst).Pairwise (· ≤ ·) := by
        simpa using hpair.2
      exact ih _ _ time next rest now
        (fun entry mem => hall entry (List.mem_cons_of_mem _ mem))
        (hall (time, fire) (by simp))

/-- The admission of one fire on the receipts `seen` it read. -/
def admitOn (trigger : Trigger) (configured : Option Nat) (reason : String) (now : Nat)
    (seen : List Nat) : Trigger :=
  match decideFire (effective configured) (windowCount seen now) with
  | .admit => { trigger with admittedAt := now :: trigger.admittedAt }
  | .exhausted => stop trigger reason

theorem admission_on_every_receipt_preserves_bound (trigger : Trigger)
    (configured : Option Nat) (reason : String) (earlier now : Nat)
    (bounded : Bounded trigger (effective configured) earlier) (forward : earlier ≤ now) :
    Bounded (admitOn trigger configured reason now trigger.admittedAt)
      (effective configured) now := by
  obtain ⟨past, within⟩ := bounded
  have shifted : windowCount trigger.admittedAt now ≤ effective configured :=
    Nat.le_trans (windowCount_antitone _ forward) within
  have monotone : ∀ time ∈ trigger.admittedAt, time ≤ now :=
    fun time mem => Nat.le_trans (past time mem) forward
  unfold admitOn
  cases hdecide : decideFire (effective configured) (windowCount trigger.admittedAt now) with
  | admit =>
      have under : windowCount trigger.admittedAt now < effective configured := by
        by_contra over
        simp [decideFire, over] at hdecide
      refine ⟨?_, ?_⟩
      · intro time mem
        simp only [List.mem_cons] at mem
        rcases mem with rfl | old
        · exact Nat.le_refl _
        · exact monotone time old
      · have count : windowCount (now :: trigger.admittedAt) now =
            windowCount trigger.admittedAt now + 1 := by
          simp [windowCount, List.filter_cons, windowSeconds]
        show windowCount (now :: trigger.admittedAt) now ≤ effective configured
        rw [count]
        omega
  | exhausted => exact ⟨monotone, shifted⟩

/-- An admission's commit when other admissions of the Trigger run at once.
It read `seen`, the receipts committed before its read, and inserts its
receipt at sequence `seen.length`; the receipts of a Trigger hold the dense
sequences below `trigger.admittedAt.length`, so the unique index admits that
sequence only when no receipt committed after the read. An aborted commit is
`none`: its owner runs the admission again on a fresh read. -/
def commitConcurrent (trigger : Trigger) (configured : Option Nat) (reason : String)
    (now : Nat) (seen : List Nat) : Option Trigger :=
  if seen.length = trigger.admittedAt.length then
    some (admitOn trigger configured reason now seen)
  else none

/-- What an admission reads is a suffix of what is committed when it commits:
receipts are only ever added at the front. A commit the unique sequence lets
through therefore decided on exactly the committed receipts. -/
theorem committed_admission_read_every_receipt (trigger : Trigger)
    (configured : Option Nat) (reason : String) (now : Nat) (seen : List Nat)
    (after : Trigger) (read : seen <:+ trigger.admittedAt)
    (h : commitConcurrent trigger configured reason now seen = some after) :
    seen = trigger.admittedAt := by
  unfold commitConcurrent at h
  split at h
  · exact read.eq_of_length (by assumption)
  · contradiction

/-- However admissions of one Trigger interleave, every commit keeps the
rolling window within the budget. -/
theorem concurrent_admission_preserves_bound (trigger : Trigger) (configured : Option Nat)
    (reason : String) (earlier now : Nat) (seen : List Nat) (after : Trigger)
    (bounded : Bounded trigger (effective configured) earlier) (forward : earlier ≤ now)
    (read : seen <:+ trigger.admittedAt)
    (h : commitConcurrent trigger configured reason now seen = some after) :
    Bounded after (effective configured) now := by
  have same := committed_admission_read_every_receipt trigger configured reason now seen after
    read h
  subst same
  simp only [commitConcurrent, if_true, Option.some.injEq] at h
  subst h
  exact admission_on_every_receipt_preserves_bound trigger configured reason earlier now
    bounded forward

/-- Without the unique sequence an admission commits on whatever it read: two
admissions that each read one receipt under a budget of two both commit, and
three receipts sit in the window. -/
theorem count_by_scan_over_admits :
    let seen := [10]
    let first := admitOn { admittedAt := seen } (some 2) "" 20 seen
    let second := admitOn first (some 2) "" 20 seen
    windowCount second.admittedAt 20 = 3 := by decide

theorem the_unique_sequence_aborts_the_stale_twin :
    let seen := [10]
    let first := commitConcurrent { admittedAt := seen } (some 2) "" 20 seen
    (first.bind fun trigger => commitConcurrent trigger (some 2) "" 20 seen) = none := by decide

end Triggers.FireBudget
