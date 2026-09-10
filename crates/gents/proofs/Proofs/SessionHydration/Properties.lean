import Proofs.SessionHydration.Executable

namespace SessionHydration

theorem terminalFor_insert_self (st : State) (r : Request) (outcome : Outcome)
    (documents : Finset Document) :
    terminalFor { st with terminals := insert (terminal r outcome documents) st.terminals } r.key := by
  exact ⟨terminal r outcome documents, Finset.mem_insert_self _ _, rfl⟩

/-- A request that fails admission never delivers a document. -/
theorem hydration_request_grants_nothing (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (hnot : ¬ admits cat r) :
    (applyStep cat st r delivery terminalWrite).delivered = st.delivered := by
  by_cases hterminal : terminalFor st r.key
  · simp [applyStep, hterminal]
  · cases terminalWrite <;> simp [applyStep, hterminal, hnot]

/-- Every selected document is scoped to the requester's lineage. -/
theorem selected_tenancy_sound (cat : Catalog) (r : Request) (doc : Document)
    (hdoc : doc ∈ selectedDocuments cat r) : doc.requester = r.requester := by
  simp [selectedDocuments, eligible] at hdoc
  exact hdoc.2.2.1

/-- Selection also preserves the exact agent/session ownership tuple. -/
theorem selected_session_sound (cat : Catalog) (r : Request) (doc : Document)
    (hdoc : doc ∈ selectedDocuments cat r) :
    doc.agent = r.agent ∧ doc.session = r.session := by
  simp [selectedDocuments, eligible] at hdoc
  exact ⟨hdoc.2.2.2.1, hdoc.2.2.2.2⟩

/-- Hydration can only select the client-routable transcript collections named by #1142. -/
theorem selected_collection_sound (cat : Catalog) (r : Request) (doc : Document)
    (hdoc : doc ∈ selectedDocuments cat r) : doc.collection ∈ transcriptCollections := by
  simp [selectedDocuments, eligible] at hdoc
  exact hdoc.2.1

/-- An unknown or mismatched session owner cannot cause delivery. -/
theorem session_ownership_required (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (howner : ownedSession r ∉ cat.sessions) :
    (applyStep cat st r delivery terminalWrite).delivered = st.delivered := by
  apply hydration_request_grants_nothing cat st r delivery terminalWrite
  intro hadmits
  exact howner hadmits.2.2

/-- Membership in another network, or an unverified membership row, cannot admit hydration. -/
theorem selected_network_membership_required (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (hmembership : verifiedMembership cat r ∉ cat.verifiedActiveMemberships) :
    (applyStep cat st r delivery terminalWrite).delivered = st.delivered := by
  apply hydration_request_grants_nothing cat st r delivery terminalWrite
  intro hadmits
  exact hmembership hadmits.2.1

/-- Pairing admission binds the target peer to the exact requester and agent. -/
theorem applied_pairing_route_required (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (hpairing : appliedPairingRoute r ∉ cat.appliedPairingRoutes) :
    (applyStep cat st r delivery terminalWrite).delivered = st.delivered := by
  apply hydration_request_grants_nothing cat st r delivery terminalWrite
  intro hadmits
  exact hpairing hadmits.1

/-- A successfully committed terminal write records the outcome of the bounded
delivery attempt. The theorem deliberately makes no claim about a failed
terminal write. -/
theorem committed_pending_reaches_terminal (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult)
    (hpending : ¬ terminalFor st r.key) :
    terminalFor (applyStep cat st r delivery .committed) r.key := by
  unfold applyStep
  rw [if_neg hpending]
  split
  · cases delivery <;> apply terminalFor_insert_self
  · apply terminalFor_insert_self

/-- Exhausted delivery cannot add transcript documents. -/
theorem exhausted_delivery_grants_nothing (cat : Catalog) (st : State) (r : Request) :
    (applyStep cat st r .exhausted .committed).delivered = st.delivered := by
  unfold applyStep
  split <;> simp_all

/-- A failed terminal write cannot invent a terminal outcome. A successful
delivery remains visible as a set-valued side effect and can be retried
idempotently. -/
theorem failed_terminal_write_stays_pending (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (hpending : ¬ terminalFor st r.key) :
    ¬ terminalFor (applyStep cat st r delivery .failed) r.key := by
  unfold applyStep
  rw [if_neg hpending]
  by_cases hadmits : admits cat r
  · rw [if_pos hadmits]
    cases delivery <;> simpa [terminalFor] using hpending
  · rw [if_neg hadmits]
    exact hpending

theorem terminal_request_is_noop (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (hterminal : terminalFor st r.key) :
    applyStep cat st r delivery terminalWrite = st := by
  simp [applyStep, hterminal]

/-- Re-processing after a crash or duplicate event is idempotent. -/
theorem applyStep_idempotent (cat : Catalog) (st : State) (r : Request)
    (delivery firstReplay : DeliveryResult) :
    applyStep cat (applyStep cat st r delivery .committed) r firstReplay .committed =
      applyStep cat st r delivery .committed := by
  by_cases hterminal : terminalFor st r.key
  · rw [terminal_request_is_noop cat st r delivery .committed hterminal]
    exact terminal_request_is_noop cat st r firstReplay .committed hterminal
  · have hafter := committed_pending_reaches_terminal cat st r delivery hterminal
    exact terminal_request_is_noop cat (applyStep cat st r delivery .committed) r
      firstReplay .committed hafter

/-- Repeating a delivered push after its terminal write failed has the same
set-valued delivery and final terminal state as one successful attempt. -/
theorem delivered_replay_is_idempotent (cat : Catalog) (st : State) (r : Request) :
    applyStep cat (applyStep cat st r .delivered .failed) r .delivered .committed =
      applyStep cat st r .delivered .committed := by
  by_cases hterminal : terminalFor st r.key
  · simp [applyStep, hterminal]
  · by_cases hadmits : admits cat r
    · let docs := selectedDocuments cat r
      have hpendingAfter :
          ¬ terminalFor { st with delivered := st.delivered ∪ docs } r.key := by
        simpa [terminalFor] using hterminal
      simp [applyStep, hterminal, hadmits, docs, hpendingAfter, Finset.union_assoc]
    · simp [applyStep, hterminal, hadmits]

/-- Hydration is not a scope/template transition and cannot flap pairing. -/
theorem pairing_noninterference (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult) :
    (applyStep cat st r delivery terminalWrite).pairingState = st.pairingState := by
  unfold applyStep
  by_cases hterminal : terminalFor st r.key
  · rw [if_pos hterminal]
  · rw [if_neg hterminal]
    by_cases hadmits : admits cat r
    · rw [if_pos hadmits]
      cases delivery <;> cases terminalWrite <;> rfl
    · rw [if_neg hadmits]
      cases terminalWrite <;> rfl
end SessionHydration
