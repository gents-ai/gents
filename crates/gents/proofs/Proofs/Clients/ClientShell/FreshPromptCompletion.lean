import Proofs.PromptAssembly.CurrentInput
import Proofs.CanonicalOutput.Execution
import Proofs.Request.Executable

namespace ClientShell.FreshPromptCompletion

open CanonicalOutput CanonicalOutput.Execution RequestContext

structure Witness where
  before : World
  generation : Generation
  closing : Segment
  message : MessageEnvelope
  published : World
  processing : RequestContext
  completed : RequestContext
  freshInput : PromptAssembly.CurrentInput.publishesAuthoredInput false = true
  exactRequest : message.header.request = some before.requestId
  exactSession : message.header.session = before.sessionId
  userPrompt : message.header.role = .user
  authoredPublication :
    publishAuthored before generation closing message = .ok published
  requestCompletion : step? processing .finish = some completed

/-- Ordered fresh-prompt publication followed by the ordinary successful
completion transition leaves the exact request prompt durably present when the
request is Completed. The witness's ordered adapter contract is bound by the
daemon's awaited AuthoredInputReady write and the native completion regression;
this theorem does not claim that either model owner alone enforces ordering. -/
theorem completed_fresh_request_has_authored_prompt (witness : Witness) :
    authoredPublicationPresent witness.published witness.closing witness.message = true ∧
      witness.completed.state = .completed ∧
      witness.message.header.request = some witness.before.requestId ∧
      witness.message.header.session = witness.before.sessionId := by
  have hpub := authored_publication_is_atomic_and_headed witness.before witness.published
    witness.generation witness.closing witness.message witness.authoredPublication
  have hstate : witness.completed.state = .completed := by
    have hstate := congrArg (Option.map RequestContext.state) witness.requestCompletion
    simp [step?] at hstate
    exact hstate.2.symm
  exact ⟨hpub.1, hstate, witness.exactRequest, witness.exactSession⟩

end ClientShell.FreshPromptCompletion
