import Proofs.Session.CommandPolicy.Types
import Proofs.Session.CommandPolicy.Options

namespace CommandPolicy

def matchesPrefix : List String → List String → Bool
  | [], _ => true
  | _ :: _, [] => false
  | p :: ps, a :: rest =>
      if p = a then matchesPrefix ps rest else false

def firstMatchingPrefix (argv : List String) : List (List String) → Option (List String)
  | [] => none
  | candidate :: rest =>
      if matchesPrefix candidate argv then
        some candidate
      else
        firstMatchingPrefix argv rest

def commandAllowlisted (command : String) (allowlist : List String) : Bool :=
  allowlist.any (fun allowed => decide (allowed = command))

def stringMatches (value expected : String) : Bool :=
  decide (value = expected)

def stringIn (value : String) (values : List String) : Bool :=
  values.any (fun candidate => stringMatches value candidate)

def argStartsWith (arg candidatePrefix : String) : Bool :=
  arg.startsWith candidatePrefix

/-- In-place editing rewrites the input files: GNU `-i`, BSD `-i` and `-I`, in
any option cluster (`-ni`), and `--in-place` or any abbreviation of it. `-e`
takes a script and `-l` and `-f` a value, so the letters after them are not
options. -/
def sedArgumentDenied (arg : String) : Bool :=
  abbreviatesLong arg "in-place"
    || shortClusterDenied arg ['i', 'I'] ['e', 'l', 'f']

def validateSedArgs (args : List String) : Decision :=
  match List.find? sedArgumentDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "sed" arg)
  | none => .allow

def findArgumentDenied (arg : String) : Bool :=
  stringIn arg
    [ "-delete"
    , "-exec"
    , "-execdir"
    , "-ok"
    , "-okdir"
    , "-fprint"
    , "-fprint0"
    , "-fprintf"
    , "-fls"
    ]

def validateFindArgs (args : List String) : Decision :=
  match List.find? findArgumentDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "find" arg)
  | none => .allow

/-- `--pre` and `--hostname-bin` run the named program and `-z` runs a
decompressor for each compressed file, alone or in a cluster. -/
def ripgrepArgumentDenied (arg : String) : Bool :=
  stringMatches arg "--search-zip"
    || shortClusterDenied arg ['z'] ['A', 'B', 'C', 'd', 'e', 'E', 'f', 'g', 'j', 'm', 'M', 'r', 't', 'T']
    || stringMatches arg "--pre"
    || argStartsWith arg "--pre="
    || stringMatches arg "--hostname-bin"
    || argStartsWith arg "--hostname-bin="

def validateRipgrepArgs (args : List String) : Decision :=
  match List.find? ripgrepArgumentDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "rg" arg)
  | none => .allow

def tailscaleSubcommandAllowed (subcommand : String) : Bool :=
  stringIn subcommand ["status", "ip", "netcheck", "version", "ping"]

/-- `tailscale status --web` serves the status page on a listening socket and
opens a browser. -/
def tailscaleArgumentDenied (arg : String) : Bool :=
  match goFlagName? arg with
  | some name => stringIn name ["web", "listen", "browser"]
  | none => false

def validateTailscaleArgs (args : List String) : Decision :=
  match args with
  | [] => .deny (.readOnlySubcommandRequired "tailscale")
  | subcommand :: rest =>
      if tailscaleSubcommandAllowed subcommand then
        match List.find? tailscaleArgumentDenied rest with
        | some arg => .deny (.readOnlyArgumentNotAllowed "tailscale" arg)
        | none => .allow
      else
        .deny (.readOnlySubcommandNotAllowlisted "tailscale" subcommand)

/-- Long options that write a file (`--output`, `--cookie-jar`,
`--dump-header`, `--trace`, `--libcurl`, `--stderr`, `--etag-save`, `--hsts`,
`--alt-svc`, `--ssl-sessions`), send a body or another method, read options from
a file, or widen the protocols the launch fixes. -/
def curlDeniedLongOptions : List String :=
  [ "alt-svc", "config", "cookie-jar", "create-dirs", "data", "data-ascii"
  , "data-binary", "data-raw", "data-urlencode", "dump-header", "etag-save", "form"
  , "form-string", "hsts", "json", "libcurl", "next", "output", "output-dir", "proto"
  , "proto-default", "proto-redir", "remote-header-name", "remote-name"
  , "remote-name-all", "request", "ssl-sessions", "stderr", "trace", "trace-ascii"
  , "upload-file" ]

def curlDeniedShortOptions : List Char :=
  ['c', 'd', 'D', 'F', 'J', 'K', 'o', 'O', 'Q', 'T', 'X', ':']

def curlValuedShortOptions : List Char :=
  ['A', 'b', 'C', 'e', 'E', 'H', 'm', 'P', 'r', 't', 'u', 'U', 'w', 'x', 'y', 'Y', 'z']

