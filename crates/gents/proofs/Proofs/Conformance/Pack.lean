import Proofs.Pack.PluginOwnership
import Proofs.Pack.PluginWiring
import Proofs.Pack.Store
import Proofs.Conformance.Family

open Lean

namespace Pack.PluginOwnership

deriving instance ToJson for Record, Admission, Removal, Verdict

attribute [conformance_function] verdict

end Pack.PluginOwnership

namespace Pack.PluginWiring

deriving instance ToJson for Dynamic, Policy, Job, Entry, Surface, Plugin, Source, Warning, Outcome

end Pack.PluginWiring

namespace Pack.Store

deriving instance ToJson for Semver, Version, Pack, Action, Result, Step, Listing

end Pack.Store

namespace Conformance.Pack

open _root_.Pack

/-! ## Plugin wiring -/

section Wiring

open PluginWiring

inductive WiringAction | wire | unwire
  deriving DecidableEq, Repr, ToJson

/-- One wiring or unwiring. The consents are the node owner's, given with a
wiring and applied to what it wrote. -/
structure WiringStep where
  action : WiringAction
  plugin : Plugin
  reviewed : Option Job
  skills : List String
  sources : List Source
  consentDynamic : Bool
  consentToolCalls : Bool
  deriving ToJson

structure WiringScript where
  name : String
  start : Surface
  steps : List WiringStep
  deriving ToJson

/-- Each step's outcome; a refused step leaves the Agent as it was. -/
def runWiring (surface : Surface) : List WiringStep → List (Option Outcome)
  | [] => []
  | step :: rest =>
    let outcome := match step.action with
      | .wire =>
        (wire surface step.plugin step.reviewed step.skills step.sources).map fun wired =>
          let wired :=
            if step.consentToolCalls then consentToolCalls step.plugin.coordinate wired else wired
          if step.consentDynamic then consentDynamic step.plugin.coordinate wired else wired
      | .unwire => unwire surface step.plugin step.skills step.sources
    outcome :: runWiring ((outcome.map (·.surface)).getD surface) rest

private def runner (ownerPack : Option String := none) (runsJobs : Bool := true) : Plugin :=
  { coordinate := "acme/runner", digest := "sha256:aa", runsJobs, ownerPack }

private def other : Plugin :=
  { coordinate := "acme/other", digest := "sha256:bb", runsJobs := true, ownerPack := none }

private def fresh (enabled : Option Bool := none) : Surface :=
  { plugins := [], targets := [], enabled, skills := ["existing"] }

private def block (targets : List String) (dynamic : PluginWiring.Dynamic := .check)
    (dynamicTargets trustedPacks : List String := []) : Job :=
  { defaultJob runner with
    agentTargets := targets, policy := { dynamic, dynamicTargets, trustedPacks } }

private def content : Source :=
  { pack := "acme/content", targets := ["wf-a", "wf-b"], operatorInstalled := true }

private def extra : Source :=
  { pack := "acme/extra", targets := ["wf-c"], operatorInstalled := true }

private def single (operatorInstalled : Bool := true) : Source :=
  { pack := "acme/content", targets := ["wf-a"], operatorInstalled }

private def wireStep (plugin : Plugin) (reviewed : Option Job := none) (skills : List String := [])
    (sources : List Source := []) (consentDynamic consentToolCalls : Bool := false) : WiringStep :=
  { action := .wire, plugin, reviewed, skills, sources, consentDynamic, consentToolCalls }

private def unwireStep (plugin : Plugin) (skills : List String := [])
    (sources : List Source := []) : WiringStep :=
  { action := .unwire, plugin, reviewed := none, skills, sources
    consentDynamic := false, consentToolCalls := false }

private def wiredWith (job : Option Job) (targets : List String) (enabled : Option Bool := none)
    (toolCalls : Bool := false) : Surface :=
  { plugins := [{ plugin := "acme/runner", digest := "sha256:old", toolCalls, job }]
    targets, enabled, skills := ["existing"] }

private def narrow : Job :=
  { agentTargets := ["a", "c"], maxLive := 2, maxSpawns := 10, maxMessagesPerSlot := 9
    resultBytes := 2048, maxWallSecs := 100
    policy := { dynamic := .off, dynamicTargets := ["a", "c"], trustedPacks := [] } }

private def skill : List String := ["create-workflow"]

