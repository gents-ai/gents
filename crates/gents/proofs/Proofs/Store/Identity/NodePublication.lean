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

/-- The runtime's periodic writes of its own peer records once it serves: the
registry heartbeat upserts the Node's `PeerRegistry` row with its addresses and
status, the endpoint heartbeat upserts its signed endpoint binding there, and
the directory projection upserts and sweeps the `NodeDirectoryEntry` rows that
describe the Node and its Agents. Each is a Node-run peer publication: the
projector grants the runtime, and for the registry every reader, access to
the row it writes. -/
inductive Periodic where
  | registryHeartbeat
  | endpointHeartbeat
  | directory
  deriving DecidableEq, Repr

/-- What a periodic publication's loop does once the runtime serves. -/
inductive Loop where
  /-- It writes its records at start and on every interval. -/
  | beats
  /-- It writes nothing for as long as it runs. -/
  | idle
  deriving DecidableEq, Repr

/-- Decision (gents#3831): a periodic publication is a peer publication, so it
obeys `publication` and nothing else. On a home that refuses peer publication
its loop stays idle from its start: it writes no record, so nothing is granted
to the Node or to every reader, and no later beat has a record to update. -/
def periodic (provider : Provider) (_ : Periodic) : Loop :=
  match publication provider with
  | .proceeds => .beats
  | .awaitsTransportGrants => .idle

/-- Whether a periodic loop ever writes, by creating or updating, a peer record. -/
def writesPeerRecord (provider : Provider) (loop : Periodic) : Bool :=
  periodic provider loop == .beats

theorem periodic_publication_follows_publication (provider : Provider) (loop : Periodic) :
    (periodic provider loop = .beats) ↔ publication provider = .proceeds := by
  cases provider <;> simp [periodic, publication]

theorem no_heartbeat_writes_a_peer_record_on_a_vera_home (loop : Periodic) :
    writesPeerRecord .embeddedVera loop = false := by
  cases loop <;> rfl

theorem heartbeats_on_the_node_store_beat_as_before (loop : Periodic) :
    periodic .nodeStore loop = .beats ∧ writesPeerRecord .nodeStore loop = true := by
  cases loop <;> exact ⟨rfl, rfl⟩

end Identity.NodePublication
