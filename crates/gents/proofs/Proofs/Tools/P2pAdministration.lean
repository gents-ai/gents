/-! The P2P administration tool acts only as the running node: every command
except help requires the node identity the embedded node itself runs as,
before it reads options. A peer lookup then requires a transport peer ID. -/

namespace P2pAdministration

inductive Actor where
  | absent
  | node
  | foreign
  deriving DecidableEq, Repr

inductive Command where
  | help
  | status
  | networkGet
  deriving DecidableEq, Repr

/-- External premise: the transport adapter accepts a peer's public key form
and rejects a multiaddress such as `/ip4/127.0.0.1/tcp/4001`. -/
inductive Peer where
  | transportId
  | multiaddress
  deriving DecidableEq, Repr

inductive Outcome where
  | answered
  | identityUnavailable
  | foreignActor
  | malformedPeer
  deriving DecidableEq, Repr

def admit (actor : Actor) (command : Command) (peer : Peer) : Outcome :=
  if command = .help then .answered
  else match actor with
    | .absent => .identityUnavailable
    | .foreign => .foreignActor
    | .node => if command = .networkGet ∧ peer = .multiaddress then .malformedPeer else .answered

theorem p2p_rejects_a_foreign_actor_and_malformed_peers (command : Command) (peer : Peer)
    (administers : command ≠ .help) :
    admit .foreign command peer = .foreignActor ∧
    admit .node .networkGet .multiaddress = .malformedPeer := by
  simp [admit, administers]

end P2pAdministration
