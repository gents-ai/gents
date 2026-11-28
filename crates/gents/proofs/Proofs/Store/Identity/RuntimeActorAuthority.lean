import Proofs.Store.Identity.Household

namespace Identity.RuntimeActorAuthority

open Identity.Household

/-- Runtime code paths classified by the authority they must act under.

Decision: runtime work divides into two disjoint authority classes:
- `nodeOperational` and `runtimeConfigUse` act as the hosting runtime's node
  key. Node-operational paths operate only on node-owned operational state
  (`NodeRuntime`, `NodeReadiness`, `OwedDelivery`, deferred operations, P2P
  enrollment and pairing, signed-command nonces). Runtime configuration-use
  paths read Node or configuration documents through `runtimeMayUse` and never
  read private User sessions or requests.
- `sessionResume`, `sessionSelection`, `requestWatch`, and `wakeInputLoad`
  operate on private sessions, session messages, or requests and must act as
  the selected Agent, never as the hosting runtime's node key and never without
  an actor.

External premise: even though `NodeProvisioning.runtimeMayStart` refuses a
store whose Node holds `bypass-dac` at startup, an administrator can grant
`bypass-dac` while running, and `Household.readObserved` lets a `bypass-dac`
reader observe past document ACP. Enforcing this authority split at the runtime
entry points guarantees that private session and request paths never rely on
document ACP to exclude the node key. -/
inductive RuntimePath where
  | nodeOperational
  | runtimeConfigUse
  | sessionResume
  | sessionSelection
  | requestWatch
  | wakeInputLoad
  deriving BEq, DecidableEq, Repr

/-- The authority class a runtime path requires. -/
inductive RequiredAuthority where
  | nodeKey
  | selectedAgent
  deriving BEq, DecidableEq, Repr

/-- The principal presented at a runtime entry point. -/
inductive ActingPrincipal where
  | nodeKey
  | selectedAgent
  | otherActor
  | absent
  deriving BEq, DecidableEq, Repr

def RuntimePath.selectedAgentPaths : List RuntimePath :=
  [.sessionResume, .sessionSelection, .requestWatch, .wakeInputLoad]

def RuntimePath.wire : RuntimePath → String
  | .nodeOperational => "node_operational"
  | .runtimeConfigUse => "runtime_config_use"
  | .sessionResume => "session_resume"
  | .sessionSelection => "session_selection"
  | .requestWatch => "request_watch"
  | .wakeInputLoad => "wake_input_load"

def requiredAuthority : RuntimePath → RequiredAuthority
  | .nodeOperational | .runtimeConfigUse => .nodeKey
  | .sessionResume | .sessionSelection | .requestWatch | .wakeInputLoad => .selectedAgent

/-- Whether `path` admits `actor` before executing any store read or write.
Node-key paths admit only `.nodeKey`. Selected-Agent paths refuse `.nodeKey`
and `.absent` unconditionally; any other concrete actor (the selected Agent or
another non-node-key actor such as a sibling Agent) is admitted to the store,
where document ACP without `bypass-dac` decides visibility
(`without_bypass_document_acp_decides`). -/
def actorAdmitted (path : RuntimePath) (actor : ActingPrincipal) : Bool :=
  match requiredAuthority path, actor with
  | .nodeKey, .nodeKey => true
  | .nodeKey, _ => false
  | .selectedAgent, .selectedAgent => true
  | .selectedAgent, .otherActor => true
  | .selectedAgent, .nodeKey => false
  | .selectedAgent, .absent => false

/-- A User-owned Node granting `runtime_reader` to the hosting runtime, two
User-owned Agents (`selected` and `sibling`), and a User-owned private session
and request whose `working_agent` is `selected`. -/
def privateSessionFixture : State :=
  let base : State :=
    { objects := [⟨"nodes", "node", "user"⟩]
    , relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩]
    , enrolled := [] }
  let withSelected := commitAgentCreation base "selected-agent" "user" "selected" "node" "runtime" true
  let withBoth := commitAgentCreation withSelected "sibling-agent" "user" "sibling" "node" "runtime" true
  let withSession := commitUserSession withBoth "private-session" "user" "selected" "selected-agent" true
  { withSession with
    objects := ⟨"delivered_requests", "private-request", "user"⟩ :: withSession.objects
    relations := ⟨"delivered_requests", "private-request", "parent", .object "agents" "selected-agent"⟩ ::
      ⟨"delivered_requests", "private-request", "session", .object "sessions" "private-session"⟩ ::
      ⟨"delivered_requests", "private-request", "working_agent", .actor "selected"⟩ ::
      withSession.relations }

def targetDocument : RuntimePath → String × String
  | .nodeOperational | .runtimeConfigUse => ("nodes", "node")
  | .sessionResume | .sessionSelection => ("sessions", "private-session")
  | .requestWatch | .wakeInputLoad => ("delivered_requests", "private-request")

def principalDid : ActingPrincipal → String
  | .nodeKey => "runtime"
  | .selectedAgent => "selected"
  | .otherActor => "sibling"
  | .absent => ""

