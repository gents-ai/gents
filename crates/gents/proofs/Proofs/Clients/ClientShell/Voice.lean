import Mathlib.Data.Nat.Basic

namespace ClientShell.Voice

inductive Mode where
  | off | dictation | conversation
  deriving DecidableEq, Repr

inductive Phase where
  | idle | recording | transcribing | submitting | waiting | speaking
  deriving DecidableEq, Repr

structure State where
  epoch : Nat := 0
  mode : Mode := .off
  phase : Phase := .idle
  deriving DecidableEq, Repr

/-- Every explicit start and every stop, view disposal, disconnection or scope
change retires the previous epoch. Native capture and playback handles are
released before the editor publishes the retired state. Completing a native
operation never supplies permission to restart a stopped conversation. -/
def retire (s : State) : State := ⟨s.epoch + 1, .off, .idle⟩

def start (s : State) (mode : Mode) : State :=
  if mode == .off then retire s else ⟨s.epoch + 1, mode, .recording⟩

inductive Completion where
  | captured | transcribed | submitted | replied | played | failed
  deriving DecidableEq, Repr

/-- Dictation ends in an editable draft. Only explicitly started conversation
mode admits submission and another capture cycle. A submitted turn is observed
through its exact accepted request identity by the native adapter; an unrelated
session update is not a reply completion. Audio is held only in memory and is
not part of the request transcript. -/
def complete (s : State) (epoch : Nat) (event : Completion) : State :=
  if epoch != s.epoch || s.mode == .off then s else
  match event, s.phase with
  | .failed, _ => retire s
  | .captured, .recording => { s with phase := .transcribing }
  | .transcribed, .transcribing =>
      if s.mode == .dictation then retire s else { s with phase := .submitting }
  | .submitted, .submitting =>
      if s.mode == .conversation then { s with phase := .waiting } else retire s
  | .replied, .waiting =>
      if s.mode == .conversation then { s with phase := .speaking } else retire s
  | .played, .speaking =>
      if s.mode == .conversation then { s with phase := .recording } else retire s
  | _, _ => s

theorem stale_completion_has_no_effect (s : State) (epoch : Nat) (event : Completion)
    (h : epoch ≠ s.epoch) : complete s epoch event = s := by
  simp [complete, h]

theorem stopped_completion_has_no_effect (s : State) (epoch : Nat) (event : Completion)
    (h : s.mode = .off) : complete s epoch event = s := by
  simp [complete, h]

theorem retire_releases_every_mode (s : State) :
    (retire s).mode = .off ∧ (retire s).phase = .idle := ⟨rfl, rfl⟩

theorem dictation_cannot_admit_submission (epoch : Nat) :
    (complete ⟨epoch, .dictation, .transcribing⟩ epoch .transcribed).mode = .off := by
  simp [complete, retire]

end ClientShell.Voice
