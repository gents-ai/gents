import Proofs.Identity.State

namespace Identity.NodeProvisioning

/-- These node permissions are separate from document ACP. Their presence does
not establish document authorization, and unbound collections do not acquire
document protection from them. Extra operations require their own justification. -/
def requiredPermissions : List String := ["read-document", "update-document", "add-dac-relation"]

structure State where
  target : DID
  authorized : Bool
  fresh : Bool
  attempted : List String := []
  granted : List String := []
  closed : Bool := false
  revealed : Bool := false
  deriving Repr

/-- Authorization, local derivation and freshness are validated by the
caller. A positive response means the native relation is present. A failed or
ambiguous response may still have applied it, so the attempt closes without
returning a binding and the creation identifier is not reused. Detached native
grants do not atomically fence a concurrent authorization revocation. -/
def attempt (s : State) (permission : String) (applied succeeded : Bool) : State :=
  if s.authorized && s.fresh && !s.target.isEmpty && !s.closed &&
      requiredPermissions.contains permission && !s.attempted.contains permission then
    { s with
      attempted := permission :: s.attempted
      granted := if applied || succeeded then permission :: s.granted else s.granted
      closed := !succeeded }
  else s

def finish (s : State) : State :=
  if s.authorized && s.fresh && !s.target.isEmpty && !s.closed &&
      requiredPermissions.all (s.attempted.contains ·) then
    { s with closed := true, revealed := true }
  else s

def revoke (s : State) (permission : String) : State :=
  { s with granted := s.granted.filter (· != permission) }

/-- Identity resolution and configuration reconciliation are observations;
they do not infer a new node grant from a missing one. Existing identities
need an explicit authorized adoption, not implicit replay of this operation. -/
def reopen (s : State) : State := s

theorem reopen_preserves_revocation (s : State) (permission : String) :
    reopen (revoke s permission) = revoke s permission := by rfl

theorem closed_attempt_is_unchanged (s : State) (permission : String)
    (applied succeeded : Bool) (closed : s.closed = true) :
    attempt s permission applied succeeded = s := by
  simp [attempt, closed]

theorem absent_authority_cannot_grant (s : State) (permission : String)
    (applied succeeded : Bool) (unauthorized : s.authorized = false) :
    attempt s permission applied succeeded = s := by
  simp [attempt, unauthorized]

theorem failed_grant_can_have_applied :
    let initial : State := { target := "agent", authorized := true, fresh := true }
    let failed := attempt initial "read-document" true false
    failed.closed = true ∧ failed.revealed = false ∧ failed.granted.contains "read-document" = true := by
  decide

/-- This witness exposes the race in any detached grant: an administrator can
revoke after an earlier observation and an already-started grant can restore it.
Preventing that needs native conflict validation or additional ordering. -/
theorem detached_grant_may_follow_revocation :
    let initial : State := { target := "agent", authorized := true, fresh := true, granted := ["read-document"] }
    let revoked := revoke initial "read-document"
    let late := attempt revoked "read-document" true true
    revoked.granted.contains "read-document" = false ∧ late.granted.contains "read-document" = true := by
  decide

/-- Before any bootstrap NAC grant, exclusive local initialization durably
consumes a one-shot marker for a verified fresh store. The authenticated client
and matching runtime principal are separately required. An absent Node means an
authoritative absence, not an empty actor-filtered read. Configuration is
published after the grants. A lost or ambiguous reservation response grants
nothing; after a crash, a durable reservation cannot authorize replay. -/
structure BootstrapObservation where
  exclusive : Bool
  matchingRuntime : Bool
  authenticatedClient : Bool
  nodeAbsentObserved : Bool
  storeEmptyObserved : Bool
  reservationDurable : Bool
  nacEnabled : Bool
  freshEvidence : Bool
  configurationPublished : Bool
  deriving Repr

def reservationConfirmed (created fileSynced directorySynced : Bool) : Bool :=
  created && fileSynced && directorySynced

theorem ambiguous_reservation_cannot_authorize (created fileSynced : Bool) :
    reservationConfirmed created fileSynced false = false := by
  cases created <;> cases fileSynced <;> rfl

def bootstrapMayGrant (o : BootstrapObservation) : Bool :=
  o.exclusive && o.matchingRuntime && o.authenticatedClient && o.nodeAbsentObserved &&
    o.storeEmptyObserved && o.reservationDurable && o.nacEnabled && o.freshEvidence &&
    !o.configurationPublished

def bootstrapAfterCrash (o : BootstrapObservation) : BootstrapObservation :=
  { o with freshEvidence := false }

theorem bootstrap_crash_cannot_replay (o : BootstrapObservation) :
    bootstrapMayGrant (bootstrapAfterCrash o) = false := by
  simp [bootstrapMayGrant, bootstrapAfterCrash]

theorem bootstrap_does_not_regrant_existing (o : BootstrapObservation)
    (existing : o.nodeAbsentObserved = false) : bootstrapMayGrant o = false := by
  simp [bootstrapMayGrant, existing]

theorem bootstrap_needs_durable_reservation (o : BootstrapObservation)
    (unreserved : o.reservationDurable = false) : bootstrapMayGrant o = false := by
  simp [bootstrapMayGrant, unreserved]

/-- Starting a server does not grant the Node owner new NAC permissions.
Missing or revoked permissions on an existing identity require an explicit
authorized operation, not implicit startup adoption. -/
def nodeOwnerGrantsAfterStartup (before : List String) : List String := before

theorem startup_preserves_node_owner_revocation (before : List String) (permission : String) :
    nodeOwnerGrantsAfterStartup (before.filter (· != permission)) =
      before.filter (· != permission) := by rfl

end Identity.NodeProvisioning
