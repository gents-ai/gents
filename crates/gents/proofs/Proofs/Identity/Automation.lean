import Proofs.Identity.Household

namespace Identity.Automation

/-- Enabled configuration and native `invoke` are independent preconditions
for a new unattended admission. Native unavailability cannot prove authority.
An already admitted attempt is outside this fresh-admission decision. -/
inductive InvokeObservation where
  | allowed | denied | unavailable
  deriving DecidableEq, Repr

def unattendedAgentAdmission (actorMatches enabled : Bool)
    (invoke : InvokeObservation) : Bool :=
  actorMatches && enabled && decide (invoke = .allowed)

theorem disabled_agent_cannot_admit (actorMatches : Bool) (invoke : InvokeObservation) :
    unattendedAgentAdmission actorMatches false invoke = false := by
  simp [unattendedAgentAdmission]

theorem unavailable_invoke_cannot_admit (actorMatches enabled : Bool) :
    unattendedAgentAdmission actorMatches enabled .unavailable = false := by
  simp [unattendedAgentAdmission]

theorem denied_invoke_cannot_admit (actorMatches enabled : Bool) :
    unattendedAgentAdmission actorMatches enabled .denied = false := by
  simp [unattendedAgentAdmission]

/-- Schedule is cadence configuration; unlike Trigger it contains no runtime
observation fields. Its creator stages the physical Node parent with the create.
Aborted publication exposes neither the document nor its relationship. -/
def publishSchedule (state : Household.State) (creator schedule node : String)
    (commit : Bool) : Household.State :=
  if commit then
    Household.apply (Household.directCreate state creator "configuration" schedule true true)
      (.grantAs creator ⟨"configuration", schedule, "parent", .object "nodes" node⟩)
  else state

theorem aborted_schedule_publication_is_unchanged (state : Household.State)
    (creator schedule node : String) :
    publishSchedule state creator schedule node false = state := by rfl

/-- The scheduler consumes cadence as the Node runtime. Before each fresh use
of a cached schedule it checks native read visibility of that exact physical
document under the runtime DID. A revocation after an admitted fire cannot
recall that fire; the native read and subsequent work are not one transaction. -/
def scheduleVisible (state : Household.State) (held runtime schedule : String) : Bool :=
  held == runtime && Household.allowed 5 state runtime "configuration" schedule "read"

theorem a_foreign_runtime_identity_cannot_be_borrowed (state : Household.State)
    (held runtime schedule : String) (different : held ≠ runtime) :
    scheduleVisible state held runtime schedule = false := by
  simp [scheduleVisible, different]

def scheduleFixture : Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩], enrolled := [] }

theorem published_schedule_inherits_only_native_node_access :
    let published := publishSchedule scheduleFixture "user" "schedule" "node" true
    [scheduleVisible published "runtime" "runtime" "schedule",
     Household.allowed 5 published "user" "configuration" "schedule" "update",
     Household.allowed 5 published "runtime" "configuration" "schedule" "update",
     Household.allowed 5 published "agent" "configuration" "schedule" "read"] =
      [true, true, false, false] := by native_decide

theorem removing_schedule_parent_revokes_runtime_visibility :
    let published := publishSchedule scheduleFixture "user" "schedule" "node" true
    let revoked := Household.apply published
      (.revoke ⟨"configuration", "schedule", "parent", .object "nodes" "node"⟩)
    scheduleVisible revoked "runtime" "runtime" "schedule" = false := by native_decide

theorem agent_created_schedule_retains_creator_and_node_owner :
    let published := publishSchedule scheduleFixture "agent" "schedule" "node" true
    [Household.allowed 5 published "agent" "configuration" "schedule" "update",
     Household.allowed 5 published "user" "configuration" "schedule" "delete",
     scheduleVisible published "runtime" "runtime" "schedule"] = [true, true, true] := by native_decide

/-- A Task delivery creates its session and publishes both native session
relationships in the same transaction as its request and fire receipt. An
aborted transaction exposes none of them. A duplicate never regrants an
existing session, so an explicit revocation remains effective. -/
structure TaskSessionPublication where
  request : Bool
  session : Bool
  parent : Bool
  workingAgent : Bool
  deriving DecidableEq, Repr

def publishTaskSession (before : TaskSessionPublication)
    (fresh newSession committed : Bool) : TaskSessionPublication :=
  if fresh && committed then
    if newSession then ⟨true, true, true, true⟩
    else { before with request := true }
  else before

theorem task_session_abort (before : TaskSessionPublication) (fresh newSession : Bool) :
    publishTaskSession before fresh newSession false = before := by
  cases fresh <;> cases newSession <;> rfl

