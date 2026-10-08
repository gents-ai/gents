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

/-- Whether a model selection may move from the `current` backend (absent on
create) to `next`, each given as (provider kind, auth). The model picks a
provider, never an account: an account-free backend is always allowed; an
OAuth backend is allowed when it keeps the current provider and account (which
covers keeping the backend), or, from no backend or another provider, when it
names that provider's default account `dflt kind`. Another account of the
current provider is refused. The owner rule for the default is the provider's
earliest-connected enabled account (the resolver in `oauth_credential.rs`); the
case rows fix it to the original account, `fun _ => none`. -/
def backendChoiceAllowed (dflt : String → Option String)
    (current : Option (String × BackendAuth)) (next : String × BackendAuth) : Bool :=
  match next.2 with
  | .principalOAuth r =>
    match current with
    | some (kind, auth) =>
      if kind = next.1 then decide (auth = next.2) else decide (r = dflt next.1)
    | none => decide (r = dflt next.1)
  | _ => true

/-- A profile edit or create, read through its stored and merged `backend_id`;
an unknown next backend is refused. -/
def profileGuard (backendOf : String → Option (String × BackendAuth))
    (dflt : String → Option String) (stored merged : Doc) : Bool :=
  match (merged "backend_id").bind backendOf with
  | some next => backendChoiceAllowed dflt ((stored "backend_id").bind backendOf) next
  | none => false

theorem profile_keep_current_allowed (dflt : String → Option String)
    (b : String × BackendAuth) : backendChoiceAllowed dflt (some b) b = true := by
  obtain ⟨kind, auth⟩ := b
  cases auth <;> simp [backendChoiceAllowed]

theorem profile_same_provider_account_switch_refused (dflt : String → Option String)
    (kind : String) (a a' : Option String) (h : a ≠ a') :
    backendChoiceAllowed dflt (some (kind, .principalOAuth a)) (kind, .principalOAuth a') =
      false := by
  simp [backendChoiceAllowed, h]

theorem profile_choice_lands_on_current_or_default (dflt : String → Option String)
    (current : Option (String × BackendAuth)) (kind : String) (r : Option String)
    (h : backendChoiceAllowed dflt current (kind, .principalOAuth r) = true) :
    current = some (kind, .principalOAuth r) ∨ r = dflt kind := by
  cases current with
  | none => simp [backendChoiceAllowed] at h; exact .inr h
  | some c =>
    obtain ⟨k, auth⟩ := c
    by_cases hk : k = kind
    · subst hk; simp [backendChoiceAllowed] at h; exact .inl (by rw [h])
    · simp [backendChoiceAllowed, hk] at h; exact .inr h

/-- A selection that names a profile (agent and compaction
`inference_profile_id`, pack inference slots) is the
backend choice between the backends the two profiles reach; `current` is
absent on create, and an unknown next profile is refused. -/
def profileChoiceAllowed (backendOf : String → Option (String × BackendAuth))
    (dflt : String → Option String) (current : Option String) (next : String) : Bool :=
  match backendOf next with
  | some n => backendChoiceAllowed dflt (current.bind backendOf) n
  | none => false

theorem profile_selection_is_backend_choice
    (backendOf : String → Option (String × BackendAuth)) (dflt : String → Option String)
    (current : Option String) (next : String) (n : String × BackendAuth)
    (h : backendOf next = some n) :
    profileChoiceAllowed backendOf dflt current next =
      backendChoiceAllowed dflt (current.bind backendOf) n := by
  simp [profileChoiceAllowed, h]

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
