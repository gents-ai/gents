import Proofs.Conformance.ContractCases.Types
import Proofs.Compaction.Rolling

namespace Conformance.ContractCases

open Compaction.Rolling

structure RollingCompactionCase where
  name : String
  beforeCursor : Nat
  targetMessages : Nat
  chunkMessages : List Nat
  chunkPairClosed : List Bool
  chunkCanDispatch : List Bool
  checkpointCovered : Nat
  completed : Bool
  planValid : Bool
  cursorAfter : Nat
  priorPayload : Option Nat
  nextChunk : List Nat
  stepInput : List Nat
  deriving Repr

private def resultFor (completed : Bool) (plan : Plan) : Result :=
  if completed then
    if valid : plan.Valid then .complete plan valid else .failed 0
  else
    .failed 0

private def rollingCase (name : String) (beforeCursor targetMessages : Nat)
    (chunks : List Chunk) (checkpointCovered : Nat) (completed : Bool)
    (priorPayload : Option Nat) (nextChunk : List Nat) : RollingCompactionCase :=
  let plan : Plan :=
    { targetMessages
    , chunks
    , checkpoint := { payload := 91, messagesCovered := checkpointCovered }
    }
  let before : DurableState := { checkpoint := none, cursor := beforeCursor }
  let after := commit before (resultFor completed plan)
  let prior := priorPayload.map (fun payload => { payload, messagesCovered := beforeCursor })
  { name
  , beforeCursor
  , targetMessages
  , chunkMessages := chunks.map Chunk.messages
  , chunkPairClosed := chunks.map Chunk.pairClosed
  , chunkCanDispatch := chunks.map Chunk.canDispatch
  , checkpointCovered
  , completed
  , planValid := decide plan.Valid
  , cursorAfter := after.cursor
  , priorPayload
  , nextChunk
  , stepInput := Compaction.Rolling.stepInput prior nextChunk
  }

def rollingCompactionCases : List RollingCompactionCase :=
  [ rollingCase "complete_multi_chunk_commits_exact_target" 10 5
      [{ messages := 2, pairClosed := true, canDispatch := true },
       { messages := 3, pairClosed := true, canDispatch := true }]
      5 true (some 41) [7, 8]
  , rollingCase "chunk_n_failure_preserves_cursor" 10 5
      [{ messages := 2, pairClosed := true, canDispatch := true },
       { messages := 3, pairClosed := true, canDispatch := true }]
      5 false (some 41) [7, 8]
  , rollingCase "pair_open_chunk_is_not_committable" 10 5
      [{ messages := 2, pairClosed := true, canDispatch := true },
       { messages := 3, pairClosed := false, canDispatch := true }]
      5 true none [7]
  , rollingCase "zero_length_chunk_is_not_committable" 10 5
      [{ messages := 0, pairClosed := true, canDispatch := true },
       { messages := 5, pairClosed := true, canDispatch := true }]
      5 true none [7]
  , rollingCase "zero_output_chunk_is_not_committable" 10 5
      [{ messages := 2, pairClosed := true, canDispatch := false },
       { messages := 3, pairClosed := true, canDispatch := true }]
      5 true none [7]
  , rollingCase "partial_prefix_is_not_committable" 10 6
      [{ messages := 2, pairClosed := true, canDispatch := true },
       { messages := 3, pairClosed := true, canDispatch := true }]
      5 true none [7]
  ]

theorem rollingCompactionCases_pinned :
    rollingCompactionCases.map
      (fun row => (row.name, row.planValid, row.cursorAfter, row.stepInput)) =
      [ ("complete_multi_chunk_commits_exact_target", true, 5, [41, 7, 8])
      , ("chunk_n_failure_preserves_cursor", true, 10, [41, 7, 8])
      , ("pair_open_chunk_is_not_committable", false, 10, [7])
      , ("zero_length_chunk_is_not_committable", false, 10, [7])
      , ("zero_output_chunk_is_not_committable", false, 10, [7])
      , ("partial_prefix_is_not_committable", false, 10, [7])
      ] := by
  rfl

end Conformance.ContractCases
