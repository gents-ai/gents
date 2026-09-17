import Proofs.Mailbox.Notification
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.MailboxNotificationContracts
open Mailbox.Notification Conformance.Contracts

def outcomeName : WriteOutcome → String
  | .created => "created"
  | .reused => "reused"
  | .updated => "updated"

def casesJson : String := jsonArray <|
  [Mode.event, Mode.condition].flatMap fun mode =>
    [false, true].flatMap fun hasOpen =>
      [false, true].map fun same =>
        "{\"mode\":" ++ jsonString (if mode = .event then "event" else "condition") ++
        ",\"open_exists\":" ++ (if hasOpen then "true" else "false") ++
        ",\"same_content\":" ++ (if same then "true" else "false") ++
        ",\"outcome\":" ++ jsonString (outcomeName (decideWrite mode hasOpen same)) ++ "}"

end Conformance.MailboxNotificationContracts
