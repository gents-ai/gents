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

/-!
## Open limitation: interrupted requests and inference-call cancellation

This composed model proves that request interruption releases scheduler state,
but it does not model `InferenceCall` as a separate state machine. The stronger
runtime claim is that every queued or running call linked to an interrupted
request is eventually persisted with `call_state = "cancelled"`. That boundary
is documented in `Proofs.Conformance.Deviations` rather than stated here as a
theorem.
-/

end ComposedState
