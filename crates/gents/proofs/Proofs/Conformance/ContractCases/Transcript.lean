import Proofs.Conformance.ContractCases.Types
import Proofs.Transcript.Executable

namespace Conformance.ContractCases

def transcriptCaseFromModel (witness : Transcript.TranscriptCase) : TranscriptCase :=
  { name := witness.name
  , group := witness.group
  , action := witness.action
  , legal := witness.legal
  , preMessageCount := witness.preMessageCount
  , postMessageCount := witness.postMessageCount
  , preToolCallCount := witness.preToolCallCount
  , postToolCallCount := witness.postToolCallCount
  , preInFlightCount := witness.preInFlightCount
  , postInFlightCount := witness.postInFlightCount
  , assistantSequence := witness.assistantSequence
  , resultSequence := witness.resultSequence
  , logicalResultId := witness.logicalResultId
  , payloadHash := witness.payloadHash
  , expectedPairClosed := witness.expectedPairClosed
  , expectedOrdered := witness.expectedOrdered
  , expectedDuplicateReusedSequence := witness.expectedDuplicateReusedSequence
  , expectedStrongDrain := witness.expectedStrongDrain
  }

def transcriptConformanceCases : List TranscriptCase :=
  Transcript.transcriptConformanceCases.map transcriptCaseFromModel

end Conformance.ContractCases
