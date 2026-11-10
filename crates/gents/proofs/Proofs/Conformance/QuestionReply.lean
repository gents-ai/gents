import Proofs.ClientShell.QuestionReply
import Lean

namespace Conformance.QuestionReply
open Lean

def cases : List Json :=
  [ ("evidence", true, some "question", some "question", false),
    ("mismatched_evidence", true, some "question", some "other", true),
    ("legacy_observed", true, some "question", none, true),
    ("legacy_missing", true, some "question", none, false),
    ("empty", true, some "", none, true),
    ("no_request_source", true, none, some "question", true),
    ("automated", false, some "question", some "question", true) ].map
    fun (name, person, requestSource, evidenceSource, observedQuestion) => Json.mkObj
      [("name", toJson name), ("operation", toJson "question_reply_source"),
       ("person", toJson person), ("request_source", toJson requestSource),
       ("evidence_source", toJson evidenceSource),
       ("observed_question", toJson observedQuestion),
       ("source", toJson (ClientShell.QuestionReply.source person requestSource evidenceSource observedQuestion))]

end Conformance.QuestionReply
