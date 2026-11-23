import Proofs.Store.DocumentAcpChoice

/-! Writes the runtime makes as its Node to publish peer records, which
grant other runtimes access to documents (gents#3831).

Each device group is owned by the configured account. A live membership
check verifies the group owner and the publishing device at one certified
revision. If the check fails, publication returns before writing. -/

namespace Identity.NodePublication

open DocumentAcpChoice (Provider)

inductive Publication where
  | proceeds
  /-- Refused with a typed error before anything is written. -/
  | awaitsTransportGrants
  deriving DecidableEq, Repr

/-- Before preflight binds an account group, a Vera home serves without peer
publication. Once bound, each publication checks the device's membership. -/
def publication (provider : Provider) (admitted : Bool := false) : Publication :=
  match provider with
  | .nodeStore => .proceeds
  | .embeddedVera => if admitted then .proceeds else .awaitsTransportGrants

theorem an_unadmitted_vera_home_publishes_no_peer_records :
    publication .embeddedVera false = .awaitsTransportGrants := rfl

theorem an_admitted_vera_home_can_publish :
    publication .embeddedVera true = .proceeds := rfl

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

/-- The enrollment root: the signed `Network` row the runtime creates as its
Node when it starts serving, naming the network an enrollment offer invites a
device into. Creating it is a Node-run peer publication: the projector grants
the runtime `reader` of the row and parents it to the Node, so every identity
that reads the Node reads it. Serving reads nothing else from it; enrollment
offers, operator decisions and the pending list are its only consumers. -/
inductive Root where
  /-- Serving creates or reloads the row and issues offers in its network. -/
  | created
  /-- Serving starts without one and writes no `Network` row. -/
  | absent
  deriving DecidableEq, Repr

/-- Decision (gents#3831): the enrollment root is a peer publication, so it
obeys `publication`. A home that refuses peer publication serves without it:
the home keeps no bookkeeping copy, since nothing it serves reads one. -/
def enrollmentRoot (provider : Provider) : Root :=
  match publication provider with
  | .proceeds => .created
  | .awaitsTransportGrants => .absent

/-- A request that enrolls or pairs a device through the enrollment root. -/
inductive EnrollmentRequest where
  /-- Minting an enrollment offer, for the runtime's status or a tool. -/
  | offer
  /-- An operator approving, denying or revoking an enrollment request. -/
  | decision
  /-- An operator listing the enrollment requests that wait for a decision. -/
  | pending
  deriving DecidableEq, Repr

inductive Answer where
  | served
  /-- Refused before anything is read or written, saying that peer networking
  is not available on this home yet. -/
  | peerNetworkingUnavailable
  deriving DecidableEq, Repr

/-- Decision (gents#3831): without its root a home answers every enrollment
request with the same refusal, rather than reporting a missing network. -/
def enrollment (provider : Provider) (_ : EnrollmentRequest) : Answer :=
  match enrollmentRoot provider with
  | .created => .served
  | .absent => .peerNetworkingUnavailable

/-- Whether serving on a home publishes a peer-readable `Network` row. -/
def publishesNetworkRow (provider : Provider) : Bool :=
  enrollmentRoot provider == .created

theorem enrollment_root_follows_publication (provider : Provider) :
    (enrollmentRoot provider = .created) ↔ publication provider = .proceeds := by
  cases provider <;> simp [enrollmentRoot, publication]

theorem a_vera_home_publishes_no_network_row :
    publishesNetworkRow .embeddedVera = false := rfl

theorem a_vera_home_refuses_every_enrollment_request (request : EnrollmentRequest) :
    enrollment .embeddedVera request = .peerNetworkingUnavailable := by
  cases request <;> rfl

theorem a_home_on_the_node_store_enrolls_as_before (request : EnrollmentRequest) :
    enrollmentRoot .nodeStore = .created ∧ publishesNetworkRow .nodeStore = true ∧
      enrollment .nodeStore request = .served := by
  cases request <;> exact ⟨rfl, rfl, rfl⟩

end Identity.NodePublication
