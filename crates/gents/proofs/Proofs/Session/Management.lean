import Proofs.Session.Executable

namespace SessionQueue

/-- Both interactive user input and agent messages may steer, but only after
native admission has verified the same requester and execution context. -/
def steersInto (active candidate : QueueEntry) : Prop :=
  candidate.fresh = true ∧ candidate.delivery = .steer ∧ candidate.policy = .append ∧
  (candidate.source = .user ∨ candidate.source = .steering) ∧
  candidate.origin = .interactive ∧ candidate.requester = active.requester ∧
  candidate.turnContext = active.turnContext

instance (active candidate : QueueEntry) : Decidable (steersInto active candidate) := by
  unfold steersInto
  infer_instance

def steeringRun (active : QueueEntry) (admitted : List RequestId) :
    List QueueEntry → List QueueEntry
  | [] => []
  | entry :: rest =>
      if steersInto active entry ∧ entry.requestId ∈ admitted then
        entry :: steeringRun active admitted rest
      else []

/-- A safe boundary is after the previous provider/tool round has settled and
before the next request is sized or compacted. The native execution owner
supplies the exact active row and generation; this operation grants no lease. -/
def intakeSteering? (target : AgentSession.Scope) (pre : SessionQueueState)
    (active : QueueEntry) (admitted : List RequestId) (safeBoundary : Bool) :
    Option SessionQueueState :=
  if pre.scope = target ∧ pre.active = some active.requestId ∧
      pre.folding = [] ∧ safeBoundary then
    let selected := steeringRun active admitted pre.pending
    some { pre with folding := selected, pending := pre.pending.drop selected.length }
  else none

/-- Replacement observes the entire pending order so a concurrent append,
claim, intake or prior edit invalidates it. Only a contiguous group belonging
to the caller is replaceable; fresh admissions inherit its signed slots.
An empty replacement cancels the group. Selection is provisional until
authored consumption: an edit invalidates it and restores the remaining order.
The active request and already-consumed input cannot be edited. -/
private def replacementAllowed (target : AgentSession.Scope) (pre : SessionQueueState)
    (caller : Option Nat) (expected : List RequestId) (offset count : Nat)
    (replacements : List QueueEntry) : Bool :=
  let pending := pre.folding ++ pre.pending
  let old := (pending.drop offset).take count
  let fresh := replacements.map (·.requestId)
  decide (pre.scope = target) && decide (pending.map (·.requestId) = expected) &&
      decide (count > 0) && decide (old.length = count) &&
      old.all (fun entry => decide (entry.source = .user ∧ entry.policy = .append ∧
        entry.origin = .interactive ∧ entry.requester = caller ∧ entry.fresh = true)) &&
      (match old.head? with
        | none => true
        | some first => old.all (fun entry => decide (entry.turnContext = first.turnContext))) &&
      decide (replacements = [] ∨ replacements.length = count) && decide fresh.Nodup &&
      replacements.all (fun entry => decide (RequestIdFresh pre entry)) &&
      (old.zip replacements).all (fun pair => decide (
        pair.2.source = .user ∧ pair.2.policy = .append ∧
        pair.2.origin = .interactive ∧ pair.2.requester = caller ∧
        pair.2.turnContext = pair.1.turnContext ∧ pair.2.delivery = pair.1.delivery ∧
        pair.2.orderKey = pair.1.orderKey))

def replacePendingGroup? (target : AgentSession.Scope) (pre : SessionQueueState)
    (caller : Option Nat) (expected : List RequestId) (offset count : Nat)
    (replacements : List QueueEntry) : Option SessionQueueState :=
  let pending := pre.folding ++ pre.pending
  let old := (pending.drop offset).take count
  if replacementAllowed target pre caller expected offset count replacements then
    some { pre with
      pending := pending.take offset ++ replacements ++ pending.drop (offset + count)
      folding := []
      terminal := pre.terminal ∪ (old.map (·.requestId)).toFinset }
  else none

theorem intake_preserves_active {target : AgentSession.Scope} {pre post : SessionQueueState}
    {active : QueueEntry} {admitted : List RequestId} {safe : Bool}
    (h : intakeSteering? target pre active admitted safe = some post) :
    post.active = pre.active ∧ post.terminal = pre.terminal := by
  unfold intakeSteering? at h
  split at h
  · cases h
    exact ⟨rfl, rfl⟩
  · contradiction

theorem unsafe_boundary_refuses (target : AgentSession.Scope) (pre : SessionQueueState)
    (active : QueueEntry) (admitted : List RequestId) :
    intakeSteering? target pre active admitted false = none := by
  simp [intakeSteering?]

theorem queue_delivery_is_barrier (active entry : QueueEntry) (rest : List QueueEntry)
    (admitted : List RequestId) (h : entry.delivery = .queue) :
    steeringRun active admitted (entry :: rest) = [] := by
  simp [steeringRun, steersInto, h]

theorem stale_input_is_barrier (active entry : QueueEntry) (rest : List QueueEntry)
    (admitted : List RequestId) (h : entry.fresh = false) :
    steeringRun active admitted (entry :: rest) = [] := by
  simp [steeringRun, steersInto, h]

theorem replacement_preserves_active_and_invalidates_selection
    {target : AgentSession.Scope} {pre post : SessionQueueState}
    {caller : Option Nat} {expected : List RequestId} {offset count : Nat}
    {replacements : List QueueEntry}
    (h : replacePendingGroup? target pre caller expected offset count replacements = some post) :
    post.active = pre.active ∧ post.folding = [] := by
  unfold replacePendingGroup? at h
  simp only at h
  split at h
  · cases h
    exact ⟨rfl, rfl⟩
  · contradiction

end SessionQueue
