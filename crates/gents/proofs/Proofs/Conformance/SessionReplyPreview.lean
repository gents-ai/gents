import Proofs.Session.AgentSession
import Proofs.Conformance.Functions.Session
import Proofs.Conformance.Family

namespace Conformance.SessionReplyPreview
open Lean AgentSession

private def states : List RequestState :=
  [.pending, .claimed, .processing, .completed, .failed, .superseded, .dead, .interrupted]

structure Input where
  state : RequestState
  doc : Nat
  request : Nat
  reply : Option String
  deriving ToJson

def before : Observation :=
  { activity := 3
    preview := some "Question"
    latest := some ⟨1, 2, .processing⟩
    replyPreview := some "Previous reply" }

@[conformance_family]
def observation : Family Input (Option String) where
  inputs :=
    states.flatMap fun state =>
      [1, 9].flatMap fun doc =>
        [2, 9].flatMap fun request =>
          [none, some "Visible answer", some (String.mk (List.replicate 260 '界'))].map fun reply =>
            { state, doc, request, reply }
  model := fun i => (observeReply before ⟨i.doc, i.request, i.state⟩ i.reply).replyPreview

end Conformance.SessionReplyPreview
