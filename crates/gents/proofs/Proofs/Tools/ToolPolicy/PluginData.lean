import Proofs.Shared.ToolPolicy.WriteInput
import Proofs.Tools.ToolPolicy.ApplicationWrite

namespace ToolPolicy.PluginData

/-- The context a plugin's `data_calls` and `writes` run in. One predicate
covers each invocation. A pinless step 0 is a `toolCall`: it is never adopted, so it
has no job stamps, and the engine keeps its notes in memory until adoption. -/
inductive Invocation where
  | toolCall
  | jobStep
  | callback
  | ux
  deriving DecidableEq, Repr

/-- Host facts a manifest's `data.stamped` map may bind to a field of the
written collection. The caller DID is the authenticated invocation actor used for
native ACP and data ownership; the node transport identity never substitutes for it. -/
inductive Stamp where
  | callerDid
  | sessionId
  | requestDocId
  | toolCallDocId
  | jobRowDocId
  | chainRootDocId
  deriving DecidableEq, Repr

/-- Whether the invocation has a value for the stamp. A callback runs outside
any session, so it has only its authenticated caller DID; a non-job tool output has no job row
or chain root. -/
def Stamp.available : Invocation → Stamp → Bool
  | .callback, s => s == .callerDid
  | .ux, s => s == .callerDid
  | .toolCall, .jobRowDocId => false
  | .toolCall, .chainRootDocId => false
  | .toolCall, _ => true
  | .jobStep, _ => true

/-- A declared stamp applies only to a field of the written collection. -/
def applicableStamps (declared : List (String × Stamp)) (collectionFields : List String) :
    List (String × Stamp) :=
  declared.filter fun entry => collectionFields.contains entry.1

/-- The refusal names the stamp and the invocation that lacks it. -/
inductive Refusal where
  | stampUnavailable (s : Stamp) (inv : Invocation)
  | other
  deriving DecidableEq, Repr

/-- Native facts about one write of a step, tool or callback output. -/
structure Observation where
  inOwnSdl : Bool
  runtimeOrProtected : Bool
  actorIsInvoker : Bool
  opGranted : Bool
  stampedSuppliedByGuest : Bool
  invocation : Invocation
  applicable : List (String × Stamp)
  write : ToolPolicy.ApplicationWrite.Observation
  deriving Repr

def admitted (op : ToolPolicy.ApplicationWrite.Operation) (o : Observation) : Bool :=
  o.inOwnSdl && !o.runtimeOrProtected && o.actorIsInvoker && o.opGranted &&
  !o.stampedSuppliedByGuest && o.applicable.all (fun entry => entry.2.available o.invocation) &&
  ToolPolicy.ApplicationWrite.admitted op o.write

/-- A refused write rejects the whole output; the native owner commits writes
in the output's transaction. -/
def refusal (op : ToolPolicy.ApplicationWrite.Operation) (o : Observation) : Option Refusal :=
  match o.applicable.find? (fun entry => !entry.2.available o.invocation) with
  | some (_, s) => some (.stampUnavailable s o.invocation)
  | none => if admitted op o then none else some .other

/-- Reads run in the actor's ACP view, which DefraDB enforces. Own-SDL alone
is not enough: a pack may repeat a runtime collection's SDL exactly, so a read
passes the same runtime-and-protected refusal as a write, and the manifest must
grant `query`. -/
def queryAdmitted (limit : Nat) (inOwnSdl runtimeOrProtected queryGranted : Bool) : Bool :=
  inOwnSdl && !runtimeOrProtected && queryGranted && 0 < limit && limit ≤ 256

/-- A filter value is a literal or the `$caller.did` placeholder, which
the host resolves for invoker-owned lookup of kept names. -/
inductive FilterValue where
  | literal (value : String)
  | callerDid
  deriving DecidableEq, Repr

abbrev Filter := List (String × FilterValue)

def substituteValue (callerDid : String) : FilterValue → FilterValue
  | .callerDid => .literal callerDid
  | .literal value => .literal value

def substituteCallerDid (filter : Filter) (callerDid : String) : Filter :=
  filter.map fun entry => (entry.1, substituteValue callerDid entry.2)

