import Proofs.Runtime.CompletionLoop
import Proofs.Conformance.Family

namespace CompletionLoop

private def done : Call := { text := ["Hello ", "world"] }
private def failure (kind : String) : Call := { failure := some kind }
private def chargedDone : Call := { text := ["done"], usage := some ⟨100, 100, 1000⟩ }
private def cases : List Input :=
  [{ calls := [done] },
   { calls := [done], recoveredPair := true },
   { calls := [failure "transport", done] },
   { calls := [failure "transport", failure "transport", failure "transport", done] },
   { calls := List.replicate 4 (failure "transport") },
   { calls := [failure "transport"], deadlineFits := false },
   { calls := [failure "permanent"] },
   { calls := [failure "reasoning", done] },
   { calls := [failure "reasoning", failure "reasoning"] },
   { calls := [failure "parse", failure "parse", done] },
   { calls := [{ failure "parse" with firstPoll := true },
               { failure "parse" with firstPoll := true }, done] },
   { calls := [{ failure "transport" with text := ["Hel"] }, done] },
   { calls := [{ reasoning := ["still thinking"] }, done] },
   { calls := [{ failure "transport" with tool := true }, done] },
   { calls := [{ tool := true }, failure "parse", failure "parse", done], context := true, maxTurns := 4 },
   { calls := [{ tool := true }, { tool := true }, done], maxTurns := 1 },
   { calls := [{ reasoning := ["thinking"] }, { reasoning := ["again"] }] },
   { calls := [failure "rate_limit", failure "connection", failure "transport", done] },
   { calls := [failure "parse", failure "parse", done], initialDirty := true },
   { calls := [done], context := true },
   { calls := [done], missingCapture := true },
   { calls := [{}], missingCapture := true },
   { calls := [{ tool := true }, failure "parse", failure "parse", { tool := true, poison := false }, done], maxTurns := 4 }] ++
  [{ calls := [{ tool := true, usage := some ⟨1000, 1000, 2000⟩ }, chargedDone], limit := some 5000 },
   { calls := [{ chargedDone with usage := some ⟨2500, 2500, 5000⟩ }], limit := some 5000 },
   { calls := [{ tool := true, usage := some ⟨2500, 2500, 5000⟩ }], limit := some 5000 },
   { calls := [{ chargedDone with usage := some ⟨3000, 2500, 5500⟩ }], limit := some 5000 },
   { calls := [{ failure "transport" with text := ["partial"] }, chargedDone], limit := some 5000 },
   { calls := [{ failure "transport" with tool := true }, chargedDone], limit := some 5000 },
   { calls := [chargedDone], limit := some 0 },
   { calls := [chargedDone], limit := some 5000, nestedUsage := some ⟨500, 500, 1000⟩ },
   { calls := [chargedDone], limit := some 5000, nestedUsage := some ⟨2500, 2500, 5000⟩ },
   { calls := [{ usage := some ⟨1000, 1000, 2000⟩ }, chargedDone], limit := some 5000 },
   { calls := [{ text := ["invalid"], usage := some ⟨100, 100, 200⟩ },
               { text := ["{\"answer\":\"done\"}"], usage := some ⟨100, 100, 200⟩ }], limit := some 5000, structured := true }] ++
  [0, 1, 2, 3].flatMap (fun transport => [false, true].map fun repair =>
    { calls := [failure "transport", failure "parse", failure "reasoning", done], transport, repair })

@[conformance_family]
def stream : Conformance.Family Input Observation := ⟨cases, run⟩

@[conformance_family]
def text : Conformance.Family Input TextObservation :=
  ⟨[{ calls := [{ failure "transport" with text := ["discarded"] }, done] },
    { calls := [failure "transport", done] },
    { calls := [failure "permanent"] }, { calls := [done] }], toText⟩

end CompletionLoop
