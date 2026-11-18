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

/-- What a node opening a home's store does about document access control. A
fresh store takes the request. External premise: an embedded Vera node keeps
all its state in one directory inside the store's data directory, created
when the store is, so the store's lock, lineage marker and retirement cover
it. `requested` is the home's init config. -/
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

/-- Whether a home may be created or opened on a provider yet. External
premises: a Vera node accepts a write only under a delegation, and clients do
not mint one yet, so every write a User signs is rejected; and DefraDB stages a
relationship write inside a row transaction for the node's own store alone,
while the runtime stages them there. A home on an embedded Vera node would
therefore stop partway through its initialization and could not be served.
This definition is the only statement of that limit: when both premises are
gone, `.embeddedVera` becomes usable here and `enter` is `admit`. -/
def Provider.usable : Provider → Bool
  | .nodeStore => true
  | .embeddedVera => false

inductive Entry where
  | nodeStore
  | embeddedVera
  | switchRefused
  | veraUnavailable
  | nodeKeyCannotSign
  /-- The home would be admitted on a provider that is not usable yet. -/
  | notYetUsable
  deriving DecidableEq, Repr

/-- What `gents init`, a serving runtime and an offline client do about a
home's choice, from its init config alone and before anything is written or a
node is built. `store` is what the init config records: `.fresh` for a home
that has none. Every refusal of `admit` is kept and comes first, so a build
without the Vera node, a recorded choice and a key that cannot sign are named
as they were; the limit refuses only what `admit` would have started. The
owners of a store (`admit`, `reopen`) are not subject to it. -/
def enter (store : Store) (requested : Provider) (available nodeKeySigns : Bool) : Entry :=
  match admit store requested available nodeKeySigns with
  | .nodeStore => if Provider.nodeStore.usable then .nodeStore else .notYetUsable
  | .embeddedVera => if Provider.embeddedVera.usable then .embeddedVera else .notYetUsable
  | .switchRefused => .switchRefused
  | .veraUnavailable => .veraUnavailable
  | .nodeKeyCannotSign => .nodeKeyCannotSign

def Entry.started : Entry → Option Provider
  | .nodeStore => some .nodeStore
  | .embeddedVera => some .embeddedVera
  | _ => none

theorem a_home_is_entered_only_on_a_usable_provider
    (store : Store) (requested provider : Provider) (available nodeKeySigns : Bool)
    (h : (enter store requested available nodeKeySigns).started = some provider) :
    provider.usable = true := by
  cases store <;> cases requested <;> cases provider <;> cases available <;> cases nodeKeySigns
    <;> simp_all [enter, admit, Entry.started, Store.recorded, Provider.usable]

theorem an_entered_home_is_one_its_store_admits
    (store : Store) (requested provider : Provider) (available nodeKeySigns : Bool)
    (h : (enter store requested available nodeKeySigns).started = some provider) :
    (admit store requested available nodeKeySigns).started = some provider := by
  cases store <;> cases requested <;> cases provider <;> cases available <;> cases nodeKeySigns
    <;> simp_all [enter, admit, Entry.started, Admission.started, Store.recorded, Provider.usable]

theorem the_limit_refuses_only_what_the_store_would_start
    (store : Store) (requested : Provider) (available nodeKeySigns : Bool)
    (h : enter store requested available nodeKeySigns = .notYetUsable) :
    ∃ provider, (admit store requested available nodeKeySigns).started = some provider
      ∧ provider.usable = false := by
  cases store <;> cases requested <;> cases available <;> cases nodeKeySigns
    <;> simp_all [enter, admit, Admission.started, Store.recorded, Provider.usable]

theorem a_home_on_the_node_store_is_entered_as_it_is_admitted
    (store : Store) (available nodeKeySigns : Bool)
    (h : admit store .nodeStore available nodeKeySigns = .nodeStore) :
    enter store .nodeStore available nodeKeySigns = .nodeStore := by
  simp [enter, h, Provider.usable]

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

end DocumentAcpChoice
