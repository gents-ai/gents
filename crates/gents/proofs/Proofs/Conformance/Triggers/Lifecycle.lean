import Proofs.Triggers
import Proofs.Request

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

theorem requestStateToTriggerTerminal_eq_true_iff (rs : RequestState) :
    requestStateToTriggerTerminal rs = true ↔ isTerminal rs := by
  cases rs <;> simp [requestStateToTriggerTerminal, HasTerminal.isTerminal, RequestState.instHasTerminal]

theorem requestStateToTriggerTerminal_eq_false_iff (rs : RequestState) :
    requestStateToTriggerTerminal rs = false ↔ ¬ isTerminal rs := by
  cases rs <;> simp [requestStateToTriggerTerminal, HasTerminal.isTerminal, RequestState.instHasTerminal]

def TriggerLifecycleCoherent (rTrig : AgentRequest) (rReq : RequestContext) : Prop :=
  rTrig.isTerminal = requestStateToTriggerTerminal rReq.state ∧
  rTrig.executionOrigin = rReq.origin

theorem triggerLifecycleCoherent_terminal_iff
    {rTrig : AgentRequest} {rReq : RequestContext}
    (h : TriggerLifecycleCoherent rTrig rReq) :
    rTrig.isTerminal = true ↔ isTerminal rReq.state := by
  rcases h with ⟨h_terminal, _⟩
  rw [h_terminal]
  exact requestStateToTriggerTerminal_eq_true_iff rReq.state

theorem triggerLifecycleCoherent_origin_eq
    {rTrig : AgentRequest} {rReq : RequestContext}
    (h : TriggerLifecycleCoherent rTrig rReq) :
    rTrig.executionOrigin = rReq.origin :=
  h.2
