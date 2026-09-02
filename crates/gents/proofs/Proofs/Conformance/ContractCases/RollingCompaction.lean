import Proofs.Conformance.ContractCases.Types
import Proofs.Compaction.Rolling

namespace Conformance.ContractCases

open Compaction.Rolling

structure RollingCompactionCase where
  name : String
  targetMessages : Nat
  chunkMessages : List Nat
  chunkPairClosed : List Bool
  chunkCanDispatch : List Bool
  checkpointCovered : Nat
  planValid : Bool
  priorPayload : Option Nat
  nextChunk : List Nat
  stepInput : List Nat
  deriving Repr

private def rollingCase (name : String) (targetMessages : Nat)
    (chunks : List Chunk) (checkpointCovered : Nat)
    (priorPayload : Option Nat) (nextChunk : List Nat) : RollingCompactionCase :=
  let plan : Plan :=
    { targetMessages
    , chunks
    , checkpoint := { payload := 91, messagesCovered := checkpointCovered }
    }
  let prior := priorPayload.map (fun payload => { payload, messagesCovered := 0 })
  { name
  , targetMessages
  , chunkMessages := chunks.map Chunk.messages
  , chunkPairClosed := chunks.map Chunk.pairClosed
  , chunkCanDispatch := chunks.map Chunk.canDispatch
  , checkpointCovered
  , planValid := decide plan.Valid
  , priorPayload
  , nextChunk
  , stepInput := Compaction.Rolling.stepInput prior nextChunk
  }

def rollingCompactionCases : List RollingCompactionCase :=
  [ rollingCase "complete_multi_chunk_is_valid" 5
      [{ messages := 2, pairClosed := true, canDispatch := true },
       { messages := 3, pairClosed := true, canDispatch := true }]
      5 (some 41) [7, 8]
  , rollingCase "pair_open_chunk_is_not_committable" 5
      [{ messages := 2, pairClosed := true, canDispatch := true },
       { messages := 3, pairClosed := false, canDispatch := true }]
      5 none [7]
  , rollingCase "zero_length_chunk_is_not_committable" 5
      [{ messages := 0, pairClosed := true, canDispatch := true },
       { messages := 5, pairClosed := true, canDispatch := true }]
      5 none [7]
  , rollingCase "zero_output_chunk_is_not_committable" 5
      [{ messages := 2, pairClosed := true, canDispatch := false },
       { messages := 3, pairClosed := true, canDispatch := true }]
      5 none [7]
  , rollingCase "partial_prefix_is_not_committable" 6
      [{ messages := 2, pairClosed := true, canDispatch := true },
       { messages := 3, pairClosed := true, canDispatch := true }]
      5 none [7]
  ]

theorem rollingCompactionCases_pinned :
    rollingCompactionCases.map
      (fun row => (row.name, row.planValid, row.stepInput)) =
      [ ("complete_multi_chunk_is_valid", true, [41, 7, 8])
      , ("pair_open_chunk_is_not_committable", false, [7])
      , ("zero_length_chunk_is_not_committable", false, [7])
      , ("zero_output_chunk_is_not_committable", false, [7])
      , ("partial_prefix_is_not_committable", false, [7])
      ] := by
  rfl

end Conformance.ContractCases
