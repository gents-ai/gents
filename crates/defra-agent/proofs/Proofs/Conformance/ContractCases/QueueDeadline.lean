import Proofs.Conformance.ContractCases.Types
import Proofs.Request.Executable
import Proofs.Session.Executable

/-!
# Queue and Deadline Conformance Cases

Finite witness rows for R4a queue admission and claim deadline preservation.
The rows replay existing Lean queue/request executable semantics so Rust
conformance tests can detect drift without re-implementing the proof model.
-/

namespace Conformance.ContractCases

open SessionQueue

def requestIds (entries : List QueueEntry) : List RequestId :=
  entries.map QueueEntry.requestId

def terminalIds (candidates : List RequestId) (state : SessionQueueState) : List RequestId :=
  candidates.filter (fun requestId => if requestId ∈ state.terminal then true else false)

def coalescedPendingCount
    (source : QueueSource)
    (key : QueueKey)
    (entries : List QueueEntry) : Nat :=
  (entries.filter (fun entry =>
    if CoalescedKeyMatch entry source key then true else false)).length

def queueKeyLabel (source : QueueSource) (sessionId : SessionId) : String :=
  source.toDefraDB ++ ":" ++ toString sessionId

def userEntry
    (requestId : RequestId)
    (createdAt : Time)
    (queuedAfter : Option RequestId := none) : QueueEntry :=
  { requestId := requestId
  , createdAt := createdAt
  , source := .user
  , policy := .append
  , queueKey := none
  , queuedAfter := queuedAfter
  }

def subagentCompletionEntry
    (requestId : RequestId)
    (createdAt : Time)
    (sessionId : SessionId) : QueueEntry :=
  { requestId := requestId
  , createdAt := createdAt
  , source := .subagentCompletion
  , policy := .coalesce
  , queueKey := some sessionId
  , queuedAfter := none
  }

def queueState
    (sessionId : SessionId)
    (active : Option RequestId)
    (pending : List QueueEntry)
    (terminal : Finset RequestId := ∅) : SessionQueueState :=
  { sessionId := sessionId
  , active := active
  , pending := pending
  , terminal := terminal
  }

def activeBlocksLaterSameSessionClaimCase : QueueDeadlineConformanceCase :=
  let sessionId := 900
  let pre := queueState sessionId (some 100) [userEntry 101 20 (some 100)]
  let post? := SessionQueue.step? pre .claimNext
  let post := post?.getD pre
  { name := "active_request_blocks_later_same_session_claim"
  , group := "queue_admission"
  , action := "claimNext"
  , sessionId := sessionId
  , legal := post?.isSome
  , preActiveRequestId := pre.active
  , postActiveRequestId := post.active
  , prePendingRequestIds := requestIds pre.pending
  , postPendingRequestIds := requestIds post.pending
  , claimedRequestId := none
  , blockedByActive := decide (pre.active.isSome ∧ post?.isNone)
  , supersededRequestIds := []
  , queueKey := none
  , postCoalescedPendingCount := 0
  , automatedDrainedRequestIds := []
  , preservedUserPendingRequestIds := []
  , postTerminalRequestIds := terminalIds [100, 101] post
  , preRequestDeadline := none
  , synthesizedClaimDeadline := none
  , postDeadline := none
  , explicitDeadlinePreserved := false
  }

def terminalActiveAllowsNextPendingClaimCase : QueueDeadlineConformanceCase :=
  let sessionId := 900
  let pre := queueState sessionId (some 100) [userEntry 101 20 (some 100)]
  match SessionQueue.step? pre .finishActive with
  | some afterFinish =>
      match SessionQueue.step? afterFinish .claimNext with
      | some post =>
          { name := "terminal_active_allows_next_pending_same_session_claim"
          , group := "queue_admission"
          , action := "finishActive_then_claimNext"
          , sessionId := sessionId
          , legal := true
          , preActiveRequestId := pre.active
          , postActiveRequestId := post.active
          , prePendingRequestIds := requestIds pre.pending
          , postPendingRequestIds := requestIds post.pending
          , claimedRequestId := post.active
          , blockedByActive := false
          , supersededRequestIds := []
          , queueKey := none
          , postCoalescedPendingCount := 0
          , automatedDrainedRequestIds := []
          , preservedUserPendingRequestIds := []
          , postTerminalRequestIds := terminalIds [100, 101] post
          , preRequestDeadline := none
          , synthesizedClaimDeadline := none
          , postDeadline := none
          , explicitDeadlinePreserved := false
          }
      | none =>
          { activeBlocksLaterSameSessionClaimCase with
            name := "terminal_active_allows_next_pending_same_session_claim"
          , action := "finishActive_then_claimNext"
          , legal := false
          }
  | none =>
      { activeBlocksLaterSameSessionClaimCase with
        name := "terminal_active_allows_next_pending_same_session_claim"
      , action := "finishActive_then_claimNext"
      , legal := false
      }

