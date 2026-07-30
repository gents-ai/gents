import Proofs.Client

namespace ClientShell.Timeline

inductive Role
  | user
  | assistant
  deriving DecidableEq, Repr

structure Msg where
  key : Nat
  seq : Int
  role : Role
  emitsItem : Bool
  token : Option Nat
  deriving DecidableEq, Repr

inductive Slot
  | message (key : Nat) (seq : Int) (role : Role)
  | toolGroup (seq : Int)
  | pending
  | overlay
  deriving DecidableEq, Repr

def dedup (seenKeys : List Nat) (seenTokens : List Nat) : List Msg → List Msg
  | [] => []
  | m :: rest =>
      if m.key ∈ seenKeys then
        dedup seenKeys seenTokens rest
      else
        match m.token with
        | some t =>
            if t ∈ seenTokens then
              dedup (m.key :: seenKeys) seenTokens rest
            else
              m :: dedup (m.key :: seenKeys) (t :: seenTokens) rest
        | none =>
            m :: dedup (m.key :: seenKeys) seenTokens rest

def kept (msgs : List Msg) : List Msg := dedup [] [] msgs

def hasGroup (groups : List Int) (s : Int) : Bool := s ∈ groups

def bodyGo (groups : List Int) (attached : List Int) : List Msg → List Slot
  | [] => []
  | m :: rest =>
      let msgSlots := if m.emitsItem then [Slot.message m.key m.seq m.role] else []
      if hasGroup groups m.seq ∧ m.seq ∉ attached then
        msgSlots ++ Slot.toolGroup m.seq :: bodyGo groups (m.seq :: attached) rest
      else
        msgSlots ++ bodyGo groups attached rest

def body (groups : List Int) (msgs : List Msg) : List Slot :=
  bodyGo groups [] (kept msgs)

def attachedSeqs (groups : List Int) (msgs : List Msg) : List Int :=
  ((kept msgs).map Msg.seq).filter (fun s => hasGroup groups s)

def orphans (groups : List Int) (msgs : List Msg) : List Int :=
  groups.filter (fun s => s ∉ attachedSeqs groups msgs)

structure Overlay where
  matchesTrailing : Bool
  deriving DecidableEq, Repr

def buildOrder (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) (overlay : Option Overlay) : List Slot :=
  body groups msgs
    ++ (if hasPending then [Slot.pending] else [])
    ++ (orphans groups msgs).map Slot.toolGroup
    ++ (match overlay with
        | some o => if o.matchesTrailing then [] else [Slot.overlay]
        | none => [])

theorem overlay_not_in_bodyGo (groups : List Int) (attached : List Int) (ms : List Msg) :
    Slot.overlay ∉ bodyGo groups attached ms := by
  induction ms generalizing attached with
  | nil => simp [bodyGo]
  | cons m rest ih =>
      unfold bodyGo
      by_cases hg : hasGroup groups m.seq ∧ m.seq ∉ attached
      · simp only [hg, if_true]
        cases m.emitsItem <;> simp [List.mem_append, ih]
      · simp only [hg, if_false]
        cases m.emitsItem <;> simp [List.mem_append, ih]

theorem overlay_not_in_body (groups : List Int) (msgs : List Msg) :
    Slot.overlay ∉ body groups msgs :=
  overlay_not_in_bodyGo groups [] (kept msgs)

theorem overlay_not_in_orphans (groups : List Int) (msgs : List Msg) :
    Slot.overlay ∉ (orphans groups msgs).map Slot.toolGroup := by
  simp

theorem overlay_shown_iff (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) (o : Overlay) :
    (Slot.overlay ∈ buildOrder groups msgs hasPending (some o)) ↔ o.matchesTrailing = false := by
  unfold buildOrder
  have hb := overlay_not_in_body groups msgs
  have ho := overlay_not_in_orphans groups msgs
  cases hp : hasPending <;> cases hm : o.matchesTrailing <;>
    simp [hp, hm, List.mem_append, hb, ho]

theorem no_overlay_when_absent (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) :
    Slot.overlay ∉ buildOrder groups msgs hasPending none := by
  unfold buildOrder
  have hb := overlay_not_in_body groups msgs
  have ho := overlay_not_in_orphans groups msgs
  cases hp : hasPending <;> simp [hp, List.mem_append, hb, ho]

