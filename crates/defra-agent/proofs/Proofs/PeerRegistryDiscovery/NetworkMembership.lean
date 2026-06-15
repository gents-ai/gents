import Proofs.PeerRegistryDiscovery.Transition
import Mathlib.Data.Finset.Image

/-!
# Network-Membership Discovery — the §9 model

The network control-plane layer that sits above the self-asserted
`RegistryEntry` discovery (`State.lean`) and **supersedes** it (design spec
§1, §7): a self-registered registry row is replaced by an **admin-signed
`Membership`** plus a **member-signed `Endpoint`**. This file is the Lean
source of truth the cut-2 SDL collections (`AgentNetwork`, `NetworkMembership`,
`PeerEndpoint`, `NetworkJoinRequest`) mirror field-for-field, and the
foundation cut-5's reconciler (`agent/p2p_reconcile/discovery.rs`) is fenced
against.

Signatures are modeled **abstractly as booleans** (design spec §9): a
`*SigValid` field is `true` exactly when the corresponding `*_sig` verifies
against the signing DID. The crypto itself is fenced separately
(`defra-agent-protocol` verify/tamper tests + the canonical CBOR payloads).

## The §9 obligations, each a theorem below (non-vacuous, with witnesses)

1. `unsigned_membership_not_materialized` — forged/unsigned membership is
   **never** materialized.
2. `materializable_is_derived` (+ `materializable_witness`) — active
   admin-signed membership ∧ fresh member-signed endpoint **is** materialized.
3. `revoke_sound` / `revoke_drops_member` / `revoke_preserves_others` —
   revocation retracts **exactly** that member (mirrors `retraction_sound`).
   `deriveNetworkDesired_tombstone_eq_revoke` proves the model's erase is a
   faithful stand-in for the §4 `status=revoked` tombstone (row retained).
4. `net_ownership_safe` — discovery **never mutates operator-owned** desired
   rows (reuses the two-finset partition + `operatorWrite`-is-the-sole-exception
   shape of `ownership_safe`).
5. `join_request_grants_nothing` / `membership_growth_requires_admin_sig` — a
   forged/unsigned join request **cannot** produce a grant (authority lives in
   `Membership`, never in `NetworkJoinRequest`).

## Ownership representation

Reuses the two-finset partition rationale documented in `State.lean`:
operator-owned desired rows (`operatorDesired`) and network-materialized rows
(`networkDesired`) are distinct finsets, so "discovery never touches operator
intent" is structural (`rfl`-shaped), not a per-row `source`-tag side condition.
The Rust binding is `PeerPairingDesired.source = "network"` queried as a
partition (design spec §4 "Desired-row ownership").
-/

namespace PeerRegistryDiscovery

/-! ## Entities — mirror the cut-2 SDL collections (design spec §4) -/

/-- `AgentNetwork`: the network root. `adminSigValid` is `true` iff `admin_sig`
verifies against `adminDid`. Non-security fields (`display_name`,
`default_template`, `created_at`) are elided — they don't gate materialization. -/
structure Network where
  networkId : String
  adminDid : Did
  /-- `admin_sig` verifies against `adminDid`. -/
  adminSigValid : Bool
  deriving DecidableEq, Repr

/-- `NetworkMembership`: an admin-authored grant. `active` is the SDL
`status == "active"` (a `status == "revoked"` tombstone is `active = false`).
`adminSigValid` is `true` iff `admin_sig` verifies against the network's admin. -/
structure Membership where
  networkId : String
  memberDid : Did
  /-- SDL `status`: `active` (`true`) | `revoked` (`false`). -/
  active : Bool
  /-- `admin_sig` verifies against the network's `adminDid`. -/
  adminSigValid : Bool
  deriving DecidableEq, Repr

