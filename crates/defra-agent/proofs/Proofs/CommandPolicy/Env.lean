import Proofs.Basic

/-!
# Command Policy Environment Filtering

Symbolic model of `build_shell_env_from_vars`.
-/

namespace CommandPolicy

/-- Environment-name classes relevant to the shell filter. -/
inductive EnvKey where
  | path
  | shell
  | tmpdir
  | temp
  | tmp
  | home
  | lang
  | lcAll
  | lcCtype
  | logname
  | user
  | pager
  | gitPager
  | noColor
  | cliColor
  | term
  | key
  | secret
  | token
  | other
  deriving DecidableEq, Repr

/-- Values preserved or forced by the filtered shell environment. -/
inductive EnvValue where
  | inherited
  | fallbackPath
  | forcedCat
  | forcedNoColor
  | forcedCliColorOff
  | forcedDumb
  deriving DecidableEq, Repr

/-- Rust rejects names containing KEY, SECRET, or TOKEN. -/
def containsSecretMarker : EnvKey → Bool
  | .key => true
  | .secret => true
  | .token => true
  | _ => false

/-- Core environment names that may be inherited from the parent process. -/
def coreEnvKey : EnvKey → Bool
  | .path => true
  | .shell => true
  | .tmpdir => true
  | .temp => true
  | .tmp => true
  | .home => true
  | .lang => true
  | .lcAll => true
  | .lcCtype => true
  | .logname => true
  | .user => true
  | _ => false

/-- Noninteractive values forced after inheritance and secret filtering. -/
def forcedEnvValue : EnvKey → Option EnvValue
  | .pager => some .forcedCat
  | .gitPager => some .forcedCat
  | .noColor => some .forcedNoColor
  | .cliColor => some .forcedCliColorOff
  | .term => some .forcedDumb
  | _ => none

/-- Filtered environment lookup. `inputHas` abstracts whether a non-secret
    parent variable exists. -/
def filteredEnv
    (inputHas : EnvKey → Bool)
    (envKey : EnvKey) : Option EnvValue :=
  if containsSecretMarker envKey then
    none
  else
    match forcedEnvValue envKey with
    | some value => some value
    | none =>
        match envKey with
        | .path =>
            if inputHas .path then some .inherited else some .fallbackPath
        | _ =>
            if coreEnvKey envKey && inputHas envKey then some .inherited else none

end CommandPolicy
