import Proofs.Store.DocumentAcpChoice

/-! Writes the runtime makes as its Node to publish peer records, which
grant other runtimes access to documents (gents#3831).

Enrollment publishes the local runtime's Node and peer records and grants
the recipient read and parent relationships on them; an accepted replicated
row from another account grants the receiving runtime its document-scoped
transport relationships. On a home whose document access control runs on an
embedded Vera node those grants would be registered under the Node, which
Vera has no way to tie to the account that asked. External premise
(defradb.rs#2980, not merged): DefraDB has no provider-side grant of
transport relationships for a Vera home yet. -/

namespace Identity.NodePublication

open DocumentAcpChoice (Provider)

inductive Publication where
  | proceeds
  /-- Refused with a typed error before anything is written. -/
  | awaitsTransportGrants
  deriving DecidableEq, Repr

/-- Decision (gents#3831): until the provider grants transport relationships
itself, a Vera home refuses every Node-run peer publication rather than
registering it under the Node. This blocks enrolling a device, publishing the
peer registry and the directory, and so every multi-device and pairing flow on
a Vera home; a home on the node's own store is unchanged. -/
def publication : Provider → Publication
  | .nodeStore => .proceeds
  | .embeddedVera => .awaitsTransportGrants

theorem a_vera_home_publishes_no_peer_records :
    publication .embeddedVera = .awaitsTransportGrants := rfl

theorem a_home_on_the_node_store_publishes_as_before :
    publication .nodeStore = .proceeds := rfl

end Identity.NodePublication
