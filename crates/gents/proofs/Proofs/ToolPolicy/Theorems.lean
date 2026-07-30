import Proofs.ToolPolicy.Meet

namespace ToolPolicy

theorem bash_meet_mode_le (a b : BashPolicy) :
    (a.meet b).mode.rank ≤ a.mode.rank ∧
      (a.meet b).mode.rank ≤ b.mode.rank := by
  cases a with
  | mk am an af aa ar as =>
      cases b with
      | mk bm bn bf ba br bs =>
        cases am <;> cases bm <;>
    simp [BashPolicy.meet, ExecMode.rank]

theorem bash_meet_network_le (a b : BashPolicy) :
    (a.meet b).network.rank ≤ a.network.rank ∧
      (a.meet b).network.rank ≤ b.network.rank := by
  cases a with
  | mk am an af aa ar as =>
      cases b with
      | mk bm bn bf ba br bs =>
        cases an <;> cases bn <;>
    simp [BashPolicy.meet, NetMode.rank]

theorem bash_meet_forbidden_superset (a b : BashPolicy) :
    a.forbidden ⊆ (a.meet b).forbidden ∧
      b.forbidden ⊆ (a.meet b).forbidden := by
  constructor
  · intro f hf
    simp [BashPolicy.meet, hf]
  · intro f hf
    simp [BashPolicy.meet, hf]

theorem bash_meet_sandbox (a b : BashPolicy) :
    (a.meet b).sandbox = true → a.sandbox = true ∧ b.sandbox = true := by
  unfold BashPolicy.meet
  intro h
  constructor
  · exact bool_and_left h
  · exact bool_and_right h

theorem bash_meet_allowed_left (a b : BashPolicy) (req : CmdReq)
    (h : (a.meet b).allowedGate req) :
    a.allowedGate req := by
  cases hA : a.allowed <;> cases hB : b.allowed <;>
    simp [BashPolicy.meet, BashPolicy.allowedGate, EndpointScope.meet, hA, hB] at h ⊢
  · obtain ⟨pre, ⟨hka, _hkb⟩, hp⟩ := h
    exact ⟨pre, hka, hp⟩
  · exact h

theorem bash_meet_allowed_right (a b : BashPolicy) (req : CmdReq)
    (h : (a.meet b).allowedGate req) :
    b.allowedGate req := by
  cases hA : a.allowed <;> cases hB : b.allowed <;>
    simp [BashPolicy.meet, BashPolicy.allowedGate, EndpointScope.meet, hA, hB] at h ⊢
  · obtain ⟨pre, ⟨_hka, hkb⟩, hp⟩ := h
    exact ⟨pre, hkb, hp⟩
  · exact h

theorem bash_meet_readonly_left (a b : BashPolicy) (k : String) :
    (a.meet b).readOnly.permits k → a.readOnly.permits k := by
  unfold BashPolicy.meet
  exact EndpointScope.meet_permits_left unitVM a.readOnly b.readOnly k

theorem bash_meet_readonly_right (a b : BashPolicy) (k : String) :
    (a.meet b).readOnly.permits k → b.readOnly.permits k := by
  unfold BashPolicy.meet
  exact EndpointScope.meet_permits_right unitVM a.readOnly b.readOnly k

theorem bash_meet_allowedPrefixMatched_left (a b : BashPolicy) (req : CmdReq) :
    (a.meet b).allowedPrefixMatched req → a.allowedPrefixMatched req := by
  intro h
  cases hA : a.allowed <;> cases hB : b.allowed <;>
    simp [BashPolicy.meet, BashPolicy.allowedPrefixMatched, EndpointScope.meet, hA, hB] at h ⊢
  · obtain ⟨pre, ⟨hka, _hkb⟩, hp⟩ := h
    exact ⟨pre, hka, hp⟩
  · exact h