theorem attachedSeqs_subset_groups (groups : List Int) (msgs : List Msg) {s : Int}
    (h : s ∈ attachedSeqs groups msgs) : s ∈ groups := by
  unfold attachedSeqs at h
  rw [List.mem_filter] at h
  simpa [hasGroup] using h.2

theorem group_attached_or_orphan (groups : List Int) (msgs : List Msg) {s : Int}
    (h : s ∈ groups) : s ∈ attachedSeqs groups msgs ∨ s ∈ orphans groups msgs := by
  by_cases ha : s ∈ attachedSeqs groups msgs
  · exact Or.inl ha
  · refine Or.inr ?_
    unfold orphans
    rw [List.mem_filter]
    exact ⟨h, by simpa using ha⟩

theorem group_not_both (groups : List Int) (msgs : List Msg) {s : Int}
    (ha : s ∈ attachedSeqs groups msgs) : s ∉ orphans groups msgs := by
  unfold orphans
  rw [List.mem_filter]
  simp [ha]

theorem pending_shown_iff (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) (overlay : Option Overlay) :
    (Slot.pending ∈ buildOrder groups msgs hasPending overlay) ↔ hasPending = true := by
  unfold buildOrder
  have hb : Slot.pending ∉ body groups msgs := by
    unfold body
    generalize (kept msgs) = ks
    generalize ([] : List Int) = acc
    induction ks generalizing acc with
    | nil => simp [bodyGo]
    | cons m rest ih =>
        unfold bodyGo
        by_cases hg : hasGroup groups m.seq ∧ m.seq ∉ acc
        · simp only [hg, if_true]; cases m.emitsItem <;> simp [List.mem_append, ih]
        · simp only [hg, if_false]; cases m.emitsItem <;> simp [List.mem_append, ih]
  have ho : Slot.pending ∉ (orphans groups msgs).map Slot.toolGroup := by simp
  cases hp : hasPending <;>
    cases overlay with
    | none => simp [hp, List.mem_append, hb, ho]
    | some o => cases o.matchesTrailing <;> simp [hp, List.mem_append, hb, ho]

theorem overlay_is_last (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) (o : Overlay) (hshow : o.matchesTrailing = false) :
    (buildOrder groups msgs hasPending (some o)).getLast? = some Slot.overlay := by
  unfold buildOrder
  simp [hshow, List.getLast?_append]

theorem dedup_keys_nodup (msgs : List Msg) :
    ∀ seenKeys seenTokens,
      ((dedup seenKeys seenTokens msgs).map Msg.key).Nodup ∧
        ∀ k ∈ (dedup seenKeys seenTokens msgs).map Msg.key, k ∉ seenKeys := by
  induction msgs with
  | nil => intro _ _; simp [dedup]
  | cons m rest ih =>
      intro seenKeys seenTokens
      by_cases hk : m.key ∈ seenKeys
      · simp only [dedup, hk, if_true]
        exact ih seenKeys seenTokens
      · match hmt : m.token with
        | some t =>
            by_cases ht : t ∈ seenTokens
            · simp only [dedup, hk, if_false, hmt, ht, if_true]
              obtain ⟨hnd, hns⟩ := ih (m.key :: seenKeys) seenTokens
              exact ⟨hnd, fun k hkm => (hns k hkm) ∘ List.mem_cons_of_mem _⟩
            · simp only [dedup, hk, if_false, hmt, ht, if_false, List.map_cons, List.nodup_cons]
              obtain ⟨hnd, hns⟩ := ih (m.key :: seenKeys) (t :: seenTokens)
              refine ⟨⟨fun hmem => hns m.key hmem (List.mem_cons_self ..), hnd⟩, ?_⟩
              intro k hk'
              rcases List.mem_cons.mp hk' with h | h
              · subst h; exact hk
              · exact (hns k h) ∘ List.mem_cons_of_mem _
        | none =>
            simp only [dedup, hk, if_false, hmt, List.map_cons, List.nodup_cons]
            obtain ⟨hnd, hns⟩ := ih (m.key :: seenKeys) seenTokens
            refine ⟨⟨fun hmem => hns m.key hmem (List.mem_cons_self ..), hnd⟩, ?_⟩
            intro k hk'
            rcases List.mem_cons.mp hk' with h | h
            · subst h; exact hk
            · exact (hns k h) ∘ List.mem_cons_of_mem _

theorem kept_keys_nodup (msgs : List Msg) :
    ((kept msgs).map Msg.key).Nodup :=
  (dedup_keys_nodup msgs [] []).1

end ClientShell.Timeline
