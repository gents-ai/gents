import Proofs.CommandPolicy.ArtifactAuthority
import Proofs.Workspace.Properties
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.Contracts
open CommandPolicy
open Conformance.ContractCases

def artifactBindingJson : Option ArtifactBinding → String
  | none => "null"
  | some c => "{\"authority\":" ++ jsonString c.authority.toDefraDB ++
    ",\"state\":" ++ jsonString c.state.toDefraDB ++
    ",\"seal_matches\":" ++ boolString c.sealMatches ++
    ",\"owner_matches\":" ++ boolString c.ownerMatches ++
    ",\"incarnation_matches\":" ++ boolString c.incarnationMatches ++
    ",\"private_root_eligible\":" ++ boolString c.privateRootEligible ++
    ",\"sandbox_enforced\":" ++ boolString c.sandboxEnforced ++ "}"

def artifactModeMeetCasesJson : String := jsonArray <|
  ArtifactCases.meetCases.map fun (a, b, result) =>
    "{\"left\":" ++ jsonString a.toDefraDB ++ ",\"right\":" ++
    jsonString b.toDefraDB ++ ",\"expected\":" ++ jsonString result.toDefraDB ++ "}"

def artifactAdmissionCasesJson : String := jsonArray <|
  ArtifactCases.admissionCases.map fun (name, mode, binding, expected) =>
    "{\"name\":" ++ jsonString name ++ ",\"mode\":" ++ jsonString mode.toDefraDB ++
    ",\"binding\":" ++ artifactBindingJson binding ++
    ",\"expected_admitted\":" ++ boolString expected ++
    ",\"expected_bound_mode\":" ++ jsonOptionalString
      ((IsolatedWorkspace.effectiveBoundCommand mode binding).map ExecutionMode.toDefraDB) ++ "}"

def artifactSpawnCasesJson : String := jsonArray <|
  ArtifactCases.spawnCases.map fun c =>
    let label := match c.kind with
      | .foreground => "foreground"
      | .background => "background"
      | .persistentLsp => "persistent_lsp"
    "{\"name\":" ++ jsonString c.name ++ ",\"kind\":" ++ jsonString label ++ ",\"mode\":\"artifact_write\",\"binding\":" ++
    artifactBindingJson c.binding ++ ",\"expected_mode\":" ++
    jsonOptionalString (c.expected.map ExecutionMode.toDefraDB) ++ "}"

end Conformance.Contracts