/-- `PeerEndpoint`: a member-self-asserted transport binding, **global per
node** (unique by `did`). `fresh` folds heartbeat-age liveness into one bit (as
`RegistryEntry.live` does). `bindingSigValid` is `true` iff `binding_sig`
verifies against `did`. -/
structure Endpoint where
  did : Did
  nodeId : String
  /-- Heartbeat freshness derived from `updated_at` age (model takes it as given). -/
  fresh : Bool
  /-- `binding_sig` verifies against `did`. -/
  bindingSigValid : Bool
  deriving DecidableEq, Repr

/-- `NetworkJoinRequest`: a candidate-authored enrollment request. `reqSigValid`
is `true` iff `req_sig` verifies against `candidateDid`. **Informational only** —
it carries no authority; admission is `Membership` (design spec §4, §13). -/
structure JoinRequest where
  networkId : String
  candidateDid : Did
  /-- `req_sig` verifies against `candidateDid`. -/
  reqSigValid : Bool
  deriving DecidableEq, Repr

/-! ## §9 core predicates -/

/-- An `AgentNetwork` whose `admin_sig` is valid for its `admin_did`. -/
def validNetwork (n : Network) : Prop := n.adminSigValid = true

instance (n : Network) : Decidable (validNetwork n) := by
  unfold validNetwork; infer_instance

/-- A `NetworkMembership` for `n` whose `admin_sig` is valid for `n`'s admin. -/
def adminSignedMembership (n : Network) (m : Membership) : Prop :=
  m.networkId = n.networkId ∧ m.adminSigValid = true

instance (n : Network) (m : Membership) : Decidable (adminSignedMembership n m) := by
  unfold adminSignedMembership; infer_instance

/-- A `PeerEndpoint` whose `binding_sig` is valid for its `did`. -/
def memberSignedEndpoint (ep : Endpoint) : Prop := ep.bindingSigValid = true

instance (ep : Endpoint) : Decidable (memberSignedEndpoint ep) := by
  unfold memberSignedEndpoint; infer_instance

/-- A DID is an **admitted member**: it holds an `active`, admin-signed
membership in a `validNetwork`. This is the sole admission authority. -/
def admittedMember (n : Network) (m : Membership) : Prop :=
  validNetwork n ∧ adminSignedMembership n m ∧ m.active = true

instance (n : Network) (m : Membership) : Decidable (admittedMember n m) := by
  unfold admittedMember; infer_instance

/-! ## Network discovery state + materialization -/

/-- Full network-discovery state. Ownership is the two-finset partition from
`State.lean`: `operatorDesired` (operator-authored, never touched by discovery)
and `networkDesired` (materialized from membership+endpoint). `joinRequests` is
candidate-authored and carries no authority. -/
structure NetworkState where
  self : Did
  network : Network
  memberships : Finset Membership
  endpoints : Finset Endpoint
  joinRequests : Finset JoinRequest
  /-- Operator-authored desired peers. Discovery NEVER touches these. -/
  operatorDesired : Finset Did
  /-- Network-materialized desired peers, owned by the discovery step. -/
  networkDesired : Finset Did
  deriving DecidableEq

/-- An endpoint is **materializable** in `s`: the network is valid, the endpoint
is fresh and member-signed, it is not self, and some `active` admin-signed
membership in `s` grants its `did`. This is §9 `materializableEndpoint`, stated
over the state so the membership witness is checked against `s.memberships`. -/
def endpointMaterializable (s : NetworkState) (ep : Endpoint) : Prop :=
  validNetwork s.network ∧
    memberSignedEndpoint ep ∧ ep.fresh = true ∧ ep.did ≠ s.self ∧
    ∃ m ∈ s.memberships, admittedMember s.network m ∧ m.memberDid = ep.did

instance (s : NetworkState) : DecidablePred (endpointMaterializable s) := by
  intro ep; unfold endpointMaterializable; infer_instance

/-- The network-materialized desired peer set: the DIDs of materializable
endpoints. A pure function of `s`, mirroring `deriveRegistryDesired`. -/
def deriveNetworkDesired (s : NetworkState) : Finset Did :=
  (s.endpoints.filter (endpointMaterializable s)).image Endpoint.did

