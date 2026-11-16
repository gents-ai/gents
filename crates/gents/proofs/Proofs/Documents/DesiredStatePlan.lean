/-! A desired-configuration plan is admitted before it reads the database.
Every document's creation and replacement payloads must decode as the
collection's canonical configuration, the replacement must keep the owner of
the creation, and no two documents may name the same owner and logical ID in
one collection. -/

namespace DesiredStatePlan

/-- One document of a one-collection plan. `undeclaredField`: its payload
carries a field the canonical configuration does not declare.
`sparseReplacement`: its replacement payload omits a field the canonical
configuration requires. `replacementOwner`: the owner its replacement names
when that differs from the creation's spelling, `none` when both payloads are
the same document. -/
structure Document where
  owner : Nat
  id : Nat
  undeclaredField : Bool
  sparseReplacement : Bool
  replacementOwner : Option Nat
  deriving DecidableEq, Repr

/-- Premise (canonical configuration decoding): a canonical configuration
root refuses a field it does not declare and requires each field it does not
default. -/
def decodes (document : Document) : Bool :=
  !document.undeclaredField && !document.sparseReplacement

/-- Decision: a replacement may not move a document to another owner, so a
plan cannot change the scope of what it replaces. -/
def keepsOwner (document : Document) : Bool :=
  document.replacementOwner.all (· == document.owner)

def identity (document : Document) : Nat × Nat := (document.owner, document.id)

/-- Decision: one plan names each document once; the same logical ID under two
owners is two documents. -/
def admitted (plan : List Document) : Bool :=
  plan.all decodes && plan.all keepsOwner && decide (plan.map identity).Nodup

def clean (owner id : Nat) : Document := ⟨owner, id, false, false, none⟩

theorem plans_reject_unknown_fields_sparse_updates_and_scope_changes :
    admitted [{ clean 0 0 with undeclaredField := true }] = false ∧
    admitted [{ clean 0 0 with sparseReplacement := true }] = false ∧
    admitted [{ clean 0 0 with replacementOwner := some 2 }] = false ∧
    admitted [clean 3 1, clean 4 1] = true ∧
    admitted [clean 3 1, clean 3 1] = false := by
  decide

theorem a_rejected_document_rejects_its_plan (plan : List Document) (document : Document)
    (h_mem : document ∈ plan) (h : decodes document = false) : admitted plan = false := by
  have : plan.all decodes = false := by
    rw [List.all_eq_false]
    exact ⟨document, h_mem, by simp [h]⟩
  simp [admitted, this]

end DesiredStatePlan