@[conformance_family]
def pluginWiring : Family WiringScript (List (Option Outcome)) where
  inputs :=
    [ ⟨"a job plugin wired with nothing reviewed and no pack gets no block", fresh,
        [wireStep runner]⟩
    , ⟨"packs wired in without a reviewed block get the default block", fresh,
        [wireStep (runner (some "acme/engine")) (sources := [single])]⟩
    , ⟨"the owner's consent raises check to run", fresh,
        [wireStep (runner (some "acme/engine")) (sources := [single]) (consentDynamic := true)]⟩
    , ⟨"consent leaves a block that is off off", fresh,
        [wireStep runner (reviewed := some (block ["wf-a"] .off)) (consentDynamic := true)]⟩
    , ⟨"the owner's consent lets the plugin call the Agent's tools", fresh,
        [wireStep (runner (runsJobs := false)) (consentToolCalls := true)]⟩
    , ⟨"a reviewed block is written as reviewed", fresh,
        [wireStep (runner (some "gents/rhai_workflows"))
          (reviewed := some (block ["wf-read-only"] .check ["wf-read-only"] ["gents/rhai_workflows"]))]⟩
    , ⟨"a later wiring never widens the block", wiredWith (some narrow) ["a", "c"],
        [wireStep runner (reviewed := some
          { block ["b", "c"] .check ["b", "c"] with maxLive := 8 })]⟩
    , ⟨"a later wiring cannot raise the dynamic mode",
        wiredWith (some (block ["a"] .check)) ["a"],
        [wireStep runner (reviewed := some (block ["a"] .run))]⟩
    , ⟨"wiring leaves an unset Agent switch unset", fresh none,
        [wireStep runner (sources := [single])]⟩
    , ⟨"wiring leaves a disabled Agent switch disabled", fresh (some false),
        [wireStep runner (sources := [single])]⟩
    , ⟨"new targets on an Agent that starts Agents are reported", fresh (some true),
        [wireStep runner (sources := [single])]⟩
    , ⟨"targets the Agent already had are not reported as new",
        { fresh (some true) with targets := ["wf-a"] },
        [wireStep runner (sources := [single])]⟩
    , ⟨"a pack the operator did not install is reported", fresh,
        [wireStep runner (sources := [single false])]⟩
    , ⟨"skills are added once", fresh,
        [wireStep runner (skills := skill), wireStep runner (skills := skill)]⟩
    , ⟨"a plugin that runs no jobs gets no block", fresh,
        [wireStep (runner (runsJobs := false)) (reviewed := some (block ["wf-a"]))
          (sources := [content])]⟩
    , ⟨"rewiring pins the new artifact and keeps the entry",
        wiredWith none [] (toolCalls := true),
        [wireStep (runner (runsJobs := false))]⟩
    , ⟨"a block with no targets is refused and the next wiring starts clean", fresh,
        [wireStep runner (reviewed := some (block [])), wireStep runner (reviewed := some (block ["wf-a"]))]⟩
    , ⟨"a block naming a target twice is refused", fresh,
        [wireStep runner (reviewed := some (block ["wf-a", "wf-a"]))]⟩
    , ⟨"dynamic targets outside the block's targets are refused", fresh,
        [wireStep runner (reviewed := some (block ["wf-a"] .check ["wf-b"]))]⟩
    , ⟨"dynamic targets outside the block's targets pass when dynamic is off", fresh,
        [wireStep runner (reviewed := some (block ["wf-a"] .off ["wf-b"]))]⟩
    , ⟨"a bound below its range is refused", fresh,
        [wireStep runner (reviewed := some { block ["wf-a"] with maxLive := 0 })]⟩
    , ⟨"a bound above its range is refused", fresh,
        [wireStep runner (reviewed := some { block ["wf-a"] with maxLive := 65 })]⟩
    , ⟨"the ends of every range pass", fresh,
        [ wireStep runner (reviewed := some
            { block ["wf-a"] with
              maxLive := 1, maxSpawns := 1, maxMessagesPerSlot := 0, resultBytes := 1024
              maxWallSecs := 60 })
        , wireStep other (reviewed := some
            { block ["wf-a"] with
              maxLive := 64, maxSpawns := 1024, maxMessagesPerSlot := 16, resultBytes := 1048576
              maxWallSecs := 86400 }) ]⟩
    , ⟨"unwiring a reviewed plugin restores the Agent", fresh,
        [ wireStep (runner (some "acme/engine")) (reviewed := some (block ["wf-a"])) (skills := skill)
        , unwireStep (runner (some "acme/engine")) (skills := skill) ]⟩
    , ⟨"unwiring a plugin wired with packs restores the Agent", fresh,
        [ wireStep (runner (some "acme/engine")) (reviewed := some (block ["wf-a"])) (skills := skill)
            (sources := [content])
        , unwireStep (runner (some "acme/engine")) (skills := skill) ]⟩
    , ⟨"shared targets and skills survive one removal and leave with the last", fresh,
        [ wireStep runner (reviewed := some (block ["shared", "runner-own"])) (skills := skill)
        , wireStep other (reviewed := some (block ["shared", "other-own"])) (skills := skill)
        , unwireStep runner (skills := skill)
        , unwireStep other (skills := skill) ]⟩
    , ⟨"a target dynamic scripts of another plugin reach survives",
        { plugins :=
            [ { plugin := "acme/runner", digest := "sha256:aa", toolCalls := false
                job := some (block ["shared"]) }
            , { plugin := "acme/other", digest := "sha256:bb", toolCalls := false
                job := some (block ["other-own"] .off ["shared"]) } ]
          targets := ["shared", "other-own"], enabled := none, skills := [] },
        [unwireStep runner]⟩
    , ⟨"unwiring one pack keeps the plugin and the other pack", fresh,
        [ wireStep (runner (some "acme/engine")) (sources := [content, extra])
        , unwireStep (runner (some "acme/engine")) (sources := [content]) ]⟩
    , ⟨"unwiring the last pack drops the block", fresh,
        [ wireStep (runner (some "acme/engine")) (sources := [content])
        , unwireStep (runner (some "acme/engine")) (sources := [content]) ]⟩
    , ⟨"the plugin's own pack stays trusted when its targets are unwired", fresh,
        [ wireStep (runner (some "acme/content")) (sources := [content, extra])
        , unwireStep (runner (some "acme/content")) (sources := [content]) ]⟩
    , ⟨"unwiring a pack keeps the dynamic mode and the dynamic targets left",
        wiredWith (some (block ["a", "b"] .run ["a"])) ["a", "b"] (some true),
        [unwireStep runner (sources := [{ pack := "acme/content", targets := ["b"], operatorInstalled := true }])]⟩
    , ⟨"unwiring a pack narrows the dynamic targets with the targets",
        wiredWith (some (block ["a", "b"] .run ["a"])) ["a", "b"] (some true),
        [unwireStep runner (sources := [{ pack := "acme/content", targets := ["a"], operatorInstalled := true }])]⟩
    , ⟨"unwiring a pack from a plugin with no block changes nothing", wiredWith none ["a"],
        [unwireStep runner (sources := [content])]⟩
    , ⟨"a block left invalid by unwiring a pack is refused",
        wiredWith (some { block ["a", "b"] with maxLive := 0 }) ["a", "b"],
        [unwireStep runner (sources := [{ pack := "acme/content", targets := ["b"], operatorInstalled := true }])]⟩
    , ⟨"unwiring a plugin that is not wired changes nothing", fresh,
        [unwireStep runner (skills := ["existing"])]⟩
    , ⟨"a target the Agent held before the wiring leaves with the block",
        { fresh with targets := ["reviewer"] },
        [wireStep runner (reviewed := some (block ["reviewer"])), unwireStep runner]⟩ ]
  model := fun script => runWiring script.start script.steps

