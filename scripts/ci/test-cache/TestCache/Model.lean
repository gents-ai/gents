/-!
The gate's test-result cache: when may a test be skipped because an earlier
run already saw it pass?

The cache is sound when every test it skips would have passed. That cannot be
proved about real programs; it is proved here from premises, each of which the
gate enforces or declares:

* (a) **Determinism.** A test that is not excluded behaves as a function of
  its inputs (`World`). Tests known to violate this (wall-clock windows,
  load-sensitive deadlines, randomness that can decide pass/fail) are listed in
  `scripts/ci/test-cache.toml` and always run (`Policy.excluded`).
* (b) **Complete key.** Every input other than files reaches the test through
  `StaticKey`, and every file the test observes is a query in its recorded
  trace. The tracer enforces the second half on Linux: an access it cannot
  classify makes the execution `traceComplete = false`, and such an execution
  never records.
* (c) **No outside world.** Network, live models and the service manager are
  inputs no key can capture. The tracer refuses non-loopback connections and
  the service manager (again `traceComplete = false`); wall-clock sensitivity
  is not observable and is premise (a)'s declared list.
* (d) **Trust.** Only trusted runs record (`Execution.trusted`). Fork pull
  requests never run on our runners; the store accepts no other writer.
* Content digests are collision-free: the model identifies a digest with the
  content it hashes (SHA-256 in the gate).
-/

namespace TestCache

/-- What a test observes when it asks about one path. The gate's digests:
a file is its git blob id when tracked, its SHA-256 otherwise; a directory is
its sorted entry names; a path that does not exist is `absent`. -/
inductive Answer where
  | file (digest : String)
  | dir (entries : List String)
  | absent
  deriving DecidableEq, Repr

/-- A path in the gate's normalized namespace (`ws:` workspace-relative,
`target:` inside the archive, `host:` absolute on the runner, `tree:` a
derived tree's sources, `up:` an ancestor of the checkout). The gate keys an
ancestor by its kind alone: tests search upward for the workspace root, and
an ancestor's other entries are other checkouts on the runner, which no test
reads; every marker the search probes is its own query. -/
abbrev Path := String

/-- The repository and host as one test sees them. -/
abbrev FileSystem := Path → Answer

/-- A test's behaviour once every non-file input is fixed: it asks about paths
one at a time, each question may depend on earlier answers, and it ends by
passing or failing. Reading a file, listing a directory, probing existence and
executing a program are all queries. -/
inductive Behaviour where
  | finish (passed : Bool)
  | query (path : Path) (next : Answer → Behaviour)

/-- One query and its answer, in the order the test asked. -/
abbrev Trace := List (Path × Answer)

/-- Run a behaviour against a file system: the outcome and every query made. -/
def Behaviour.run : Behaviour → FileSystem → Bool × Trace
  | .finish passed, _ => (passed, [])
  | .query path next, fs =>
    let rest := (next (fs path)).run fs
    (rest.1, (path, fs path) :: rest.2)

/-- `fs` gives every recorded query the recorded answer. -/
def agrees (fs : FileSystem) (trace : Trace) : Bool :=
  trace.all fun entry => decide (fs entry.1 = entry.2)

/-- A test: the nextest binary id and the test's name within it. -/
structure TestId where
  binary : String
  name : String
  deriving DecidableEq, Repr

/-- The digest of every non-file input of one test binary: the binary's own
bytes (which fix the toolchain, target, profile, features and every crate it
links), the policy version, the nextest version and the hash of its
`ci` profile, and the behaviour-changing environment variables named in
`scripts/ci/test-cache.toml`. Programs the test executes and files it reads are
not here: they are trace queries. -/
abbrev StaticKey := String

/-- Premise (a) and (b) as a hypothesis: what each test does given its key. -/
abbrev World := TestId → StaticKey → Behaviour

/-- A cached pass: test `test` passed with key `key` after making exactly the
queries in `trace`. `run` names the trusted run that observed it. -/
structure Record where
  test : TestId
  key : StaticKey
  trace : Trace
  run : String
  deriving DecidableEq, Repr

/-- The cache. `contradicted` holds the keys under which a trusted run saw a
test fail after a pass had been recorded for the same answers: proof that a
premise failed for that test (`contradiction_refutes_premises`). -/
structure Store where
  records : List Record
  contradicted : List (TestId × StaticKey)
  deriving Repr

def Store.empty : Store := ⟨[], []⟩

/-- What a run asks of the cache. `full` ignores every record: the periodic
run on main and a forced run on a pull request. -/
structure Policy where
  excluded : List TestId
  full : Bool
  deriving Repr

inductive Reason where
  | fullRun
  | excluded
  | contradicted
  | miss
  deriving DecidableEq, Repr

inductive Decision where
  | skip (record : Record)
  | run (reason : Reason)
  deriving DecidableEq, Repr

/-- A record that vouches for `test` under `key` on file system `fs`. -/
def vouches (fs : FileSystem) (test : TestId) (key : StaticKey) (record : Record) : Bool :=
  decide (record.test = test) && decide (record.key = key) && agrees fs record.trace

