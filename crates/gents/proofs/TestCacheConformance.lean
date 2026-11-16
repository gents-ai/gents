import Lean.Data.Json
import Proofs.Ci.TestCache

/-! Reads test-cache scenarios as JSON on stdin and prints `Proofs.Ci.TestCache`'s
decisions and stores (`lake env lean --run TestCacheConformance.lean`);
`scripts/ci/test_test_cache.py` compares the gate's implementation to it. -/

open Lean (Json FromJson ToJson fromJson? toJson)
open TestCache

namespace TestCache

instance : ToJson Answer where
  toJson
    | .file digest => Json.mkObj [("file", digest)]
    | .dir entries => Json.mkObj [("dir", toJson entries)]
    | .absent => "absent"

instance : FromJson Answer where
  fromJson? json :=
    if json == "absent" then pure .absent
    else match json.getObjValAs? String "file" with
      | .ok digest => pure (.file digest)
      | .error _ => do
        let entries ← json.getObjValAs? (List String) "dir"
        pure (.dir entries)

deriving instance ToJson, FromJson for TestId
deriving instance ToJson, FromJson for Record

instance : ToJson Store where
  toJson store := Json.mkObj [
    ("records", toJson store.records),
    ("contradicted", toJson store.contradicted)]

instance : FromJson Store where
  fromJson? json := do
    pure ⟨← json.getObjValAs? (List Record) "records",
      ← json.getObjValAs? (List (TestId × StaticKey)) "contradicted"⟩

instance : FromJson Policy where
  fromJson? json := do
    pure ⟨← json.getObjValAs? (List TestId) "excluded", ← json.getObjValAs? Bool "full"⟩

instance : ToJson Reason where
  toJson
    | .fullRun => "full"
    | .excluded => "excluded"
    | .contradicted => "contradicted"
    | .miss => "miss"

instance : ToJson Decision where
  toJson
    | .skip record => Json.mkObj [("skip", record.run)]
    | .run reason => Json.mkObj [("run", toJson reason)]

/-- A finite file system: listed paths answer as listed, every other path is absent. -/
def fileSystem (entries : List (Path × Answer)) : FileSystem :=
  fun path => (entries.lookup path).getD .absent

def step (policy : Policy) (store : Store) (json : Json) : Except String (Store × Json) := do
  match json.getObjVal? "decide" with
  | .ok query =>
    let test ← query.getObjValAs? TestId "test"
    let key ← query.getObjValAs? StaticKey "key"
    let fs ← query.getObjValAs? (List (Path × Answer)) "fs"
    pure (store, toJson (decide policy store test key (fileSystem fs)))
  | .error _ =>
    let e ← json.getObjVal? "observe"
    let execution : Execution := {
      test := ← e.getObjValAs? TestId "test"
      key := ← e.getObjValAs? StaticKey "key"
      fs := fileSystem (← e.getObjValAs? (List (Path × Answer)) "fs")
      passed := ← e.getObjValAs? Bool "passed"
      trace := ← e.getObjValAs? Trace "trace"
      trusted := ← e.getObjValAs? Bool "trusted"
      traceComplete := ← e.getObjValAs? Bool "traceComplete"
      checkoutClean := ← e.getObjValAs? Bool "checkoutClean"
      run := ← e.getObjValAs? String "run" }
    let next := observe policy store execution
    pure (next, toJson next)

/-- Every scenario's step results, in order. -/
def scenario (json : Json) : Except String Json := do
  let policy ← json.getObjValAs? Policy "policy"
  let mut store ← json.getObjValAs? Store "store"
  let mut results : Array Json := #[]
  for item in ← json.getObjValAs? (List Json) "steps" do
    let (next, result) ← step policy store item
    store := next
    results := results.push result
  pure (Json.arr results)

end TestCache

partial def readAll (stream : IO.FS.Stream) (acc : String := "") : IO String := do
  let line ← stream.getLine
  if line.isEmpty then pure acc else readAll stream (acc ++ line)

def main : IO UInt32 := do
  let input ← readAll (← IO.getStdin)
  match Json.parse input >>= fun json => do
      let scenarios ← json.getArr?
      pure (Json.arr (← scenarios.mapM TestCache.scenario)) with
  | .ok output =>
    IO.println output.compress
    pure 0
  | .error message =>
    IO.eprintln s!"test-cache-model: {message}"
    pure 2