end Wiring

/-! ## The pack store -/

section Store

open Store

structure StoreScript where
  name : String
  steps : List Step
  /-- Packs observed besides those the steps name. -/
  watch : List Store.Pack
  deriving ToJson

structure PackView where
  pack : Store.Pack
  contains : Bool
  found : Bool
  indexed : Bool
  unpacked : Bool
  deriving ToJson

structure Latest where
  name : String
  version : Option String
  deriving ToJson

structure StoreObservation where
  results : List Result
  /-- Files in the archive directory: nothing is left behind by a refusal. -/
  archives : Nat
  packs : List PackView
  latest : List Latest
  names : List Listing
  deriving ToJson

def observeStore (script : StoreScript) : StoreObservation :=
  let ran := run State.empty script.steps
  let state := ran.1
  let packs := (script.steps.map (·.pack) ++ script.watch).eraseDups
  { results := ran.2
    archives := state.archives.length
    packs := packs.map fun pack =>
      { pack, contains := state.contains pack, found := state.found pack
        indexed := pack ∈ state.index, unpacked := pack ∈ state.unpacked }
    latest := (packs.map (·.name)).eraseDups.map fun name => { name, version := state.latest name }
    names := state.names }

private def release (major minor patch : Nat) : Version :=
  { text := s!"{major}.{minor}.{patch}", semver := some { major, minor, patch, pre := none } }

private def pre (major minor patch : Nat) (tag : String) : Version :=
  { text := s!"{major}.{minor}.{patch}-{tag}", semver := some { major, minor, patch, pre := some tag } }

private def plain (text : String) : Version := { text, semver := none }

private def pack (name : String) (version : Version) : Store.Pack := { name, version }

private def a1 := pack "store_a" (release 1 0 0)
private def a2 := pack "store_a" (release 2 0 0)

private def did (action : Action) (pack : Store.Pack) : Step := { action, pack }

