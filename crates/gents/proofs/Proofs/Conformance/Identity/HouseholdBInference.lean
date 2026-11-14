import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Identity.HouseholdStore
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household Identity.Conformance

structure CompactionInput where
  phase : String
  actor : String
  document : String
  deriving ToJson

def compactionState (phase : String) : State :=
  let initial := Configuration.compactionSelectionFixture
  let edgeA : Relation := ⟨"configuration", "compaction-a", "selected_by", .entitySet "configuration" "context" "selected_by"⟩
  let edgeB : Relation := ⟨"configuration", "compaction-b", "selected_by", .entitySet "configuration" "context" "selected_by"⟩
  let rebound := apply (apply initial (.revoke edgeA)) (.grant edgeB)
  if phase == "revoked" || phase == "unchanged" then apply initial (.revoke edgeA)
  else if phase == "rebound" then rebound
  else if phase == "direct-rebound" then apply rebound (.grant
    ⟨"configuration", "compaction-a", "reader", .actor "selected"⟩)
  else if phase == "context-revoked" then apply rebound (.revoke
    ⟨"configuration", "context", "selected_by", .entitySet "agents" "agent" "executor"⟩)
  else if phase == "executor-revoked" then apply rebound (.revoke
    ⟨"agents", "agent", "executor", .actor "selected"⟩)
  else initial

def compactionCases : List CompactionInput :=
  (["initial", "revoked", "unchanged", "rebound", "direct-rebound", "context-revoked",
    "executor-revoked"] : List String).flatMap fun phase =>
    (["selected", "sibling"] : List String).flatMap fun actor =>
      (["compaction-a", "compaction-b"] : List String).map fun document => { phase, actor, document }

structure CompactionVisibility where
  expected : Bool
  guardExpected : Bool
  deriving ToJson

/-- The phases are one ordered history on one served node: each phase's
selection change is applied after the previous phase's observations. -/
@[conformance_family]
def compactionSelection : Family CompactionInput CompactionVisibility where
  inputs := compactionCases
  model := fun i =>
    let state := compactionState i.phase
    let visible := Household.allowed 8 state i.actor "configuration" i.document "read"
    { expected := visible
      guardExpected := Household.allowed 8 state i.actor "agents" "agent" "read" &&
        Household.allowed 8 state i.actor "configuration" "context" "read" && visible }

structure CompactionUpdate where
  updateExpected : Bool
  ownerUpdateExpected : Bool
  deriving ToJson

/-- A successful update changes the stored threshold the later phases read, so
the selected Agent's and the owner's updates are attempted once, on the initial
selection of the selected document. -/
@[conformance_family]
def compactionUpdate : Family CompactionInput CompactionUpdate where
  inputs := compactionCases.filter fun i =>
    i.phase == "initial" && i.actor == "selected" && i.document == "compaction-a"
  model := fun i =>
    let state := compactionState i.phase
    { updateExpected := Household.allowed 8 state i.actor "configuration" i.document "update"
      ownerUpdateExpected := Household.allowed 8 state "user" "configuration" i.document "update" }

structure RuntimeDefaultAccountInput where
  name : String
  runtimeActor : Bool
  sameNode : Bool
  sameSnapshot : Bool
  nativeRead : Bool
  visibleDefault : Option String
  deriving ToJson

def runtimeDefaultAccountCases : List RuntimeDefaultAccountInput :=
  [("visible-named", true, true, true, true, some "named"),
   ("visible-empty", true, true, true, true, none),
   ("foreign-node", true, false, true, true, some "named"),
   ("agent-actor", false, true, true, true, some "named"),
   ("other-snapshot", true, true, false, true, some "named"),
   ("read-failed", true, true, true, false, some "named")].map
    fun (name, runtimeActor, sameNode, sameSnapshot, nativeRead, visibleDefault) =>
      { name, runtimeActor, sameNode, sameSnapshot, nativeRead, visibleDefault }

def runtimeDefaultAccountAllowed (i : RuntimeDefaultAccountInput) : Bool :=
  Configuration.runtimeMayReadDefaultAccount ⟨i.runtimeActor, i.sameNode, i.sameSnapshot, i.nativeRead⟩

@[conformance_family]
def runtimeDefaultAccountRead : Family RuntimeDefaultAccountInput Bool where
  inputs := runtimeDefaultAccountCases.filter fun i => i.sameSnapshot && i.nativeRead
  model := runtimeDefaultAccountAllowed

