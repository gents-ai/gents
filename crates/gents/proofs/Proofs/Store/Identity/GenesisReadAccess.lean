/-! What a node receiving a replicated document's genesis may read of it before
any relationship reaches it. Only discovery and enrollment documents, under
this home's Household policy, are read from their genesis: the peer registry
by anyone, an enrollment record by the node it names as its first reader.

External premise: DefraDB asks the runtime once per received genesis, with the
policy ID and resource of the receiving collection's own binding, and can take
no refusal from it; an absent answer leaves the genesis unread. -/

namespace Identity.GenesisReadAccess

/-- The receiving collection. Each enrollment record is listed; every other
collection is `other`. -/
inductive Collection where
  | peerRegistry
  | networkEnrollmentRequest
  | networkEnrollmentDecision
  | networkAuthorizationRevision
  | networkEnrollmentRouteReceipt
  | enrollmentRenewalIntent
  | enrollmentRenewalOffer
  | other
  deriving DecidableEq, Repr

inductive Resource where
  | discovery
  | enrollmentRecords
  | other
  deriving DecidableEq, Repr

/-- Whether the collection's binding names this home's Household policy ID,
or any other policy. -/
inductive Binding where
  | homeHousehold
  | otherPolicy
  deriving DecidableEq, Repr

inductive Access where
  | none
  | everyone
  | addressedReceiver
  /-- A collection whose genesis this home reads is bound to a policy other
  than its Household policy. Since a policy's ID is the home's own, the
  bindings and the IDs disagree about which home this is; that is reported,
  never read as the absence of access. -/
  | policyMismatch
  deriving DecidableEq, Repr

def Collection.enrollmentRecord : Collection → Bool
  | .networkEnrollmentRequest | .networkEnrollmentDecision | .networkAuthorizationRevision
  | .networkEnrollmentRouteReceipt | .enrollmentRenewalIntent | .enrollmentRenewalOffer => true
  | _ => false

/-- The access the Household policy gives a genesis of this collection and
resource, whatever the binding. -/
def householdAccess (collection : Collection) (resource : Resource) : Access :=
  match collection, resource with
  | .peerRegistry, .discovery => .everyone
  | collection, .enrollmentRecords =>
      if collection.enrollmentRecord then .addressedReceiver else .none
  | _, _ => .none

def receiverAccess (collection : Collection) (resource : Resource) (binding : Binding) : Access :=
  match householdAccess collection resource, binding with
  | .none, _ => .none
  | access, .homeHousehold => access
  | _, .otherPolicy => .policyMismatch

theorem only_the_home_household_policy_grants
    (collection : Collection) (resource : Resource)
    (h : receiverAccess collection resource .otherPolicy ≠ .none) :
    receiverAccess collection resource .otherPolicy = .policyMismatch := by
  cases collection <;> cases resource <;> simp_all [receiverAccess, householdAccess,
    Collection.enrollmentRecord]

theorem a_mismatch_is_reported_exactly_where_the_home_would_grant
    (collection : Collection) (resource : Resource) :
    (receiverAccess collection resource .otherPolicy = .policyMismatch)
      = (receiverAccess collection resource .homeHousehold ≠ .none) := by
  cases collection <;> cases resource <;> simp [receiverAccess, householdAccess,
    Collection.enrollmentRecord]

theorem the_peer_registry_is_public_and_nothing_else_is :
    ∀ collection resource,
      receiverAccess collection resource .homeHousehold = .everyone ↔
        collection = .peerRegistry ∧ resource = .discovery := by
  intro collection resource
  cases collection <;> cases resource <;> simp [receiverAccess, householdAccess,
    Collection.enrollmentRecord]

end Identity.GenesisReadAccess
