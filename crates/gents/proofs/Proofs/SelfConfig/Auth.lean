import Proofs.Configuration
import Proofs.SelfConfig.Theorems

namespace SelfConfig

open Configuration (BackendAuth)

/-- Preserve the existing raw-secret restriction while allowing environment and
principal-OAuth references to be edited as part of the single auth field. -/
def authPatchAllowed (stored candidate : BackendAuth) : Bool :=
  match candidate with
  | .apiKey _ => decide (candidate = stored)
  | _ => true

/-- Decoding is the canonical typed-validator boundary, not a second parser. -/
def authGuard (decode : Doc → Option BackendAuth) (stored candidate : Doc) : Bool :=
  match decode stored, decode candidate with
  | some oldAuth, some newAuth => authPatchAllowed oldAuth newAuth
  | _, _ => false

def backendStep (decode : Doc → Option BackendAuth) (validate : Doc → Bool)
    (stored : Doc) (p : Patch) : Option Doc :=
  step validate (authGuard decode stored) .inferenceBackend stored p

theorem backend_step_cannot_set_new_raw_key (decode : Doc → Option BackendAuth)
    (validate : Doc → Bool) (stored merged : Doc) (p : Patch) (key : String)
    (h : backendStep decode validate stored p = some merged)
    (hm : decode merged = some (.apiKey key)) :
    decode stored = some (.apiKey key) := by
  have hg := (step_accept_validates validate (authGuard decode stored)
    .inferenceBackend stored p merged h).2
  cases hs : decode stored with
  | none => simp [authGuard, hs] at hg
  | some oldAuth =>
    simp [authGuard, hs, hm, authPatchAllowed] at hg
    exact congrArg some hg.symm

theorem environment_reference_patch_allowed (old : BackendAuth) (name : String) :
    authPatchAllowed old (.environment name) = true := rfl

theorem oauth_reference_patch_allowed (old : BackendAuth) :
    authPatchAllowed old .principalOAuth = true := rfl

/-- Schema publication is additive, separate from document transactions. The
shared schema owner supplies compatibility and exact-artifact validation;
document ACP is unchanged by publishing a schema. -/
def schemaPublicationAllowed (automationGranted artifactMatches compatible : Bool) : Bool :=
  automationGranted && artifactMatches && compatible

theorem schema_publication_requires_grant (digest compatible : Bool) :
    schemaPublicationAllowed false digest compatible = false := by
  simp [schemaPublicationAllowed]

theorem schema_publication_requires_previewed_artifact (grant compatible : Bool) :
    schemaPublicationAllowed grant false compatible = false := by
  simp [schemaPublicationAllowed]

theorem schema_publication_rejects_incompatible_contract (grant digest : Bool) :
    schemaPublicationAllowed grant digest false = false := by
  simp [schemaPublicationAllowed]

end SelfConfig