/-- The cache's one decision. A record is looked up by the test and its
key and then verified, query by query, against the current answers: a
recorded trace is a dynamic dependency list, so a test that never read a file
is not invalidated by a change to it. -/
def decide (policy : Policy) (store : Store) (test : TestId) (key : StaticKey)
    (fs : FileSystem) : Decision :=
  if policy.full then .run .fullRun
  else if policy.excluded.contains test then .run .excluded
  else if store.contradicted.contains (test, key) then .run .contradicted
  else match store.records.find? (vouches fs test key) with
    | some record => .skip record
    | none => .run .miss

/-- One executed test as the gate saw it. `traceComplete` is the tracer's
verdict that it classified every access (premises b and c); `checkoutClean`
that no process of the run wrote a tracked path, so the answers it recorded
are the commit's. -/
structure Execution where
  test : TestId
  key : StaticKey
  fs : FileSystem
  passed : Bool
  trace : Trace
  trusted : Bool
  traceComplete : Bool
  checkoutClean : Bool
  run : String

/-- The execution is one the premises speak for. -/
def Execution.vouched (policy : Policy) (e : Execution) : Bool :=
  e.trusted && e.traceComplete && e.checkoutClean && !policy.excluded.contains e.test

/-- A trusted failure where a recorded pass vouches for the same answers. -/
def Execution.contradicts (policy : Policy) (e : Execution) (record : Record) : Bool :=
  !e.passed && e.vouched policy && vouches e.fs e.test e.key record

/-- Recording. Only a vouched pass becomes a record. A vouched failure that a
record vouches against marks the key contradicted: the next run executes the
test and the gate reports the contradiction as a premise violation to fix by
excluding the test with an issue. A contradiction is never cleared by the
gate (`TestCacheStore.tla`: an expiring one lets a slow writer's stale pass
skip the test again). -/
def observe (policy : Policy) (store : Store) (e : Execution) : Store :=
  if e.passed && e.vouched policy then
    { store with records := ⟨e.test, e.key, e.trace, e.run⟩ :: store.records }
  else if store.records.any (e.contradicts policy) then
    { store with contradicted := (e.test, e.key) :: store.contradicted }
  else store

/-- Premises (a), (b) and (c) for one execution: when the gate vouches for it,
what it observed is what the world says the test does. -/
def Faithful (world : World) (policy : Policy) (e : Execution) : Prop :=
  e.vouched policy = true → (world e.test e.key).run e.fs = (e.passed, e.trace)

/-- Every record is a pass the world can produce. -/
def Valid (world : World) (store : Store) : Prop :=
  ∀ record ∈ store.records, ∃ fs, (world record.test record.key).run fs = (true, record.trace)

/-- How the store evolves: faithful executions are observed, and any records
may be evicted (retention, size caps); contradictions are kept. -/
inductive Reachable (world : World) (policy : Policy) : Store → Prop where
  | empty : Reachable world policy Store.empty
  | observe (store : Store) (e : Execution) :
      Reachable world policy store → Faithful world policy e →
      Reachable world policy (observe policy store e)
  | evict (store : Store) (kept : List Record) :
      Reachable world policy store → (∀ record ∈ kept, record ∈ store.records) →
      Reachable world policy { store with records := kept }

/-! ## Proofs -/

theorem agrees_iff (fs : FileSystem) (trace : Trace) :
    agrees fs trace = true ↔ ∀ entry ∈ trace, fs entry.1 = entry.2 := by
  simp [agrees, List.all_eq_true]

