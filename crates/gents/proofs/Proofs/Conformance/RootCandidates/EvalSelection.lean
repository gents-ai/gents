import Proofs.Runtime.EvalSelection
import Proofs.Conformance.Family

namespace Runtime.EvalSelection
open Lean Conformance

private def original : Verdict := ⟨"v1", 0, "check-v1", "acceptance", "model_acceptance", none, some 0, 1, none⟩
private def regrade (id : String) : Verdict :=
  { original with verdictId := id, kind := "passed", scoreBp := some 10000, regradeOf := some "v1" }

private def verdictRows : List (List Verdict) :=
  [[], [original], [original, regrade "v2"], [original, regrade "v2", regrade "v3"],
   [regrade "v2", regrade "v3", { original with stageIndex := 1 }],
   [original, { original with verdictId := "v2", check := "another" }],
   [original, regrade "v2", { regrade "v3" with regradeOf := some "v2" }]]

@[conformance_family]
def latestVerdicts : Family VerdictInput (List Verdict) :=
  ⟨(verdictRows.flatMap fun rows => [rows, rows.reverse]).map (fun rows => ⟨rows⟩),
    fun input => select input.rows⟩

private def baselineRows : List (List Trial) :=
  [[], [⟨"a", 0, "scored", 5000⟩], [⟨"a", 0, "unknown", 0⟩],
   [⟨"a", 0, "not_evidence", 0⟩], [⟨"a", 0, "scored", 12000⟩],
   [⟨"a", 0, "scored", 9000⟩, ⟨"a", 0, "scored", 1000⟩],
   [⟨"a", 0, "not_evidence", 0⟩, ⟨"a", 0, "scored", 1000⟩],
   [⟨"b", 1, "scored", 2000⟩, ⟨"a", 0, "unknown", 0⟩]]

private def candidateRows : List (List Trial) :=
  [[], [⟨"a", 0, "scored", 5000⟩], [⟨"a", 0, "unknown", 0⟩],
   [⟨"a", 0, "not_evidence", 0⟩], [⟨"a", 0, "scored", 12000⟩],
   [⟨"a", 0, "scored", 9000⟩, ⟨"a", 0, "scored", 1000⟩],
   [⟨"a", 0, "scored", 1000⟩, ⟨"a", 0, "scored", 9000⟩]]

@[conformance_family]
def pairTrials : Family PairInput Paired :=
  ⟨baselineRows.flatMap (fun baseline => candidateRows.map fun candidate => ⟨baseline, candidate⟩), pair⟩

end Runtime.EvalSelection