theorem task_session_duplicate_preserves_revocation
    (before : TaskSessionPublication) (newSession committed : Bool) :
    publishTaskSession before false newSession committed = before := by rfl

theorem task_session_fresh_commit :
    publishTaskSession ⟨false, false, false, false⟩ true true true =
      ⟨true, true, true, true⟩ := by rfl

/-- A fresh Task request and its physical Agent and session relationships
commit together. The caller must verify both targets and authenticate the
selected Agent before entering the native transaction. A failed transaction
leaves neither document nor grants. Duplicate deliveries are observed rather
than regranted, preserving later revocations. -/
def commitTaskRequest (state : Household.State)
    (requestDoc actor selectedAgent agentDoc sessionDoc : String)
    (targetsVerified fresh committed : Bool) : Household.State :=
  if actor == selectedAgent && targetsVerified && fresh && committed then
    { state with
      objects := ⟨"delivered_requests", requestDoc, actor⟩ :: state.objects
      relations :=
        ⟨"delivered_requests", requestDoc, "parent", .object "agents" agentDoc⟩ ::
        ⟨"delivered_requests", requestDoc, "session", .object "sessions" sessionDoc⟩ ::
        state.relations }
  else state

theorem task_request_aborted (state : Household.State) (request actor selected agent session : String)
    (verified fresh : Bool) :
    commitTaskRequest state request actor selected agent session verified fresh false = state := by
  simp [commitTaskRequest]

theorem task_request_duplicate_preserves_revocation (state : Household.State)
    (request actor selected agent session : String) (verified committed : Bool) :
    commitTaskRequest state request actor selected agent session verified false committed = state := by
  simp [commitTaskRequest]

theorem task_request_rejects_foreign_creator (state : Household.State)
    (request actor selected agent session : String) (verified fresh committed : Bool)
    (different : actor ≠ selected) :
    commitTaskRequest state request actor selected agent session verified fresh committed = state := by
  simp [commitTaskRequest, different]

theorem task_request_commits_exact_parents (state : Household.State)
    (request actor agent session : String) :
    (commitTaskRequest state request actor actor agent session true true true).relations =
      ⟨"delivered_requests", request, "parent", .object "agents" agent⟩ ::
      ⟨"delivered_requests", request, "session", .object "sessions" session⟩ ::
      state.relations := by simp [commitTaskRequest]

/-- A completed fire is an admitted arrival only when the checkpoint actor can
read its request. The node DID is not a substitute for the selected Agent's
authority; the other prefix and cursor conditions remain native prerequisites. -/
def checkpointTaskArrival (state : Household.State) (actor request : String)
    (receipt prefixComplete cursorWritable : Bool) (cursor through : Nat) : Nat :=
  if receipt && prefixComplete && cursorWritable &&
      Household.allowed 5 state actor "delivered_requests" request "read"
    then max cursor through else cursor

theorem invisible_task_cannot_advance (state : Household.State) (actor request : String)
    (receipt prefixComplete cursorWritable : Bool) (cursor through : Nat)
    (hidden : Household.allowed 5 state actor "delivered_requests" request "read" = false) :
    checkpointTaskArrival state actor request receipt prefixComplete cursorWritable cursor through = cursor := by
  simp [checkpointTaskArrival, hidden]

theorem visible_task_advances_complete_prefix (state : Household.State) (actor request : String)
    (cursor through : Nat)
    (visible : Household.allowed 5 state actor "delivered_requests" request "read" = true) :
    checkpointTaskArrival state actor request true true true cursor through = max cursor through := by
  simp [checkpointTaskArrival, visible]

theorem checkpoint_task_duplicate_is_idempotent (state : Household.State) (actor request : String)
    (cursor : Nat)
    (visible : Household.allowed 5 state actor "delivered_requests" request "read" = true) :
    checkpointTaskArrival state actor request true true true cursor cursor = cursor := by
  simp [checkpointTaskArrival, visible]

/-- Trigger combines configuration and runtime observations in one document.
ACP1 update authorizes the document, rather than individual fields. -/
structure Trigger where
  task : String
  enabled : Bool
  nextRun : Nat
  deriving DecidableEq, Repr

def updateTrigger (current replacement : Trigger) (nativeUpdate : Bool) : Trigger :=
  if nativeUpdate then replacement else current

theorem update_for_observation_also_permits_routing_change :
    updateTrigger ⟨"task-a", true, 0⟩ ⟨"task-b", false, 1⟩ true =
      ⟨"task-b", false, 1⟩ := by rfl

theorem denied_update_preserves_all_fields (current replacement : Trigger) :
    updateTrigger current replacement false = current := by rfl

