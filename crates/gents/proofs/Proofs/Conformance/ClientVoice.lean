import Lean
import Proofs.ClientShell.Voice

namespace Conformance.ClientVoice

open Lean ClientShell.Voice

def modeName : Mode → String
  | .off => "off" | .dictation => "dictation" | .conversation => "conversation"

def phaseName : Phase → String
  | .idle => "idle" | .recording => "recording" | .transcribing => "transcribing"
  | .submitting => "submitting" | .waiting => "waiting" | .speaking => "speaking"

def eventName : Completion → String
  | .captured => "captured" | .transcribed => "transcribed" | .submitted => "submitted"
  | .replied => "replied" | .played => "played" | .failed => "failed"

def stateJson (s : State) : Json := Json.mkObj [
  ("epoch", toJson s.epoch), ("mode", toJson (modeName s.mode)),
  ("phase", toJson (phaseName s.phase))]

def modes : List Mode := [.off, .dictation, .conversation]
def phases : List Phase := [.idle, .recording, .transcribing, .submitting, .waiting, .speaking]
def completions : List Completion := [.captured, .transcribed, .submitted, .replied, .played, .failed]

def cases : List Json :=
  modes.flatMap fun mode => phases.flatMap fun phase =>
  let s : State := ⟨2, mode, phase⟩
  let completeCases := [1, 2, 3].flatMap fun epoch => completions.map fun event =>
    Json.mkObj [("before", stateJson s), ("kind", toJson "complete"),
      ("epoch", toJson epoch), ("event", toJson (eventName event)),
      ("expected", stateJson (complete s epoch event))]
  let startCases := modes.map fun next => Json.mkObj [
    ("before", stateJson s), ("kind", toJson "start"), ("mode", toJson (modeName next)),
    ("expected", stateJson (start s next))]
  completeCases ++ startCases ++ [Json.mkObj [
    ("before", stateJson s), ("kind", toJson "retire"), ("expected", stateJson (retire s))]]

end Conformance.ClientVoice

def main : IO Unit := IO.println (Lean.toJson Conformance.ClientVoice.cases).compress
