/-! A read of the published plugin store. Only an explicit bootstrap at node
start (or an installer) creates the store; a read never does, so a deleted or
replaced home is never recreated by a reader. External premise: an open
directory handle keeps its inode identity across unlink and rename, an
`openat` without `O_CREAT` is anchored to that directory and creates nothing,
and a shared advisory lock contends with an installer's exclusive one. -/

namespace PluginStoreHome

inductive Outcome where
  | acquired | missing | replaced | busy
  deriving DecidableEq, Repr

/-- What the reader observed: whether the home, store directory and lock file
are still the ones it captured, whether they exist, and whether the shared
lock attempt met an installer's exclusive lock. -/
def acquire (sameHome storePresent lockPresent contended : Bool) : Outcome :=
  if !sameHome then .replaced
  else if !storePresent || !lockPresent then .missing
  else if contended then .busy
  else .acquired

theorem a_replaced_home_is_never_read (storePresent lockPresent contended : Bool) :
    acquire false storePresent lockPresent contended = .replaced := rfl

theorem an_absent_store_is_not_contention (lockPresent contended : Bool) :
    acquire true false lockPresent contended = .missing := by
  cases lockPresent <;> cases contended <;> rfl

theorem only_a_present_uncontended_store_is_acquired
    (sameHome storePresent lockPresent contended : Bool) :
    acquire sameHome storePresent lockPresent contended = .acquired ↔
      sameHome = true ∧ storePresent = true ∧ lockPresent = true ∧ contended = false := by
  cases sameHome <;> cases storePresent <;> cases lockPresent <;> cases contended <;> decide

end PluginStoreHome
