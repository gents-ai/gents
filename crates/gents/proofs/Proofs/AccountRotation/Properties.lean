import Proofs.AccountRotation.Policy

namespace AccountRotation

theorem bestRank?_mem : ∀ {rs : List (Option Nat)} {best : Nat},
    bestRank? rs = some best → ∃ i : Nat, rs[i]? = some (some best)
  | [], _, h => by simp [bestRank?] at h
  | none :: rest, best, h => by
    simp only [bestRank?] at h
    obtain ⟨i, hi⟩ := bestRank?_mem h
    exact ⟨i + 1, by simpa using hi⟩
  | some rank :: rest, best, h => by
    simp only [bestRank?] at h
    cases hrest : bestRank? rest with
    | none =>
      simp [hrest] at h
      exact ⟨0, by simp [h]⟩
    | some b =>
      simp [hrest] at h
      by_cases hrb : rank ≤ b
      · exact ⟨0, by simp [← h, Nat.min_eq_left hrb]⟩
      · obtain ⟨i, hi⟩ := bestRank?_mem hrest
        have hb : best = b := by rw [← h]; exact Nat.min_eq_right (by omega)
        exact ⟨i + 1, by simpa [hb] using hi⟩

theorem bestRank?_none : ∀ {rs : List (Option Nat)}, bestRank? rs = none →
    ∀ (i r : Nat), rs[i]? ≠ some (some r)
  | [], _, i, r => by simp
  | none :: rest, h, i, r => by
    simp only [bestRank?] at h
    cases i with
    | zero => simp
    | succ i => simpa using bestRank?_none h i r
  | some rank :: rest, h, _, _ => by
    simp only [bestRank?] at h
    cases hrest : bestRank? rest <;> simp [hrest] at h

theorem bestRank?_le : ∀ {rs : List (Option Nat)} {best : Nat}, bestRank? rs = some best →
    ∀ (i r : Nat), rs[i]? = some (some r) → best ≤ r
  | [], _, h, _, _, _ => by simp [bestRank?] at h
  | none :: rest, best, h, i, r, hr => by
    simp only [bestRank?] at h
    cases i with
    | zero => simp at hr
    | succ i => exact bestRank?_le h i r (by simpa using hr)
  | some rank :: rest, best, h, i, r, hr => by
    simp only [bestRank?] at h
    cases hrest : bestRank? rest with
    | none =>
      simp [hrest] at h
      cases i with
      | zero => simp at hr; omega
      | succ i =>
        exact absurd (show rest[i]? = some (some r) by simpa using hr) (bestRank?_none hrest i r)
    | some b =>
      simp [hrest] at h
      cases i with
      | zero =>
        simp at hr
        rw [← h]
        exact Nat.le_trans (Nat.min_le_left _ _) (Nat.le_of_eq hr)
      | succ i =>
        have := bestRank?_le hrest i r (by simpa using hr)
        rw [← h]
        exact Nat.le_trans (Nat.min_le_right _ _) this

theorem mem_cyclic {n start i : Nat} (hi : i < n) : i ∈ cyclic n start := by
  unfold cyclic
  by_cases h : start ≤ i
  · exact List.mem_append_left _ (List.mem_filter.mpr ⟨List.mem_range.mpr hi, by simp [h]⟩)
  · exact List.mem_append_right _ (List.mem_filter.mpr ⟨List.mem_range.mpr hi, by simp; omega⟩)

theorem pick_best {rs : List (Option Nat)} {n : Nat} {current : Option Nat} {cursor i : Nat}
    (h : pick rs n current cursor = some i) :
    ∃ best, bestRank? rs = some best ∧ rs[i]? = some (some best) := by
  unfold pick at h
  cases hb : bestRank? rs with
  | none => simp [hb] at h
  | some best =>
    refine ⟨best, rfl, ?_⟩
    cases current with
    | none =>
      simp only [hb] at h
      simpa [hit] using List.find?_some h
    | some c =>
      simp only [hb] at h
      by_cases hc : hit rs best c = true
      · rw [if_pos hc] at h
        cases h
        simpa [hit] using hc
      · rw [if_neg hc] at h
        simpa [hit] using List.find?_some h

