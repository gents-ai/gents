import Proofs.PeerRegistryDiscovery.Transition
import Mathlib.Data.Finset.Image

namespace PeerRegistryDiscovery

open DiscoveryState

theorem mem_deriveRegistryDesired {self : PeerId} {reg : Registry} {p : PeerId} :
    p ∈ deriveRegistryDesired self reg ↔
      ∃ e ∈ reg, e.live = true ∧ e.peer ≠ self ∧ e.peer = p := by
  unfold deriveRegistryDesired
  simp only [Finset.mem_image, Finset.mem_filter]
  constructor
  · rintro ⟨e, ⟨he_mem, he_live, he_self⟩, he_peer⟩
    exact ⟨e, he_mem, he_live, he_self, he_peer⟩
  · rintro ⟨e, he_mem, he_live, he_self, he_peer⟩
    exact ⟨e, ⟨he_mem, he_live, he_self⟩, he_peer⟩

theorem self_not_mem_derive {self : PeerId} {reg : Registry} :
    self ∉ deriveRegistryDesired self reg := by
  rw [mem_deriveRegistryDesired]
  rintro ⟨e, _, _, he_self, he_peer⟩
  exact he_self (he_peer.trans rfl)

theorem derive_idempotent (s : DiscoveryState) :
    deriveStep (deriveStep s) = deriveStep s := by
  unfold deriveStep
  rfl

theorem deriveRegistryDesired_idempotent (self : PeerId) (reg : Registry) :
    deriveRegistryDesired self (deriveStep ⟨self, reg, ∅, ∅, ∅⟩).registry
      = deriveRegistryDesired self reg := rfl

theorem derive_settles (s : DiscoveryState) : (deriveStep s).settled := by
  unfold settled deriveStep
  rfl

theorem derive_convergent {s : DiscoveryState} (h : s.settled) :
    deriveStep s = s := by
  unfold settled at h
  unfold deriveStep
  rw [← h]

theorem ownership_safe {pre post : DiscoveryState} (h : Transition pre post)
    (h_not_operator : ∀ d, post ≠ operatorWriteState pre d) :
    post.operatorDesired = pre.operatorDesired := by
  cases h with
  | derive h_post => subst h_post; rfl
  | join tok e tofu _ h_post => subst h_post; rfl
  | reciprocalJoin tok e tofu _ h_post => subst h_post; rfl
  | removeEntry e h_post => subst h_post; rfl
  | operatorWrite d h_post =>
      exact absurd h_post (h_not_operator d)

theorem derive_preserves_operator_and_registry (s : DiscoveryState) :
    (deriveStep s).operatorDesired = s.operatorDesired ∧
    (deriveStep s).registry = s.registry := ⟨rfl, rfl⟩

theorem derive_preserves_operator_in_effective (s : DiscoveryState) :
    s.operatorDesired ⊆ (deriveStep s).effectiveDesired := by
  intro p hp
  exact Finset.mem_union_left _ hp

theorem retraction_characterization {self : PeerId} {reg : Registry}
    {e : RegistryEntry} {p : PeerId} :
    p ∈ deriveRegistryDesired self (reg.erase e) ↔
      ∃ e' ∈ reg, e' ≠ e ∧ e'.live = true ∧ e'.peer ≠ self ∧ e'.peer = p := by
  rw [mem_deriveRegistryDesired]
  constructor
  · rintro ⟨e', he'_mem, he'_live, he'_self, he'_peer⟩
    have hne : e' ≠ e := (Finset.ne_of_mem_erase he'_mem)
    exact ⟨e', Finset.mem_of_mem_erase he'_mem, hne, he'_live, he'_self, he'_peer⟩
  · rintro ⟨e', he'_mem, hne, he'_live, he'_self, he'_peer⟩
    exact ⟨e', Finset.mem_erase.mpr ⟨hne, he'_mem⟩, he'_live, he'_self, he'_peer⟩

theorem retraction_preserves_others {self : PeerId} {reg : Registry}
    {e e' : RegistryEntry} (hne : e' ≠ e) (he'_mem : e' ∈ reg)
    (he'_live : e'.live = true) (he'_self : e'.peer ≠ self) :
    e'.peer ∈ deriveRegistryDesired self (reg.erase e) := by
  rw [retraction_characterization]
  exact ⟨e', he'_mem, hne, he'_live, he'_self, rfl⟩

