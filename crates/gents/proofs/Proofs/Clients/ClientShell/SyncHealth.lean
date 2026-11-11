import Proofs.Clients.ClientShell.ObservationOrdering
import Mathlib.Tactic.SplitIfs

/-!
Client sync health: the one product state the desktop shows for its sync.

External premises (#3538, discovery-only P2P):
* The desktop reads and writes its runtime only over authenticated HTTP as the
  User. Whether that runtime is reachable and readable is the runtime
  connection's state (`RuntimeConnectionObservation`; TLA
  `RuntimeConnectionTransport`), one per configured runtime.
* P2P carries only discovery and enrollment. Nothing replicates between the
  desktop and its runtime, so DefraDB's DAG, push-retry and quarantine
  counters, the replicator count and replicated-schema skew describe no path
  the desktop uses.

Decision (user, W10): sync health is derived from three facts and nothing
else: the runtime connections, the enrollment activation failures
(`EnrollmentActivationObservation`) and P2P discovery (the transport probe and
the configured peers' dial results). `Facts` has no replication field, so no
product state can be derived from replication (`project_reads_only_facts`).
Every state other than healthy names the cause the facts witness and the
action the user takes next (`unhealthy_view_names_cause_and_action`).
-/

namespace ClientSyncHealth

open RuntimeConnectionObservation (Status)
open EnrollmentActivationObservation (Failure)

/-- The P2P transport probe: `degraded` failed fewer than the wedge threshold
consecutive times, `wedged` at least that many. -/
inductive Transport where
  | healthy | degraded | wedged
  deriving DecidableEq, Repr

/-- Discovery: the transport probe and how many configured peers dialed. -/
structure Discovery where
  transport : Transport
  configuredPeers : Nat
  dialedPeers : Nat
  deriving DecidableEq, Repr

/-- The three facts. `runtimes` holds one connection status per configured
runtime in directory order (`none` before the observer reconciles it);
`enrollment` holds one activation failure per current enrollment authority. -/
structure Facts where
  runtimes : List (Option Status)
  enrollment : List Failure
  discovery : Discovery
  deriving DecidableEq, Repr

inductive Layer where
  | runtime | enrollment | discovery
  deriving DecidableEq, Repr

inductive Cause where
  | runtimeConnecting | runtimeDisconnected | runtimeRefused | runtimeNoEndpoint
  | enrollmentAuthentication | enrollmentRoute
  | discoveryDegraded | discoveryOffline
  deriving DecidableEq, Repr

/-- `waiting` progresses without the user; `blocked` needs the user's action. -/
inductive Tone where
  | waiting | blocked
  deriving DecidableEq, Repr

/-- `wait`: the client retries by itself (the runtime source reconnects, the
supervisor re-dials and re-applies enrollment routes). `restoreAccess`: only
the runtime's owner can accept this device again. `connectRuntime`: no
operator endpoint exists to read (`ClientChatReadiness.noEndpoint`).
`enrollAgain`: the enrolled server could not be authenticated, so a fresh
enrollment is the only remedy. `checkNetwork`: no configured peer can be
dialed. -/
inductive NextAction where
  | wait | restoreAccess | connectRuntime | enrollAgain | checkNetwork
  deriving DecidableEq, Repr

def Cause.layer : Cause → Layer
  | .runtimeConnecting | .runtimeDisconnected | .runtimeRefused | .runtimeNoEndpoint => .runtime
  | .enrollmentAuthentication | .enrollmentRoute => .enrollment
  | .discoveryDegraded | .discoveryOffline => .discovery

def Cause.nextAction : Cause → NextAction
  | .runtimeConnecting | .runtimeDisconnected | .enrollmentRoute | .discoveryDegraded => .wait
  | .runtimeRefused => .restoreAccess
  | .runtimeNoEndpoint => .connectRuntime
  | .enrollmentAuthentication => .enrollAgain
  | .discoveryOffline => .checkNetwork

def Cause.tone (cause : Cause) : Tone :=
  if cause.nextAction = .wait then .waiting else .blocked

def runtimeCause : Option Status → Option Cause
  | none | some .connecting => some .runtimeConnecting
  | some .connected => none
  | some .disconnected => some .runtimeDisconnected
  | some .refused => some .runtimeRefused
  | some .noEndpoint => some .runtimeNoEndpoint

def enrollmentCause : Failure → Cause
  | .authentication => .enrollmentAuthentication
  | .route => .enrollmentRoute

/-- A configured peer that never dialed is offline; with no configured peer a
healthy transport is reachable, and a degraded one counts only dialed peers. -/
def discoveryCause (discovery : Discovery) : Option Cause :=
  match discovery.transport with
  | .wedged => some .discoveryOffline
  | .degraded =>
    if discovery.dialedPeers = 0 then some .discoveryOffline else some .discoveryDegraded
  | .healthy =>
    if discovery.dialedPeers = 0 ∧ 0 < discovery.configuredPeers then some .discoveryOffline
    else none

/-- Every cause the facts witness, in layer order. -/
def causes (facts : Facts) : List Cause :=
  facts.runtimes.filterMap runtimeCause ++ facts.enrollment.map enrollmentCause ++
    (discoveryCause facts.discovery).toList

/-- The shown cause: the first one needing the user, else the first one. -/
def project (facts : Facts) : Option Cause :=
  match (causes facts).find? (fun cause => cause.tone == .blocked) with
  | some cause => some cause
  | none => (causes facts).head?

inductive State where
  | healthy | waiting | blocked
  deriving DecidableEq, Repr

structure View where
  state : State
  cause : Option Cause
  nextAction : Option NextAction
  deriving DecidableEq, Repr

def Tone.state : Tone → State
  | .waiting => .waiting
  | .blocked => .blocked

def view (facts : Facts) : View :=
  match project facts with
  | none => ⟨.healthy, none, none⟩
  | some cause => ⟨cause.tone.state, some cause, some cause.nextAction⟩

/-! Every product state is derived from the three facts. -/

theorem runtimeCause_layer {status : Option Status} {cause : Cause}
    (h : runtimeCause status = some cause) : cause.layer = .runtime := by
  rcases status with _ | status
  · simp [runtimeCause] at h; subst h; rfl
  · cases status <;> simp [runtimeCause] at h <;> subst h <;> rfl

theorem enrollmentCause_layer (failure : Failure) :
    (enrollmentCause failure).layer = .enrollment := by
  cases failure <;> rfl

theorem discoveryCause_layer {discovery : Discovery} {cause : Cause}
    (h : discoveryCause discovery = some cause) : cause.layer = .discovery := by
  rcases discovery with ⟨transport, configured, dialed⟩
  cases transport <;> simp only [discoveryCause] at h <;>
    (try split_ifs at h) <;> simp at h <;> subst h <;> rfl

/-- Each cause is witnessed by the fact of its own layer. -/
theorem cause_witnessed (facts : Facts) (cause : Cause) (h : cause ∈ causes facts) :
    (cause.layer = .runtime ∧ ∃ status ∈ facts.runtimes, runtimeCause status = some cause) ∨
    (cause.layer = .enrollment ∧ ∃ failure ∈ facts.enrollment, enrollmentCause failure = cause) ∨
    (cause.layer = .discovery ∧ discoveryCause facts.discovery = some cause) := by
  simp only [causes, List.mem_append, List.mem_filterMap, List.mem_map, Option.mem_toList] at h
  rcases h with (⟨status, mem, eq⟩ | ⟨failure, mem, eq⟩) | eq
  · exact Or.inl ⟨runtimeCause_layer eq, status, mem, eq⟩
  · subst eq
    exact Or.inr (Or.inl ⟨enrollmentCause_layer failure, failure, mem, rfl⟩)
  · exact Or.inr (Or.inr ⟨discoveryCause_layer eq, eq⟩)

theorem project_mem_causes {facts : Facts} {cause : Cause}
    (h : project facts = some cause) : cause ∈ causes facts := by
  unfold project at h
  split at h
  · rename_i found
    simp at h; subst h
    exact List.mem_of_find?_eq_some found
  · exact List.mem_of_mem_head? h

/-- The shown state is derived from the facts of its cause's layer. -/
theorem project_derived_from_facts {facts : Facts} {cause : Cause}
    (h : project facts = some cause) :
    (cause.layer = .runtime ∧ ∃ status ∈ facts.runtimes, runtimeCause status = some cause) ∨
    (cause.layer = .enrollment ∧ ∃ failure ∈ facts.enrollment, enrollmentCause failure = cause) ∨
    (cause.layer = .discovery ∧ discoveryCause facts.discovery = some cause) :=
  cause_witnessed facts cause (project_mem_causes h)

/-- No other input reaches the projection: facts that agree on the runtime
connections, the enrollment failures and discovery show the same state. -/
theorem project_reads_only_facts (left right : Facts)
    (runtimes : left.runtimes = right.runtimes)
    (enrollment : left.enrollment = right.enrollment)
    (discovery : left.discovery = right.discovery) :
    view left = view right := by
  rcases left with ⟨r, e, d⟩
  rcases right with ⟨r', e', d'⟩
  simp only at runtimes enrollment discovery
  subst runtimes enrollment discovery
  rfl

/-- There is no replication-derived state: every cause belongs to one of the
three layers. -/
theorem cause_layer_exhaustive (cause : Cause) :
    cause.layer = .runtime ∨ cause.layer = .enrollment ∨ cause.layer = .discovery := by
  cases cause <;> simp [Cause.layer]

private theorem toList_eq_nil {α : Type} (value : Option α) :
    value.toList = [] ↔ value = none := by
  cases value <;> simp

theorem healthy_iff_no_cause (facts : Facts) :
    project facts = none ↔ causes facts = [] := by
  unfold project
  constructor
  · intro h
    split at h
    · simp at h
    · simpa using h
  · intro h
    simp [h]

/-- Healthy is exactly: every configured runtime connected, no enrollment
activation failure, and discovery reachable. -/
theorem healthy_iff (facts : Facts) :
    project facts = none ↔
      (∀ status ∈ facts.runtimes, status = some .connected) ∧
      facts.enrollment = [] ∧ discoveryCause facts.discovery = none := by
  rw [healthy_iff_no_cause]
  simp only [causes, List.append_eq_nil_iff, List.filterMap_eq_nil_iff, List.map_eq_nil_iff,
    toList_eq_nil]
  constructor
  · rintro ⟨⟨runtimes, enrollment⟩, discovery⟩
    refine ⟨fun status mem => ?_, enrollment, discovery⟩
    have := runtimes status mem
    rcases status with _ | status
    · simp [runtimeCause] at this
    · cases status <;> simp [runtimeCause] at this ⊢
  · rintro ⟨runtimes, enrollment, discovery⟩
    refine ⟨⟨fun status mem => ?_, enrollment⟩, discovery⟩
    rw [runtimes status mem]
    rfl

/-- A cause that needs the user is never hidden behind one that does not. -/
theorem blocked_dominates (facts : Facts) (cause : Cause)
    (mem : cause ∈ causes facts) (blocked : cause.tone = .blocked) :
    ∃ shown, project facts = some shown ∧ shown.tone = .blocked := by
  unfold project
  split
  · rename_i shown found
    exact ⟨shown, rfl, by simpa using List.find?_some found⟩
  · rename_i missing
    have := List.find?_eq_none.mp missing cause mem
    simp [blocked] at this

/-! Every non-healthy state names its cause and next action. -/

theorem unhealthy_view_names_cause_and_action (facts : Facts)
    (h : (view facts).state ≠ .healthy) :
    ∃ cause, (view facts).cause = some cause ∧
      (view facts).nextAction = some cause.nextAction ∧
      (cause ∈ causes facts) := by
  unfold view at h ⊢
  split
  · rename_i none_case
    simp [none_case] at h
  · rename_i cause some_case
    exact ⟨cause, rfl, rfl, project_mem_causes some_case⟩

theorem healthy_view_has_no_cause (facts : Facts) (h : (view facts).state = .healthy) :
    (view facts).cause = none ∧ (view facts).nextAction = none := by
  unfold view at h ⊢
  cases hp : project facts with
  | none => simp
  | some cause =>
    simp only [hp] at h
    cases hc : cause.tone <;> simp [hc, Tone.state] at h

/-- A waiting cause asks nothing of the user; a blocked one names an action. -/
theorem waiting_iff_wait (cause : Cause) :
    cause.tone = .waiting ↔ cause.nextAction = .wait := by
  cases cause <;> simp [Cause.tone, Cause.nextAction]

/-! Conformance inputs. -/

def allStatuses : List (Option Status) :=
  [none, some .connecting, some .connected, some .disconnected, some .refused, some .noEndpoint]

def runtimeInputs : List (List (Option Status)) :=
  [] :: (allStatuses.map fun status => [status]) ++
    [[some .connected, some .refused], [some .disconnected, some .connected],
     [some .connecting, some .noEndpoint]]

def enrollmentInputs : List (List Failure) :=
  [[], [.authentication], [.route], [.route, .authentication]]

def discoveryInputs : List Discovery :=
  [.healthy, .degraded, .wedged].flatMap fun transport =>
    [(0, 0), (1, 0), (1, 1), (2, 1)].map fun (configured, dialed) =>
      ⟨transport, configured, dialed⟩

def allFacts : List Facts :=
  runtimeInputs.flatMap fun runtimes => enrollmentInputs.flatMap fun enrollment =>
    discoveryInputs.map fun discovery => ⟨runtimes, enrollment, discovery⟩

end ClientSyncHealth
