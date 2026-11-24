import Proofs.Inference.AccountListing
import Proofs.Conformance.Family

namespace AccountListing.Contracts

open AccountListing

structure Input where
  name : String
  accountEmail : Option String
  accountId : Option String
  key : Option String
  deriving Lean.ToJson

/-- What a listing selects and how it names the account. -/
structure Listing where
  selected : List String
  identity : Option String
  deriving Lean.ToJson

private def case (name : String) (accountEmail accountId key : Option String) : Input :=
  { name, accountEmail, accountId, key }

private def cases : List Input :=
  [ case "the_email_names_the_account" (some "person@example.test") (some "acct") (some "key")
  , case "the_account_id_without_an_email" none (some "acct") (some "key")
  , case "the_key_without_an_email_or_account_id" none none (some "key")
  , case "nothing_names_an_account_without_any" none none none
  , case "a_blank_email_is_not_shown" (some " ") (some "acct") none
  , case "a_blank_account_id_is_not_shown" none (some "") (some "key")
  , case "a_shown_value_keeps_its_spacing" (some " person@example.test ") none none ]

@[conformance_family]
def listing : Conformance.Family Input Listing where
  inputs := cases
  model := fun i =>
    { selected := summaryFields, identity := identityOf i.accountEmail i.accountId i.key }

end AccountListing.Contracts
