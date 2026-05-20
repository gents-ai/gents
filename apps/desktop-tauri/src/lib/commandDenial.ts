/*
 * Command-policy denial detection from a persisted AgentToolCall.result.
 *
 * FIXME(#329): this module performs sentinel-string parsing of
 * the error messages produced by `bail!()` in
 * crates/defra-agent/src/toolset/shared/command.rs:209-241. The Rust
 * validator does not currently persist structured denial reasons on the
 * AgentToolCall row — the Lean DenialReason taxonomy
 * (crates/defra-agent/proofs/Proofs/CommandPolicy/Types.lean:65-117)
 * exists but isn't materialized in production yet.
 *
 * When the runtime is enriched to persist structured fields
 * (`denial_reason`, `denied_argv`, `denied_command`, `denied_argument`,
 * `denied_subcommand`, `denied_prefix`, `policy_mode`, `policy_network`),
 * this regex-based parser should be retired and replaced with a direct
 * read of those fields from the snapshot. The follow-up issue documents
 * the full Path A scope (DenialReason Rust enum mirroring Lean,
 * ToolError::PolicyDenial variant, validator refactor, schema
 * extension, persistence threading, FailureClass::PolicyDenied).
 *
 * Until then: this module is the bridge between today's stringly-typed
 * persistence and the structured UI the panel-286 prototype proposed.
 *
 * Sentinel coverage matches the bail! messages in command.rs at the time
 * this branch was authored. If a new bail!  string is introduced or an
 * existing one is reworded, the corresponding regex below stops matching
 * and the denial silently falls back to the generic tool-failure render.
 * Tests in tests/command-denial.test.tsx assert the full set.
 */

export type DenialCategory =
  | "read-only-guard"
  | "sandbox-violation"
  | "network-denied"
  | "forbidden-prefix"
  | "allowed-prefix-required"
  | "policy-config";

// Stable rule ids mirror Lean DenialReason.toContract
// (Proofs/CommandPolicy/Types.lean:80-90). When the structured persistence
// lands these will come directly from the row's `denial_reason` field.
export type DenialRuleId =
  | "forbiddenPrefix"
  | "allowedPrefixRequired"
  | "readOnlyCommandNotAllowlisted"
  | "readOnlyArgumentNotAllowed"
  | "readOnlySubcommandRequired"
  | "readOnlySubcommandNotAllowlisted"
  | "readOnlyUrlRequired"
  | "disabledNetworkUnenforceable"
  | "disabledNetworkCommand"
  | "workspaceWriteSandboxUnavailable";

export type CommandDenialView = {
  category: DenialCategory;
  categoryLabel: string;
  ruleId: DenialRuleId;
  /** Free-form sentence describing the denial in operator-readable terms. */
  reasonLine: string;
  /**
   * Best-effort decomposition of the persisted error string. Fields the
   * regex couldn't recover are left undefined.
   */
  deniedCommand?: string;
  deniedArgument?: string;
  deniedSubcommand?: string;
  /** Raw error string from the validator; surfaced under "diagnostic". */
  diagnostic: string;
};

type Rule = {
  pattern: RegExp;
  build: (match: RegExpMatchArray, diagnostic: string) => CommandDenialView;
};