theorem retraction_drops_unique_source {self : PeerId} {reg : Registry}
    {e : RegistryEntry}
    (h_unique : ∀ e' ∈ reg, e' ≠ e → e'.live = true → e'.peer ≠ self →
                  e'.peer ≠ e.peer) :
    e.peer ∉ deriveRegistryDesired self (reg.erase e) := by
  rw [retraction_characterization]
  rintro ⟨e', he'_mem, hne, he'_live, he'_self, he'_peer⟩
  exact (h_unique e' he'_mem hne he'_live he'_self) he'_peer

theorem retraction_sound {pre post post' : DiscoveryState} {e : RegistryEntry}
    (h_remove : post = removeEntryState pre e)
    (h_derive : post' = deriveStep post) :
    post'.operatorDesired = pre.operatorDesired ∧
    post'.registryDesired = deriveRegistryDesired pre.self (pre.registry.erase e) := by
  subst h_remove
  subst h_derive
  exact ⟨rfl, rfl⟩

theorem registry_growth_requires_member_signature {pre post : DiscoveryState}
    (h : Transition pre post)
    (h_grew : ¬ post.registry ⊆ pre.registry) :
    ∃ (tok : Token) (tofu : Bool), signedByMember tok pre.registry pre.self tofu := by
  cases h with
  | derive hp =>
      refine absurd ?_ h_grew
      rw [hp]; exact subset_rfl
  | join tok e tofu hsig _hp =>
      exact ⟨tok, tofu, hsig.1⟩
  | reciprocalJoin tok e tofu hsig _hp =>
      exact ⟨tok, tofu, hsig.1⟩
  | removeEntry e hp =>
      refine absurd ?_ h_grew
      rw [hp]; exact Finset.erase_subset e pre.registry
  | operatorWrite d hp =>
      refine absurd ?_ h_grew
      rw [hp]; exact subset_rfl

theorem no_join_without_admissible_token {pre post : DiscoveryState}
    (h_none : ∀ (tok : Token) (tofu : Bool), ¬ signedByMember tok pre.registry pre.self tofu)
    (h : Transition pre post) :
    ∀ (tok : Token) (e : RegistryEntry) (tofu : Bool)
      (hadm : admitsJoin pre tok tofu)
      (hpost : post = joinState pre e tok),
      h ≠ Transition.join tok e tofu hadm hpost := by
  intro tok e tofu hadm _ _
  exact absurd hadm.1 (h_none tok tofu)

theorem non_member_invite_rejected
    {tok : Token} {reg : Registry} {self : PeerId}
    (h_unsigned_or_nonmember : tok.sigValid = false ∨ ¬ isMember tok.issuer reg) :
    ¬ signedByMember tok reg self false := by
  unfold signedByMember
  rintro ⟨hsig, hor⟩
  rcases h_unsigned_or_nonmember with h_unsig | h_nonmem
  · exact absurd hsig (by simp [h_unsig])
  · rcases hor with h_mem | h_boot
    · exact h_nonmem h_mem
    · exact absurd h_boot.1 (by simp)

theorem joinState_consumes_nonce (s : DiscoveryState) (e : RegistryEntry) (tok : Token) :
    tok.nonce ∈ (joinState s e tok).consumedNonces := by
  unfold joinState
  simp

theorem replay_rejected {pre post : DiscoveryState} {tok : Token} {e : RegistryEntry}
    {tofu : Bool} (_hadm : admitsJoin pre tok tofu)
    (hpost : post = joinState pre e tok) :
    ∀ tofu', ¬ admitsJoin post tok tofu' := by
  intro tofu' hadm'
  have h_consumed : tok.nonce ∈ post.consumedNonces := by
    rw [hpost]; exact joinState_consumes_nonce pre e tok
  exact hadm'.2 h_consumed

theorem replay_rejected_witness :
    ∃ (pre : DiscoveryState) (tok : Token) (tofu : Bool),
      admitsJoin pre tok tofu := by
  let member : RegistryEntry := ⟨"peer-a", "did:key:a", true⟩
  let pre : DiscoveryState :=
    { self := "peer-self"
    , registry := {member}
    , operatorDesired := ∅
    , registryDesired := ∅
    , consumedNonces := ∅ }
  let tok : Token := ⟨"did:key:a", true, "nonce-1"⟩
  refine ⟨pre, tok, false, ?_, ?_⟩
  ·
    refine ⟨rfl, Or.inl ?_⟩
    exact ⟨member, Finset.mem_singleton_self member, rfl, rfl⟩
  ·
    exact Finset.not_mem_empty _

theorem reciprocal_join_still_gated {pre post : DiscoveryState}
    {tok : Token} {e : RegistryEntry} {tofu : Bool}
    (h : Transition pre post)
    (h_is_reciprocal :
      ∃ (hadm : admitsJoin pre tok tofu) (hpost : post = joinState pre e tok),
        h = Transition.reciprocalJoin tok e tofu hadm hpost) :
    admitsJoin pre tok tofu :=
  h_is_reciprocal.choose

theorem reciprocal_join_witness :
    ∃ (pre post : DiscoveryState) (tok : Token) (tofu : Bool),
      Transition pre post ∧ admitsJoin pre tok tofu := by
  let member : RegistryEntry := ⟨"peer-a", "did:key:a", true⟩
  let joiner : RegistryEntry := ⟨"peer-b", "did:key:b", true⟩
  let pre : DiscoveryState :=
    { self := "peer-self"
    , registry := {member}
    , operatorDesired := ∅
    , registryDesired := ∅
    , consumedNonces := ∅ }
  let tok : Token := ⟨"did:key:a", true, "nonce-1"⟩
  have hadm : admitsJoin pre tok false := by
    refine ⟨⟨rfl, Or.inl ?_⟩, Finset.not_mem_empty _⟩
    exact ⟨member, Finset.mem_singleton_self member, rfl, rfl⟩
  exact ⟨pre, joinState pre joiner tok, tok, false,
    Transition.reciprocalJoin tok joiner false hadm rfl, hadm⟩

theorem reciprocal_join_rejected_on_bad_signature {pre : DiscoveryState}
    {tok : Token} {tofu : Bool}
    (h_bad_sig : tok.sigValid = false) :
    ¬ admitsJoin pre tok tofu := by
  rintro ⟨⟨hsig, _⟩, _⟩
  rw [h_bad_sig] at hsig
  exact Bool.false_ne_true hsig

theorem reciprocal_join_rejected_witness :
    ∃ tok : Token, tok.sigValid = false := by
  exact ⟨⟨"did:key:x", false, "nonce-x"⟩, rfl⟩

theorem reciprocal_replay_rejected {pre post : DiscoveryState} {tok : Token}
    {e : RegistryEntry} {tofu : Bool} (hadm : admitsJoin pre tok tofu)
    (hpost : post = joinState pre e tok) :
    ∀ tofu', ¬ admitsJoin post tok tofu' :=
  replay_rejected hadm hpost

end PeerRegistryDiscovery
