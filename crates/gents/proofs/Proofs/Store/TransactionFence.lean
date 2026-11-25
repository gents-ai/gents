/-!
# Transaction fences

Which concurrent commits make a transaction abort at its own commit, and when
a decision taken on what a transaction read is fenced against a concurrent
writer.

External premise (the pinned DefraDB; evidenced by the `gents-store` tests in
`config_client::isolation_premise_tests` and
`txn_owner_tests::defradb_refuses_a_concurrent_duplicate_after_read_then_insert`):
a transaction aborts at commit when a transaction that committed after it
began wrote a document it read by document id (`docID:`, `docIDs:`, or a
`_docID` equality filter) or found by an equality filter on every field of a
unique index, wrote a document it also writes, inserted a value
of a unique index it also inserts, or deleted a document its filtered read
returned. Nothing else a filtered, listed, counted, ordered or arrival-head
read returned or did not return is validated: an insert into the range, a
departure from it and a value change beside it all commit. The embedded write
gate admits concurrent writers and orders none of them. The transaction owners
run an aborted callback again (`TransactionReplay`).
-/

namespace TransactionFence

abbrev Doc := Nat
abbrev UniqueValue := Nat

/-- What one transaction did that the store validates at its commit. -/
structure Footprint where
  pointReads : List Doc
  scanned : List Doc
  writes : List Doc
  deletes : List Doc
  uniqueInserts : List UniqueValue
  deriving DecidableEq, Repr

def Footprint.empty : Footprint := ⟨[], [], [], [], []⟩

/-- How a transaction reads. A read by document id, or by equality on every
field of a unique index (which the store answers from that index), records
the document it found as read by id; a filtered, listed, counted or ordered
read records the documents it returned; an arrival-head read records nothing
the store validates. -/
inductive Read where
  | byDocId (doc : Doc)
  | byUniqueKey (doc : Doc)
  | filtered (returned : List Doc)
  | arrivalHead
  deriving DecidableEq, Repr

def Footprint.read (f : Footprint) : Read → Footprint
  | .byDocId doc => { f with pointReads := doc :: f.pointReads }
  | .byUniqueKey doc => { f with pointReads := doc :: f.pointReads }
  | .filtered returned => { f with scanned := returned ++ f.scanned }
  | .arrivalHead => f

/-- Whether `later` aborts at its commit because `earlier` committed after
`later` began. -/
def conflicts (later earlier : Footprint) : Bool :=
  later.pointReads.any earlier.writes.contains ||
    later.writes.any earlier.writes.contains ||
    later.uniqueInserts.any earlier.uniqueInserts.contains ||
    later.scanned.any earlier.deletes.contains

/-- How a deciding transaction names what orders it against a concurrent
writer it must not miss. -/
inductive Fence where
  /-- Both transactions write one document; the filtered update
  (`update_X(filter:)`) is the compare-and-set form. -/
  | commonWrite (doc : Doc)
  /-- The decider reads, by document id, a document the writer writes. -/
  | pointRead (doc : Doc)
  /-- Both insert one value of a unique index. -/
  | uniqueIndex (value : UniqueValue)
  deriving DecidableEq, Repr

def Fence.holds (decider writer : Footprint) : Fence → Bool
  | .commonWrite doc => decider.writes.contains doc && writer.writes.contains doc
  | .pointRead doc => decider.pointReads.contains doc && writer.writes.contains doc
  | .uniqueIndex value =>
      decider.uniqueInserts.contains value && writer.uniqueInserts.contains value

/-- The decision: a transaction is fenced against a concurrent writer iff,
when that writer commits first, the decider's commit aborts and its callback
runs again over what the writer committed. -/
def fencedAgainst (decider writer : Footprint) : Bool := conflicts decider writer

/-- An arrival creates one fresh document and deletes nothing. -/
def arrival (doc : Doc) (unique : List UniqueValue) : Footprint :=
  { Footprint.empty with writes := [doc], uniqueInserts := unique }

