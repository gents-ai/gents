import Lean
import Proofs.Store.Identity.Household

namespace Identity.DeclaredAccess

open Identity.Household

/-- Packs and configuration declare access policy; core only applies it. A
declaration names actor subjects of one relation on one document: a
RepositoryPlacement's `readers` and a Task's `outcome_readers` declare
`reader`, and a configured agent target (`AgentTargetAccess`) declares
`session_writer` on the target's Agent. A pack writes each subject as one of
its slot references, which installation resolves to the Agent that slot
provisions. Core applies exactly those actor tuples, as the actor that
publishes the declaration, so native ACP bounds every grant by that actor's
own authority to manage the relation. Core adds no relation the declaration
does not name, never a wildcard and never a managing relation. Only a changed
declaration grants or revokes, so a revoked tuple stays revoked on an
unchanged republish; removing the document or the declaration revokes its
tuples. Native tuples carry no provenance: an identical tuple granted
independently is indistinguishable and is revoked with the declaration. -/
def declaredRelation (resource document relation subject : String) : Relation :=
  ⟨resource, document, relation, .actor subject⟩

def readerRelation (resource document reader : String) : Relation :=
  declaredRelation resource document "reader" reader

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

def grantDeclared (state : State) (grantor resource document relation : String)
    (subjects : List String) : State :=
  subjects.foldl (fun state subject =>
    apply state (.grantAs grantor (declaredRelation resource document relation subject))) state

def revokeDeclared (state : State) (grantor resource document relation : String)
    (subjects : List String) : State :=
  subjects.foldl (fun state subject =>
    apply state (.revokeAs grantor (declaredRelation resource document relation subject))) state

/-- Apply a changed declaration of one relation on one document. -/
def applyDeclaredRelation (state : State) (grantor resource document relation : String)
    (prior next : List String) : State :=
  grantDeclared (revokeDeclared state grantor resource document relation (removed prior next))
    grantor resource document relation (added prior next)

def grantAll (state : State) (grantor resource document : String) (readers : List String) : State :=
  grantDeclared state grantor resource document "reader" readers

def revokeAll (state : State) (grantor resource document : String) (readers : List String) : State :=
  revokeDeclared state grantor resource document "reader" readers

/-- Apply a changed `reader` declaration on one document. -/
def applyDeclared (state : State) (grantor resource document : String)
    (prior next : List String) : State :=
  applyDeclaredRelation state grantor resource document "reader" prior next

/-- Every relation a grant adds is a declared tuple on the document. -/
theorem grantDeclared_adds_only_declared (state : State) (grantor resource document relation : String)
    (subjects : List String) (fact : Relation)
    (member : fact ∈ (grantDeclared state grantor resource document relation subjects).relations) :
    fact ∈ state.relations ∨
      ∃ subject ∈ subjects, fact = declaredRelation resource document relation subject := by
  induction subjects generalizing state with
  | nil => exact Or.inl member
  | cons head tail ih =>
    simp only [grantDeclared, List.foldl_cons] at member
    rcases ih _ member with inner | ⟨subject, mem, eq⟩
    · simp only [apply] at inner
      split at inner
      · simp only [List.mem_cons] at inner
        rcases inner with eq | old
        · exact Or.inr ⟨head, List.mem_cons_self _ _, eq⟩
        · exact Or.inl old
      · exact Or.inl inner
    · exact Or.inr ⟨subject, List.mem_cons_of_mem _ mem, eq⟩

/-- A revocation only removes tuples. -/
theorem revokeDeclared_removes_only (state : State) (grantor resource document relation : String)
    (subjects : List String) (fact : Relation)
    (member : fact ∈ (revokeDeclared state grantor resource document relation subjects).relations) :
    fact ∈ state.relations := by
  induction subjects generalizing state with
  | nil => exact member
  | cons head tail ih =>
    simp only [revokeDeclared, List.foldl_cons] at member
    have inner := ih _ member
    simp only [apply] at inner
    split at inner
    · exact (List.mem_filter.mp inner).1
    · exact inner

theorem applyDeclaredRelation_adds_only_declared (state : State)
    (grantor resource document relation : String) (prior next : List String) (fact : Relation)
    (member : fact ∈ (applyDeclaredRelation state grantor resource document relation prior next).relations) :
    fact ∈ state.relations ∨
      ∃ subject ∈ next, fact = declaredRelation resource document relation subject := by
  rcases grantDeclared_adds_only_declared _ _ _ _ _ _ _ member with old | ⟨subject, mem, eq⟩
  · exact Or.inl (revokeDeclared_removes_only _ _ _ _ _ _ _ old)
  · exact Or.inr ⟨subject, (List.mem_filter.mp mem).1, eq⟩

theorem grantAll_adds_only_declared (state : State) (grantor resource document : String)
    (readers : List String) (relation : Relation)
    (member : relation ∈ (grantAll state grantor resource document readers).relations) :
    relation ∈ state.relations ∨
      ∃ reader ∈ readers, relation = readerRelation resource document reader :=
  grantDeclared_adds_only_declared state grantor resource document "reader" readers relation member

theorem grantDeclared_preserves_objects (state : State) (grantor resource document relation : String)
    (subjects : List String) :
    (grantDeclared state grantor resource document relation subjects).objects = state.objects := by
  induction subjects generalizing state with
  | nil => rfl
  | cons head tail ih =>
    simp only [grantDeclared, List.foldl_cons]
    rw [show (List.foldl _ _ tail) = grantDeclared _ grantor resource document relation tail from rfl, ih]
    simp only [apply]; split <;> rfl

theorem grantAll_preserves_objects (state : State) (grantor resource document : String)
    (readers : List String) :
    (grantAll state grantor resource document readers).objects = state.objects :=
  grantDeclared_preserves_objects state grantor resource document "reader" readers

/-- A grantor that may manage none of the declared tuples applies nothing:
grants never exceed the publishing actor's authority. -/
theorem grantDeclared_without_authority_is_inert (state : State)
    (grantor resource document relation : String) (subjects : List String)
    (denied : ∀ subject ∈ subjects,
      mayManage state grantor (declaredRelation resource document relation subject) = false) :
    grantDeclared state grantor resource document relation subjects = state := by
  induction subjects with
  | nil => rfl
  | cons head tail ih =>
    simp only [grantDeclared, List.foldl_cons]
    have step : apply state (.grantAs grantor (declaredRelation resource document relation head)) = state := by
      simp [apply, denied head (List.mem_cons_self _ _)]
    rw [step]
    exact ih fun subject mem => denied subject (List.mem_cons_of_mem _ mem)

/-- An unchanged republish grants and revokes nothing. -/
theorem unchanged_declaration_relation_is_inert (state : State)
    (grantor resource document relation : String) (subjects : List String) :
    applyDeclaredRelation state grantor resource document relation subjects subjects = state := by
  have a : added subjects subjects = [] := by simp [added]
  have r : removed subjects subjects = [] := by simp [removed]
  simp [applyDeclaredRelation, a, r, grantDeclared, revokeDeclared]

theorem unchanged_declaration_is_inert (state : State) (grantor resource document : String)
    (readers : List String) :
    applyDeclared state grantor resource document readers readers = state :=
  unchanged_declaration_relation_is_inert state grantor resource document "reader" readers

theorem fresh_declaration_grants_every_declared_reader (prior : List String) :
    added [] prior = prior ∧ removed [] prior = [] := by simp [added, removed]

end Identity.DeclaredAccess
