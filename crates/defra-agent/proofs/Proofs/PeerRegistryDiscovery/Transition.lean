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
needs its issuer and whether its signature verifies.

SCOPE: the token carries no *invitee* field. It authorizes WHETHER a join may
happen (a live member sanctioned it), not WHICH identity is admitted — see
`Transition.join`. This matches the trusted-fleet TOFU model where the admitted
entry is self-asserted; binding the inserted entry to a token-authorized invitee
would require adding an `invitee` field here (and to the wire `InviteToken`) and
is deliberately out of scope. -/
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

/-- The registry holds a live member OTHER than `self`. The TOFU bootstrap arm is
only legitimate when this is false: an empty registry — or one holding only this
node's own self-registration row — has no peer trust set to check an invite
against. Without this guard `tofuBootstrap` would be a free flag a caller could
set to bypass `isMember` on a populated registry, draining the membership check.
Self is identified by `peer` here (the model's convention, matching
`deriveRegistryDesired`'s self-exclusion); the Rust `decide_join_admission`
excludes self by DID — the same intent under the per-node peer↔did identity. -/
def hasPeerMember (reg : Registry) (self : PeerId) : Prop :=
  ∃ e ∈ reg, e.live = true ∧ e.peer ≠ self

instance (reg : Registry) (self : PeerId) : Decidable (hasPeerMember reg self) := by
  unfold hasPeerMember
  infer_instance

/-- Admission predicate on a join. Authorized iff the token's signature verifies
AND either (registry-checked arm) the issuer is a live member, or (TOFU bootstrap
arm) the bootstrap flag is set AND the registry has no peer members besides
`self`. The bootstrap conjunct with `¬ hasPeerMember` is what stops the flag from
bypassing `isMember` on a populated registry. At runtime the bootstrap bit is not
attacker-chosen: it is computed from registry state by the conformance-fenced
`decide_join_admission` (Rust), which takes the bootstrap arm only when no peer
members exist. -/
def signedByMember (tok : Token) (reg : Registry) (self : PeerId) (tofuBootstrap : Bool) : Prop :=
  tok.sigValid = true ∧
    (isMember tok.issuer reg ∨ (tofuBootstrap = true ∧ ¬ hasPeerMember reg self))

instance (tok : Token) (reg : Registry) (self : PeerId) (tofuBootstrap : Bool) :
    Decidable (signedByMember tok reg self tofuBootstrap) := by
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
  member (or TOFU bootstrap). This is the fenced authorization gate.

  The admitted entry `e` is unconstrained relative to `tok.issuer`: the gate
  fences WHETHER a join is admitted (a member sanctioned it), not WHICH identity
  `e` carries. Under the trusted-fleet TOFU model `e` is self-asserted by the
  joining node; binding `e.did` to a token-authorized invitee is intentionally
  not modeled (the `Token` has no invitee field — see its docstring). -/
  | join {pre post : DiscoveryState} (tok : Token) (e : RegistryEntry) (tofuBootstrap : Bool) :
      signedByMember tok pre.registry pre.self tofuBootstrap →
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
