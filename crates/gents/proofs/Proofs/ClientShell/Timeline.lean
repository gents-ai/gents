import Proofs.Client

/-!
# Client Shell Timeline Ordering (#608 parity)

Every client shell renders a session's transcript in the same order: messages
interleaved with their tool groups, with a pending turn immediately before the
first durable message from its request (or at the tail), then orphan tool
groups and the live-assistant overlay. While an unmaterialized foreground tool
is running, the overlay is placed immediately before that tool's orphan group,
without jumping ahead of earlier historical orphan groups. Otherwise the
overlay remains at the tail. The *order and the message↔tool-group partition* are semantics a
second shell must reproduce exactly; only the pixels are presentation.

This models `gents_protocol::timeline::build_timeline_order`. Request-owned
pending inputs are ordered and deduplicated by physical identity. The Rust
function is structured in the same three phases this model concatenates:

    buildOrder = placePendingInputs (dedupPending [] pending) body ++ orphanTail

where `body` interleaves each surviving (deduped) message with the tool group it
owns, `orphans` are the tool groups no surviving message owns, and the final
phase inserts the visible overlay at the tail or immediately before one
identified orphan sequence.

Model boundary: the input message list is taken **already sorted** by the
shell's total sequence order, and tool-group keys are unique (the Rust uses map
keys and sorts first; sort correctness is a
standard fact, not re-derived here). The fence is the interleave / partition /
tail discipline *on top of* that order — which is exactly the part a second
shell re-implements and can get wrong.
-/

namespace ClientShell.Timeline

/-- A page's accounting coverage and an exact prompt-owner lookup are independent.
A missing prompt in a visible page alone is not evidence that it is pending. -/
structure ReadCoverage where
  sessionComplete : Bool
  promptOwnerKnown : Bool
  deriving DecidableEq, Repr

def pendingOwnerAbsent (coverage : ReadCoverage) (materialized : Bool) : Bool :=
  coverage.promptOwnerKnown && !materialized

/-- A physical prompt lookup covers only its own request, even when its rows
are merged with a bounded session page. Native adapters retain scope checks. -/
def pendingRequestOwnerAbsent (coverage : ReadCoverage) (requested observed : Nat)
    (materialized : Bool) : Bool :=
  pendingOwnerAbsent ⟨coverage.sessionComplete,
    coverage.promptOwnerKnown && requested == observed⟩ materialized

theorem unrelated_request_tip_is_not_pending (coverage : ReadCoverage)
    (requested observed : Nat) (different : requested ≠ observed) (materialized : Bool) :
    pendingRequestOwnerAbsent coverage requested observed materialized = false := by
  simp [pendingRequestOwnerAbsent, pendingOwnerAbsent, different]

theorem page_absence_is_not_pending (complete materialized : Bool) :
    pendingOwnerAbsent ⟨complete, false⟩ materialized = false := by
  simp [pendingOwnerAbsent]

theorem exact_prompt_independent_of_history (complete materialized : Bool) :
    pendingOwnerAbsent ⟨complete, true⟩ materialized = !materialized := by
  simp [pendingOwnerAbsent]

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
  | pending (key : Nat)
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

/-- Live overlay placement. A running foreground tool identifies the exact
orphan sequence immediately before which its reasoning belongs. -/
inductive OverlayPlacement
  | tail
  | beforeOrphan (seq : Int)
  deriving DecidableEq, Repr

structure Overlay where
  hasDurableOwner : Bool
  placement : OverlayPlacement
  deriving DecidableEq, Repr

/-- A request-owned pending user turn normally sits at the body tail. During
partial replication, a later message from the same request may arrive first;
then the pending turn is inserted immediately before that message. -/
inductive PendingPlacement
  | tail
  | beforeMessage (seq : Int)
  deriving DecidableEq, Repr

