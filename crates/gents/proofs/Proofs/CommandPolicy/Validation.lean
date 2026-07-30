import Proofs.CommandPolicy.Types

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

def firstArgWhere (p : String → Bool) : List String → Option String
  | [] => none
  | arg :: rest =>
      if p arg then
        some arg
      else
        firstArgWhere p rest

def anyArgWhere (p : String → Bool) (args : List String) : Bool :=
  (firstArgWhere p args).isSome

def argStartsWith (arg candidatePrefix : String) : Bool :=
  arg.startsWith candidatePrefix

def sedArgumentDenied (arg : String) : Bool :=
  stringMatches arg "--in-place"
    || argStartsWith arg "--in-place="
    || argStartsWith arg "-i"

def validateSedArgs (args : List String) : Decision :=
  match firstArgWhere sedArgumentDenied args with
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
  match firstArgWhere findArgumentDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "find" arg)
  | none => .allow

def ripgrepArgumentDenied (arg : String) : Bool :=
  stringIn arg ["--search-zip", "-z"]
    || stringMatches arg "--pre"
    || argStartsWith arg "--pre="
    || stringMatches arg "--hostname-bin"
    || argStartsWith arg "--hostname-bin="

def validateRipgrepArgs (args : List String) : Decision :=
  match firstArgWhere ripgrepArgumentDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "rg" arg)
  | none => .allow

def launchctlSubcommandAllowed (subcommand : String) : Bool :=
  stringIn subcommand ["list", "print", "print-disabled", "blame"]

def validateLaunchctlArgs (args : List String) : Decision :=
  match args with
  | [] => .deny (.readOnlySubcommandRequired "launchctl")
  | subcommand :: _ =>
      if launchctlSubcommandAllowed subcommand then
        .allow
      else
        .deny (.readOnlySubcommandNotAllowlisted "launchctl" subcommand)

def tailscaleSubcommandAllowed (subcommand : String) : Bool :=
  stringIn subcommand ["status", "ip", "netcheck", "version", "ping"]

def validateTailscaleArgs (args : List String) : Decision :=
  match args with
  | [] => .deny (.readOnlySubcommandRequired "tailscale")
  | subcommand :: _ =>
      if tailscaleSubcommandAllowed subcommand then
        .allow
      else
        .deny (.readOnlySubcommandNotAllowlisted "tailscale" subcommand)

def curlArgumentDenied (arg : String) : Bool :=
  stringIn arg
      [ "-d"
      , "--data"
      , "--data-raw"
      , "--data-binary"
      , "--data-urlencode"
      , "-F"
      , "--form"
      , "-T"
      , "--upload-file"
      , "-X"
      , "--request"
      , "-o"
      , "--output"
      , "-O"
      , "--remote-name"
      , "--remote-header-name"
      , "-K"
      , "--config"
      , "--next"
      ]
    || argStartsWith arg "-d"
    || argStartsWith arg "--data="
    || argStartsWith arg "-F"
    || argStartsWith arg "--form="
    || argStartsWith arg "-T"
    || argStartsWith arg "--upload-file="
    || argStartsWith arg "-X"
    || argStartsWith arg "--request="
    || argStartsWith arg "-o"
    || argStartsWith arg "--output="
    || argStartsWith arg "-O"
    || argStartsWith arg "-K"
    || argStartsWith arg "--config="

def curlHasHttpUrl (arg : String) : Bool :=
  argStartsWith arg "http://" || argStartsWith arg "https://"

def validateCurlArgs (args : List String) : Decision :=
  match firstArgWhere curlArgumentDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "curl" arg)
  | none =>
      if anyArgWhere curlHasHttpUrl args then
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

def gitReadOnlyFlagDenied (arg : String) : Bool :=
  stringIn arg ["--output", "--ext-diff", "--textconv", "--exec", "--paginate"]
    || argStartsWith arg "--output="
    || argStartsWith arg "--exec="

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
  match firstArgWhere (fun arg => !gitBranchArgAllowed arg) args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "git" arg)
  | none => .allow

def gitReadOnlySubcommandAllowed (subcommand : String) : Bool :=
  stringIn subcommand ["status", "diff", "show", "log", "ls-files", "grep", "rev-parse"]

def validateGitArgs (args : List String) : Decision :=
  match firstArgWhere gitGlobalOptionDenied args with
  | some arg => .deny (.readOnlyArgumentNotAllowed "git" arg)
  | none =>
      match findGitSubcommand args with
      | none => .deny (.readOnlySubcommandRequired "git")
      | some (subcommand, subcommandArgs) =>
          match firstArgWhere gitReadOnlyFlagDenied subcommandArgs with
          | some arg => .deny (.readOnlyArgumentNotAllowed "git" arg)
          | none =>
              if gitReadOnlySubcommandAllowed subcommand then
                .allow
              else if stringMatches subcommand "branch" then
                validateGitBranchArgs subcommandArgs
              else
                .deny (.readOnlySubcommandNotAllowlisted "git" subcommand)

def sudoCommandName (command : String) : String :=
  if stringMatches command "/bin/launchctl"
      || stringMatches command "/usr/bin/launchctl"
      || stringMatches command "/sbin/launchctl" then
    "launchctl"
  else
    command

def validateSudoArgs (args : List String) : Decision :=
  match args with
  | [] => .deny (.readOnlySubcommandRequired "sudo")
  | command :: rest =>
      let commandName := sudoCommandName command
      if stringMatches commandName "launchctl" then
        if stringMatches command "/bin/launchctl" then
          validateLaunchctlArgs rest
        else
          .deny (.readOnlyArgumentNotAllowed "sudo" command)
      else
        .deny (.readOnlySubcommandNotAllowlisted "sudo" commandName)

def validateReadOnlyArgs (request : CommandRequest) : Decision :=
  match request.lookupCommand with
  | "sed" => validateSedArgs request.args
  | "find" => validateFindArgs request.args
  | "git" => validateGitArgs request.args
  | "rg" => validateRipgrepArgs request.args
  | "launchctl" => validateLaunchctlArgs request.args
  | "tailscale" => validateTailscaleArgs request.args
  | "curl" => validateCurlArgs request.args
  | "sudo" => validateSudoArgs request.args
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
      | .workspaceWrite => .allow
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
      | .workspaceWrite => .allow
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
