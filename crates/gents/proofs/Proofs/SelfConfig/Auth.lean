import Proofs.Configuration
import Proofs.SelfConfig.Theorems

namespace SelfConfig

open Configuration (BackendAuth)

/-- The OAuth account a backend names; a non-OAuth backend names none. -/
def oauthRef : BackendAuth → Option String
  | .principalOAuth a => a
  | _ => none

/-- Preserve the existing raw-secret restriction while allowing environment
references and the auth kind to be edited as part of the single auth field.
OAuth account references are operator-managed: a principal-OAuth candidate
keeps the stored reference, and a non-OAuth backend counts as the original
account (`none`), so the model can select OAuth but never name an account. -/
def authPatchAllowed (stored candidate : BackendAuth) : Bool :=
  match candidate with
  | .apiKey _ => decide (candidate = stored)
  | .principalOAuth a => decide (a = oauthRef stored)
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

theorem backend_step_cannot_change_oauth_account (decode : Doc → Option BackendAuth)
    (validate : Doc → Bool) (stored merged : Doc) (p : Patch) (old : BackendAuth)
    (a : Option String) (hs : decode stored = some old)
    (h : backendStep decode validate stored p = some merged)
    (hm : decode merged = some (.principalOAuth a)) :
    oauthRef old = a := by
  have hg := (step_accept_validates validate (authGuard decode stored)
    .inferenceBackend stored p merged h).2
  simp [authGuard, hs, hm, authPatchAllowed] at hg
  exact hg.symm

theorem oauth_reference_kept_allowed (old : BackendAuth) (a : Option String)
    (h : oauthRef old = a) : authPatchAllowed old (.principalOAuth a) = true := by
  simp [authPatchAllowed, h]

theorem oauth_original_introduce_allowed (old : BackendAuth)
    (h : ∀ a, old ≠ .principalOAuth a) : authPatchAllowed old (.principalOAuth none) = true := by
  cases old <;> simp_all [authPatchAllowed, oauthRef]

/-- Schema publication is additive, separate from document transactions. The
shared schema owner supplies compatibility and exact-artifact validation;
document ACP is unchanged by publishing a schema. -/
def schemaPublicationAllowed (schemaGranted artifactMatches compatible : Bool) : Bool :=
  schemaGranted && artifactMatches && compatible

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
