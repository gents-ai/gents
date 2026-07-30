import Proofs.PeerRegistryDiscovery.Transition
import Mathlib.Data.Finset.Image

namespace PeerRegistryDiscovery

structure Network where
  networkId : String
  adminDid : Did
  adminSigValid : Bool
  deriving DecidableEq, Repr

structure Membership where
  networkId : String
  memberDid : Did
  active : Bool
  adminSigValid : Bool
  deriving DecidableEq, Repr

structure Endpoint where
  did : Did
  nodeId : String
  fresh : Bool
  bindingSigValid : Bool
  deriving DecidableEq, Repr

structure JoinRequest where
  networkId : String
  candidateDid : Did
  reqSigValid : Bool
  deriving DecidableEq, Repr

def validNetwork (n : Network) : Prop := n.adminSigValid = true

instance (n : Network) : Decidable (validNetwork n) := by
  unfold validNetwork; infer_instance

def adminSignedMembership (n : Network) (m : Membership) : Prop :=
  m.networkId = n.networkId ∧ m.adminSigValid = true

instance (n : Network) (m : Membership) : Decidable (adminSignedMembership n m) := by
  unfold adminSignedMembership; infer_instance

def memberSignedEndpoint (ep : Endpoint) : Prop := ep.bindingSigValid = true

instance (ep : Endpoint) : Decidable (memberSignedEndpoint ep) := by
  unfold memberSignedEndpoint; infer_instance

def admittedMember (n : Network) (m : Membership) : Prop :=
  validNetwork n ∧ adminSignedMembership n m ∧ m.active = true

instance (n : Network) (m : Membership) : Decidable (admittedMember n m) := by
  unfold admittedMember; infer_instance

structure NetworkState where
  self : Did
  network : Network
  memberships : Finset Membership
  endpoints : Finset Endpoint
  joinRequests : Finset JoinRequest
  operatorDesired : Finset Did
  networkDesired : Finset Did
  deriving DecidableEq

def endpointMaterializable (s : NetworkState) (ep : Endpoint) : Prop :=
  validNetwork s.network ∧
    memberSignedEndpoint ep ∧ ep.fresh = true ∧ ep.did ≠ s.self ∧
    ∃ m ∈ s.memberships, admittedMember s.network m ∧ m.memberDid = ep.did

instance (s : NetworkState) : DecidablePred (endpointMaterializable s) := by
  intro ep; unfold endpointMaterializable; infer_instance

def deriveNetworkDesired (s : NetworkState) : Finset Did :=
  (s.endpoints.filter (endpointMaterializable s)).image Endpoint.did

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

def deriveNetStep (s : NetworkState) : NetworkState :=
  { s with networkDesired := deriveNetworkDesired s }

def grantState (s : NetworkState) (m : Membership) : NetworkState :=
  { s with memberships := insert m s.memberships }

def revokeState (s : NetworkState) (m : Membership) : NetworkState :=
  { s with memberships := s.memberships.erase m }

def requestState (s : NetworkState) (jr : JoinRequest) : NetworkState :=
  { s with joinRequests := insert jr s.joinRequests }

def endpointState (s : NetworkState) (ep : Endpoint) : NetworkState :=
  { s with endpoints := insert ep s.endpoints }

def netOperatorWriteState (s : NetworkState) (d : Finset Did) : NetworkState :=
  { s with operatorDesired := d }

inductive NetTransition : NetworkState → NetworkState → Prop where
  | derive {pre post : NetworkState} :
      post = deriveNetStep pre → NetTransition pre post
  | adminGrant {pre post : NetworkState} (m : Membership) :
      adminSignedMembership pre.network m → m.active = true →
      post = grantState pre m → NetTransition pre post
  | adminRevoke {pre post : NetworkState} (m : Membership) :
      post = revokeState pre m → NetTransition pre post
  | joinRequest {pre post : NetworkState} (jr : JoinRequest) :
      post = requestState pre jr → NetTransition pre post
  | endpointRefresh {pre post : NetworkState} (ep : Endpoint) :
      post = endpointState pre ep → NetTransition pre post
  | operatorWrite {pre post : NetworkState} (d : Finset Did) :
      post = netOperatorWriteState pre d → NetTransition pre post