theorem pick_live {rs : List (Option Nat)} {n : Nat} (hn : rs.length = n)
    (current : Option Nat) (cursor : Nat) {i r : Nat} (hr : rs[i]? = some (some r)) :
    (pick rs n current cursor).isSome = true := by
  unfold pick
  cases hb : bestRank? rs with
  | none => exact absurd hr (bestRank?_none hb i r)
  | some best =>
    obtain ⟨j, hj⟩ := bestRank?_mem hb
    have hjlt : j < n := by
      rcases Nat.lt_or_ge j n with hlt | hge
      · exact hlt
      · have hnone : rs[j]? = none := List.getElem?_eq_none (by omega)
        rw [hnone] at hj
        cases hj
    have hjhit : hit rs best j = true := by simp [hit, hj]
    cases current with
    | none =>
      dsimp only
      cases hf : (cyclic n cursor).find? (hit rs best) with
      | none => exact absurd hjhit (List.find?_eq_none.mp hf j (mem_cyclic hjlt))
      | some k => rfl
    | some c =>
      dsimp only
      by_cases hc : hit rs best c = true
      · rw [if_pos hc]
        rfl
      · rw [if_neg hc]
        cases hf : (cyclic n (c + 1)).find? (hit rs best) with
        | none => exact absurd hjhit (List.find?_eq_none.mp hf j (mem_cyclic hjlt))
        | some k => rfl

theorem ranks_length (policy : Policy) (now : Time) (family : Family) (pool : List Account) :
    (ranks policy now family pool).length = pool.length := by
  simp [ranks]

theorem ranks_getElem? (policy : Policy) (now : Time) (family : Family) (pool : List Account)
    (i : Nat) :
    (ranks policy now family pool)[i]? =
      pool[i]?.map (fun account => (tier? policy now family account).map Tier.rank) := by
  simp [ranks]

/-- A selected account is enabled, unblocked and inside the policy. -/
theorem select_eligible {policy : Policy} {now : Time} {family : Family}
    {pool : List Account} {current : Option Nat} {cursor i : Nat}
    (h : select policy now family pool current cursor = some i) :
    ∃ account tier, pool[i]? = some account ∧ tier? policy now family account = some tier := by
  obtain ⟨best, _, hi⟩ := pick_best h
  rw [ranks_getElem?] at hi
  cases hp : pool[i]? with
  | none => simp [hp] at hi
  | some account =>
    cases ht : tier? policy now family account with
    | none => simp [hp, ht] at hi
    | some tier => exact ⟨account, tier, rfl, ht⟩

theorem tier?_overage_requires_policy {policy : Policy} {now : Time} {family : Family}
    {account : Account} (h : tier? policy now family account = some .overage) :
    policy.allowOverage = true := by
  unfold tier? at h
  split at h
  · cases h
  · split at h
    · split at h
      · rename_i hp
        simp only [Bool.and_eq_true] at hp
        exact hp.1
      · cases h
    · split at h <;> cases h

/-- Paid extra usage is drawn only when the operator enabled it. -/
theorem select_no_overage_unless_allowed {policy : Policy} {now : Time} {family : Family}
    {pool : List Account} {current : Option Nat} {cursor i : Nat}
    (hpolicy : policy.allowOverage = false)
    (h : select policy now family pool current cursor = some i) :
    ∃ account, pool[i]? = some account ∧
      (tier? policy now family account = some .clear ∨
        tier? policy now family account = some .warned) := by
  obtain ⟨account, tier, ha, ht⟩ := select_eligible h
  refine ⟨account, ha, ?_⟩
  cases tier with
  | clear => exact Or.inl ht
  | warned => exact Or.inr ht
  | overage =>
    have := tier?_overage_requires_policy ht
    simp [hpolicy] at this

