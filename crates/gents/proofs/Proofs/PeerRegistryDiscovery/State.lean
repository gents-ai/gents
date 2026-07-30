import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card
import Mathlib.Data.Finset.Image

namespace PeerRegistryDiscovery

abbrev Did := String
abbrev PeerId := String
abbrev Nonce := String

structure RegistryEntry where
  peer : PeerId
  did : Did
  live : Bool
  deriving DecidableEq, Repr

abbrev Registry := Finset RegistryEntry

def deriveRegistryDesired (self : PeerId) (reg : Registry) : Finset PeerId :=
  (reg.filter (fun e => e.live = true ∧ e.peer ≠ self)).image RegistryEntry.peer

structure DiscoveryState where
  self : PeerId
  registry : Registry
  operatorDesired : Finset PeerId
  registryDesired : Finset PeerId
  consumedNonces : Finset Nonce
  deriving DecidableEq

namespace DiscoveryState

def effectiveDesired (s : DiscoveryState) : Finset PeerId :=
  s.operatorDesired ∪ s.registryDesired

def settled (s : DiscoveryState) : Prop :=
  s.registryDesired = deriveRegistryDesired s.self s.registry

instance (s : DiscoveryState) : Decidable s.settled := by
  unfold settled
  infer_instance

def settle (s : DiscoveryState) : DiscoveryState :=
  { s with registryDesired := deriveRegistryDesired s.self s.registry }

theorem settle_settled (s : DiscoveryState) : (settle s).settled := by
  unfold settled settle
  rfl

theorem settle_preserves_operator (s : DiscoveryState) :
    (settle s).operatorDesired = s.operatorDesired := rfl

end DiscoveryState

end PeerRegistryDiscovery