def insertPendingKeyBefore (key : Nat) (target : Int) : List Slot → List Slot
  | [] => [(Slot.pending key)]
  | slot :: rest =>
      match slot with
      | .message _ seq _ =>
          if seq = target then (Slot.pending key) :: slot :: rest
          else slot :: insertPendingKeyBefore key target rest
      | .toolGroup seq =>
          if seq = target then (Slot.pending key) :: slot :: rest
          else slot :: insertPendingKeyBefore key target rest
      | _ => slot :: insertPendingKeyBefore key target rest

def placePending : Option PendingPlacement → List Slot → List Slot
  | none, body => body
  | some .tail, body => body ++ [(Slot.pending 0)]
  | some (.beforeMessage target), body => insertPendingKeyBefore 0 target body

theorem pending_before_matching_message_head
    (target : Int) (key : Nat) (role : Role) (rest : List Slot) :
    placePending (some (.beforeMessage target))
        (Slot.message key target role :: rest) =
      (Slot.pending 0) :: Slot.message key target role :: rest := by
  simp [placePending, insertPendingKeyBefore]

theorem pending_before_matching_tool_group_head
    (target : Int) (rest : List Slot) :
    placePending (some (.beforeMessage target))
        (Slot.toolGroup target :: rest) =
      (Slot.pending 0) :: Slot.toolGroup target :: rest := by
  simp [placePending, insertPendingKeyBefore]

/-- Insert `visibleOverlay` immediately before the first matching orphan.
If the target is absent (a partial-sync race), keep the overlay at the tail. -/
def insertOverlayBefore (target : Int) (visibleOverlay : List Slot) : List Int → List Slot
  | [] => visibleOverlay
  | seq :: rest =>
      if seq = target then
        visibleOverlay ++ Slot.toolGroup seq :: rest.map Slot.toolGroup
      else
        Slot.toolGroup seq :: insertOverlayBefore target visibleOverlay rest

def placeOrphanTail (placement : OverlayPlacement) (visibleOverlay : List Slot)
    (orphanSeqs : List Int) : List Slot :=
  match placement with
  | .tail => orphanSeqs.map Slot.toolGroup ++ visibleOverlay
  | .beforeOrphan target => insertOverlayBefore target visibleOverlay orphanSeqs

def orphanTail (groups : List Int) (msgs : List Msg) (overlay : Option Overlay) : List Slot :=
  let visibleOverlay :=
    match overlay with
    | some o => if o.hasDurableOwner then [] else [Slot.overlay]
    | none => []
  match overlay with
  | some o => placeOrphanTail o.placement visibleOverlay (orphans groups msgs)
  | none => (orphans groups msgs).map Slot.toolGroup

/-- Singleton specialization for the established slot-partition lemmas. -/
def buildSingleOrder (groups : List Int) (msgs : List Msg)
    (pending : Option PendingPlacement) (overlay : Option Overlay) : List Slot :=
  placePending pending (body groups msgs) ++ orphanTail groups msgs overlay

/-- Identity is an interned physical request document reference, not prompt text.
The caller supplies admitted request order and anchors before the first visible
message of the same or a following request. Interrupted admission remains visible
without acquiring canonical authored-message authority. -/
structure PendingInput where
  key : Nat
  placement : PendingPlacement
  deriving DecidableEq, Repr

def dedupPending (seen : List Nat) : List PendingInput → List PendingInput
  | [] => []
  | p :: rest =>
      if p.key ∈ seen then dedupPending seen rest
      else p :: dedupPending (p.key :: seen) rest

def placePendingKey (p : PendingInput) (slots : List Slot) : List Slot :=
  match p.placement with
  | .tail => slots ++ [.pending p.key]
  | .beforeMessage target => insertPendingKeyBefore p.key target slots

def placePendingInputs : List PendingInput → List Slot → List Slot
  | [], slots => slots
  | p :: rest, slots => placePendingInputs rest (placePendingKey p slots)

