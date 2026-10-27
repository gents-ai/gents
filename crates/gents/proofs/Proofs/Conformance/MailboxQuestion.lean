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

private def waitsJson (policy : Policy) : String := jsonArray <|
  [false, true].flatMap fun isOpen =>
    [false, true].flatMap fun sameParent =>
      [false, true].map fun completed =>
        "{\"open\":" ++ boolJson isOpen ++
        ",\"same_parent\":" ++ boolJson sameParent ++
        ",\"completed\":" ++ boolJson completed ++
        ",\"waiting\":" ++ boolJson (awaitingAnswer policy isOpen sameParent completed) ++ "}"

private def presentationJson (policy : Policy) (multiSelect requestedPrompt nonempty : Bool)
    (order : Option (List Nat)) : String :=
  let hasPrompt := requestedPrompt && policy.optionCount > 0
  "{\"multi_select\":" ++ boolJson multiSelect ++
  ",\"has_prompt\":" ++ boolJson hasPrompt ++
  ",\"nonempty_prompt\":" ++ boolJson nonempty ++
  ",\"order\":" ++ (match order with
    | none => "null"
    | some ranks => jsonArray (ranks.map toString)) ++
  ",\"valid\":" ++ boolJson (presentationValid policy multiSelect hasPrompt nonempty order) ++ "}"

private def presentationsJson (policy : Policy) : String :=
  let ranks := List.range policy.optionCount
  jsonArray [
    presentationJson policy false false true none,
    presentationJson policy false true true none,
    presentationJson policy false true false none,
    presentationJson policy true true true none,
    presentationJson policy true false true (some ranks),
    presentationJson policy true false true (some ranks.reverse),
    presentationJson policy true false true (some (0 :: ranks)),
    presentationJson policy true false true (some [policy.optionCount]),
    presentationJson policy true false true (some (ranks.drop 1)),
    presentationJson policy false false true (some ranks)]

private def versionJson (version : Option Nat) : String :=
  match version with
  | none => "null"
  | some version => toString version

private def receiptVersionsJson (policy : Policy) : String := jsonArray <|
  [none, some 1, some 2, some 3, some 4].map fun version =>
    "{\"version\":" ++ versionJson version ++
    ",\"current\":" ++ boolJson (valid policy && receiptVersionCurrent policy.version version) ++ "}"

private def runtimesJson (policy : Policy) : String :=
  let replyVersion := if policy.version ≥ 3 then some policy.version else none
  jsonArray <| [none, some 1, some 2, some 3, some 4].map fun version =>
    "{\"version\":" ++ versionJson version ++
    ",\"reply_version\":" ++ versionJson replyVersion ++
    ",\"supported\":" ++ boolJson (runtimeSupports replyVersion version) ++ "}"

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
  ",\"dismissible\":" ++ boolJson ((dismiss? policy.version baseline "owner").isSome) ++
  ",\"waits\":" ++ waitsJson policy ++ ",\"presentations\":" ++ presentationsJson policy ++ ",\"receipt_versions\":" ++ receiptVersionsJson policy ++ ",\"runtimes\":" ++ runtimesJson policy ++ "}"

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
