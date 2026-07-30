import type { CommandDenialView as GeneratedCommandDenialView } from "./generated/CommandDenialView.js";


export type DenialCategory =
  | "read-only-guard"
  | "sandbox-violation"
  | "network-denied"
  | "forbidden-prefix"
  | "allowed-prefix-required"
  | "policy-config";

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

export type CommandDenialView = GeneratedCommandDenialView;

type Rule = {
  pattern: RegExp;
  build: (match: RegExpMatchArray, diagnostic: string) => CommandDenialView;
};

const RULES: Rule[] = [
  {
    pattern: /^command is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyCommandNotAllowlisted",
      reasonLine:
        "The read-only bash tool refuses commands outside its allowlist.",
      deniedCommand: m[1].trim(),
      diagnostic,
    }),
  },
  {
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
    pattern:
      /^command_network_mode=disabled cannot be enforced for unrestricted bash$/,
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
    pattern: /^curl is not allowed when command_network_mode=disabled$/,
    build: (_m, diagnostic) => ({
      category: "network-denied",
      categoryLabel: "Network access denied",
      ruleId: "disabledNetworkCommand",
      reasonLine:
        "curl is denied because this behavior has network mode disabled.",
      deniedCommand: "curl",
      diagnostic,
    }),
  },
  {
    pattern:
      /^tailscale network probes are not allowed when command_network_mode=disabled$/,
    build: (_m, diagnostic) => ({
      category: "network-denied",
      categoryLabel: "Network access denied",
      ruleId: "disabledNetworkCommand",
      reasonLine:
        "tailscale network probes are denied while network mode is disabled.",
      deniedCommand: "tailscale",
      diagnostic,
    }),
  },
  {
    pattern:
      /^macOS sandbox-exec is required for workspace_write bash but was not found$/,
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
    pattern:
      /^workspace_write bash requires macOS seatbelt sandbox enforcement on this build$/,
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
    pattern:
      /^launchctl subcommand is not allowed by the read-only bash tool: (.+)$/,
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
    pattern:
      /^tailscale subcommand is not allowed by the read-only bash tool: (.+)$/,
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
    pattern:
      /^curl requires an http:\/\/ or https:\/\/ URL in the read-only bash tool$/,
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
    pattern:
      /^git global options that redirect config or helper lookup are not allowed$/,
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
    pattern: /^rg argument is not allowed by the read-only bash tool: (.+)$/,
    build: (m, diagnostic) => ({
      category: "read-only-guard",
      categoryLabel: "Read-only guard",
      ruleId: "readOnlyArgumentNotAllowed",
      reasonLine:
        "ripgrep args that shell out or rewrite source aren't allowed.",
      deniedCommand: "rg",
      deniedArgument: m[1].trim(),
      diagnostic,
    }),
  },
  {
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
    pattern:
      /^command is not allowed by command execution policy prefixes: (.+)$/,
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

export function parseCommandDenial(
  raw: string | null | undefined,
): CommandDenialView | null {
  if (!raw) {
    return null;
  }
  const stripped = raw.replace(/^(?:error|Error|ERROR):\s*/, "").trim();
  for (const rule of RULES) {
    const match = stripped.match(rule.pattern);
    if (match) {
      return rule.build(match, raw);
    }
  }
  return null;
}

export const COMMAND_DENIAL_RULES: ReadonlyArray<RegExp> = RULES.map(
  (r) => r.pattern,
);