/-- ACP1 commits a configuration document before detached reader grants.
Seeding an event cursor in the document transaction does not imply that an
agent has permission to read the configuration after that transaction. -/
structure Publication where
  committed : Bool
  cursorSeeded : Bool
  agentReader : Bool
  deriving DecidableEq, Repr

def publishConfig (eventSource : Bool) : Publication :=
  ⟨true, eventSource, false⟩

def grantReader (state : Publication) (succeeded : Bool) : Publication :=
  { state with agentReader := state.agentReader || succeeded }

theorem seeded_cursor_does_not_supply_agent_grant :
    publishConfig true = ⟨true, true, false⟩ := by rfl

theorem failed_grant_leaves_committed_config_unreadable :
    grantReader (publishConfig true) false = ⟨true, true, false⟩ := by rfl

/-- Replacing the selected agent in a configuration document does not itself
remove a previously granted native relationship or grant the new agent access. -/
structure Assignment where
  selected : String
  granted : String → Bool

def reassign (state : Assignment) (agent : String) : Assignment :=
  { state with selected := agent }

theorem reassignment_does_not_revoke (state : Assignment) (oldAgent newAgent : String) :
    (reassign state newAgent).granted oldAgent = state.granted oldAgent := by rfl

theorem reassignment_does_not_grant (state : Assignment) (newAgent : String) :
    (reassign state newAgent).granted newAgent = state.granted newAgent := by rfl

/-- Callback discovery and planner loading read configuration as the runtime.
The checkpoint reads the binding and callback as the selected Agent. A fresh
binding publication installs those two standing reader relations; it does
not regrant them during later replacement, so revocation remains effective.
The EventSource read is a separate configuration dependency. -/
def publishCallbackReaders (state : Household.State)
    (creator binding callback agent : String) (fresh : Bool) : Household.State :=
  if fresh then
    let withBinding := Household.apply state (.grantAs creator
      ⟨"configuration", binding, "reader", .actor agent⟩)
    Household.apply withBinding (.grantAs creator
      ⟨"configuration", callback, "reader", .actor agent⟩)
  else state

theorem callback_replacement_preserves_revocation (state : Household.State)
    (creator binding callback agent : String) :
    publishCallbackReaders state creator binding callback agent false = state := by rfl

def callbackConfigurationFixture : Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩,
      ⟨"configuration", "binding", "user"⟩,
      ⟨"configuration", "callback", "user"⟩,
      ⟨"configuration", "module", "user"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"configuration", "binding", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "callback", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "module", "parent", .object "nodes" "node"⟩], enrolled := [] }

theorem callback_configuration_grants_are_scoped :
    let state := publishCallbackReaders callbackConfigurationFixture "user" "binding" "callback" "agent" true
    [Household.allowed 5 state "runtime" "configuration" "binding" "read",
     Household.allowed 5 state "runtime" "configuration" "callback" "read",
     Household.allowed 5 state "runtime" "configuration" "module" "read",
     Household.allowed 5 state "agent" "configuration" "binding" "read",
     Household.allowed 5 state "agent" "configuration" "callback" "read",
     Household.allowed 5 state "agent" "configuration" "module" "read",
     Household.allowed 5 state "sibling" "configuration" "binding" "read",
     Household.allowed 5 state "runtime" "configuration" "callback" "update"] =
    [true, true, true, true, true, false, false, false] := by native_decide

/-- A pending callback claim reads its binding and callback as the selected
Agent and checks the retained configuration in the claim transaction. An
already claimed attempt continues without a new configuration check. Native
ACP dependencies must participate in transaction conflict validation for a
concurrent revocation to fence commit. -/
def callbackClaimAllowed (state : Household.State) (actor selected binding callback : String)
    (bindingCurrent callbackCurrent : Bool) : Bool :=
  actor == selected && bindingCurrent && callbackCurrent &&
    Household.allowed 5 state actor "configuration" binding "read" &&
    Household.allowed 5 state actor "configuration" callback "read"

theorem revoked_binding_blocks_new_callback_claim :
    let granted := publishCallbackReaders callbackConfigurationFixture "user" "binding" "callback" "agent" true
    let revoked := Household.apply granted
      (.revoke ⟨"configuration", "binding", "reader", .actor "agent"⟩)
    callbackClaimAllowed revoked "agent" "agent" "binding" "callback" true true = false := by native_decide

/-- A pending invocation retains a digest of the physical configuration documents
and their contents used to select and project its input. The source is read with the selected Agent in the claim
transaction. Digest equality relies on collision resistance of SHA-256. -/
def callbackGenerationAllowed (sourceVisible : Bool) (observed current : Option String) : Bool :=
  sourceVisible && observed.isSome && observed == current

