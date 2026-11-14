import Proofs.Conformance.Identity.HouseholdStore
import Proofs.Conformance.Identity.HouseholdDocuments
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household Identity.Conformance

deriving instance ToJson for Identity.Automation.EventSourceGeneration
deriving instance ToJson for Identity.Automation.EventExecutionGeneration

structure RevokedInput where
  revoked : Bool
  deriving ToJson

@[conformance_family]
def callbackBindingClaim : Family RevokedInput Bool where
  inputs := [⟨false⟩, ⟨true⟩]
  model := fun i =>
    let granted := Automation.publishCallbackReaders ConfigurationFixture.callbackState userDid
      "binding" "callback" agentDid true
    let state := if i.revoked then Household.apply granted
      (.revoke ⟨"configuration", "binding", "reader", .actor agentDid⟩) else granted
    Automation.callbackClaimAllowed state agentDid agentDid "binding" "callback" true true

structure UnattendedInput where
  name : String
  actorMatches : Bool
  enabled : Bool
  invoke : String
  deriving ToJson

def invokeObservation (invoke : String) : Automation.InvokeObservation :=
  if invoke == "allowed" then .allowed else if invoke == "denied" then .denied else .unavailable

def unattendedSelectedAgentInputs : List UnattendedInput :=
  [⟨"allowed", true, true, "allowed"⟩, ⟨"disabled", true, false, "allowed"⟩,
   ⟨"reader_only", true, true, "denied"⟩]

def unattendedAllowed (i : UnattendedInput) : Bool :=
  Automation.unattendedAgentAdmission i.actorMatches i.enabled (invokeObservation i.invoke)

/-- The claimant of a callback invocation is the selected Agent reading its own
Agent document; the scenario changes only the document's `enabled` field and
the Agent's executor relation. -/
@[conformance_family]
def unattendedCallbackClaim : Family UnattendedInput Bool where
  inputs := unattendedSelectedAgentInputs
  model := unattendedAllowed

/-- Event admission runs as the selected Agent reading its own Agent document;
the scenario changes only the document's `enabled` field and the Agent's
executor relation. -/
@[conformance_family]
def unattendedEventAdmission : Family UnattendedInput Bool where
  inputs := unattendedSelectedAgentInputs
  model := unattendedAllowed

def unattendedInputs : List UnattendedInput :=
  unattendedSelectedAgentInputs ++
    [⟨"unavailable", true, true, "unavailable"⟩, ⟨"wrong_actor", false, true, "allowed"⟩]

/-- An unavailable invoke observation needs a node without node ACP, which
`household_unattended_documents_unavailable` builds on its own fixture. -/
@[conformance_family]
def unattendedDocumentsAdmission : Family UnattendedInput Bool where
  inputs := unattendedInputs.filter fun i => i.invoke != "unavailable"
  model := unattendedAllowed

@[conformance_family]
def unattendedDocumentsUnavailable : Family UnattendedInput Bool where
  inputs := unattendedInputs.filter fun i => i.invoke == "unavailable"
  model := unattendedAllowed

/-- An unavailable invoke observation needs a node without node ACP, which
`household_unattended_session_unavailable` builds on its own fixture. -/
@[conformance_family]
def unattendedSessionAdmission : Family UnattendedInput Bool where
  inputs := unattendedInputs.filter fun i => i.invoke != "unavailable"
  model := unattendedAllowed

@[conformance_family]
def unattendedSessionUnavailable : Family UnattendedInput Bool where
  inputs := unattendedInputs.filter fun i => i.invoke == "unavailable"
  model := unattendedAllowed

structure CallbackGenerationInput where
  name : String
  sourceVisible : Bool
  observed : Option String
  current : Option String
  deriving ToJson

def callbackGenerationInputs : List CallbackGenerationInput :=
  [⟨"same", true, some "original", some "original"⟩,
   ⟨"source_revoked", false, some "original", some "original"⟩,
   ⟨"source_filter", true, some "original", some "source_filter"⟩,
   ⟨"binding_input", true, some "original", some "binding_input"⟩,
   ⟨"callback_handler", true, some "original", some "callback_handler"⟩,
   ⟨"binding_recreated", true, some "original", some "binding_recreated"⟩,
   ⟨"source_recreated", true, some "original", some "source_recreated"⟩,
   ⟨"callback_recreated", true, some "original", some "callback_recreated"⟩,
   ⟨"missing", true, none, some "original"⟩]

