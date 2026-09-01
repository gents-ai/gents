import Proofs.Enrollment.Transition

namespace Enrollment

def decideRequestAdmissible (s : State) (o : Offer) (r : Request) : Bool :=
  decide (requestAdmissible s o r)

theorem decideRequestAdmissible_agrees (s : State) (o : Offer) (r : Request) :
    decideRequestAdmissible s o r = true ↔ requestAdmissible s o r := by
  simp [decideRequestAdmissible]

def decideEnrollmentReady (s : State) (r : Request) : Bool :=
  decide (enrollmentReady s r)

theorem decideEnrollmentReady_agrees (s : State) (r : Request) :
    decideEnrollmentReady s r = true ↔ enrollmentReady s r := by
  simp [decideEnrollmentReady]

end Enrollment
