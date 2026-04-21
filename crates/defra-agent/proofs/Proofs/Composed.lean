import Proofs.Process
import Proofs.Request
import Proofs.Persistence

/-!
# Cross-Layer Composition

Combines the process, request, and persistence layers into a single
single-execution composed state.
-/

/-- The composed state of all single-execution layers. -/
structure ComposedState where
  process : ProcessState
  request : RequestContext
  deriving Repr

namespace ComposedState

/-- A composed transition is valid only when cross-layer guards hold. -/
inductive Transition : ComposedState → ComposedState → Prop where
  | process_step {pre post : ComposedState} :
      ProcessState.Transition pre.process post.process →
      post.request = pre.request →
      Transition pre post
  | request_step {pre post : ComposedState} :
      RequestContext.Transition pre.request post.request →
      post.process = pre.process →
      (pre.request.state = .pending → pre.process.acceptsWork) →
      Transition pre post
  | persistence_step {pre post : ComposedState} (policy : PersistenceState.FailurePolicy)
      (nextPersistence : PersistenceState) :
      PersistenceState.Transition policy pre.request.persistence nextPersistence →
      post.request = { pre.request with persistence := nextPersistence } →
      post.process = pre.process →
      Transition pre post

/-- A trace is a sequence of valid composed transitions. -/
inductive Trace : ComposedState → ComposedState → Prop where
  | refl {s : ComposedState} : Trace s s
  | step {s₁ s₂ s₃ : ComposedState} :
      Transition s₁ s₂ → Trace s₂ s₃ → Trace s₁ s₃

/-- The initial state of the system. -/
def initial : ComposedState :=
  { process := .uninitialized
  , request :=
    { state := .pending
    , origin := .interactive
    , backend := { val := "initial-backend" }
    , admission := .released
    , deadline := 0
    , claimTime := 0
    , currentTime := 0
    , retryCount := 0
    , maxRetries := 3
    , progressSeq := 0
    , messageSeq := 0
    , isLatest := true
    , persistence := .uncommitted
    }
  }

/-- Cross-layer safety: an interrupted request's companion inference calls
    will be persisted as cancelled.

    Runtime implementation (Task 9): `AdmissionPermit::mark_interrupted`
    sets the permit's terminal to `call_state = "cancelled"` /
    `failure_reason = "Cancelled"`, which `Drop` persists to the
    `InferenceCall` row. The daemon's interrupt arm fires this via the
    `inference_token` threaded through `AdmissionCallContext`.

    This theorem is a placeholder until `InferenceCall` is modeled in
    Lean (future work). Once that model exists, the theorem body should
    state: "if `r.state = .interrupted`, then for every
    `c : InferenceCall` with `c.request_id = r.request_id` and
    `c.state ∈ {queued, running}`, there exists a sequence of
    `InferenceCall` steps ending in `c.state = cancelled`." -/
theorem interrupted_request_cancels_calls
    (_r : RequestContext)
    (_h_interrupted : _r.state = .interrupted) :
    True := by
  trivial

end ComposedState
