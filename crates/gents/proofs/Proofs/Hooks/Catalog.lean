import Mathlib.Data.List.Basic

namespace Hooks.Catalog

/-- How a lifecycle seam is carried. The first four are the contract
(`Hooks.stage` or `Hooks.stageDropping`, a value core consumes, `Hooks.admit`,
`Hooks.Delivery`). The last three are seams the hook catalog
(gents-ai/gents#3642) lists that are not hooks and must not be forced into
one. -/
inductive Shape where
  | staging
  | returned
  | prepared
  | delivery
  | contributor
  | provider
  | core
  deriving DecidableEq, Repr

def Shape.isHook : Shape → Bool
  | .staging | .returned | .prepared | .delivery => true
  | .contributor | .provider | .core => false

/-- Whether a plugin's answer can keep core's transition from committing
(maintainer decision, gents-ai/gents#3656). A plugin may refuse work from
starting or continuing. It may never prevent work from stopping: at interrupt,
stop and terminal transitions, and at any delivery, core's transition has
committed or commits regardless, and a failed plugin staging is owed as a
delivery (`Hooks.mustCommit`). -/
inductive Policy where
  | mayRefuse
  | mustCommit
  | notHook
  deriving DecidableEq, Repr

structure Point where
  name : String
  shapes : List Shape
  policy : Policy
  deriving DecidableEq, Repr

/-- A verdict prepared outside any transaction (admit, deny, unavailable) and
re-validated at the claim boundary against the request and its admission
record.

What has to change in core: admission hard-codes Goal policy beside its one
call into the feature (`request/admission.rs`: the client Goal continuation
flag, the Goal queue-source denial, the continuation-edge check). For the
contract to suffice those checks become the Goals plugin's prepared verdict
and admission keeps only the generic rule that a missing, stale or
unavailable verdict never admits. The Trigger and Task reads there are core:
triggers and tasks stay in core. -/
def requestAdmitVerify : Point := ⟨"request.admit.verify", [.prepared], .mayRefuse⟩

/-- A plugin stamps its own input slot and the queue partition key before
core signs (`Hooks.createStage`), and stages its own rows with the request.
Fire receipts, session creation and the hop are core's `create`. -/
def requestCreateStage : Point :=
  ⟨"request.create.stage", [.returned, .staging], .mayRefuse⟩

/-- A delivery owed by the creating transaction. Its consumers today are
workspace binding, the trigger driver and session relationships, all core;
workspace stays core and gets no hook work (gents-ai/gents#3656). No bundled
plugin listens here yet. -/
def requestCreateCommitted : Point :=
  ⟨"request.create.committed", [.delivery, .core], .mustCommit⟩

/-- A claim verdict (`Hooks.ClaimVerdict`). The serial queue gate, arrival
order and the session stop latch are guards of core's own claim, not
listeners. -/
def requestClaimGate : Point := ⟨"request.claim.gate", [.returned], .mayRefuse⟩

/-- Staging after the claim affected the row. Session projection and queue
folding at this point are core. -/
def requestClaimApplied : Point := ⟨"request.claim.applied", [.staging], .mayRefuse⟩

/-- Not a hook. Preparing a turn's input is a pipeline whose steps return
content (overlay, context, summaries), some after writing in their own
transactions or calling a model. Provider input has one boundary, the owned
loop; a plugin contributes through rows it staged earlier, not by returning
prompt text here. -/
def requestPrepareInput : Point := ⟨"request.prepare_input", [.contributor], .notHook⟩

/-- An external effect whose outcome selects the terminal state: prepared
outside, bound to the execution generation, re-validated inside. A denial
selects a failed terminal; it does not keep the request from ending. Its
consumers today are workspace seal and Task hooks, both core. -/
def requestWorkComplete : Point := ⟨"request.work.complete", [.prepared, .core], .mayRefuse⟩

/-- Staging after core's terminal compare-and-set. The terminal always
commits; a plugin that cannot stage is owed a delivery. A failed TriggerFire
read rolls back every request terminal today
(`request/lifecycle/execution_lease.rs`).

What has to change in core: there are fourteen terminal write sites and they
do not run the same dependents. A hook here is only meaningful once every
site goes through one core terminal transition that returns the row. -/
def requestTerminalStage : Point := ⟨"request.terminal.stage", [.staging], .mustCommit⟩

/-- Delivery owed by the terminal transaction (`Hooks.Delivery`). -/
def requestTerminalCommitted : Point :=
  ⟨"request.terminal.committed", [.delivery], .mustCommit⟩

/-- Staging around the latch write of an interrupt or stop: Goals pauses. The
latch always commits. A failed read of Goal state blocks an interrupt today
(`request/interrupt.rs`). The catalog lists one `control.session` point; it is
two here because resume is refusable and halting is not. -/
def controlSessionHalt : Point := ⟨"control.session.halt", [.staging], .mustCommit⟩

/-- Staging around resume: Goals restages, creating its continuation through
core's `request.create` (`Hooks.Resume`). Resume starts work, so a plugin may
refuse it. Quiescence witnesses read process state and are core. -/
def controlSessionResume : Point := ⟨"control.session.resume", [.staging], .mayRefuse⟩

/-- A prepared verdict bound to the physical call and canonical arguments. -/
def toolCallDispatchDecide : Point :=
  ⟨"tool_call.dispatch.decide", [.prepared], .mayRefuse⟩

/-- Staging after the tool-call terminal is won; the terminal always commits.
The existing pack path, writes prepared by a guest and applied by the host in
the terminal transaction, is this shape, except that a refused write re-runs
the terminal as failed today. -/
def toolCallTerminalStage : Point := ⟨"tool_call.terminal.stage", [.staging], .mustCommit⟩

/-- Delivery owed by the tool-call terminal. -/
def toolCallTerminalCommitted : Point :=
  ⟨"tool_call.terminal.committed", [.delivery], .mustCommit⟩

/-- Staging: a plugin is shown its rows of the source session and returns
creates for the child. -/
def sessionForkStage : Point := ⟨"session.fork.stage", [.staging], .mayRefuse⟩

/-- Not a hook. Per-turn callbacks are the owned loop's existing session
seam; compaction, steering intake and output obligations are core. There is
no point at which a plugin may refuse the next provider turn: a monitor that
wants one stops the request instead (`Hooks.Safety`). -/
def inferenceTurn : Point := ⟨"inference.turn", [.core], .notHook⟩

/-- Not a hook of its own. The sweep is core's loop over
`Hooks.Delivery.pending`; a plugin registers nothing beyond the deliveries it
is owed. -/
def runtimeRecoverSweep : Point := ⟨"runtime.recover.sweep", [.core], .notHook⟩

/-- Not a data hook. Runtime generation and process-state observers take no
rows and return no effects; they stay an observer registry. Trigger sources,
tool handlers and callback handlers a feature supplies are the same kind of
seam: Goals is a trigger source and owns three tools, which this contract
does not describe. -/
def runtimeLifecycleObserve : Point :=
  ⟨"runtime.lifecycle.observe", [.provider], .notHook⟩

/-- Added to the catalog for a monitor of what an agent produces. A delivery
owed by the transaction that durably records an assistant message or a
tool-call terminal of a request. It carries the request and session document
IDs, the recorded item's document ID, kind and sequence, and the actor the
request executes under; the listener declares reads for the content. It is
outside the owned loop and sees only durable output, so the loop stays the
sole provider-input boundary. It may request a stop
(`Hooks.Safety.StopOp`). -/
def requestOutputRecorded : Point := ⟨"request.output.recorded", [.delivery], .mustCommit⟩

def points : List Point :=
  [requestAdmitVerify, requestCreateStage, requestCreateCommitted, requestClaimGate,
   requestClaimApplied, requestPrepareInput, requestWorkComplete, requestTerminalStage,
   requestTerminalCommitted, controlSessionHalt, controlSessionResume, toolCallDispatchDecide,
   toolCallTerminalStage, toolCallTerminalCommitted, sessionForkStage, inferenceTurn,
   runtimeRecoverSweep, runtimeLifecycleObserve, requestOutputRecorded]

/-- The seventeen catalogued points, with `control.session` split in two and
`request.output.recorded` added. -/
theorem every_catalog_point_is_classified :
    points.length = 19 ∧ (points.map (·.name)).Nodup ∧
      points.all (fun point => !point.shapes.isEmpty) = true := by
  decide

/-- Fifteen points are carried by the contract; four are not hooks. A point
has a refusal policy exactly when the contract carries it. -/
theorem policy_matches_shape :
    (points.filter fun point => point.shapes.any Shape.isHook).length = 15 ∧
      points.all (fun point =>
        point.shapes.any Shape.isHook == (point.policy != .notHook)) = true := by
  decide

/-- Nothing owed after commit can be refused, and the only staging points
that must commit are the halting and terminal ones. -/
theorem deliveries_must_commit :
    points.all (fun point =>
      !point.shapes.contains .delivery || point.policy == .mustCommit) = true ∧
    (points.filter fun point =>
      point.shapes.contains .staging && point.policy == .mustCommit).map (·.name) =
      ["request.terminal.stage", "control.session.halt", "tool_call.terminal.stage"] := by
  decide

end Hooks.Catalog
