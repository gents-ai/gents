import Proofs.StreamingResponse.State
import Proofs.CanonicalOutput.ReconstructionGrowth
import Mathlib.Data.List.Perm.Subperm

namespace StreamingResponse

open CanonicalOutput

/-- Reordered immutable arrivals may fill the first gap. They cannot shorten the
selected contiguous list when they preserve earlier successful ordinal lookups. -/
theorem contiguousFlushes_prefix_of_extension
    (before after : List Segment) (writer : Writer)
    (hstable : ∀ ordinal flush, flushAt before writer ordinal = .ok flush →
      flushAt after writer ordinal = .ok flush)
    (fuel laterFuel start : Nat) (initial extended : List Flush)
    (hfuel : fuel ≤ laterFuel)
    (hinitial : contiguousFlushes before writer start fuel = .ok initial)
    (hextended : contiguousFlushes after writer start laterFuel = .ok extended) :
    initial.IsPrefix extended := by
  induction fuel generalizing laterFuel start initial extended with
  | zero =>
      simp only [contiguousFlushes] at hinitial
      cases hinitial
      exact List.nil_prefix
  | succ fuel ih =>
      cases laterFuel with
      | zero => omega
      | succ laterFuel =>
          cases hb : flushAt before writer start with
          | error error =>
              cases error <;> simp only [contiguousFlushes, hb] at hinitial
              all_goals first
                | contradiction
                | (cases hinitial; exact List.nil_prefix)
          | ok flush =>
              have ha := hstable start flush hb
              simp only [contiguousFlushes, hb] at hinitial
              simp only [contiguousFlushes, ha] at hextended
              cases ht : contiguousFlushes before writer (start + 1) fuel with
              | error error =>
                  simp [ht, Except.bind] at hinitial
                  contradiction
              | ok rest =>
                  simp [ht, Except.bind] at hinitial
                  change Except.ok (flush :: rest) = Except.ok initial at hinitial
                  cases hinitial
                  cases hn : contiguousFlushes after writer (start + 1) laterFuel with
                  | error error =>
                      simp [hn, Except.bind] at hextended
                      contradiction
                  | ok next =>
                      simp [hn, Except.bind] at hextended
                      change Except.ok (flush :: next) = Except.ok extended at hextended
                      cases hextended
                      exact List.cons_prefix_cons.mpr
                        ⟨rfl, ih laterFuel (start + 1) rest next (by omega) ht hn⟩

theorem reconstructOpen_witness (observation : Observation) (streams : Streams)
    (h : reconstructOpen observation = .ok streams) :
    ∃ flushes raw,
      contiguousFlushes (Execution.sourceData observation.records observation.target.coordinate)
        observation.target.writer 0
        (Execution.sourceData observation.records observation.target.coordinate).length = .ok flushes ∧
      consumeFlushes flushes [] = .ok raw ∧ streams = visibleStreams raw := by
  simp only [reconstructOpen, reconstructPrefix, Option.getD] at h
  repeat' first
    | contradiction
    | split at h
  cases hs : contiguousFlushes
      (Execution.sourceData observation.records observation.target.coordinate)
      observation.target.writer 0
      (Execution.sourceData observation.records observation.target.coordinate).length with
  | error error => simp [hs, Except.bind] at h; contradiction
  | ok flushes =>
      simp [hs, Except.bind] at h
      change (if flushes = [] then Except.error OpenError.loading else
        match consumeFlushes flushes [] with
        | .ok raw => .ok (visibleStreams raw)
        | .error _ => .error .invalid) = .ok streams at h
      split at h
      · contradiction
      · cases hc : consumeFlushes flushes [] with
        | error error => simp [hc] at h
        | ok raw =>
            simp [hc] at h
            exact ⟨flushes, raw, rfl, hc, h.symm⟩

