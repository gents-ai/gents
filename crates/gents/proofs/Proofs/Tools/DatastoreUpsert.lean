/-! An upsert tool writes one document of its collection, found by its
declared key fields. It creates the document when none has the input's key
values, and updates the one that has them when every commit of it was signed
by the writer (`DocumentSigner.signer`). It never overwrites a document another
identity signed or one with no single signer, and refuses a key that matches
several documents. The lookup and the write run in one transaction, under the
same ACP and signing as a create: a document the writer cannot read is not
found, so the write creates instead of touching it. -/

namespace DatastoreUpsert

/-- A declaration names at least one key field, each one of its fields and
none twice. -/
def wellFormed (fields keys : List String) : Bool :=
  !keys.isEmpty && keys.all (fields.contains ·) && keys.eraseDups.length == keys.length

/-- What the input's key values find in the collection. -/
inductive Existing where
  | none
  | mine
  | foreign
  | unsigned
  | several
  deriving DecidableEq, Repr

inductive Outcome where
  | created
  | updated
  | refusedMissingKey
  | refusedForeign
  | refusedAmbiguous
  deriving DecidableEq, Repr

/-- `keyed` is whether the input gives every key field a value. -/
def call (keyed : Bool) (existing : Existing) : Outcome :=
  if !keyed then .refusedMissingKey
  else
    match existing with
    | .none => .created
    | .mine => .updated
    | .foreign => .refusedForeign
    | .unsigned => .refusedForeign
    | .several => .refusedAmbiguous

theorem updates_only_what_the_writer_signed (keyed : Bool) (existing : Existing)
    (updated : call keyed existing = .updated) : existing = .mine := by
  cases keyed <;> cases existing <;> simp_all [call]

theorem creates_only_when_no_document_has_the_key (keyed : Bool) (existing : Existing)
    (created : call keyed existing = .created) : existing = .none := by
  cases keyed <;> cases existing <;> simp_all [call]

theorem a_key_names_declared_fields (fields keys : List String)
    (formed : wellFormed fields keys = true) :
    keys ≠ [] ∧ ∀ key ∈ keys, key ∈ fields := by
  simp only [wellFormed, Bool.and_eq_true] at formed
  obtain ⟨⟨nonempty, declared⟩, _⟩ := formed
  refine ⟨by simpa using nonempty, fun key member => ?_⟩
  simpa using (List.all_eq_true.mp declared) key member

end DatastoreUpsert