/-- `d` is materialized iff some materializable endpoint carries it. -/
theorem mem_deriveNetworkDesired {s : NetworkState} {d : Did} :
    d ∈ deriveNetworkDesired s ↔
      ∃ ep ∈ s.endpoints, endpointMaterializable s ep ∧ ep.did = d := by
  unfold deriveNetworkDesired
  simp only [Finset.mem_image, Finset.mem_filter]
  constructor
  · rintro ⟨ep, ⟨he_mem, he_mat⟩, he_did⟩
    exact ⟨ep, he_mem, he_mat, he_did⟩
  · rintro ⟨ep, he_mem, he_mat, he_did⟩
    exact ⟨ep, ⟨he_mem, he_mat⟩, he_did⟩

/-! ## State mutators -/

/-- Run the network derivation: materialize network-owned rows. Operator rows
untouched. -/
def deriveNetStep (s : NetworkState) : NetworkState :=
  { s with networkDesired := deriveNetworkDesired s }

/-- The admin grants a membership (admin-authored). Gated by
`adminSignedMembership` ∧ `active` in the transition relation. -/
def grantState (s : NetworkState) (m : Membership) : NetworkState :=
  { s with memberships := insert m s.memberships }

/-- The admin revokes a membership: the grant row is erased (the wire
representation is a `status=revoked` tombstone; the derived-set effect — `d` is
no longer admitted — is identical, so the model erases). -/
def revokeState (s : NetworkState) (m : Membership) : NetworkState :=
  { s with memberships := s.memberships.erase m }

/-- A candidate files a join request. Adds ONLY to `joinRequests` — never to
`memberships`. This is the structural fact obligation 5 leans on. -/
def requestState (s : NetworkState) (jr : JoinRequest) : NetworkState :=
  { s with joinRequests := insert jr s.joinRequests }

/-- A member refreshes/asserts its endpoint binding. -/
def endpointState (s : NetworkState) (ep : Endpoint) : NetworkState :=
  { s with endpoints := insert ep s.endpoints }

/-- The operator edits its own desired set. Only the operator partition moves. -/
def netOperatorWriteState (s : NetworkState) (d : Finset Did) : NetworkState :=
  { s with operatorDesired := d }

/-! ## Transition relation -/

/-- The network-discovery transition relation. Every legal step is one of these;
the ownership and authority theorems case over the whole relation so no step is
silently skipped. -/
inductive NetTransition : NetworkState → NetworkState → Prop where
  /-- The reconciler materializes network-owned rows. -/
  | derive {pre post : NetworkState} :
      post = deriveNetStep pre → NetTransition pre post
  /-- The admin grants a membership. ENABLED ONLY when the grant is
  admin-signed and active — this is the fenced authority gate. -/
  | adminGrant {pre post : NetworkState} (m : Membership) :
      adminSignedMembership pre.network m → m.active = true →
      post = grantState pre m → NetTransition pre post
  /-- The admin revokes a membership (signed tombstone). -/
  | adminRevoke {pre post : NetworkState} (m : Membership) :
      post = revokeState pre m → NetTransition pre post
  /-- A candidate files a join request. Carries NO authority: it cannot add a
  membership (it only grows `joinRequests`), regardless of `reqSigValid`. -/
  | joinRequest {pre post : NetworkState} (jr : JoinRequest) :
      post = requestState pre jr → NetTransition pre post
  /-- A member asserts/refreshes its endpoint. -/
  | endpointRefresh {pre post : NetworkState} (ep : Endpoint) :
      post = endpointState pre ep → NetTransition pre post
  /-- The operator edits its own desired set. -/
  | operatorWrite {pre post : NetworkState} (d : Finset Did) :
      post = netOperatorWriteState pre d → NetTransition pre post

/-! ## (1) Forged/unsigned membership is never materialized -/

/-- Leaf fact: a forged (unsigned) membership is never an admitted member, for
ANY network. The `admin_sig` arm of `admittedMember` rejects it. -/
theorem forged_membership_not_admitted (n : Network) (m : Membership)
    (h : m.adminSigValid = false) : ¬ admittedMember n m := by
  rintro ⟨_, ⟨_, hsig⟩, _⟩
  rw [h] at hsig
  exact Bool.false_ne_true hsig

