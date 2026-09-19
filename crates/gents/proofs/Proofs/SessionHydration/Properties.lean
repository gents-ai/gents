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

theorem selected_has_exact_request_input (cat : Catalog) (r : Request)
    (documents : Finset Document) (hselected : selectedDocuments cat r = some documents) :
    ∃ input ∈ cat.closureInputs,
      input.request = r ∧ buildDocuments input = .ok documents :=
  selected_documents_have_exact_input cat r documents hselected

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

/-- A confirmed admitted request reaches a terminal once its receipt commits:
`served` for a successfully built closure, `rejected` for invalid closure input. -/
theorem confirmed_admitted_commit_reaches_terminal (cat : Catalog) (st : State) (r : Request)
    (hpending : ¬ terminalFor st r.key) (hadmits : admits cat r) :
    terminalFor (applyStep cat st r .confirmed .committed) r.key := by
  simp only [applyStep, hpending, hadmits, ↓reduceIte]
  cases hselected : selectedDocuments cat r with
  | none => simp [hselected, terminalFor_insert_self]
  | some documents =>
      simp only [hselected]
      exact terminalFor_insert_self st r .served documents

/-- A denied request reaches `rejected` once its terminal receipt commits. -/
theorem denied_commit_reaches_terminal (cat : Catalog) (st : State) (r : Request)
    (hpending : ¬ terminalFor st r.key) (hnot : ¬ admits cat r) :
    terminalFor (applyStep cat st r .confirmed .committed) r.key := by
  simp only [applyStep, hpending, hnot, ↓reduceIte]
  apply terminalFor_insert_self

/-- An indeterminate transport result cannot confirm transcript delivery. It
may still have produced partial side effects, represented separately by
`attempted`. -/
theorem indeterminate_delivery_confirms_nothing (cat : Catalog) (st : State) (r : Request) :
    (applyStep cat st r .indeterminate .notAttempted).confirmedDelivered =
      st.confirmedDelivered := by
  by_cases hterminal : terminalFor st r.key
  · simp [applyStep, hterminal]
  · by_cases hadmits : admits cat r
    · cases hselected : selectedDocuments cat r <;> simp [applyStep, hterminal, hadmits, hselected]
    · simp [applyStep, hterminal, hadmits]

/-- Every admitted transport attempt is limited to the exact selected set,
including when the transport returns an indeterminate result. -/
theorem indeterminate_attempt_is_scope_bounded (cat : Catalog) (st : State) (r : Request)
    (documents : Finset Document) (hselected : selectedDocuments cat r = some documents)
    (hpending : ¬ terminalFor st r.key) (hadmits : admits cat r) :
    (applyStep cat st r .indeterminate .notAttempted).attempted =
      st.attempted ∪ documents := by
  simp [applyStep, hpending, hadmits, hselected]

/-- An ambiguous transport result is not a rejection: it remains pending so a
later sweep can safely replay the same content-addressed document set. -/
theorem indeterminate_delivery_stays_pending (cat : Catalog) (st : State) (r : Request)
    (hpending : ¬ terminalFor st r.key) :
    ¬ terminalFor (applyStep cat st r .indeterminate .notAttempted) r.key := by
  unfold applyStep
  rw [if_neg hpending]
  by_cases hadmits : admits cat r
  · rw [if_pos hadmits]
    cases hselected : selectedDocuments cat r <;> simpa [hselected, terminalFor] using hpending
  · rw [if_neg hadmits]
    exact hpending

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
    cases hselected : selectedDocuments cat r <;>
      cases delivery <;> simpa [hselected, terminalFor] using hpending
  · rw [if_neg hadmits]
    exact hpending

theorem terminal_request_is_noop (cat : Catalog) (st : State) (r : Request)
    (delivery : DeliveryResult) (terminalWrite : TerminalWriteResult)
    (hterminal : terminalFor st r.key) :
    applyStep cat st r delivery terminalWrite = st := by
  simp [applyStep, hterminal]

/-- Repeating an indeterminate push is set-idempotent and remains pending. -/
theorem indeterminate_replay_is_idempotent (cat : Catalog) (st : State) (r : Request) :
    applyStep cat (applyStep cat st r .indeterminate .notAttempted) r
        .indeterminate .notAttempted =
      applyStep cat st r .indeterminate .notAttempted := by
  by_cases hterminal : terminalFor st r.key
  · simp [applyStep, hterminal]
  · by_cases hadmits : admits cat r
    · cases hselected : selectedDocuments cat r with
      | none => simp [applyStep, hterminal, hadmits, hselected]
      | some docs =>
      have hpendingAfter :
          ¬ terminalFor { st with attempted := st.attempted ∪ docs } r.key := by
        simpa [terminalFor] using hterminal
      simp [applyStep, hterminal, hadmits, hselected, hpendingAfter, Finset.union_assoc]
    · simp [applyStep, hterminal, hadmits]

/-- Repeating a confirmed push after its terminal write failed has the same
set-valued delivery and final terminal state as one successful attempt. -/
theorem confirmed_replay_is_idempotent (cat : Catalog) (st : State) (r : Request) :
    applyStep cat (applyStep cat st r .confirmed .failed) r .confirmed .committed =
      applyStep cat st r .confirmed .committed := by
  by_cases hterminal : terminalFor st r.key
  · simp [applyStep, hterminal]
  · by_cases hadmits : admits cat r
    · cases hselected : selectedDocuments cat r with
      | none => simp [applyStep, hterminal, hadmits, hselected]
      | some docs =>
      have hpendingAfter :
          ¬ terminalFor { st with
            attempted := st.attempted ∪ docs
            confirmedDelivered := st.confirmedDelivered ∪ docs } r.key := by
        simpa [terminalFor] using hterminal
      simp [applyStep, hterminal, hadmits, hselected, hpendingAfter, Finset.union_assoc]
    · simp [applyStep, hterminal, hadmits]

end SessionHydration