theorem hidden_source_blocks_callback_claim (observed current : Option String) :
    callbackGenerationAllowed false observed current = false := by rfl

theorem stale_configuration_blocks_callback_claim (observed current : String)
    (different : observed ≠ current) :
    callbackGenerationAllowed true (some observed) (some current) = false := by
  simp [callbackGenerationAllowed, different]

theorem missing_generation_blocks_callback_claim (current : Option String) :
    callbackGenerationAllowed true none current = false := by rfl

/-- Module planning happens before the claim. Its resulting plan and claim
commit together only if the selected Agent can read the same module in that
transaction. Recovery uses the committed plan without rechecking a standing
grant after admission. -/
def callbackModuleClaim (visible same prepared committed : Bool) : Bool :=
  visible && same && prepared && committed

theorem callback_module_without_read_never_commits (same prepared committed : Bool) :
    callbackModuleClaim false same prepared committed = false := by rfl

theorem callback_module_without_plan_never_commits (visible same committed : Bool) :
    callbackModuleClaim visible same false committed = false := by
  simp [callbackModuleClaim]

/-- Once claimed, callback recovery uses the committed configuration snapshot;
a new admission uses the current selection and must pass its own checks. -/
def callbackRecoveryConfiguration (claimed : Bool) (snapshot current : String) : String :=
  if claimed then snapshot else current

theorem callback_recovery_keeps_admitted_configuration (snapshot current : String) :
    callbackRecoveryConfiguration true snapshot current = snapshot := by rfl

/-- Each callback workspace effect reads its host placement with the selected
Agent's native authority; the Node's visibility is not a substitute. -/
def callbackPlacementReadable (state : Household.State) (actor placement : String) : Bool :=
  Household.allowed 5 state actor "configuration" placement "read"

theorem callback_placement_revocation_blocks_read :
    let initial : Household.State :=
      { objects := [⟨"configuration", "placement", "user"⟩],
        relations := [⟨"configuration", "placement", "reader", .actor "agent"⟩], enrolled := [] }
    let revoked := Household.apply initial (.revoke ⟨"configuration", "placement", "reader", .actor "agent"⟩)
    callbackPlacementReadable revoked "agent" "placement" = false := by native_decide

/-- A prospective activation fence is an application protocol, not a native
ACP1 property. It must check the selected actor, the current binding and every
required standing grant before admitting unattended work. Its checks alone do
not make detached grants atomic with activation or revoke stale grants. -/
def mayActivate (actorIsSelected bindingCurrent grantsComplete : Bool) : Bool :=
  actorIsSelected && bindingCurrent && grantsComplete

theorem missing_standing_grant_blocks_fenced_activation (selected current : Bool) :
    mayActivate selected current false = false := by
  cases selected <;> cases current <;> rfl

theorem stale_binding_blocks_fenced_activation (selected granted : Bool) :
    mayActivate selected false granted = false := by
  cases selected <;> rfl

theorem foreign_actor_blocks_fenced_activation (current granted : Bool) :
    mayActivate false current granted = false := by rfl

/-- The Trigger combines user-authored routing and runtime observations in
one ACP document. Its creator grants the runtime a writer relationship and
the selected Agent a reader relationship; the selected EventSource has its
own reader grant. These native relations are independent of references and
must be managed when an operator rebinds a trigger. -/
def publishEventConfiguration (state : Household.State)
    (creator trigger source runtime agent : String) (fresh : Bool) : Household.State :=
  if fresh then
    let withRuntime := Household.apply state (.grantAs creator
      ⟨"configuration", trigger, "writer", .actor runtime⟩)
    let withTrigger := Household.apply withRuntime (.grantAs creator
      ⟨"configuration", trigger, "reader", .actor agent⟩)
    Household.apply withTrigger (.grantAs creator
      ⟨"configuration", source, "reader", .actor agent⟩)
  else state

theorem event_replacement_never_restores_grants (state : Household.State)
    (creator trigger source runtime agent : String) :
    publishEventConfiguration state creator trigger source runtime agent false = state := by rfl

/-- A fresh or rebound callback binding gives its selected Agent a standing
reader on its EventSource. Replacing an unchanged binding does not repair a
revoked grant. Other consumers may share that same source-reader relation. -/
def publishCallbackSource (state : Household.State)
    (creator source agent : String) (selectionChanged : Bool) : Household.State :=
  if selectionChanged then Household.apply state (.grantAs creator
    ⟨"configuration", source, "reader", .actor agent⟩) else state

theorem unchanged_callback_does_not_regrant_source (state : Household.State)
    (creator source agent : String) :
    publishCallbackSource state creator source agent false = state := by rfl

