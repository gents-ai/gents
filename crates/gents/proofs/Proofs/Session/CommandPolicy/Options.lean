import Proofs.Shared.Basic

namespace CommandPolicy

/-- The letters of a single-dash option cluster: `-ni` is `n` then `i`. A long
option, a lone `-` and an operand are not clusters. -/
def shortCluster? (arg : String) : Option (List Char) :=
  match arg.toList with
  | '-' :: c :: rest => if c = '-' then none else some (c :: rest)
  | _ => none

/-- The name of a long option, `--name` or `--name=value`. -/
def longName? (arg : String) : Option String :=
  match arg.toList with
  | '-' :: '-' :: c :: rest => some (String.mk ((c :: rest).takeWhile (· ≠ '=')))
  | _ => none

/-- GNU getopt, git's option parser for `grep` and curl before 8.7 accept any
unique prefix of a long option, so an argument names `option` when its long
name is a prefix of it at least `minimum` characters long. -/
def abbreviatesLong (arg option : String) (minimum : Nat := 1) : Bool :=
  match longName? arg with
  | some name => decide (minimum ≤ name.length) && option.startsWith name
  | none => false

def abbreviatesAnyLong (arg : String) (options : List String) : Bool :=
  options.any (abbreviatesLong arg ·)

/-- The first letter of a cluster that `denied` names. A letter in `valued`
takes the rest of the cluster as its value, so the scan stops there. -/
def clusterDeniedLetter (denied valued : List Char) : List Char → Option Char
  | [] => none
  | c :: rest =>
      if denied.contains c then some c
      else if valued.contains c then none
      else clusterDeniedLetter denied valued rest

def shortClusterDenied (arg : String) (denied valued : List Char) : Bool :=
  match shortCluster? arg with
  | some letters => (clusterDeniedLetter denied valued letters).isSome
  | none => false

/-- Whether a cluster ends in a letter that takes the next argument as its value. -/
def clusterTakesNext (valued : List Char) : List Char → Bool
  | [] => false
  | [c] => valued.contains c
  | c :: rest => if valued.contains c then false else clusterTakesNext valued rest

def hasInfix (value part : String) : Bool :=
  decide ((value.splitOn part).length > 1)

/-- The name of a Go `flag` option, which takes one or two dashes: `-web`,
`--web=true`. -/
def goFlagName? (arg : String) : Option String :=
  match arg.toList with
  | '-' :: '-' :: rest => some (String.mk (rest.takeWhile (· ≠ '=')))
  | '-' :: rest => some (String.mk (rest.takeWhile (· ≠ '=')))
  | _ => none

theorem cluster_finds_a_letter_after_flags :
    shortClusterDenied "-ni" ['i'] ['e'] = true := by native_decide

theorem cluster_stops_at_a_valued_letter :
    shortClusterDenied "-es/i/x/" ['i'] ['e'] = false := by native_decide

theorem abbreviation_names_the_option :
    abbreviatesLong "--cookie-j" "cookie-jar" = true ∧
      abbreviatesLong "--trace-time" "trace" = false := by native_decide

end CommandPolicy