/-- A denied long option or any abbreviation of it, a denied letter in any
cluster, and a `--write-out` format naming an output file (`%output{file}`). -/
def curlArgumentDenied (arg : String) : Bool :=
  abbreviatesAnyLong arg curlDeniedLongOptions
    || shortClusterDenied arg curlDeniedShortOptions curlValuedShortOptions
    || hasInfix arg "%output"

def curlHasHttpUrl (arg : String) : Bool :=
  argStartsWith arg "http://" || argStartsWith arg "https://"

def validateCurlArgs (args : List String) : Decision :=
  match List.find? curlArgumentDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "curl" arg)
  | none =>
      if args.any curlHasHttpUrl then
        .allow
      else
        .deny (.readOnlyUrlRequired "curl")

def gitGlobalOptionDenied (arg : String) : Bool :=
  stringIn arg
      [ "-C"
      , "-c"
      , "--config-env"
      , "--exec-path"
      , "--git-dir"
      , "--namespace"
      , "--super-prefix"
      , "--work-tree"
      ]
    || argStartsWith arg "-C"
    || argStartsWith arg "-c"
    || argStartsWith arg "--config-env="
    || argStartsWith arg "--exec-path="
    || argStartsWith arg "--git-dir="
    || argStartsWith arg "--namespace="
    || argStartsWith arg "--super-prefix="
    || argStartsWith arg "--work-tree="

def gitOptionConsumesNext (arg : String) : Bool :=
  stringIn arg
    [ "-C"
    , "-c"
    , "--config-env"
    , "--exec-path"
    , "--git-dir"
    , "--namespace"
    , "--super-prefix"
    , "--work-tree"
    ]

def findGitSubcommand : List String → Option (String × List String)
  | [] => none
  | arg :: rest =>
      if gitOptionConsumesNext arg then
        match rest with
        | [] => none
        | _ :: remaining => findGitSubcommand remaining
      else if stringMatches arg "--" || argStartsWith arg "-" then
        findGitSubcommand rest
      else
        some (arg, rest)

/-- Flags that write a file or run a program: `--output`, an external diff,
a textconv filter (or an abbreviation `git grep` accepts), `--exec`, and a
signature check (`--show-signature`, a `%G` format placeholder), which runs the
configured GPG program. -/
def gitReadOnlyFlagDenied (arg : String) : Bool :=
  stringIn arg ["--output", "--ext-diff", "--exec", "--paginate", "--show-signature"]
    || argStartsWith arg "--output="
    || argStartsWith arg "--exec="
    || abbreviatesLong arg "textconv" 5
    || hasInfix arg "%G"

/-- `git grep -O` opens the matches with the named program, also inside a
cluster or as `--open-files-in-pager` or an abbreviation of it; `git status -v`
renders the staged diff through any configured textconv filter. -/
def gitSubcommandFlagDenied (subcommand arg : String) : Bool :=
  gitReadOnlyFlagDenied arg
    || (stringMatches subcommand "grep"
        && (abbreviatesLong arg "open-files-in-pager"
            || shortClusterDenied arg ['O'] ['A', 'B', 'C', 'e', 'f', 'm']))
    || (stringMatches subcommand "status"
        && (abbreviatesLong arg "verbose" || shortClusterDenied arg ['v'] ['u', 'M']))

def gitBranchArgAllowed (arg : String) : Bool :=
  stringIn arg
    [ "--list"
    , "-l"
    , "--show-current"
    , "-a"
    , "--all"
    , "-r"
    , "--remotes"
    , "-v"
    , "-vv"
    , "--verbose"
    ]
    || argStartsWith arg "--format="

def validateGitBranchArgs (args : List String) : Decision :=
  match List.find? (fun arg => !gitBranchArgAllowed arg) args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "git" arg)
  | none => .allow

def gitReadOnlySubcommandAllowed (subcommand : String) : Bool :=
  stringIn subcommand ["status", "diff", "show", "log", "ls-files", "grep", "rev-parse"]

def validateGitArgs (args : List String) : Decision :=
  match List.find? gitGlobalOptionDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "git" arg)
  | none =>
      match findGitSubcommand args with
      | none => .deny (.readOnlySubcommandRequired "git")
      | some (subcommand, subcommandArgs) =>
          match List.find? (gitSubcommandFlagDenied subcommand) subcommandArgs with
          | some arg => .deny (.readOnlyArgumentNotAllowed "git" arg)
          | none =>
              if gitReadOnlySubcommandAllowed subcommand then
                .allow
              else if stringMatches subcommand "branch" then
                validateGitBranchArgs subcommandArgs
              else
                .deny (.readOnlySubcommandNotAllowlisted "git" subcommand)

/-- `file -C` compiles a magic file into the working directory. -/
def fileArgumentDenied (arg : String) : Bool :=
  abbreviatesLong arg "compile" || shortClusterDenied arg ['C'] ['e', 'f', 'F', 'm', 'P']

def validateFileArgs (args : List String) : Decision :=
  match List.find? fileArgumentDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "file" arg)
  | none => .allow