/-- Rebinding a CallbackBinding grants its new Agent access to the binding
and referenced Callback. Changing only the referenced Callback grants that
document; unchanged selection never restores revoked relations. -/
def rebindCallbackReaders (state : Household.State)
    (creator binding callback agent : String) (actorChanged callbackChanged : Bool) : Household.State :=
  let withBinding := if actorChanged then Household.apply state (.grantAs creator
      ⟨"configuration", binding, "reader", .actor agent⟩) else state
  if actorChanged || callbackChanged then Household.apply withBinding (.grantAs creator
    ⟨"configuration", callback, "reader", .actor agent⟩) else withBinding

theorem unchanged_callback_selection_preserves_grants (state : Household.State)
    (creator binding callback agent : String) :
    rebindCallbackReaders state creator binding callback agent false false = state := by rfl

/-- A selected module is shared configuration, so a new consumer receives a
reader tuple and unchanged selections never restore a revoked tuple. Removing a
consumer cannot revoke another consumer's access to the same module. -/
def publishCallbackModuleReader (state : Household.State)
    (creator module agent : String) (selectionChanged : Bool) : Household.State :=
  if selectionChanged then Household.apply state (.grantAs creator
    ⟨"configuration", module, "reader", .actor agent⟩) else state

theorem unchanged_module_selection_preserves_revocation (state : Household.State)
    (creator module agent : String) :
    publishCallbackModuleReader state creator module agent false = state := by rfl

/-- Trigger and CallbackBinding each select exactly one Agent. Moving that
selection replaces their reader tuple; shared source and callback grants have
independent provenance and are not removed by this operation. -/
def rebindExclusiveReader (state : Household.State)
    (creator document oldAgent newAgent : String) : Household.State :=
  if oldAgent == newAgent then state else
    let revoked := Household.apply state
      (.revoke ⟨"configuration", document, "reader", .actor oldAgent⟩)
    Household.apply revoked (.grantAs creator
      ⟨"configuration", document, "reader", .actor newAgent⟩)

theorem unchanged_exclusive_reader_preserves_revocation (state : Household.State)
    (creator document agent : String) :
    rebindExclusiveReader state creator document agent agent = state := by
  simp [rebindExclusiveReader]

def eventConfigurationFixture : Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩,
      ⟨"configuration", "trigger", "user"⟩,
      ⟨"configuration", "source", "user"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"configuration", "trigger", "parent", .object "nodes" "node"⟩,
      ⟨"configuration", "source", "parent", .object "nodes" "node"⟩], enrolled := [] }

theorem event_configuration_preserves_distinct_writers :
    let state := publishEventConfiguration eventConfigurationFixture "user" "trigger" "source" "runtime" "agent" true
    [Household.allowed 5 state "user" "configuration" "trigger" "update",
     Household.allowed 5 state "runtime" "configuration" "trigger" "update",
     Household.allowed 5 state "agent" "configuration" "trigger" "read",
     Household.allowed 5 state "agent" "configuration" "source" "read",
     Household.allowed 5 state "agent" "configuration" "trigger" "update",
     Household.allowed 5 state "sibling" "configuration" "source" "read"] =
    [true, true, true, true, false, false] := by native_decide

/-- Event admission re-observes the selected Agent's binding and source in
its native transaction. References do not authorize a new Agent, and a stale
runtime snapshot does not override native visibility. Native ACP dependency
conflict validation is the separate premise for a concurrent revocation. -/
def eventClaimAllowed (state : Household.State) (actor selected trigger source : String)
    (bindingCurrent sourceCurrent : Bool) : Bool :=
  actor == selected && bindingCurrent && sourceCurrent &&
    Household.allowed 5 state actor "configuration" trigger "read" &&
    Household.allowed 5 state actor "configuration" source "read"

theorem revoked_source_blocks_event_claim :
    let granted := publishEventConfiguration eventConfigurationFixture "user" "trigger" "source" "runtime" "agent" true
    let revoked := Household.apply granted
      (.revoke ⟨"configuration", "source", "reader", .actor "agent"⟩)
    eventClaimAllowed revoked "agent" "agent" "trigger" "source" true true = false := by native_decide

theorem former_agent_cannot_claim_after_rebinding (state : Household.State)
    (oldAgent newAgent trigger source : String) (different : oldAgent ≠ newAgent)
    (currentSource : Bool) :
    eventClaimAllowed state oldAgent newAgent trigger source true currentSource = false := by
  simp [eventClaimAllowed, different]