/-- No eligible account sits in a better tier than the selected one. -/
theorem select_best_tier {policy : Policy} {now : Time} {family : Family}
    {pool : List Account} {current : Option Nat} {cursor i j : Nat}
    {account : Account} {tier : Tier}
    (h : select policy now family pool current cursor = some i)
    (hj : pool[j]? = some account) (ht : tier? policy now family account = some tier) :
    ∃ chosen chosenTier, pool[i]? = some chosen ∧
      tier? policy now family chosen = some chosenTier ∧ chosenTier.rank ≤ tier.rank := by
  obtain ⟨best, hb, hi⟩ := pick_best h
  have hjr : (ranks policy now family pool)[j]? = some (some tier.rank) := by
    rw [ranks_getElem?, hj]
    simp [ht]
  have hle := bestRank?_le hb j tier.rank hjr
  rw [ranks_getElem?] at hi
  cases hp : pool[i]? with
  | none => simp [hp] at hi
  | some chosen =>
    cases hct : tier? policy now family chosen with
    | none => simp [hp, hct] at hi
    | some chosenTier =>
      simp [hp, hct] at hi
      exact ⟨chosen, chosenTier, rfl, hct, by omega⟩

/-- A session stays on its account while that account is in the best tier any
eligible account offers. -/
theorem select_sticky {policy : Policy} {now : Time} {family : Family}
    {pool : List Account} {cursor c : Nat} {account : Account} {tier : Tier}
    (hc : pool[c]? = some account) (ht : tier? policy now family account = some tier)
    (hbest : ∀ (j : Nat) (other : Account) (otherTier : Tier), pool[j]? = some other →
      tier? policy now family other = some otherTier → tier.rank ≤ otherTier.rank) :
    select policy now family pool (some c) cursor = some c := by
  have hcr : (ranks policy now family pool)[c]? = some (some tier.rank) := by
    rw [ranks_getElem?, hc]
    simp [ht]
  unfold select pick
  cases hb : bestRank? (ranks policy now family pool) with
  | none => exact absurd hcr (bestRank?_none hb c tier.rank)
  | some best =>
    have hle := bestRank?_le hb c tier.rank hcr
    obtain ⟨j, hj⟩ := bestRank?_mem hb
    rw [ranks_getElem?] at hj
    cases hp : pool[j]? with
    | none => simp [hp] at hj
    | some other =>
      cases hot : tier? policy now family other with
      | none => simp [hp, hot] at hj
      | some otherTier =>
        simp [hp, hot] at hj
        have hge := hbest j other otherTier hp hot
        have heq : best = tier.rank := by omega
        dsimp only
        rw [if_pos (by simp [hit, hcr, heq])]

/-- While any enabled account has headroom for the family, a call is served. -/
theorem select_live {policy : Policy} {now : Time} {family : Family} {pool : List Account}
    (current : Option Nat) (cursor : Nat) {i : Nat} {account : Account} {tier : Tier}
    (ha : pool[i]? = some account) (ht : tier? policy now family account = some tier) :
    (select policy now family pool current cursor).isSome = true := by
  apply pick_live (ranks_length policy now family pool) current cursor (i := i) (r := tier.rank)
  rw [ranks_getElem?, ha]
  simp [ht]

theorem select_none_exhausted {policy : Policy} {now : Time} {family : Family}
    {pool : List Account} {current : Option Nat} {cursor : Nat}
    (h : select policy now family pool current cursor = none) :
    ∀ (i : Nat) (account : Account), pool[i]? = some account →
      tier? policy now family account = none := by
  intro i account ha
  cases ht : tier? policy now family account with
  | none => rfl
  | some tier =>
    have := select_live current cursor ha ht
    rw [h] at this
    cases this

theorem reject_blocked (policy : Policy) (now resetAt : Time) (family : Family)
    (account : Account) : tier? policy now family (reject now resetAt account) = none := by
  have : now < max account.blockedUntil (max resetAt (now + 1)) :=
    Nat.lt_of_lt_of_le (Nat.lt_succ_self now)
      (Nat.le_trans (Nat.le_max_right resetAt (now + 1)) (Nat.le_max_right _ _))
  simp [tier?, reject, this]

def eligibleAt (policy : Policy) (now : Time) (family : Family) (pool : List Account)
    (i : Nat) : Prop :=
  ∃ account tier, pool[i]? = some account ∧ tier? policy now family account = some tier

