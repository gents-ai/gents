import Proofs.PeerRegistryDiscovery.Transition
import Mathlib.Data.Finset.Image

/-!
# Peer Registry Discovery — Derivation properties

The four obligations from the spec's "Discovery reconciler" section, plus the
signed-invite guard. Every transition-level theorem is discharged by `cases`
over the full `Transition` relation, so no convenient case is silently skipped.
-/

namespace PeerRegistryDiscovery

open DiscoveryState

/-! ## Membership characterization of the derivation -/

/-- `p` is derived iff some live, non-self registry entry carries peer `p`. -/
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

/-- Self is never derived. (Sanity: the filter excludes it.) -/
theorem self_not_mem_derive {self : PeerId} {reg : Registry} :
    self ∉ deriveRegistryDesired self reg := by
  rw [mem_deriveRegistryDesired]
  rintro ⟨e, _, _, he_self, he_peer⟩
  exact he_self (he_peer.trans rfl)

/-! ## (1) Idempotence -/

/-- Deriving twice equals deriving once: the registry is the sole input and a
derive step does not change it. -/
theorem derive_idempotent (s : DiscoveryState) :
    deriveStep (deriveStep s) = deriveStep s := by
  unfold deriveStep
  rfl

/-- The function-level idempotence the Rust derivation mirrors. -/
theorem deriveRegistryDesired_idempotent (self : PeerId) (reg : Registry) :
    deriveRegistryDesired self (deriveStep ⟨self, reg, ∅, ∅⟩).registry
      = deriveRegistryDesired self reg := rfl

/-! ## (2) Convergence

The derived set is a *pure function of the registry*, so it converges in a
single derive step: the post-state is settled, and any further derive over an
unchanged registry is a fixpoint. We state exactly this and do not claim a
multi-step reachability result over arbitrary interleavings. -/

/-- One derive step settles the registry-owned partition. -/
theorem derive_settles (s : DiscoveryState) : (deriveStep s).settled := by
  unfold settled deriveStep
  rfl

/-- A settled state with an unchanged registry is a derive fixpoint: re-running
the derivation changes nothing. This is the convergence content — stability of
the derived set across ticks given a stable registry. -/
theorem derive_convergent {s : DiscoveryState} (h : s.settled) :
    deriveStep s = s := by
  unfold settled at h
  unfold deriveStep
  rw [← h]

/-! ## (3) Ownership safety

Quantified over ALL transitions. The discovery step (and registry edits) never
touch the operator partition; only an explicit `operatorWrite` does — and that
IS the operator, by construction not the discovery step. So: every non-operator
transition preserves `operatorDesired`, and `operatorWrite` is the sole
exception (named honestly). -/

/-- No derivation/retraction/join transition mutates an operator-owned row. The
only transition that changes `operatorDesired` is `operatorWrite` itself. -/
theorem ownership_safe {pre post : DiscoveryState} (h : Transition pre post)
    (h_not_operator : ∀ d, post ≠ operatorWriteState pre d) :
    post.operatorDesired = pre.operatorDesired := by
  cases h with
  | derive h_post => subst h_post; rfl
  | join tok e tofu _ h_post => subst h_post; rfl
  | removeEntry e h_post => subst h_post; rfl
  | operatorWrite d h_post =>
      exact absurd h_post (h_not_operator d)

/-- Sharper form: the derive step specifically preserves the operator partition
*and* the registry, mutating only `registryDesired`. -/
theorem derive_preserves_operator_and_registry (s : DiscoveryState) :
    (deriveStep s).operatorDesired = s.operatorDesired ∧
    (deriveStep s).registry = s.registry := ⟨rfl, rfl⟩

/-- The operator partition flows untouched into the effective desired set under
a derive step (operator intent survives derivation). -/
theorem derive_preserves_operator_in_effective (s : DiscoveryState) :
    s.operatorDesired ⊆ (deriveStep s).effectiveDesired := by
  intro p hp
  exact Finset.mem_union_left _ hp

/-! ## (4) Retraction soundness

Removing/staling entry `e` then re-deriving removes exactly `e`'s registry-owned
derived row(s) and no others. Made precise as a membership characterization of
the post-derivation set in terms of the pre-registry, plus the corollaries that
(a) an unrelated derived peer survives, and (b) `e.peer` is dropped exactly when
`e` was its sole live, non-self source. -/

/-- After removing `e` and deriving, `p` is derived iff some live non-self
entry *other than `e`* carries `p`. -/
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

