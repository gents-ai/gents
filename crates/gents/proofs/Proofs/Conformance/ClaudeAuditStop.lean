import Proofs.Session.PromptAssembly.ClaudeAuditStop
import Proofs.Conformance.Family

namespace Conformance.ClaudeAuditStop

open Lean

inductive ProviderStepKind where
  | blockStartText
  | blockStartThinking
  | blockStartToolUse
  | thinkingText
  | signature
  | blockStop
  deriving ToJson

structure ProviderStep where
  kind : ProviderStepKind
  index : Nat
  deriving ToJson

structure ProviderCase where
  steps : List ProviderStep
  deriving ToJson

inductive ProviderVerdict where
  | rejected
  | accepted
  | refusedStop
  deriving ToJson

structure ProviderStepOutcome where
  verdict : ProviderVerdict
  reasoningStop : Option Nat
  deriving ToJson

structure ProviderOutcome where
  steps : List ProviderStepOutcome
  deriving ToJson

def ProviderStepKind.toEvent (kind : ProviderStepKind) (index : Nat) :
    PromptAssembly.ClaudeAuditStop.Event :=
  match kind with
  | .blockStartText => .blockStart index .text
  | .blockStartThinking => .blockStart index .thinking
  | .blockStartToolUse => .blockStart index .toolUse
  | .thinkingText => .thinkingText index
  | .signature => .signature index
  | .blockStop => .blockStop index

def ProviderStepOutcome.ofObservation :
    PromptAssembly.ClaudeAuditStop.Observation → ProviderStepOutcome
  | .rejected => { verdict := .rejected, reasoningStop := none }
  | .accepted reasoningStop => { verdict := .accepted, reasoningStop := reasoningStop }
  | .refusedStop => { verdict := .refusedStop, reasoningStop := none }

@[conformance_family]
def reasoningStop : Conformance.Family ProviderCase ProviderOutcome where
  inputs := [
    { steps := [
        { kind := .blockStartThinking, index := 0 },
        { kind := .thinkingText, index := 0 },
        { kind := .signature, index := 0 },
        { kind := .blockStop, index := 0 },
        { kind := .blockStop, index := 0 },
        { kind := .blockStartText, index := 1 },
        { kind := .blockStop, index := 1 }] },
    { steps := [
        { kind := .blockStartText, index := 0 },
        { kind := .blockStop, index := 0 },
        { kind := .blockStartThinking, index := 1 },
        { kind := .thinkingText, index := 1 },
        { kind := .signature, index := 1 },
        { kind := .blockStop, index := 1 }] },
    { steps := [
        { kind := .blockStartToolUse, index := 0 },
        { kind := .blockStartThinking, index := 1 },
        { kind := .thinkingText, index := 1 },
        { kind := .signature, index := 1 },
        { kind := .blockStop, index := 1 }] } ]
  model := fun c => { steps := (PromptAssembly.ClaudeAuditStop.trace
      (c.steps.map fun step => step.kind.toEvent step.index)).map ProviderStepOutcome.ofObservation }

end Conformance.ClaudeAuditStop