/-- `lsof -D` builds or updates a device cache file. -/
def validateLsofArgs (args : List String) : Decision :=
  match List.find? (argStartsWith · "-D") args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "lsof" arg)
  | none => .allow

def dateValuedShortOptions : List Char := ['d', 'f', 'r', 'v', 'z']

def dateValuedLongOptions : List String := ["date", "file", "reference"]

def dateTakesNext (arg : String) : Bool :=
  (match shortCluster? arg with
    | some letters => clusterTakesNext dateValuedShortOptions letters
    | none => false)
    || (!hasInfix arg "=" && abbreviatesAnyLong arg dateValuedLongOptions)

/-- Operands, skipping the value an option takes as its next argument. -/
def dateOperands : List String → List String
  | [] => []
  | arg :: rest =>
      if dateTakesNext arg then
        match rest with
        | [] => []
        | _ :: after => dateOperands after
      else if argStartsWith arg "-" then
        dateOperands rest
      else
        arg :: dateOperands rest

/-- `date -s`, `--set` and an operand that is not a `+format` set the clock;
BSD `-j` parses the operand without setting anything. -/
def validateDateArgs (args : List String) : Decision :=
  match List.find? (fun arg => abbreviatesLong arg "set"
      || shortClusterDenied arg ['s'] ('I' :: dateValuedShortOptions)) args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "date" arg)
  | none =>
      if args.contains "-j" then
        .allow
      else
        match List.find? (fun operand => !argStartsWith operand "+") (dateOperands args) with
        | some operand => .deny (.readOnlyArgumentNotAllowed "date" operand)
        | none => .allow

/-- An operand, `-F`/`--file` and `-b`/`--boot` set the host name. -/
def hostnameArgumentDenied (arg : String) : Bool :=
  !argStartsWith arg "-"
    || abbreviatesAnyLong arg ["file", "boot"]
    || shortClusterDenied arg ['F', 'b'] []

def validateHostnameArgs (args : List String) : Decision :=
  match List.find? hostnameArgumentDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "hostname" arg)
  | none => .allow

def validateReadOnlyArgs (request : CommandRequest) : Decision :=
  match request.lookupCommand with
  | "sed" => validateSedArgs request.args
  | "find" => validateFindArgs request.args
  | "git" => validateGitArgs request.args
  | "rg" => validateRipgrepArgs request.args
  | "tailscale" => validateTailscaleArgs request.args
  | "curl" => validateCurlArgs request.args
  | "file" => validateFileArgs request.args
  | "lsof" => validateLsofArgs request.args
  | "date" => validateDateArgs request.args
  | "hostname" => validateHostnameArgs request.args
  | _ => .allow

def validateReadOnlyCommand
    (allowlist : List String)
    (allowedPrefixMatched : Bool)
    (request : CommandRequest) : Decision :=
  if commandAllowlisted request.lookupCommand allowlist || allowedPrefixMatched then
    validateReadOnlyArgs request
  else
    .deny (.readOnlyCommandNotAllowlisted request.lookupCommand)

def readOnlyNetworkDenied (request : CommandRequest) : Bool :=
  if request.lookupCommand = "curl" then
    true
  else if request.lookupCommand = "tailscale" then
    match request.args with
    | "ping" :: _ => true
    | "netcheck" :: _ => true
    | _ => false
  else
    false

def validateNetworkMode
    (mode : ExecutionMode)
    (networkMode : NetworkMode)
    (request : CommandRequest) : Decision :=
  match networkMode with
  | .inherit => .allow
  | .enabled => .allow
  | .disabled =>
      match mode with
      | .workspaceWrite | .artifactWrite => .allow
      | .unrestricted => .deny .disabledNetworkUnenforceable
      | .readOnly =>
          if readOnlyNetworkDenied request then
            .deny (.disabledNetworkCommand request.lookupCommand)
          else
            .allow

def validateAfterPrefixes
    (policy : Policy)
    (allowedPrefixMatched : Bool)
    (request : CommandRequest) : Decision :=
  match validateNetworkMode policy.mode policy.networkMode request with
  | .deny reason => .deny reason
  | .allow =>
      match policy.mode with
      | .readOnly =>
          validateReadOnlyCommand
            policy.readOnlyAllowlist
            allowedPrefixMatched
            request
      | .workspaceWrite | .artifactWrite => .allow
      | .unrestricted => .allow

def validatePolicy (policy : Policy) (request : CommandRequest) : Decision :=
  match firstMatchingPrefix request.argv policy.forbiddenArgvPrefixes with
  | some matched => .deny (.forbiddenPrefix matched)
  | none =>
      match policy.allowedArgvPrefixes with
      | [] => validateAfterPrefixes policy false request
      | _ :: _ =>
          match firstMatchingPrefix request.argv policy.allowedArgvPrefixes with
          | none => .deny (.allowedPrefixRequired request.argv)
          | some _ => validateAfterPrefixes policy true request

end CommandPolicy
