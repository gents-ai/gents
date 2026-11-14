import Proofs.Session.AgentSession
import Proofs.Automation.Triggers.Durable

/-! Get-or-create destinations. The rendered `session_id_template` is the
session's own ID: admission accepts a key that names no session yet, and the
first claim creates the one document through `AgentSession.ensure?`, which also
records the opening request and Task on that session. Requests sharing a key
share `(node, session)`, so the session claim gate serializes their claims and
the unique `(node_did, session_id)` index binds the rest. -/
namespace Triggers.SessionKey

open AgentSession

/-- `create_session` needs a rendered destination to create. -/
def configValid (template : Option String) (createSession : Bool) : Bool :=
  !createSession || template.isSome

/-- Claim-time creation over the durable store, through the one session owner. -/
def ensureInStore (store : List Document) (requested : Document) : Option (List Document) :=
  match store.find? (fun session => session.scope == requested.scope) with
  | none => (ensure? none requested).map fun created => store ++ [created]
  | some current => (ensure? (some current) requested).map fun _ => store

def claimAll (store : List Document) : List Document → Option (List Document)
  | [] => some store
  | requested :: rest => (ensureInStore store requested).bind (claimAll · rest)

theorem missing_key_admitted_when_creating (id : Triggers.Durable.Identity) (key : String)
    (h : key.isEmpty = false) :
    Triggers.Durable.resolveSession id (some key) false true = some key := by
  simp [Triggers.Durable.resolveSession, h]

theorem empty_key_refused_even_when_creating (id : Triggers.Durable.Identity) (owned create : Bool) :
    Triggers.Durable.resolveSession id (some "") owned create = none := by
  have empty : "".isEmpty = true := by decide
  simp [Triggers.Durable.resolveSession, empty]

/-- A literal target without `create_session` keeps its existing contract. -/
theorem existing_only_refuses_missing (id : Triggers.Durable.Identity) (key : String) :
    Triggers.Durable.resolveSession id (some key) false false = none := by
  simp [Triggers.Durable.resolveSession]

theorem no_template_cannot_create : configValid none true = false := rfl

private theorem find_in_extended (store : List Document) (first : Document) (scope : Scope)
    (absent : ∀ session ∈ store, session.scope ≠ scope) (hfirst : first.scope = scope) :
    (store ++ [first]).find? (fun session => session.scope == scope) = some first := by
  have hnone : store.find? (fun session => session.scope == scope) = none := by
    apply List.find?_eq_none.mpr
    intro session member
    simpa using absent session member
  simp [List.find?_append, hnone, hfirst]

private theorem repeat_claim_is_inert (store : List Document) (first requested : Document)
    (absent : ∀ session ∈ store, session.scope ≠ first.scope)
    (scope : requested.scope = first.scope) (agent : requested.agent = first.agent) :
    ensureInStore (store ++ [first]) requested = some (store ++ [first]) := by
  have found : (store ++ [first]).find? (fun session => session.scope == requested.scope) =
      some first := by
    rw [scope]
    exact find_in_extended store first first.scope absent rfl
  have kept : ensure? (some first) requested = some first :=
    ensure_existing_preserves_document first requested scope.symm agent.symm
  simp [ensureInStore, found, kept]

private theorem later_claims_are_inert (store : List Document) (first : Document) (rest : List Document)
    (absent : ∀ session ∈ store, session.scope ≠ first.scope)
    (same : ∀ requested ∈ rest, requested.scope = first.scope ∧ requested.agent = first.agent) :
    claimAll (store ++ [first]) rest = some (store ++ [first]) := by
  induction rest with
  | nil => rfl
  | cons requested tail ih =>
      have hh := same requested (List.mem_cons_self _ _)
      simp only [claimAll, repeat_claim_is_inert store first requested absent hh.1 hh.2,
        Option.bind_some]
      exact ih fun r mem => same r (List.mem_cons_of_mem _ mem)

/-- Any order of same-key claims leaves exactly the first claimant's document.
Concurrent fires differ only in which request is first. -/
theorem same_key_claims_create_one_session (store : List Document) (first : Document)
    (rest : List Document)
    (absent : ∀ session ∈ store, session.scope ≠ first.scope)
    (same : ∀ requested ∈ rest, requested.scope = first.scope ∧ requested.agent = first.agent) :
    claimAll store (first :: rest) = some (store ++ [first]) := by
  have none_found : store.find? (fun session => session.scope == first.scope) = none := by
    apply List.find?_eq_none.mpr
    intro session member
    simpa using absent session member
  simp only [claimAll, ensureInStore, none_found, ensure?, Option.map_some, Option.bind_some]
  exact later_claims_are_inert store first rest absent same

theorem exactly_one_session_for_key (store : List Document) (first : Document)
    (absent : ∀ session ∈ store, session.scope ≠ first.scope) :
    ((store ++ [first]).filter (fun session => session.scope == first.scope)).length = 1 := by
  have hnone : store.filter (fun session => session.scope == first.scope) = [] := by
    apply List.filter_eq_nil_iff.mpr
    intro session member
    simpa using absent session member
  simp [List.filter_append, hnone]

/-- A key already bound to another agent never hands that session to this one. -/
theorem other_agent_key_refused (store : List Document) (first requested : Document)
    (absent : ∀ session ∈ store, session.scope ≠ first.scope)
    (scope : requested.scope = first.scope) (agent : requested.agent ≠ first.agent) :
    ensureInStore (store ++ [first]) requested = none := by
  have found : (store ++ [first]).find? (fun session => session.scope == requested.scope) =
      some first := by
    rw [scope]
    exact find_in_extended store first first.scope absent rfl
  unfold ensureInStore
  rw [found]
  have refused : ensure? (some first) requested = none := by
    simp [ensure?, scope, Ne.symm agent]
  simp [refused]

end Triggers.SessionKey