@[conformance_family]
def callbackGenerationClaim : Family CallbackGenerationInput Bool where
  inputs := callbackGenerationInputs
  model := fun i => Automation.callbackGenerationAllowed i.sourceVisible i.observed i.current

structure ActorRevokedInput where
  actor : String
  role : String
  revoked : Bool
  deriving ToJson

@[conformance_family]
def callbackPlacement : Family ActorRevokedInput Bool where
  inputs := [(userDid, false), (agentDid, false), (siblingDid, false), (agentDid, true)].map
    fun (actor, revoked) => { actor, role := roleOf actor, revoked }
  model := fun i =>
    let initial : State :=
      { objects := [⟨"configuration", "placement", userDid⟩],
        relations := [⟨"configuration", "placement", "reader", .actor agentDid⟩], enrolled := [] }
    let state := if i.revoked then Household.apply initial
      (.revoke ⟨"configuration", "placement", "reader", .actor agentDid⟩) else initial
    Automation.callbackPlacementReadable state i.actor "placement"

structure ChildInput where
  name : String
  fresh : Bool
  committed : Bool
  revoke : Bool
  deriving ToJson

structure ChildRead where
  userRead : Bool
  agentRead : Bool
  deriving ToJson

@[conformance_family]
def callbackChild : Family ChildInput ChildRead where
  inputs := [⟨"commit", true, true, false⟩, ⟨"abort", true, false, false⟩,
    ⟨"duplicate", false, true, true⟩]
  model := fun i =>
    let initial : State :=
      { objects := [⟨"nodes", "home", userDid⟩, ⟨"agents", "agent-doc", userDid⟩],
        relations := [⟨"agents", "agent-doc", "parent", .object "nodes" "home"⟩,
          ⟨"agents", "agent-doc", "executor", .actor agentDid⟩], enrolled := [] }
    let published := Automation.publishCallbackChild initial agentDid agentDid "child" "agent-doc" true true
    let before := if i.revoke then Household.apply published
      (.revoke ⟨"agent_children", "child", "parent", .object "agents" "agent-doc"⟩) else initial
    let result := Automation.publishCallbackChild before agentDid agentDid "child" "agent-doc"
      i.fresh i.committed
    { userRead := Household.allowed 5 result userDid "agent_children" "child" "read"
      agentRead := Household.allowed 5 result agentDid "agent_children" "child" "read" }

structure ModuleClaimInput where
  name : String
  visible : Bool
  same : Bool
  prepared : Bool
  committed : Bool
  deriving ToJson

def moduleClaimInputs : List ModuleClaimInput :=
  [⟨"allowed", true, true, true, true⟩, ⟨"revoked", false, true, true, true⟩,
   ⟨"changed", true, false, true, true⟩, ⟨"recreated", true, false, true, true⟩,
   ⟨"unprepared", true, true, false, true⟩, ⟨"aborted", true, true, true, false⟩]

@[conformance_family]
def callbackModuleClaim : Family ModuleClaimInput Bool where
  inputs := moduleClaimInputs.filter fun i => i.committed
  model := fun i => Automation.callbackModuleClaim i.visible i.same i.prepared i.committed

/-- model-production-gap: `claim_invocation` commits the module plan and the
claim in its own transaction; no production path aborts that transaction after
a valid module plan has been prepared, so an aborted module claim cannot be
driven. -/
@[conformance_family]
def callbackModuleClaimUnreached : Family ModuleClaimInput Bool where
  inputs := moduleClaimInputs.filter fun i => !i.committed
  model := fun i => Automation.callbackModuleClaim i.visible i.same i.prepared i.committed

structure ClaimedInput where
  claimed : Bool
  deriving ToJson

/-- The callback admitted with display name `admitted` is later changed to
`changed`; recovery reports the display name of the configuration it selects. -/
@[conformance_family]
def callbackRecoveryConfiguration : Family ClaimedInput String where
  inputs := [⟨true⟩, ⟨false⟩]
  model := fun i => Automation.callbackRecoveryConfiguration i.claimed "admitted" "changed"

