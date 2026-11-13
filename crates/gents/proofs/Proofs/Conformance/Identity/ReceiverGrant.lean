import Proofs.Store.Identity.Operations
import Proofs.Conformance.Family

namespace Conformance.Identity

open Lean Identity.Operations

structure ReceiverGrantInput where
  receiverHostsNode : Bool
  senderGranted : Bool
  delivered : Bool
  deriving ToJson

/-- The receiver a native node can observe is the one transport publication
names, the receiving node's own key, so every input hosts the node. -/
@[conformance_family]
def receiverGrant : Family ReceiverGrantInput Bool where
  inputs := [(false, false), (true, false), (true, true)].map fun (senderGranted, delivered) =>
    { receiverHostsNode := true, senderGranted, delivered }
  model := fun i => receiverReads i.receiverHostsNode i.senderGranted i.delivered

structure NativeReadInput where
  bypass : Bool
  documentAcp : Bool
  deriving ToJson

/-- `Household.readObserved` over its whole domain, for the native tests that
observe a hosting node key or another actor after a revocation. -/
@[conformance_family]
def nativeRead : Family NativeReadInput Bool where
  inputs := [(false, false), (false, true), (true, false), (true, true)].map
    fun (bypass, documentAcp) => { bypass, documentAcp }
  model := fun i => _root_.Identity.Household.readObserved i.bypass i.documentAcp

end Conformance.Identity
