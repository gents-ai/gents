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

/-- Who takes the store's exclusive lock. A `runtime` writer pinned its home
when it started and must never write into another home at the same path; an
`offline` writer (the CLI without a runtime) names its home on each call and
may initialize a new one. -/
inductive Writer where
  | offline | runtime
  deriving DecidableEq, Repr

inductive WriteOutcome where
  | acquired | created | missing | replaced | busy
  deriving DecidableEq, Repr

/-- What a writer observed after its exclusive lock attempt: whether a home
is at its path, whether that home (and the store a reader pinned in it, if
any) is the one the runtime pinned, whether a reader pinned a store, whether
the store existed before the attempt, and whether the lock met another
holder. A runtime writer gets `missing` for a removed home or a removed
pinned store and `replaced` for another home at its path, and creates the
store only in its own home and only when no reader has pinned one there; an
offline writer creates the store in the home it names. Absence and
replacement are decided before contention, so neither is ever `busy`.
External premise: directories are created and the lock file opened relative
to the pinned home's open handle (`mkdirat`, `openat(O_CREAT)`), and the
observation is repeated after the lock is taken. -/
def write (writer : Writer) (homePresent sameHome storePinned storePresent contended : Bool) :
    WriteOutcome :=
  match writer with
  | .runtime =>
    if !homePresent then .missing
    else if !sameHome then .replaced
    else if storePinned && !storePresent then .missing
    else if contended then .busy
    else if !storePresent then .created
    else .acquired
  | .offline =>
    if contended then .busy
    else if !storePresent then .created
    else .acquired

theorem a_runtime_writer_never_writes_a_removed_home
    (sameHome storePinned storePresent contended : Bool) :
    write .runtime false sameHome storePinned storePresent contended = .missing := rfl

theorem a_runtime_writer_never_writes_a_replaced_home
    (storePinned storePresent contended : Bool) :
    write .runtime true false storePinned storePresent contended = .replaced := rfl

theorem a_runtime_writer_creates_only_an_unpinned_store_in_its_own_home
    (homePresent sameHome storePinned storePresent contended : Bool) :
    write .runtime homePresent sameHome storePinned storePresent contended = .created ↔
      homePresent = true ∧ sameHome = true ∧ storePinned = false ∧ storePresent = false ∧
        contended = false := by
  cases homePresent <;> cases sameHome <;> cases storePinned <;> cases storePresent <;>
    cases contended <;> decide

theorem absence_and_replacement_are_never_contention
    (writer : Writer) (homePresent sameHome storePinned storePresent contended : Bool) :
    write writer homePresent sameHome storePinned storePresent contended = .busy →
      contended = true ∧ (writer = .runtime → homePresent = true ∧ sameHome = true) := by
  cases writer <;> cases homePresent <;> cases sameHome <;> cases storePinned <;>
    cases storePresent <;> cases contended <;> decide

theorem an_offline_writer_initializes_the_home_it_names
    (homePresent sameHome storePinned : Bool) :
    write .offline homePresent sameHome storePinned false false = .created := rfl

end PluginStoreHome