/-- Request-owned pending observations join the same body and orphan-tail owner.
First physical identity wins; equal anchors and tail positions retain admission
order. Missing anchors fall back to the body tail, before orphan tool groups. -/
def buildOrder (groups : List Int) (msgs : List Msg)
    (pending : List PendingInput) (overlay : Option Overlay) : List Slot :=
  placePendingInputs (dedupPending [] pending) (body groups msgs)
    ++ orphanTail groups msgs overlay

theorem no_pending_preserves_body_and_tail (groups : List Int) (msgs : List Msg)
    (overlay : Option Overlay) :
    buildOrder groups msgs [] overlay = buildSingleOrder groups msgs none overlay := by
  simp [buildOrder, buildSingleOrder, orphanTail, placePendingInputs, dedupPending, placePending]

theorem keyed_pending_before_matching_message_head (p : PendingInput)
    (target : Int) (key : Nat) (role : Role) (rest : List Slot)
    (h : p.placement = .beforeMessage target) :
    placePendingKey p (.message key target role :: rest) =
      .pending p.key :: .message key target role :: rest := by
  simp [placePendingKey, h, insertPendingKeyBefore]

theorem keyed_pending_membership (p : PendingInput) (probe : Nat) (slots : List Slot) :
    (.pending probe ∈ placePendingKey p slots) ↔ probe = p.key ∨ .pending probe ∈ slots := by
  cases p with
  | mk key placement =>
    cases placement with
    | tail => simp [placePendingKey, eq_comm, or_comm]
    | beforeMessage target =>
      simp only [placePendingKey]
      induction slots with
      | nil => simp [insertPendingKeyBefore, eq_comm]
      | cons slot rest ih =>
        cases slot with
        | message k seq role =>
          by_cases h : seq = target <;> simp [insertPendingKeyBefore, h, ih, or_assoc, or_left_comm, or_comm]
        | toolGroup seq =>
          by_cases h : seq = target <;> simp [insertPendingKeyBefore, h, ih, or_assoc, or_left_comm, or_comm]
        | pending k => simp [insertPendingKeyBefore, ih, or_assoc, or_left_comm, or_comm]
        | overlay => simp [insertPendingKeyBefore, ih]

theorem pending_inputs_membership (pending : List PendingInput) (probe : Nat)
    (slots : List Slot) :
    (.pending probe ∈ placePendingInputs pending slots) ↔
      probe ∈ pending.map PendingInput.key ∨ .pending probe ∈ slots := by
  induction pending generalizing slots with
  | nil => simp [placePendingInputs]
  | cons p rest ih =>
    simp [placePendingInputs, ih, keyed_pending_membership, or_assoc, or_left_comm, or_comm]

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

theorem overlay_mem_insertPendingBefore_iff (target : Int) (slots : List Slot) :
    (Slot.overlay ∈ insertPendingKeyBefore 0 target slots) ↔ Slot.overlay ∈ slots := by
  induction slots with
  | nil => simp [insertPendingKeyBefore]
  | cons slot rest ih =>
      cases slot with
      | message key seq role =>
          by_cases h : seq = target <;> simp [insertPendingKeyBefore, h, ih]
      | toolGroup seq =>
          by_cases h : seq = target <;> simp [insertPendingKeyBefore, h, ih]
      | pending key => simp [insertPendingKeyBefore, ih]
      | overlay =>
          change (Slot.overlay ∈ Slot.overlay :: insertPendingKeyBefore 0 target rest) ↔ _
          simp

theorem overlay_mem_placePending_iff (pending : Option PendingPlacement)
    (slots : List Slot) :
    (Slot.overlay ∈ placePending pending slots) ↔ Slot.overlay ∈ slots := by
  cases pending with
  | none => rfl
  | some placement =>
      cases placement with
      | tail => simp [placePending]
      | beforeMessage target => exact overlay_mem_insertPendingBefore_iff target slots

theorem overlay_not_in_orphans (groups : List Int) (msgs : List Msg) :
    Slot.overlay ∉ (orphans groups msgs).map Slot.toolGroup := by
  simp

