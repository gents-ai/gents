import Proofs.CanonicalOutput.Execution.SessionComposition
import Mathlib.Data.Finset.Card

/-!
Local request-worker capacity, distinct from backend inference admission.

A worker is held only by running work. No request parks a worker while it
waits on another session: `create_session`/`send_message` never block their
caller (`Background.ProcessControl.waitAdmissible`), and the started session's
result arrives as a completion notification that wakes the caller later. The
request/lease/tool owners remain authoritative; capacity operations cannot
mutate their worlds.
-/
namespace CanonicalOutput.Execution.WorkerCapacity

abbrev Ticket := DocId × Generation

structure State where
  activeLimit : Nat
  active : Finset Ticket := ∅
  deriving DecidableEq

def Safe (s : State) : Prop :=
  s.active.card ≤ s.activeLimit

def initial (activeLimit : Nat) : State :=
  { activeLimit }

theorem initial_safe (a : Nat) : Safe (initial a) := by
  simp [Safe, initial]

def acquire (s : State) (ticket : Ticket) : Option State :=
  if ticket ∉ s.active ∧ s.active.card < s.activeLimit then
    some { s with active := insert ticket s.active }
  else none

/-- Cleanup does not need a live lease: an expired request must still be able
to release its local resources, without authorizing a durable write. -/
def release (s : State) (ticket : Ticket) : State :=
  { s with active := s.active.erase ticket }

theorem acquire_safe {s t : State} {ticket : Ticket}
    (hs : Safe s) (h : acquire s ticket = some t) : Safe t := by
  unfold acquire at h
  split at h
  next guards =>
    cases Option.some.inj h
    rcases guards with ⟨hout, room⟩
    simp only [Safe, Finset.card_insert_of_not_mem hout]
    omega
  next => contradiction

theorem release_safe {s : State} (hs : Safe s) (ticket : Ticket) :
    Safe (release s ticket) :=
  le_trans Finset.card_erase_le hs

theorem full_capacity_refuses (s : State) (ticket : Ticket)
    (h : s.activeLimit ≤ s.active.card) : acquire s ticket = none := by
  simp [acquire]
  omega

/-- The application may change while workers are held. Its trace cannot spend
or manufacture a local worker ticket. -/
inductive SchedulerTrace : (World × State) → (World × State) → Prop where
  | application {before after : World} (capacity : State)
      (trace : SessionComposition.Trace before after) :
      SchedulerTrace (before, capacity) (after, capacity)
  | acquire {world : World} {before after : State} (ticket : Ticket)
      (h : WorkerCapacity.acquire before ticket = some after) :
      SchedulerTrace (world, before) (world, after)
  | release (world : World) (before : State) (ticket : Ticket) :
      SchedulerTrace (world, before) (world, WorkerCapacity.release before ticket)
  | trans {first second third : World × State} :
      SchedulerTrace first second → SchedulerTrace second third → SchedulerTrace first third

theorem SchedulerTrace.safe {before after : World × State}
    (trace : SchedulerTrace before after) (hs : Safe before.2) : Safe after.2 := by
  induction trace with
  | application _ _ => exact hs
  | acquire ticket h => exact acquire_safe hs h
  | release world before ticket => exact release_safe hs ticket
  | trans _ _ ih₁ ih₂ => exact ih₂ (ih₁ hs)

theorem SchedulerTrace.applicationTrace {before after : World × State}
    (trace : SchedulerTrace before after) :
    SessionComposition.Trace before.1 after.1 := by
  induction trace with
  | application _ application => exact application
  | acquire _ _ => exact .refl _
  | release _ _ _ => exact .refl _
  | trans _ _ ih₁ ih₂ => exact .trans ih₁ ih₂

/-- Capacity one: a second request waits for the first to release its worker. -/
def capacityOneRun : Option (Bool × Bool) := do
  let first ← acquire (initial 1) (10, 7)
  let blocked := (acquire first (11, 8)).isNone
  let second ← acquire (release first (10, 7)) (11, 8)
  pure (blocked, decide ((11, 8) ∈ second.active))

theorem capacity_one_second_request_waits_for_release :
    capacityOneRun = some (true, true) := by native_decide

end CanonicalOutput.Execution.WorkerCapacity
