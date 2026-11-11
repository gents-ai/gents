import Mathlib.Data.List.Basic

namespace Hooks.Catalog

/-- How a lifecycle seam is carried. The first four are the contract
(`Hooks.stage`, a value core consumes, `Hooks.admit`, `Hooks.Delivery`). The
last three are seams the hook catalog (gents-ai/gents#3642) lists that are not
hooks and must not be forced into one. -/
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

structure Point where
  name : String
  shapes : List Shape
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
def requestAdmitVerify : Point := ⟨"request.admit.verify", [.prepared]⟩

/-- A plugin stamps its own input slot and the queue partition key before
core signs (`Hooks.createStage`), and stages its own rows with the request.
Fire receipts, session creation and the hop are core's `create`. -/
def requestCreateStage : Point := ⟨"request.create.stage", [.returned, .staging]⟩

/-- A delivery owed by the creating transaction. Its consumers today are
workspace binding, the trigger driver and session relationships, all core;
workspace stays core and gets no hook work (gents-ai/gents#3656). No bundled
plugin listens here yet. -/
def requestCreateCommitted : Point := ⟨"request.create.committed", [.delivery, .core]⟩

/-- A claim verdict (`Hooks.ClaimVerdict`). The serial queue gate, arrival
order and the session stop latch are guards of core's own claim, not
listeners. -/
def requestClaimGate : Point := ⟨"request.claim.gate", [.returned]⟩

/-- Staging after the claim affected the row. Session projection and queue
folding at this point are core. -/
def requestClaimApplied : Point := ⟨"request.claim.applied", [.staging]⟩

/-- Not a hook. Preparing a turn's input is a pipeline whose steps return
content (overlay, context, summaries), some after writing in their own
transactions or calling a model. Provider input has one boundary, the owned
loop; a plugin contributes through rows it staged earlier, not by returning
prompt text here. -/
def requestPrepareInput : Point := ⟨"request.prepare_input", [.contributor]⟩

/-- An external effect whose outcome selects the terminal state: prepared
outside, bound to the execution generation, re-validated inside. Its
consumers today are workspace seal and Task hooks, both core. -/
def requestWorkComplete : Point := ⟨"request.work.complete", [.prepared, .core]⟩

/-- Staging after core's terminal compare-and-set.

What has to change in core: there are fourteen terminal write sites and they
do not run the same dependents. A hook here is only meaningful once every
site goes through one core terminal transition that returns the row; the
contract's `around` assumes exactly one. -/
def requestTerminalStage : Point := ⟨"request.terminal.stage", [.staging]⟩

/-- Delivery owed by the terminal transaction (`Hooks.Delivery`). -/
def requestTerminalCommitted : Point := ⟨"request.terminal.committed", [.delivery]⟩

/-- Staging on both sides of the latch write (`Hooks.around`): Goals pauses
on interrupt and stop and restages on resume. Quiescence witnesses read
process state and are core. -/
def controlSession : Point := ⟨"control.session", [.staging]⟩

/-- A prepared verdict bound to the physical call and canonical arguments. -/
def toolCallDispatchDecide : Point := ⟨"tool_call.dispatch.decide", [.prepared]⟩

/-- Staging after the tool-call terminal is won. The existing pack path,
writes prepared by a guest and applied by the host in the terminal
transaction, is this shape. -/
def toolCallTerminalStage : Point := ⟨"tool_call.terminal.stage", [.staging]⟩

/-- Delivery owed by the tool-call terminal. -/
def toolCallTerminalCommitted : Point := ⟨"tool_call.terminal.committed", [.delivery]⟩

/-- Staging: a plugin is shown its rows of the source session and returns
creates for the child. -/
def sessionForkStage : Point := ⟨"session.fork.stage", [.staging]⟩

/-- Not a hook. Per-turn callbacks are the owned loop's existing session
seam; compaction, steering intake and output obligations are core. -/
def inferenceTurn : Point := ⟨"inference.turn", [.core]⟩

/-- Not a hook of its own. The sweep is core's loop over
`Hooks.Delivery.pending`; a plugin registers nothing beyond the deliveries it
is owed. -/
def runtimeRecoverSweep : Point := ⟨"runtime.recover.sweep", [.core]⟩

/-- Not a data hook. Runtime generation and process-state observers take no
rows and return no effects; they stay an observer registry. Trigger sources,
tool handlers and callback handlers a feature supplies are the same kind of
seam: Goals is a trigger source and owns three tools, which this contract
does not describe. -/
def runtimeLifecycleObserve : Point := ⟨"runtime.lifecycle.observe", [.provider]⟩

def points : List Point :=
  [requestAdmitVerify, requestCreateStage, requestCreateCommitted, requestClaimGate,
   requestClaimApplied, requestPrepareInput, requestWorkComplete, requestTerminalStage,
   requestTerminalCommitted, controlSession, toolCallDispatchDecide, toolCallTerminalStage,
   toolCallTerminalCommitted, sessionForkStage, inferenceTurn, runtimeRecoverSweep,
   runtimeLifecycleObserve]

theorem every_catalog_point_is_classified :
    points.length = 17 ∧ (points.map (·.name)).Nodup ∧
      points.all (fun point => !point.shapes.isEmpty) = true := by
  decide

/-- Thirteen of the seventeen points are carried by the contract; four are
not hooks. -/
theorem contract_carries_thirteen_points :
    (points.filter fun point => point.shapes.any Shape.isHook).length = 13 := by
  decide

end Hooks.Catalog