theorem overlay_mem_insertOverlayBefore_iff (target : Int) (visible : List Slot)
    (orphanSeqs : List Int) :
    (Slot.overlay ∈ insertOverlayBefore target visible orphanSeqs) ↔
      Slot.overlay ∈ visible := by
  induction orphanSeqs with
  | nil => simp [insertOverlayBefore]
  | cons seq rest ih =>
      by_cases htarget : seq = target
      · simp [insertOverlayBefore, htarget]
      · simp [insertOverlayBefore, htarget, ih]

theorem pending_mem_insertOverlayBefore_iff (target : Int) (visible : List Slot)
    (orphanSeqs : List Int) :
    ((Slot.pending 0) ∈ insertOverlayBefore target visible orphanSeqs) ↔
      (Slot.pending 0) ∈ visible := by
  induction orphanSeqs with
  | nil => simp [insertOverlayBefore]
  | cons seq rest ih =>
      by_cases htarget : seq = target
      · simp [insertOverlayBefore, htarget]
      · simp [insertOverlayBefore, htarget, ih]

/-! ## Overlay: shown iff live, and precisely placed -/

/-- The overlay is emitted exactly when it is present and no durable assistant
turn already owns the same request-local content. -/
theorem overlay_shown_iff (groups : List Int) (msgs : List Msg)
    (pending : Option PendingPlacement) (o : Overlay) :
    (Slot.overlay ∈ buildSingleOrder groups msgs pending (some o)) ↔ o.hasDurableOwner = false := by
  unfold buildSingleOrder orphanTail
  have hb := overlay_not_in_body groups msgs
  have ho := overlay_not_in_orphans groups msgs
  have hp := overlay_mem_placePending_iff pending (body groups msgs)
  cases hm : o.hasDurableOwner <;>
    cases hplace : o.placement with
    | tail => simp [hp, hm, hplace, placeOrphanTail, List.mem_append, hb, ho]
    | beforeOrphan target =>
        simp [hp, hm, hplace, placeOrphanTail, List.mem_append, hb,
          overlay_mem_insertOverlayBefore_iff]

/-- No overlay slot appears when the overlay is absent. -/
theorem no_overlay_when_absent (groups : List Int) (msgs : List Msg)
    (pending : Option PendingPlacement) :
    Slot.overlay ∉ buildSingleOrder groups msgs pending none := by
  unfold buildSingleOrder orphanTail
  have hb := overlay_not_in_body groups msgs
  have ho := overlay_not_in_orphans groups msgs
  have hp := overlay_mem_placePending_iff pending (body groups msgs)
  simp [hp, List.mem_append, hb, ho]

/-! ## Tool-group multiplicity in the emitted timeline -/

private theorem group_count_bodyGo (groups attached : List Int) (msgs : List Msg) (seq : Int) :
    (bodyGo groups attached msgs).count (.toolGroup seq) =
      if seq ∈ groups ∧ seq ∉ attached ∧ seq ∈ msgs.map Msg.seq then 1 else 0 := by
  induction msgs generalizing attached with
  | nil => simp [bodyGo]
  | cons m rest ih =>
      by_cases heq : m.seq = seq
      · by_cases hsg : seq ∈ groups <;> by_cases hsa : seq ∈ attached <;>
          cases hemits : m.emitsItem <;>
          simp [bodyGo, List.count_cons, ih, hasGroup, heq, hsg, hsa, hemits]
      · have hne := Ne.symm heq
        by_cases hmg : m.seq ∈ groups <;> by_cases hma : m.seq ∈ attached <;>
          cases hemits : m.emitsItem <;>
          simp [bodyGo, List.count_cons, ih, hasGroup, heq, hne, hmg, hma, hemits]