/-- The event source selected at discovery is retained with the fire intent.
Admission compares its runtime-relevant generation with the currently
Agent-readable source in the native transaction. Metadata that does not
change event selection does not affect this generation. -/
structure EventSourceGeneration where
  collection : String
  eventKind : String
  filter : Option String
  correlation : Option String
  grouped : Bool
  expectedCount : Option Nat
  expectedField : Option String
  timeout : Option Nat
  minimum : Nat
  workspaceAuthority : Option String
  deriving DecidableEq, Repr

def eventGenerationAllowed (observed current : EventSourceGeneration) : Bool :=
  decide (observed = current)

/-- A stale generation aborts the enclosing receipt transaction; no staged
receipt is visible when the native transaction rolls back. -/
def eventGenerationReceipt (observed current : EventSourceGeneration) (committed : Bool) : Bool :=
  eventGenerationAllowed observed current && committed

theorem changed_generation_has_no_receipt (observed current : EventSourceGeneration)
    (changed : observed ≠ current) (committed : Bool) :
    eventGenerationReceipt observed current committed = false := by
  simp [eventGenerationReceipt, eventGenerationAllowed, changed]

theorem event_generation_rejects_changed_filter (generation : EventSourceGeneration)
    (filter : Option String) (different : generation.filter ≠ filter) :
    eventGenerationAllowed generation { generation with filter } = false := by
  simp only [eventGenerationAllowed, decide_eq_false_iff_not]
  intro equal
  apply different
  exact congrArg EventSourceGeneration.filter equal

theorem event_generation_rejects_changed_correlation (generation : EventSourceGeneration)
    (correlation : Option String) (different : generation.correlation ≠ correlation) :
    eventGenerationAllowed generation { generation with correlation } = false := by
  simp only [eventGenerationAllowed, decide_eq_false_iff_not]
  intro equal
  apply different
  exact congrArg EventSourceGeneration.correlation equal

/-- The cached Task and Trigger fields that produce an event request are
compared at admission with the current documents read by the selected Agent.
The hook payload is serialized by the existing Task owner. -/
structure EventExecutionGeneration where
  taskId : String
  agent : String
  name : Option String
  prompt : String
  objective : Option String
  budget : Option Int
  output : Option String
  hooks : String
  emit : Bool
  sourceId : String
  triggerDoc : String
  sourceDoc : String
  taskDoc : String
  sessionTemplate : Option String
  concurrency : String
  deriving DecidableEq, Repr

def eventExecutionAllowed (observed current : EventExecutionGeneration) : Bool :=
  decide (observed = current)

theorem event_execution_rejects_changed_prompt (generation : EventExecutionGeneration)
    (prompt : String) (different : generation.prompt ≠ prompt) :
    eventExecutionAllowed generation { generation with prompt } = false := by
  simp only [eventExecutionAllowed, decide_eq_false_iff_not]
  intro equal
  apply different
  exact congrArg EventExecutionGeneration.prompt equal

theorem event_execution_rejects_changed_source (generation : EventExecutionGeneration)
    (sourceId : String) (different : generation.sourceId ≠ sourceId) :
    eventExecutionAllowed generation { generation with sourceId } = false := by
  simp only [eventExecutionAllowed, decide_eq_false_iff_not]
  intro equal
  apply different
  exact congrArg EventExecutionGeneration.sourceId equal

theorem event_execution_rejects_recreated_source (generation : EventExecutionGeneration)
    (sourceDoc : String) (different : generation.sourceDoc ≠ sourceDoc) :
    eventExecutionAllowed generation { generation with sourceDoc } = false := by
  simp only [eventExecutionAllowed, decide_eq_false_iff_not]
  intro equal
  apply different
  exact congrArg EventExecutionGeneration.sourceDoc equal

/-- Rebinding requires an owner-managed grant for the new Agent. A source may
be shared by other consumers, so changing one reference cannot by itself
revoke an existing source reader. Revocation is a separate native operation. -/
def rebindEventConfiguration (state : Household.State)
    (creator trigger source oldAgent newAgent : String) : Household.State :=
  let withTrigger := rebindExclusiveReader state creator trigger oldAgent newAgent
  Household.apply withTrigger (.grantAs creator
    ⟨"configuration", source, "reader", .actor newAgent⟩)

theorem rebound_event_does_not_implicitly_revoke_shared_source :
    let granted := publishEventConfiguration eventConfigurationFixture "user" "trigger" "source" "runtime" "agent" true
    let rebound := rebindEventConfiguration granted "user" "trigger" "source" "agent" "sibling"
    [Household.allowed 5 rebound "agent" "configuration" "source" "read",
     Household.allowed 5 rebound "sibling" "configuration" "source" "read"] = [true, true] := by native_decide

