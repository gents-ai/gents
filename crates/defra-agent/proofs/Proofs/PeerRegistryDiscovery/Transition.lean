import Proofs.PeerRegistryDiscovery.State

/-!
# Peer Registry Discovery — Transitions

The discovery step is the only mutator of the registry-owned partition. It
materializes registry-owned desired rows by running the pure derivation; it
never touches the operator partition. Registry edits (a member appearing, a
member staling/leaving) and operator edits are also modeled so the ownership
and retraction theorems are quantified over *every* legal step.

A `Join` transition (a node entering the registry's trust set) is gated by a
signature predicate so "non-member invite rejected" is a theorem, not prose.
-/

namespace PeerRegistryDiscovery

/-! ## Signed-invite authorization (abstract) -/

/-- Opaque invite token. Concretely a v2 signed `InviteToken`; the model only
needs its issuer and whether its signature verifies. -/
structure Token where
  issuer : Did
  /-- The signature over the canonical payload verifies against `issuer`'s
  `did:key`. A forged/absent signature is `false`. -/
  sigValid : Bool
  deriving DecidableEq, Repr

/-- `issuer` is a live member of the registry. This is the registry-checked
arm of the join policy. -/
def isMember (issuer : Did) (reg : Registry) : Prop :=
  ∃ e ∈ reg, e.did = issuer ∧ e.live = true

instance (issuer : Did) (reg : Registry) : Decidable (isMember issuer reg) := by
  unfold isMember
  infer_instance

/-- Admission predicate on a join. A join is authorized iff the token's
signature verifies AND either (registry-checked arm) the issuer is a live
member, or (TOFU bootstrap arm) the first-join flag holds — the operator
handed the token out-of-band and there is no registry yet to check against. -/
def signedByMember (tok : Token) (reg : Registry) (tofuBootstrap : Bool) : Prop :=
  tok.sigValid = true ∧ (tofuBootstrap = true ∨ isMember tok.issuer reg)

instance (tok : Token) (reg : Registry) (tofuBootstrap : Bool) :
    Decidable (signedByMember tok reg tofuBootstrap) := by
  unfold signedByMember
  infer_instance

/-! ## State mutators -/

/-- Run the derivation: materialize registry-owned rows. Operator rows untouched. -/
def deriveStep (s : DiscoveryState) : DiscoveryState :=
  { s with registryDesired := deriveRegistryDesired s.self s.registry }

/-- A member joins: its self-registered row enters the registry. Gated by
`signedByMember` in the transition relation below. -/
def joinState (s : DiscoveryState) (e : RegistryEntry) : DiscoveryState :=
  { s with registry := insert e s.registry }

/-- A registry entry is removed or staled (heartbeat lapsed / deregistered).
Staling is modeled as removal of the live row; a stale row contributes nothing
to the derivation, exactly like an absent one. -/
def removeEntryState (s : DiscoveryState) (e : RegistryEntry) : DiscoveryState :=
  { s with registry := s.registry.erase e }

/-- Operator writes its own desired set. Only the operator partition moves. -/
def operatorWriteState (s : DiscoveryState) (d : Finset PeerId) : DiscoveryState :=
  { s with operatorDesired := d }

/-! ## Transition relation -/

inductive Transition : DiscoveryState → DiscoveryState → Prop where
  /-- The discovery reconciler materializes registry-owned rows. -/
  | derive {pre post : DiscoveryState} :
      post = deriveStep pre →
      Transition pre post
  /-- A node joins the trust set. ENABLED ONLY when the invite is signed by a
  member (or TOFU bootstrap). This is the fenced authorization gate. -/
  | join {pre post : DiscoveryState} (tok : Token) (e : RegistryEntry) (tofuBootstrap : Bool) :
      signedByMember tok pre.registry tofuBootstrap →
      post = joinState pre e →
      Transition pre post
  /-- A registry entry stales or is removed. -/
  | removeEntry {pre post : DiscoveryState} (e : RegistryEntry) :
      post = removeEntryState pre e →
      Transition pre post
  /-- The operator edits its own desired set. -/
  | operatorWrite {pre post : DiscoveryState} (d : Finset PeerId) :
      post = operatorWriteState pre d →
      Transition pre post

end PeerRegistryDiscovery