/-- Benign source arrival is a relation on immutable records, not on rendered
bytes: old facts remain, writer/source stay fixed, and no new competing fact is
introduced at an already observed ordinal. Missing ordinals may freely arrive. -/
structure BenignSourceExtension (before after : Observation) : Prop where
  coordinate : before.target.coordinate = after.target.coordinate
  writer : before.target.writer = after.target.writer
  retained : ∀ record,
    record ∈ Execution.sourceData before.records before.target.coordinate →
    record ∈ Execution.sourceData after.records after.target.coordinate
  noCompeting : ∀ ordinal old new,
    old ∈ Execution.sourceData before.records before.target.coordinate →
    new ∈ Execution.sourceData after.records after.target.coordinate →
    old.flush.any (fun flush => flush.ordinal == ordinal) = true →
    new.flush.any (fun flush => flush.ordinal == ordinal) = true → old = new

/-- Universal non-rewind for the actual shared reconstruction. Publication,
retraction, authorization changes and conflicts are deliberately not benign
extensions; recovery or presentation may intentionally narrow the final view. -/
theorem reconstructOpen_grows_under_benign_arrival
    (before after : Observation) (initial extended : Streams)
    (hextension : BenignSourceExtension before after)
    (hinitial : reconstructOpen before = .ok initial)
    (hextended : reconstructOpen after = .ok extended) : StreamsGrow initial extended := by
  obtain ⟨oldFlushes, oldStreams, hselect, hconsume, rfl⟩ :=
    reconstructOpen_witness before initial hinitial
  obtain ⟨newFlushes, newStreams, hselectNew, hconsumeNew, rfl⟩ :=
    reconstructOpen_witness after extended hextended
  have hstable : ∀ ordinal flush,
      flushAt (Execution.sourceData before.records before.target.coordinate)
        before.target.writer ordinal = .ok flush →
      flushAt (Execution.sourceData after.records after.target.coordinate)
        before.target.writer ordinal = .ok flush := by
    intro ordinal flush hlookup
    obtain ⟨old, hold, hord⟩ := flushAt_success_has_ordinal_record _ _ _ _ hlookup
    have heq := flushAt_preserved_by_benign_extension _ _ before.target.writer ordinal
      hextension.retained (by
        intro record hrecord hordinal
        rw [← hextension.noCompeting ordinal old record hold hrecord hord hordinal]
        exact hold)
    exact heq ▸ hlookup
  have hfuel : (Execution.sourceData before.records before.target.coordinate).length ≤
      (Execution.sourceData after.records after.target.coordinate).length :=
    ((List.nodup_dedup _).subperm hextension.retained).length_le
  rw [← hextension.writer] at hselectNew
  have hp := contiguousFlushes_prefix_of_extension _ _ _ hstable _ _ _ _ _
    hfuel hselect hselectNew
  exact (reconstructed_prefix_grows _ _ _ _ hp hconsume hconsumeNew).filterDeclarations
    (fun declaration => declaration.kind != .opaque)

theorem open_live_view_has_reconstructed_prefix
    (observation : Observation) (streams : Streams)
    (hmessage : observation.target.messageId = none)
    (hscope : targetScoped observation = true)
    (hclose : observeClose observation.records observation.target.coordinate = .open)
    (hview : project observation = .live streams) :
    reconstructOpen observation = .ok streams := by
  simp only [project, hmessage, hscope, Bool.not_true, Bool.false_eq_true,
    ↓reduceIte, hclose] at hview
  split at hview
  · contradiction
  · split at hview
    · cases hr : reconstructOpen observation with
      | error error => cases error <;> simp [hr] at hview
      | ok actual => simp [hr] at hview; cases hview; rfl
    · contradiction

theorem live_views_do_not_rewind_under_benign_arrival
    (before after : Observation) (initial extended : Streams)
    (hextension : BenignSourceExtension before after)
    (hbmessage : before.target.messageId = none) (hamessage : after.target.messageId = none)
    (hbscope : targetScoped before = true) (hascope : targetScoped after = true)
    (hbclose : observeClose before.records before.target.coordinate = .open)
    (haclose : observeClose after.records after.target.coordinate = .open)
    (hbefore : project before = .live initial) (hafter : project after = .live extended) :
    StreamsGrow initial extended :=
  reconstructOpen_grows_under_benign_arrival before after initial extended hextension
    (open_live_view_has_reconstructed_prefix before initial hbmessage hbscope hbclose hbefore)
    (open_live_view_has_reconstructed_prefix after extended hamessage hascope haclose hafter)

end StreamingResponse
