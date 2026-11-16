import Proofs.Session.Request.Transition

namespace RequestContext

theorem dead_transition_is_expired_preclaim {pre post : RequestContext}
    (h : Transition pre post) (dead : post.state = .dead) :
    pre.state = .pending ∧ ∃ t, pre.validUntil = some t ∧ pre.currentTime > t := by
  cases h <;> simp_all

end RequestContext
