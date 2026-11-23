import Proofs.Session.SessionQueue.State

namespace SessionQueue

/-- A direct steer may pass ordinary queued turns without moving or consuming them.
Earlier steering inputs retain their own order and block later steers when incompatible. -/
inductive BoundaryInputKind where
  | queued | userSteer | agentSteer | blocked
  deriving DecidableEq, Repr

def boundaryCandidate : List (Nat × BoundaryInputKind) → Option Nat
  | [] => none
  | (_, .queued) :: rest => boundaryCandidate rest
  | (id, .agentSteer) :: _ => some id
  | (id, .userSteer) :: _ => some id
  | (_, .blocked) :: _ => none

def steersInto (active candidate : QueueEntry) : Prop :=
  candidate.fresh = true ∧ candidate.delivery = .steer ∧ candidate.policy = .append ∧
  (candidate.source = .user ∨ candidate.source = .steering) ∧
  candidate.origin = .interactive ∧ candidate.requester = active.requester ∧
  candidate.turnContext = active.turnContext ∧
  candidate.goalAssignment = active.goalAssignment

instance (active candidate : QueueEntry) : Decidable (steersInto active candidate) := by
  unfold steersInto
  infer_instance

/-- How the intake boundary of the active turn sees one pending entry. An
ordinary queued turn is passed whatever its freshness or author; a steer the
active turn may take is a candidate; anything else stops intake. -/
def boundaryKind (active : QueueEntry) (admitted : List RequestId) (entry : QueueEntry) :
    BoundaryInputKind :=
  if entry.delivery = .queue ∧ entry.policy = .append ∧ entry.appendWellFormed ∧
      entry.origin = .interactive then .queued
  else if steersInto active entry ∧ entry.requestId ∈ admitted then
    if entry.source = .user then .userSteer else .agentSteer
  else .blocked

/-- The entries intake selects, in created order: each is the boundary
candidate of the queue left once the earlier ones are removed
(`steeringRun_head_is_boundaryCandidate`). -/
def steeringRun (active : QueueEntry) (admitted : List RequestId) :
    List QueueEntry → List QueueEntry
  | [] => []
  | entry :: rest =>
      match boundaryKind active admitted entry with
      | .queued => steeringRun active admitted rest
      | .blocked => []
      | .userSteer | .agentSteer => entry :: steeringRun active admitted rest

/-- The entries intake leaves pending, in their created order. -/
def steeringRest (active : QueueEntry) (admitted : List RequestId) :
    List QueueEntry → List QueueEntry
  | [] => []
  | entry :: rest =>
      match boundaryKind active admitted entry with
      | .queued => entry :: steeringRest active admitted rest
      | .blocked => entry :: rest
      | .userSteer | .agentSteer => steeringRest active admitted rest

theorem steeringRun_head_is_boundaryCandidate (active : QueueEntry) (admitted : List RequestId)
    (entries : List QueueEntry) :
    (steeringRun active admitted entries).head?.map (·.requestId) =
      boundaryCandidate (entries.map fun entry => (entry.requestId, boundaryKind active admitted entry)) := by
  induction entries with
  | nil => rfl
  | cons entry rest ih =>
      cases h : boundaryKind active admitted entry <;>
        simp [steeringRun, boundaryCandidate, h, ih]

theorem boundaryKind_queued {active entry : QueueEntry} {admitted : List RequestId}
    (h : boundaryKind active admitted entry = .queued) : entry.delivery = .queue := by
  unfold boundaryKind at h
  split at h
  · rename_i hc
    exact hc.1
  · split at h
    · split at h <;> cases h
    · cases h

theorem boundaryKind_steer {active entry : QueueEntry} {admitted : List RequestId}
    (h : boundaryKind active admitted entry = .userSteer ∨
      boundaryKind active admitted entry = .agentSteer) :
    steersInto active entry ∧ entry.requestId ∈ admitted := by
  unfold boundaryKind at h
  split at h
  · simp at h
  · split at h
    · rename_i hc
      exact hc
    · simp at h

