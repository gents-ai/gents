import Proofs.AgentSession
import Lean

namespace Conformance.SessionReplyPreview
open Lean AgentSession

private def states : List RequestState :=
  [.pending, .claimed, .processing, .completed, .failed, .superseded, .dead, .interrupted]

private def caseJson (state : RequestState) (doc request : Nat) (reply : Option String) : Json :=
  let before : Observation :=
    { activity := 3
      preview := some "Question"
      latest := some ⟨1, 2, .processing⟩
      replyPreview := some "Previous reply" }
  let current : RequestObservation := ⟨doc, request, state⟩
  let after := observeReply before current reply
  Json.mkObj [("state", toJson state.toDefraDB), ("doc", toJson doc), ("request", toJson request),
    ("reply", toJson reply), ("preview", toJson after.replyPreview)]

def casesJson : String := (toJson <|
  states.flatMap fun state =>
    [1, 9].flatMap fun doc =>
      [2, 9].flatMap fun request =>
        [none, some "Visible answer", some (String.mk (List.replicate 260 '界'))].map fun reply =>
          caseJson state doc request reply).compress

end Conformance.SessionReplyPreview
