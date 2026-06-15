import Proofs.PeerRegistryDiscovery.State

/-!
# Peer Registry Discovery — Transitions

The discovery step is the only mutator of the network-owned partition. It
materializes network-owned desired rows by running the pure derivation
(`deriveMaterializable`); it never touches the operator partition. Membership
edits (an admin admitting or revoking a member), join-request submissions, and
operator edits are also modeled so the ownership and retraction theorems are
quantified over *every* legal step.

A `join` transition (a node redeeming a single-use invite) is gated by a
signature predicate so "non-member invite rejected" is a theorem, not prose.

## Why these predicates

The membership world is signed end-to-end. Five predicates name the trust
conditions the derivation depends on, so that the obligation theorems in later
tasks can quantify over them rather than re-spell the conjunctions:

* `validNetwork` — the network record's admin self-attestation verifies.
* `adminSignedMembership` — a membership row is signed by *this* network's admin.
* `memberSignedEndpoint` — an endpoint announcement is self-signed and fresh.
* `admittedMember` — a DID has an active, admin-signed membership in a valid
  network (the predicate-level twin of `deriveMaterializable`'s `Nonempty` arm).
* `materializableEndpoint` — an endpoint that the derivation materializes.
-/

namespace PeerRegistryDiscovery

/-! ## Membership / endpoint trust predicates -/

/-- The network record's admin self-attestation verifies. A network whose admin
signature does not verify can authorize nothing. -/
def validNetwork (n : Network) : Prop := n.adminSigValid = true

instance (n : Network) : Decidable (validNetwork n) := by
  unfold validNetwork
  infer_instance

/-- A membership row is authoritative for `n`: signed by `n`'s admin (matching
`signedBy` and a verifying `adminSigValid`) and scoped to `n`'s network id. -/
def adminSignedMembership (m : Membership) (n : Network) : Prop :=
  m.adminSigValid = true ∧ m.signedBy = n.adminDid ∧ m.networkId = n.networkId

instance (m : Membership) (n : Network) : Decidable (adminSignedMembership m n) := by
  unfold adminSignedMembership
  infer_instance

/-- An endpoint announcement is self-signed by the announcing node and fresh
(heartbeat within window). An endpoint failing either bit is ignored. -/
def memberSignedEndpoint (ep : Endpoint) : Prop :=
  ep.memberSigValid = true ∧ ep.fresh = true

instance (ep : Endpoint) : Decidable (memberSignedEndpoint ep) := by
  unfold memberSignedEndpoint
  infer_instance

/-- `did` is an admitted member of `s`'s network: the network is valid and there
is an active membership row for `did` signed by the network's admin. This is the
predicate-level twin of the `Nonempty`-of-filtered-memberships arm of
`deriveMaterializable`. -/
def admittedMember (did : Did) (s : DiscoveryState) : Prop :=
  validNetwork s.network ∧
    ∃ m ∈ s.memberships, m.memberDid = did ∧ m.active = true ∧
      adminSignedMembership m s.network

instance (did : Did) (s : DiscoveryState) : Decidable (admittedMember did s) := by
  unfold admittedMember adminSignedMembership validNetwork
  infer_instance

/-- An endpoint the derivation materializes: its announcing DID is an admitted
member, the announcement is member-signed and fresh, and it is not self. -/
def materializableEndpoint (ep : Endpoint) (s : DiscoveryState) : Prop :=
  admittedMember ep.did s ∧ memberSignedEndpoint ep ∧ ep.peer ≠ s.self

instance (ep : Endpoint) (s : DiscoveryState) : Decidable (materializableEndpoint ep s) := by
  unfold materializableEndpoint admittedMember adminSignedMembership validNetwork
    memberSignedEndpoint
  infer_instance

/-! ## Bridge: predicate ↔ derivation

`deriveMaterializable` is the executable derivation; `materializableEndpoint` is
its declarative spec. This lemma links them so the obligation theorems can be
stated against the predicate and discharged against the derivation. -/

