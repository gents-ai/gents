import Proofs.CanonicalOutput.Execution.Properties
import Proofs.Request.CrossActorAuthorization

namespace CanonicalOutput.Execution.AdmissionHandoff

open Request.CrossActorAuthorization

/-- The handoff is ephemeral; it retains the original task context, not a new
source of authorization. The claimed effect identifies the executing agent. -/
structure Context where
  request : Nat
  session : Nat
  call : Nat
  generation : Generation
  selectedAgent : String
  effect : Effect

inductive Decision where
  | run | skip | terminate
  deriving BEq, DecidableEq, Repr

inductive Phase where
  | initial | waiting | permitted | closed
  deriving BEq, DecidableEq, Repr

inductive Stop where
  | cancelled | leaseLost | shutdown | deadline | channelClosed
  deriving BEq, DecidableEq, Repr

/-- An interrupted asynchronous operation may already have committed. Unknown
is an observation, not evidence that its transaction or effect was aborted. -/
inductive Observation where
  | notStarted | unknown | committed | rejected
  deriving BEq, DecidableEq, Repr

inductive Action where
  | offer
  | beginDispatch
  | linearizeDispatch
  | observeDispatch
  | rejectDispatch
  | beginEffect
  | observeEffect
  | reply (call : Nat) (agent : String) (decision : Decision)
  | stop (reason : Stop)
  | time (now : Nat)
  | replaceGeneration (generation : Generation)
  | invoke
  deriving Repr

structure State where
  world : World
  context : Context
  phase : Phase := .initial
  acceptedReplies : Nat := 0
  decision : Option Decision := none
  dispatchObservation : Observation := .notStarted
  nativeDispatchCommitted : Option Bool := none
  effectObservation : Observation := .notStarted
  invoked : Bool := false

/-- Publication and the selected agent's already-claimed turn are premises;
native policy and lease checks remain in the existing dispatch owner. -/
def offerable (s : State) : Bool :=
  s.world.requestId == s.context.request && s.world.sessionId == s.context.session &&
  activeTurn s.context.effect.lifecycle &&
  s.context.effect.committedBy == some s.context.selectedAgent &&
  toolDispatchPublicationValid s.world s.context.generation s.context.call &&
  toolReadyToDispatch s.world s.context.call

/-- The effectful hook can begin native dispatch and hook-owned effects before
it replies. Dispatch linearizes through the existing native owner against the
current world, and its result can be observed after the future was dropped.
`replaceGeneration` represents an externally observed replacement; it does
not model how ownership is replaced. `invoke` is the later guest call: it has
no additional lease check in this model. The biased caller must observe a stop
before invoking to exclude the invocation; elapsed time alone does not
establish that observation. -/
def step (s : State) : Action → State
  | .offer =>
      if s.phase = .initial ∧ offerable s = true then { s with phase := .waiting } else s
  | .beginDispatch =>
      if s.phase = .waiting ∧ s.dispatchObservation = .notStarted then
        { s with dispatchObservation := .unknown }
      else s
  | .linearizeDispatch =>
      if s.dispatchObservation = .unknown ∧ s.nativeDispatchCommitted = none then
        match dispatch s.world s.context.generation ⟨s.context.call, true, true⟩ with
        | .ok world => { s with world := world, nativeDispatchCommitted := some true }
        | .error _ => { s with nativeDispatchCommitted := some false }
      else s
  | .observeDispatch =>
      if s.dispatchObservation = .unknown then
        match s.nativeDispatchCommitted with
        | none => s
        | some true => { s with dispatchObservation := .committed }
        | some false => { s with dispatchObservation := .rejected }
      else s
  | .rejectDispatch =>
      if s.dispatchObservation = .unknown ∧ s.nativeDispatchCommitted = none then
        { s with nativeDispatchCommitted := some false }
      else s
  | .beginEffect =>
      if s.phase = .waiting ∧ s.dispatchObservation = .committed ∧
          s.effectObservation = .notStarted then
        { s with effectObservation := .unknown }
      else s
  | .observeEffect =>
      if s.effectObservation = .unknown then { s with effectObservation := .committed } else s
  | .reply call agent decision =>
      if s.phase = .waiting ∧ call = s.context.call ∧ agent = s.context.selectedAgent ∧
          (decision ≠ .run ∨ s.dispatchObservation = .committed) then
        let replied := { s with acceptedReplies := s.acceptedReplies + 1, decision := some decision }
        { replied with phase := if decision == .run then .permitted else .closed }
      else s
  | .stop _ => { s with phase := .closed }
  | .time now => { s with world := { s.world with lease := { s.world.lease with now := now } } }
  | .replaceGeneration generation =>
      { s with world := { s.world with lease := { s.world.lease with lease := .active generation 5 20 } } }
  | .invoke =>
      if s.phase = .permitted then
        { s with phase := .closed, invoked := true }
      else s

def run (s : State) (actions : List Action) : State := actions.foldl step s

theorem stop_blocks_late_invoke (s : State) (reason : Stop) :
    (step (step s (.stop reason)) .invoke).invoked = s.invoked := by
  simp [step]

theorem stop_blocks_late_reply (s : State) (reason : Stop) (call : Nat)
    (agent : String) (decision : Decision) :
    (step (step s (.stop reason)) (.reply call agent decision)).acceptedReplies =
      s.acceptedReplies := by
  simp [step]

theorem closed_never_accepts_reply (s : State) (call : Nat) (agent : String)
    (decision : Decision) (closed : s.phase = .closed) :
    (step s (.reply call agent decision)).acceptedReplies = s.acceptedReplies := by
  simp [step, closed]

/-- Delegation still uses the claimed parent Agent as immediate author. -/
theorem delegation_preserves_immediate_author (s : State) (agent document session targetAgent target : String)
    (causeHop targetHop : Nat) (active : activeTurn s.context.effect.lifecycle = true)
    (author : s.context.effect.committedBy = some agent) :
    outbound s.context.effect document session targetAgent target causeHop targetHop =
      some ⟨⟨document, session, targetAgent, agent, target⟩,
        CausalHop.nextHop (.crossSession causeHop) targetHop⟩ := by
  simp [outbound, active, author]

end CanonicalOutput.Execution.AdmissionHandoff
