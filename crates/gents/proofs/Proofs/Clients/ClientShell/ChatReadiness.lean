namespace ClientChatReadiness

/-- A route records only an endpoint and its selected runtime binding. It is
never a cached ACP grant, transport peer or enrollment lease. -/
structure Route where
  operatorEndpoint : Bool
  bindingMatches : Bool
  deriving DecidableEq, Repr

inductive Cause where
  | noRoute | noEndpoint
  deriving DecidableEq, Repr

inductive Readiness where
  | ready | notReady (cause : Cause)
  deriving DecidableEq, Repr

def chatReadiness : Option Route → Readiness
  | none => .notReady .noRoute
  | some route => if route.operatorEndpoint && route.bindingMatches then .ready else .notReady .noEndpoint

/-- Route readiness permits an attempt; native ACP still decides admission. -/
theorem ready_iff_bound_endpoint (route : Route) :
    chatReadiness (some route) = .ready ↔ route.operatorEndpoint && route.bindingMatches = true := by
  cases route with | mk endpoint binding => cases endpoint <;> cases binding <;> decide

inductive NextAction where
  | connectRuntime
  deriving DecidableEq, Repr

def nextAction (_cause : Cause) : NextAction := .connectRuntime

def causeToken : Cause → String
  | .noRoute => "no_route" | .noEndpoint => "no_endpoint"

def nextActionToken : NextAction → String
  | .connectRuntime => "connect_runtime"

def allCauses : List Cause := [.noRoute, .noEndpoint]

theorem allCauses_complete (cause : Cause) : cause ∈ allCauses := by cases cause <;> simp [allCauses]

def fenceRoutes : List (Option Route) := none ::
  [false, true].flatMap fun operatorEndpoint =>
    [false, true].map fun bindingMatches => some ⟨operatorEndpoint, bindingMatches⟩

end ClientChatReadiness
