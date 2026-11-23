import Proofs.Session.PromptAssembly.ClaudeOrigin
import Proofs.Store.Identity.Household

/-! The sections of the per-request `<context>` message. The message is built
for every request and stripped from history before the next one, so a section
changes nothing in the cached prefix. -/

namespace PromptAssembly.RequestContext

open PromptAssembly.ClaudeOrigin (Cause)

inductive Section where
  | instructions
  | workflowReturn
  | profileModel
  deriving DecidableEq, Repr

/-- `creation` is the cause named by the session's verified creation receipt,
`client` when the session has none (a client opened it). `profileModel` is
whether the Agent runs an admitted inference profile, read per request.

Decision (maintainer, 2026-11-16, gents#4067 C2): a plugin job collects its
child's final reply itself, so a child that is not told so spends calls
messaging a caller it may not reach. Only a verified plugin-job receipt adds
the `workflowReturn` section; no user-authored text decides it, and
`agent_message` keeps its reachability.

Decision (maintainer, 2026-11-17, gents#4067 B4): every request names its
profile's model. An Agent built without a profile has only a placeholder
model, so it gets no line rather than a guess. -/
def sections (instructions : Bool) (creation : Cause) (profileModel : Bool) :
    List Section :=
  (if instructions then [.instructions] else []) ++
    (if creation = .pluginJob then [.workflowReturn] else []) ++
    (if profileModel then [.profileModel] else [])

theorem only_a_plugin_job_child_is_told_where_its_answer_returns
    (instructions : Bool) (creation : Cause) (profileModel : Bool) :
    Section.workflowReturn ∈ sections instructions creation profileModel ↔
      creation = .pluginJob := by
  cases instructions <;> cases profileModel <;> by_cases h : creation = .pluginJob <;>
    simp [sections, h]

theorem only_a_profile_names_a_model
    (instructions : Bool) (creation : Cause) (profileModel : Bool) :
    Section.profileModel ∈ sections instructions creation profileModel ↔ profileModel = true := by
  cases instructions <;> cases profileModel <;> by_cases h : creation = .pluginJob <;>
    simp [sections, h]

theorem other_sessions_keep_their_instructions_only
    (instructions : Bool) (creation : Cause) (h : creation ≠ .pluginJob) :
    sections instructions creation false = if instructions then [.instructions] else [] := by
  simp [sections, h]

theorem instructions_come_first (creation : Cause) (profileModel : Bool) :
    (sections true creation profileModel).head? = some .instructions := by
  simp [sections]

/-- What reads a request's stored history. -/
inductive HistoryReader where
  | dispatch
  | admissionEstimate
  | summarizer
  deriving DecidableEq, Repr

/-- Decision (gents#4067 B4 review): a stored `<context>` row was the context
of the request that wrote it. Dispatch sends the current request's context
instead, so the admission estimate counts what dispatch sends only when it
drops the stored rows too, and the summarizer would otherwise fold stale
instructions into the session's summary. No reader keeps them. -/
def keepsStoredContext : HistoryReader → Bool
  | .dispatch | .admissionEstimate | .summarizer => false

theorem estimate_reads_what_dispatch_sends :
    keepsStoredContext .admissionEstimate = keepsStoredContext .dispatch := rfl

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
