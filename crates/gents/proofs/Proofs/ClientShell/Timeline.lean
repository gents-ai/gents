import Proofs.Client

/-!
# Client Shell Timeline Ordering (#608 parity)

Every client shell renders a session's transcript in the same order: messages
interleaved with their tool groups, then the pending turn, then orphan tool
groups and the live-assistant overlay. While an unmaterialized foreground tool
is running, the overlay is placed before the orphan phase so the reasoning that
caused the tool call does not appear after it. Otherwise the overlay remains at
the tail. The *order and the message↔tool-group partition* are semantics a
second shell must reproduce exactly; only the pixels are presentation.

This models `gents_protocol::timeline::build_timeline_order`. The Rust
function is structured in the same four phases this model concatenates:

    buildOrder = body ++ pending ++ overlayBefore ++ orphans ++ overlayAfter

where `body` interleaves each surviving (deduped) message with the tool group it
owns, `orphans` are the tool groups no surviving message owns, and the two tail
phases are gated singletons.

Model boundary: the input message list is taken **already sorted** by the
shell's total sequence order (the Rust sorts first; sort correctness is a
standard fact, not re-derived here). The fence is the interleave / partition /
tail discipline *on top of* that order — which is exactly the part a second
shell re-implements and can get wrong.
-/

namespace ClientShell.Timeline

inductive Role
  | user
  | assistant
  deriving DecidableEq, Repr

/-- The ordering-relevant projection of one transcript message. `seq` is the
sort key and the tool-group attach key; `emitsItem` says whether it contributes
a visible slot; `token` is an opaque presentation-dedup token. -/
structure Msg where
  key : Nat
  seq : Int
  role : Role
  emitsItem : Bool
  token : Option Nat
  deriving DecidableEq, Repr

/-- One ordered slot. A shell maps each to a rich item; the order and identity
of the slots is the shared contract. -/
inductive Slot
  | message (key : Nat) (seq : Int) (role : Role)
  | toolGroup (seq : Int)
  | pending
  | overlay
  deriving DecidableEq, Repr

/-- First-wins dedup by `key`, then by `token`. Threads the seen sets in order,
so the SURVIVING messages keep their input order. -/
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

/-- The kept (deduped) messages, in input order. -/
def kept (msgs : List Msg) : List Msg := dedup [] [] msgs

/-- Does sequence `s` have a tool group? -/
def hasGroup (groups : List Int) (s : Int) : Bool := s ∈ groups

/-- Body slots: each kept message emits its slot (when `emitsItem`) immediately
followed by the tool group it owns (when it owns one). Mirrors the per-message
loop; the `attached` accumulator prevents a repeated sequence from re-emitting a
group. -/
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

/-- The sequences a surviving message attaches a group to. -/
def attachedSeqs (groups : List Int) (msgs : List Msg) : List Int :=
  ((kept msgs).map Msg.seq).filter (fun s => hasGroup groups s)

/-- Orphan groups: those attached to no surviving message. -/
def orphans (groups : List Int) (msgs : List Msg) : List Int :=
  groups.filter (fun s => s ∉ attachedSeqs groups msgs)

/-- Live overlay placement. `beforeOrphans` is set only while a foreground
tool group from the same request is still unmaterialized and running. -/
structure Overlay where
  matchesTrailing : Bool
  beforeOrphans : Bool
  deriving DecidableEq, Repr

/-- The five-phase timeline order. -/
def buildOrder (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) (overlay : Option Overlay) : List Slot :=
  let visibleOverlay :=
    match overlay with
    | some o => if o.matchesTrailing then [] else [Slot.overlay]
    | none => []
  let overlayBefore :=
    match overlay with
    | some o => if o.beforeOrphans then visibleOverlay else []
    | none => []
  let overlayAfter :=
    match overlay with
    | some o => if o.beforeOrphans then [] else visibleOverlay
    | none => []
  body groups msgs
    ++ (if hasPending then [Slot.pending] else [])
    ++ overlayBefore
    ++ (orphans groups msgs).map Slot.toolGroup
    ++ overlayAfter

/-! ## Slot-membership helpers -/

/-- No `Slot.overlay` is produced by the body phase: the interleave emits only
message and tool-group slots. -/
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