theorem steeringRun_steers {active : QueueEntry} {admitted : List RequestId}
    {entries : List QueueEntry} {selected : QueueEntry}
    (h : selected ∈ steeringRun active admitted entries) :
    steersInto active selected ∧ selected.requestId ∈ admitted := by
  induction entries with
  | nil => simp [steeringRun] at h
  | cons entry rest ih =>
      cases hk : boundaryKind active admitted entry <;> simp only [steeringRun, hk] at h
      · exact ih h
      · rcases List.mem_cons.mp h with rfl | h
        · exact boundaryKind_steer (Or.inl hk)
        · exact ih h
      · rcases List.mem_cons.mp h with rfl | h
        · exact boundaryKind_steer (Or.inr hk)
        · exact ih h
      · simp at h

theorem steeringRun_append_rest_perm (active : QueueEntry) (admitted : List RequestId)
    (entries : List QueueEntry) :
    (steeringRun active admitted entries ++ steeringRest active admitted entries).Perm entries := by
  induction entries with
  | nil => exact List.Perm.refl _
  | cons entry rest ih =>
      cases hk : boundaryKind active admitted entry <;>
        simp only [steeringRun, steeringRest, hk, List.cons_append, List.nil_append]
      · exact List.perm_middle.trans (ih.cons entry)
      · exact (ih.cons entry)
      · exact (ih.cons entry)
      · exact List.Perm.refl _

theorem steeringRun_sublist (active : QueueEntry) (admitted : List RequestId)
    (entries : List QueueEntry) : (steeringRun active admitted entries).Sublist entries := by
  induction entries with
  | nil => exact List.Sublist.slnil
  | cons entry rest ih =>
      cases hk : boundaryKind active admitted entry <;> simp only [steeringRun, hk]
      · exact ih.cons entry
      · exact ih.cons₂ entry
      · exact ih.cons₂ entry
      · exact List.nil_sublist _

theorem steeringRest_sublist (active : QueueEntry) (admitted : List RequestId)
    (entries : List QueueEntry) : (steeringRest active admitted entries).Sublist entries := by
  induction entries with
  | nil => exact List.Sublist.slnil
  | cons entry rest ih =>
      cases hk : boundaryKind active admitted entry <;> simp only [steeringRest, hk]
      · exact ih.cons₂ entry
      · exact ih.cons entry
      · exact ih.cons entry
      · exact List.Sublist.refl _

/-- Intake passes only ordinary queued turns: whatever it leaves pending ahead
of a selected entry has queue delivery. -/
theorem steeringRest_before_selected_is_queued (active : QueueEntry) (admitted : List RequestId)
    (entries : List QueueEntry) (h_order : CreatedOrdered entries) :
    ∀ kept ∈ steeringRest active admitted entries, ∀ selected ∈ steeringRun active admitted entries,
      kept.orderKey < selected.orderKey → kept.delivery = .queue := by
  induction entries with
  | nil => intro kept hk; simp [steeringRest] at hk
  | cons entry rest ih =>
      simp only [CreatedOrdered] at h_order
      intro kept h_kept selected h_selected h_lt
      cases hk : boundaryKind active admitted entry <;>
        simp only [steeringRun, steeringRest, hk] at h_kept h_selected
      · rcases List.mem_cons.mp h_kept with rfl | h_kept
        · exact boundaryKind_queued hk
        · exact ih h_order.2 kept h_kept selected h_selected h_lt
      · rcases List.mem_cons.mp h_selected with rfl | h_selected
        · exact absurd h_lt (Nat.not_lt.mpr
            (h_order.1 kept ((steeringRest_sublist active admitted rest).subset h_kept)))
        · exact ih h_order.2 kept h_kept selected h_selected h_lt
      · rcases List.mem_cons.mp h_selected with rfl | h_selected
        · exact absurd h_lt (Nat.not_lt.mpr
            (h_order.1 kept ((steeringRest_sublist active admitted rest).subset h_kept)))
        · exact ih h_order.2 kept h_kept selected h_selected h_lt
      · simp at h_selected

end SessionQueue
