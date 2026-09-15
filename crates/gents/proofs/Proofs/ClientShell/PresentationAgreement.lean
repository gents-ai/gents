import Proofs.ClientShell.Projection

/-!
# Client shell presentation agreement

The React composer owns only its local text. For a non-empty local draft it
must present the canonical shell decision unchanged; an empty local draft adds
only the canonical `composerEmpty` blocker. It never reconstructs readiness
from transport, deployment, behavior, or turn observations.
-/

namespace ClientShell
namespace PresentationAgreement

def adaptLocalDraft (composerNonEmpty : Bool) (canonicalNonEmpty : SendDecision) : SendDecision :=
  if composerNonEmpty then canonicalNonEmpty else .blocked .composerEmpty

theorem nonempty_delegates_to_canonical (canonical : SendDecision) :
    adaptLocalDraft true canonical = canonical := rfl

theorem empty_adds_only_composer_blocker (canonical : SendDecision) :
    adaptLocalDraft false canonical = .blocked .composerEmpty := rfl

theorem canonical_blocker_preserved
    (reason : SendBlockedReason) (composerNonEmpty : Bool)
    (h : adaptLocalDraft composerNonEmpty (.blocked reason) = .ready) : False := by
  cases composerNonEmpty <;> simp [adaptLocalDraft] at h

theorem presented_ready_iff_nonempty_and_canonical_ready
    (composerNonEmpty : Bool) (canonical : SendDecision) :
    adaptLocalDraft composerNonEmpty canonical = .ready ↔
      composerNonEmpty = true ∧ canonical = .ready := by
  cases composerNonEmpty <;> cases canonical <;> simp [adaptLocalDraft]

end PresentationAgreement
end ClientShell