/-- The checkpoint scenario claims one invocation, which records the claim time
and retains its configuration together; the mixed cases are replayed by
`household_callback_claim_recorded`. -/
@[conformance_family]
def callbackClaimCheckpoint : Family CallbackClaimInput Bool where
  inputs := [⟨false, false⟩, ⟨true, true⟩]
  model := fun i => Identity.UnattendedReceipt.visibleCallbackClaim i.claimTime i.retainedConfiguration

structure EventGrant where
  read : Bool
  claim : Bool
  deriving ToJson

def eventSelected (i : EventConfigurationInput) : String :=
  if i.rebound then siblingDid else agentDid

/-- The event-grants scenario observes reads and claims as the two Agents; the
owner's and runtime's writes are replayed by `household_event_configuration_access`. -/
@[conformance_family]
def eventGrantsConfiguration : Family EventConfigurationInput EventGrant where
  inputs := eventConfigurationInputs.filter fun i => i.permission == "read" && i.role != "runtime"
  model := fun i =>
    let state := eventConfigurationAfter i
    { read := Household.allowed 5 state i.actor "configuration" i.id i.permission
      claim := Automation.eventClaimAllowed state i.actor (eventSelected i) "trigger" "source" true true }

/-- A rebound CallbackBinding follows the same exclusive reader rebinding as the
rebound Trigger. -/
@[conformance_family]
def callbackBindingRebound : Family EventConfigurationInput Bool where
  inputs := eventConfigurationInputs.filter fun i => i.rebound && !i.postRevoke && i.id == "trigger"
  model := fun i => Household.allowed 5 (eventConfigurationAfter i) i.actor "configuration" "trigger" "read"

structure ActorInput where
  actor : String
  role : String
  deriving ToJson

def actorInputs (actors : List String) : List ActorInput :=
  actors.map fun actor => { actor, role := roleOf actor }

def eventReboundCheckpointState : State :=
  { objects := [⟨"nodes", "home", userDid⟩,
      ⟨"agents", "old-agent", userDid⟩, ⟨"sessions", "old-session", agentDid⟩,
      ⟨"delivered_requests", "old-request", agentDid⟩],
    relations := [⟨"agents", "old-agent", "parent", .object "nodes" "home"⟩,
      ⟨"agents", "old-agent", "executor", .actor agentDid⟩,
      ⟨"sessions", "old-session", "parent", .object "agents" "old-agent"⟩,
      ⟨"sessions", "old-session", "working_agent", .actor agentDid⟩,
      ⟨"delivered_requests", "old-request", "parent", .object "agents" "old-agent"⟩,
      ⟨"delivered_requests", "old-request", "session", .object "sessions" "old-session"⟩,
      ⟨"delivered_requests", "old-request", "working_agent", .actor agentDid⟩], enrolled := [] }

@[conformance_family]
def eventReboundCheckpoint : Family ActorInput Bool where
  inputs := actorInputs [userDid, agentDid, siblingDid]
  model := fun i =>
    Household.allowed 5 eventReboundCheckpointState i.actor "delivered_requests" "old-request" "read"

def reboundCursor (i : ActorInput) : Nat :=
  Automation.checkpointTaskArrival eventReboundCheckpointState i.actor "old-request" true true true 0 1

/-- The rebound scenario checkpoints the Trigger's cursor as the rebound Agent,
which is the consumer's selected Agent and holds its writer relation. -/
@[conformance_family]
def eventReboundCursor : Family ActorInput Nat where
  inputs := actorInputs [siblingDid]
  model := reboundCursor

