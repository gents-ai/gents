import Proofs.Basic

/-!
# Command Policy Environment Filtering

Symbolic model of `build_shell_env_from_vars`.
-/

namespace CommandPolicy

def fallbackPath : String :=
  "/usr/bin:/bin:/usr/sbin:/sbin"

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

namespace EnvKey

def toContract : EnvKey → String
  | .path => "path"
  | .shell => "shell"
  | .tmpdir => "tmpdir"
  | .temp => "temp"
  | .tmp => "tmp"
  | .home => "home"
  | .lang => "lang"
  | .lcAll => "lcAll"
  | .lcCtype => "lcCtype"
  | .logname => "logname"
  | .user => "user"
  | .pager => "pager"
  | .gitPager => "gitPager"
  | .noColor => "noColor"
  | .cliColor => "cliColor"
  | .term => "term"
  | .key => "key"
  | .secret => "secret"
  | .token => "token"
  | .other => "other"

def sampleName : EnvKey → String
  | .path => "PATH"
  | .shell => "SHELL"
  | .tmpdir => "TMPDIR"
  | .temp => "TEMP"
  | .tmp => "TMP"
  | .home => "HOME"
  | .lang => "LANG"
  | .lcAll => "LC_ALL"
  | .lcCtype => "LC_CTYPE"
  | .logname => "LOGNAME"
  | .user => "USER"
  | .pager => "PAGER"
  | .gitPager => "GIT_PAGER"
  | .noColor => "NO_COLOR"
  | .cliColor => "CLICOLOR"
  | .term => "TERM"
  | .key => "OPENAI_API_KEY"
  | .secret => "DATABASE_SECRET"
  | .token => "SESSION_TOKEN"
  | .other => "UNRELATED"

end EnvKey

/-- Symbolic values preserved or forced by the filtered shell environment.
    The forced tags correspond to Rust constants: `cat`, `1`, `0`, and `dumb`. -/
inductive EnvValue where
  | inherited
  | fallbackPath
  | forcedCat
  | forcedNoColor
  | forcedCliColorOff
  | forcedDumb
  deriving DecidableEq, Repr

namespace EnvValue

def toContract : EnvValue → String
  | .inherited => "inherited"
  | .fallbackPath => "fallbackPath"
  | .forcedCat => "forcedCat"
  | .forcedNoColor => "forcedNoColor"
  | .forcedCliColorOff => "forcedCliColorOff"
  | .forcedDumb => "forcedDumb"

def toRustValue (inputValue : String) : EnvValue → String
  | .inherited => inputValue
  | .fallbackPath => CommandPolicy.fallbackPath
  | .forcedCat => "cat"
  | .forcedNoColor => "1"
  | .forcedCliColorOff => "0"
  | .forcedDumb => "dumb"

end EnvValue

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
