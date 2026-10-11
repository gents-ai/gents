import Proofs.Session.Management
import Proofs.Session.FoldCases

namespace SessionQueue.ManagementCases

open FoldCases

def active : QueueEntry := user 1

def pending (id : Nat) (delivery : QueueDelivery := .steer) : QueueEntry :=
  { user id (some 1) with delivery }

def initial (entries : List QueueEntry) : SessionQueueState :=
  { queue with active := some 1, pending := entries }

inductive Operation where
  | intake (active : QueueEntry) (admitted : List RequestId) (safeBoundary : Bool)
  | replace (caller : Option Nat) (expected : List RequestId) (offset count : Nat)
      (replacements : List QueueEntry)

def execute (before : SessionQueueState) : Operation → Option SessionQueueState
  | .intake active admitted safe => intakeSteering? queue.scope before active admitted safe
  | .replace caller expected offset count replacements =>
      replacePendingGroup? queue.scope before caller expected offset count replacements

structure Case where
  name : String
  before : SessionQueueState
  operation : Operation

instance : Inhabited Case := ⟨⟨"", initial [], .intake active [] false⟩⟩

def Case.after (c : Case) : Option SessionQueueState := execute c.before c.operation

def intakeCase (name : String) (entries : List QueueEntry) (admitted : List RequestId)
    (safe : Bool := true) (observed : QueueEntry := active) : Case :=
  ⟨name, initial entries, .intake observed admitted safe⟩

def replacement (id slot : Nat) : QueueEntry :=
  { user id (some 1) with orderKey := slot, delivery := .steer }

def managementCase (name : String) (entries : List QueueEntry) (expected : List RequestId)
    (offset count : Nat) (fresh : List QueueEntry) (caller : Option Nat := some 1) : Case :=
  let before := initial entries
  ⟨name, before, .replace caller expected offset count fresh⟩

def cases : List Case :=
  [ intakeCase "steer_contiguous_admitted" [pending 2, pending 3] [2,3]
  , intakeCase "queue_is_barrier" [pending 2 .queue, pending 3] [2,3]
  , intakeCase "unadmitted_is_barrier" [pending 2, pending 3] [3]
  , intakeCase "unsafe_provider_boundary" [pending 2] [2] false
  , intakeCase "stale_active_observation" [pending 2] [2] true (user 9)
  , intakeCase "foreign_requester_is_barrier"
      [{ pending 2 with requester := some 2 }, pending 3] [2,3]
  , intakeCase "agent_message_same_authority"
      [{ pending 2 with source := .steering }] [2]
  , managementCase "edit_fresh_same_slot" [pending 2, pending 3] [2,3] 0 1 [replacement 4 2]
  , managementCase "reorder_content_uses_original_slots" [pending 2, pending 3] [2,3] 0 2
      [replacement 5 2, replacement 4 3]
  , managementCase "cancel_pending_group" [pending 2, pending 3] [2,3] 0 1 []
  , managementCase "stale_pending_snapshot" [pending 2, pending 3] [2] 0 1 [replacement 4 2]
  , managementCase "reused_request_id" [pending 2] [2] 0 1 [replacement 2 2]
  , managementCase "changed_slot_rejected" [pending 2] [2] 0 1 [replacement 4 8]
  , managementCase "foreign_group_rejected" [{ pending 2 with requester := some 2 }]
      [2] 0 1 [replacement 4 2]
  , managementCase "context_barrier_rejected" [pending 2, { pending 3 with turnContext := 7 }]
      [2,3] 0 2 [replacement 4 2, replacement 5 3]
  , ⟨"cancel_selected_unpublished", { initial [pending 3] with folding := [pending 2] },
      .replace (some 1) [2,3] 0 1 []⟩
  , ⟨"edit_selected_unpublished", { initial [pending 3] with folding := [pending 2] },
      .replace (some 1) [2,3] 0 1 [replacement 4 2]⟩
  , intakeCase "expired_steering_is_barrier" [{pending 2 with fresh := false}, pending 3] [2,3]
  , intakeCase "malformed_ttl_steering_is_barrier" [{pending 2 with fresh := false}, pending 3] [2,3]
  , managementCase "expired_pending_edit_rejected" [{pending 2 with fresh := false}] [2] 0 1 [replacement 4 2]
  , managementCase "malformed_ttl_edit_rejected" [{pending 2 with fresh := false}] [2] 0 1 [replacement 4 2]

  ]

theorem intake_and_management_decisions :
    (cases.map fun c => c.after.isSome) =
      [true,true,true,false,false,true,true,true,true,true,false,false,false,false,false,true,true,true,true,false,false] := by
  native_decide

theorem selected_exact_contiguous_prefix :
    ((cases[0]!).after.map fun s => (s.active, s.folding.map (·.requestId), s.pending.map (·.requestId))) =
      some (some 1, [2,3], []) := by native_decide

theorem queue_and_unadmitted_barriers_preserve_pending :
    ([cases[1]!, cases[2]!, cases[5]!].map fun c =>
      c.after.map fun s => (s.folding.map (·.requestId), s.pending.map (·.requestId))) =
      [some ([],[2,3]), some ([],[2,3]), some ([],[2,3])] := by native_decide

theorem replacement_retains_slot_and_fresh_issuance :
    ((cases[8]!).after.map fun s => s.pending.map fun e =>
      (e.requestId, e.orderKey, e.createdAt)) = some [(5,2,5),(4,3,4)] := by native_decide

theorem selected_edit_invalidates_provisional_selection :
    ([cases[15]!, cases[16]!].map fun c => c.after.map fun s =>
      (s.active, s.folding.map (·.requestId), s.pending.map (·.requestId))) =
      [some (some 1, [], [3]), some (some 1, [], [4,3])] := by native_decide


theorem expired_and_malformed_are_steering_barriers :
    ([cases[17]!,cases[18]!].map fun c => c.after.map fun s =>
      (s.folding.map (·.requestId),s.pending.map (·.requestId))) =
      [some ([],[2,3]),some ([],[2,3])] := by native_decide

theorem expired_and_malformed_edits_are_rejected :
    ([cases[19]!,cases[20]!].map fun c => c.after.isSome) = [false,false] := by native_decide

end SessionQueue.ManagementCases
