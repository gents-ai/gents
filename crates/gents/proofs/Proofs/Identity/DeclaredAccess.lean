import Lean
import Proofs.Identity.Household

namespace Identity.DeclaredAccess

open Identity.Household

/-- The pack declares data access policy; core only applies it. A declaration
names `reader` subjects for one of the pack's own documents (a
RepositoryPlacement's `readers`, a Task's `outcome_readers`); a pack writes
each subject as one of its slot references, which installation resolves to the
Agent that slot provisions. Core applies exactly those `reader` actor tuples,
as the actor that publishes the document, so native ACP bounds every grant by
that actor's own authority to manage the document. Core adds no relation the
declaration does not name, never a wildcard and never a managing relation.
Only a changed declaration grants or revokes, so a revoked tuple stays revoked
on an unchanged republish; removing the document or the declaration revokes
its tuples. Native tuples carry no provenance: an identical tuple granted
independently is indistinguishable and is revoked with the declaration. -/
def readerRelation (resource document reader : String) : Relation :=
  ⟨resource, document, "reader", .actor reader⟩

/-- Pack admission: every declared subject is one of the pack's provisioned
Agents or the installing User. -/
def admissible (provisioned : List String) (installer : String) (readers : List String) : Bool :=
  readers.all fun reader => provisioned.contains reader || reader == installer

theorem admissible_subjects (provisioned : List String) (installer : String)
    (readers : List String) (ok : admissible provisioned installer readers = true)
    (reader : String) (member : reader ∈ readers) :
    reader ∈ provisioned ∨ reader = installer := by
  simp [admissible, List.all_eq_true] at ok
  rcases ok reader member with h | h
  · exact Or.inl h
  · exact Or.inr h

def added (prior next : List String) : List String := next.filter fun r => !prior.contains r
def removed (prior next : List String) : List String := prior.filter fun r => !next.contains r

def grantAll (state : State) (grantor resource document : String) (readers : List String) : State :=
  readers.foldl (fun state reader =>
    apply state (.grantAs grantor (readerRelation resource document reader))) state

def revokeAll (state : State) (grantor resource document : String) (readers : List String) : State :=
  readers.foldl (fun state reader =>
    apply state (.revokeAs grantor (readerRelation resource document reader))) state

/-- Apply a changed declaration on one document. -/
def applyDeclared (state : State) (grantor resource document : String)
    (prior next : List String) : State :=
  grantAll (revokeAll state grantor resource document (removed prior next))
    grantor resource document (added prior next)

/-- Every relation a grant adds is a declared `reader` tuple on the document. -/
theorem grantAll_adds_only_declared (state : State) (grantor resource document : String)
    (readers : List String) (relation : Relation)
    (member : relation ∈ (grantAll state grantor resource document readers).relations) :
    relation ∈ state.relations ∨
      ∃ reader ∈ readers, relation = readerRelation resource document reader := by
  induction readers generalizing state with
  | nil => exact Or.inl member
  | cons head tail ih =>
    simp only [grantAll, List.foldl_cons] at member
    rcases ih _ member with inner | ⟨reader, mem, eq⟩
    · simp only [apply] at inner
      split at inner
      · simp only [List.mem_cons] at inner
        rcases inner with eq | old
        · exact Or.inr ⟨head, List.mem_cons_self _ _, eq⟩
        · exact Or.inl old
      · exact Or.inl inner
    · exact Or.inr ⟨reader, List.mem_cons_of_mem _ mem, eq⟩

theorem grantAll_preserves_objects (state : State) (grantor resource document : String)
    (readers : List String) :
    (grantAll state grantor resource document readers).objects = state.objects := by
  induction readers generalizing state with
  | nil => rfl
  | cons head tail ih =>
    simp only [grantAll, List.foldl_cons]
    rw [show (List.foldl _ _ tail) = grantAll _ grantor resource document tail from rfl, ih]
    simp only [apply]; split <;> rfl

/-- An unchanged republish grants and revokes nothing. -/
theorem unchanged_declaration_is_inert (state : State) (grantor resource document : String)
    (readers : List String) :
    applyDeclared state grantor resource document readers readers = state := by
  have a : added readers readers = [] := by simp [added]
  have r : removed readers readers = [] := by simp [removed]
  simp [applyDeclared, a, r, grantAll, revokeAll]

theorem fresh_declaration_grants_every_declared_reader (prior : List String) :
    added [] prior = prior ∧ removed [] prior = [] := by simp [added, removed]

end Identity.DeclaredAccess
