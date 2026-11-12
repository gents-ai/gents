import Proofs.Automation.Mailbox.Notification
import Proofs.Conformance.Family

namespace Mailbox.Notification

deriving instance Lean.ToJson for Mode, WriteOutcome

attribute [conformance_function] decideWrite

end Mailbox.Notification

namespace Conformance.MailboxNotificationContracts
open Lean Mailbox.Notification

structure StampInput where
  mode : Mode
  openExists : Bool
  sameContent : Bool
  deriving ToJson

structure StampOutput where
  outcome : WriteOutcome
  afterDismiss : WriteOutcome
  deriving ToJson

@[conformance_family]
def stamp : Family StampInput StampOutput where
  inputs :=
    [Mode.event, Mode.condition].flatMap fun mode =>
      [false, true].flatMap fun openExists =>
        [false, true].map fun sameContent => { mode, openExists, sameContent }
  model := fun i =>
    { outcome := decideWrite i.mode i.openExists i.sameContent
      afterDismiss := decideWrite i.mode false i.sameContent }

end Conformance.MailboxNotificationContracts