private theorem group_count_insertPending (pendingKey : Nat) (seq target : Int) (slots : List Slot) :
    (insertPendingKeyBefore pendingKey target slots).count (.toolGroup seq) = slots.count (.toolGroup seq) := by
  induction slots with
  | nil => simp [insertPendingKeyBefore]
  | cons slot rest ih =>
      cases slot with
      | message key position role =>
          by_cases h : position = target <;> simp [insertPendingKeyBefore, h, List.count_cons, ih]
      | toolGroup position =>
          by_cases h : position = target <;> simp [insertPendingKeyBefore, h, List.count_cons, ih]
      | pending key => simp [insertPendingKeyBefore, List.count_cons, ih]
      | overlay => simp [insertPendingKeyBefore, List.count_cons, ih]

private theorem group_count_placePending (seq : Int) (pending : Option PendingPlacement)
    (slots : List Slot) :
    (placePending pending slots).count (.toolGroup seq) = slots.count (.toolGroup seq) := by
  cases pending with
  | none => rfl
  | some placement =>
      cases placement <;> simp [placePending, group_count_insertPending]

private theorem group_count_pendingInputs (seq : Int) (pending : List PendingInput)
    (slots : List Slot) :
    (placePendingInputs pending slots).count (.toolGroup seq) = slots.count (.toolGroup seq) := by
  induction pending generalizing slots with
  | nil => rfl
  | cons p rest ih =>
    rw [placePendingInputs, ih]
    cases h : p.placement <;> simp [placePendingKey, h, group_count_insertPending]

private theorem group_count_map (seq : Int) (groups : List Int) :
    (groups.map Slot.toolGroup).count (.toolGroup seq) = groups.count seq := by
  induction groups with
  | nil => rfl
  | cons head rest ih =>
      by_cases heq : head = seq <;> simp [List.count_cons, ih, heq]

private theorem group_count_insertOverlay (seq target : Int) (visible : List Slot)
    (groups : List Int) :
    (insertOverlayBefore target visible groups).count (.toolGroup seq) =
      visible.count (.toolGroup seq) + groups.count seq := by
  induction groups with
  | nil => simp [insertOverlayBefore]
  | cons head rest ih =>
      by_cases ht : head = target <;>
        by_cases hs : head = seq <;>
        simp_all [insertOverlayBefore, List.count_cons, group_count_map, Nat.add_comm, Nat.add_left_comm, Nat.add_assoc]

private theorem group_count_buildSingleOrder (seq : Int) (groups : List Int) (msgs : List Msg)
    (pending : Option PendingPlacement) (overlay : Option Overlay) :
    (buildSingleOrder groups msgs pending overlay).count (.toolGroup seq) =
      (body groups msgs).count (.toolGroup seq) + (orphans groups msgs).count seq := by
  cases overlay with
  | none => simp [buildSingleOrder, orphanTail, group_count_placePending, group_count_map]
  | some o =>
      cases hshow : o.hasDurableOwner <;> cases hplace : o.placement <;>
        simp [buildSingleOrder, orphanTail, hshow, hplace, placeOrphanTail, group_count_placePending,
          group_count_map, group_count_insertOverlay]

private theorem group_count_buildOrder (seq : Int) (groups : List Int) (msgs : List Msg)
    (pending : List PendingInput) (overlay : Option Overlay) :
    (buildOrder groups msgs pending overlay).count (.toolGroup seq) =
      (body groups msgs).count (.toolGroup seq) + (orphans groups msgs).count seq := by
  have h := group_count_buildSingleOrder seq groups msgs none overlay
  simpa [buildOrder, buildSingleOrder, placePending, group_count_pendingInputs] using h

private theorem nodup_count (groups : List Int) (hunique : groups.Nodup) (seq : Int) :
    groups.count seq = if seq ∈ groups then 1 else 0 := by
  induction groups with
  | nil => simp
  | cons head rest ih =>
      simp only [List.nodup_cons] at hunique
      by_cases heq : head = seq
      · subst head
        simp [List.count_cons, (List.count_eq_zero.mpr hunique.1)]
      · simp [List.count_cons, heq, Ne.symm heq, ih hunique.2]