theorem bash_meet_allowedPrefixMatched_right (a b : BashPolicy) (req : CmdReq) :
    (a.meet b).allowedPrefixMatched req → b.allowedPrefixMatched req := by
  intro h
  cases hA : a.allowed <;> cases hB : b.allowed <;>
    simp [BashPolicy.meet, BashPolicy.allowedPrefixMatched, EndpointScope.meet, hA, hB] at h ⊢
  · obtain ⟨pre, ⟨_hka, hkb⟩, hp⟩ := h
    exact ⟨pre, hkb, hp⟩
  · exact h

theorem bash_meet_network_gate_left (a b : BashPolicy) (req : CmdReq)
    (h : req.wantsNetwork → (a.meet b).network.rank ≥ NetMode.rank .inherit) :
    req.wantsNetwork → a.network.rank ≥ NetMode.rank .inherit := by
  intro hw
  exact le_trans (h hw) (bash_meet_network_le a b).1

theorem bash_meet_network_gate_right (a b : BashPolicy) (req : CmdReq)
    (h : req.wantsNetwork → (a.meet b).network.rank ≥ NetMode.rank .inherit) :
    req.wantsNetwork → b.network.rank ≥ NetMode.rank .inherit := by
  intro hw
  exact le_trans (h hw) (bash_meet_network_le a b).2

theorem bash_meet_mode_gate_left (a b : BashPolicy) (req : CmdReq)
    (h : (a.meet b).modeGate req) :
    a.modeGate req := by
  cases ha : a.mode <;> simp [BashPolicy.modeGate, ha]
  case readOnly =>
    have hmeet : (a.meet b).mode = ExecMode.readOnly := by
      unfold BashPolicy.meet
      rw [ha]
      cases b.mode <;> simp [ExecMode.rank]
    rw [BashPolicy.modeGate, hmeet] at h
    simp at h
    obtain ⟨hw, hro⟩ := h
    refine ⟨hw, ?_⟩
    rcases hro with hhead | hprefix
    · exact Or.inl (bash_meet_readonly_left a b req.cmdHead hhead)
    · exact Or.inr (bash_meet_allowedPrefixMatched_left a b req hprefix)

theorem bash_meet_mode_gate_right (a b : BashPolicy) (req : CmdReq)
    (h : (a.meet b).modeGate req) :
    b.modeGate req := by
  cases hb : b.mode <;> simp [BashPolicy.modeGate, hb]
  case readOnly =>
    have hmeet : (a.meet b).mode = ExecMode.readOnly := by
      unfold BashPolicy.meet
      rw [hb]
      cases a.mode <;> simp [ExecMode.rank]
    rw [BashPolicy.modeGate, hmeet] at h
    simp at h
    obtain ⟨hw, hro⟩ := h
    refine ⟨hw, ?_⟩
    rcases hro with hhead | hprefix
    · exact Or.inl (bash_meet_readonly_right a b req.cmdHead hhead)
    · exact Or.inr (bash_meet_allowedPrefixMatched_right a b req hprefix)

theorem BashPolicy.meet_permits_left (a b : BashPolicy) (req : CmdReq) :
    (a.meet b).permits req → a.permits req := by
  intro h
  rcases h with ⟨hs, hf, hal, hn, hmode⟩
  refine ⟨(bash_meet_sandbox a b hs).1, ?_,
    bash_meet_allowed_left a b req hal, ?_,
    bash_meet_mode_gate_left a b req hmode⟩
  · intro f hf'
    exact hf f ((bash_meet_forbidden_superset a b).1 hf')
  · exact bash_meet_network_gate_left a b req hn

theorem BashPolicy.meet_permits_right (a b : BashPolicy) (req : CmdReq) :
    (a.meet b).permits req → b.permits req := by
  intro h
  rcases h with ⟨hs, hf, hal, hn, hmode⟩
  refine ⟨(bash_meet_sandbox a b hs).2, ?_,
    bash_meet_allowed_right a b req hal, ?_,
    bash_meet_mode_gate_right a b req hmode⟩
  · intro f hf'
    exact hf f ((bash_meet_forbidden_superset a b).2 hf')
  · exact bash_meet_network_gate_right a b req hn

end ToolPolicy