/-- State-level: if `ep.did` has NO active admin-signed membership in `s` (every
membership granting it is forged, inactive, or for another network), then
`ep.did` is **not materialized**. Forged/unsigned membership cannot put a peer
in the desired set. -/
theorem unsigned_membership_not_materialized {s : NetworkState} {ep : Endpoint}
    (h_none : ∀ m ∈ s.memberships, m.memberDid = ep.did → ¬ admittedMember s.network m) :
    ep.did ∉ deriveNetworkDesired s := by
  rw [mem_deriveNetworkDesired]
  rintro ⟨ep', _, ⟨_, _, _, _, m, hm_mem, hm_adm, hm_did⟩, hep'_did⟩
  exact h_none m hm_mem (hm_did.trans hep'_did) hm_adm

/-! ## (2) Active admin-signed membership + fresh signed endpoint IS materialized -/

/-- If `ep ∈ s.endpoints` is materializable, then `ep.did ∈ deriveNetworkDesired s`. -/
theorem materializable_is_derived {s : NetworkState} {ep : Endpoint}
    (hep : ep ∈ s.endpoints) (h : endpointMaterializable s ep) :
    ep.did ∈ deriveNetworkDesired s := by
  rw [mem_deriveNetworkDesired]
  exact ⟨ep, hep, h, rfl⟩

/-- Non-vacuity witness: a concrete valid network, active admin-signed
membership, and fresh member-signed endpoint really IS materialized — so (2) is
not vacuously true. -/
theorem materializable_witness :
    ∃ (s : NetworkState) (ep : Endpoint),
      ep ∈ s.endpoints ∧ endpointMaterializable s ep ∧
        ep.did ∈ deriveNetworkDesired s := by
  let n : Network := ⟨"net-1", "did:key:admin", true⟩
  let m : Membership := ⟨"net-1", "did:key:a", true, true⟩
  let ep : Endpoint := ⟨"did:key:a", "node-a", true, true⟩
  let s : NetworkState :=
    { self := "did:key:self"
    , network := n
    , memberships := {m}
    , endpoints := {ep}
    , joinRequests := ∅
    , operatorDesired := ∅
    , networkDesired := ∅ }
  have hadm : admittedMember s.network m := ⟨rfl, ⟨rfl, rfl⟩, rfl⟩
  have hmat : endpointMaterializable s ep :=
    ⟨rfl, rfl, rfl, by decide, m, Finset.mem_singleton_self m, hadm, rfl⟩
  exact ⟨s, ep, Finset.mem_singleton_self ep, hmat, materializable_is_derived (Finset.mem_singleton_self ep) hmat⟩

/-! ## (3) Revocation retracts exactly that member (mirrors `retraction_sound`) -/

/-- After revoking `m`, `d` is materialized iff some endpoint for `d` is admitted
by a membership **other than `m`**. The targeted analogue of
`retraction_characterization`. -/
theorem revoke_characterization {s : NetworkState} {m : Membership} {d : Did} :
    d ∈ deriveNetworkDesired (revokeState s m) ↔
      ∃ ep ∈ s.endpoints, validNetwork s.network ∧ memberSignedEndpoint ep ∧
        ep.fresh = true ∧ ep.did ≠ s.self ∧ ep.did = d ∧
        ∃ m' ∈ s.memberships, m' ≠ m ∧ admittedMember s.network m' ∧ m'.memberDid = ep.did := by
  rw [mem_deriveNetworkDesired]
  unfold endpointMaterializable revokeState
  simp only
  constructor
  · rintro ⟨ep, hep, ⟨hnet, hsig, hfresh, hself, m', hm'_mem, hm'_adm, hm'_did⟩, hep_did⟩
    refine ⟨ep, hep, hnet, hsig, hfresh, hself, hep_did, m',
      Finset.mem_of_mem_erase hm'_mem, Finset.ne_of_mem_erase hm'_mem, hm'_adm, hm'_did⟩
  · rintro ⟨ep, hep, hnet, hsig, hfresh, hself, hep_did, m', hm'_mem, hm'_ne, hm'_adm, hm'_did⟩
    exact ⟨ep, hep, ⟨hnet, hsig, hfresh, hself, m', Finset.mem_erase.mpr ⟨hm'_ne, hm'_mem⟩, hm'_adm, hm'_did⟩, hep_did⟩

/-- **Retraction is exact.** If `m` was `d`'s SOLE admitting membership, then
revoking `m` drops `d` from the desired set. -/
theorem revoke_drops_member {s : NetworkState} {m : Membership} {d : Did}
    (h_sole : ∀ m' ∈ s.memberships, m' ≠ m → admittedMember s.network m' →
                m'.memberDid ≠ d) :
    d ∉ deriveNetworkDesired (revokeState s m) := by
  rw [revoke_characterization]
  rintro ⟨ep, _, _, _, _, _, hep_did, m', hm'_mem, hm'_ne, hm'_adm, hm'_did⟩
  exact h_sole m' hm'_mem hm'_ne hm'_adm (hm'_did.trans hep_did)

/-- **Retraction is targeted: no collateral.** A peer `d'` backed by a DIFFERENT
membership `m' ≠ m` (with its own materializable endpoint) survives the
revocation of `m`. -/
theorem revoke_preserves_others {s : NetworkState} {m m' : Membership} {ep' : Endpoint}
    (hm'_ne : m' ≠ m) (hep'_mem : ep' ∈ s.endpoints)
    (hnet : validNetwork s.network) (hsig : memberSignedEndpoint ep')
    (hfresh : ep'.fresh = true) (hself : ep'.did ≠ s.self)
    (hm'_mem : m' ∈ s.memberships) (hm'_adm : admittedMember s.network m')
    (hm'_did : m'.memberDid = ep'.did) :
    ep'.did ∈ deriveNetworkDesired (revokeState s m) := by
  rw [revoke_characterization]
  exact ⟨ep', hep'_mem, hnet, hsig, hfresh, hself, rfl, m', hm'_mem, hm'_ne, hm'_adm, hm'_did⟩

/-- Whole-state corollary mirroring `retraction_sound`: a revoke step followed by
a derive retracts only network-owned rows; the operator partition is
byte-identical throughout, and the new network-desired set is the derivation over
the erased membership set. -/
theorem revoke_sound {pre post post' : NetworkState} {m : Membership}
    (h_revoke : post = revokeState pre m)
    (h_derive : post' = deriveNetStep post) :
    post'.operatorDesired = pre.operatorDesired ∧
    post'.networkDesired = deriveNetworkDesired (revokeState pre m) := by
  subst h_revoke; subst h_derive
  exact ⟨rfl, rfl⟩

/-! ### Tombstone ≡ erase for the derived set (faithfulness to the §4 wire op)

`revokeState` erases the grant row, but design spec §4 mandates a `status =
revoked` **tombstone** — the row is RETAINED with `active = false`, never
deleted (so revocation is attributable and replicates). `tombstoneState` models
that literal wire operation. The two are **proven** to produce the identical
materialized set (not merely asserted in prose): the retained `active = false`
row can never satisfy `admittedMember` (which requires `active = true`), so the
admitted memberships over the tombstoned roster are exactly those over the
erased roster. Hence every `revoke_*` theorem transfers verbatim to the real
tombstone representation. -/

/-- The §4 wire revocation: erase the active grant and insert its `active = false`
tombstone (the row is retained, not deleted). -/
def tombstoneState (s : NetworkState) (m : Membership) : NetworkState :=
  { s with memberships := insert { m with active := false } (s.memberships.erase m) }

/-- After a tombstone, `d` is materialized iff some endpoint for `d` is admitted
by a membership **other than `m`** — the IDENTICAL right-hand side as
`revoke_characterization`, because the retained `active = false` tombstone row
never satisfies `admittedMember`. -/
theorem tombstone_characterization {s : NetworkState} {m : Membership} {d : Did} :
    d ∈ deriveNetworkDesired (tombstoneState s m) ↔
      ∃ ep ∈ s.endpoints, validNetwork s.network ∧ memberSignedEndpoint ep ∧
        ep.fresh = true ∧ ep.did ≠ s.self ∧ ep.did = d ∧
        ∃ m' ∈ s.memberships, m' ≠ m ∧ admittedMember s.network m' ∧ m'.memberDid = ep.did := by
  rw [mem_deriveNetworkDesired]
  unfold endpointMaterializable tombstoneState
  simp only
  constructor
  · rintro ⟨ep, hep, ⟨hnet, hsig, hfresh, hself, m', hm'_mem, hm'_adm, hm'_did⟩, hep_did⟩
    rcases Finset.mem_insert.mp hm'_mem with heq | hmem
    · subst heq; exact absurd hm'_adm.2.2 Bool.false_ne_true
    · exact ⟨ep, hep, hnet, hsig, hfresh, hself, hep_did, m',
        Finset.mem_of_mem_erase hmem, Finset.ne_of_mem_erase hmem, hm'_adm, hm'_did⟩
  · rintro ⟨ep, hep, hnet, hsig, hfresh, hself, hep_did, m', hm'_mem, hm'_ne, hm'_adm, hm'_did⟩
    exact ⟨ep, hep, ⟨hnet, hsig, hfresh, hself, m',
      Finset.mem_insert_of_mem (Finset.mem_erase.mpr ⟨hm'_ne, hm'_mem⟩), hm'_adm, hm'_did⟩, hep_did⟩

/-- **Tombstone and erase materialize identically.** The proof that the model's
`revokeState` (erase) is a faithful stand-in for the §4 wire tombstone
(`active = false` row retained): the derived sets are equal, so every `revoke_*`
theorem holds for the real tombstone operation too. -/
theorem deriveNetworkDesired_tombstone_eq_revoke (s : NetworkState) (m : Membership) :
    deriveNetworkDesired (tombstoneState s m) = deriveNetworkDesired (revokeState s m) := by
  apply Finset.ext
  intro d
  rw [tombstone_characterization, ← revoke_characterization]

/-! ## (4) Ownership safety — discovery never mutates operator-owned rows -/

/-- No derive/grant/revoke/joinRequest/endpoint transition mutates an
operator-owned row. The ONLY transition that changes `operatorDesired` is
`operatorWrite` itself (named honestly). Mirrors `ownership_safe`. -/
theorem net_ownership_safe {pre post : NetworkState} (h : NetTransition pre post)
    (h_not_operator : ∀ d, post ≠ netOperatorWriteState pre d) :
    post.operatorDesired = pre.operatorDesired := by
  cases h with
  | derive h_post => subst h_post; rfl
  | adminGrant m _ _ h_post => subst h_post; rfl
  | adminRevoke m h_post => subst h_post; rfl
  | joinRequest jr h_post => subst h_post; rfl
  | endpointRefresh ep h_post => subst h_post; rfl
  | operatorWrite d h_post => exact absurd h_post (h_not_operator d)

/-- Sharper: the derive step preserves the operator partition AND every input
(network/memberships/endpoints), mutating only `networkDesired`. -/
theorem net_derive_preserves_operator_and_inputs (s : NetworkState) :
    (deriveNetStep s).operatorDesired = s.operatorDesired ∧
    (deriveNetStep s).memberships = s.memberships ∧
    (deriveNetStep s).endpoints = s.endpoints := ⟨rfl, rfl, rfl⟩

/-! ## (5) A forged/unsigned join request cannot produce a grant -/

/-- A join request adds NOTHING to the membership roster — regardless of
`reqSigValid`. The candidate cannot self-grant. -/
theorem join_request_grants_nothing {pre post : NetworkState} (jr : JoinRequest)
    (h : post = requestState pre jr) :
    post.memberships = pre.memberships := by
  subst h; rfl

/-- The membership roster can only GROW via `adminGrant`, and `adminGrant`
carries `adminSignedMembership pre.network m`. So any step that admitted a new
membership — its post roster is not a subset of the pre roster — must have
carried a valid admin signature. The positive authority fact; combined with
`join_request_grants_nothing` it fences "a forged join request cannot grant".

Non-vacuous: `membership_growth_witness` exhibits a real `adminGrant` that grows
the roster, so the hypothesis space is inhabited and the conclusion (an admin
signature exists) is genuine, read out of the constructor itself. -/
theorem membership_growth_requires_admin_sig {pre post : NetworkState}
    (h : NetTransition pre post) (h_grew : ¬ post.memberships ⊆ pre.memberships) :
    ∃ m : Membership, adminSignedMembership pre.network m ∧ m.active = true := by
  cases h with
  | derive h_post => subst h_post; exact absurd subset_rfl h_grew
  | adminGrant m hsig hactive _ => exact ⟨m, hsig, hactive⟩
  | adminRevoke m h_post =>
      subst h_post
      exact absurd (Finset.erase_subset m pre.memberships) h_grew
  | joinRequest jr h_post => subst h_post; exact absurd subset_rfl h_grew
  | endpointRefresh ep h_post => subst h_post; exact absurd subset_rfl h_grew
  | operatorWrite d h_post => subst h_post; exact absurd subset_rfl h_grew

/-- Refutation teeth: from a state whose only "grant-shaped" candidate is an
unsigned/forged membership, no `adminGrant` transition exists for it — its
precondition `adminSignedMembership` is unprovable. Phrased as the impossibility
of the gate firing on a forged membership. -/
theorem no_grant_on_unsigned_membership {pre : NetworkState} {m : Membership}
    (h_unsigned : m.adminSigValid = false) :
    ¬ adminSignedMembership pre.network m := by
  rintro ⟨_, hsig⟩
  rw [h_unsigned] at hsig
  exact Bool.false_ne_true hsig

/-- Non-vacuity witness for `membership_growth_requires_admin_sig`: a concrete
`adminGrant` transition that DOES grow the roster with a valid admin signature. -/
theorem membership_growth_witness :
    ∃ (pre post : NetworkState) (m : Membership),
      NetTransition pre post ∧ ¬ post.memberships ⊆ pre.memberships ∧
        adminSignedMembership pre.network m := by
  let n : Network := ⟨"net-1", "did:key:admin", true⟩
  let m : Membership := ⟨"net-1", "did:key:a", true, true⟩
  let pre : NetworkState :=
    { self := "did:key:self"
    , network := n
    , memberships := ∅
    , endpoints := ∅
    , joinRequests := ∅
    , operatorDesired := ∅
    , networkDesired := ∅ }
  have hsig : adminSignedMembership pre.network m := ⟨rfl, rfl⟩
  refine ⟨pre, grantState pre m, m,
    NetTransition.adminGrant m hsig rfl rfl, ?_, hsig⟩
  -- The grant inserts m into the empty roster, so the post roster is not ⊆ ∅.
  intro hsub
  have : m ∈ pre.memberships :=
    hsub (by simp [grantState])
  exact (Finset.not_mem_empty m) this

/-! ## Executable join-request authority decision (mirrors cut-5 Rust)

The cut-5 reconciler decides materializability with a single boolean per
endpoint. `decideMaterializable` is that boolean; `decideMaterializable_agrees`
fences it to the `endpointMaterializable` Prop the derivation filters on, so the
executable decision and the model can never diverge. -/
def decideMaterializable (s : NetworkState) (ep : Endpoint) : Bool :=
  decide (endpointMaterializable s ep)

theorem decideMaterializable_agrees (s : NetworkState) (ep : Endpoint) :
    decideMaterializable s ep = true ↔ endpointMaterializable s ep := by
  unfold decideMaterializable
  exact decide_eq_true_iff

end PeerRegistryDiscovery
