import Proofs.Shared.CompletionRetry.RepeatedToolSuccess
import Proofs.Conformance.Family

namespace Conformance.RepeatedToolSuccessContracts
open Lean CompletionRetry.RepeatedToolSuccess

deriving instance ToJson for CompletionRetry.ErrorIdentity.ErrorText, Event

structure Input where
  name : String
  events : List Event
  deriving ToJson

structure Output where
  counts : List Nat
  stopped : Bool
  deriving ToJson

private def changing (field : Nat) : List Event :=
  (List.range 12).map fun n =>
    ⟨(if field = 0 then n else 0), (if field = 1 then n else 0),
      some ⟨(if field = 2 then n else 0), (if field = 3 then n else 0)⟩, true⟩

@[conformance_family]
def streak : Family Input Output where
  inputs :=
    [ ⟨"empty", []⟩
    , ⟨"seven_continue", List.replicate 7 same⟩
    , ⟨"eight_stop", List.replicate 8 same⟩
    , ⟨"ninth_is_not_dispatched", List.replicate 12 same⟩
    , ⟨"changed_tool", changing 0⟩
    , ⟨"changed_arguments", changing 1⟩
    , ⟨"changed_result", changing 2⟩
    , ⟨"command_volatile_fields_do_not_reset", changing 3⟩
    , ⟨"other_tools_keep_volatile_text", (changing 3).map fun e => { e with commandEnvelope := false }⟩
    , ⟨"non_success_resets", List.replicate 7 same ++ [{ same with result := none }] ++ List.replicate 7 same⟩
    , ⟨"changed_result_starts_a_new_streak", List.replicate 7 same ++ List.replicate 8 { same with result := some ⟨1, 0⟩ }⟩ ]
  model := fun i =>
    let (counts, ending) := run {} i.events
    { counts, stopped := ending }

end Conformance.RepeatedToolSuccessContracts
