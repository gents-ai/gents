import Proofs.CanonicalOutput.Reconstruction

namespace CanonicalOutput

theorem uniqueRecord_success_mem {α ε : Type} [DecidableEq α]
    (missing conflict : ε) (records : List α) (record : α)
    (h : uniqueRecord missing conflict records = .ok record) : record ∈ records := by
  apply List.mem_dedup.mp
  rw [(uniqueRecord_eq_ok_iff missing conflict records record).mp h]
  simp

theorem flushAt_success_has_ordinal_record
    (records : List Segment) (writer : Writer) (ordinal : Nat) (flush : Flush)
    (h : flushAt records writer ordinal = .ok flush) :
    ∃ record, record ∈ records ∧
      record.flush.any (fun value => value.ordinal == ordinal) = true := by
  unfold flushAt at h
  split at h
  · contradiction
  · rename_i record hrecord
    have hm := uniqueRecord_success_mem _ _ _ record hrecord
    exact ⟨record, (List.mem_filter.mp hm).1, (List.mem_filter.mp hm).2⟩

theorem uniqueRecord_same_members {α ε : Type} [DecidableEq α]
    (missing conflict : ε) (left right : List α)
    (hmembers : ∀ value, value ∈ left ↔ value ∈ right) :
    uniqueRecord missing conflict left = uniqueRecord missing conflict right := by
  have hp : left.dedup.Perm right.dedup :=
    (List.perm_ext_iff_of_nodup (List.nodup_dedup _) (List.nodup_dedup _)).mpr
      (by simpa only [List.mem_dedup] using hmembers)
  have h := uniqueRecord_order_independent missing conflict left.dedup right.dedup hp
  simpa only [uniqueRecord, List.dedup_idem] using h

/-- Immutable arrivals preserve an existing ordinal when every old record is
retained and no distinct fact is introduced at that ordinal. Exact duplicate
delivery and arbitrary list reordering are allowed. New ordinals are unrestricted;
callers apply this to the earlier contiguous prefix, not every later gap. -/
theorem flushAt_preserved_by_benign_extension
    (before after : List Segment) (writer : Writer) (ordinal : Nat)
    (hretained : ∀ record, record ∈ before → record ∈ after)
    (hnoNew : ∀ record, record ∈ after →
      record.flush.any (fun flush => flush.ordinal == ordinal) = true → record ∈ before) :
    flushAt before writer ordinal = flushAt after writer ordinal := by
  have hlookup := uniqueRecord_same_members
    (ExtentError.missingOrdinal ordinal) (ExtentError.conflictingOrdinal ordinal)
    (before.filter fun record => record.flush.any (fun flush => flush.ordinal == ordinal))
    (after.filter fun record => record.flush.any (fun flush => flush.ordinal == ordinal))
    (by
      intro record
      simp only [List.mem_filter]
      exact ⟨fun h => ⟨hretained record h.1, h.2⟩,
        fun h => ⟨hnoNew record h.1 h.2, h.2⟩⟩)
  simp only [flushAt, hlookup]

/-- A preview extension preserves every existing stream's declaration and byte
prefix. Newly declared streams may appear only after the existing streams. -/
inductive StreamsGrow : Streams → Streams → Prop
  | nil (after : Streams) : StreamsGrow [] after
  | cons {declaration : Declaration} {before after : List UInt8}
      {rest next : Streams} (bytes : before.IsPrefix after)
      (streams : StreamsGrow rest next) :
      StreamsGrow ((declaration, before) :: rest) ((declaration, after) :: next)

theorem StreamsGrow.refl (streams : Streams) : StreamsGrow streams streams := by
  induction streams with
  | nil => exact .nil _
  | cons stream rest ih => exact .cons ⟨[], by simp⟩ ih

