import Proofs.Session.SessionQueue.Executable
import Proofs.Store.TransactionFence

/-!
# Natural finish

The active turn finishes naturally against the pending queue its finishing
transaction reads: if that read differs from the snapshot its intake saw, the
turn takes the new input instead of finishing. The read is a filtered read,
so an input that commits after it and before the finish commits is not a
conflict (`TransactionFence.a_filtered_read_does_not_fence_an_arrival`), and
the finish commits over it. `finishActive` keeps every queued entry pending in
its created place, so that input is an ordinary later turn: it is neither
lost nor taken into the finished turn (`QueuedSteering.agentSteerTargets`).
-/

namespace SessionQueue

inductive NaturalFinish where
  | finish
  | intake
  deriving DecidableEq, Repr

def naturalFinish (snapshot observed : List RequestId) : NaturalFinish :=
  if observed = snapshot then .finish else .intake

theorem changed_queue_is_taken_in (snapshot observed : List RequestId)
    (h : observed ≠ snapshot) : naturalFinish snapshot observed = .intake := by
  simp [naturalFinish, h]

/-- An input appended after the finish's read, while the turn is active, is
still pending once the finish commits, and nothing is active: it runs as the
next turn the claim gate admits. -/
theorem input_after_the_finish_read_is_a_later_turn
    (pre mid post : SessionQueueState) (entry : QueueEntry)
    (h_append : step? pre (.appendPending entry) = some mid)
    (h_finish : step? mid .finishActive = some post) :
    entry ∈ post.pending ∧ post.active = none := by
  simp only [step?] at h_append h_finish
  split at h_append
  · cases h_append
    split at h_finish
    · cases h_finish
      simp [SessionQueueState.finishActive, SessionQueueState.queued, mergeCreated,
        SessionQueueState.appendPending, List.mem_merge]
    · contradiction
  · contradiction

end SessionQueue
