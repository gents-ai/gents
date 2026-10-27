import Proofs.Mailbox.Question
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.MailboxQuestionContracts
open Mailbox.Question Conformance.Contracts

private def boolJson (value : Bool) : String := if value then "true" else "false"

private def baseline : Mailbox.Item :=
  { identity :=
      { itemKey := "question"
        requesterDid := "owner"
        nodeDid := "node"
        sourceKind := .session
        sourceId := "request"
        kind := .ask }
    status := .open
    resolvedDocId := "" }

private def caseJson (policy : Policy) (skip selected text : Bool) : String :=
  "{\"version\":" ++ toString policy.version ++
  ",\"optional\":" ++ boolJson policy.optional ++
  ",\"option_count\":" ++ toString policy.optionCount ++
  ",\"free_text\":" ++ boolJson policy.freeText ++
  ",\"skip\":" ++ boolJson skip ++
  ",\"selected\":" ++ boolJson selected ++
  ",\"text\":" ++ boolJson text ++
  ",\"valid\":" ++ boolJson (valid policy) ++
  ",\"accepted\":" ++ boolJson (accepts policy skip selected text) ++
  ",\"dismissible\":" ++ boolJson ((dismiss? policy.version baseline "owner").isSome) ++ "}"

def casesJson : String := jsonArray <|
  [0, 1, 2, 3].flatMap fun version =>
    [false, true].flatMap fun optional =>
      [0, 1, 2, 4, 5].flatMap fun optionCount =>
        [false, true].flatMap fun freeText =>
          [false, true].flatMap fun skip =>
            [false, true].flatMap fun selected =>
              [false, true].map fun text =>
                caseJson { version, optional, optionCount, freeText } skip selected text

end Conformance.MailboxQuestionContracts
