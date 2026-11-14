import Proofs.Session.CommandPolicy.ArtifactAuthority
import Proofs.Session.Workspace.Properties
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.Family

namespace Conformance.Contracts
open CommandPolicy
open Conformance.ContractCases

/-- An artifact binding as the resolver reads it: authority and state carry
their DefraDB names. -/
structure ArtifactBindingWire where
  authority : String
  state : String
  sealMatches : Bool
  ownerMatches : Bool
  incarnationMatches : Bool
  privateRootEligible : Bool
  sandboxEnforced : Bool
  deriving Lean.ToJson

def ArtifactBindingWire.ofBinding (c : ArtifactBinding) : ArtifactBindingWire :=
  { authority := c.authority.toDefraDB, state := c.state.toDefraDB, sealMatches := c.sealMatches
    ownerMatches := c.ownerMatches, incarnationMatches := c.incarnationMatches
    privateRootEligible := c.privateRootEligible, sandboxEnforced := c.sandboxEnforced }

def ArtifactBindingWire.binding (w : ArtifactBindingWire) : Option ArtifactBinding := do
  let authority ← [BindingAuthority.readOnly, .readWrite, .integrate].find?
    (·.toDefraDB == w.authority)
  let state ← [WorkspaceState.provisioning, .ready, .provisionFailed, .sealed, .cleaning,
    .cleaned].find? (·.toDefraDB == w.state)
  pure { authority, state, sealMatches := w.sealMatches, ownerMatches := w.ownerMatches
         incarnationMatches := w.incarnationMatches
         privateRootEligible := w.privateRootEligible, sandboxEnforced := w.sandboxEnforced }

def executionModeOfDefraDB (mode : String) : ExecutionMode :=
  ([ExecutionMode.readOnly, .workspaceWrite, .artifactWrite, .unrestricted].find?
    (·.toDefraDB == mode)).getD .readOnly

structure ArtifactAdmissionInput where
  name : String
  mode : String
  binding : Option ArtifactBindingWire
  deriving Lean.ToJson

structure ArtifactAdmissionOutput where
  admitted : Bool
  boundMode : Option String
  deriving Lean.ToJson

@[conformance_family]
def artifactAdmission : Family ArtifactAdmissionInput ArtifactAdmissionOutput where
  inputs := ArtifactCases.admissionCases.map fun (name, mode, binding, _) =>
    { name, mode := mode.toDefraDB, binding := binding.map .ofBinding }
  model := fun i =>
    let mode := executionModeOfDefraDB i.mode
    let binding := i.binding.bind (·.binding)
    { admitted := admitArtifact mode binding
    , boundMode := (IsolatedWorkspace.effectiveBoundCommand mode binding).map
        ExecutionMode.toDefraDB }

structure ArtifactSpawnInput where
  name : String
  kind : String
  requestedMode : String
  binding : Option ArtifactBindingWire
  deriving Lean.ToJson

def spawnKindName : SpawnKind → String
  | .foreground => "foreground"
  | .background => "background"
  | .persistentLsp => "persistent_lsp"

def ArtifactSpawnInput.ofCase (c : ArtifactCases.SpawnCase) : ArtifactSpawnInput :=
  { name := c.name, kind := spawnKindName c.kind, requestedMode := ExecutionMode.artifactWrite.toDefraDB
    binding := c.binding.map .ofBinding }

/-- The mode a spawn runs with; `none` is a refused spawn. -/
structure ArtifactSpawnOutput where
  mode : Option String
  deriving Lean.ToJson

def artifactSpawnMode (i : ArtifactSpawnInput) : ArtifactSpawnOutput :=
  let kind := ([SpawnKind.foreground, .background, .persistentLsp].find?
    (spawnKindName · == i.kind)).getD .persistentLsp
  { mode := (artifactSpawn kind (executionModeOfDefraDB i.requestedMode)
      (i.binding.bind (·.binding))).map ExecutionMode.toDefraDB }

/-- Foreground and background commands launch through the managed command
runner. -/
@[conformance_family]
def artifactSpawnLaunch : Family ArtifactSpawnInput ArtifactSpawnOutput where
  inputs := (ArtifactCases.spawnCases.filter (·.kind != .persistentLsp)).map .ofCase
  model := artifactSpawnMode

/-- A persistent language server is denied by the LSP tool and pool before any
pool lookup or linter dispatch. -/
@[conformance_family]
def artifactSpawnLsp : Family ArtifactSpawnInput ArtifactSpawnOutput where
  inputs := (ArtifactCases.spawnCases.filter (·.kind == .persistentLsp)).map .ofCase
  model := artifactSpawnMode

example : (ArtifactCases.spawnCases.filter (·.kind == .persistentLsp)).all (fun c =>
    c.binding == some ArtifactCases.valid) = true := by
  decide

end Conformance.Contracts