/-! ## Overlay: shown iff live, and always last -/

/-- The overlay is emitted exactly when it is present and not a duplicate of the
trailing assistant. -/
theorem overlay_shown_iff (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) (o : Overlay) :
    (Slot.overlay ∈ buildOrder groups msgs hasPending (some o)) ↔ o.matchesTrailing = false := by
  unfold buildOrder
  have hb := overlay_not_in_body groups msgs
  have ho := overlay_not_in_orphans groups msgs
  cases hp : hasPending <;> cases hm : o.matchesTrailing <;>
    cases hplace : o.beforeOrphans <;>
      simp [hp, hm, hplace, List.mem_append, hb, ho]

/-- No overlay slot appears when the overlay is absent. -/
theorem no_overlay_when_absent (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) :
    Slot.overlay ∉ buildOrder groups msgs hasPending none := by
  unfold buildOrder
  have hb := overlay_not_in_body groups msgs
  have ho := overlay_not_in_orphans groups msgs
  cases hp : hasPending <;> simp [hp, List.mem_append, hb, ho]

/-! ## Partition: every tool group is placed exactly once -/

/-- A sequence that a surviving message attaches a group to is a real group. -/
theorem attachedSeqs_subset_groups (groups : List Int) (msgs : List Msg) {s : Int}
    (h : s ∈ attachedSeqs groups msgs) : s ∈ groups := by
  unfold attachedSeqs at h
  rw [List.mem_filter] at h
  simpa [hasGroup] using h.2

/-- **Partition (completeness).** Every tool group is either attached to a
surviving message or an orphan — none is dropped. -/
theorem group_attached_or_orphan (groups : List Int) (msgs : List Msg) {s : Int}
    (h : s ∈ groups) : s ∈ attachedSeqs groups msgs ∨ s ∈ orphans groups msgs := by
  by_cases ha : s ∈ attachedSeqs groups msgs
  · exact Or.inl ha
  · refine Or.inr ?_
    unfold orphans
    rw [List.mem_filter]
    exact ⟨h, by simpa using ha⟩

/-- **Partition (disjointness).** No tool group is both attached and an orphan —
none is placed twice. -/
theorem group_not_both (groups : List Int) (msgs : List Msg) {s : Int}
    (ha : s ∈ attachedSeqs groups msgs) : s ∉ orphans groups msgs := by
  unfold orphans
  rw [List.mem_filter]
  simp [ha]

/-! ## Tail structure -/

/-- The pending turn appears exactly when a turn is pending. -/
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
    | some o =>
        cases o.matchesTrailing <;> cases o.beforeOrphans <;>
          simp [hp, List.mem_append, hb, ho]

/-- **Tail overlay is last.** When no running orphan tool needs the live
reasoning placed before it, an emitted overlay remains the final slot. -/
theorem overlay_is_last (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) (o : Overlay) (hshow : o.matchesTrailing = false)
    (htail : o.beforeOrphans = false) :
    (buildOrder groups msgs hasPending (some o)).getLast? = some Slot.overlay := by
  unfold buildOrder
  simp [hshow, htail, List.getLast?_append]

/-- **Running-tool overlay shape.** When requested, the emitted overlay is
immediately before the complete orphan-group phase. This prevents reasoning
that caused an unmaterialized running tool from appearing after that tool. -/
theorem overlay_before_orphans_shape (groups : List Int) (msgs : List Msg)
    (hasPending : Bool) (o : Overlay) (hshow : o.matchesTrailing = false)
    (hbefore : o.beforeOrphans = true) :
    buildOrder groups msgs hasPending (some o) =
      body groups msgs
        ++ (if hasPending then [Slot.pending] else [])
        ++ [Slot.overlay]
        ++ (orphans groups msgs).map Slot.toolGroup := by
  unfold buildOrder
  simp [hshow, hbefore]

/-! ## Dedup: no message is shown twice -/

/-- The surviving messages have distinct keys, and none was already seen. First-
wins dedup is what stops a shell from rendering the same message twice. -/
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

/-- **Each surviving message key is unique.** -/
theorem kept_keys_nodup (msgs : List Msg) :
    ((kept msgs).map Msg.key).Nodup :=
  (dedup_keys_nodup msgs [] []).1

end ClientShell.Timeline