@[conformance_family]
def store : Family StoreScript StoreObservation where
  inputs :=
    [ ⟨"a received pack is stored and indexed", [did .receive a1],
        [pack "store_a" (release 9 9 9), pack "store_b" (release 1 0 0)]⟩
    , ⟨"receiving a pack twice keeps one archive", [did .receive a1, did .receive a1], []⟩
    , ⟨"a pack with another digest than asked for is not stored",
        [did .receiveExpectingAnother a1], []⟩
    , ⟨"a refused pack does not disturb a stored one",
        [did .receive a1, did .receiveExpectingAnother a2], []⟩
    , ⟨"release removes the archive and its unpacked copy, once",
        [did .receive a1, did .openPack a1, did .release a1, did .release a1], []⟩
    , ⟨"a pack that was never received cannot be opened or released",
        [did .openPack a1, did .release a1], []⟩
    , ⟨"release removes its own index entry and keeps the other version's",
        [did .receive a1, did .receive a2, did .release a1], []⟩
    , ⟨"releasing the highest version makes the next one the latest",
        [did .receive a1, did .receive a2, did .release a2], []⟩
    , ⟨"the highest version is by number, not by text",
        [did .receive (pack "store_a" (release 1 2 0)), did .receive (pack "store_a" (release 1 10 0))], []⟩
    , ⟨"the order of receipt does not decide the latest",
        [did .receive (pack "store_a" (release 1 10 0)), did .receive (pack "store_a" (release 1 2 0))], []⟩
    , ⟨"minor and patch decide between equal majors",
        [ did .receive (pack "store_a" (release 1 2 3)), did .receive (pack "store_a" (release 1 2 4))
        , did .receive (pack "store_a" (release 1 1 9)) ], []⟩
    , ⟨"a version that is not semantic sorts below every semantic one",
        [did .receive (pack "store_a" (release 1 0 0)), did .receive (pack "store_a" (plain "latest"))], []⟩
    , ⟨"versions that are not semantic sort as text",
        [did .receive (pack "store_a" (plain "nightly")), did .receive (pack "store_a" (plain "latest"))], []⟩
    , ⟨"a pre-release sorts below its release",
        [did .receive (pack "store_a" (pre 1 0 0 "rc1")), did .receive (pack "store_a" (release 1 0 0))], []⟩
    , ⟨"versions differing only in case stay distinct",
        [did .receive (pack "store_a" (pre 1 0 0 "RC1")), did .receive (pack "store_a" (pre 1 0 0 "rc1"))], []⟩
    , ⟨"a version with a path separator is stored but not indexed",
        [did .receive (pack "store_a" (plain "../escape"))], []⟩
    , ⟨"a bare dot version is stored but not indexed",
        [did .receive (pack "store_a" (plain ".")), did .receive (pack "store_b" (plain ".."))], []⟩
    , ⟨"a pack with a version that is not indexable is released by digest",
        [did .receive (pack "store_a" (plain "../escape")), did .release (pack "store_a" (plain "../escape"))], []⟩
    , ⟨"a pack whose archive was lost is no longer found",
        [did .receive a1, did .receive a2, did .loseArchive a2], []⟩
    , ⟨"releasing a pack whose archive was lost still cleans its index entry",
        [did .receive a1, did .loseArchive a1, did .release a1], []⟩
    , ⟨"a damaged pack cannot be opened and is still released with its index entry",
        [did .receive a1, did .damage a1, did .openPack a1, did .release a1], []⟩
    , ⟨"packs are listed by name with their versions highest first",
        [ did .receive (pack "store_b" (release 1 0 0)), did .receive a1, did .receive a2
        , did .receive (pack "store_a" (plain "latest")) ], []⟩ ]
  model := observeStore

end Store

/-! ## Plugin artifact bytes -/

structure BytesInput where
  name : String
  /-- The digest each installed-plugin record names. -/
  referenced : List String
  stored : List String
  candidates : List String
  deriving ToJson

structure BytesResult where
  released : List String
  remaining : List String
  deriving ToJson

@[conformance_family]
def pluginBytesRelease : Family BytesInput BytesResult where
  inputs :=
    [ ⟨"bytes two records share stay and orphaned bytes go", ["a", "a"], ["a", "b"], ["a", "b"]⟩
    , ⟨"bytes that are not candidates stay", [], ["a", "b"], ["b"]⟩
    , ⟨"a candidate that is not stored is not reported", [], ["a"], ["b"]⟩
    , ⟨"released digests are reported in order", [], ["c", "a", "b"], ["c", "b", "a"]⟩
    , ⟨"nothing is released while every candidate is referenced", ["a", "b"], ["a", "b"], ["a", "b"]⟩ ]
  model := fun input =>
    { released := PluginOwnership.released input.referenced input.stored input.candidates
      remaining :=
        (PluginOwnership.remaining input.referenced input.stored input.candidates).mergeSort
          fun a b => a ≤ b }

end Conformance.Pack
