/-!
Desktop chat write readiness: whether a chat send (submit or retry) is
attempted against a saved deployment route, or refused before any request is
prepared.

External premises (#3538, discovery-only P2P):
* The desktop writes to its runtime only over the authenticated operator HTTP
  endpoint (`/api/v0/graphql`), signed by the User client key, with `client`
  request admission. There is no replica fallback: without an operator
  endpoint no write path exists.
* The runtime authorizes that write itself, through native DefraDB ACP/NAC on
  the signing DID; a revoked device is refused there.

Decision (user, #3538): an authenticated endpoint means ready. Pairing and the
enrollment lease are P2P route observations and do not gate chat writes: the
route fields this contract reads are the route's provenance, whether it holds
an operator endpoint and whether it is a managed local runtime, and nothing
else. Connection progress and its UX belong to the frontend's runtime
connection machinery (`RuntimeConnectionObservation`), not to this fence.
A send while the runtime is unreachable reaches the endpoint and fails; the
optimistic send (`ClientOptimisticSend`) shows it failed with Retry.
-/

namespace ClientChatReadiness

/-- Saved route provenance. `other` covers manual and source-less records. -/
inductive Source where
  | localStandard
  | enrollment
  | other
  deriving DecidableEq, Repr

/-- One saved deployment route as the desktop's peer directory holds it.
`managed` is `PeerRecord::is_managed_runtime` (a local runtime home and an
HTTP(S) GraphQL URL). -/
structure Route where
  source : Source
  operatorEndpoint : Bool
  managed : Bool
  deriving DecidableEq, Repr

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

theorem endpoint_opens_writes (route : Route) (endpoint : route.operatorEndpoint = true) :
    chatReadiness (some route) = .ready := by
  simp [chatReadiness, endpoint]

/-- A route without an endpoint has no write path and is refused. -/
theorem endpointless_route_is_refused (route : Route) (endpoint : route.operatorEndpoint = false) :
    chatReadiness (some route) ≠ .ready := by
  rcases route with ⟨source, operator, managed⟩
  simp only at endpoint
  subst endpoint
  cases source <;> cases managed <;> simp [chatReadiness]

theorem ready_iff_endpoint (route : Option Route) :
    chatReadiness route = .ready ↔ ∃ r, route = some r ∧ r.operatorEndpoint = true := by
  cases route with
  | none => simp [chatReadiness]
  | some r =>
    rcases r with ⟨source, operator, managed⟩
    cases operator <;> cases source <;> cases managed <;> simp [chatReadiness]

/-! Refusals and their next action. -/

/-- The writer that stores a route's GraphQL URL (local discovery, an
enrollment advertisement, a manual connect), or a new manual route where none
existed. `operator` is whether that URL is an operator endpoint. -/
inductive Event where
  | endpointWritten (operator : Bool)
  | addDeployment (operator : Bool)
  deriving DecidableEq, Repr

def step : Event → Option Route → Option Route
  | .addDeployment operator, none => some ⟨.other, operator, false⟩
  | .endpointWritten _, none => none
  | .addDeployment _, some route => some route
  | .endpointWritten operator, some route => some { route with operatorEndpoint := operator }

/-- The next action a refusal tells the user to take. -/
inductive NextAction where
  | connectRuntime
  | awaitEndpointAdvertisement
  deriving DecidableEq, Repr

def nextAction : Cause → NextAction
  | .noRoute | .noEndpoint => .connectRuntime
  | .endpointNotAdvertised => .awaitEndpointAdvertisement

/-- The writer each next action leads to. -/
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
    rcases route with ⟨source, operator, managed⟩
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

/-! Conformance inputs. -/

def allSources : List Source := [.localStandard, .enrollment, .other]
def bools : List Bool := [true, false]

def fenceRoutes : List (Option Route) :=
  none :: (allSources.flatMap fun source =>
    bools.flatMap fun endpoint => bools.map fun managed => some ⟨source, endpoint, managed⟩)

end ClientChatReadiness
