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
  simp [applyCreate, hstamped, hclosed, hfresh]

theorem create_preserves_graph_edges (state : RegistryState)
    (request : CreateRequest) :
    (applyCreate state request).graphEdges = state.graphEdges := by
  unfold applyCreate
  split
  · rfl
  · split
    · rfl
    · split <;> rfl

theorem terminalize_preserves_graph_edges (state : RegistryState)
    (ownerPrefix : OwnerPrefix) :
    (terminalizePrefix state ownerPrefix).graphEdges = state.graphEdges := by
  rfl

theorem terminalize_never_reopens (state : RegistryState)
    (ownerPrefix : OwnerPrefix) :
    ownerPrefix ∉ (terminalizePrefix state ownerPrefix).openPrefixes := by
  simp [terminalizePrefix]

end Mailbox