/-- Runtime tool groups come from unique map keys. Under that explicit adapter
premise, the actual emitted timeline contains each group exactly once. Pending
turns and overlays preserve this count, whatever their placement. -/
theorem tool_group_emitted_once (groups : List Int) (hunique : groups.Nodup)
    (msgs : List Msg) (pending : List PendingInput) (overlay : Option Overlay)
    (seq : Int) :
    (buildOrder groups msgs pending overlay).count (.toolGroup seq) =
      if seq ∈ groups then 1 else 0 := by
  rw [group_count_buildOrder]
  simp only [body, group_count_bodyGo, List.not_mem_nil, true_and]
  by_cases hattached : seq ∈ attachedSeqs groups msgs
  · have horphan : (orphans groups msgs).count seq = 0 := by
      apply List.count_eq_zero.mpr
      simp [orphans, hattached]
    simp only [attachedSeqs, List.mem_filter, List.mem_map] at hattached
    simp only [hasGroup, decide_eq_true_eq] at hattached
    simp [horphan, hattached.1, hattached.2]
  · have hcount : (orphans groups msgs).count seq = groups.count seq := by
      apply List.count_filter
      simpa using hattached
    rw [hcount, nodup_count groups hunique seq]
    have hnone : ¬ (seq ∈ groups ∧ seq ∈ (kept msgs).map Msg.seq) := by
      simpa [attachedSeqs, hasGroup, and_comm] using hattached
    simp only [List.mem_map] at hnone ⊢
    simp only [not_false_eq_true, true_and, hnone, ↓reduceIte, Nat.zero_add]

/-! ## Auxiliary membership partition -/

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
this membership partition alone does not establish output multiplicity. -/
theorem group_not_both (groups : List Int) (msgs : List Msg) {s : Int}
    (ha : s ∈ attachedSeqs groups msgs) : s ∉ orphans groups msgs := by
  unfold orphans
  rw [List.mem_filter]
  simp [ha]

/-! ## Tail structure -/

theorem pending_not_in_body (groups : List Int) (msgs : List Msg) :
    (Slot.pending 0) ∉ body groups msgs := by
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

theorem pending_mem_insertPendingBefore (target : Int) (slots : List Slot) :
    (Slot.pending 0) ∈ insertPendingKeyBefore 0 target slots := by
  induction slots with
  | nil => simp [insertPendingKeyBefore]
  | cons slot rest ih =>
      cases slot with
      | message key seq role =>
          by_cases h : seq = target <;> simp [insertPendingKeyBefore, h, ih]
      | toolGroup seq =>
          by_cases h : seq = target <;> simp [insertPendingKeyBefore, h, ih]
      | pending key => simp [insertPendingKeyBefore, ih]
      | overlay => simp [insertPendingKeyBefore, ih]

theorem pending_mem_placePending_iff (pending : Option PendingPlacement)
    (slots : List Slot) (habsent : (Slot.pending 0) ∉ slots) :
    ((Slot.pending 0) ∈ placePending pending slots) ↔ pending.isSome := by
  cases pending with
  | none => simp [placePending, habsent]
  | some placement =>
      cases placement with
      | tail => simp [placePending]
      | beforeMessage target => simp [placePending, pending_mem_insertPendingBefore]

/-- The pending turn appears exactly when a turn is pending. -/
theorem pending_shown_iff (groups : List Int) (msgs : List Msg)
    (pending : Option PendingPlacement) (overlay : Option Overlay) :
    ((Slot.pending 0) ∈ buildSingleOrder groups msgs pending overlay) ↔ pending.isSome := by
  unfold buildSingleOrder orphanTail
  have hb := pending_not_in_body groups msgs
  have hp := pending_mem_placePending_iff pending (body groups msgs) hb
  have ho : (Slot.pending 0) ∉ (orphans groups msgs).map Slot.toolGroup := by simp
  cases overlay with
  | none => simpa [List.mem_append, ho] using hp
  | some o =>
      cases hm : o.hasDurableOwner <;> cases hplace : o.placement with
      | tail =>
          simpa [hm, hplace, placeOrphanTail, List.mem_append, ho] using hp
      | beforeOrphan target =>
          simpa [hm, hplace, placeOrphanTail, List.mem_append, ho,
            pending_mem_insertOverlayBefore_iff] using hp