/-- Retraction is targeted: any derived peer backed by a *different* live entry
survives the removal of `e`. No collateral retraction. -/
theorem retraction_preserves_others {self : PeerId} {reg : Registry}
    {e e' : RegistryEntry} (hne : e' ≠ e) (he'_mem : e' ∈ reg)
    (he'_live : e'.live = true) (he'_self : e'.peer ≠ self) :
    e'.peer ∈ deriveRegistryDesired self (reg.erase e) := by
  rw [retraction_characterization]
  exact ⟨e', he'_mem, hne, he'_live, he'_self, rfl⟩

/-- Retraction is exact: `e.peer` is dropped precisely when `e` was the *only*
live, non-self entry carrying it. (If another live entry carries `e.peer`, it
survives by `retraction_preserves_others`.) -/
theorem retraction_drops_unique_source {self : PeerId} {reg : Registry}
    {e : RegistryEntry}
    (h_unique : ∀ e' ∈ reg, e' ≠ e → e'.live = true → e'.peer ≠ self →
                  e'.peer ≠ e.peer) :
    e.peer ∉ deriveRegistryDesired self (reg.erase e) := by
  rw [retraction_characterization]
  rintro ⟨e', he'_mem, hne, he'_live, he'_self, he'_peer⟩
  exact (h_unique e' he'_mem hne he'_live he'_self) he'_peer

/-- Whole-state corollary: a `removeEntry` step followed by `derive` retracts
only registry-owned rows; the operator partition is byte-identical throughout. -/
theorem retraction_sound {pre post post' : DiscoveryState} {e : RegistryEntry}
    (h_remove : post = removeEntryState pre e)
    (h_derive : post' = deriveStep post) :
    post'.operatorDesired = pre.operatorDesired ∧
    post'.registryDesired = deriveRegistryDesired pre.self (pre.registry.erase e) := by
  subst h_remove
  subst h_derive
  exact ⟨rfl, rfl⟩

/-! ## (5) Signed-invite guard

A `join` step is enabled only when the issuer is a live member, or the
TOFU-bootstrap flag holds. This fences "non-member invite rejected". -/

/-- A `join` is the ONLY transition that can grow the registry, and it can fire
only with a member-signed token. So any step that admitted a new entry — its
post-state registry is not a subset of the pre-state registry — must have
carried a member signature.

This is the positive authorization fact, and it is genuinely non-vacuous: we
case on the relation and read the signature witness out of the `join`
constructor itself (it is NOT supplied as a hypothesis the caller had to already
prove), while `derive` / `removeEntry` / `operatorWrite` never grow the registry
and so are refuted by `h_grew`. The hypothesis is satisfiable — a real join of a
fresh entry makes the registry grow — and the conclusion is not contained in it.
Together with `no_join_without_admissible_token` (the refutation direction) and
`non_member_invite_rejected` (the leaf guard), this fences "a node enters the
trust set only via a member-signed invite".

Scope (see `Transition.join`): the signature gates WHETHER a node is admitted,
not WHICH identity — the inserted entry is self-asserted under the trusted-fleet
TOFU model, so this does not claim `e.did` is bound to the token issuer. -/
theorem registry_growth_requires_member_signature {pre post : DiscoveryState}
    (h : Transition pre post)
    (h_grew : ¬ post.registry ⊆ pre.registry) :
    ∃ (tok : Token) (tofu : Bool), signedByMember tok pre.registry pre.self tofu := by
  cases h with
  | derive hp =>
      refine absurd ?_ h_grew
      rw [hp]; exact subset_rfl
  | join tok e tofu hsig _hp =>
      exact ⟨tok, tofu, hsig⟩
  | removeEntry e hp =>
      refine absurd ?_ h_grew
      rw [hp]; exact Finset.erase_subset e pre.registry
  | operatorWrite d hp =>
      refine absurd ?_ h_grew
      rw [hp]; exact subset_rfl

/-- The contrapositive teeth: if NO token is admissible in `pre` (every signed
token is from a non-member and TOFU bootstrap is off), then NO join transition
out of `pre` exists. "Non-member invite rejected" as a refutation. -/
theorem no_join_without_admissible_token {pre post : DiscoveryState}
    (h_none : ∀ (tok : Token) (tofu : Bool), ¬ signedByMember tok pre.registry pre.self tofu)
    (h : Transition pre post) :
    ∀ (tok : Token) (e : RegistryEntry) (tofu : Bool)
      (hsig : signedByMember tok pre.registry pre.self tofu)
      (hpost : post = joinState pre e),
      h ≠ Transition.join tok e tofu hsig hpost := by
  intro tok e tofu hsig _ _
  exact absurd hsig (h_none tok tofu)

/-- A non-member, signature-invalid, non-bootstrap token is never admissible:
the join guard rejects it. This is the leaf "forged/non-member invite" fact. -/
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

end PeerRegistryDiscovery