/-- A peer is in the derived network-owned set iff some endpoint announcing it is
`materializableEndpoint`. The proof reconciles `admittedMember`'s
`∃ m ∈ memberships, …` with `deriveMaterializable`'s `(memberships.filter …).Nonempty`:
membership in the filtered finset is exactly that existential. -/
theorem mem_deriveMaterializable {s : DiscoveryState} {p : PeerId} :
    p ∈ deriveMaterializable s ↔
      ∃ ep ∈ s.endpoints, materializableEndpoint ep s ∧ ep.peer = p := by
  unfold deriveMaterializable
  rw [Finset.mem_image]
  constructor
  · rintro ⟨ep, hep_filt, hpeer⟩
    rw [Finset.mem_filter] at hep_filt
    obtain ⟨hep_mem, hfresh, hsig, hself, hadmin, hne⟩ := hep_filt
    refine ⟨ep, hep_mem, ?_, hpeer⟩
    refine ⟨⟨hadmin, ?_⟩, ⟨hsig, hfresh⟩, hself⟩
    obtain ⟨m, hm_filt⟩ := hne
    rw [Finset.mem_filter] at hm_filt
    obtain ⟨hm_mem, hmdid, hmactive, hmadminsig, hmsignedby, hmnet⟩ := hm_filt
    exact ⟨m, hm_mem, hmdid, hmactive, hmadminsig, hmsignedby, hmnet⟩
  · rintro ⟨ep, hep_mem, ⟨⟨hadmin, m, hm_mem, hmdid, hmactive, hmadminsig, hmsignedby, hmnet⟩,
      ⟨hsig, hfresh⟩, hne⟩, hpeer⟩
    refine ⟨ep, ?_, hpeer⟩
    rw [Finset.mem_filter]
    refine ⟨hep_mem, hfresh, hsig, hne, hadmin, ?_⟩
    refine ⟨m, ?_⟩
    rw [Finset.mem_filter]
    exact ⟨hm_mem, hmdid, hmactive, hmadminsig, hmsignedby, hmnet⟩

/-! ## Signed-invite authorization (abstract)

A join redeems a single-use invite. The model needs the token's issuer (to check
it is an admitted member), whether its signature verifies, and its nonce (to make
the freshness window single-use). -/

/-- Opaque invite token. Concretely a v2 signed `InviteToken`; the model only
needs its issuer, whether its signature verifies, and its single-use nonce.

SCOPE: the token carries no *invitee* field. It authorizes WHETHER a join may
happen (an admitted member sanctioned it), not WHICH identity is admitted. -/
structure Token where
  issuer : Did
  /-- The signature over the canonical payload verifies against `issuer`'s
  `did:key`. A forged/absent signature is `false`. -/
  sigValid : Bool
  /-- The token's single-use nonce. Two redemptions of the *same* physical invite
  carry the same nonce; join admission rejects a nonce already in
  `consumedNonces`, which makes the freshness window single-use. -/
  nonce : Nonce
  deriving DecidableEq, Repr

/-- Admission predicate on a join. Authorized iff the token's signature verifies
AND either (membership-checked arm) the issuer is an admitted member, or (TOFU
bootstrap arm) the bootstrap flag is set, the network has no memberships yet, AND
the token's issuer is the network admin.

The bootstrap guard is `s.memberships = ∅`: an empty membership set has no peer
trust set to check an invite against, so a one-time admin-issued invite seeds the
network. The conjunct stops `tofuBootstrap` from being a free flag that bypasses
the membership check on a populated network — once a single membership row exists,
the TOFU arm is dead and admission falls back to `admittedMember`.

Bootstrap is admin-only: the `tok.issuer = s.network.adminDid` conjunct means only
the network admin can seed an empty network; a non-admin cannot use the bootstrap
flag to inject the first membership-less join.

FORWARD-NOTE (cut 5): the membership reconciler's join-gate (the successor to the
registry-era `decide_join_admission` in `discovery.rs`, which keyed bootstrap off
`!any_members`) must mirror THIS guard — `s.memberships = ∅` AND issuer = admin —
not the registry-era `!any_members` shape it supersedes. -/
def signedByMember (tok : Token) (s : DiscoveryState) (tofuBootstrap : Bool) : Prop :=
  tok.sigValid = true ∧
    (admittedMember tok.issuer s ∨
      (tofuBootstrap = true ∧ s.memberships = ∅ ∧ tok.issuer = s.network.adminDid))

instance (tok : Token) (s : DiscoveryState) (tofuBootstrap : Bool) :
    Decidable (signedByMember tok s tofuBootstrap) := by
  unfold signedByMember admittedMember adminSignedMembership validNetwork
  infer_instance

/-- Full join admission. Wraps `signedByMember` with the single-use freshness
check: the token's nonce must not already have been redeemed. -/
def admitsJoin (s : DiscoveryState) (tok : Token) (tofuBootstrap : Bool) : Prop :=
  signedByMember tok s tofuBootstrap ∧ tok.nonce ∉ s.consumedNonces

instance (s : DiscoveryState) (tok : Token) (tofuBootstrap : Bool) :
    Decidable (admitsJoin s tok tofuBootstrap) := by
  unfold admitsJoin signedByMember admittedMember adminSignedMembership validNetwork
  infer_instance

/-! ## State mutators -/

