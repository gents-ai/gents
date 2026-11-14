import Proofs.Clients.ClientShell.QuestionReply
import Proofs.Conformance.Family
import Lean

namespace Conformance.QuestionReply
open Lean

structure QuestionInput where
  name : String
  operation : String
  person : Bool
  requestSource : Option String
  evidenceSource : Option String
  observedQuestion : Bool
  deriving ToJson

structure QuestionOutput where
  source : Option String
  deriving ToJson

@[conformance_family]
def source : Family QuestionInput QuestionOutput where
  inputs :=
    [ ("evidence", true, some "question", some "question", false),
      ("mismatched_evidence", true, some "question", some "other", true),
      ("legacy_observed", true, some "question", none, true),
      ("legacy_missing", true, some "question", none, false),
      ("empty", true, some "", none, true),
      ("no_request_source", true, none, some "question", true),
      ("automated", false, some "question", some "question", true) ].map
      fun (name, person, requestSource, evidenceSource, observedQuestion) =>
        { name, operation := "question_reply_source", person, requestSource, evidenceSource
          observedQuestion }
  model := fun i =>
    { source := ClientShell.QuestionReply.source i.person i.requestSource i.evidenceSource
        i.observedQuestion }

end Conformance.QuestionReply
