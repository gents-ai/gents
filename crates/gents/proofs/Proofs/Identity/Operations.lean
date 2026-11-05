import Proofs.Identity.Household

namespace Identity.Operations

open Identity.Household

/-- A runtime-authored operational document has a separate publication step:
ACP1 creates it for the authenticated runtime before the runtime attaches its
Node parent. A field containing a Node DID does not create this relationship.
The parent grant is a detached operation and may fail or be delayed. -/
def beforeParent (user runtime reader : DID) : State :=
  directCreate
    { objects := [⟨"nodes", "home", user⟩],
      relations := [⟨"nodes", "home", "runtime_reader", .actor reader⟩],
      enrolled := [] }
    runtime "operations" "readiness" true true

def afterParent (user runtime reader : DID) : State :=
  apply (beforeParent user runtime reader)
    (.grantAs runtime ⟨"operations", "readiness", "parent", .object "nodes" "home"⟩)

theorem operational_parent_grant_changes_user_visibility :
    (allowed 5 (beforeParent "user" "runtime" "reader") "user" "operations" "readiness" "read",
     allowed 5 (afterParent "user" "runtime" "reader") "user" "operations" "readiness" "read") = (false, true) := by
  native_decide

theorem parent_node_runtime_reader_inherits_operational_read :
    allowed 5 (afterParent "user" "runtime" "reader") "reader" "operations" "readiness" "read" = true := by
  native_decide

theorem runtime_owns_operation_user_reads_but_cannot_update :
    (allowed 5 (afterParent "user" "runtime" "reader") "runtime" "operations" "readiness" "update",
     allowed 5 (afterParent "user" "runtime" "reader") "user" "operations" "readiness" "read",
     allowed 5 (afterParent "user" "runtime" "reader") "user" "operations" "readiness" "update",
     allowed 5 (afterParent "user" "runtime" "reader") "agent" "operations" "readiness" "read") =
      (true, true, false, false) := by
  native_decide

/-- A document owner can attach a Node parent without proving that it is the
Node named by the document. Native ownership and a parent tuple alone therefore
do not authenticate a readiness publisher or its target. -/
def forgedParent : State :=
  apply (directCreate
    { objects := [⟨"nodes", "home", "user"⟩], relations := [], enrolled := [] }
    "other" "operations" "forged" true true)
    (.grantAs "other" ⟨"operations", "forged", "parent", .object "nodes" "home"⟩)

theorem forged_operation_can_inherit_parent_read :
    allowed 5 forgedParent "user" "operations" "forged" "read" = true := by
  native_decide

/-- Installing a policy on an existing unbound collection can leave legacy
objects readable without registered ownership. Only an absent collection or
one already bound to the expected policy is eligible for this fresh path. A
matching schema does not itself verify registration of its existing objects. -/
inductive CollectionBinding where
  | absent | expected | unbound | different
  deriving BEq, Repr

def readinessPreflight : CollectionBinding → Bool
  | .absent | .expected => true
  | .unbound | .different => false

theorem unbound_readiness_rejected : readinessPreflight .unbound = false := by rfl

/-- Recovery authenticates the runtime against native ownership and compares
the stored physical parent with the uniquely observed current Node. These are
fresh observations, not a native transaction fence on the later grant. -/
def readinessRecoveryEligible (pending : Bool) (runtime embeddedRuntime : DID) (parentRegistered : Bool)
    (nativeOwner recordedParent currentParent : Option String) : Bool :=
  pending && !runtime.isEmpty && runtime == embeddedRuntime && parentRegistered && nativeOwner == some runtime &&
    recordedParent.isSome && recordedParent == currentParent

def repairReadiness (state : ParentPublication) (runtime embeddedRuntime : DID) (parentRegistered : Bool)
    (nativeOwner recordedParent currentParent : Option String)
    (grantSucceeded markSucceeded : Bool) : ParentPublication :=
  if readinessRecoveryEligible (!state.parentLinked) runtime embeddedRuntime parentRegistered nativeOwner recordedParent currentParent then
    repairParent state grantSucceeded markSucceeded
  else state

theorem readiness_unknown_owner_cannot_repair (state : ParentPublication) (runtime embeddedRuntime : DID)
    (parentRegistered : Bool)
    (recordedParent currentParent : Option String) (grantSucceeded markSucceeded : Bool) :
    repairReadiness state runtime embeddedRuntime parentRegistered none recordedParent currentParent grantSucceeded markSucceeded = state := by
  simp [repairReadiness, readinessRecoveryEligible]

theorem readiness_wrong_parent_cannot_repair (state : ParentPublication) (runtime embeddedRuntime owner : DID)
    (parentRegistered : Bool)
    (recordedParent currentParent : Option String) (different : recordedParent ≠ currentParent)
    (grantSucceeded markSucceeded : Bool) :
    repairReadiness state runtime embeddedRuntime parentRegistered (some owner) recordedParent currentParent grantSucceeded markSucceeded = state := by
  simp [repairReadiness, readinessRecoveryEligible, different]

