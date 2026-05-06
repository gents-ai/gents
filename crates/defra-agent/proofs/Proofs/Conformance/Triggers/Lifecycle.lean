import Proofs.Triggers
import Proofs.Request

/-!
# Trigger/Request Lifecycle Projection

Shared vocabulary for relating the trigger-engine request projection to the
request-lifecycle model.
-/

/-- Bool projection of lifecycle terminality into the trigger layer. -/
def requestStateToTriggerTerminal : RequestState → Bool
  | .completed => true
  | .failed => true
  | .superseded => true
  | .dead => true
  | .interrupted => true
  | .pending => false
  | .claimed => false
  | .processing => false
  | .inputRequired => false

/-- The Bool projection is definitionally coherent with `HasTerminal`. -/
theorem requestStateToTriggerTerminal_eq_true_iff (rs : RequestState) :
    requestStateToTriggerTerminal rs = true ↔ isTerminal rs := by
  cases rs <;> simp [requestStateToTriggerTerminal, HasTerminal.isTerminal, RequestState.instHasTerminal]

/-- The false branch is exactly non-terminality at the lifecycle layer. -/
theorem requestStateToTriggerTerminal_eq_false_iff (rs : RequestState) :
    requestStateToTriggerTerminal rs = false ↔ ¬ isTerminal rs := by
  cases rs <;> simp [requestStateToTriggerTerminal, HasTerminal.isTerminal, RequestState.instHasTerminal]

/--
Cross-layer coherence between a trigger-layer `AgentRequest` and a lifecycle
`RequestContext`.

This is intentionally thin: it only relates the fields the trigger layer
observes directly.
-/
def TriggerLifecycleCoherent (rTrig : AgentRequest) (rReq : RequestContext) : Prop :=
  rTrig.isTerminal = requestStateToTriggerTerminal rReq.state ∧
  rTrig.executionOrigin = rReq.origin

/-- Terminal observations in the trigger layer coincide with lifecycle terminality. -/
theorem triggerLifecycleCoherent_terminal_iff
    {rTrig : AgentRequest} {rReq : RequestContext}
    (h : TriggerLifecycleCoherent rTrig rReq) :
    rTrig.isTerminal = true ↔ isTerminal rReq.state := by
  rcases h with ⟨h_terminal, _⟩
  rw [h_terminal]
  exact requestStateToTriggerTerminal_eq_true_iff rReq.state

/-- The coherence relation directly exposes origin equality. -/
theorem triggerLifecycleCoherent_origin_eq
    {rTrig : AgentRequest} {rReq : RequestContext}
    (h : TriggerLifecycleCoherent rTrig rReq) :
    rTrig.executionOrigin = rReq.origin :=
  h.2
