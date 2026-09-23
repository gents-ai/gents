import Proofs.Mailbox.Transition

namespace Mailbox

theorem terminal_statuses_are_stuck (status : Status)
    (hterminal : status.terminal = true) (action : ResolutionAction) :
    stepStatus? status action = none := by
  cases status <;> cases action <;> simp_all [Status.terminal, stepStatus?]

theorem legal_resolution_starts_open (item post : Item)
    (action : ResolutionAction) (h : applyResolution? item action = some post) :
    item.status = .open := by
  cases action <;> simp only [applyResolution?] at h <;> split at h
  <;> simp_all

theorem tenancy_frozen (item post : Item) (action : ResolutionAction)
    (h : applyResolution? item action = some post) :
    post.identity = item.identity := by
  cases action <;> simp only [applyResolution?] at h <;> split at h
  · cases h
    rfl
  · simp_all
  · cases h
    rfl
  · simp_all
  · cases h
    rfl
  · simp_all

theorem acted_has_satisfying_document (item post : Item) (docId : String)
    (h : applyResolution? item (.act docId) = some post) :
    post.status = .acted ∧ post.resolvedDocId = docId ∧ docId ≠ "" := by
  simp only [applyResolution?] at h
  split at h
  · rename_i admitted
    cases h
    exact ⟨rfl, rfl, admitted.2⟩
  · simp_all

theorem dismiss_is_owner_only (item post : Item) (principalDid : String)
    (h : applyResolution? item (.dismiss principalDid) = some post) :
    principalDid = item.identity.requesterDid ∧ post.status = .dismissed := by
  simp only [applyResolution?] at h
  split at h
  · rename_i admitted
    cases h
    exact ⟨admitted.2, rfl⟩
  · simp_all

theorem deadline_expiry (item : Item) (hopen : item.status = .open) :
    ∃ post, applyResolution? item (.expire true) = some post ∧
      post.status = .expired := by
  refine ⟨{ item with status := .expired, resolvedDocId := "" }, ?_, rfl⟩
  simp [applyResolution?, hopen]

theorem unstamped_create_changes_nothing (state : RegistryState)
    (request : CreateRequest) (h : stamped request = false) :
    applyCreate state request = state := by
  simp [applyCreate, h]

theorem open_retry_is_idempotent (state : RegistryState)
    (request : CreateRequest) (hstamped : stamped request = true)
    (hopen : request.identity.ownerPrefix ∈ state.openPrefixes) :
    applyCreate state request = state := by
  simp [applyCreate, hstamped, hopen]

theorem duplicate_key_fails_closed (state : RegistryState)
    (request : CreateRequest) (hstamped : stamped request = true)
    (hclosed : request.identity.ownerPrefix ∉ state.openPrefixes)
    (hkey : request.identity.itemKey ∈ state.itemKeys) :
    applyCreate state request = state := by
  simp [applyCreate, hstamped, hclosed, hkey]

theorem admitted_fresh_create_records_prefix (state : RegistryState)
    (request : CreateRequest) (hstamped : stamped request = true)
    (hclosed : request.identity.ownerPrefix ∉ state.openPrefixes)
    (hfresh : request.identity.itemKey ∉ state.itemKeys) :
    request.identity.ownerPrefix ∈ (applyCreate state request).openPrefixes ∧
      request.identity.itemKey ∈ (applyCreate state request).itemKeys := by
  have hcreate : applyCreate state request =
      { state with rows := ⟨request.storedEnvelope, true⟩ :: state.rows } := by
    simp [applyCreate, hstamped, hclosed, hfresh]
  rw [hcreate]
  simp [RegistryState.openPrefixes, RegistryState.itemKeys,
    CreateRequest.storedEnvelope]

theorem admitted_fresh_create_stores_envelope (state : RegistryState)
    (request : CreateRequest) (hstamped : stamped request = true)
    (hclosed : request.identity.ownerPrefix ∉ state.openPrefixes)
    (hfresh : request.identity.itemKey ∉ state.itemKeys) :
    request.storedEnvelope ∈
      ((applyCreate state request).rows.map (·.envelope)) := by
  simp [applyCreate, hstamped, hclosed, hfresh]

theorem stored_receipt_is_durable (state post : RegistryState)
    (request : CreateRequest) (row : StoredEnvelope)
    (h : storedCreateReceipt? state request = some (post, row)) :
    (∃ stored ∈ post.rows, stored.envelope = row ∧ stored.isOpen = true) ∧
      row.identity.ownerPrefix = request.identity.ownerPrefix ∧
      row.handling = request.handling ∧ row.sessionId = request.sessionId ∧
      row.requestId = request.requestId ∧ row.content = request.content := by
  by_cases hstamp : stamped request = true
  · let next := applyCreate state request
    cases hfind : next.rows.find? (fun found =>
        found.isOpen &&
        found.envelope.identity.ownerPrefix == request.identity.ownerPrefix &&
        found.envelope.identity.agentDid == request.identity.agentDid &&
        found.envelope.handling == request.handling &&
        found.envelope.sessionId == request.sessionId &&
        found.envelope.requestId == request.requestId &&
        found.envelope.content == request.content) with
    | none => simp [storedCreateReceipt?, hstamp, next, hfind] at h
    | some found =>
        have hmem := List.mem_of_find?_eq_some hfind
        have hpred := List.find?_some hfind
        simp [storedCreateReceipt?, hstamp, next, hfind] at h
        rcases h with ⟨rfl, rfl⟩
        constructor
        · have hopen : found.isOpen = true := by
            simp only [Bool.and_eq_true] at hpred
            aesop
          exact ⟨found, by simpa [next] using hmem, rfl, hopen⟩
        · simp_all
  · simp [storedCreateReceipt?, hstamp] at h

theorem terminalize_never_reopens (state : RegistryState)
    (ownerPrefix : OwnerPrefix) :
    ownerPrefix ∉ (terminalizePrefix state ownerPrefix).openPrefixes := by
  intro hmember
  simp only [terminalizePrefix, RegistryState.openPrefixes,
    List.mem_toFinset, List.mem_map] at hmember
  obtain ⟨row, hfiltered, hprefix⟩ := hmember
  have ⟨hmapped, hisOpen⟩ := List.mem_filter.mp hfiltered
  obtain ⟨original, _, heq⟩ := List.mem_map.mp hmapped
  rw [← heq] at hisOpen hprefix
  by_cases h : original.envelope.identity.ownerPrefix = ownerPrefix
  · simp [h] at hisOpen
  · simp [h] at hprefix

end Mailbox