/-- Notes use `ignore`; kept names use `fail`. -/
inductive OnConflict where
  | ignore
  | fail
  deriving DecidableEq, Repr

abbrev Fingerprint := String

inductive InsertOutcome where
  | inserted
  | unchanged
  | abortStep
  deriving DecidableEq, Repr

/-- Read-then-insert in the step transaction with an immutable fingerprint
compare. The unique index on `(chain_root_doc_id, note_id)` is a backstop
only: a unique violation aborts the whole transaction. Unique indexes are
node-local, so readers pick a deterministic winner among P2P twins. -/
def insertNote (existing : Option Fingerprint) (new : Fingerprint)
    (onConflict : OnConflict) : InsertOutcome :=
  match existing with
  | none => .inserted
  | some old =>
    if old = new then .unchanged
    else match onConflict with
      | .ignore => .unchanged
      | .fail => .abortStep

/-- The pack's install-time validation requires the declared collections to be
a subset of the type names in the pack's own `schemas/*.graphql`. The runtime
predicate `writable_output_collection` (`triggers/callback/plugin.rs`) refuses runtime
and protected collections as a second check; alone it is not "own pack". -/
theorem writes_stay_in_own_sdl (op : ToolPolicy.ApplicationWrite.Operation) (o : Observation)
    (h : admitted op o = true) : o.inOwnSdl = true := by
  simp only [admitted, Bool.and_eq_true] at h; exact h.1.1.1.1.1.1

