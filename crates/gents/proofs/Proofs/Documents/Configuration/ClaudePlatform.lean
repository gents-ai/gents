import Proofs.Configuration

namespace Configuration.ClaudePlatform

/-- The typed `claude_platform` settings of an InferenceBackend. A platform
backend names the account-independent place it is served from; the credential
stays in `BackendAuth`. `bearerToken` selects the Authorization bearer form of
a static Foundry credential instead of `x-api-key`. -/
inductive Field where
  | region | project | location | workspaceId | resource | bearerToken
  deriving DecidableEq, Repr

def Field.wireName : Field → String
  | .region => "region"
  | .project => "project"
  | .location => "location"
  | .workspaceId => "workspace_id"
  | .resource => "resource"
  | .bearerToken => "bearer_token"

/-- Required and optional settings of a Claude platform provider kind; `none`
for every other kind, which must carry no settings at all. -/
def shape : String → Option (List Field × List Field)
  | "ClaudeBedrock" => some ([.region], [])
  | "ClaudeVertex" => some ([.project], [.location])
  | "ClaudeFoundry" => some ([.resource], [.bearerToken])
  | "ClaudeAnthropicAws" => some ([.region, .workspaceId], [])
  | "ClaudeMantle" => some ([.region], [])
  | "ClaudeGoogleCloud" => some ([.project, .workspaceId], [.location])
  | "ClaudeGateway" => some ([], [])
  | _ => none

/-- `present` is the set of fields the document sets, or `none` when it has no
`claude_platform` object. A platform kind needs every required field and refuses
any field outside its shape; any other kind refuses the object itself. -/
def valid (kind : String) (present : Option (List Field)) : Bool :=
  match shape kind, present with
  | none, none => true
  | none, some _ => false
  | some (required, _), none => required.isEmpty
  | some (required, optional), some fields =>
    required.all (fun f => fields.contains f) &&
      fields.all (fun f => required.contains f || optional.contains f)

theorem other_kind_refuses_settings (kind : String) (fields : List Field)
    (h : shape kind = none) : valid kind (some fields) = false := by
  simp [valid, h]

theorem gateway_takes_no_settings (f : Field) :
    valid "ClaudeGateway" (some [f]) = false := by
  cases f <;> decide

/-- The setting values the default base URL reads. -/
structure Values where
  region : Option String := none
  project : Option String := none
  location : Option String := none
  workspaceId : Option String := none
  resource : Option String := none
  deriving Repr

/-- The Vertex host of a location: the global and the multi-region hosts are
named, every other location is a regional prefix. -/
def vertexBase (location : String) : String :=
  if location = "global" then "https://aiplatform.googleapis.com/v1"
  else if location = "us" then "https://aiplatform.us.rep.googleapis.com/v1"
  else if location = "eu" then "https://aiplatform.eu.rep.googleapis.com/v1"
  else "https://" ++ location ++ "-aiplatform.googleapis.com/v1"

/-- The base URL a platform kind is served from when `endpoint` is not
overridden; `none` when a needed setting is missing or the kind has no default. -/
def defaultBaseUrl (kind : String) (v : Values) : Option String :=
  match kind with
  | "ClaudeBedrock" => v.region.map fun r => "https://bedrock-runtime." ++ r ++ ".amazonaws.com"
  | "ClaudeMantle" => v.region.map fun r => "https://bedrock-mantle." ++ r ++ ".api.aws/anthropic"
  | "ClaudeAnthropicAws" =>
    v.region.map fun r => "https://aws-external-anthropic." ++ r ++ ".api.aws"
  | "ClaudeFoundry" =>
    v.resource.map fun r => "https://" ++ r ++ ".services.ai.azure.com/anthropic/"
  | "ClaudeVertex" => some (vertexBase (v.location.getD "us-east5"))
  | "ClaudeGoogleCloud" =>
    match v.project, v.workspaceId with
    | some p, some w =>
      some ("https://claude.googleapis.com/v1/projects/" ++ p ++ "/locations/" ++
        v.location.getD "global" ++ "/workspaces/" ++ w ++ "/invoke")
    | _, _ => none
  | _ => none

end Configuration.ClaudePlatform
