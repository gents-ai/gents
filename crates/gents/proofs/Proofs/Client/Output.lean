import Proofs.Client.Replacement
import Proofs.StreamingResponse.Properties

/-!
# Client execution/output composition

Execution status and output visibility are projected together here, but keep
their distinct owners: request lifecycle determines `head`; canonical immutable
facts determine `output`. This makes the three independence rules executable
without recreating a response lifecycle.
-/

namespace Client

structure OutputProjection where
  head : ClientHeadProjection
  output : StreamingResponse.View
  deriving DecidableEq, Repr

def projectOutput (attempt : AttemptView)
    (observation : StreamingResponse.Observation) : OutputProjection :=
  { head := projectHead attempt
  , output := StreamingResponse.project observation
  }

def outputMissing : StreamingResponse.View → Bool
  | .absent | .loading | .settling _ => true
  | _ => false

def activeRequestState : RequestState → Bool
  | .claimed | .processing | .inputRequired => true
  | _ => false

theorem active_request_projects_running {attempt : AttemptView}
    (hactive : activeRequestState attempt.request.lifecycleState = true)
    (hnotSuperseded : attempt.request.isSuperseded = false) :
    (projectHead attempt).turnState = .running := by
  obtain ⟨⟨state, superseded⟩⟩ := attempt
  cases state <;> simp_all [activeRequestState, projectHead, deriveAttempt]

/-- Missing or not-yet-reconstructible output does not demote terminal request
execution. The output premise is computed by the shared canonical projection. -/
theorem missing_output_cannot_demote_completed {attempt : AttemptView}
    {observation : StreamingResponse.Observation}
    (hcompleted : attempt.request.lifecycleState = .completed)
    (hnotSuperseded : attempt.request.isSuperseded = false)
    (hmissing : outputMissing (projectOutput attempt observation).output = true) :
    (projectOutput attempt observation).head.turnState = .completed ∧
      outputMissing (projectOutput attempt observation).output = true := by
  constructor
  · obtain ⟨⟨state, superseded⟩⟩ := attempt
    simp_all [projectOutput, projectHead, deriveAttempt]
  · exact hmissing

/-- A fully reconstructed Complete message remains an output fact. It cannot
terminalize a request whose lifecycle owner still reports active execution. -/
theorem complete_message_cannot_terminalize_active_request {attempt : AttemptView}
    {observation : StreamingResponse.Observation}
    {message : CanonicalOutput.MessageEnvelope}
    {native : CanonicalOutput.ReconstructedMessage}
    (hactive : activeRequestState attempt.request.lifecycleState = true)
    (hnotSuperseded : attempt.request.isSuperseded = false)
    (hcomplete : message.header.outcome = .complete)
    (houtput : (projectOutput attempt observation).output =
      .published message native) :
    (projectOutput attempt observation).head.turnState = .running ∧
      (projectOutput attempt observation).head.isTerminal = false ∧
      message.header.outcome = .complete ∧
      (projectOutput attempt observation).output = .published message native := by
  have hrunning := active_request_projects_running hactive hnotSuperseded
  exact ⟨hrunning, by simp [projectOutput, ClientHeadProjection.isTerminal,
    hrunning, ClientTurnState.isTerminal], hcomplete, houtput⟩

/-- Partial is an output outcome, not a second request failure state. -/
theorem partial_message_is_not_failure_status {attempt : AttemptView}
    {observation : StreamingResponse.Observation}
    {message : CanonicalOutput.MessageEnvelope}
    {native : CanonicalOutput.ReconstructedMessage}
    (hactive : activeRequestState attempt.request.lifecycleState = true)
    (hnotSuperseded : attempt.request.isSuperseded = false)
    (hpartial : message.header.outcome = .partial)
    (houtput : (projectOutput attempt observation).output =
      .published message native) :
    (projectOutput attempt observation).head.turnState = .running ∧
      (projectOutput attempt observation).head.turnState ≠ .failed ∧
      message.header.outcome = .partial ∧
      (projectOutput attempt observation).output = .published message native := by
  have hrunning := active_request_projects_running hactive hnotSuperseded
  refine ⟨hrunning, ?_, hpartial, houtput⟩
  change (projectHead attempt).turnState ≠ .failed
  simp [hrunning]

end Client
