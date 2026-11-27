import Proofs.Conformance.CoverageLedger
import Proofs.Conformance.Contracts.Json.Vocabularies
import Proofs.Conformance.Contracts.Json.Shards.S01
import Proofs.Conformance.Contracts.Json.Shards.S02
import Proofs.Conformance.Contracts.Json.Shards.S03
import Proofs.Conformance.Contracts.Json.Shards.S04
import Proofs.Conformance.Contracts.Json.Shards.S05
import Proofs.Conformance.Contracts.Json.Shards.S06
import Proofs.Conformance.Contracts.Json.Shards.S07
import Proofs.Conformance.Contracts.Json.Shards.S08
import Proofs.Conformance.Contracts.Json.Shards.S09
import Proofs.Conformance.Contracts.Json.Shards.S10
import Proofs.Conformance.Contracts.Json.Shards.S11
import Proofs.Conformance.Contracts.Json.Shards.S12
import Proofs.Conformance.Contracts.Json.Shards.S13
import Proofs.Conformance.Contracts.Json.Shards.S14
import Proofs.Conformance.Contracts.Json.Shards.S15
import Proofs.Conformance.Contracts.Json.Shards.S16
import Proofs.Conformance.Contracts.Json.Shards.S17
import Proofs.Conformance.Contracts.Json.Shards.S18
import Proofs.Conformance.Contracts.Json.Shards.S19
import Proofs.Conformance.Contracts.Json.Shards.S20
import Proofs.Conformance.Contracts.Json.Shards.S21
import Proofs.Conformance.Contracts.Json.Shards.S22
import Proofs.Conformance.Contracts.Json.Shards.S23
import Proofs.Conformance.Contracts.Json.Shards.S24

namespace Conformance.Contracts

open Conformance.ContractCases

/-- The snapshot's concatenation chains are bounded to keep model execution
within Lean's interpreter recursion limit. -/
def snapshotPromptTailJson : String :=
  ""

set_option maxHeartbeats 2000000 in
private def remainingEntries : List (String × Lean.Json) :=
  conformance_remaining_entries%

private def allEntries : List (String × Lean.Json) :=
  Vocabularies.entries
    ++ Shards.S01.entries
    ++ Shards.S02.entries
    ++ Shards.S03.entries
    ++ Shards.S04.entries
    ++ Shards.S05.entries
    ++ Shards.S06.entries
    ++ Shards.S07.entries
    ++ Shards.S08.entries
    ++ Shards.S09.entries
    ++ Shards.S10.entries
    ++ Shards.S11.entries
    ++ Shards.S12.entries
    ++ Shards.S13.entries
    ++ Shards.S14.entries
    ++ Shards.S15.entries
    ++ Shards.S16.entries
    ++ Shards.S17.entries
    ++ Shards.S18.entries
    ++ Shards.S19.entries
    ++ Shards.S20.entries
    ++ Shards.S21.entries
    ++ Shards.S22.entries
    ++ Shards.S23.entries
    ++ Shards.S24.entries
    ++ remainingEntries

def snapshotFamiliesJson : String :=
  (Lean.Json.mkObj allEntries).compress

def snapshotVocabulariesJson : String :=
  (conformance_vocabularies%).compress

def snapshotCaptureTailJson : String :=
  ""
    ++ "\"families\":" ++ snapshotFamiliesJson ++ ","
    ++ "\"family_vocabularies\":" ++ snapshotVocabulariesJson ++ ","

def snapshotCoverageTailJson : String :=
  ""
    ++ "\"coverage_ledger\":"
      ++ coverageLedgerJson
    ++ ",\"feature_surface_requirements\":"
      ++ featureSurfaceRequirementsJson
    ++ "}"

def snapshotJsonTail : String :=
  snapshotPromptTailJson ++ snapshotCaptureTailJson ++ snapshotCoverageTailJson

def snapshotJson : String :=
  "{"
    ++ "\"generated_by\":\"lake env lean --run Proofs/Conformance/Contracts.lean\","
    ++ snapshotJsonTail

end Conformance.Contracts
