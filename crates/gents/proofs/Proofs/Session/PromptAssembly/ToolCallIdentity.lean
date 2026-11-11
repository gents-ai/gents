namespace PromptAssembly.ToolCallIdentity

structure Identity where
  nativeId : String
  callId : Option String
  deriving DecidableEq, Repr

/-- Responses requires a call ID on both a tool announcement and its result.
Other providers may retain only their native tool-use ID. Filling the absent
wire field reuses that identity; it does not rename the stored call or replace
an explicit provider call ID. Apply only to the transient provider view. -/
def project (responses : Bool) (identity : Identity) : Identity :=
  if responses then
    { identity with callId := some (identity.callId.getD identity.nativeId) }
  else identity

/-- An OpenAI function-call item ID is an optional provider-issued handle,
distinct from the call/result correlation ID. Foreign native tool-use IDs
must not be presented as those handles. The Responses input contract permits
omitting the item ID while requiring the call ID. -/
def openAiItemId (id : String) : Option String :=
  if id.startsWith "fc_" then some id else none

theorem foreign_item_id_omitted (id : String) (foreign : id.startsWith "fc_" = false) :
    openAiItemId id = none := by simp [openAiItemId, foreign]

theorem provider_item_id_preserved (id : String) (native : id.startsWith "fc_" = true) :
    openAiItemId id = some id := by simp [openAiItemId, native]

theorem responses_call_id_present (identity : Identity) :
    (project true identity).callId.isSome = true := rfl

theorem native_id_preserved (responses : Bool) (identity : Identity) :
    (project responses identity).nativeId = identity.nativeId := by
  cases responses <;> rfl

theorem explicit_call_id_preserved (responses : Bool) (id call : String) :
    project responses ⟨id, some call⟩ = ⟨id, some call⟩ := by
  cases responses <;> rfl

theorem absent_responses_id_reuses_native (id : String) :
    project true ⟨id, none⟩ = ⟨id, some id⟩ := rfl

theorem other_wire_unchanged (identity : Identity) :
    project false identity = identity := rfl

theorem idempotent (responses : Bool) (identity : Identity) :
    project responses (project responses identity) = project responses identity := by
  cases responses <;> rfl

theorem matching_pair_preserved (responses : Bool) (call result : Identity)
    (same : call = result) :
    project responses call = project responses result := congrArg (project responses) same

end PromptAssembly.ToolCallIdentity
