import Proofs.Enrollment.DurableProjection

/-!
Desktop chat write readiness: the client-side fence that decides whether a chat
send (submit, resend, retry, or replay of an ambiguous submission) is attempted
against a saved deployment route, or refused before any request is saved.

External premises (#3538, discovery-only P2P):
* The desktop writes to its runtime only over the authenticated operator HTTP
  endpoint (`/api/v0/graphql`), signed by the User client key, with `client`
  request admission. There is no replica fallback: without an operator
  endpoint no write path exists.
* The runtime's `client` admission branch and its HTTP access layer do not read
  the enrollment authorization lease; the lease is checked only for
  `enrollment` admission, which the desktop no longer emits.
* `pairingReady` is written only by the desktop route reconciler (both P2P
  route legs applied), and cleared by restart, authority failure, endpoint
  rotation, generation rotation and the stale-record write path. Under
  discovery-only P2P those legs carry only `PeerRegistry`.

Two candidate contracts are encoded. `operatorEndpointSuffices` is the
contract implemented by 4af6350c91. `enrollmentFenceRequired` is the
pre-#3538 fence, restated with the operator endpoint it now needs. Which one
is intended is an open product decision; until it is made, the native owner
implements the narrower `enrollmentFenceRequired`
(`fence_is_narrower` proves it never admits a send the other refuses).
-/

namespace ClientChatReadiness

/-- Saved route provenance. `other` covers manual and source-less records,
which were never chat authorities. -/
inductive Source where
  | localStandard
  | enrollment
  | other
  deriving DecidableEq, Repr

/-- One saved deployment route as the desktop's peer directory holds it.
`generationComplete` is the conjunction of network id, request digest and a
positive authorization sequence. `leaseExpiresAt = none` is an absent or
non-canonical expiry, which fails closed. -/
structure Route where
  source : Source
  operatorEndpoint : Bool
  pairingReady : Bool
  generationComplete : Bool
  leaseExpiresAt : Option Nat
  deriving DecidableEq, Repr

/-- A lease is fresh strictly before its expiry, matching
`gents_protocol::enrollment::authorization_lease_is_fresh_at`. -/
def leaseFresh (route : Route) (now : Nat) : Bool :=
  match route.leaseExpiresAt with
  | some expiresAt => decide (now < expiresAt)
  | none => false

/-- Why a send is refused. Each cause has exactly one next action. -/
inductive Cause where
  | noRoute
  | noEndpoint
  | unownedRoute
  | leaseExpired
  | enrollmentIncomplete
  | pairingPending
  deriving DecidableEq, Repr

inductive Readiness where
  | ready
  | notReady (cause : Cause)
  deriving DecidableEq, Repr

/-- The existing enrollment route predicate, reused rather than restated. -/
def enrollmentReady (route : Route) (now : Nat) : Bool :=
  Enrollment.enrollmentRouteReady (route.source == .enrollment) route.pairingReady
    route.generationComplete route.generationComplete route.generationComplete
    (leaseFresh route now) route.operatorEndpoint

/-- Contract A (4af6350c91): a valid operator endpoint alone opens writes;
otherwise an enrollment route that is ready opens them. -/
def operatorEndpointSuffices (route : Option Route) (now : Nat) : Readiness :=
  match route with
  | none => .notReady .noRoute
  | some route =>
    if route.operatorEndpoint || enrollmentReady route now then .ready
    else .notReady .pairingPending

/-- Contract B: the only write path is the operator endpoint, and the saved
route's own fence must also be open. Cause precedence names the first thing
the user can act on: connect, then re-enroll, then wait for pairing repair. -/
def enrollmentFenceRequired (route : Option Route) (now : Nat) : Readiness :=
  match route with
  | none => .notReady .noRoute
  | some route =>
    if !route.operatorEndpoint then .notReady .noEndpoint
    else match route.source with
      | .other => .notReady .unownedRoute
      | .localStandard =>
        if route.pairingReady then .ready else .notReady .pairingPending
      | .enrollment =>
        if !leaseFresh route now then .notReady .leaseExpired
        else if !route.generationComplete then .notReady .enrollmentIncomplete
        else if !route.pairingReady then .notReady .pairingPending
        else .ready

/-- Durable transitions that change readiness inputs. `restart` is
`mark_managed_runtime_restarting`; `authorityFailure` is
`demote_enrollment_readiness_after_authority_failure` and
`demote_enrollment_peer`; `reconciled ready` is `persist_pairing_readiness`. -/
inductive Event where
  | restart
  | authorityFailure
  | reconciled (ready : Bool)
  | endpointLost
  deriving DecidableEq, Repr

def apply : Event → Route → Route
  | .restart, route => { route with pairingReady := false }
  | .authorityFailure, route =>
    if route.source == .enrollment then { route with pairingReady := false } else route
  | .reconciled ready, route => { route with pairingReady := ready }
  | .endpointLost, route => { route with operatorEndpoint := false }

/-! Where the contracts differ. -/

theorem fence_is_narrower (route : Option Route) (now : Nat) :
    enrollmentFenceRequired route now = .ready → operatorEndpointSuffices route now = .ready := by
  cases route with
  | none => simp [enrollmentFenceRequired]
  | some route =>
    rcases route with ⟨source, endpoint, pairing, generation, expiry⟩
    cases source <;> cases endpoint <;> simp [enrollmentFenceRequired, operatorEndpointSuffices]

/-- Under contract A a restart with an operator endpoint leaves writes open. -/
theorem endpoint_suffices_keeps_writes_open_through_restart (route : Route) (now : Nat)
    (endpoint : route.operatorEndpoint = true) :
    operatorEndpointSuffices (some (apply .restart route)) now = .ready := by
  simp [operatorEndpointSuffices, apply, endpoint]

/-- Under contract A an expired lease with an operator endpoint leaves writes open. -/
theorem endpoint_suffices_ignores_expired_lease (route : Route) (now : Nat)
    (endpoint : route.operatorEndpoint = true) (_expired : leaseFresh route now = false) :
    operatorEndpointSuffices (some route) now = .ready := by
  simp [operatorEndpointSuffices, endpoint]

/-- Under contract A an authority failure with an operator endpoint leaves writes open. -/
theorem endpoint_suffices_ignores_authority_demotion (route : Route) (now : Nat)
    (endpoint : route.operatorEndpoint = true) :
    operatorEndpointSuffices (some (apply .authorityFailure route)) now = .ready := by
  by_cases enrollment : route.source = .enrollment <;>
    simp [apply, enrollment, operatorEndpointSuffices, endpoint]

/-- Under contract A a ready enrollment route without an endpoint is reported
ready although no write path exists; the send then fails later with a
different error. -/
theorem endpoint_suffices_admits_endpointless_enrollment :
    operatorEndpointSuffices
      (some ⟨.enrollment, false, true, true, some 10⟩) 0 = .ready := by decide

/-- Under contract B a restart closes writes until the route reconciles. -/
theorem restart_closes_writes (route : Route) (now : Nat) :
    enrollmentFenceRequired (some (apply .restart route)) now ≠ .ready := by
  rcases route with ⟨source, endpoint, pairing, generation, expiry⟩
  cases source <;> cases endpoint <;> cases generation <;>
    simp [enrollmentFenceRequired, apply] <;> split <;> simp

/-- Under contract B an expired lease is refused and says so. -/
theorem expired_lease_names_its_cause (route : Route) (now : Nat)
    (enrollment : route.source = .enrollment) (endpoint : route.operatorEndpoint = true)
    (expired : leaseFresh route now = false) :
    enrollmentFenceRequired (some route) now = .notReady .leaseExpired := by
  simp [enrollmentFenceRequired, enrollment, endpoint, expired]

/-- Under contract B an authority failure closes enrollment writes. -/
theorem authority_demotion_closes_enrollment_writes (route : Route) (now : Nat)
    (enrollment : route.source = .enrollment) :
    enrollmentFenceRequired (some (apply .authorityFailure route)) now ≠ .ready := by
  rcases route with ⟨source, endpoint, pairing, generation, expiry⟩
  subst enrollment
  cases endpoint <;> cases generation <;>
    simp [enrollmentFenceRequired, apply] <;> split <;> simp

/-- Both contracts agree once the route has reconciled, its endpoint is
present and its enrollment generation is fresh. -/
theorem contracts_agree_on_a_reconciled_route (source : Source) (expiry now : Nat)
    (owned : source ≠ .other) (fresh : now < expiry) :
    enrollmentFenceRequired (some ⟨source, true, true, true, some expiry⟩) now =
      operatorEndpointSuffices (some ⟨source, true, true, true, some expiry⟩) now := by
  cases source <;> simp_all [enrollmentFenceRequired, operatorEndpointSuffices, leaseFresh]

/-! Refusals and the user-visible composer state. -/

/-- The next action a refusal tells the user to take. -/
inductive NextAction where
  | addDeployment
  | reconnectRuntime
  | reEnroll
  | waitForPairingRepair
  deriving DecidableEq, Repr

def nextAction : Cause → NextAction
  | .noRoute => .addDeployment
  | .noEndpoint => .reconnectRuntime
  | .unownedRoute => .addDeployment
  | .leaseExpired => .reEnroll
  | .enrollmentIncomplete => .reEnroll
  | .pairingPending => .waitForPairingRepair

/-- The stable token a refusal message carries for its cause. -/
def causeToken : Cause → String
  | .noRoute => "no_route"
  | .noEndpoint => "no_endpoint"
  | .unownedRoute => "unowned_route"
  | .leaseExpired => "lease_expired"
  | .enrollmentIncomplete => "enrollment_incomplete"
  | .pairingPending => "pairing_pending"

def allCauses : List Cause :=
  [.noRoute, .noEndpoint, .unownedRoute, .leaseExpired, .enrollmentIncomplete, .pairingPending]

theorem allCauses_complete (cause : Cause) : cause ∈ allCauses := by
  cases cause <;> decide

/-- A refusal names its cause: distinct causes carry distinct tokens, so the
`writes.rs` message identifies which fence input closed the write. -/
theorem refusal_names_cause (left right : Cause) :
    causeToken left = causeToken right → left = right := by
  cases left <;> cases right <;> decide

inductive Composer where
  | open
  | blocked (cause : Cause) (action : NextAction)
  deriving DecidableEq, Repr

def composer : Readiness → Composer
  | .ready => .open
  | .notReady cause => .blocked cause (nextAction cause)

/-- No user-visible state is silently stuck: the composer is open, or it names
the cause and the next action. -/
theorem composer_never_silently_stuck (readiness : Readiness) :
    composer readiness = .open ∨
      ∃ cause, composer readiness = .blocked cause (nextAction cause) := by
  cases readiness with
  | ready => exact Or.inl rfl
  | notReady cause => exact Or.inr ⟨cause, rfl⟩

/-- Pairing repair is the only progress a `pairingPending` refusal awaits:
reconciling a route with a fresh, complete generation and an endpoint reopens it. -/
theorem pairing_repair_reopens (source : Source) (expiry now : Nat)
    (owned : source ≠ .other) (fresh : now < expiry) :
    enrollmentFenceRequired
      (some (apply (.reconciled true) ⟨source, true, false, true, some expiry⟩)) now = .ready := by
  cases source <;> simp_all [enrollmentFenceRequired, apply, leaseFresh]

/-! Time: the snapshot's `chat_safe` is an observation at its own instant. -/

/-- A lease that was fresh at snapshot time can be expired at send time, so
only the send-time evaluation decides; a stale snapshot never admits a write
because the native send path re-evaluates at its own `now`. -/
theorem snapshot_readiness_can_lapse_before_send :
    let route : Route := ⟨.enrollment, true, true, true, some 10⟩
    enrollmentFenceRequired (some route) 9 = .ready ∧
      enrollmentFenceRequired (some route) 10 = .notReady .leaseExpired := by
  decide

/-! Recovery of an ambiguous submission. -/

/-- Replay of a retained submission needs owner access, which is granted by the
same send-time readiness. A refusal keeps the submission unknown and carries
the refusal's cause; it never prepares a new identity. -/
inductive RecoveryAttempt where
  | replay
  | unknown (cause : Option Cause)
  deriving DecidableEq, Repr

def recoveryAttempt : Readiness → RecoveryAttempt
  | .ready => .replay
  | .notReady cause => .unknown (some cause)

theorem refused_recovery_names_cause (cause : Cause) :
    recoveryAttempt (.notReady cause) = .unknown (some cause) := rfl

/-! Route reconcile outcomes. -/

/-- One reconcile leg's outcome. -/
inductive Leg where
  | ready
  | drifted
  | unavailable
  deriving DecidableEq, Repr

/-- Readiness persistence after one reconcile: confirmed drift closes, both
legs ready opens, and an unavailable observation keeps the prior value until a
retry observes. Decision record: an outage of the P2P route observation is not
evidence that the HTTP write path is down, so it does not demote. -/
def combinedLegs : Leg → Leg → Option Bool
  | .ready, .ready => some true
  | .drifted, _ => some false
  | _, .drifted => some false
  | _, _ => none

theorem unavailable_preserves_prior_readiness (other : Leg) (h : other ≠ .drifted) :
    combinedLegs .unavailable other = none ∧ combinedLegs other .unavailable = none := by
  cases other <;> simp_all [combinedLegs]

/-! Conformance inputs. -/

def allSources : List Source := [.localStandard, .enrollment, .other]

def caseRoutes : List (Option Route) :=
  none :: (allSources.flatMap fun source =>
    [true, false].flatMap fun endpoint =>
      [true, false].flatMap fun pairing =>
        [true, false].flatMap fun generation =>
          [some 10, some 5, none].map fun expiry =>
            some ⟨source, endpoint, pairing, generation, expiry⟩)

/-- The observation instant the generated cases use. -/
def caseNow : Nat := 7

end ClientChatReadiness