/-- model-production-gap: model and production disagree. Production
`checkpoint_prefix` (crates/gents-documents/src/event_consumer.rs:411-414)
refuses a checkpoint by any actor other than the consumer's currently selected
Agent (`event consumer selected Agent changed before checkpoint`), while
`Identity.Automation.checkpointTaskArrival`
(Proofs/Store/Identity/Automation.lean:152-156) gates only on reading the
request, and `reboundCursor` passes `cursorWritable = true` for the former
Agent. The model therefore advances the owner's and the former Agent's cursor
where production refuses. Production is the safe side; this is an authority
model, so the fix belongs in Lean first, for the maintainer and the security
pass (#3683). -/
@[conformance_family]
def eventReboundCursorUnreached : Family ActorInput Nat where
  inputs := actorInputs [userDid, agentDid]
  model := reboundCursor

structure PresenceInput where
  name : String
  present : Bool
  deriving ToJson

@[conformance_family]
def ordinaryReceiptVisibility : Family PresenceInput Bool where
  inputs := [⟨"hidden", true⟩, ⟨"absent", false⟩]
  model := fun i => Identity.UnattendedReceipt.ordinaryVisible i.present false

structure StaleClaimInput where
  actor : String
  selected : String
  deriving ToJson

structure StaleClaim where
  sourceRead : Bool
  claimAllowed : Bool
  deriving ToJson

@[conformance_family]
def eventStaleClaim : Family StaleClaimInput StaleClaim where
  inputs := [⟨agentDid, siblingDid⟩]
  model := fun i =>
    let granted := Automation.publishEventConfiguration ConfigurationFixture.eventConfigurationState
      userDid "trigger" "source" runtimeDid agentDid true
    let rebound := Automation.rebindEventConfiguration granted userDid "trigger" "source" agentDid i.selected
    { sourceRead := Household.allowed 5 rebound i.actor "configuration" "source" "read"
      claimAllowed := Automation.eventClaimAllowed rebound i.actor i.selected "trigger" "source" true true }

@[conformance_family]
def eventCursorRebind : Family ActorRevokedInput Bool where
  inputs := [(agentDid, false), (siblingDid, false), (siblingDid, true)].map
    fun (actor, revoked) => { actor, role := roleOf actor, revoked }
  model := fun i =>
    let initial : State := { objects := [⟨"nodes", "home", userDid⟩], relations := [], enrolled := [] }
    let created := Automation.publishEventCursor initial userDid "cursor" "home" agentDid true true
    let rebound := Automation.rebindEventCursor created userDid "cursor" agentDid siblingDid
    let state := if i.revoked then Household.apply rebound
      (.revoke ⟨"configuration", "cursor", "writer", .actor siblingDid⟩) else rebound
    Household.allowed 5 state i.actor "configuration" "cursor" "update"

structure CursorInput where
  cursor : String
  deriving ToJson

@[conformance_family]
def eventCursorMove : Family CursorInput Bool where
  inputs := [⟨"old"⟩, ⟨"new"⟩]
  model := fun i =>
    let initial : State := { objects := [⟨"nodes", "home", userDid⟩], relations := [], enrolled := [] }
    let created := Automation.publishEventCursor initial userDid "old" "home" agentDid true true
    let moved := Automation.moveEventCursor created userDid "old" "new" "home" agentDid
    Household.allowed 5 moved agentDid "configuration" i.cursor "update"

structure SourceGenerationInput where
  name : String
  observed : Automation.EventSourceGeneration
  current : Automation.EventSourceGeneration
  deriving ToJson

def sourceGenerationInputs : List SourceGenerationInput :=
  let original : Automation.EventSourceGeneration :=
    ⟨"Records", "created", some "{eligible: {_eq: true}}", some "batch", true,
      some 2, none, some 30, 1, none⟩
  [("same", original),
   ("filter", { original with filter := some "{eligible: {_eq: false}}" }),
   ("correlation", { original with correlation := some "other_batch" }),
   ("collection", { original with collection := "Other" }),
   ("group", { original with grouped := false, expectedCount := none, timeout := none }),
   ("count", { original with expectedCount := some 3 }),
   ("timeout", { original with timeout := some 60 }),
   ("minimum", { original with minimum := 2 }),
   ("workspace", { original with workspaceAuthority := some "readOnly" })].map
    fun (name, current) => { name, observed := original, current }

structure GenerationAdmission where
  allowed : Bool
  receipt : Bool
  deriving ToJson

@[conformance_family]
def eventGenerationAdmission : Family SourceGenerationInput GenerationAdmission where
  inputs := sourceGenerationInputs
  model := fun i =>
    { allowed := Automation.eventGenerationAllowed i.observed i.current
      receipt := Automation.eventGenerationReceipt i.observed i.current true }

@[conformance_family]
def eventSourceGenerationProjection : Family SourceGenerationInput Bool where
  inputs := sourceGenerationInputs
  model := fun i => Automation.eventGenerationAllowed i.observed i.current

structure ExecutionGenerationInput where
  name : String
  observed : Automation.EventExecutionGeneration
  current : Automation.EventExecutionGeneration
  deriving ToJson

def executionGenerationInputs : List ExecutionGenerationInput :=
  let original : Automation.EventExecutionGeneration :=
    ⟨"task", "agent", none, "work", none, none, none, "[]", false, "source",
      "trigger-doc", "source-doc", "task-doc", none, "parallel"⟩
  [("same", original), ("prompt", { original with prompt := "changed" }),
   ("source", { original with sourceId := "other" }),
   ("trigger_recreated", { original with triggerDoc := "new-trigger" }),
   ("source_recreated", { original with sourceDoc := "new-source" }),
   ("task_recreated", { original with taskDoc := "new-task" }),
   ("agent", { original with agent := "other" }),
   ("session", { original with sessionTemplate := some "session" }),
   ("concurrency", { original with concurrency := "serial" }),
   ("objective", { original with objective := some "objective" }),
   ("budget", { original with objective := some "objective", budget := some 25 }),
   ("output", { original with output := some "schema" }),
   ("hooks", { original with hooks := "[{\"hook_id\":\"hook\",\"phase\":\"before\",\"command\":[\"true\"]}]" }),
   ("emit", { original with emit := true })].map
    fun (name, current) => { name, observed := original, current }

@[conformance_family]
def eventExecutionAdmission : Family ExecutionGenerationInput Bool where
  inputs := executionGenerationInputs
  model := fun i => Automation.eventExecutionAllowed i.observed i.current

@[conformance_family]
def eventExecutionProjection : Family ExecutionGenerationInput Bool where
  inputs := executionGenerationInputs
  model := fun i => Automation.eventExecutionAllowed i.observed i.current

structure ModuleReaderInput where
  actor : String
  role : String
  changed : Bool
  revoked : Bool
  deriving ToJson

@[conformance_family]
def callbackModuleReader : Family ModuleReaderInput Bool where
  inputs := [(agentDid, false, false), (siblingDid, false, false), (agentDid, true, false),
    (siblingDid, true, false), (agentDid, false, true)].map
    fun (actor, changed, revoked) => { actor, role := roleOf actor, changed, revoked }
  model := fun i =>
    let initial : State := { objects := [⟨"configuration", "module", userDid⟩], relations := [], enrolled := [] }
    let granted := Automation.publishCallbackModuleReader initial userDid "module" agentDid true
    let revokedState := if i.revoked then Household.apply granted
      (.revoke ⟨"configuration", "module", "reader", .actor agentDid⟩) else granted
    let state := Automation.publishCallbackModuleReader revokedState userDid "module" siblingDid i.changed
    Household.allowed 5 state i.actor "configuration" "module" "read"

@[conformance_family]
def callbackReboundVisibility : Family ActorInput Bool where
  inputs := actorInputs [userDid, agentDid, siblingDid]
  model := fun i =>
    let initial : State :=
      { objects := [⟨"nodes", "home", userDid⟩, ⟨"agents", "old-agent", userDid⟩],
        relations := [⟨"agents", "old-agent", "parent", .object "nodes" "home"⟩,
          ⟨"agents", "old-agent", "executor", .actor agentDid⟩], enrolled := [] }
    let state := Household.commitAgentChild initial "old-invocation" agentDid "old-agent" true
    Household.allowed 5 state i.actor "agent_children" "old-invocation" "read"

@[conformance_family]
def eventGroupState : Family ActorInput Bool where
  inputs := actorInputs [userDid, agentDid, runtimeDid, siblingDid]
  model := fun i =>
    let initial : State :=
      { objects := [⟨"nodes", "home", userDid⟩, ⟨"agents", "agent-doc", userDid⟩],
        relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩,
          ⟨"agents", "agent-doc", "parent", .object "nodes" "home"⟩,
          ⟨"agents", "agent-doc", "executor", .actor agentDid⟩], enrolled := [] }
    let state := Household.commitAgentChild initial "group" agentDid "agent-doc" true
    Household.allowed 5 state i.actor "agent_children" "group" "read"