// Order matters: rules with attached parameters (capturing groups) come
// before broader category-level rules so a specific match wins.
const RULES: Rule[] = [
  {
    // command.rs:290 — read-only allowlist
    pattern: /^command is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyCommandNotAllowlisted",
      reasonLine: "The read-only bash tool refuses commands outside its allowlist.",
      deniedCommand: m[1].trim(),
      diagnostic,
    }),
  },
  {
    // command.rs:301 — sed in-place (the canonical Lean case)
    pattern: /^sed in-place edits are not allowed$/,
    build: (_m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyArgumentNotAllowed",
      reasonLine: "sed in-place edits aren't allowed under the read-only tool.",
      deniedCommand: "sed",
      deniedArgument: "--in-place",
      diagnostic,
    }),
  },
  {
    // command.rs:319 — find write/execute args
    pattern: /^find arguments that can write or execute are not allowed$/,
    build: (_m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyArgumentNotAllowed",
      reasonLine: "find arguments that can write or execute aren't allowed.",
      deniedCommand: "find",
      diagnostic,
    }),
  },
  {
    // command.rs:392 — disabled network on unrestricted
    pattern: /^command_network_mode=disabled cannot be enforced for unrestricted bash$/,
    build: (_m, diagnostic) => ({
      category: "network-denied",
      categoryLabel: "Network access denied",
      ruleId: "disabledNetworkUnenforceable",
      reasonLine:
        "Network mode is disabled, but the unrestricted bash tool can't enforce it — failing closed.",
      diagnostic,
    }),
  },
  {
    // command.rs:401 — curl + disabled network
    pattern: /^curl is not allowed when command_network_mode=disabled$/,
    build: (_m, diagnostic) => ({
      category: "network-denied",
      categoryLabel: "Network access denied",
      ruleId: "disabledNetworkCommand",
      reasonLine: "curl is denied because this behavior has network mode disabled.",
      deniedCommand: "curl",
      diagnostic,
    }),
  },
  {
    // command.rs:404 — tailscale ping/netcheck + disabled network
    pattern: /^tailscale network probes are not allowed when command_network_mode=disabled$/,
    build: (_m, diagnostic) => ({
      category: "network-denied",
      categoryLabel: "Network access denied",
      ruleId: "disabledNetworkCommand",
      reasonLine: "tailscale network probes are denied while network mode is disabled.",
      deniedCommand: "tailscale",
      diagnostic,
    }),
  },
  {
    // command.rs:424 — workspace_write sandbox unavailable (macOS sandbox-exec missing)
    pattern: /^macOS sandbox-exec is required for workspace_write bash but was not found$/,
    build: (_m, diagnostic) => ({
      category: "sandbox-violation",
      categoryLabel: "Sandbox violation",
      ruleId: "workspaceWriteSandboxUnavailable",
      reasonLine:
        "workspace_write needs an enforced sandbox; sandbox-exec is missing on this host.",
      diagnostic,
    }),
  },
  {
    // command.rs:426 + :492 — workspace_write needs seatbelt enforcement
    pattern: /^workspace_write bash requires macOS seatbelt sandbox enforcement on this build$/,
    build: (_m, diagnostic) => ({
      category: "sandbox-violation",
      categoryLabel: "Sandbox violation",
      ruleId: "workspaceWriteSandboxUnavailable",
      reasonLine:
        "workspace_write needs macOS seatbelt enforcement; this build can't guarantee it.",
      diagnostic,
    }),
  },
  {
    // command.rs:529 — launchctl subcommand
    pattern: /^launchctl subcommand is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlySubcommandNotAllowlisted",
      reasonLine:
        "launchctl only allows read-only subcommands (list, print, print-disabled, blame).",
      deniedCommand: "launchctl",
      deniedSubcommand: m[1].trim(),
      diagnostic,
    }),
  },
  {
    // command.rs:541 — tailscale subcommand
    pattern: /^tailscale subcommand is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlySubcommandNotAllowlisted",
      reasonLine:
        "tailscale only allows the read-only subcommands (status, ip, netcheck, version, ping).",
      deniedCommand: "tailscale",
      deniedSubcommand: m[1].trim(),
      diagnostic,
    }),
  },
  {
    // command.rs:586 — curl arg
    pattern: /^curl argument is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyArgumentNotAllowed",
      reasonLine:
        "curl is read-only here — write/upload/output arguments aren't allowed.",
      deniedCommand: "curl",
      deniedArgument: m[1].trim(),
      diagnostic,
    }),
  },
  {
    // command.rs:591 — curl missing http(s) URL
    pattern: /^curl requires an http:\/\/ or https:\/\/ URL in the read-only bash tool$/,
    build: (_m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyUrlRequired",
      reasonLine:
        "curl needs an explicit http:// or https:// URL under the read-only tool.",
      deniedCommand: "curl",
      diagnostic,
    }),
  },
  {
    // command.rs:610 — sudo path mismatch for launchctl
    pattern: /^sudo launchctl must use the absolute \/bin\/launchctl path$/,
    build: (_m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyArgumentNotAllowed",
      reasonLine: "sudo launchctl must use the absolute /bin/launchctl path.",
      deniedCommand: "sudo",
      diagnostic,
    }),
  },
  {
    // command.rs:612 — sudo subcommand other than launchctl
    pattern: /^sudo command is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlySubcommandNotAllowlisted",
      reasonLine: "sudo only proxies a narrow set of read-only commands.",
      deniedCommand: "sudo",
      deniedSubcommand: m[1].trim(),
      diagnostic,
    }),
  },
  {
    // command.rs:622 — git global options
    pattern: /^git global options that redirect config or helper lookup are not allowed$/,
    build: (_m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyArgumentNotAllowed",
      reasonLine:
        "git global options that redirect config or helper lookup are blocked under read-only.",
      deniedCommand: "git",
      diagnostic,
    }),
  },
  {
    // command.rs:633 — git subcommand
    pattern: /^git subcommand is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlySubcommandNotAllowlisted",
      reasonLine:
        "git only allows read-only subcommands (status, diff, show, log, ls-files, grep, rev-parse, branch).",
      deniedCommand: "git",
      deniedSubcommand: m[1].trim(),
      diagnostic,
    }),
  },
  {
    // command.rs:646 — rg arg
    pattern: /^rg argument is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyArgumentNotAllowed",
      reasonLine: "ripgrep args that shell out or rewrite source aren't allowed.",
      deniedCommand: "rg",
      deniedArgument: m[1].trim(),
      diagnostic,
    }),
  },
  {
    // command.rs:714 — git subcommand arg
    pattern: /^git argument is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyArgumentNotAllowed",
      reasonLine:
        "git argument is blocked — read-only git rejects flags that can write or exec.",
      deniedCommand: "git",
      deniedArgument: m[1].trim(),
      diagnostic,
    }),
  },
  {
    // command.rs:730 — git branch arg
    pattern: /^git branch argument is not read-only: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyArgumentNotAllowed",
      reasonLine:
        "git branch only accepts read-only flags (--list, --show-current, -a, -r, -v, --format=...).",
      deniedCommand: "git",
      deniedArgument: m[1].trim(),
      diagnostic,
    }),
  },
  {
    // command.rs:219 — forbidden prefix. Format: "...prefix: <argv>"
    pattern: /^command is forbidden by command execution policy prefix: (.+)$/,
    build: (m, diagnostic) => ({
      category: "forbidden-prefix",
      categoryLabel: "Forbidden prefix",
      ruleId: "forbiddenPrefix",
      reasonLine:
        "argv begins with a forbidden prefix configured on this behavior.",
      deniedCommand: m[1].trim().split(/\s+/)[0],
      diagnostic,
    }),
  },
  {
    // command.rs:228 — allowed prefix required. Format: "...prefixes: <argv>"
    pattern: /^command is not allowed by command execution policy prefixes: (.+)$/,
    build: (m, diagnostic) => ({
      category: "allowed-prefix-required",
      categoryLabel: "Allowed prefix required",
      ruleId: "allowedPrefixRequired",
      reasonLine:
        "Policy requires argv to match one of the configured allowed prefixes; this argv matches none.",
      deniedCommand: m[1].trim().split(/\s+/)[0],
      diagnostic,
    }),
  },
];

/**
 * Parse a denial from a tool-call error string.
 *
 * Returns null when the input doesn't match a known command-policy
 * sentinel — including the empty / null case, runtime errors, MCP
 * failures, and arbitrary tool output. Null means "render as a normal
 * tool failure"; the caller never panics on this returning null.
 */
export function parseCommandDenial(
  raw: string | null | undefined,
): CommandDenialView | null {
  if (!raw) {
    return null;
  }
  // Tool outputs sometimes wrap the error in framing — strip leading
  // "error: " / "Error: " prefixes if present so the regex anchors hit.
  const stripped = raw.replace(/^(?:error|Error|ERROR):\s*/, "").trim();
  for (const rule of RULES) {
    const match = stripped.match(rule.pattern);
    if (match) {
      return rule.build(match, raw);
    }
  }
  return null;
}

/** All rule patterns, for tests / drift detection. */
export const COMMAND_DENIAL_RULES: ReadonlyArray<RegExp> = RULES.map(
  (r) => r.pattern,
);