theorem readiness_foreign_runtime_cannot_repair (state : ParentPublication) (runtime embeddedRuntime : DID)
    (different : runtime ≠ embeddedRuntime) (parentRegistered : Bool)
    (nativeOwner recordedParent currentParent : Option String) (grantSucceeded markSucceeded : Bool) :
    repairReadiness state runtime embeddedRuntime parentRegistered nativeOwner recordedParent currentParent
      grantSucceeded markSucceeded = state := by
  simp [repairReadiness, readinessRecoveryEligible, different]

theorem readiness_unregistered_parent_cannot_repair (state : ParentPublication) (runtime embeddedRuntime : DID)
    (nativeOwner recordedParent currentParent : Option String) (grantSucceeded markSucceeded : Bool) :
    repairReadiness state runtime embeddedRuntime false nativeOwner recordedParent currentParent
      grantSucceeded markSucceeded = state := by
  simp [repairReadiness, readinessRecoveryEligible]

theorem failed_readiness_grant_cannot_mark_complete :
    (repairReadiness beginParentPublication "runtime" "runtime" true (some "runtime")
      (some "node") (some "node") false true).parentLinked = false := by decide

theorem readiness_grant_survives_failed_marker :
    let first := repairReadiness beginParentPublication "runtime" "runtime" true (some "runtime")
      (some "node") (some "node") true false
    let second := repairReadiness first "runtime" "runtime" true (some "runtime")
      (some "node") (some "node") false true
    first.parentGranted = true ∧ first.parentLinked = false ∧ second.parentLinked = true := by decide

/-- A stale worker may have observed an incomplete marker before another
worker completed publication and an owner revoked the grant. A detached ACP1
grant can then restore the relationship; a pending marker is not a fence. -/
theorem stale_readiness_repair_can_restore_revocation :
    let linked := repairReadiness beginParentPublication "runtime" "runtime" true (some "runtime")
      (some "node") (some "node") true true
    let revoked := ownerRevokesLinkedParent linked
    (finishStaleParentGrant revoked true true).parentGranted = true := by decide

/-- A reader authenticates a readiness row using its native owner and the
physical live Node, not the row's writable node DID alone. Counts and owners
are observations from the same read transaction. They do not fence later
deletion, replacement, permission revocation, or a downstream write. -/
structure ReadinessObservation where
  requestedRuntime : DID
  embeddedRuntime : DID
  nodeCount : Nat
  readinessCount : Nat
  nodeId : Option String
  nodeOwner : Option DID
  readinessOwner : Option DID
  recordedParent : Option String
  parentLinked : Bool
  parentPresent : Bool
  deriving Repr

def readinessObservationUsable (o : ReadinessObservation) : Bool :=
  !o.requestedRuntime.isEmpty && o.requestedRuntime == o.embeddedRuntime &&
    o.nodeCount == 1 && o.readinessCount == 1 &&
    o.nodeId.any (fun id => !id.isEmpty) && o.nodeOwner.any (fun owner => !owner.isEmpty) &&
    o.readinessOwner == some o.requestedRuntime && o.recordedParent == o.nodeId && o.parentLinked

/-- The completion marker records publication progress; it cannot establish
that an owner has not subsequently revoked the native relationship. -/
theorem readiness_marker_does_not_prove_current_relation :
    let o : ReadinessObservation := ⟨"runtime", "runtime", 1, 1, some "node", some "user",
      some "runtime", some "node", true, false⟩
    readinessObservationUsable o = true ∧ o.parentPresent = false := by decide

theorem readiness_unregistered_owner_is_unusable (o : ReadinessObservation)
    (missing : o.readinessOwner = none) : readinessObservationUsable o = false := by
  simp [readinessObservationUsable, missing]

theorem readiness_pending_is_unusable (o : ReadinessObservation)
    (pending : o.parentLinked = false) : readinessObservationUsable o = false := by
  simp [readinessObservationUsable, pending]

/-- Goal reconciliation uses the live runtime status owner. Persisted
NodeRuntime is observable data and cannot establish that the runtime settled. -/
structure LiveReconcileStatus where
  idle : Bool
  lastResultKnown : Bool
  lastResultError : Bool
  deriving Repr

def goalRuntimeSettled (status : Option LiveReconcileStatus) (_persistedSettled : Bool) : Bool :=
  match status with
  | none => false
  | some live => live.idle && live.lastResultKnown && !live.lastResultError

theorem missing_live_runtime_is_not_settled (persisted : Bool) :
    goalRuntimeSettled none persisted = false := by rfl

theorem persisted_runtime_cannot_change_settled (status : Option LiveReconcileStatus) :
    goalRuntimeSettled status false = goalRuntimeSettled status true := by
  cases status <;> rfl

end Identity.Operations
