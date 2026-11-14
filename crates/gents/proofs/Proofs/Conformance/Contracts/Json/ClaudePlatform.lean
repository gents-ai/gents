import Proofs.Documents.Configuration.ClaudePlatform
import Proofs.Conformance.Family

namespace Conformance.Contracts.ClaudePlatform
open Configuration.ClaudePlatform Lean

structure Settings where
  region : Option String
  project : Option String
  location : Option String
  workspaceId : Option String
  resource : Option String
  bearerToken : Option Bool
  deriving ToJson

structure Input where
  name : String
  kind : String
  claudePlatform : Option Settings
  deriving ToJson

structure Output where
  valid : Bool
  defaultBaseUrl : Option String
  deriving ToJson

private def settings (fields : List Field) (v : Values) (bearer : Bool) : Settings :=
  { region := if fields.contains .region then v.region else none
    project := if fields.contains .project then v.project else none
    location := if fields.contains .location then v.location else none
    workspaceId := if fields.contains .workspaceId then v.workspaceId else none
    resource := if fields.contains .resource then v.resource else none
    bearerToken := if fields.contains .bearerToken then some bearer else none }

private def presentFields (s : Settings) : List Field :=
  (if s.region.isSome then [Field.region] else []) ++
  (if s.project.isSome then [Field.project] else []) ++
  (if s.location.isSome then [Field.location] else []) ++
  (if s.workspaceId.isSome then [Field.workspaceId] else []) ++
  (if s.resource.isSome then [Field.resource] else []) ++
  (if s.bearerToken.isSome then [Field.bearerToken] else [])

private def valuesOfSettings (s : Settings) : Values :=
  { region := s.region, project := s.project, location := s.location
    workspaceId := s.workspaceId, resource := s.resource }

private def caseInput (name kind : String) (present : Option (List Field))
    (values : Values) (bearer : Bool) : Input :=
  { name, kind, claudePlatform := present.map fun fields => settings fields values bearer }

private def aws : Values := { region := some "us-west-2", workspaceId := some "wrkspc_01" }
private def gcp : Values :=
  { project := some "proj-1", location := some "europe-west4", workspaceId := some "ws-1" }

private def cases : List Input := [
  caseInput "bedrock_region" "ClaudeBedrock" (some [.region]) aws false,
  caseInput "bedrock_missing_region" "ClaudeBedrock" none aws false,
  caseInput "bedrock_extra_project" "ClaudeBedrock" (some [.region, .project]) gcp false,
  caseInput "mantle_region" "ClaudeMantle" (some [.region]) aws false,
  caseInput "mantle_missing_region" "ClaudeMantle" (some []) aws false,
  caseInput "anthropic_aws_region_workspace" "ClaudeAnthropicAws"
    (some [.region, .workspaceId]) aws false,
  caseInput "anthropic_aws_missing_workspace" "ClaudeAnthropicAws" (some [.region]) aws false,
  caseInput "foundry_resource" "ClaudeFoundry" (some [.resource])
    { resource := some "my-resource" } false,
  caseInput "foundry_bearer_token" "ClaudeFoundry" (some [.resource, .bearerToken])
    { resource := some "my-resource" } true,
  caseInput "foundry_missing_resource" "ClaudeFoundry" (some [.bearerToken]) {} true,
  caseInput "bearer_token_outside_foundry" "ClaudeMantle" (some [.region, .bearerToken]) aws true,
  caseInput "vertex_project_default_location" "ClaudeVertex" (some [.project]) gcp false,
  caseInput "vertex_global" "ClaudeVertex" (some [.project, .location])
    { project := some "proj-1", location := some "global" } false,
  caseInput "vertex_us_multi_region" "ClaudeVertex" (some [.project, .location])
    { project := some "proj-1", location := some "us" } false,
  caseInput "vertex_eu_multi_region" "ClaudeVertex" (some [.project, .location])
    { project := some "proj-1", location := some "eu" } false,
  caseInput "vertex_regional" "ClaudeVertex" (some [.project, .location]) gcp false,
  caseInput "vertex_missing_project" "ClaudeVertex" (some [.location]) gcp false,
  caseInput "google_cloud_default_location" "ClaudeGoogleCloud" (some [.project, .workspaceId])
    gcp false,
  caseInput "google_cloud_location" "ClaudeGoogleCloud"
    (some [.project, .location, .workspaceId]) gcp false,
  caseInput "google_cloud_missing_workspace" "ClaudeGoogleCloud" (some [.project]) gcp false,
  caseInput "gateway_absent" "ClaudeGateway" none {} false,
  caseInput "gateway_empty" "ClaudeGateway" (some []) {} false,
  caseInput "gateway_region" "ClaudeGateway" (some [.region]) aws false,
  caseInput "subscription_absent" "ClaudeCliSubscription" none {} false,
  caseInput "subscription_settings" "ClaudeCliSubscription" (some [.region]) aws false,
  caseInput "openai_empty_settings" "OpenAiCompatible" (some []) {} false
]

@[conformance_family]
def outcome : Conformance.Family Input Output where
  inputs := cases
  model := fun i =>
    let present := i.claudePlatform.map presentFields
    let values := (i.claudePlatform.map valuesOfSettings).getD {}
    { valid := valid i.kind present
      defaultBaseUrl := if valid i.kind present then defaultBaseUrl i.kind values else none }

end Conformance.Contracts.ClaudePlatform