theorem StreamsGrow.trans {before middle after : Streams}
    (first : StreamsGrow before middle) (second : StreamsGrow middle after) :
    StreamsGrow before after := by
  induction first generalizing after with
  | nil => exact .nil _
  | cons bytes streams ih =>
      cases second with
      | cons later rest => exact .cons (bytes.trans later) (ih rest)

theorem StreamsGrow.filterDeclarations (keep : Declaration → Bool)
    {before after : Streams} (h : StreamsGrow before after) :
    StreamsGrow (before.filter fun stream => keep stream.1)
      (after.filter fun stream => keep stream.1) := by
  induction h with
  | nil => exact .nil _
  | @cons declaration old new rest next bytes streams ih =>
      cases hk : keep declaration <;> simp only [List.filter_cons, hk, Bool.false_eq_true,
        ↓reduceIte, List.filter] <;> first | exact ih | exact .cons bytes ih

theorem streamsGrow_append (streams suffix : Streams) :
    StreamsGrow streams (streams ++ suffix) := by
  induction streams with
  | nil => exact .nil _
  | cons stream rest ih => exact .cons ⟨[], by simp⟩ ih

theorem streamsGrow_modify (streams : Streams) (index : Nat) (bytes : List UInt8) :
    StreamsGrow streams (streams.modify (fun old => (old.1, old.2 ++ bytes)) index) := by
  induction streams generalizing index with
  | nil => exact .nil _
  | cons stream rest ih =>
      cases index with
      | zero => exact .cons ⟨bytes, rfl⟩ (StreamsGrow.refl rest)
      | succ index => exact .cons ⟨[], by simp⟩ (ih index)

theorem consumeRuns_grows (runs : List Run) (bytes : List UInt8)
    (before after : Streams) (h : consumeRuns runs bytes before = .ok after) :
    StreamsGrow before after := by
  induction runs generalizing bytes before with
  | nil =>
      cases bytes <;> simp only [consumeRuns] at h
      · cases h; exact StreamsGrow.refl _
      · contradiction
  | cons run rest ih =>
      simp only [consumeRuns] at h
      repeat' first
        | contradiction
        | (solve | exact (streamsGrow_append _ _).trans (ih _ _ h))
        | (solve | exact (streamsGrow_modify _ _ _).trans (ih _ _ h))
        | split at h

theorem consumeFlushes_grows (flushes : List Flush) (before after : Streams)
    (h : consumeFlushes flushes before = .ok after) : StreamsGrow before after := by
  induction flushes generalizing before with
  | nil => cases h; exact StreamsGrow.refl _
  | cons flush rest ih =>
      simp only [consumeFlushes] at h
      split at h
      · contradiction
      · cases hr : consumeRuns flush.runs flush.payload before with
        | error error => simp [hr, Except.bind] at h; contradiction
        | ok next =>
            simp [hr, Except.bind] at h
            exact (consumeRuns_grows _ _ _ _ hr).trans (ih _ h)

theorem consumeFlushes_append (initial suffix : List Flush) (before : Streams) :
    consumeFlushes (initial ++ suffix) before =
      (consumeFlushes initial before).bind (consumeFlushes suffix) := by
  induction initial generalizing before with
  | nil => rfl
  | cons flush rest ih =>
      simp only [List.cons_append, consumeFlushes]
      split
      · rfl
      · cases hr : consumeRuns flush.runs flush.payload before with
        | error error => rfl
        | ok next => exact ih next

/-- Once ordinal selection establishes that a later contiguous flush sequence
extends the earlier one, successful reconstruction cannot rewind any stream. -/
theorem reconstructed_prefix_grows (initial extended : List Flush)
    (before after : Streams) (hprefix : initial.IsPrefix extended)
    (hbefore : consumeFlushes initial [] = .ok before)
    (hafter : consumeFlushes extended [] = .ok after) : StreamsGrow before after := by
  obtain ⟨suffix, rfl⟩ := hprefix
  rw [consumeFlushes_append, hbefore] at hafter
  exact consumeFlushes_grows suffix before after hafter

end CanonicalOutput