/-- Run the derivation: materialize network-owned rows. Operator rows untouched. -/
def deriveStep (s : DiscoveryState) : DiscoveryState :=
  { s with registryDesired := deriveMaterializable s }

/-- Key-based upsert over `(networkId, memberDid)`: drop any existing membership
for the same key, then insert `m`. This preserves `wellFormed` by construction —
after the upsert there is at most one row per key — so every membership mutator
(admit, revoke) routes through it. -/
def upsertMembership (s : DiscoveryState) (m : Membership) : DiscoveryState :=
  { s with memberships :=
      insert m (s.memberships.filter (fun x =>
        ¬ (x.networkId = m.networkId ∧ x.memberDid = m.memberDid))) }

/-- A candidate submits a join request. Pure additive insert into `requests`;
nothing else moves. A request alone never admits anyone. -/
def submitRequestState (s : DiscoveryState) (req : JoinRequest) : DiscoveryState :=
  { s with requests := insert req s.requests }

/-- The admin approves a request by writing a signed membership row. -/
def approveMembershipState (s : DiscoveryState) (m : Membership) : DiscoveryState :=
  upsertMembership s m

/-- The admin revokes a membership by writing a tombstone row (`active = false`)
for the same key. Routed through the same upsert, so the live row is replaced. -/
def revokeState (s : DiscoveryState) (tomb : Membership) : DiscoveryState :=
  upsertMembership s tomb

/-- A join redeems a single-use token: consumes the nonce, mutates NOTHING else.
Consuming the nonce here (not in a separate step) makes the join atomic w.r.t.
replay — the very transition that admits `tok` burns `tok.nonce`, so
`admitsJoin post tok` can never hold again. The admitted endpoint is a
self-asserted announcement (TOFU model); the join itself only burns the nonce. -/
def joinState (s : DiscoveryState) (tok : Token) : DiscoveryState :=
  { s with consumedNonces := insert tok.nonce s.consumedNonces }

/-- Operator writes its own desired set. Only the operator partition moves. -/
def operatorWriteState (s : DiscoveryState) (d : Finset PeerId) : DiscoveryState :=
  { s with operatorDesired := d }

/-! ## Transition relation -/

inductive Transition : DiscoveryState → DiscoveryState → Prop where
  /-- The discovery reconciler materializes network-owned rows. -/
  | derive {pre post : DiscoveryState} :
      post = deriveStep pre →
      Transition pre post
  /-- A node redeems an invite. ENABLED ONLY when the invite is signed by an
  admitted member (or TOFU bootstrap) and its nonce is unconsumed. This is the
  fenced authorization gate; the join burns the nonce. -/
  | join {pre post : DiscoveryState} (tok : Token) (tofuBootstrap : Bool) :
      admitsJoin pre tok tofuBootstrap →
      post = joinState pre tok →
      Transition pre post
  /-- A **reciprocal** join: wires a return replicator (`--reciprocal`). The
  reciprocal flag only changes WHAT gets wired (the return leg, outside this
  state), never WHETHER the join is admitted — so its precondition is the SAME
  `admitsJoin` gate and it applies the SAME `joinState` mutator. At this layer a
  reciprocal join is state-indistinguishable from a plain join, by design: it
  cannot skip the admission gate. -/
  | reciprocalJoin {pre post : DiscoveryState} (tok : Token) (tofuBootstrap : Bool) :
      admitsJoin pre tok tofuBootstrap →
      post = joinState pre tok →
      Transition pre post
  /-- A candidate submits a join request. -/
  | submitRequest {pre post : DiscoveryState} (req : JoinRequest) :
      post = submitRequestState pre req →
      Transition pre post
  /-- The admin approves a pending, well-signed request whose target network is
  this one, by writing an active admin-signed membership for the candidate. -/
  | approveRequest {pre post : DiscoveryState} (req : JoinRequest) (m : Membership) :
      req ∈ pre.requests →
      req.reqSigValid = true →
      req.networkId = pre.network.networkId →
      adminSignedMembership m pre.network →
      m.memberDid = req.candidateDid →
      m.active = true →
      post = approveMembershipState pre m →
      Transition pre post
  /-- The admin revokes a membership by writing an admin-signed tombstone
  (`active = false`) for the key. -/
  | revoke {pre post : DiscoveryState} (tomb : Membership) :
      adminSignedMembership tomb pre.network →
      tomb.active = false →
      post = revokeState pre tomb →
      Transition pre post
  /-- The operator edits its own desired set. -/
  | operatorWrite {pre post : DiscoveryState} (d : Finset PeerId) :
      post = operatorWriteState pre d →
      Transition pre post

end PeerRegistryDiscovery
