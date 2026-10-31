import Proofs.Configuration.ClaudePlatform
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.Contracts.ClaudePlatform
open Configuration.ClaudePlatform

private def valuesOf (fields : List Field) (v : Values) : List (String × String) :=
  fields.filterMap fun f =>
    match f with
    | .region => v.region.map ("region", ·)
    | .project => v.project.map ("project", ·)
    | .location => v.location.map ("location", ·)
    | .workspaceId => v.workspaceId.map ("workspace_id", ·)
    | .resource => v.resource.map ("resource", ·)
    | .bearerToken => none

/-- The values the document actually sets: a field outside `fields` is absent. -/
private def restrict (fields : List Field) (v : Values) : Values :=
  { region := if fields.contains .region then v.region else none
    project := if fields.contains .project then v.project else none
    location := if fields.contains .location then v.location else none
    workspaceId := if fields.contains .workspaceId then v.workspaceId else none
    resource := if fields.contains .resource then v.resource else none }

/-- One backend: the kind, the settings it sets (`bearer_token` is a flag), and
the Lean verdict and default base URL. -/
private def caseJson (name kind : String) (present : Option (List Field))
    (values : Values) (bearer : Bool) : String :=
  let v := restrict (present.getD []) values
  let fields := (present.map fun fs => valuesOf fs v).map fun pairs =>
    "{" ++ ",".intercalate (pairs.map fun (k, x) => jsonString k ++ ":" ++ jsonString x) ++
      (if present.any (·.contains .bearerToken) then
        (if pairs.isEmpty then "" else ",") ++ "\"bearer_token\":" ++ toString bearer
       else "") ++ "}"
  "{\"name\":" ++ jsonString name ++ ",\"kind\":" ++ jsonString kind ++
    ",\"claude_platform\":" ++ fields.getD "null" ++
    ",\"valid\":" ++ toString (valid kind present) ++
    ",\"default_base_url\":" ++ (match (if valid kind present then defaultBaseUrl kind v else none) with
      | none => "null"
      | some url => jsonString url) ++ "}"

private def aws : Values := { region := some "us-west-2", workspaceId := some "wrkspc_01" }
private def gcp : Values :=
  { project := some "proj-1", location := some "europe-west4", workspaceId := some "ws-1" }

def casesJson : String := jsonArray [
  caseJson "bedrock_region" "ClaudeBedrock" (some [.region]) aws false,
  caseJson "bedrock_missing_region" "ClaudeBedrock" none aws false,
  caseJson "bedrock_extra_project" "ClaudeBedrock" (some [.region, .project]) gcp false,
  caseJson "mantle_region" "ClaudeMantle" (some [.region]) aws false,
  caseJson "mantle_missing_region" "ClaudeMantle" (some []) aws false,
  caseJson "anthropic_aws_region_workspace" "ClaudeAnthropicAws"
    (some [.region, .workspaceId]) aws false,
  caseJson "anthropic_aws_missing_workspace" "ClaudeAnthropicAws" (some [.region]) aws false,
  caseJson "foundry_resource" "ClaudeFoundry" (some [.resource])
    { resource := some "my-resource" } false,
  caseJson "foundry_bearer_token" "ClaudeFoundry" (some [.resource, .bearerToken])
    { resource := some "my-resource" } true,
  caseJson "foundry_missing_resource" "ClaudeFoundry" (some [.bearerToken]) {} true,
  caseJson "bearer_token_outside_foundry" "ClaudeMantle" (some [.region, .bearerToken]) aws true,
  caseJson "vertex_project_default_location" "ClaudeVertex" (some [.project]) gcp false,
  caseJson "vertex_global" "ClaudeVertex" (some [.project, .location])
    { project := some "proj-1", location := some "global" } false,
  caseJson "vertex_us_multi_region" "ClaudeVertex" (some [.project, .location])
    { project := some "proj-1", location := some "us" } false,
  caseJson "vertex_eu_multi_region" "ClaudeVertex" (some [.project, .location])
    { project := some "proj-1", location := some "eu" } false,
  caseJson "vertex_regional" "ClaudeVertex" (some [.project, .location]) gcp false,
  caseJson "vertex_missing_project" "ClaudeVertex" (some [.location]) gcp false,
  caseJson "google_cloud_default_location" "ClaudeGoogleCloud" (some [.project, .workspaceId])
    gcp false,
  caseJson "google_cloud_location" "ClaudeGoogleCloud"
    (some [.project, .location, .workspaceId]) gcp false,
  caseJson "google_cloud_missing_workspace" "ClaudeGoogleCloud" (some [.project]) gcp false,
  caseJson "gateway_absent" "ClaudeGateway" none {} false,
  caseJson "gateway_empty" "ClaudeGateway" (some []) {} false,
  caseJson "gateway_region" "ClaudeGateway" (some [.region]) aws false,
  caseJson "subscription_absent" "ClaudeCliSubscription" none {} false,
  caseJson "subscription_settings" "ClaudeCliSubscription" (some [.region]) aws false,
  caseJson "openai_empty_settings" "OpenAiCompatible" (some []) {} false
]

end Conformance.Contracts.ClaudePlatform
