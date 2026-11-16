import Lean

namespace ClientShell.QuestionReply

def source (person : Bool) (requestSource evidenceSource : Option String)
    (observedQuestion : Bool) : Option String :=
  if !person then none
  else match requestSource, evidenceSource with
    | some request, some evidence =>
        if !request.isEmpty && request == evidence then some request else none
    | some request, none =>
        if !request.isEmpty && observedQuestion then some request else none
    | _, _ => none

example : source true (some "q") (some "q") false = some "q" := by decide
example : source true (some "q") (some "other") true = none := by decide
example : source true (some "q") none true = some "q" := by decide
example : source true (some "q") none false = none := by decide
example : source false (some "q") (some "q") true = none := by decide

end ClientShell.QuestionReply