theorem forged_membership_not_admitted (n : Network) (m : Membership)
    (h : m.adminSigValid = false) : ¬ admittedMember n m := by
  rintro ⟨_, ⟨_, hsig⟩, _⟩
  rw [h] at hsig
  exact Bool.false_ne_true hsig

theorem unsigned_membership_not_materialized {s : NetworkState} {ep : Endpoint}
    (h_none : ∀ m ∈ s.memberships, m.memberDid = ep.did → ¬ admittedMember s.network m) :
    ep.did ∉ deriveNetworkDesired s := by
  rw [mem_deriveNetworkDesired]
  rintro ⟨ep', _, ⟨_, _, _, _, m, hm_mem, hm_adm, hm_did⟩, hep'_did⟩
  exact h_none m hm_mem (hm_did.trans hep'_did) hm_adm

theorem forged_endpoint_not_materializable {s : NetworkState} {ep : Endpoint}
    (h : ep.bindingSigValid = false) : ¬ endpointMaterializable s ep := by
  rintro ⟨_, hsig, _, _, _⟩
  unfold memberSignedEndpoint at hsig
  rw [h] at hsig
  exact Bool.false_ne_true hsig

theorem unsigned_endpoint_not_materialized {s : NetworkState} {d : Did}
    (h_none : ∀ ep ∈ s.endpoints, ep.did = d → ep.bindingSigValid = false) :
    d ∉ deriveNetworkDesired s := by
  rw [mem_deriveNetworkDesired]
  rintro ⟨ep, hep, hmat, hep_did⟩
  exact forged_endpoint_not_materializable (h_none ep hep hep_did) hmat

theorem materializable_is_derived {s : NetworkState} {ep : Endpoint}
    (hep : ep ∈ s.endpoints) (h : endpointMaterializable s ep) :
    ep.did ∈ deriveNetworkDesired s := by
  rw [mem_deriveNetworkDesired]
  exact ⟨ep, hep, h, rfl⟩

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

theorem revoke_drops_member {s : NetworkState} {m : Membership} {d : Did}
    (h_sole : ∀ m' ∈ s.memberships, m' ≠ m → admittedMember s.network m' →
                m'.memberDid ≠ d) :
    d ∉ deriveNetworkDesired (revokeState s m) := by
  rw [revoke_characterization]
  rintro ⟨ep, _, _, _, _, _, hep_did, m', hm'_mem, hm'_ne, hm'_adm, hm'_did⟩
  exact h_sole m' hm'_mem hm'_ne hm'_adm (hm'_did.trans hep_did)

theorem revoke_preserves_others {s : NetworkState} {m m' : Membership} {ep' : Endpoint}
    (hm'_ne : m' ≠ m) (hep'_mem : ep' ∈ s.endpoints)
    (hnet : validNetwork s.network) (hsig : memberSignedEndpoint ep')
    (hfresh : ep'.fresh = true) (hself : ep'.did ≠ s.self)
    (hm'_mem : m' ∈ s.memberships) (hm'_adm : admittedMember s.network m')
    (hm'_did : m'.memberDid = ep'.did) :
    ep'.did ∈ deriveNetworkDesired (revokeState s m) := by
  rw [revoke_characterization]
  exact ⟨ep', hep'_mem, hnet, hsig, hfresh, hself, rfl, m', hm'_mem, hm'_ne, hm'_adm, hm'_did⟩

theorem revoke_sound {pre post post' : NetworkState} {m : Membership}
    (h_revoke : post = revokeState pre m)
    (h_derive : post' = deriveNetStep post) :
    post'.operatorDesired = pre.operatorDesired ∧
    post'.networkDesired = deriveNetworkDesired (revokeState pre m) := by
  subst h_revoke; subst h_derive
  exact ⟨rfl, rfl⟩

def tombstoneState (s : NetworkState) (m : Membership) : NetworkState :=
  { s with memberships := insert { m with active := false } (s.memberships.erase m) }

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

theorem deriveNetworkDesired_tombstone_eq_revoke (s : NetworkState) (m : Membership) :
    deriveNetworkDesired (tombstoneState s m) = deriveNetworkDesired (revokeState s m) := by
  apply Finset.ext
  intro d
  rw [tombstone_characterization, ← revoke_characterization]

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

theorem net_derive_preserves_operator_and_inputs (s : NetworkState) :
    (deriveNetStep s).operatorDesired = s.operatorDesired ∧
    (deriveNetStep s).memberships = s.memberships ∧
    (deriveNetStep s).endpoints = s.endpoints := ⟨rfl, rfl, rfl⟩

theorem join_request_grants_nothing {pre post : NetworkState} (jr : JoinRequest)
    (h : post = requestState pre jr) :
    post.memberships = pre.memberships := by
  subst h; rfl

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

theorem no_grant_on_unsigned_membership {pre : NetworkState} {m : Membership}
    (h_unsigned : m.adminSigValid = false) :
    ¬ adminSignedMembership pre.network m := by
  rintro ⟨_, hsig⟩
  rw [h_unsigned] at hsig
  exact Bool.false_ne_true hsig

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
  intro hsub
  have : m ∈ pre.memberships :=
    hsub (by simp [grantState])
  exact (Finset.not_mem_empty m) this

def decideMaterializable (s : NetworkState) (ep : Endpoint) : Bool :=
  decide (endpointMaterializable s ep)

theorem decideMaterializable_agrees (s : NetworkState) (ep : Endpoint) :
    decideMaterializable s ep = true ↔ endpointMaterializable s ep := by
  unfold decideMaterializable
  exact decide_eq_true_iff

structure V5JoinClaim where
  issuerDid : Did
  joinerDid : Did
  networkSigValid : Bool
  networkIdConsistent : Bool
  grant : Membership
  deriving DecidableEq, Repr

def admitsV5Join (n : Network) (c : V5JoinClaim) : Prop :=
  c.issuerDid = n.adminDid ∧
    c.networkSigValid = true ∧
    c.networkIdConsistent = true ∧
    admittedMember n c.grant ∧
    c.grant.memberDid = c.joinerDid

instance (n : Network) (c : V5JoinClaim) : Decidable (admitsV5Join n c) := by
  unfold admitsV5Join; infer_instance

theorem v5_non_admin_issuer_rejected (n : Network) (c : V5JoinClaim)
    (h : c.issuerDid ≠ n.adminDid) : ¬ admitsV5Join n c := fun hc => h hc.1

theorem v5_forged_grant_rejected (n : Network) (c : V5JoinClaim)
    (h : ¬ admittedMember n c.grant) : ¬ admitsV5Join n c := fun hc => h hc.2.2.2.1

theorem v5_wrong_grantee_rejected (n : Network) (c : V5JoinClaim)
    (h : c.grant.memberDid ≠ c.joinerDid) : ¬ admitsV5Join n c := fun hc => h hc.2.2.2.2

theorem v5_invalid_network_sig_rejected (n : Network) (c : V5JoinClaim)
    (h : c.networkSigValid = false) : ¬ admitsV5Join n c := by
  intro hc
  have hv : c.networkSigValid = true := hc.2.1
  rw [h] at hv
  simp at hv

theorem v5_admits_witness :
    ∃ (n : Network) (c : V5JoinClaim), admitsV5Join n c := by
  refine ⟨{ networkId := "net", adminDid := "admin", adminSigValid := true },
          { issuerDid := "admin", joinerDid := "member",
            networkSigValid := true, networkIdConsistent := true,
            grant := { networkId := "net", memberDid := "member",
                       active := true, adminSigValid := true } }, ?_⟩
  exact ⟨rfl, rfl, rfl, ⟨rfl, ⟨rfl, rfl⟩, rfl⟩, rfl⟩

end PeerRegistryDiscovery
