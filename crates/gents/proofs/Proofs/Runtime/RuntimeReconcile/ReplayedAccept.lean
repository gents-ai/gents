import Proofs.Runtime.RuntimeReconcile.Executable
import Proofs.Session.Request.CrossActorAuthorization

namespace RuntimeState

/-- The request lifecycle the runtime's accepted set stands for. Production
keeps no accepted set: it reads the durable `AgentRequest.lifecycle_state`,
and the claim transaction moves a request from `pending` to `claimed`. -/
def acceptedLifecycle (s : RuntimeState) (requestId : RequestId) : RequestState :=
  if requestId ∈ s.accepted then .claimed else .pending

/-- A replayed accept is a claim of a request whose lifecycle has left
`pending`. The reconcile model refuses it, and the claim transaction leaves
such a request as it is whatever the claim decides, since
`Request.CrossActorAuthorization.applyDecision` changes only a pending
request; so no request is accepted twice.
`Conformance.CrossActorAuthorization.signedClientClaim` replays this decision
through its `existing_effect_unchanged` case, a second claim of a claimed
request. -/
theorem replayed_accept_is_a_claim_of_a_claimed_request
    {pre post : RuntimeState} {process : ProcessState} {sessionId : SessionId}
    {requestId : RequestId} {requested : AgentDid}
    (h_step : step? pre (.acceptRequest process sessionId requestId requested) = some post)
    (agent : String) (committedBy : Option String)
    (decision : Request.CrossActorAuthorization.Decision) :
    acceptedLifecycle pre requestId = .pending ∧
      acceptedLifecycle post requestId = .claimed ∧
      step? post (.acceptRequest process sessionId requestId requested) = none ∧
      Request.CrossActorAuthorization.applyDecision
          { lifecycle := acceptedLifecycle post requestId, committedBy } agent decision =
        { lifecycle := .claimed, committedBy } := by
  have h_replay := accept_step_replay_rejected h_step
  simp only [step?] at h_step
  split at h_step
  · rename_i h_can
    cases h_step
    refine ⟨by simp [acceptedLifecycle, h_can.1], by simp [acceptedLifecycle], h_replay, ?_⟩
    simp only [acceptedLifecycle, Finset.mem_insert, true_or, if_true]
    cases decision <;> rfl
  · cases h_step

end RuntimeState
