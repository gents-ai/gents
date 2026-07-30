import Proofs.PeerRegistryDiscovery.State

namespace PeerRegistryDiscovery

structure Token where
  issuer : Did
  sigValid : Bool
  nonce : Nonce
  deriving DecidableEq, Repr

def isMember (issuer : Did) (reg : Registry) : Prop :=
  ∃ e ∈ reg, e.did = issuer ∧ e.live = true

instance (issuer : Did) (reg : Registry) : Decidable (isMember issuer reg) := by
  unfold isMember
  infer_instance

def hasPeerMember (reg : Registry) (self : PeerId) : Prop :=
  ∃ e ∈ reg, e.live = true ∧ e.peer ≠ self

instance (reg : Registry) (self : PeerId) : Decidable (hasPeerMember reg self) := by
  unfold hasPeerMember
  infer_instance

def signedByMember (tok : Token) (reg : Registry) (self : PeerId) (tofuBootstrap : Bool) : Prop :=
  tok.sigValid = true ∧
    (isMember tok.issuer reg ∨ (tofuBootstrap = true ∧ ¬ hasPeerMember reg self))

instance (tok : Token) (reg : Registry) (self : PeerId) (tofuBootstrap : Bool) :
    Decidable (signedByMember tok reg self tofuBootstrap) := by
  unfold signedByMember
  infer_instance

def admitsJoin (s : DiscoveryState) (tok : Token) (tofuBootstrap : Bool) : Prop :=
  signedByMember tok s.registry s.self tofuBootstrap ∧ tok.nonce ∉ s.consumedNonces

instance (s : DiscoveryState) (tok : Token) (tofuBootstrap : Bool) :
    Decidable (admitsJoin s tok tofuBootstrap) := by
  unfold admitsJoin
  infer_instance

def deriveStep (s : DiscoveryState) : DiscoveryState :=
  { s with registryDesired := deriveRegistryDesired s.self s.registry }

def joinState (s : DiscoveryState) (e : RegistryEntry) (tok : Token) : DiscoveryState :=
  { s with registry := insert e s.registry
         , consumedNonces := insert tok.nonce s.consumedNonces }

def removeEntryState (s : DiscoveryState) (e : RegistryEntry) : DiscoveryState :=
  { s with registry := s.registry.erase e }

def operatorWriteState (s : DiscoveryState) (d : Finset PeerId) : DiscoveryState :=
  { s with operatorDesired := d }

inductive Transition : DiscoveryState → DiscoveryState → Prop where
  | derive {pre post : DiscoveryState} :
      post = deriveStep pre →
      Transition pre post
  | join {pre post : DiscoveryState} (tok : Token) (e : RegistryEntry) (tofuBootstrap : Bool) :
      admitsJoin pre tok tofuBootstrap →
      post = joinState pre e tok →
      Transition pre post
  | reciprocalJoin {pre post : DiscoveryState} (tok : Token) (e : RegistryEntry) (tofuBootstrap : Bool) :
      admitsJoin pre tok tofuBootstrap →
      post = joinState pre e tok →
      Transition pre post
  | removeEntry {pre post : DiscoveryState} (e : RegistryEntry) :
      post = removeEntryState pre e →
      Transition pre post
  | operatorWrite {pre post : DiscoveryState} (d : Finset PeerId) :
      post = operatorWriteState pre d →
      Transition pre post

end PeerRegistryDiscovery
