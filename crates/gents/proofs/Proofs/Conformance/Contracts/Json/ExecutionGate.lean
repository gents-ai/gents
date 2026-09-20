import Proofs.CanonicalOutput.Execution.GateCases
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.ExecutionGateContracts

open CanonicalOutput.Execution.Gate.Cases
open Conformance.Contracts

structure Case where
  name : String
  observed : Option Bool

def cases : List Case :=
  [ ⟨"foreground_accept_dispatch_close_deliver_terminal", foregroundEndToEnd⟩
  , ⟨"foreground_complete_while_running_rejected", foregroundCompletionWhileRunningRejected⟩
  , ⟨"explicit_background_parent_terminal_late_delivery", backgroundLateDelivery⟩
  , ⟨"recovery_cancels_pending_and_blocks_remote_dispatch", recoveryCancelsPendingAtomically⟩
  , ⟨"recovery_hands_off_running_without_fake_stop", recoveryHandsOffRunning⟩
  , ⟨"foreign_request_same_generation_unchanged", some foreignSameGenerationUnchanged⟩
  , ⟨"delivery_recovery_race_renewal_wins", renewalBeforeRecovery⟩
  , ⟨"stale_writer_loses_after_recovery", recoveryBeforeStaleWriter⟩ ]
  ++ [ ⟨"producer_output_does_not_renew", outputAloneDoesNotRenew⟩
     , ⟨"input_required_tool_wait_explicitly_renews", toolWaitExplicitRenewal⟩
     , ⟨"suspended_holder_cannot_publish", sameTaskWaitDoesNotCommit⟩
     , ⟨"complete_extent_cannot_be_truncated", completeTruncationRejected⟩
     , ⟨"spawned_background_lost_ack_replays", some spawnedAdmissionReplayCases⟩
     , ⟨"corrupt_payload_revocation_preserves_facts", some corruptRevocationCases⟩ ]

def caseJson (value : Case) : String :=
  "{" ++ "\"name\":" ++ jsonString value.name ++ ","
    ++ "\"completed\":" ++ jsonOptionalBool (some (value.observed == some true)) ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

example : cases.all (fun value => value.observed == some true) = true := by
  native_decide

end Conformance.ExecutionGateContracts
