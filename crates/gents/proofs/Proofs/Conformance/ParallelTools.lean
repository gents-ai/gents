import Proofs.ToolPolicy.ParallelTools
import Lean

open Lean ToolPolicy.ParallelTools

private def readA : Call := ⟨1, "read_a", .read [["root", "a"]]⟩
private def readB : Call := ⟨2, "read_b", .read [["root", "b"]]⟩
private def writeA : Call := ⟨3, "write_a", .write [["root", "a"]]⟩
private def writeChild : Call := ⟨4, "write_child", .write [["root", "a", "child"]]⟩
private def unknown : Call := ⟨5, "unknown", .barrier⟩

private def cases : List (String × List Call) :=
  [("independent_reads", [readA, readB]),
   ("read_write_overlap", [readA, writeA, readB]),
   ("disjoint_write", [readA, {writeA with footprint := .write [["root", "b"]]}]),
   ("nested_write", [writeA, writeChild]),
   ("unknown_barrier", [readA, unknown, readB]),
   ("barrier_first", [unknown, readA])]

private def footprintJson : Footprint → Json
  | .read paths => Json.mkObj [("kind", toJson "read"), ("paths", toJson paths)]
  | .write paths => Json.mkObj [("kind", toJson "write"), ("paths", toJson paths)]
  | .barrier => Json.mkObj [("kind", toJson "barrier")]

private def callJson (call : Call) : Json :=
  Json.mkObj [("id", toJson call.id), ("key", toJson call.key), ("footprint", footprintJson call.footprint)]

private def waveJson (entry : String × List Call) : Json :=
  let (wave, remaining) := nextWave entry.2
  Json.mkObj [("name", toJson entry.1), ("calls", toJson (entry.2.map callJson)),
    ("wave", toJson (wave.map (·.id))), ("remaining", toJson (remaining.map (·.id)))]

theorem concrete_waves :
    cases.map (fun c => ((nextWave c.2).1.map (·.id), (nextWave c.2).2.map (·.id))) =
      [([1, 2], []), ([1], [3, 2]), ([1, 3], []), ([3], [4]), ([1], [5, 2]), ([5], [1])] := by
  decide

def main : IO Unit := do
  let result := fun id => if id == 1 then some "first" else if id == 2 then some "second" else none
  let reordered := orderedResults [readA, readB] result
  let incomplete := orderedResults [readA, writeA] result
  let admittedCalls := admitted [(readA, .run), (readB, .skip), (writeA, .terminate), (unknown, .run)]
  let stopped := stopWave [.running, .finished "done", .failed "failure", .skipped "skip"]
  let loopResult := fun id => if id == 1 then some "ALPHA" else if id == 2 then some "BETA" else none
  let loopOrdered := orderedResults [readA, readB] loopResult
  let loopStopped := stopWave [.finished "ALPHA", .running]
  let limited := executableWave 1 [readA, readB]
  let repeated := executableWave 8 [readA, {readB with key := readA.key}, writeA]
  let executionJson : Execution → Json
    | .running => toJson "running"
    | .cancelRequested => toJson "cancel_requested"
    | .finished s => Json.mkObj [("finished", toJson s)]
    | .failed s => Json.mkObj [("failed", toJson s)]
    | .skipped s => Json.mkObj [("skipped", toJson s)]
    | .cancelled => toJson "cancelled"
  IO.println ((Json.mkObj [
    ("waves", toJson (cases.map waveJson)),
    ("hook_footprints", toJson (([false, true].flatMap fun allowed =>
      [readA.footprint, writeA.footprint, Footprint.barrier].map fun footprint =>
        Json.mkObj [("allowed", toJson allowed), ("input", footprintJson footprint),
          ("expected", footprintJson (speculativeFootprint allowed footprint))]))),
    ("result_calls", toJson ([readA, readB].map callJson)),
    ("result_values", Json.mkObj [("1", toJson "first"), ("2", toJson "second")]),
    ("ordered", toJson reordered),
    ("loop_ordered", toJson loopOrdered),
    ("loop_stopped", toJson (loopStopped.map executionJson)),
    ("incomplete_calls", toJson ([readA, writeA].map callJson)),
    ("incomplete", toJson incomplete),
    ("decisions", toJson #["run", "skip", "terminate", "run"]),
    ("decision_calls", toJson ([readA, readB, writeA, unknown].map callJson)),
    ("admitted", toJson (admittedCalls.map (·.id))),
    ("limited", Json.mkObj [("limit", toJson (1 : Nat)), ("calls", toJson ([readA, readB].map callJson)), ("wave", toJson (limited.map (·.id)))]),
    ("repeated", Json.mkObj [("limit", toJson (8 : Nat)), ("calls", toJson ([readA, {readB with key := readA.key}, writeA].map callJson)), ("wave", toJson (repeated.map (·.id)))]),
    ("executions", toJson ([Execution.running, .finished "done", .failed "failure", .skipped "skip"].map executionJson)),
    ("stopped", toJson (stopped.map executionJson))]).compress)
