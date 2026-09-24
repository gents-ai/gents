import Proofs.SessionHydration.State

namespace SessionHydration

def decideAdmits (cat : Catalog) (r : Request) : Bool := decide (admits cat r)

theorem decideAdmits_agrees (cat : Catalog) (r : Request) :
    decideAdmits cat r = true ↔ admits cat r := by
  unfold decideAdmits
  exact decide_eq_true_iff

def decideSelected (cat : Catalog) (r : Request) (doc : Document) : Bool :=
  match selectedDocuments cat r with
  | some documents => decide (doc ∈ documents)
  | none => false

theorem decideSelected_agrees (cat : Catalog) (r : Request) (doc : Document) :
    decideSelected cat r doc = true ↔
      ∃ documents, selectedDocuments cat r = some documents ∧ doc ∈ documents := by
  unfold decideSelected
  cases selectedDocuments cat r <;> simp

end SessionHydration