structure ScheduleInput where
  name : String
  actor : String
  role : String
  revokeParent : Bool
  missing : Bool
  deriving ToJson

@[conformance_family]
def scheduleVisibility : Family ScheduleInput Bool where
  inputs := [("runtime", runtimeDid, false, false), ("agent", agentDid, false, false),
    ("revoked-parent", runtimeDid, true, false), ("replaced", runtimeDid, false, true)].map
    fun (name, actor, revokeParent, missing) => { name, actor, role := roleOf actor, revokeParent, missing }
  model := fun i =>
    let initial : State :=
      { objects := [⟨"nodes", "home", userDid⟩],
        relations := [⟨"nodes", "home", "runtime_reader", .actor runtimeDid⟩], enrolled := [] }
    let published := Automation.publishSchedule initial userDid "schedule" "home" true
    let state := if i.revokeParent then Household.apply published
      (.revoke ⟨"configuration", "schedule", "parent", .object "nodes" "home"⟩) else published
    let selected := if i.missing then "replaced" else "schedule"
    Automation.scheduleVisible state runtimeDid i.actor selected

structure ScheduleWriteInput where
  name : String
  grant : Bool
  revoke : Bool
  deriving ToJson

@[conformance_family]
def scheduleWrite : Family ScheduleWriteInput Bool where
  inputs := [⟨"denied", false, false⟩, ⟨"granted", true, false⟩, ⟨"revoked", true, true⟩]
  model := fun i =>
    let initial : State := { objects := [⟨"nodes", "home", userDid⟩], relations := [], enrolled := [] }
    let published := Automation.publishSchedule initial userDid "schedule" "home" true
    let writer : Relation := ⟨"configuration", "schedule", "writer", .actor agentDid⟩
    let granted := if i.grant then Household.apply published (.grantAs userDid writer) else published
    let state := if i.revoke then Household.apply granted (.revokeAs userDid writer) else granted
    Household.allowed 5 state agentDid "configuration" "schedule" "update"