/-- **Tail overlay is last.** When no running orphan tool needs the live
reasoning placed before it, an emitted overlay remains the final slot. -/
theorem overlay_is_last (groups : List Int) (msgs : List Msg)
    (pending : Option PendingPlacement) (o : Overlay) (hshow : o.hasDurableOwner = false)
    (htail : o.placement = .tail) :
    (buildSingleOrder groups msgs pending (some o)).getLast? = some Slot.overlay := by
  unfold buildSingleOrder orphanTail
  simp [hshow, htail, placeOrphanTail, List.getLast?_append]

/-- Prefix groups remain before the overlay when it targets a later orphan. -/
theorem insertOverlayBefore_prefix (target : Int) (visible : List Slot)
    (earlier suffix : List Int) (hnot : target ∉ earlier) :
    insertOverlayBefore target visible (earlier ++ target :: suffix) =
      earlier.map Slot.toolGroup
        ++ visible
        ++ Slot.toolGroup target :: suffix.map Slot.toolGroup := by
  induction earlier with
  | nil => simp [insertOverlayBefore]
  | cons seq rest ih =>
      simp only [List.mem_cons, not_or] at hnot
      have hseq : seq ≠ target := Ne.symm hnot.1
      simp [insertOverlayBefore, hseq, ih hnot.2, List.append_assoc]

/-- **Running-tool overlay shape.** The emitted overlay appears immediately
before its target orphan while all earlier historical orphans stay earlier. -/
theorem overlay_before_target_shape (groups : List Int) (msgs : List Msg)
    (pending : Option PendingPlacement) (o : Overlay) (target : Int) (earlier suffix : List Int)
    (hshow : o.hasDurableOwner = false)
    (hplace : o.placement = .beforeOrphan target)
    (horphans : orphans groups msgs = earlier ++ target :: suffix)
    (hnot : target ∉ earlier) :
    buildSingleOrder groups msgs pending (some o) =
      placePending pending (body groups msgs)
        ++ earlier.map Slot.toolGroup
        ++ [Slot.overlay]
        ++ Slot.toolGroup target :: suffix.map Slot.toolGroup := by
  unfold buildSingleOrder orphanTail
  simp [hshow, hplace, placeOrphanTail, horphans,
    insertOverlayBefore_prefix target [Slot.overlay] earlier suffix hnot]

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

theorem dedup_pending_key_membership (seen : List Nat) (pending : List PendingInput)
    (probe : Nat) :
    probe ∈ (dedupPending seen pending).map PendingInput.key ↔
      probe ∈ pending.map PendingInput.key ∧ probe ∉ seen := by
  induction pending generalizing seen with
  | nil => simp [dedupPending]
  | cons p rest ih =>
    by_cases h : p.key ∈ seen <;> by_cases hp : probe = p.key <;>
      simp_all [dedupPending]

theorem pending_keys_nodup (seen : List Nat) (pending : List PendingInput) :
    ((dedupPending seen pending).map PendingInput.key).Nodup := by
  induction pending generalizing seen with
  | nil => simp [dedupPending]
  | cons p rest ih =>
    by_cases h : p.key ∈ seen
    · simpa [dedupPending, h] using ih seen
    · simp [dedupPending, h, ih, dedup_pending_key_membership]

