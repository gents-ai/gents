import Proofs.Conformance.Family
import Proofs.Conformance.ContractCases.Types
import Proofs.Session.Compaction.Rolling

namespace Conformance.ContractCases

open Compaction.Rolling

structure RollingChunk where
  messages : Nat
  pairClosed : Bool
  canDispatch : Bool
  deriving Lean.ToJson

structure RollingInput where
  name : String
  targetMessages : Nat
  chunks : List RollingChunk
  checkpointCovered : Nat
  priorPayload : Option Nat
  nextChunk : List Nat
  deriving Lean.ToJson

structure RollingOutput where
  planValid : Bool
  stepInput : List Nat
  deriving Lean.ToJson

private def rollingInput (name : String) (targetMessages : Nat)
    (chunks : List RollingChunk) (checkpointCovered : Nat)
    (priorPayload : Option Nat) (nextChunk : List Nat) : RollingInput :=
  { name, targetMessages, chunks, checkpointCovered, priorPayload, nextChunk }

@[conformance_family]
def rollingCompactionCommit : Conformance.Family RollingInput RollingOutput where
  inputs :=
    [ rollingInput "complete_multi_chunk_is_valid" 5
        [{ messages := 2, pairClosed := true, canDispatch := true },
         { messages := 3, pairClosed := true, canDispatch := true }]
        5 (some 41) [7, 8]
    , rollingInput "pair_open_chunk_is_not_committable" 5
        [{ messages := 2, pairClosed := true, canDispatch := true },
         { messages := 3, pairClosed := false, canDispatch := true }]
        5 none [7]
    , rollingInput "zero_length_chunk_is_not_committable" 5
        [{ messages := 0, pairClosed := true, canDispatch := true },
         { messages := 5, pairClosed := true, canDispatch := true }]
        5 none [7]
    , rollingInput "zero_output_chunk_is_not_committable" 5
        [{ messages := 2, pairClosed := true, canDispatch := false },
         { messages := 3, pairClosed := true, canDispatch := true }]
        5 none [7]
    , rollingInput "partial_prefix_is_not_committable" 6
        [{ messages := 2, pairClosed := true, canDispatch := true },
         { messages := 3, pairClosed := true, canDispatch := true }]
        5 none [7]
    ]
  model := fun i =>
    let chunks : List Chunk := i.chunks.map fun chunk =>
      { messages := chunk.messages
      , pairClosed := chunk.pairClosed
      , canDispatch := chunk.canDispatch }
    let plan : Plan :=
      { targetMessages := i.targetMessages
      , chunks
      , checkpoint := { payload := 91, messagesCovered := i.checkpointCovered }
      }
    let prior := i.priorPayload.map (fun payload => { payload, messagesCovered := 0 })
    { planValid := decide plan.Valid
    , stepInput := Compaction.Rolling.stepInput prior i.nextChunk
    }

theorem rollingCompactionCommit_pinned :
    rollingCompactionCommit.inputs.map
      (fun i => (i.name, (rollingCompactionCommit.model i).planValid,
        (rollingCompactionCommit.model i).stepInput)) =
      [ ("complete_multi_chunk_is_valid", true, [41, 7, 8])
      , ("pair_open_chunk_is_not_committable", false, [7])
      , ("zero_length_chunk_is_not_committable", false, [7])
      , ("zero_output_chunk_is_not_committable", false, [7])
      , ("partial_prefix_is_not_committable", false, [7])
      ] := by
  rfl

end Conformance.ContractCases