/-- A configuration transaction can seed a cursor before the selected Agent
starts. The creator owns the cursor and grants the Agent its writer relation;
the Node parent gives the user control. Later reads of an existing cursor do
not repair or restore a revoked writer. -/
def publishEventCursor (state : Household.State)
    (creator cursor node agent : String) (fresh committed : Bool) : Household.State :=
  if fresh && committed then
    let created := Household.directCreate state creator "configuration" cursor true true
    let parented := Household.apply created (.grantAs creator
      ⟨"configuration", cursor, "parent", .object "nodes" node⟩)
    Household.apply parented (.grantAs creator
      ⟨"configuration", cursor, "writer", .actor agent⟩)
  else state

theorem existing_event_cursor_never_regrants (state : Household.State)
    (creator cursor node agent : String) (committed : Bool) :
    publishEventCursor state creator cursor node agent false committed = state := by rfl

/-- A consumer reassignment changes the writer on its own cursor in the
configuration transaction. The native cursor owner must authorize both
relationship operations; unchanged selection does not repair a revocation. -/
def rebindEventCursor (state : Household.State)
    (creator cursor oldAgent newAgent : String) : Household.State :=
  if oldAgent == newAgent then state else
    let revoked := Household.apply state (.revokeAs creator
      ⟨"configuration", cursor, "writer", .actor oldAgent⟩)
    Household.apply revoked (.grantAs creator
      ⟨"configuration", cursor, "writer", .actor newAgent⟩)

theorem unchanged_cursor_binding_preserves_state (state : Household.State)
    (creator cursor agent : String) :
    rebindEventCursor state creator cursor agent agent = state := by simp [rebindEventCursor]

theorem cursor_rebinding_replaces_the_selected_writer :
    let state : Household.State := { objects := [⟨"nodes", "node", "user"⟩], relations := [], enrolled := [] }
    let published := publishEventCursor state "user" "cursor" "node" "agent" true true
    let rebound := rebindEventCursor published "user" "cursor" "agent" "sibling"
    [Household.allowed 5 rebound "agent" "configuration" "cursor" "update",
     Household.allowed 5 rebound "sibling" "configuration" "cursor" "update"] = [false, true] := by native_decide

/-- A source-collection change moves a consumer to a distinct cursor. The
old writer is revoked and a fresh cursor is seeded in the same transaction;
if the destination already exists, its owner must separately grant access. -/
def moveEventCursor (state : Household.State)
    (creator oldCursor newCursor node agent : String) : Household.State :=
  let revoked := Household.apply state (.revokeAs creator
    ⟨"configuration", oldCursor, "writer", .actor agent⟩)
  publishEventCursor revoked creator newCursor node agent true true

theorem source_change_moves_cursor_authority :
    let state : Household.State := { objects := [⟨"nodes", "node", "user"⟩], relations := [], enrolled := [] }
    let old := publishEventCursor state "user" "old" "node" "agent" true true
    let moved := moveEventCursor old "user" "old" "new" "node" "agent"
    [Household.allowed 5 moved "agent" "configuration" "old" "update",
     Household.allowed 5 moved "agent" "configuration" "new" "update"] = [false, true] := by native_decide

theorem aborted_event_cursor_has_no_effect (state : Household.State)
    (creator cursor node agent : String) (fresh : Bool) :
    publishEventCursor state creator cursor node agent fresh false = state := by
  cases fresh <;> rfl

theorem client_seeded_cursor_has_selected_writer :
    let state : Household.State := { objects := [⟨"nodes", "node", "user"⟩], relations := [], enrolled := [] }
    let published := publishEventCursor state "user" "cursor" "node" "agent" true true
    [Household.allowed 5 published "user" "configuration" "cursor" "update",
     Household.allowed 5 published "agent" "configuration" "cursor" "update",
     Household.allowed 5 published "sibling" "configuration" "cursor" "read"] = [true, true, false] := by native_decide

/-- A grouped event clock is caused and owned by the selected Agent. Its
Agent parent gives the user inherited control; reads during reconciliation
must therefore use the selected Agent rather than the runtime actor. -/
def eventGroupFixture : Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "agent-doc", "user"⟩],
    relations := [⟨"nodes", "node", "runtime_reader", .actor "runtime"⟩,
      ⟨"agents", "agent-doc", "parent", .object "nodes" "node"⟩,
      ⟨"agents", "agent-doc", "executor", .actor "agent"⟩], enrolled := [] }

theorem event_group_owner_and_user_read_without_runtime_fallback :
    let state := Household.commitAgentChild eventGroupFixture "group" "agent" "agent-doc" true
    [Household.allowed 5 state "agent" "agent_children" "group" "read",
     Household.allowed 5 state "user" "agent_children" "group" "read",
     Household.allowed 5 state "runtime" "agent_children" "group" "read",
     Household.allowed 5 state "sibling" "agent_children" "group" "read"] =
    [true, true, false, false] := by native_decide