/-- See `writes_stay_in_own_sdl`: the runtime refusal of runtime and protected
collections is the second check behind install-time own-schema validation. -/
theorem never_runtime_or_protected (op : ToolPolicy.ApplicationWrite.Operation)
    (o : Observation) (h : admitted op o = true) : o.runtimeOrProtected = false := by
  simp only [admitted, Bool.and_eq_true, Bool.not_eq_true'] at h; exact h.1.1.1.1.1.2

theorem query_never_runtime_or_protected (limit : Nat)
    (inOwnSdl runtimeOrProtected queryGranted : Bool)
    (h : queryAdmitted limit inOwnSdl runtimeOrProtected queryGranted = true) :
    runtimeOrProtected = false := by
  cases runtimeOrProtected <;> simp_all [queryAdmitted]

theorem query_stays_in_own_sdl_and_granted (limit : Nat)
    (inOwnSdl runtimeOrProtected queryGranted : Bool)
    (h : queryAdmitted limit inOwnSdl runtimeOrProtected queryGranted = true) :
    inOwnSdl = true ∧ queryGranted = true := by
  cases inOwnSdl <;> cases queryGranted <;> simp_all [queryAdmitted]

theorem actor_is_invoking_principal (op : ToolPolicy.ApplicationWrite.Operation)
    (o : Observation) (h : admitted op o = true) : o.actorIsInvoker = true := by
  simp only [admitted, Bool.and_eq_true] at h; exact h.1.1.1.1.2

/-- Stamped fields are runtime-filled: the host overwrites them and a guest
value refuses the write. -/
theorem filled_fields_host_only (op : ToolPolicy.ApplicationWrite.Operation) (o : Observation)
    (h : o.stampedSuppliedByGuest = true) :
    admitted op o = false ∧
      ∀ (expected actual : ToolPolicy.WriteInput.Kind) (nullable required : Bool),
        ToolPolicy.WriteInput.admits expected nullable required true (some actual) = false :=
  ⟨by simp [admitted, h], fun expected actual nullable required =>
    ToolPolicy.WriteInput.filled_rejects_caller_value expected actual nullable required⟩

/-- An applicable stamp with no value for the invocation refuses the write
with a refusal naming an unavailable stamp and the invocation. -/
theorem stamp_unavailable_refuses_write (op : ToolPolicy.ApplicationWrite.Operation)
    (o : Observation) (field : String) (s : Stamp) (h_mem : (field, s) ∈ o.applicable)
    (h_unavailable : s.available o.invocation = false) :
    admitted op o = false ∧
      ∃ s', s'.available o.invocation = false ∧
        refusal op o = some (.stampUnavailable s' o.invocation) := by
  have h_any : o.applicable.all (fun entry => entry.2.available o.invocation) = false := by
    rw [List.all_eq_false]; exact ⟨(field, s), h_mem, by simp [h_unavailable]⟩
  refine ⟨by simp [admitted, h_any], ?_⟩
  cases h_find : o.applicable.find? (fun entry => !entry.2.available o.invocation) with
  | none =>
    have := List.find?_eq_none.mp h_find (field, s) h_mem
    simp [h_unavailable] at this
  | some entry =>
    have h_pred := List.find?_some h_find
    exact ⟨entry.2, by simpa using h_pred, by simp [refusal, h_find]⟩

theorem callback_has_only_caller_did (s : Stamp) :
    s.available .callback = true ↔ s = .callerDid := by
  cases s <;> simp [Stamp.available]

theorem ux_has_only_caller_did (s : Stamp) :
    s.available .ux = true ↔ s = .callerDid := by
  cases s <;> simp [Stamp.available]

/-- A WfScript or WfName write is never refused for a WfNote-only job stamp. -/
theorem stamp_scoped_to_written_collection (declared : List (String × Stamp))
    (collectionFields : List String) (field : String) (s : Stamp)
    (h_absent : field ∉ collectionFields) :
    (field, s) ∉ applicableStamps declared collectionFields := by
  simp [applicableStamps, h_absent]

/-- Whatever filter the guest wrote, the placeholder resolves to the authenticated caller
DID and no placeholder survives. -/
theorem caller_did_is_authenticated_fact (filter : Filter) (callerDid field : String)
    (h_mem : (field, FilterValue.callerDid) ∈ filter) :
    (field, FilterValue.literal callerDid) ∈ substituteCallerDid filter callerDid ∧
      ∀ entry ∈ substituteCallerDid filter callerDid, entry.2 ≠ .callerDid := by
  constructor
  · exact List.mem_map.mpr ⟨_, h_mem, rfl⟩
  · intro entry h_entry
    obtain ⟨original, _, rfl⟩ := List.mem_map.mp h_entry
    cases h_value : original.2 <;> simp [substituteValue, h_value]

theorem writes_bounded (op : ToolPolicy.ApplicationWrite.Operation) (o : Observation)
    (h : admitted op o = true) :
    ToolPolicy.ApplicationWrite.admitted op o.write = true ∧
      0 < o.write.limit ∧ o.write.limit ≤ 100 ∧ o.write.targets ≤ o.write.limit := by
  simp only [admitted, Bool.and_eq_true] at h
  have h_write := h.2
  simp only [ToolPolicy.ApplicationWrite.admitted, Bool.and_eq_true, decide_eq_true_eq] at h_write
  exact ⟨h.2, h_write.1.1.2.1, h_write.1.1.2.2, h_write.1.2⟩

theorem note_first_wins (old new : Fingerprint) :
    insertNote (some old) new .ignore = .unchanged := by
  by_cases h : old = new <;> simp [insertNote, h]

theorem identical_replay_noop (fingerprint : Fingerprint) (onConflict : OnConflict) :
    insertNote (some fingerprint) fingerprint onConflict = .unchanged := by
  simp [insertNote]

theorem on_conflict_fail_aborts_step (old new : Fingerprint) (h : old ≠ new) :
    insertNote (some old) new .fail = .abortStep := by
  simp [insertNote, h]

/-- Tool calls, job steps and callbacks share one predicate: the invocation
matters only through which applicable stamps it can fill. -/
theorem admission_independent_of_invocation_kind (op : ToolPolicy.ApplicationWrite.Operation)
    (o : Observation) (inv inv' : Invocation)
    (h_same : o.applicable.all (fun entry => entry.2.available inv) =
      o.applicable.all (fun entry => entry.2.available inv')) :
    admitted op { o with invocation := inv } = admitted op { o with invocation := inv' } := by
  simp only [admitted]; rw [h_same]

/-- Most writes one output may carry. -/
def maxWrites : Nat := 100

inductive DataOp where
  | insert
  | update
  | delete
  deriving DecidableEq, Repr

def DataOp.operation : DataOp → ToolPolicy.ApplicationWrite.Operation
  | .insert => .create
  | .update => .update
  | .delete => .delete

/-- One write as the grant sees it, to a collection that is neither runtime nor
protected: whether the grant lists its collection, knows that collection's field
names and grants its operation, and whether it names a document, asks for
`on_conflict`, or carries a value for a stamped field. -/
structure GrantedWrite where
  op : DataOp
  collectionGranted : Bool
  fieldsKnown : Bool
  opGranted : Bool
  docId : Bool
  onConflict : Bool
  guestStamped : Bool
  deriving DecidableEq, Repr

def GrantedWrite.targeted (w : GrantedWrite) : Bool :=
  w.op == .insert || w.docId

/-- The native facts of a granted write. Every caller applies writes in a
transaction opened as the invoking principal, so the actor is the invoker. -/
def GrantedWrite.observation (w : GrantedWrite) (invocation : Invocation)
    (applicable : List (String × Stamp)) : Observation :=
  { inOwnSdl := w.collectionGranted
    runtimeOrProtected := false
    actorIsInvoker := true
    opGranted := w.opGranted
    stampedSuppliedByGuest := w.guestStamped
    invocation
    applicable
    write :=
      { granted := w.collectionGranted
        application := true
        blocked := false
        credential := false
        bounded := w.targeted
        targets := if w.targeted then 1 else 0
        limit := maxWrites
        preview := false
        digestMatches := true } }

/-- One write's refusal. Without its collection's field names no declared stamp
could be matched, so the write would be admitted unstamped: it is refused. -/
def grantedRefusal (declared : List (String × Stamp)) (collectionFields : List String)
    (invocation : Invocation) (w : GrantedWrite) : Option Refusal :=
  if !w.fieldsKnown then some .other
  else refusal w.op.operation (w.observation invocation (applicableStamps declared collectionFields))

/-- The first refusal of an output's writes. More than `maxWrites` writes, or
`on_conflict` on anything but an insert, refuses the whole output. -/
def admitWrites (declared : List (String × Stamp)) (collectionFields : List String)
    (invocation : Invocation) (writes : List GrantedWrite) : Option Refusal :=
  if writes.length > maxWrites || writes.any (fun w => w.onConflict && w.op != .insert) then
    some .other
  else writes.findSome? (grantedRefusal declared collectionFields invocation)

/-- The fields the host fills in an admitted write: every applicable stamp,
except on a delete, which writes no fields. -/
def stampedFields (declared : List (String × Stamp)) (collectionFields : List String)
    (w : GrantedWrite) : List String :=
  if w.op == .delete then [] else (applicableStamps declared collectionFields).map Prod.fst

theorem granted_collection_without_field_names_is_refused
    (declared : List (String × Stamp)) (collectionFields : List String) (invocation : Invocation)
    (w : GrantedWrite) (h : w.fieldsKnown = false) (h_conflict : w.onConflict = false) :
    admitWrites declared collectionFields invocation [w] = some .other := by
  simp [admitWrites, grantedRefusal, h, h_conflict, maxWrites]

/-- Unique indexes are node-local, so P2P twins can coexist under one key. Rows
are listed in `_docID` order and carry their `created_at`; a row without one
reads as earliest. `earlier` orders them by `created_at`, then `_docID`. -/
def earlier : Option Nat × Nat → Option Nat × Nat → Bool
  | (none, i), (none, j) => i < j
  | (none, _), (some _, _) => true
  | (some _, _), (none, _) => false
  | (some a, i), (some b, j) => a < b || (a == b && i < j)

/-- The stored row every reader picks among twins: the earliest `created_at`,
then the lowest `_docID`. -/
def twinWinner (rows : List (Option Nat)) : Option Nat :=
  (rows.zipIdx.foldl (fun best row => match best with
    | none => some row
    | some current => if earlier row current then some row else best) none).map Prod.snd

/-- An insert with `on_conflict` against the rows stored under its key: how
many unique keys the grant declares for the collection, whether the write
carries the key's field, the stored rows in `_docID` order, the stored row
whose guest fields the write repeats (none: fields no stored row has). -/
structure ConflictWrite where
  uniqueKeys : Nat
  keyPresent : Bool
  stored : List (Option Nat)
  sameAs : Option Nat
  onConflict : OnConflict
  deriving DecidableEq, Repr

inductive ConflictReceipt where
  | inserted
  | existing
  | duplicate
  deriving DecidableEq, Repr

inductive ConflictRefusal where
  | keyCount
  | keyField
  | duplicate
  deriving DecidableEq, Repr

/-- The fingerprint of a stored row's guest fields, and of the write's. -/
def rowFingerprint (rank : Nat) : Fingerprint := s!"row {rank}"

def writeFingerprint : Option Nat → Fingerprint
  | some rank => rowFingerprint rank
  | none => "fresh"

/-- The write's outcome: a receipt naming the stored winner when the write
repeats it, or a refusal that rolls the whole output back. -/
def resolveConflict (w : ConflictWrite) : Except ConflictRefusal (ConflictReceipt × Option Nat) :=
  if w.uniqueKeys != 1 then .error .keyCount
  else if !w.keyPresent then .error .keyField
  else match twinWinner w.stored with
    | none => .ok (.inserted, none)
    | some winner =>
      match insertNote (some (rowFingerprint winner)) (writeFingerprint w.sameAs) w.onConflict with
      | .inserted => .ok (.inserted, none)
      | .unchanged =>
        if w.sameAs = some winner then .ok (.existing, some winner) else .ok (.duplicate, none)
      | .abortStep => .error .duplicate

/-- The rows under the key after the write, as (stored rank, stored rank whose
guest fields the row carries); an inserted row has no stored rank. A refusal
or a receipt leaves the stored rows as they were. -/
def rowsAfter (w : ConflictWrite) : List (Option Nat × Option Nat) :=
  let stored := (List.range w.stored.length).map fun rank => (some rank, some rank)
  match resolveConflict w with
  | .ok (.inserted, _) => stored ++ [(none, w.sameAs)]
  | _ => stored

/-- The unique key's field, as the guest names it in `fields`. -/
def conflictKeyField : String := "key"

/-- What a refused conflict tells the guest. -/
def conflictRefusalText (collection key : String) (uniqueKeys : Nat) : ConflictRefusal → String
  | .keyCount =>
    s!"on_conflict needs a collection with exactly one unique key; {collection} has {uniqueKeys}"
  | .keyField => s!"on_conflict needs the unique key field \"{conflictKeyField}\" in fields"
  | .duplicate => s!"duplicate {collection} {key}"

theorem on_conflict_ignore_with_a_different_row_reports_duplicate :
    resolveConflict ⟨1, true, [none], none, .ignore⟩ = .ok (.duplicate, none) ∧
      rowsAfter ⟨1, true, [none], none, .ignore⟩ = [(some 0, some 0)] := by
  constructor <;> rfl

theorem on_conflict_fail_refuses_a_different_existing_row_and_keeps_an_equal_one :
    resolveConflict ⟨1, true, [none], some 0, .fail⟩ = .ok (.existing, some 0) ∧
      resolveConflict ⟨1, true, [none], none, .fail⟩ = .error .duplicate ∧
      rowsAfter ⟨1, true, [none], none, .fail⟩ = [(some 0, some 0)] := by
  refine ⟨rfl, rfl, rfl⟩

theorem on_conflict_needs_exactly_one_unique_key (w : ConflictWrite) :
    (w.uniqueKeys ≠ 1 → resolveConflict w = .error .keyCount) ∧
      (w.uniqueKeys = 1 → w.keyPresent = false → resolveConflict w = .error .keyField) := by
  constructor
  · intro h; simp [resolveConflict, h]
  · intro h hk; simp [resolveConflict, h, hk]

theorem p2p_twin_readers_pick_the_earliest_created_then_lowest_doc_id :
    twinWinner [some 2, some 1] = some 1 ∧ twinWinner [some 3, some 3] = some 0 ∧
      twinWinner [some 1, none] = some 1 := by
  decide

end ToolPolicy.PluginData
