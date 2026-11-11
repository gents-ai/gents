import Proofs.Automation.EventDelivery.Durable

/-! Rolling fire budget. Admission counts its own receipts in the window, so
budget accounting commits atomically with the receipt it limits. `dispatch` is
sequential per source and a Trigger has one source, so concurrent admissions of
one Trigger are not modeled. Exhaustion stops the Trigger through `enabled`,
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

/-- The budget counts receipts, so it is valid only where a fire writes one: an
event source or a targeted schedule. An untargeted schedule is bound by its
cadence and cannot amplify itself. -/
def configValid (configured : Option Nat) (receipted : Bool) : Bool :=
  configured.all (0 < ·) && (configured.isNone || receipted)

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

end Triggers.FireBudget
