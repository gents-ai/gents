/-! Handing a local store from the process that holds it to the next process
that opens it. The store is excluded by an OS advisory lock on
`<data dir>.lock`, which the kernel drops when its holder exits, so a holder
that died never excludes anyone. The lock file also carries what the holder
last declared: its process id, then, once it has begun to stop, that it is
releasing. The interleaving of a holder and its successors is
`tla/StoreHandoff.tla`; this module is its acquirer's sequential choice
(`Read` there). The holder's order (declare, release, then acknowledge) is
`ClientInstallation.step` for the client installation. -/

namespace StoreHandoff

/-- What a process finds when it tries the store lock without waiting. -/
inductive Observation where
  | free
  /-- Held by another process; `releasing` when that process declared, before
  it began to release the store, that it is stopping. -/
  | held (releasing : Bool)
  deriving DecidableEq, Repr

inductive Decision where
  /-- The try took the lock. -/
  | acquire
  /-- Wait for the holder's release, within the opening operation's own
  deadline; past it, report the holder as still releasing. -/
  | wait
  /-- Refuse at once, naming the holder and how to stop it. -/
  | refuse
  deriving DecidableEq, Repr

/-- A serving holder is a live runtime and is refused at once: waiting for it
would wait for something nobody asked to happen. A holder that declared its
release is in the act of shutting down, so the opener waits on the lock's own
release instead of failing with "already using" (`RefusalNamesLiveHolder`;
the TLC counterexample to refusing every held lock is BeginStop, Start,
TryLock, Read). The wait is bounded because a shutdown can wedge, and a third
process can take the store first (`AcquirerSettles`). -/
def decide : Observation → Decision
  | .free => .acquire
  | .held true => .wait
  | .held false => .refuse

/-- What the holder's lock file says in each of its phases. The declaration is
written before the release begins and never withdrawn, so every read that
finds the lock held by a stopping holder finds it declared. -/
inductive HolderPhase where
  | serving | releasing
  deriving DecidableEq, Repr

def declares : HolderPhase → Bool
  | .serving => false
  | .releasing => true

theorem a_free_store_is_taken : decide .free = .acquire := rfl

theorem a_releasing_holder_is_awaited (phase : HolderPhase) (h : phase = .releasing) :
    decide (.held (declares phase)) = .wait := by
  subst h; rfl

theorem only_a_serving_holder_is_refused (phase : HolderPhase)
    (h : decide (.held (declares phase)) = .refuse) : phase = .serving := by
  cases phase <;> simp_all [decide, declares]

theorem a_held_store_is_never_taken_without_its_release (releasing : Bool) :
    decide (.held releasing) ≠ .acquire := by
  cases releasing <;> simp [decide]

end StoreHandoff