theorem markRejected_eligible {policy : Policy} {now : Time} {family : Family}
    {pool : List Account} {i : Nat} {resetAt : Time} {j : Nat}
    (h : eligibleAt policy now family (markRejected now pool i resetAt) j) :
    eligibleAt policy now family pool j ∧ j ≠ i := by
  obtain ⟨account, tier, ha, ht⟩ := h
  by_cases hji : j = i
  · subst hji
    cases hp : pool[j]? with
    | none => simp [markRejected, hp] at ha
    | some original =>
      have hlt : j < pool.length := by
        rcases Nat.lt_or_ge j pool.length with hlt | hge
        · exact hlt
        · have hnone : pool[j]? = none := List.getElem?_eq_none (by omega)
          rw [hnone] at hp
          cases hp
      simp [markRejected, hp, hlt] at ha
      subst ha
      rw [reject_blocked] at ht
      cases ht
  · have hother : (markRejected now pool i resetAt)[j]? = pool[j]? := by
      unfold markRejected
      split
      · rfl
      · simp [List.getElem?_set, Ne.symm hji]
    rw [hother] at ha
    exact ⟨⟨account, tier, ha, ht⟩, hji⟩

theorem mem_attempt_eligible {policy : Policy} {now : Time} {family : Family}
    {rejects : Nat → Option Time} :
    ∀ (fuel : Nat) (pool : List Account) (current : Option Nat) (cursor j : Nat),
      j ∈ attempt policy now family rejects fuel pool current cursor →
      eligibleAt policy now family pool j
  | 0, _, _, _, _, h => by simp [attempt] at h
  | fuel + 1, pool, current, cursor, j, h => by
    unfold attempt at h
    cases hs : select policy now family pool current cursor with
    | none => simp [hs] at h
    | some i =>
      cases hr : rejects i with
      | none =>
        simp [hs, hr] at h
        subst h
        exact select_eligible hs
      | some resetAt =>
        simp only [hs, hr, List.mem_cons] at h
        rcases h with rfl | h
        · exact select_eligible hs
        · exact (markRejected_eligible (mem_attempt_eligible fuel _ _ _ _ h)).1

/-- One attempt never resends to an account it already tried. -/
theorem attempt_nodup {policy : Policy} {now : Time} {family : Family}
    {rejects : Nat → Option Time} :
    ∀ (fuel : Nat) (pool : List Account) (current : Option Nat) (cursor : Nat),
      (attempt policy now family rejects fuel pool current cursor).Nodup
  | 0, _, _, _ => by simp [attempt]
  | fuel + 1, pool, current, cursor => by
    unfold attempt
    cases hs : select policy now family pool current cursor with
    | none => simp
    | some i =>
      dsimp only
      cases hr : rejects i with
      | none => simp
      | some resetAt =>
        dsimp only
        refine List.nodup_cons.mpr ⟨?_, attempt_nodup fuel _ _ _⟩
        intro hmem
        exact (markRejected_eligible (mem_attempt_eligible fuel _ _ _ _ hmem)).2 rfl

theorem attempt_length_le {policy : Policy} {now : Time} {family : Family}
    {rejects : Nat → Option Time} :
    ∀ (fuel : Nat) (pool : List Account) (current : Option Nat) (cursor : Nat),
      (attempt policy now family rejects fuel pool current cursor).length ≤ fuel
  | 0, _, _, _ => by simp [attempt]
  | fuel + 1, pool, current, cursor => by
    unfold attempt
    cases select policy now family pool current cursor with
    | none => simp
    | some i =>
      dsimp only
      cases rejects i with
      | none => simp
      | some resetAt =>
        dsimp only
        have := attempt_length_le (policy := policy) (now := now) (family := family)
          (rejects := rejects) fuel (markRejected now pool i resetAt) (some i) cursor
        simp only [List.length_cons]
        omega

/-- An attempt sends nothing only when no account has headroom. -/
theorem attempt_empty_exhausted {policy : Policy} {now : Time} {family : Family}
    {rejects : Nat → Option Time} {fuel : Nat} {pool : List Account}
    {current : Option Nat} {cursor : Nat}
    (h : attempt policy now family rejects (fuel + 1) pool current cursor = []) :
    ∀ (i : Nat) (account : Account), pool[i]? = some account →
      tier? policy now family account = none := by
  apply select_none_exhausted (current := current) (cursor := cursor)
  unfold attempt at h
  cases hs : select policy now family pool current cursor with
  | none => rfl
  | some i =>
    cases hr : rejects i <;> simp [hs, hr] at h

end AccountRotation
