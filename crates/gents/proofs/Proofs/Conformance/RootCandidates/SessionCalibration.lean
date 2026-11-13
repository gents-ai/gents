import Proofs.Runtime.SessionCalibration
import Proofs.Conformance.Family

namespace Runtime.SessionCalibration

private def base : Row :=
  ⟨true, true, true, 65755, none, none, some 109881, some 7000, some 109000, some 878⟩

private def rows : List Row :=
  [base, { base with accounting := false }, { base with matching := false },
   { base with complete := false }, { base with input := some 3 },
   { base with input := some 50000, cached := some 0, created := some 0 },
   { base with input := none }, { base with input := some (-1) },
   { base with output := some (-1) }, { base with cached := some (-1) },
   { base with dispatched := some 17000 },
   { base with input := none, dispatched := some 17000 },
   { base with estimated := 999999, raw := some 65755 },
   { base with estimated := 0, input := some 1, cached := none, created := none }]

@[conformance_family]
def sessionCalibrationSeed : Conformance.Family Input Observation :=
  ⟨[⟨[]⟩] ++ rows.map (fun row => ⟨[row]⟩) ++
    rows.flatMap (fun first => rows.map fun second => ⟨[first, second]⟩) ++
    [⟨[{ base with estimated := 10000, input := some 13000, cached := none, created := none },
       { base with estimated := 10000, input := some 26000, cached := none, created := none }]⟩,
     ⟨List.replicate 8 { base with estimated := 10000, input := some 13000, cached := none, created := none } ++
       [{ base with estimated := 10000, input := some 26000, cached := none, created := none }]⟩], seed⟩

end Runtime.SessionCalibration
