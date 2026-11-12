/-! How a failed Claude token refresh is classified from its HTTP status and the
OAuth error code of its body. A refresh is expired when its status is 401, or
when it is a 400 with `invalid_grant`; it is not entitled on a 403; every other
failure is an unclassified rejection. The account-on-hold and error-description
reads are outside this model: it covers bodies that carry only an error code. -/

namespace RefreshClassification

inductive RefreshClass where
  | expired
  | notEntitled
  | other
  deriving DecidableEq, Repr

def refreshClass (status : Nat) (errorCode : Option String) : RefreshClass :=
  if status = 401 then .expired
  else if status = 400 ∧ errorCode = some "invalid_grant" then .expired
  else if status = 403 then .notEntitled
  else .other

theorem an_unauthorized_refresh_is_expired (errorCode : Option String) :
    refreshClass 401 errorCode = .expired := by
  simp [refreshClass]

theorem a_400_invalid_grant_is_expired :
    refreshClass 400 (some "invalid_grant") = .expired := by
  decide

theorem a_400_without_invalid_grant_is_other (errorCode : Option String)
    (h : errorCode ≠ some "invalid_grant") :
    refreshClass 400 errorCode = .other := by
  simp [refreshClass, h]

theorem an_invalid_scope_refresh_is_other :
    refreshClass 400 (some "invalid_scope") = .other := by
  decide

theorem a_forbidden_refresh_is_not_entitled (errorCode : Option String) :
    refreshClass 403 errorCode = .notEntitled := by
  simp [refreshClass]

theorem a_server_error_refresh_is_other (errorCode : Option String) :
    refreshClass 500 errorCode = .other := by
  simp [refreshClass]

end RefreshClassification