/-- The trace is a sufficient key for files: a behaviour run on any file system
that answers its recorded queries the same way runs the same way. -/
theorem run_replays (b : Behaviour) (fs fs' : FileSystem)
    (h : ∀ entry ∈ (b.run fs).2, fs' entry.1 = entry.2) : b.run fs' = b.run fs := by
  induction b with
  | finish passed => rfl
  | query path next ih =>
    simp only [Behaviour.run] at h ⊢
    have hp : fs' path = fs path := h (path, fs path) (List.mem_cons_self _ _)
    rw [hp, ih (fs path) (fun entry mem => h entry (List.mem_cons_of_mem _ mem))]

theorem valid_empty (world : World) : Valid world Store.empty := by
  intro record mem
  simp [Store.empty] at mem

theorem observe_valid (world : World) (policy : Policy) (store : Store) (e : Execution)
    (valid : Valid world store) (faithful : Faithful world policy e) :
    Valid world (observe policy store e) := by
  unfold observe
  split
  · rename_i h
    simp only [Bool.and_eq_true] at h
    intro record mem
    simp only [List.mem_cons] at mem
    rcases mem with rfl | mem
    · exact ⟨e.fs, by rw [faithful h.2]; simp [h.1]⟩
    · exact valid record mem
  · split
    · exact valid
    · exact valid

theorem reachable_valid (world : World) (policy : Policy) (store : Store)
    (reachable : Reachable world policy store) : Valid world store := by
  induction reachable with
  | empty => exact valid_empty world
  | observe store e _ faithful ih => exact observe_valid world policy store e ih faithful
  | evict store kept _ subset ih =>
    intro record mem
    exact ih record (subset record mem)

/-- A skip is backed by a record that vouches for this test, key and file system. -/
theorem decide_skip_vouches (policy : Policy) (store : Store) (test : TestId)
    (key : StaticKey) (fs : FileSystem) (record : Record)
    (h : decide policy store test key fs = .skip record) :
    record ∈ store.records ∧ record.test = test ∧ record.key = key ∧
      agrees fs record.trace = true := by
  unfold decide at h
  split at h
  · cases h
  split at h
  · cases h
  split at h
  · cases h
  split at h
  · rename_i found
    cases h
    have mem := List.mem_of_find?_eq_some found
    have holds := List.find?_some found
    simp only [vouches, Bool.and_eq_true, decide_eq_true_eq] at holds
    exact ⟨mem, holds.1.1, holds.1.2, holds.2⟩
  · cases h

/-- **Soundness.** In any store the gate can reach under the premises, a
skipped test would have passed, and would have made the recorded queries. -/
theorem skip_sound (world : World) (policy : Policy) (store : Store) (test : TestId)
    (key : StaticKey) (fs : FileSystem) (record : Record)
    (reachable : Reachable world policy store)
    (h : decide policy store test key fs = .skip record) :
    (world test key).run fs = (true, record.trace) := by
  obtain ⟨mem, htest, hkey, hagrees⟩ := decide_skip_vouches policy store test key fs record h
  obtain ⟨fs₀, hrun⟩ := reachable_valid world policy store reachable record mem
  subst htest hkey
  rw [agrees_iff] at hagrees
  rw [run_replays _ fs₀ fs (by rw [hrun]; exact hagrees), hrun]

theorem skip_passes (world : World) (policy : Policy) (store : Store) (test : TestId)
    (key : StaticKey) (fs : FileSystem) (record : Record)
    (reachable : Reachable world policy store)
    (h : decide policy store test key fs = .skip record) :
    ((world test key).run fs).1 = true := by
  rw [skip_sound world policy store test key fs record reachable h]

/-- A full run executes every test. -/
theorem full_never_skips (policy : Policy) (store : Store) (test : TestId)
    (key : StaticKey) (fs : FileSystem) (full : policy.full = true) :
    decide policy store test key fs = .run .fullRun := by
  simp [decide, full]

/-- An excluded test always runs, whatever the store holds. -/
theorem excluded_never_skips (policy : Policy) (store : Store) (test : TestId)
    (key : StaticKey) (fs : FileSystem) (excluded : policy.excluded.contains test = true) :
    ∀ record, decide policy store test key fs ≠ .skip record := by
  intro record h
  unfold decide at h
  split at h
  · cases h
  · simp [excluded] at h

/-- A contradicted key always runs. -/
theorem contradicted_never_skips (policy : Policy) (store : Store) (test : TestId)
    (key : StaticKey) (fs : FileSystem)
    (contradicted : store.contradicted.contains (test, key) = true) :
    ∀ record, decide policy store test key fs ≠ .skip record := by
  intro record h
  unfold decide at h
  split at h
  · cases h
  split at h
  · cases h
  · simp [contradicted] at h

/-- Only a trusted run changes the cache's records. -/
theorem untrusted_records_nothing (policy : Policy) (store : Store) (e : Execution)
    (untrusted : e.trusted = false) : (observe policy store e).records = store.records := by
  unfold observe
  simp only [Execution.vouched, untrusted, Bool.false_and, Bool.and_false]
  split
  · contradiction
  · split <;> rfl

/-- Once contradicted, a key stays contradicted through every later observation. -/
theorem contradiction_persists (policy : Policy) (store : Store) (e : Execution)
    (entry : TestId × StaticKey) (mem : entry ∈ store.contradicted) :
    entry ∈ (observe policy store e).contradicted := by
  unfold observe
  split
  · exact mem
  · split
    · exact List.mem_cons_of_mem _ mem
    · exact mem

/-- A contradiction proves a premise false: in a valid store, a vouched failure
whose answers a record vouches for cannot be faithful. The gate therefore
reports it as a defect in the exclusion list or the tracer, not as a result. -/
theorem contradiction_refutes_premises (world : World) (policy : Policy) (store : Store)
    (e : Execution) (record : Record) (valid : Valid world store)
    (mem : record ∈ store.records) (contradicts : e.contradicts policy record = true) :
    ¬ Faithful world policy e := by
  intro faithful
  simp only [Execution.contradicts, vouches, Bool.and_eq_true, Bool.not_eq_true',
    decide_eq_true_eq] at contradicts
  obtain ⟨⟨failed, vouched⟩, ⟨htest, hkey⟩, hagrees⟩ := contradicts
  obtain ⟨fs₀, hrun⟩ := valid record mem
  rw [htest, hkey] at hrun
  rw [agrees_iff] at hagrees
  have replay := run_replays _ fs₀ e.fs (by rw [hrun]; exact hagrees)
  rw [hrun, faithful vouched] at replay
  have outcome := congrArg Prod.fst replay
  simp [failed] at outcome

end TestCache
