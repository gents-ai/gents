import Proofs.Inference.AccountStore

/-! What an account listing reads of an `OAuthCredential` document, and how it
names the account without a token.

Every client that shows accounts lists them; only the runtime that holds an
account reads its tokens. The tokens' email claim is what names an account, so
the runtime copies it into `account_email` whenever it stores or refreshes the
tokens, and a listing reads that field instead. The document stays
`secret_configuration`: the field changes what a listing selects, not who may
read the document. -/

namespace AccountListing

open AccountStore

/-- The secret fields of an `OAuthCredential` document, the ones no datastore
tool reads (`ToolPolicy.FieldRead.restrictedFields`). -/
def tokenFields : List String := ["access_token", "refresh_token", "id_token"]

/-- What a listing selects. -/
def summaryFields : List String :=
  [ "credential_id", "node_did", "provider", "account_id", "account_email"
  , "chatgpt_plan_type", "access_token_expires_at", "last_refresh", "enabled"
  , "account_ref", "connected_at", "provider_account_key", "label" ]

theorem listing_selects_no_token : ∀ field ∈ summaryFields, field ∉ tokenFields := by
  decide

/-- A stored value a listing shows: present unless blank. The value is shown
as stored. Production treats Unicode whitespace as blank; `String.trim` trims
the ASCII subset. -/
def shown (value : Option String) : Option String :=
  value.filter fun text => text.trim != ""

/-- How a listing names an account: its email, else its account id, else the
provider's account key. -/
def identityOf (accountEmail accountId key : Option String) : Option String :=
  (shown accountEmail).orElse fun _ => (shown accountId).orElse fun _ => shown key

/-- A stored document: the account row and its `account_email`. -/
structure Stored where
  row : Row
  accountEmail : Option String
  deriving DecidableEq, Repr

/-- A store or a refresh writes the new tokens and, when they show an email
claim, that email. Tokens that show none leave the stored email as it was. -/
def write (stored : Stored) (token : String) (claim : Option String) : Stored :=
  { row := { stored.row with token }
    accountEmail := if (shown claim).isSome then claim else stored.accountEmail }

/-- A listed account: no token. -/
structure Summary where
  id : Nat
  provider : Provider
  key : Option String
  accountId : Option String
  accountEmail : Option String
  label : Option String
  enabled : Bool
  deriving DecidableEq, Repr

def summary (stored : Stored) : Summary :=
  { id := stored.row.id
    provider := stored.row.provider
    key := stored.row.key
    accountId := stored.row.accountId
    accountEmail := stored.accountEmail
    label := stored.row.label
    enabled := stored.row.enabled }

def Summary.identity (listed : Summary) : Option String :=
  identityOf listed.accountEmail listed.accountId listed.key

/-- A listing does not depend on the tokens. -/
theorem summary_ignores_the_token (stored : Stored) (token : String) :
    summary { stored with row := { stored.row with token } } = summary stored := rfl

/-- After a write whose tokens show an email, the listing names the account by
that email. -/
theorem written_email_names_the_account (stored : Stored) (token : String)
    (claim : Option String) (email : String) (present : shown claim = some email) :
    (summary (write stored token claim)).identity = some email := by
  simp [write, summary, Summary.identity, identityOf, present, Option.orElse]

/-- A write whose tokens show no email changes nothing a listing reads. -/
theorem write_without_email_keeps_the_listing (stored : Stored) (token : String)
    (claim : Option String) (none : shown claim = Option.none) :
    summary (write stored token claim) = summary stored := by
  simp [write, summary, none]

end AccountListing
