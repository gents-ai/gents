import Proofs.ToolPolicy.WriteInput
import Proofs.ToolPolicy.ApplicationWrite

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
written collection. -/
inductive Stamp where
  | nodeDid
  | sessionId
  | requestDocId
  | toolCallDocId
  | jobRowDocId
  | chainRootDocId
  deriving DecidableEq, Repr

/-- Whether the invocation has a value for the stamp. A callback runs outside
any session, so it has only the node DID; a non-job tool output has no job row
or chain root. -/
def Stamp.available : Invocation → Stamp → Bool
  | .callback, s => s == .nodeDid
  | .ux, s => s == .nodeDid
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

/-- A filter value is a literal or the `$caller.node_did` placeholder, which
the host resolves for own-node lookup of kept names. -/
inductive FilterValue where
  | literal (value : String)
  | callerNodeDid
  deriving DecidableEq, Repr

abbrev Filter := List (String × FilterValue)

def substituteValue (hostNodeDid : String) : FilterValue → FilterValue
  | .callerNodeDid => .literal hostNodeDid
  | .literal value => .literal value

def substituteCallerNodeDid (filter : Filter) (hostNodeDid : String) : Filter :=
  filter.map fun entry => (entry.1, substituteValue hostNodeDid entry.2)

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

theorem callback_has_only_node_did (s : Stamp) :
    s.available .callback = true ↔ s = .nodeDid := by
  cases s <;> simp [Stamp.available]

theorem ux_has_only_node_did (s : Stamp) :
    s.available .ux = true ↔ s = .nodeDid := by
  cases s <;> simp [Stamp.available]

/-- A WfScript or WfName write is never refused for a WfNote-only job stamp. -/
theorem stamp_scoped_to_written_collection (declared : List (String × Stamp))
    (collectionFields : List String) (field : String) (s : Stamp)
    (h_absent : field ∉ collectionFields) :
    (field, s) ∉ applicableStamps declared collectionFields := by
  simp [applicableStamps, h_absent]

/-- Whatever filter the guest wrote, the placeholder resolves to the host node
DID and no placeholder survives. -/
theorem caller_node_did_is_host_fact (filter : Filter) (hostNodeDid field : String)
    (h_mem : (field, FilterValue.callerNodeDid) ∈ filter) :
    (field, FilterValue.literal hostNodeDid) ∈ substituteCallerNodeDid filter hostNodeDid ∧
      ∀ entry ∈ substituteCallerNodeDid filter hostNodeDid, entry.2 ≠ .callerNodeDid := by
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

end ToolPolicy.PluginData