structure TaskReceiptInput where
  creator : String
  creatorRole : String
  actor : String
  role : String
  permission : String
  producerReader : Bool
  deriving ToJson

def taskReceiptInputs : List TaskReceiptInput :=
  [(userDid, userDid, "read", false), (userDid, agentDid, "read", false),
   (agentDid, userDid, "read", false), (agentDid, agentDid, "read", false),
   (agentDid, agentDid, "update", false), (agentDid, siblingDid, "read", false),
   (agentDid, siblingDid, "read", true)].map fun (creator, actor, permission, producerReader) =>
    { creator, creatorRole := roleOf creator, actor, role := roleOf actor, permission, producerReader }

def taskReceiptAllowed (i : TaskReceiptInput) : Bool :=
  let initial : State :=
    { objects := [⟨"nodes", "home", userDid⟩, ⟨"agents", "agent-doc", userDid⟩,
        ⟨"sessions", "session", userDid⟩],
      relations := [⟨"agents", "agent-doc", "parent", .object "nodes" "home"⟩,
        ⟨"sessions", "session", "parent", .object "agents" "agent-doc"⟩,
        ⟨"sessions", "session", "working_agent", .actor agentDid⟩], enrolled := [] }
  let shared := if i.producerReader then Household.apply initial
    (.grantAs userDid ⟨"agents", "agent-doc", "reader", .actor siblingDid⟩) else initial
  let state := Automation.publishTaskReceipt shared i.creator "receipt" "session" agentDid true true
  Household.allowed 5 state i.actor "children" "receipt" i.permission

@[conformance_family]
def taskReceiptSession : Family TaskReceiptInput Bool where
  inputs := taskReceiptInputs
  model := taskReceiptAllowed

/-- Task delivery publishes the receipt as the selected Agent and never shares
the producer's Agent document, so it builds only the triggered receipt's reads
without a producer reader; `household_task_receipt_session` replays the rest. -/
@[conformance_family]
def taskReceiptDelivery : Family TaskReceiptInput Bool where
  inputs := taskReceiptInputs.filter fun i =>
    i.creator == agentDid && i.permission == "read" && !i.producerReader
  model := taskReceiptAllowed

end Conformance.Household
