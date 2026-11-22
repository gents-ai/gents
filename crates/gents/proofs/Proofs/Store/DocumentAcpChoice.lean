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

/-- What a store records of the choice. A fresh store records nothing yet.
External premise: a store created on an embedded Vera node records that
deployment's consensus key in its data directory, before its lineage marker
is written, and a store created on the node's own store records none. A store whose first initialization stopped before the marker is
never opened (`FreshBackendStore`), so every opened store that was created on
an embedded Vera node has the record, and a deployment whose key was not yet
recorded is never paired with a store that holds documents. Such a home is
recovered as every interrupted initialization is: retired and set up again. -/
inductive Store where
  | fresh
  | createdOnNodeStore
  | createdOnEmbeddedVera
  deriving DecidableEq, Repr

def Store.recorded : Store → Option Provider
  | .fresh => none
  | .createdOnNodeStore => some .nodeStore
  | .createdOnEmbeddedVera => some .embeddedVera

/-- What a node opening a home's store does about document access control,
and what `gents init`, a serving runtime and an offline client do about a
home's choice from its init config alone, before anything is written or a node
is built (there `store` is what the init config records: `.fresh` for a home
that has none). A fresh store takes the request. External premise: an embedded
Vera node keeps all its state in one directory inside the store's data
directory, created when the store is, so the store's lock, lineage marker and
retirement cover it. `requested` is the home's init config.

Both providers are entered on every admitted start. External premises: a
client mints a delegation for every write it signs on an embedded Vera node
(`Identity.UserDelegation`), and the runtime owes the relationship changes it
stages inside a row transaction to the provider and waits for its own
(`Identity.OwnerStaged`), so a home on an embedded Vera node initializes and
serves as one on the node's store does. What such a home still cannot do is
refused by the owner of that operation, never by refusing the home: peer
publications (`Identity.NodePublication.publication`). -/
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

/-- Only a fresh store's admission creates a deployment. -/
def Admission.createsDeployment (store : Store) : Admission → Bool
  | .embeddedVera => store == .fresh
  | _ => false

theorem an_existing_store_never_creates_a_deployment
    (store : Store) (requested : Provider) (available nodeKeySigns : Bool)
    (existing : store ≠ .fresh) :
    (admit store requested available nodeKeySigns).createsDeployment store = false := by
  cases store <;> cases requested <;> cases available <;> cases nodeKeySigns
    <;> simp_all [admit, Admission.createsDeployment, Store.recorded]

/-- What a node finds of an opted-in home's Vera deployment when it opens the
store again. -/
inductive Reopening where
  | served
  /-- The Vera node's directory is gone while the store remains. A deployment
  created in its place would hold the seeded policies and none of the
  relationships, so every document would lose its access records. -/
  | deploymentMissing
  /-- The directory's deployment was created under another chain identifier
  or other seeded policies than this node and build give it. -/
  | deploymentDiffers
  /-- The directory holds a deployment whose consensus key is not the one the
  store recorded: another deployment, or one created where the home's was. -/
  | deploymentReplaced
  deriving DecidableEq, Repr

/-- How a refused home is reported: as one this build cannot take, never as a
node failure worth another attempt. -/
inductive Refusal where
  /-- Another version's policies: the home is left to the version that made it. -/
  | otherVersion
  /-- The home lost part of itself and cannot be read back. -/
  | damaged
  deriving DecidableEq, Repr

/-- External premise: a Vera node reopens a directory only under the chain
identifier and seeded policies it was created with, and refuses before its
consensus key can be read, so `sameIdentity` is decided before `sameKey`.
The chain identifier is a function of the Node's DID and the seeded policies
are the build's, so neither is recorded. -/
def reopen (present sameIdentity sameKey : Bool) : Reopening :=
  if !present then .deploymentMissing
  else if !sameIdentity then .deploymentDiffers
  else if !sameKey then .deploymentReplaced
  else .served

def Reopening.refusal : Reopening → Option Refusal
  | .served => none
  | .deploymentMissing => some .damaged
  | .deploymentDiffers => some .otherVersion
  | .deploymentReplaced => some .damaged

structure Reopened where
  reopening : Reopening
  refusal : Option Refusal
  deriving DecidableEq, Repr

def reopened (present sameIdentity sameKey : Bool) : Reopened :=
  let reopening := reopen present sameIdentity sameKey
  { reopening, refusal := reopening.refusal }

theorem an_opted_in_home_opens_only_under_the_deployment_it_was_created_with
    (present sameIdentity sameKey : Bool) (h : reopen present sameIdentity sameKey = .served) :
    present = true ∧ sameIdentity = true ∧ sameKey = true := by
  cases present <;> cases sameIdentity <;> cases sameKey <;> simp_all [reopen]

theorem a_missing_deployment_is_refused_whatever_else_holds (sameIdentity sameKey : Bool) :
    reopen false sameIdentity sameKey = .deploymentMissing := rfl

theorem every_refused_reopening_is_an_incompatible_home
    (present sameIdentity sameKey : Bool) :
    (reopen present sameIdentity sameKey).refusal.isSome
      = (reopen present sameIdentity sameKey != .served) := by
  cases present <;> cases sameIdentity <;> cases sameKey <;> rfl

/-- Which IDs a home names its built-in policies by (`HomePolicyIds`).
External premises: the node's own store derives a policy's ID from the
definition and the counter it is added under, which is 0 on every home
(`counter`); vera.rs derives it from the definition, a nonce and the
creator, and every embedded Vera deployment is seeded with the built-in
policies under vera.rs' genesis creator and a fixed nonce per policy
(`seeded`). The two sets differ, so the bundled SDL is rendered with the
home's set wherever it is loaded, and every check of a binding compares
against that set. -/
inductive PolicyIds where
  | counter
  | seeded
  deriving DecidableEq, Repr

/-- Decision (gents#3831): a home's policy IDs follow its provider. -/
def policyIds : Provider → PolicyIds
  | .nodeStore => .counter
  | .embeddedVera => .seeded

/-- A home on the node's own store names the IDs every such home named
before; every Vera home names the seeded ones. -/
theorem a_vera_home_names_its_seeded_policies : policyIds .embeddedVera = .seeded := rfl

theorem a_node_store_home_keeps_its_counter_ids : policyIds .nodeStore = .counter := rfl

end DocumentAcpChoice