/-- Whether `path` exposes its target private document to `actor`, composing
the runtime authority gate `actorAdmitted` with `Household.nativeRead`. Only
`.nodeKey` can carry `runtimeBypass`; managed Agents and unrelated actors never
carry a DAC bypass (`Household.readObserved`). -/
def privateSessionObserved (path : RuntimePath) (actor : ActingPrincipal)
    (runtimeBypass : Bool) : Bool :=
  let bypass := match actor with
    | .nodeKey => runtimeBypass
    | _ => false
  let doc := targetDocument path
  actorAdmitted path actor &&
    nativeRead bypass 8 privateSessionFixture (principalDid actor) doc.1 doc.2

/-- No selected-Agent path ever admits the hosting runtime's node key. -/
theorem selected_agent_paths_never_admit_node_key (path : RuntimePath)
    (h : requiredAuthority path = .selectedAgent) :
    actorAdmitted path .nodeKey = false := by
  simp [actorAdmitted, h]

/-- No selected-Agent path ever admits an absent actor. -/
theorem selected_agent_paths_never_admit_absent_actor (path : RuntimePath)
    (h : requiredAuthority path = .selectedAgent) :
    actorAdmitted path .absent = false := by
  simp [actorAdmitted, h]

/-- A node-key path admits the node key and nothing else. -/
theorem node_key_paths_admit_only_node_key (path : RuntimePath) (actor : ActingPrincipal)
    (h : requiredAuthority path = .nodeKey) :
    actorAdmitted path actor = true ↔ actor = .nodeKey := by
  cases actor <;> simp [actorAdmitted, h]

/-- Document ACP grants read of the User's private session and request only to
the selected Agent, never to the hosting runtime's node key (even with
`runtime_reader` on the Node) and never to a sibling Agent. -/
theorem runtime_node_key_has_no_document_acp_read_of_private_session :
    allowed 8 privateSessionFixture "runtime" "sessions" "private-session" "read" = false ∧
    allowed 8 privateSessionFixture "runtime" "delivered_requests" "private-request" "read" = false ∧
    allowed 8 privateSessionFixture "sibling" "sessions" "private-session" "read" = false ∧
    allowed 8 privateSessionFixture "sibling" "delivered_requests" "private-request" "read" = false ∧
    allowed 8 privateSessionFixture "selected" "sessions" "private-session" "read" = true ∧
    allowed 8 privateSessionFixture "selected" "delivered_requests" "private-request" "read" = true := by
  native_decide

/-- Even when the hosting runtime's node key reads past document ACP
(`runtimeBypass = true`), no selected-Agent path exposes the User's private
session or request to it. -/
theorem private_session_path_never_exposes_to_node_key_even_with_bypass
    (path : RuntimePath) (h : requiredAuthority path = .selectedAgent)
    (runtimeBypass : Bool) :
    privateSessionObserved path .nodeKey runtimeBypass = false := by
  simp [privateSessionObserved, selected_agent_paths_never_admit_node_key path h]

/-- Neither a sibling Agent nor an absent actor observes the User's private
session or request on any selected-Agent path. -/
theorem private_session_path_never_exposes_to_sibling_or_absent
    (path : RuntimePath) (h : requiredAuthority path = .selectedAgent)
    (runtimeBypass : Bool) :
    privateSessionObserved path .otherActor runtimeBypass = false ∧
    privateSessionObserved path .absent runtimeBypass = false := by
  cases runtimeBypass <;> cases path <;> simp [requiredAuthority] at h <;>
    exact ⟨by native_decide, by simp [privateSessionObserved, actorAdmitted, requiredAuthority]⟩

/-- On every selected-Agent path, the User's private session or request is
observed if and only if the acting principal is the selected Agent, regardless
of whether the runtime node key holds `bypass-dac`. -/
theorem private_session_path_exposes_only_to_selected_agent
    (path : RuntimePath) (h : requiredAuthority path = .selectedAgent)
    (actor : ActingPrincipal) (runtimeBypass : Bool) :
    privateSessionObserved path actor runtimeBypass = true ↔ actor = .selectedAgent := by
  cases actor
  · simp [private_session_path_never_exposes_to_node_key_even_with_bypass path h runtimeBypass]
  · cases runtimeBypass <;> cases path <;> simp [requiredAuthority] at h <;> native_decide
  · simp [(private_session_path_never_exposes_to_sibling_or_absent path h runtimeBypass).1]
  · simp [(private_session_path_never_exposes_to_sibling_or_absent path h runtimeBypass).2]

/-- The node key's DAC bypass never changes what any selected-Agent path
observes for any actor. -/
theorem bypass_cannot_change_private_session_observation
    (path : RuntimePath) (h : requiredAuthority path = .selectedAgent)
    (actor : ActingPrincipal) :
    privateSessionObserved path actor true = privateSessionObserved path actor false := by
  cases actor
  · simp [private_session_path_never_exposes_to_node_key_even_with_bypass path h]
  · rfl
  · rfl
  · rfl

end Identity.RuntimeActorAuthority
