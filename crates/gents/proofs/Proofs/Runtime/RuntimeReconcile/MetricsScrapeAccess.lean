namespace RuntimeReconcile.MetricsScrapeAccess

/-- The address class the operator gives the dedicated metrics listener. The
listener is separate from the runtime API, so binding it never changes who may
reach the operator routes modeled by `RuntimeReconcile.HttpAccess`. -/
inductive Bind where
  | loopback
  | interface
  | unspecified
  deriving DecidableEq, Repr

inductive Decision where
  | misdirected
  | unauthorized
  | metrics
  deriving DecidableEq, Repr

/-- A wildcard bind would expose the exposition on every interface, including
ones the operator did not pick, so it is refused even with a token. A specific
non-loopback interface (a tailnet or LAN address) needs a bearer token because
any host that can route to it can otherwise read node, agent and peer
identifiers from the exposition. Loopback is reachable only by local processes,
the same audience that can already read the home directory. -/
def bindAllowed (bind : Bind) (tokenConfigured : Bool) : Bool :=
  match bind with
  | .unspecified => false
  | .loopback => true
  | .interface => tokenConfigured

/-- The listener serves only the counters exposition, a fixed read made as the
node's principal that takes no caller input. Host matching defends against DNS
rebinding as on the runtime API; when a token is configured, a request that
does not present it is refused before any read. -/
def decide (authorityMatches tokenConfigured tokenPresented : Bool) : Decision :=
  if !authorityMatches then .misdirected
  else if tokenConfigured && !tokenPresented then .unauthorized
  else .metrics

theorem unauthenticated_listener_is_loopback (bind : Bind)
    (h : bindAllowed bind false = true) : bind = .loopback := by
  cases bind <;> simp_all [bindAllowed]

theorem wildcard_bind_is_refused (tokenConfigured : Bool) :
    bindAllowed .unspecified tokenConfigured = false := by
  simp [bindAllowed]

theorem configured_token_is_required (authorityMatches : Bool) :
    decide authorityMatches true false ≠ .metrics := by
  cases authorityMatches <;> simp [decide]

theorem foreign_authority_never_scrapes (tokenConfigured tokenPresented : Bool) :
    decide false tokenConfigured tokenPresented = .misdirected := by
  simp [decide]

end RuntimeReconcile.MetricsScrapeAccess