/-- Manual Task publication can create a receipt as the user, while an
trigger creates it as the selected Agent. Session children preserve either
native creator and grant the selected Agent access through `working_agent`.
The receipt and both relationships require one native transaction after the
physical session is known; an aborted transaction publishes none of them. -/
def publishTaskReceipt (state : Household.State)
    (creator receipt session agent : String) (fresh committed : Bool) : Household.State :=
  if fresh && committed then
    let created := Household.directCreate state creator "children" receipt true true
    let parented := Household.apply created (.grantAs creator
      ⟨"children", receipt, "parent", .object "sessions" session⟩)
    Household.apply parented (.grantAs creator
      ⟨"children", receipt, "working_agent", .actor agent⟩)
  else state

theorem task_receipt_abort (state : Household.State)
    (creator receipt session agent : String) (fresh : Bool) :
    publishTaskReceipt state creator receipt session agent fresh false = state := by
  cases fresh <;> rfl

theorem task_receipt_duplicate_preserves_revocation (state : Household.State)
    (creator receipt session agent : String) (committed : Bool) :
    publishTaskReceipt state creator receipt session agent false committed = state := by rfl

def taskReceiptFixture : Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "agent-doc", "user"⟩,
      ⟨"sessions", "session", "user"⟩],
    relations := [⟨"agents", "agent-doc", "parent", .object "nodes" "node"⟩,
      ⟨"sessions", "session", "parent", .object "agents" "agent-doc"⟩,
      ⟨"sessions", "session", "working_agent", .actor "agent"⟩], enrolled := [] }

theorem manual_and_trigger_receipts_preserve_selected_access :
    let manual := publishTaskReceipt taskReceiptFixture "user" "manual" "session" "agent" true true
    let triggered := publishTaskReceipt manual "agent" "triggered" "session" "agent" true true
    [Household.allowed 5 triggered "user" "children" "manual" "read",
     Household.allowed 5 triggered "agent" "children" "manual" "read",
     Household.allowed 5 triggered "user" "children" "triggered" "read",
     Household.allowed 5 triggered "agent" "children" "triggered" "update",
     Household.allowed 5 triggered "sibling" "children" "triggered" "read"] =
    [true, true, true, true, false] := by native_decide

/-- A downstream Agent may receive a producer's session children through a
standing Agent reader relation. This grants the producer's session access in
general, not just one outcome collection; narrower per-outcome sharing needs
an explicit native reader on the individual child. -/
theorem producer_reader_allows_downstream_outcome :
    let base := Household.apply taskReceiptFixture
      (.grantAs "user" ⟨"agents", "agent-doc", "reader", .actor "consumer"⟩)
    let state := publishTaskReceipt base "agent" "outcome" "session" "agent" true true
    [Household.allowed 5 state "consumer" "children" "outcome" "read",
     Household.allowed 5 state "sibling" "children" "outcome" "read"] = [true, false] := by native_decide

/-- A callback child belongs to its authenticated agent. The publisher must
bind the physical Agent parent to that same actor even if the actor can read
other Agent documents. ACP1 does not constrain an arbitrary authored parent. -/
def callbackParentAllowed (actor : Option String) (selected : String) : Bool :=
  actor == some selected

/-- Fresh callback children publish their selected Agent parent in the same
native transaction. A duplicate is observed without restoring a revoked
relationship. -/
def publishCallbackChild (state : Household.State) (actor selected child parent : String)
    (fresh committed : Bool) : Household.State :=
  if actor == selected && fresh && committed then
    Household.commitAgentChild state child actor parent true
  else state

theorem callback_child_abort (state : Household.State) (actor selected child parent : String)
    (fresh : Bool) : publishCallbackChild state actor selected child parent fresh false = state := by
  simp [publishCallbackChild]

theorem callback_child_duplicate_preserves_revocation (state : Household.State)
    (actor selected child parent : String) (committed : Bool) :
    publishCallbackChild state actor selected child parent false committed = state := by
  simp [publishCallbackChild]

theorem callback_parent_requires_actor (selected : String) :
    callbackParentAllowed none selected = false := by rfl

theorem callback_parent_accepts_selected_actor (selected : String) :
    callbackParentAllowed (some selected) selected = true := by simp [callbackParentAllowed]

theorem callback_parent_rejects_foreign_actor (actor selected : String)
    (different : actor ≠ selected) :
    callbackParentAllowed (some actor) selected = false := by
  simp [callbackParentAllowed, different]

end Identity.Automation
