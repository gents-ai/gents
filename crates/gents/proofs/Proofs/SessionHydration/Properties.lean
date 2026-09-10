import Proofs.SessionHydration.Executable

namespace SessionHydration

theorem terminalFor_insert_self (st : State) (r : Request) (outcome : Outcome)
    (documents : Finset Document) :
    terminalFor { st with terminals := insert (terminal r outcome documents) st.terminals } r.key := by
  exact ⟨terminal r outcome documents, Finset.mem_insert_self _ _, rfl⟩

/-- A request that fails admission never confirms a document. -/
theorem hydration_request_grants_nothing (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (hnot : ¬ admits cat r) :
    (applyStep cat st r delivery terminalWrite).confirmedDelivered = st.confirmedDelivered := by
  by_cases hterminal : terminalFor st r.key
  · simp [applyStep, hterminal]
  · cases terminalWrite <;> simp [applyStep, hterminal, hnot]

/-- A request that fails admission never reaches the transport adapter. -/
theorem rejected_hydration_attempts_nothing (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (hnot : ¬ admits cat r) :
    (applyStep cat st r delivery terminalWrite).attempted = st.attempted := by
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
    (applyStep cat st r delivery terminalWrite).confirmedDelivered = st.confirmedDelivered := by
  apply hydration_request_grants_nothing cat st r delivery terminalWrite
  intro hadmits
  exact howner hadmits.2.2

/-- Membership in another network, or an unverified membership row, cannot admit hydration. -/
theorem selected_network_membership_required (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (hmembership : verifiedMembership cat r ∉ cat.verifiedActiveMemberships) :
    (applyStep cat st r delivery terminalWrite).confirmedDelivered = st.confirmedDelivered := by
  apply hydration_request_grants_nothing cat st r delivery terminalWrite
  intro hadmits
  exact hmembership hadmits.2.1

/-- Pairing admission binds the target peer to the exact requester and agent. -/
theorem applied_pairing_route_required (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (hpairing : appliedPairingRoute r ∉ cat.appliedPairingRoutes) :
    (applyStep cat st r delivery terminalWrite).confirmedDelivered = st.confirmedDelivered := by
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

/-- An indeterminate transport result cannot confirm transcript delivery. It
may still have produced partial side effects, represented separately by
`attempted`. -/
theorem indeterminate_delivery_confirms_nothing (cat : Catalog) (st : State) (r : Request) :
    (applyStep cat st r .indeterminate .committed).confirmedDelivered =
      st.confirmedDelivered := by
  by_cases hterminal : terminalFor st r.key
  · simp [applyStep, hterminal]
  · by_cases hadmits : admits cat r
    · simp [applyStep, hterminal, hadmits]
    · simp [applyStep, hterminal, hadmits]

/-- Every admitted transport attempt is limited to the exact selected set,
including when the transport returns an indeterminate result. -/
theorem indeterminate_attempt_is_scope_bounded (cat : Catalog) (st : State) (r : Request)
    (hpending : ¬ terminalFor st r.key) (hadmits : admits cat r) :
    (applyStep cat st r .indeterminate .committed).attempted =
      st.attempted ∪ selectedDocuments cat r := by
  simp [applyStep, hpending, hadmits]

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

/-- Repeating a confirmed push after its terminal write failed has the same
set-valued delivery and final terminal state as one successful attempt. -/
theorem confirmed_replay_is_idempotent (cat : Catalog) (st : State) (r : Request) :
    applyStep cat (applyStep cat st r .confirmed .failed) r .confirmed .committed =
      applyStep cat st r .confirmed .committed := by
  by_cases hterminal : terminalFor st r.key
  · simp [applyStep, hterminal]
  · by_cases hadmits : admits cat r
    · let docs := selectedDocuments cat r
      have hpendingAfter :
          ¬ terminalFor { st with
            attempted := st.attempted ∪ docs
            confirmedDelivered := st.confirmedDelivered ∪ docs } r.key := by
        simpa [terminalFor] using hterminal
      simp [applyStep, hterminal, hadmits, docs, hpendingAfter, Finset.union_assoc]
    · simp [applyStep, hterminal, hadmits]

end SessionHydration