theorem equal_anchor_preserves_request_order (first second : Nat) (target : Int)
    (key : Nat) (role : Role) (rest : List Slot) :
    placePendingInputs [⟨first, .beforeMessage target⟩, ⟨second, .beforeMessage target⟩]
      (.message key target role :: rest) =
      .pending first :: .pending second :: .message key target role :: rest := by
  simp [placePendingInputs, placePendingKey, insertPendingKeyBefore]

theorem tail_preserves_request_order (first second : Nat) (slots : List Slot) :
    placePendingInputs [⟨first, .tail⟩, ⟨second, .tail⟩] slots =
      slots ++ [.pending first, .pending second] := by
  simp [placePendingInputs, placePendingKey, List.append_assoc]

theorem keyed_pending_not_in_body (probe : Nat) (groups : List Int) (msgs : List Msg) :
    Slot.pending probe ∉ body groups msgs := by
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

theorem keyed_pending_not_in_orphanTail (probe : Nat) (groups : List Int)
    (msgs : List Msg) (overlay : Option Overlay) :
    Slot.pending probe ∉ orphanTail groups msgs overlay := by
  cases overlay with
  | none => simp [orphanTail]
  | some o =>
    cases hplace : o.placement with
    | tail => cases hshow : o.hasDurableOwner <;> simp [orphanTail, hshow, hplace, placeOrphanTail]
    | beforeOrphan target =>
      cases hshow : o.hasDurableOwner <;>
        simp only [orphanTail, hshow, hplace, placeOrphanTail] <;>
        generalize orphans groups msgs = seqs <;>
        induction seqs with
        | nil => simp [insertOverlayBefore]
        | cons seq rest ih =>
          by_cases h : seq = target <;> simp_all [insertOverlayBefore]

/-- A pending physical request identity is shown exactly when supplied by the
request projection; content equality never suppresses distinct admissions. -/
theorem pending_identity_shown_iff (groups : List Int) (msgs : List Msg)
    (pending : List PendingInput) (overlay : Option Overlay) (probe : Nat) :
    Slot.pending probe ∈ buildOrder groups msgs pending overlay ↔
      probe ∈ pending.map PendingInput.key := by
  simp [buildOrder, pending_inputs_membership, dedup_pending_key_membership,
    keyed_pending_not_in_body, keyed_pending_not_in_orphanTail]

theorem keyed_overlay_mem_insertPendingBefore_iff (pendingKey : Nat) (target : Int) (slots : List Slot) :
    (Slot.overlay ∈ insertPendingKeyBefore pendingKey target slots) ↔ Slot.overlay ∈ slots := by
  induction slots with
  | nil => simp [insertPendingKeyBefore]
  | cons slot rest ih =>
      cases slot with
      | message key seq role =>
          by_cases h : seq = target <;> simp [insertPendingKeyBefore, h, ih]
      | toolGroup seq =>
          by_cases h : seq = target <;> simp [insertPendingKeyBefore, h, ih]
      | pending key => simp [insertPendingKeyBefore, ih]
      | overlay =>
          change (Slot.overlay ∈ Slot.overlay :: insertPendingKeyBefore pendingKey target rest) ↔ _
          simp

theorem overlay_mem_pendingInputs_iff (pending : List PendingInput) (slots : List Slot) :
    (Slot.overlay ∈ placePendingInputs pending slots) ↔ Slot.overlay ∈ slots := by
  induction pending generalizing slots with
  | nil => rfl
  | cons p rest ih =>
    rw [placePendingInputs, ih]
    cases h : p.placement <;>
      simp [placePendingKey, h, keyed_overlay_mem_insertPendingBefore_iff]

theorem plural_overlay_shown_iff (groups : List Int) (msgs : List Msg)
    (pending : List PendingInput) (o : Overlay) :
    (Slot.overlay ∈ buildOrder groups msgs pending (some o)) ↔ o.hasDurableOwner = false := by
  have h := overlay_shown_iff groups msgs none o
  simpa [buildOrder, buildSingleOrder, placePending, overlay_mem_pendingInputs_iff] using h

end ClientShell.Timeline
