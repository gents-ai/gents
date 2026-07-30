import Proofs.Compaction.Properties

namespace Compaction

structure CompactionReducerCase where
  name                : String
  group               : String
  reducer             : String
  legal               : Bool
  preMessageCount     : Nat
  postMessageCount    : Nat
  preservesPairs      : Bool
  preservesOrder      : Bool
  gateOpen            : Bool
  safeToReduce        : Bool
  reducerIsIdentity   : Bool
  deriving Repr

def compactionReducerCases : List CompactionReducerCase := [
  { name              := "identity_reducer_is_no_op"
  , group             := "witness"
  , reducer           := "identity"
  , legal             := true
  , preMessageCount   := 0
  , postMessageCount  := 0
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "identity_preserves_pair_atomicity"
  , group             := "witness"
  , reducer           := "identity"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "identity_preserves_message_order"
  , group             := "witness"
  , reducer           := "identity"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := false
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "strip_preserves_pair_atomicity"
  , group             := "witness"
  , reducer           := "strip_tool_results"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "strip_preserves_message_order"
  , group             := "witness"
  , reducer           := "strip_tool_results"
  , legal             := true
  , preMessageCount   := 3
  , postMessageCount  := 3
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "strip_is_strictly_idempotent"
  , group             := "witness"
  , reducer           := "strip_tool_results"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "reduction_blocked_when_response_streaming"
  , group             := "streaming"
  , reducer           := "any_valid"
  , legal             := true
  , preMessageCount   := 1
  , postMessageCount  := 1
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := false
  , reducerIsIdentity := true }
, { name              := "reduction_allowed_when_response_terminal"
  , group             := "streaming"
  , reducer           := "any_valid"
  , legal             := true
  , preMessageCount   := 1
  , postMessageCount  := 1
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := false }
, { name              := "no_orphaned_tool_results_after_strip"
  , group             := "contract"
  , reducer           := "strip_tool_results"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
, { name              := "reapply_preserves_view_coherent"
  , group             := "contract"
  , reducer           := "any_valid"
  , legal             := true
  , preMessageCount   := 2
  , postMessageCount  := 2
  , preservesPairs    := true
  , preservesOrder    := true
  , gateOpen          := true
  , safeToReduce      := true
  , reducerIsIdentity := true }
]

theorem compactionReducerCases_count :
    compactionReducerCases.length = 10 := by decide

end Compaction
