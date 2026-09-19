import Proofs.CanonicalOutput.Reconstruction

namespace CanonicalOutput.ClosureCases

def emptySource : Segment :=
  { id := 1
    coordinate := ⟨10, .provider 0 0 0⟩
    writer := .request 7
    flush := none
    close := some (.closed .complete 0 [])
    createdAt := 5 }

theorem zero_stream_source_has_no_payload_reference :
    reconstructExtent [emptySource] emptySource = .ok [] ∧
      resolveClose [emptySource] [] ⟨1, 0⟩ = .error .invalidReference := ⟨rfl, rfl⟩

def emptyText : Segment :=
  { emptySource with
    flush := some ⟨0, [⟨0, 0, some ⟨0, 0, .text⟩⟩], []⟩
    close := some (.closed .«partial» 1 [0]) }

theorem declared_empty_text_is_a_real_payload :
    reconstructPayload [emptyText] [] ⟨1, 0⟩ = .ok (⟨0, 0, .text⟩, []) := rfl

def raw : Segment := { emptyText with close := none }
def terminalOnly : Segment := { emptyText with id := 2, flush := none }

theorem terminal_only_closure_counts_only_prior_flushes :
    reconstructPayload [raw, terminalOnly] [] ⟨2, 0⟩ =
      .ok (⟨0, 0, .text⟩, []) := rfl

theorem reference_to_plain_flush_is_invalid :
    resolveClose [raw, terminalOnly] [] ⟨1, 0⟩ = .error .invalidReference := rfl

def retracted : Segment := { terminalOnly with close := some .retracted }

theorem retraction_cannot_supply_published_payload :
    resolveClose [raw, retracted] [] ⟨2, 0⟩ = .error .invalidReference := rfl

theorem retraction_conflicts_with_a_second_closure :
    resolveClose [raw, terminalOnly, { retracted with id := 3 }] [] ⟨2, 0⟩ =
      .error .conflictingClosures := rfl

theorem final_flush_must_be_last_in_declared_extent :
    let bad := { emptyText with close := some (.closed .complete 2 [0]) }
    reconstructExtent [bad] bad = .error .invalidClosure := rfl

theorem closure_arrival_alone_does_not_prove_empty_text :
    reconstructPayload [terminalOnly] [] ⟨2, 0⟩ =
      .error (.extent (.missingOrdinal 0)) := rfl

end CanonicalOutput.ClosureCases
