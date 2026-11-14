import Proofs.Store.Identity.Operations
import Proofs.Conformance.Family

namespace Conformance.Identity

open Lean Identity.Operations

structure ReceiverGrantInput where
  senderGranted : Bool
  delivered : Bool
  deriving ToJson

@[conformance_family]
def receiverGrant : Family ReceiverGrantInput Bool where
  inputs := [(false, false), (true, false), (true, true)].map fun (senderGranted, delivered) =>
    { senderGranted, delivered }
  model := fun i => receiverHasGrant i.senderGranted i.delivered

end Conformance.Identity
