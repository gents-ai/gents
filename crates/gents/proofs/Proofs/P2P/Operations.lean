/-! The peer-registry and pairing reads and the registry writes every client
calls. The CLI used to derive these answers itself while an Agent's `p2p` tool
returned the stored rows, so the two disagreed about whether a peer is online.
One handler now derives them for both. -/

namespace PeerOperations

/-- A registered peer is online when its row says so and its heartbeat is
within the staleness window. The stored `status` alone is what a peer last
wrote: a peer that stopped without deregistering still says `online`. -/
def peerOnline (reportsOnline heartbeatFresh : Bool) : Bool :=
  reportsOnline && heartbeatFresh

theorem a_stale_heartbeat_is_offline (reportsOnline : Bool) :
    peerOnline reportsOnline false = false := by
  cases reportsOnline <;> rfl

/-- A pairing replicates when it names replicator addresses and the applied
observation holds every one of them. -/
def pairingReplicating (hasAddresses allAddressesApplied : Bool) : Bool :=
  hasAddresses && allAddressesApplied

/-- A pairing whose template pushes is subscribed when it replicates. A
pairing with no template, or one this node does not know, is subscribed when
it names collections and the applied observation holds every one of them. -/
def pairingSubscribed (templatePushes replicating hasCollections allCollectionsApplied : Bool) :
    Bool :=
  if templatePushes then replicating else hasCollections && allCollectionsApplied

/-- A pairing is connected when one of its transport peers is; a pairing with
no transport address is matched by its own peer ID. -/
def pairingConnected (hasTransportPeers transportPeerConnected ownPeerConnected : Bool) : Bool :=
  if hasTransportPeers then transportPeerConnected else ownPeerConnected

theorem an_empty_pairing_is_never_subscribed (templatePushes applied : Bool) :
    pairingSubscribed templatePushes (pairingReplicating false applied) false applied = false := by
  cases templatePushes <;> cases applied <;> rfl

/-- Why a registry write was refused. -/
inductive RegistryRefusal where
  /-- The runtime reported no P2P peer ID: it is stopped or P2P is off. -/
  | peerIdUnknown
  /-- An offered template is not one the node knows. -/
  | unknownTemplate
  deriving DecidableEq, Repr

inductive RegistryRecovery where
  /-- Read the P2P status, which shows whether the runtime and P2P run. -/
  | status
  /-- List the templates a node may offer. -/
  | listTemplates
  deriving DecidableEq, Repr

def RegistryRefusal.recovery : RegistryRefusal → RegistryRecovery
  | .peerIdUnknown => .status
  | .unknownTemplate => .listTemplates

end PeerOperations
