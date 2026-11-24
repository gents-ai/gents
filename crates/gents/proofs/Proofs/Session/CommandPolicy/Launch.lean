import Proofs.Session.CommandPolicy.Validation

namespace CommandPolicy

/-- Global options every read-only `git` runs with: `core.fsmonitor=false` and
`core.hooksPath=/dev/null` stop git starting a configured fsmonitor or hook
program, `log.showSignature=false` stops `log` and `show` running the configured
GPG program, and `--no-optional-locks` stops `status` rewriting the index. -/
def gitReadOnlyGlobalOptions : List String :=
  [ "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"
  , "-c", "log.showSignature=false", "--no-optional-locks" ]

/-- `diff`, `show` and `log` render diffs, so they also run with
`--no-ext-diff` and `--no-textconv`, which stop a configured external diff or
textconv program. -/
def gitDiffSubcommands : List String := ["diff", "show", "log"]

def gitDiffOptions : List String := ["--no-ext-diff", "--no-textconv"]

def insertGitDiffOptions : List String → List String
  | [] => []
  | arg :: rest =>
      if argStartsWith arg "-" then
        arg :: insertGitDiffOptions rest
      else if gitDiffSubcommands.contains arg then
        arg :: gitDiffOptions ++ rest
      else
        arg :: rest

/-- The arguments a validated read-only command runs with. `git` runs with the
options above. `curl` restricts every transfer and redirect to HTTP and HTTPS,
so neither another scheme nor one curl guesses from a host name carries a
request. Where no kernel profile contains the command, `sed` runs with GNU
`--sandbox`, which refuses a script with the `e`, `r`, `R`, `w` or `W` command
or the `e` or `w` flag of `s` before running it; a `sed` without `--sandbox`
refuses the option and runs nothing. External premise: git's, curl's and GNU
sed's documented meaning of these options, which
`read_only_launch_options_stop_configured_programs_and_writes` evidences. -/
def readOnlyLaunchArgs (kernelContained : Bool) (request : CommandRequest) : List String :=
  match request.lookupCommand with
  | "git" => gitReadOnlyGlobalOptions ++ insertGitDiffOptions request.args
  | "curl" => ["--proto", "=http,https", "--proto-redir", "=http,https"] ++ request.args
  | "sed" => if kernelContained then request.args else "--sandbox" :: request.args
  | _ => request.args

theorem uncontained_sed_runs_sandboxed (request : CommandRequest)
    (sed : request.lookupCommand = "sed") :
    readOnlyLaunchArgs false request = "--sandbox" :: request.args := by
  simp [readOnlyLaunchArgs, sed]

end CommandPolicy