/-- model-production-gap: production cannot build a different snapshot or a
failed native read for the runtime default account. `runtime_default_oauth_account_ref`
takes the Agent write's own `ConfigApplyTxn`, so the read always shares its
snapshot, and the metadata query and the delegated read check are answered by
the same native ACP state in that transaction, so a row the runtime may query
but not read is not produced by any served-node fixture. -/
@[conformance_family]
def runtimeDefaultAccountReadUnreached : Family RuntimeDefaultAccountInput Bool where
  inputs := runtimeDefaultAccountCases.filter fun i => !(i.sameSnapshot && i.nativeRead)
  model := runtimeDefaultAccountAllowed

structure TaskCheckpointInput where
  actor : String
  role : String
  cursor : Nat
  through : Nat
  receipt : Bool
  deriving ToJson

/-- The steps are one ordered history on one event-source cursor: each step
starts from the cursor the previous step left. -/
@[conformance_family]
def taskCheckpointArrival : Family TaskCheckpointInput Nat where
  inputs := [(runtimeDid, 0, 1, true), (agentDid, 0, 1, true), (agentDid, 1, 1, true),
    (agentDid, 1, 2, false), (agentDid, 1, 2, true)].map fun (actor, cursor, through, receipt) =>
    { actor, role := roleOf actor, cursor, through, receipt }
  model := fun i => Identity.Automation.checkpointTaskArrival HouseholdFixture.initial i.actor
    "automated-request" i.receipt true true i.cursor i.through

structure TaskTargetInput where
  name : String
  actor : String
  document : String
  transition : String
  deriving ToJson

def taskTargetState (transition : String) : State :=
  if transition == "rebound" then TaskTarget.rebindTask TaskTarget.fixture "user" "a" "b" true
  else if transition == "shared-rebound" then
    TaskTarget.rebindTask (apply TaskTarget.fixture (.grantAs "user"
      ⟨"configuration", "task", "reader", .actor "agent-a"⟩)) "user" "a" "b" true
  else if transition == "task-revoked" then
    let rebound := TaskTarget.rebindTask TaskTarget.fixture "user" "a" "b" true
    let revoked := apply rebound (.revoke (TaskTarget.taskEdge "b"))
    TaskTarget.rebindTask revoked "user" "b" "b" true
  else if transition == "target-revoked" then apply TaskTarget.fixture (.revoke
    ⟨"configuration", "target", "selected_by", .entitySet "configuration" "tools" "selected_by"⟩)
  else TaskTarget.fixture

def taskTargetCases : List TaskTargetInput :=
  [("task-owner", "user", "task", "initial"), ("task-runtime", "runtime", "task", "initial"),
   ("task-selected", "agent-a", "task", "initial"), ("task-unrelated", "agent-b", "task", "initial"),
   ("task-old-target", "agent-a", "task", "rebound"), ("task-new-target", "agent-b", "task", "rebound"),
   ("task-explicit-reader", "agent-a", "task", "shared-rebound"),
   ("task-revoked-replay", "agent-b", "task", "task-revoked"),
   ("target-selected", "agent-a", "target", "initial"),
   ("target-unrelated", "agent-b", "target", "initial"),
   ("target-revoked", "agent-a", "target", "target-revoked")].map
    fun (name, actor, document, transition) => { name, actor, document, transition }

def taskTargetVisible (i : TaskTargetInput) : Bool :=
  Household.allowed 8 (taskTargetState i.transition) i.actor "configuration" i.document "read"

/-- The cases are one ordered history on one served node: each transition is
applied after the earlier cases' observations. -/
@[conformance_family]
def taskTargetVisibility : Family TaskTargetInput Bool where
  inputs := taskTargetCases
  model := taskTargetVisible

/-- The self-configuration tool fixture holds one Task selected for another
Agent, so it builds only the unrelated-Agent case. -/
@[conformance_family]
def taskTargetTool : Family TaskTargetInput Bool where
  inputs := taskTargetCases.filter fun i => i.name == "task-unrelated"
  model := taskTargetVisible

structure PluginBackendInput where
  name : String
  actor : Option String
  revoked : String
  deriving ToJson

@[conformance_family]
def pluginBackendUse : Family PluginBackendInput Bool where
  inputs := [("selected", some "agent", ""), ("sibling", some "sibling", ""), ("absent", none, ""),
    ("profile_revoked", some "agent", "profile"), ("backend_revoked", some "agent", "backend")].map
    fun (name, actor, revoked) => { name, actor, revoked }
  model := fun i =>
    let state := if i.revoked == "" then Configuration.pluginBackendFixture else
      Household.apply Configuration.pluginBackendFixture
        (.revoke ⟨"configuration", i.revoked, "reader", .actor "agent"⟩)
    Configuration.pluginBackendMayUse state i.actor

end Conformance.Household
