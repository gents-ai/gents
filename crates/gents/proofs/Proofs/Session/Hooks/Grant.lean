import Proofs.Hooks.Effect
import Proofs.Identity.Household

namespace Hooks

/-- The subject of a relationship published with a created document: an
actor DID, or another document (a resource relationship). -/
inductive Subject (Doc : Type) where
  | actor (did : Actor)
  | document (ref : Ref Doc)
  deriving DecidableEq, Repr

/-- A relationship tuple core publishes on `target` together with the create
that makes it.

Decision 2026-11-11, maintainer-approved: a plugin's create may carry
relationship grants as data, so a plugin that creates a session child (the
Task Goal a claim applies) does not need the transaction to link it. The
plugin names the tuples; core validates them before anything is applied and
publishes them with the transaction's own relationship methods under the
transaction's actor. Rule: a plugin may grant only on a document it creates in
the same result, only with a relation its manifest declares for that
collection, and only to the transaction's actor, an actor the event names, or
a document the event names or core showed it. The native result attaches each
grant to its create, so the first condition holds there by construction. -/
structure Grant (Doc : Type) where
  target : Ref Doc
  relation : String
  subject : Subject Doc
  deriving DecidableEq, Repr

/-- What a declared relation may point at: an actor, or a document of
`collection`, which the policy calls `resource`. -/
inductive SubjectKind where
  | actor
  | document (resource collection : String)
  deriving DecidableEq, Repr

/-- One relation a manifest declares its creates in `collection` may carry.
The set is explicit: a grant no declaration covers is invalid. -/
structure Grantable where
  collection : String
  relation : String
  subject : SubjectKind
  deriving DecidableEq, Repr

/-- The subjects a result may grant to besides the transaction's actor: the
actors the event names, and the documents the event names or core showed the
plugin in this staging. -/
structure Named (Doc : Type) where
  actors : List Actor
  docs : List (Ref Doc)

variable {Doc Val Op Core : Type}

def Effect.created : Effect Doc Val Op → Option (Ref Doc)
  | .write (.create ref _) => some ref
  | _ => none

/-- The documents a result creates. -/
def created (effects : List (Effect Doc Val Op)) : List (Ref Doc) :=
  effects.filterMap Effect.created

def SubjectKind.admits : SubjectKind → Subject Doc → Bool
  | .actor, .actor _ => true
  | .document _ collection, .document ref => ref.collection == collection
  | _, _ => false

def Grantable.covers (declared : Grantable) (grant : Grant Doc) : Bool :=
  declared.collection == grant.target.collection && declared.relation == grant.relation &&
    declared.subject.admits grant.subject

variable [DecidableEq Doc]

def Subject.named (actor : Actor) (named : Named Doc) : Subject Doc → Bool
  | .actor did => did == actor || named.actors.contains did
  | .document ref => named.docs.contains ref

def Grant.permitted (declared : List Grantable) (actor : Actor) (named : Named Doc)
    (made : List (Ref Doc)) (grant : Grant Doc) : Bool :=
  made.contains grant.target && declared.any (·.covers grant) && grant.subject.named actor named

/-- Core's validation of a result's grants, decided before anything is
applied. -/
def grantsValid (declared : List Grantable) (actor : Actor) (named : Named Doc)
    (effects : List (Effect Doc Val Op)) (grants : List (Grant Doc)) : Bool :=
  grants.all (Grant.permitted declared actor named (created effects))

abbrev Relationships (Doc : Type) := List (Grant Doc)

/-- `Hooks.commit` with the result's grants: the effects commit exactly as
without grants, and the grants are published with them, all or none. -/
def commitGranting (manifest : Manifest) (declared : List Grantable) (allowed : Acp Doc)
    (perform : Actor → Op → Core → Option Core) (actor : Actor) (named : Named Doc)
    (effects : List (Effect Doc Val Op)) (grants : List (Grant Doc))
    (state : State Core Doc Val) (relations : Relationships Doc) :
    Option (State Core Doc Val × Relationships Doc) :=
  if grantsValid declared actor named effects grants then
    (commit manifest allowed perform actor effects state).map fun next => (next, grants ++ relations)
  else none

