import Proofs.Enrollment.RequestInput

namespace AgentMessageDelivery

inductive Mode where
  | queue | steer | interrupt
  deriving DecidableEq, Repr

inductive Target where
  | idle | active | unknown
  deriving DecidableEq, Repr

inductive Decision where
  | start | queue | steer | interrupt | unavailable | denied
  deriving DecidableEq, Repr

/-- A steer may be consumed by the active request only when the existing
intake owner can authenticate and match its scope, and the completion owner
can still deliver the sending agent's result. Unknown target state cannot
identify the physical request to steer. -/
def decide (mode : Mode) (target : Target) (intakeCompatible completionLinked interruptAllowed : Bool) : Decision :=
  match mode, target with
  | .queue, .idle => .start
  | .queue, _ => .queue
  | .steer, .idle => .start
  | .steer, .unknown => .unavailable
  | .steer, .active => if intakeCompatible && completionLinked then .steer else .unavailable
  | .interrupt, _ => if interruptAllowed then .interrupt else .denied

theorem steer_requires_intake (completionLinked interruptAllowed : Bool) :
    decide .steer .active false completionLinked interruptAllowed = .unavailable := by
  simp [decide]

theorem steer_requires_completion (intakeCompatible interruptAllowed : Bool) :
    decide .steer .active intakeCompatible false interruptAllowed = .unavailable := by
  cases intakeCompatible <;> rfl

theorem interrupt_requires_authority (target : Target) (intakeCompatible completionLinked : Bool) :
    decide .interrupt target intakeCompatible completionLinked false = .denied := by
  cases target <;> rfl

theorem idle_steer_starts_new_request (intakeCompatible completionLinked interruptAllowed : Bool) :
    decide .steer .idle intakeCompatible completionLinked interruptAllowed = .start := by rfl

/-- User steering retains the active request's requester. An agent steer may
use its own authenticated requester only when it carries a complete parent
request and tool-call edge; signature verification remains with admission. -/
def requesterAllowed (agentSteer sameRequester fullParentEdge : Bool) : Bool :=
  if agentSteer then fullParentEdge else sameRequester

theorem agent_steer_requires_parent_edge (sameRequester : Bool) :
    requesterAllowed true sameRequester false = false := by rfl

theorem user_steer_requires_same_requester (fullParentEdge : Bool) :
    requesterAllowed false false fullParentEdge = false := by rfl

/-- A consumed agent message contributes authored content only. It may not
replace the active request's workspace, working directory, or skill selection.
A user input retains the existing exact-context requirement. -/
def contextAllowed (agentSteer exactContext emptyAgentContext : Bool) : Bool :=
  if agentSteer then emptyAgentContext else exactContext

theorem agent_input_cannot_override_context (exactContext : Bool) :
    contextAllowed true exactContext false = false := by rfl

theorem user_input_requires_matching_context (emptyAgentContext : Bool) :
    contextAllowed false false emptyAgentContext = false := by rfl

/-- A steer names the physical active request it was intended to change.
A later turn in the same session cannot consume that input. -/
def targetsHead (requested head : String) : Bool := requested == head

theorem different_head_does_not_consume (requested head : String) (h : requested ≠ head) :
    targetsHead requested head = false := by simp [targetsHead, h]

/-- A steer inherits only the consuming request's assignment reference;
its opening self-reference is normalized by the existing reissue owner. -/
def steerAssignment (head : Enrollment.RequestInput) (headDoc : String) :=
  (Enrollment.reissuedInput head headDoc).goalAssignment

theorem steer_assignment_is_the_existing_reissue (head : Enrollment.RequestInput) (headDoc : String) :
    steerAssignment head headDoc = (Enrollment.reissuedInput head headDoc).goalAssignment := by rfl

theorem no_head_assignment_means_no_steer_assignment (head : Enrollment.RequestInput) (headDoc : String)
    (h : head.goalAssignment = none) : steerAssignment head headDoc = none := by
  simp [steerAssignment, Enrollment.reissuedInput, h]

inductive TargetClaimState where
  | unknown | pending | active | terminal
  deriving DecidableEq, Repr

inductive TargetedClaim where
  | wait | reject
  deriving DecidableEq, Repr

/-- A targeted steer is consumed only at its active turn's safe boundary.
It is never claimed as a standalone request: a live or unobserved target waits,
and an ended target is rejected so the sender can choose a new delivery. -/
def targetedClaim : TargetClaimState → TargetedClaim
  | .terminal => .reject
  | _ => .wait

/-- A request cannot wait for itself to become active. -/
def targetedClaimFor (selfTarget : Bool) (state : TargetClaimState) : TargetedClaim :=
  if selfTarget then .reject else targetedClaim state

theorem self_target_is_rejected (state : TargetClaimState) :
    targetedClaimFor true state = .reject := by rfl

theorem live_target_is_not_claimed : targetedClaim .active = .wait := by rfl

theorem ended_target_is_rejected : targetedClaim .terminal = .reject := by rfl

inductive TurnOutcome where
  | running | completed | failed | interrupted
  deriving DecidableEq, Repr

inductive SenderOutcome where
  | pending | completed | failed | interrupted
  deriving DecidableEq, Repr

/-- A folded agent message retains its caller edge and follows the exact
active request that consumed it. Its superseded marker acknowledges intake;
it is not itself a failed turn. Notification follows the consuming request's
terminal outcome. -/
def foldedSenderOutcome (callerEdgeMatches consumingRequestMatches : Bool)
    (outcome : TurnOutcome) : Option SenderOutcome :=
  if !callerEdgeMatches || !consumingRequestMatches then none
  else some (match outcome with
    | .running => .pending
    | .completed => .completed
    | .failed => .failed
    | .interrupted => .interrupted)

theorem no_early_completion (edge head : Bool) :
    foldedSenderOutcome edge head .running ≠ some .completed := by
  cases edge <;> cases head <;> decide

theorem wrong_head_has_no_notification (edge : Bool) (outcome : TurnOutcome) :
    foldedSenderOutcome edge false outcome = none := by
  cases edge <;> rfl

theorem wrong_caller_has_no_notification (head : Bool) (outcome : TurnOutcome) :
    foldedSenderOutcome false head outcome = none := by
  cases head <;> rfl

theorem consumed_input_inherits_completion :
    foldedSenderOutcome true true .completed = some .completed := by rfl

end AgentMessageDelivery