theorem a_named_fence_fences (decider writer : Footprint) (fence : Fence)
    (h : fence.holds decider writer = true) : fencedAgainst decider writer = true := by
  cases fence <;>
    simp_all [Fence.holds, fencedAgainst, conflicts, List.any_eq_true] <;>
    first
      | exact Or.inl (Or.inl (Or.inl ⟨_, h.1, h.2⟩))
      | exact Or.inl (Or.inl (Or.inr ⟨_, h.1, h.2⟩))
      | exact Or.inl (Or.inr ⟨_, h.1, h.2⟩)

/-- Whichever of two writers of one document commits second aborts. -/
theorem a_common_write_fences_both_orders (a b : Footprint) (doc : Doc)
    (ha : a.writes.contains doc = true) (hb : b.writes.contains doc = true) :
    fencedAgainst a b = true ∧ fencedAgainst b a = true :=
  ⟨a_named_fence_fences a b (.commonWrite doc) (by simp_all [Fence.holds]),
    a_named_fence_fences b a (.commonWrite doc) (by simp_all [Fence.holds])⟩

/-- A filtered read is no fence against an arrival into its range: whatever
it returned or did not return, a fresh document the decider neither read by
id nor wrote, with no unique value the decider inserts, commits beside it. -/
theorem a_filtered_read_does_not_fence_an_arrival (decider : Footprint) (returned : List Doc)
    (doc : Doc) (unique : List UniqueValue)
    (h_read : doc ∉ decider.pointReads) (h_write : doc ∉ decider.writes)
    (h_unique : ∀ value ∈ unique, value ∉ decider.uniqueInserts) :
    fencedAgainst (decider.read (.filtered returned)) (arrival doc unique) = false := by
  simp only [fencedAgainst, conflicts, Footprint.read, arrival, Footprint.empty,
    Bool.or_eq_false_iff, List.any_eq_false]
  refine ⟨⟨⟨?_, ?_⟩, ?_⟩, ?_⟩
  · intro x hx; simp only [List.contains_cons, List.contains_nil, Bool.or_false,
      beq_iff_eq, Bool.not_eq_true']
    intro he; subst he; exact h_read hx
  · intro x hx; simp only [List.contains_cons, List.contains_nil, Bool.or_false,
      beq_iff_eq, Bool.not_eq_true']
    intro he; subst he; exact h_write hx
  · intro x hx; simp only [List.contains_iff_mem, Bool.not_eq_true',
      decide_eq_false_iff_not]
    exact fun hu => h_unique x hu hx
  · intro x _; simp

/-- A read by a unique key is a read by id of the document it found: a later
write of that document aborts the reader. -/
theorem a_unique_key_read_is_a_point_read (decider writer : Footprint) (doc : Doc)
    (h : writer.writes.contains doc = true) :
    fencedAgainst (decider.read (.byUniqueKey doc)) writer = true := by
  have h_mem : doc ∈ writer.writes := by simpa using h
  simp [fencedAgainst, conflicts, Footprint.read, h_mem]

/-- Reading the arrival head adds nothing the store validates. -/
theorem an_arrival_head_read_fences_nothing (decider writer : Footprint) :
    fencedAgainst (decider.read .arrivalHead) writer = fencedAgainst decider writer := rfl

/-- A count below a limit is a filtered read: two admissions that each count
the same rows and insert a fresh row both commit. -/
theorem a_count_admits_concurrently (counted : List Doc) (first second : Doc)
    (h_fresh : first ≠ second) :
    fencedAgainst ((arrival first []).read (.filtered counted)) (arrival second []) = false ∧
      fencedAgainst ((arrival second []).read (.filtered counted)) (arrival first []) = false := by
  simp [fencedAgainst, conflicts, Footprint.read, arrival, Footprint.empty, h_fresh,
    Ne.symm h_fresh]

/-- One unique value inserted by both: the second commit aborts, and its
callback runs again over the winner's row. -/
theorem a_unique_index_fences_twin_arrivals (first second : Doc) (value : UniqueValue) :
    fencedAgainst (arrival first [value]) (arrival second [value]) = true :=
  a_named_fence_fences _ _ (.uniqueIndex value) (by simp [Fence.holds, arrival, Footprint.empty])

/-- What a production transaction that reads by filter, list, count or order
and then writes names as the fence its decision rests on: a fence the store
validates (`Fence`: a common write, a point read or a unique index), a single
writer (no second decider runs: one lease holder, one in-process lock, one
owner), idempotence (a decision taken on a stale read is harmless, because
what it missed is decided again where it lands), or an immutable binding (no
writer can change what the decision read). The structure rule
`every_range_read_then_write_names_its_fence` asks every such transaction for
one, by these names. -/
inductive DeclaredFence where
  | store (fence : Fence)
  | singleWriter
  | idempotent
  /-- The decision reads a session's binding (`SessionBinding`: its requester,
  its Agent and whether its relationships are pending) before its transaction,
  and the transaction does not read the session, so the store orders nothing
  between that read and the commit (`a_binding_read_before_the_transaction_is_not_fenced`).
  No writer can unbind a bound session (`a_bound_session_stays_bound`), so
  what the read decided holds at commit; a binding a writer could change goes
  stale (`a_rewritable_binding_can_go_stale`).

  External premise (the pinned schema and the writers on `main`):
  `AgentSession.requester_did` and `AgentSession.agent_did` are `@immutable`;
  `relationships_pending` is written true only when a session is created, and
  afterwards only from true to false; no production path deletes an
  `AgentSession`. Every other write to a session (its observation refresh,
  its title, its archive state) leaves the binding as it was (`SessionWrite`).
  The `gents` structure tests hold it: `a_session_binding_is_never_rewritten`
  (the schema directives and every production write of
  `relationships_pending` as true) and
  `agent_reachable_surfaces_never_detach_or_delete_a_session` (every
  production delete of an `AgentSession`). -/
  | immutableBinding
  deriving DecidableEq, Repr

/-- A declared store fence orders the decider against the writer it names. -/
theorem a_declared_store_fence_fences (decider writer : Footprint) (fence : Fence)
    (h : fence.holds decider writer = true) :
    fencedAgainst decider writer = true :=
  a_named_fence_fences decider writer fence h

/-- A transaction that writes, deletes and inserts nothing (a read-only one)
makes no writer abort: whatever it read, a writer that commits after it
began commits. -/
theorem a_transaction_that_writes_nothing_aborts_no_writer (reader writer : Footprint)
    (h_writes : reader.writes = []) (h_deletes : reader.deletes = [])
    (h_unique : reader.uniqueInserts = []) :
    fencedAgainst writer reader = false := by
  simp [fencedAgainst, conflicts, h_writes, h_deletes, h_unique]

/-- The only fence a read-only transaction can name is a point read, and it
orders nothing but that transaction's own reads: when the writer commits
first, the reader's commit aborts and it reads again. No write of the
reader's waits on it, so a decision taken on what it read is fenced only by
the later transaction that acts on it. -/
theorem a_read_only_transaction_names_only_point_reads (reader writer : Footprint)
    (fence : Fence) (h_writes : reader.writes = [])
    (h_unique : reader.uniqueInserts = []) (h : fence.holds reader writer = true) :
    ∃ doc, fence = .pointRead doc := by
  cases fence with
  | commonWrite doc => simp [Fence.holds, h_writes] at h
  | pointRead doc => exact ⟨doc, rfl⟩
  | uniqueIndex value => simp [Fence.holds, h_unique] at h

/-- Reading again rows a transaction already read, in the same transaction,
adds nothing the store validates. -/
theorem reading_the_same_rows_again_adds_no_conflict (decider writer : Footprint)
    (read : Read) :
    fencedAgainst ((decider.read read).read read) writer =
      fencedAgainst (decider.read read) writer := by
  cases read with
  | byDocId doc | byUniqueKey doc =>
      simp only [fencedAgainst, conflicts, Footprint.read, List.any_cons]
      cases writer.writes.contains doc <;> simp
  | filtered returned =>
      simp only [fencedAgainst, conflicts, Footprint.read, List.any_append]
      cases returned.any writer.deletes.contains <;> simp
  | arrivalHead => rfl

/-! ## Immutable bindings

`DeclaredFence.immutableBinding`: a decision may rest on facts no writer can
change. The store then orders nothing between a read outside the transaction
and the commit, and nothing needs ordering: what that read decided is what a
read inside the transaction would decide at commit, whatever commits in
between. The premise is on the variant. -/

/-- A decider that neither reads the session by id nor writes it holds no
store fence against the session's writer: that writer's commit does not abort
it, so a binding read before the transaction is checked again by nothing. -/
theorem a_binding_read_before_the_transaction_is_not_fenced (decider : Footprint)
    (session : Doc) (h_read : session ∉ decider.pointReads)
    (h_write : session ∉ decider.writes) :
    fencedAgainst decider { Footprint.empty with writes := [session] } = false := by
  simp only [fencedAgainst, conflicts, Footprint.empty, Bool.or_eq_false_iff,
    List.any_eq_false]
  refine ⟨⟨⟨?_, ?_⟩, ?_⟩, ?_⟩
  · intro x hx; simp only [List.contains_cons, List.contains_nil, Bool.or_false,
      beq_iff_eq, Bool.not_eq_true']
    intro he; subst he; exact h_read hx
  · intro x hx; simp only [List.contains_cons, List.contains_nil, Bool.or_false,
      beq_iff_eq, Bool.not_eq_true']
    intro he; subst he; exact h_write hx
  · intro x _; simp
  · intro x _; simp

/-- What a client request's admission decides on in its session. -/
structure SessionBinding where
  requester : Nat
  agent : Nat
  relationshipsPending : Bool
  deriving DecidableEq, Repr

/-- The writes a session admits under the premise of
`DeclaredFence.immutableBinding`, by what they do to its binding: none deletes
it, rebinds it or re-pends its relationships. -/
inductive SessionWrite where
  /-- Any write that leaves the binding as it was: the working Agent's
  observation refresh, a title, an archive. -/
  | other
  /-- The creator publishes the session's relationships. -/
  | publishRelationships
  deriving DecidableEq, Repr

def SessionWrite.apply : SessionWrite → SessionBinding → SessionBinding
  | .other, s => s
  | .publishRelationships, s => { s with relationshipsPending := false }

/-- The session after `writes`, in commit order. -/
def SessionBinding.after (s : SessionBinding) (writes : List SessionWrite) : SessionBinding :=
  writes.foldl (fun s w => w.apply s) s

/-- The decision `bound_published_session` takes: the session is the
request's, and its relationships are published. -/
def SessionBinding.bound (s : SessionBinding) (requester agent : Nat) : Bool :=
  s.requester == requester && s.agent == agent && !s.relationshipsPending

theorem the_binding_fields_never_change (s : SessionBinding) (writes : List SessionWrite) :
    (s.after writes).requester = s.requester ∧ (s.after writes).agent = s.agent := by
  induction writes generalizing s with
  | nil => exact ⟨rfl, rfl⟩
  | cons w rest ih =>
      cases w <;> simpa [SessionBinding.after, List.foldl, SessionWrite.apply] using ih _

theorem published_relationships_stay_published (s : SessionBinding)
    (writes : List SessionWrite) (h : s.relationshipsPending = false) :
    (s.after writes).relationshipsPending = false := by
  induction writes generalizing s with
  | nil => exact h
  | cons w rest ih =>
      cases w <;> exact ih _ (by simp [SessionWrite.apply, h])

/-- `DeclaredFence.immutableBinding` holds: a session bound to a request
before its transaction is bound to it at commit, whatever the session's other
writers commit in between, so reading the binding outside the transaction
decides what a read inside it would. -/
theorem a_bound_session_stays_bound (s : SessionBinding) (writes : List SessionWrite)
    (requester agent : Nat) (h : s.bound requester agent = true) :
    (s.after writes).bound requester agent = true := by
  obtain ⟨hr, ha⟩ := the_binding_fields_never_change s writes
  simp only [SessionBinding.bound, Bool.and_eq_true, beq_iff_eq, Bool.not_eq_true'] at h ⊢
  exact ⟨⟨hr ▸ h.1.1, ha ▸ h.1.2⟩, published_relationships_stay_published s writes h.2⟩

/-- Without the premise of `DeclaredFence.immutableBinding` the read outside
goes stale: a write that re-pends the relationships, or changes the Agent,
unbinds a session that was bound. -/
theorem a_rewritable_binding_can_go_stale :
    let s : SessionBinding := ⟨0, 0, false⟩
    s.bound 0 0 = true ∧ ({ s with relationshipsPending := true } : SessionBinding).bound 0 0 = false ∧
      ({ s with agent := 1 } : SessionBinding).bound 0 0 = false := by
  decide

end TransactionFence
