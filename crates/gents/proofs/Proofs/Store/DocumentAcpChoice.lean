/-! Where a home's document access control is decided: in the node's own
store, or through a Vera node the runtime hosts beside it (gents#3831). The
choice is made when the home is initialized and holds for every later start.
Nothing here changes who may do what: the Household policy is the same under
either provider. -/

namespace DocumentAcpChoice

inductive Provider where
  | nodeStore
  | embeddedVera
  deriving DecidableEq, Repr

inductive Admission where
  | nodeStore
  | embeddedVera
  /-- The store was created with the other provider. Relationships written
  under one are not carried to the other, so there is no conversion. -/
  | switchRefused
  /-- This build does not carry the Vera node. -/
  | veraUnavailable
  /-- The Vera node acts as the Node, under delegations the Node key signs. A
  Secure Enclave key signs none of them, and such a home is not served
  either (`ensure_home_identity_can_serve`). -/
  | nodeKeyCannotSign
  deriving DecidableEq, Repr

/-- What a store records of the choice. A fresh store records nothing yet. -/
inductive Store where
  | fresh
  | createdOnNodeStore
  | createdOnEmbeddedVera
  deriving DecidableEq, Repr

def Store.recorded : Store → Option Provider
  | .fresh => none
  | .createdOnNodeStore => some .nodeStore
  | .createdOnEmbeddedVera => some .embeddedVera

/-- What a node opening a home's store does about document access control. A
fresh store takes the request. External premise: an embedded Vera node keeps
all its state in one directory inside the store's data directory, created
when the store is, so its presence is the record and the store's lock,
lineage marker and retirement cover it. `requested` is the home's init
config. -/
def admit (store : Store) (requested : Provider) (available nodeKeySigns : Bool) : Admission :=
  if store.recorded.any (· != requested) then .switchRefused
  else match requested with
    | .nodeStore => .nodeStore
    | .embeddedVera =>
      if !available then .veraUnavailable
      else if !nodeKeySigns then .nodeKeyCannotSign
      else .embeddedVera

def Admission.started : Admission → Option Provider
  | .nodeStore => some .nodeStore
  | .embeddedVera => some .embeddedVera
  | _ => none

theorem a_start_uses_the_provider_the_store_was_created_with
    (store : Store) (requested recorded provider : Provider) (available nodeKeySigns : Bool)
    (created : store.recorded = some recorded)
    (h : (admit store requested available nodeKeySigns).started = some provider) :
    provider = recorded := by
  cases store <;> cases requested <;> cases recorded <;> cases provider <;> cases available
    <;> cases nodeKeySigns <;> simp_all [admit, Admission.started, Store.recorded]

theorem an_opted_in_home_never_falls_back_to_the_node_store
    (requested : Provider) (available nodeKeySigns : Bool) :
    admit .createdOnEmbeddedVera requested available nodeKeySigns ≠ .nodeStore := by
  cases requested <;> cases available <;> cases nodeKeySigns <;> decide

theorem a_home_that_did_not_opt_in_never_starts_a_vera_node
    (requested : Provider) (available nodeKeySigns : Bool) :
    admit .createdOnNodeStore requested available nodeKeySigns ≠ .embeddedVera := by
  cases requested <;> cases available <;> cases nodeKeySigns <;> decide

theorem a_fresh_store_takes_the_request (requested : Provider) :
    (admit .fresh requested true true).started = some requested := by
  cases requested <;> rfl

theorem a_vera_node_needs_the_build_and_a_signing_node_key
    (store : Store) (requested : Provider) (available nodeKeySigns : Bool)
    (h : admit store requested available nodeKeySigns = .embeddedVera) :
    available = true ∧ nodeKeySigns = true := by
  cases store <;> cases requested <;> cases available <;> cases nodeKeySigns
    <;> simp_all [admit, Store.recorded]

theorem the_node_store_needs_neither (store : Store) (a b c d : Bool) :
    admit store .nodeStore a b = admit store .nodeStore c d := by
  cases store <;> rfl

/-- What a node finds of an opted-in home's Vera deployment when it opens the
store again. -/
inductive Reopening where
  | served
  /-- The directory's deployment was created under another chain identifier
  or other seeded policies than this node and build give it. -/
  | deploymentDiffers
  deriving DecidableEq, Repr

/-- How a refused home is reported: as one this build cannot take, never as a
node failure worth another attempt. -/
inductive Refusal where
  /-- Another version's policies: the home is left to the version that made it. -/
  | otherVersion
  deriving DecidableEq, Repr

/-- External premise: a Vera node reopens a directory only under the chain
identifier and seeded policies it was created with. The chain identifier is a
function of the Node's DID and the seeded policies are the build's. -/
def reopen (sameIdentity : Bool) : Reopening :=
  if !sameIdentity then .deploymentDiffers else .served

def Reopening.refusal : Reopening → Option Refusal
  | .served => none
  | .deploymentDiffers => some .otherVersion

structure Reopened where
  reopening : Reopening
  refusal : Option Refusal
  deriving DecidableEq, Repr

def reopened (sameIdentity : Bool) : Reopened :=
  let reopening := reopen sameIdentity
  { reopening, refusal := reopening.refusal }

theorem an_opted_in_home_opens_only_under_the_deployment_it_was_created_with
    (sameIdentity : Bool) (h : reopen sameIdentity = .served) : sameIdentity = true := by
  cases sameIdentity <;> simp_all [reopen]

theorem every_refused_reopening_is_an_incompatible_home (sameIdentity : Bool) :
    (reopen sameIdentity).refusal.isSome = (reopen sameIdentity != .served) := by
  cases sameIdentity <;> rfl

end DocumentAcpChoice
