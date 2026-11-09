/-!
Desktop chat write readiness: the client-side fence that decides whether a chat
send (submit, resend, retry, or replay of an ambiguous submission) is attempted
against a saved deployment route, or refused before any request is saved.

External premises (#3538, discovery-only P2P):
* The desktop writes to its runtime only over the authenticated operator HTTP
  endpoint (`/api/v0/graphql`), signed by the User client key, with `client`
  request admission. There is no replica fallback: without an operator
  endpoint no write path exists.
* The runtime authorizes that write itself, through native DefraDB ACP/NAC on
  the signing DID. Its `client` admission branch and HTTP access layer do not
  read the enrollment authorization lease; the lease is checked only by the
  runtime's remote P2P command fence.
* `pairingReady` records the P2P route legs, which under discovery-only P2P
  carry only `PeerRegistry`. It is written by the route reconciler and cleared
  by managed restart, authority failure, transport address rotation,
  generation rotation and startup.

Decision (supersedes both 4af6350c91's "an operator endpoint or a ready
enrollment route" and the pairing/lease fence this branch first drafted): the
fence is exactly "the saved route has an operator endpoint".
* Pairing and the enrollment lease are P2P route observations, not evidence
  about the HTTP write path (the same reason `combinedLegs` does not demote on
  an unavailable leg). Gating on them made local-runtime chat depend on
  discovery health, which waiting cannot repair when discovery is disabled.
* Refusing on them would be a desktop-side parallel authorization layer; the
  runtime's ACP already decides whether the signed write is admitted, and a
  revoked device is refused there.
* A ready enrollment route without an endpoint has no write path, so it is
  refused rather than admitted (contract A's defect).
A send while the runtime is restarting reaches the endpoint and fails at
transport; the submission journal keeps it retained for exact replay.
-/

namespace ClientChatReadiness

/-- Saved route provenance. `other` covers manual and source-less records. -/
inductive Source where
  | localStandard
  | enrollment
  | other
  deriving DecidableEq, Repr

/-- A saved lease expiry: absent, present but not a canonical timestamp, or an
instant. -/
inductive Lease where
  | absent
  | malformed
  | expiresAt (instant : Nat)
  deriving DecidableEq, Repr

/-- One saved deployment route as the desktop's peer directory holds it.
`managed` is `PeerRecord::is_managed_runtime` (a local runtime home and an
HTTP(S) GraphQL URL). The generation fields are kept separate so partial
generations are generated cases. -/
structure Route where
  source : Source
  operatorEndpoint : Bool
  managed : Bool
  pairingReady : Bool
  networkId : Bool
  requestDigest : Bool
  sequencePositive : Bool
  lease : Lease
  deriving DecidableEq, Repr

/-- A lease is fresh strictly before its expiry and fails closed otherwise,
matching `gents_protocol::enrollment::authorization_lease_is_fresh_at`. -/
def leaseFresh : Lease → Nat → Bool
  | .expiresAt instant, now => decide (now < instant)
  | _, _ => false

theorem lease_is_stale_at_its_expiry (instant : Nat) :
    leaseFresh (.expiresAt instant) instant = false := by
  simp [leaseFresh]

/-- Why a send is refused. -/
inductive Cause where
  /-- No saved route names the runtime. -/
  | noRoute
  /-- A local, managed or manual route holds no operator endpoint. -/
  | noEndpoint
  /-- A remote enrollment route has not yet recorded the runtime's signed HTTP
  advertisement; the enrollment reconcile records it when it resolves. -/
  | endpointNotAdvertised
  deriving DecidableEq, Repr

inductive Readiness where
  | ready
  | notReady (cause : Cause)
  deriving DecidableEq, Repr

/-- The chat write fence (`PeerRecord::chat_refusal`). -/
def chatReadiness : Option Route → Readiness
  | none => .notReady .noRoute
  | some route =>
    if route.operatorEndpoint then .ready
    else if route.source == .enrollment && !route.managed then .notReady .endpointNotAdvertised
    else .notReady .noEndpoint

/-- Route observations do not gate writes: pairing, the enrollment generation
and the lease can take any value without changing readiness. -/
theorem readiness_ignores_route_observations (route : Route)
    (pairing network digest sequence : Bool) (lease : Lease) :
    chatReadiness (some ⟨route.source, route.operatorEndpoint, route.managed,
      pairing, network, digest, sequence, lease⟩) = chatReadiness (some route) := by
  simp [chatReadiness]

theorem endpoint_opens_writes (route : Route) (endpoint : route.operatorEndpoint = true) :
    chatReadiness (some route) = .ready := by
  simp [chatReadiness, endpoint]

/-- A fully ready enrollment route without an endpoint has no write path and is
refused, unlike 4af6350c91. -/
theorem endpointless_route_is_refused (route : Route) (endpoint : route.operatorEndpoint = false) :
    chatReadiness (some route) ≠ .ready := by
  rcases route with ⟨source, operator, managed, pairing, network, digest, sequence, lease⟩
  simp only at endpoint
  subst endpoint
  cases source <;> cases managed <;> simp [chatReadiness]

/-! Durable writers of route inputs. -/

/-- One durable write to the saved route.
* `restart`: `mark_managed_runtime_restarting`, which clears pairing only on
  managed enrollment routes.
* `authorityFailure`: `demote_enrollment_readiness_after_authority_failure` and
  `demote_enrollment_peer`, which clear pairing on enrollment routes.
* `transportRotation`: `refresh_endpoint` replacing a non-enrollment route's
  P2P address, which clears pairing; it never writes the GraphQL endpoint.
* `reconciled outcome`: `persist_pairing_readiness` with
  `combined_route_readiness`; `none` writes nothing.
* `generationRotation`: `upsert_enrollment_peer` with a changed generation,
  which clears pairing on an enrollment route.
* `endpointWritten operator`: a writer that stores the route's GraphQL URL
  (local discovery, an enrollment advertisement, a manual connect);
  `operator` is whether that URL is an operator endpoint.
* `addDeployment operator`: a new manual route where none existed. -/
inductive Event where
  | restart
  | authorityFailure
  | transportRotation
  | reconciled (outcome : Option Bool)
  | generationRotation
  | endpointWritten (operator : Bool)
  | addDeployment (operator : Bool)
  deriving DecidableEq, Repr

def clearPairing (route : Route) : Route := { route with pairingReady := false }

def step : Event → Option Route → Option Route
  | .addDeployment operator, none =>
    some ⟨.other, operator, false, false, false, false, false, .absent⟩
  | _, none => none
  | .addDeployment _, some route => some route
  | .restart, some route =>
    some (if route.source == .enrollment && route.managed then clearPairing route else route)
  | .authorityFailure, some route =>
    some (if route.source == .enrollment then clearPairing route else route)
  | .transportRotation, some route =>
    some (if route.source == .enrollment then route else clearPairing route)
  | .reconciled none, some route => some route
  | .reconciled (some ready), some route => some { route with pairingReady := ready }
  | .generationRotation, some route =>
    some (if route.source == .enrollment then clearPairing route else route)
  | .endpointWritten operator, some route => some { route with operatorEndpoint := operator }

/-- A restart clears pairing only on a managed enrollment route. -/
theorem restart_clears_pairing_only_on_managed_enrollment (route : Route) :
    step .restart (some route) =
      some (if route.source = .enrollment ∧ route.managed = true then clearPairing route else route) := by
  by_cases enrollment : route.source = .enrollment <;>
    by_cases managed : route.managed = true <;> simp [step, enrollment, managed]

/-- Every writer other than an endpoint write or a new deployment touches only
route observations, so it never opens or closes chat writes. -/
def endpointNeutral : Event → Bool
  | .endpointWritten _ | .addDeployment _ => false
  | _ => true

theorem endpoint_neutral_writers_preserve_readiness (event : Event) (route : Option Route)
    (neutral : endpointNeutral event = true) :
    chatReadiness (step event route) = chatReadiness route := by
  cases route with
  | none => cases event <;> simp_all [endpointNeutral, step]
  | some route =>
    cases event with
    | reconciled outcome => cases outcome <;> simp [step, chatReadiness]
    | endpointWritten _ | addDeployment _ => simp [endpointNeutral] at neutral
    | _ => simp only [step]; split <;> simp [chatReadiness, clearPairing]

/-! Refusals and the user-visible composer state. -/

/-- The next action a refusal tells the user to take. -/
inductive NextAction where
  | connectRuntime
  | awaitEndpointAdvertisement
  deriving DecidableEq, Repr

def nextAction : Cause → NextAction
  | .noRoute | .noEndpoint => .connectRuntime
  | .endpointNotAdvertised => .awaitEndpointAdvertisement

/-- The writer each next action leads to. Connecting adds a deployment where
there is none and otherwise records the runtime's operator endpoint; an
advertisement is recorded as an operator endpoint by the enrollment
reconcile. -/
def remedy : Cause → Event
  | .noRoute => .addDeployment true
  | .noEndpoint | .endpointNotAdvertised => .endpointWritten true

/-- No refusal is a dead end: the writer its next action leads to reopens
writes, from every refused route. -/
theorem every_refusal_reopens_by_its_remedy (route : Option Route) (cause : Cause)
    (refused : chatReadiness route = .notReady cause) :
    chatReadiness (step (remedy cause) route) = .ready := by
  cases route with
  | none =>
    simp [chatReadiness] at refused
    subst refused
    simp [remedy, step, chatReadiness]
  | some route =>
    rcases route with ⟨source, operator, managed, pairing, network, digest, sequence, lease⟩
    cases operator <;> cases source <;> cases managed <;> cases cause <;>
      simp_all [chatReadiness, remedy, step]

/-- The stable token a refusal message carries for its cause. -/
def causeToken : Cause → String
  | .noRoute => "no_route"
  | .noEndpoint => "no_endpoint"
  | .endpointNotAdvertised => "endpoint_not_advertised"

def nextActionToken : NextAction → String
  | .connectRuntime => "connect_runtime"
  | .awaitEndpointAdvertisement => "await_endpoint_advertisement"

def allCauses : List Cause := [.noRoute, .noEndpoint, .endpointNotAdvertised]

theorem allCauses_complete (cause : Cause) : cause ∈ allCauses := by
  cases cause <;> decide

theorem refusal_names_cause (left right : Cause) :
    causeToken left = causeToken right → left = right := by
  cases left <;> cases right <;> decide

/-! Recovery of an ambiguous submission. -/

/-- Replay of a retained submission needs owner access, granted by the same
send-time fence. A refusal keeps the submission unknown and carries the
refusal's cause; it never prepares a new identity. A record that needs no
reconciliation is returned as recorded without asking the fence. -/
inductive RecoveryAttempt where
  | recorded
  | replay
  | unknown (cause : Cause)
  deriving DecidableEq, Repr

def recoveryAttempt (needsReconciliation : Bool) (readiness : Readiness) : RecoveryAttempt :=
  if !needsReconciliation then .recorded
  else match readiness with
    | .ready => .replay
    | .notReady cause => .unknown cause

theorem refused_recovery_names_cause (cause : Cause) :
    recoveryAttempt true (.notReady cause) = .unknown cause := rfl

theorem settled_recovery_ignores_the_fence (readiness : Readiness) :
    recoveryAttempt false readiness = .recorded := rfl

/-! Route reconcile outcomes. -/

/-- One reconcile leg's outcome. -/
inductive Leg where
  | ready
  | drifted
  | unavailable
  deriving DecidableEq, Repr

/-- Pairing persistence after one reconcile: confirmed drift closes, both legs
ready opens, and an unavailable observation keeps the prior value until a
retry observes. An outage of the P2P route observation is not evidence that
the HTTP write path is down, so it does not demote. -/
def combinedLegs : Leg → Leg → Option Bool
  | .ready, .ready => some true
  | .drifted, _ => some false
  | _, .drifted => some false
  | _, _ => none

theorem unavailable_preserves_prior_readiness (other : Leg) (h : other ≠ .drifted) :
    combinedLegs .unavailable other = none ∧ combinedLegs other .unavailable = none := by
  cases other <;> simp_all [combinedLegs]

/-- Whatever the legs report, the reconcile does not change chat readiness. -/
theorem reconcile_never_gates_writes (outbound inbound : Leg) (route : Option Route) :
    chatReadiness (step (.reconciled (combinedLegs outbound inbound)) route) =
      chatReadiness route :=
  endpoint_neutral_writers_preserve_readiness _ _ rfl

/-! Conformance inputs. -/

def allSources : List Source := [.localStandard, .enrollment, .other]
def allLegs : List Leg := [.ready, .drifted, .unavailable]
def bools : List Bool := [true, false]

/-- The observation instant the generated cases use; expiries straddle it and
include the strict boundary `instant = caseNow`. -/
def caseNow : Nat := 7

def caseLeases : List Lease :=
  [.absent, .malformed, .expiresAt 6, .expiresAt 7, .expiresAt 8]

def fenceRoutes : List (Option Route) :=
  none :: (allSources.flatMap fun source =>
    bools.flatMap fun endpoint => bools.flatMap fun managed =>
      bools.flatMap fun pairing => bools.flatMap fun network =>
        bools.flatMap fun digest => bools.flatMap fun sequence =>
          caseLeases.map fun lease =>
            some ⟨source, endpoint, managed, pairing, network, digest, sequence, lease⟩)

/-- Transition cases use complete, fresh generations so the native enrollment
writer accepts the rotated generation. -/
def transitionRoutes : List (Option Route) :=
  none :: (allSources.flatMap fun source =>
    bools.flatMap fun endpoint => bools.flatMap fun managed =>
      bools.map fun pairing =>
        some ⟨source, endpoint, managed, pairing, true, true, true, .expiresAt 8⟩)

def transitionEvents : List Event :=
  [.restart, .authorityFailure, .reconciled none, .reconciled (some true),
    .reconciled (some false), .generationRotation]

end ClientChatReadiness
