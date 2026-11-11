namespace DocumentOperations

/-- Who signs the transaction. A person at a client signs as the User and may
do what an operator may. An Agent signs as itself. The client states which it
is beside the request; neither is inferred from a name or a role. -/
inductive Actor where
  | person | agent
  deriving DecidableEq, Repr

inductive Step where
  | preview | commit
  deriving DecidableEq, Repr

/-- The digest a commit presents, against the digest of the exact target set
and each target's content as the committing transaction reads them. -/
inductive Digest where
  | absent | current | stale
  deriving DecidableEq, Repr

inductive DeleteOutcome where
  | previewed | deleted | targetMissing | digestRequired | digestStale | stillReferenced
  deriving DecidableEq, Repr

/-- The one delete of configuration documents, for every client and every
collection including Agent. `present` is whether every target exists for the
owner in this transaction. `retainedValid` is whether no document that stays
requires a target (the required references `Configuration.Affected` validates),
read in the same transaction that deletes, never before it.

The differences between callers are rules about the actor: a person at a
terminal deletes in one call, and an Agent deletes only the content it
previewed. A digest that is presented is always checked, whoever presents it.
The CLI's check outside the transaction, its refusal of an owner with an
unrelated invalid document, and the desktop's delete without a prior read are
not actor rules and are gone. -/
def deleteOutcome (actor : Actor) (step : Step) (digest : Digest)
    (present retainedValid : Bool) : DeleteOutcome :=
  if !present then .targetMissing
  else match step, actor, digest with
    | .commit, _, .stale => .digestStale
    | .commit, .agent, .absent => .digestRequired
    | .preview, _, _ => if retainedValid then .previewed else .stillReferenced
    | .commit, _, _ => if retainedValid then .deleted else .stillReferenced

def writes (outcome : DeleteOutcome) : Bool := outcome == .deleted

theorem preview_writes_nothing (actor : Actor) (digest : Digest) (present retained : Bool) :
    writes (deleteOutcome actor .preview digest present retained) = false := by
  cases actor <;> cases digest <;> cases present <;> cases retained <;> rfl

theorem agent_deletes_only_what_it_previewed (digest : Digest) (present retained : Bool)
    (deleted : deleteOutcome .agent .commit digest present retained = .deleted) :
    digest = .current := by
  cases digest <;> cases present <;> cases retained <;> simp_all [deleteOutcome]

theorem person_deletes_in_one_call :
    deleteOutcome .person .commit .absent true true = .deleted := rfl

theorem stale_digest_never_deletes (actor : Actor) (step : Step) (present retained : Bool) :
    writes (deleteOutcome actor step .stale present retained) = false := by
  cases actor <;> cases step <;> cases present <;> cases retained <;> rfl

theorem referenced_target_is_kept (actor : Actor) (step : Step) (digest : Digest)
    (present : Bool) : writes (deleteOutcome actor step digest present false) = false := by
  cases actor <;> cases step <;> cases digest <;> cases present <;> rfl

theorem missing_target_is_refused (actor : Actor) (step : Step) (digest : Digest)
    (retained : Bool) : deleteOutcome actor step digest false retained = .targetMissing := by
  cases actor <;> cases step <;> cases digest <;> cases retained <;> rfl

/-- A preview answers what the commit carrying its digest answers: admitted
previews commit, and a refused preview names the commit's refusal. -/
theorem preview_matches_commit (actor : Actor) (digest : Digest) (present retained : Bool) :
    deleteOutcome actor .commit .current present retained =
      match deleteOutcome actor .preview digest present retained with
      | .previewed => .deleted
      | refusal => refusal := by
  cases actor <;> cases digest <;> cases present <;> cases retained <;> rfl

end DocumentOperations