def subagentCompletionCoalescesOneWakeupCase : QueueDeadlineConformanceCase :=
  let sessionId := 900
  let pre := queueState sessionId none []
  let first := subagentCompletionEntry 201 10 sessionId
  let duplicate := subagentCompletionEntry 202 11 sessionId
  let post? :=
    match SessionQueue.step? pre (.coalescePending first) with
    | some afterFirst => SessionQueue.step? afterFirst (.coalescePending duplicate)
    | none => none
  let post := post?.getD pre
  { name := "subagent_completion_session_coalesces_one_pending_wakeup"
  , group := "queue_coalesce"
  , action := "coalescePending_twice"
  , sessionId := sessionId
  , legal := post?.isSome
  , preActiveRequestId := pre.active
  , postActiveRequestId := post.active
  , prePendingRequestIds := requestIds pre.pending
  , postPendingRequestIds := requestIds post.pending
  , claimedRequestId := none
  , blockedByActive := false
  , supersededRequestIds := []
  , queueKey := some (queueKeyLabel QueueSource.subagentCompletion sessionId)
  , postCoalescedPendingCount :=
      coalescedPendingCount QueueSource.subagentCompletion sessionId post.pending
  , automatedDrainedRequestIds := []
  , preservedUserPendingRequestIds := []
  , postTerminalRequestIds := terminalIds [201, 202] post
  , preRequestDeadline := none
  , synthesizedClaimDeadline := none
  , postDeadline := none
  , explicitDeadlinePreserved := false
  }

def cancelDrainsAutomatedPreservesUserCase : QueueDeadlineConformanceCase :=
  let sessionId := 900
  let drainKey := sessionId
  let automated := subagentCompletionEntry 301 10 drainKey
  let user := userEntry 302 11 none
  let pre := queueState sessionId none [automated, user]
  let post? := SessionQueue.step? pre (.drainAutomated .subagentCompletion (some drainKey))
  let post := post?.getD pre
  { name := "cancel_drains_automated_wakeups_preserves_user_pending"
  , group := "queue_cancel"
  , action := "drainAutomated"
  , sessionId := sessionId
  , legal := post?.isSome
  , preActiveRequestId := pre.active
  , postActiveRequestId := post.active
  , prePendingRequestIds := requestIds pre.pending
  , postPendingRequestIds := requestIds post.pending
  , claimedRequestId := none
  , blockedByActive := false
  , supersededRequestIds := []
  , queueKey := some (queueKeyLabel QueueSource.subagentCompletion sessionId)
  , postCoalescedPendingCount :=
      coalescedPendingCount QueueSource.subagentCompletion drainKey post.pending
  , automatedDrainedRequestIds := terminalIds [301] post
  , preservedUserPendingRequestIds := [302].filter (fun requestId =>
      if requestId ∈ requestIds post.pending then true else false)
  , postTerminalRequestIds := terminalIds [301, 302] post
  , preRequestDeadline := none
  , synthesizedClaimDeadline := none
  , postDeadline := none
  , explicitDeadlinePreserved := false
  }

def claimPreservesExplicitDeadlineCase : QueueDeadlineConformanceCase :=
  let explicitDeadline := 50
  let pre : RequestContext :=
    { state := .pending
    , origin := .interactive
    , backend := contractBackend
    , admission := .released
    , deadline := 100
    , requestDeadline := some explicitDeadline
    , claimTime := 0
    , currentTime := 50
    , retryCount := 0
    , maxRetries := 3
    , progressSeq := 0
    , messageSeq := 0
    , isLatest := true
    , persistence := .uncommitted
    , validUntil := some 60
    }
  let post? := RequestContext.step? pre .claim
  { name := "claim_preserves_explicit_deadline"
  , group := "claim_deadline"
  , action := "claim"
  , sessionId := 900
  , legal := post?.isSome
  , preActiveRequestId := none
  , postActiveRequestId := none
  , prePendingRequestIds := []
  , postPendingRequestIds := []
  , claimedRequestId := some 401
  , blockedByActive := false
  , supersededRequestIds := []
  , queueKey := none
  , postCoalescedPendingCount := 0
  , automatedDrainedRequestIds := []
  , preservedUserPendingRequestIds := []
  , postTerminalRequestIds := []
  , preRequestDeadline := pre.requestDeadline
  , synthesizedClaimDeadline := some (pre.currentTime + 1)
  , postDeadline := post?.map RequestContext.deadline
  , explicitDeadlinePreserved :=
      post?.map RequestContext.deadline = some explicitDeadline
  }

def queueDeadlineConformanceCases : List QueueDeadlineConformanceCase :=
  [ activeBlocksLaterSameSessionClaimCase
  , terminalActiveAllowsNextPendingClaimCase
  , subagentCompletionCoalescesOneWakeupCase
  , cancelDrainsAutomatedPreservesUserCase
  , claimPreservesExplicitDeadlineCase
  ]

end Conformance.ContractCases