theorem granting_commits_the_effects (manifest : Manifest) (declared : List Grantable)
    (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (named : Named Doc) (effects : List (Effect Doc Val Op)) (grants : List (Grant Doc))
    (state next : State Core Doc Val) (relations published : Relationships Doc)
    (committed : commitGranting manifest declared allowed perform actor named effects grants
      state relations = some (next, published)) :
    grantsValid declared actor named effects grants = true ∧
      commit manifest allowed perform actor effects state = some next ∧
      published = grants ++ relations := by
  unfold commitGranting at committed
  split at committed
  · rename_i accepted
    cases result : commit manifest allowed perform actor effects state with
    | none => simp [result] at committed
    | some after =>
      simp only [result, Option.map_some', Option.some.injEq, Prod.mk.injEq] at committed
      obtain ⟨rfl, rfl⟩ := committed
      exact ⟨accepted, rfl, rfl⟩
  · cases committed

theorem invalid_grants_commit_nothing (manifest : Manifest) (declared : List Grantable)
    (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (named : Named Doc) (effects : List (Effect Doc Val Op)) (grants : List (Grant Doc))
    (state : State Core Doc Val) (relations : Relationships Doc)
    (invalid : grantsValid declared actor named effects grants = false) :
    commitGranting manifest declared allowed perform actor named effects grants state relations =
      none := by
  simp [commitGranting, invalid]

private theorem permitted_of_committed (manifest : Manifest) (declared : List Grantable)
    (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (named : Named Doc) (effects : List (Effect Doc Val Op)) (grants : List (Grant Doc))
    (state next : State Core Doc Val) (relations published : Relationships Doc)
    (committed : commitGranting manifest declared allowed perform actor named effects grants
      state relations = some (next, published))
    (grant : Grant Doc) (member : grant ∈ grants) :
    Grant.permitted declared actor named (created effects) grant = true := by
  have valid := (granting_commits_the_effects manifest declared allowed perform actor named
    effects grants state next relations published committed).1
  simp only [grantsValid, List.all_eq_true] at valid
  exact valid grant member

private theorem refuted (manifest : Manifest) (declared : List Grantable)
    (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (named : Named Doc) (effects : List (Effect Doc Val Op)) (grants : List (Grant Doc))
    (state : State Core Doc Val) (relations : Relationships Doc)
    (grant : Grant Doc) (member : grant ∈ grants)
    (denied : Grant.permitted declared actor named (created effects) grant = false) :
    commitGranting manifest declared allowed perform actor named effects grants state relations =
      none := by
  cases result : commitGranting manifest declared allowed perform actor named effects grants
      state relations with
  | none => rfl
  | some committed =>
    obtain ⟨next, published⟩ := committed
    have permitted := permitted_of_committed manifest declared allowed perform actor named
      effects grants state next relations published result grant member
    simp [denied] at permitted

/-- A result that grants on a document it does not create commits nothing. -/
theorem grant_on_uncreated_document_commits_nothing (manifest : Manifest)
    (declared : List Grantable) (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core)
    (actor : Actor) (named : Named Doc) (effects : List (Effect Doc Val Op))
    (grants : List (Grant Doc)) (state : State Core Doc Val) (relations : Relationships Doc)
    (grant : Grant Doc) (member : grant ∈ grants) (absent : grant.target ∉ created effects) :
    commitGranting manifest declared allowed perform actor named effects grants state relations =
      none := by
  apply refuted manifest declared allowed perform actor named effects grants state relations
    grant member
  simp [Grant.permitted, List.contains_iff_mem, absent]

/-- A result that grants a relation its manifest does not declare for the
target's collection, or to a subject of another kind, commits nothing. -/
theorem undeclared_grant_commits_nothing (manifest : Manifest) (declared : List Grantable)
    (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (named : Named Doc) (effects : List (Effect Doc Val Op)) (grants : List (Grant Doc))
    (state : State Core Doc Val) (relations : Relationships Doc)
    (grant : Grant Doc) (member : grant ∈ grants)
    (undeclared : ∀ entry ∈ declared, entry.covers grant = false) :
    commitGranting manifest declared allowed perform actor named effects grants state relations =
      none := by
  apply refuted manifest declared allowed perform actor named effects grants state relations
    grant member
  have none : declared.any (·.covers grant) = false := by
    simpa only [List.any_eq_false, Bool.not_eq_true] using undeclared
  simp [Grant.permitted, none]

/-- A result that grants to an actor that is neither the transaction's actor
nor named by the event commits nothing. -/
theorem grant_to_unnamed_actor_commits_nothing (manifest : Manifest)
    (declared : List Grantable) (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core)
    (actor : Actor) (named : Named Doc) (effects : List (Effect Doc Val Op))
    (grants : List (Grant Doc)) (state : State Core Doc Val) (relations : Relationships Doc)
    (grant : Grant Doc) (member : grant ∈ grants) (did : Actor)
    (subject : grant.subject = .actor did) (other : did ≠ actor) (unnamed : did ∉ named.actors) :
    commitGranting manifest declared allowed perform actor named effects grants state relations =
      none := by
  apply refuted manifest declared allowed perform actor named effects grants state relations
    grant member
  simp [Grant.permitted, Subject.named, subject, other, List.contains_iff_mem, unnamed]

/-- A result that grants to a document neither the event names nor core showed
the plugin commits nothing. -/
theorem grant_to_unnamed_document_commits_nothing (manifest : Manifest)
    (declared : List Grantable) (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core)
    (actor : Actor) (named : Named Doc) (effects : List (Effect Doc Val Op))
    (grants : List (Grant Doc)) (state : State Core Doc Val) (relations : Relationships Doc)
    (grant : Grant Doc) (member : grant ∈ grants) (ref : Ref Doc)
    (subject : grant.subject = .document ref) (unnamed : ref ∉ named.docs) :
    commitGranting manifest declared allowed perform actor named effects grants state relations =
      none := by
  apply refuted manifest declared allowed perform actor named effects grants state relations
    grant member
  simp [Grant.permitted, Subject.named, subject, List.contains_iff_mem, unnamed]

omit [DecidableEq Doc] in
private theorem mem_created (effects : List (Effect Doc Val Op)) (ref : Ref Doc)
    (member : ref ∈ created effects) : ∃ value, Effect.write (.create ref value) ∈ effects := by
  simp only [created, List.mem_filterMap] at member
  obtain ⟨effect, present, made⟩ := member
  cases effect with
  | write w =>
    cases w with
    | create target value =>
      simp only [Effect.created, Option.some.injEq] at made
      exact ⟨value, made ▸ present⟩
    | update _ _ _ => simp [Effect.created] at made
    | delete _ _ => simp [Effect.created] at made
  | core _ => simp [Effect.created] at made

/-- Grants cannot widen the caller's authority: every published grant is on a
document this result created, in a collection the plugin owns, by a create
document ACP permits the transaction's actor; its relation is one the manifest
declares; and its subject is the actor, or named by the event or shown. -/
theorem committed_grants_are_declared_on_authorized_creates (manifest : Manifest)
    (declared : List Grantable) (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core)
    (actor : Actor) (named : Named Doc) (effects : List (Effect Doc Val Op))
    (grants : List (Grant Doc)) (state next : State Core Doc Val)
    (relations published : Relationships Doc)
    (committed : commitGranting manifest declared allowed perform actor named effects grants
      state relations = some (next, published))
    (grant : Grant Doc) (member : grant ∈ grants) :
    (∃ value, Effect.write (.create grant.target value) ∈ effects) ∧
      grant.target.collection ∈ manifest.owns ∧
      allowed actor .create grant.target = true ∧
      (∃ entry ∈ declared, entry.covers grant = true) ∧
      grant.subject.named actor named = true := by
  have permitted := permitted_of_committed manifest declared allowed perform actor named
    effects grants state next relations published committed grant member
  simp only [Grant.permitted, Bool.and_eq_true, List.contains_iff_mem,
    List.any_eq_true] at permitted
  obtain ⟨⟨made, cover⟩, subject⟩ := permitted
  obtain ⟨value, create⟩ := mem_created effects grant.target made
  have effectsCommitted := (granting_commits_the_effects manifest declared allowed perform actor
    named effects grants state next relations published committed).2.1
  have owned := committed_writes_are_owned_and_authorized manifest allowed perform actor effects
    state next effectsCommitted (.create grant.target value) create
  exact ⟨⟨value, create⟩, owned.1, owned.2, cover, subject⟩

/-- A committed result leaves the relationships of every document it did not
create exactly as they were. -/
theorem grants_leave_existing_documents (manifest : Manifest) (declared : List Grantable)
    (allowed : Acp Doc) (perform : Actor → Op → Core → Option Core) (actor : Actor)
    (named : Named Doc) (effects : List (Effect Doc Val Op)) (grants : List (Grant Doc))
    (state next : State Core Doc Val) (relations published : Relationships Doc)
    (committed : commitGranting manifest declared allowed perform actor named effects grants
      state relations = some (next, published))
    (target : Ref Doc) (existing : target ∉ created effects) :
    published.filter (·.target == target) = relations.filter (·.target == target) := by
  obtain ⟨_, _, rfl⟩ := granting_commits_the_effects manifest declared allowed perform actor
    named effects grants state next relations published committed
  have none : grants.filter (·.target == target) = [] := by
    apply List.filter_eq_nil_iff.mpr
    intro grant member same
    have made := (committed_grants_are_declared_on_authorized_creates manifest declared allowed
      perform actor named effects grants state next relations _ committed grant member).1
    obtain ⟨value, create⟩ := made
    have equal : grant.target = target := by simpa using same
    apply existing
    simp only [created, List.mem_filterMap]
    exact ⟨_, create, by simp [Effect.created, equal]⟩
  simp [List.filter_append, none]

/-- The grants a session child publishes with its create, as the Household
policy's `children` resource declares them: `parent` to the session document
it belongs to (resource `sessions`, collection `AgentSession`), and
`working_agent` to the session's working Agent. Decision 2026-11-11,
maintainer-approved: this is the whole set a plugin may publish for now, each
owned collection naming it in its manifest; `reader` and every other relation
stay unreachable. -/
def sessionChildGrants (collection : String) : List Grantable :=
  [ ⟨collection, "parent", .document "sessions" "AgentSession"⟩
  , ⟨collection, "working_agent", .actor⟩ ]

/-- The policy resource a declaration lives on accepts its relation with that
subject type. -/
def householdAccepts (resource : String) (declared : Grantable) : Bool :=
  (Identity.Household.policy.find? (·.name == resource)).any fun definition =>
    definition.relations.any fun relation =>
      relation.name == declared.relation &&
        relation.types.contains (match declared.subject with
          | .actor => "actor"
          | .document target _ => target)

/-- The session-child grants are expressible in the existing ACP hierarchy:
each is a relation of the Household `children` resource, with that subject
type. -/
theorem session_child_grants_are_household_children_relations :
    (sessionChildGrants "Goal").all (householdAccepts "children") = true := by
  decide

end Hooks
