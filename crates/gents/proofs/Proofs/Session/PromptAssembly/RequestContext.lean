import Proofs.Session.Identity.AgentCreationReceipt
import Proofs.Store.Identity.Household

/-! The sections of the per-request `<context>` message. The message is built
for every request and stripped from history before the next one, so a section
changes nothing in the cached prefix. -/

namespace PromptAssembly.RequestContext

open Identity.AgentCreationReceipt (Cause)

inductive Section where
  | instructions
  | workflowReturn
  deriving DecidableEq, Repr

/-- `creation` is the cause named by the session's verified creation receipt,
`none` when the session has none (a client opened it).

Decision (maintainer, 2026-11-16, gents#4067 C2): a plugin job collects its
child's final reply itself, so a child that is not told so spends calls
messaging a caller it may not reach. Only a verified plugin-job receipt adds
the `workflowReturn` section; no user-authored text decides it, and
`agent_message` keeps its reachability. -/
def sections (instructions : Bool) (creation : Option Cause) : List Section :=
  (if instructions then [.instructions] else []) ++
    (if creation = some .pluginJob then [.workflowReturn] else [])

theorem only_a_plugin_job_child_is_told_where_its_answer_returns
    (instructions : Bool) (creation : Option Cause) :
    Section.workflowReturn ∈ sections instructions creation ↔ creation = some .pluginJob := by
  cases instructions <;> by_cases h : creation = some .pluginJob <;> simp [sections, h]

theorem other_sessions_keep_their_instructions_only
    (instructions : Bool) (creation : Option Cause) (h : creation ≠ some .pluginJob) :
    sections instructions creation = if instructions then [.instructions] else [] := by
  simp [sections, h]

theorem instructions_come_first (creation : Option Cause) :
    (sections true creation).head? = some .instructions := by
  simp [sections]

/-- A plugin-job child session: created by the starting Agent `starter`,
parented by the worker's Agent document and worked by `worker`. -/
def childSessionFixture : Identity.Household.State :=
  { objects := [⟨"nodes", "node", "user"⟩, ⟨"agents", "worker-doc", "user"⟩,
      ⟨"sessions", "child", "starter"⟩],
    relations := [⟨"agents", "worker-doc", "parent", .object "nodes" "node"⟩,
      ⟨"agents", "worker-doc", "executor", .actor "worker"⟩,
      ⟨"sessions", "child", "parent", .object "agents" "worker-doc"⟩,
      ⟨"sessions", "child", "working_agent", .actor "worker"⟩],
    enrolled := [] }

/-- The creation receipt is read as the Agent working the request, which
document ACP lets read its session through `working_agent`, so the read never
relies on the node key's DAC bypass (`Household.readObserved`); Agent-caused
work never falls back to runtime authority. Another Agent of the runtime does
not read it. -/
theorem the_working_agent_reads_its_session_receipt :
    [Identity.Household.nativeRead false 6 childSessionFixture "worker" "sessions" "child",
     Identity.Household.nativeRead false 6 childSessionFixture "stranger" "sessions" "child"] =
      [true, false] := by
  native_decide

end PromptAssembly.RequestContext
