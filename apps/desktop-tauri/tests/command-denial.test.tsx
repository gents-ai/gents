import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CommandDenialToolItem } from "../src/components/commandDenial";
import {
  parseCommandDenial,
  type CommandDenialView,
  type DenialRuleId,
} from "../src/lib/commandDenial";
import { MessageList } from "../src/components/Transcript";
import type { RenderedTimelineItem, RenderedToolCallView } from "../src/lib/types";

// ---------------------------------------------------------------------
// Sentinel matrix
//
// Each (rust_bail_text, expected_rule_id) pair below corresponds to a
// `bail!()` site in crates/defra-agent/src/toolset/shared/command.rs.
// When the runtime is enriched to persist structured DenialReason
// (issue #286 follow-up), this regex parser retires — but until then,
// the parser MUST match every command-policy bail string that ships.
// If a bail string is reworded upstream without updating the parser,
// the denial silently falls back to the generic tool-failure render.
// ---------------------------------------------------------------------
const SENTINELS: Array<[string, DenialRuleId]> = [
  // command.rs:219
  [
    "command is forbidden by command execution policy prefix: git commit",
    "forbiddenPrefix",
  ],
  // command.rs:228
  [
    "command is not allowed by command execution policy prefixes: ls /etc",
    "allowedPrefixRequired",
  ],
  // command.rs:290
  [
    "command is not allowed by the read-only bash tool: rm",
    "readOnlyCommandNotAllowlisted",
  ],
  // command.rs:301
  ["sed in-place edits are not allowed", "readOnlyArgumentNotAllowed"],
  // command.rs:319
  [
    "find arguments that can write or execute are not allowed",
    "readOnlyArgumentNotAllowed",
  ],
  // command.rs:392
  [
    "command_network_mode=disabled cannot be enforced for unrestricted bash",
    "disabledNetworkUnenforceable",
  ],
  // command.rs:401
  ["curl is not allowed when command_network_mode=disabled", "disabledNetworkCommand"],
  // command.rs:404
  [
    "tailscale network probes are not allowed when command_network_mode=disabled",
    "disabledNetworkCommand",
  ],
  // command.rs:424
  [
    "macOS sandbox-exec is required for workspace_write bash but was not found",
    "workspaceWriteSandboxUnavailable",
  ],
  // command.rs:426 / :492
  [
    "workspace_write bash requires macOS seatbelt sandbox enforcement on this build",
    "workspaceWriteSandboxUnavailable",
  ],
  // command.rs:529
  [
    "launchctl subcommand is not allowed by the read-only bash tool: bootout",
    "readOnlySubcommandNotAllowlisted",
  ],
  // command.rs:541
  [
    "tailscale subcommand is not allowed by the read-only bash tool: up",
    "readOnlySubcommandNotAllowlisted",
  ],
  // command.rs:586
  [
    "curl argument is not allowed by the read-only bash tool: --data",
    "readOnlyArgumentNotAllowed",
  ],
  // command.rs:591
  [
    "curl requires an http:// or https:// URL in the read-only bash tool",
    "readOnlyUrlRequired",
  ],
  // command.rs:610
  [
    "sudo launchctl must use the absolute /bin/launchctl path",
    "readOnlyArgumentNotAllowed",
  ],
  // command.rs:612
  [
    "sudo command is not allowed by the read-only bash tool: rm",
    "readOnlySubcommandNotAllowlisted",
  ],
  // command.rs:622
  [
    "git global options that redirect config or helper lookup are not allowed",
    "readOnlyArgumentNotAllowed",
  ],
  // command.rs:633
  [
    "git subcommand is not allowed by the read-only bash tool: commit",
    "readOnlySubcommandNotAllowlisted",
  ],
  // command.rs:646
  [
    "rg argument is not allowed by the read-only bash tool: --pre",
    "readOnlyArgumentNotAllowed",
  ],
  // command.rs:714
  [
    "git argument is not allowed by the read-only bash tool: --output",
    "readOnlyArgumentNotAllowed",
  ],
  // command.rs:730
  ["git branch argument is not read-only: -D", "readOnlyArgumentNotAllowed"],
];

describe("parseCommandDenial", () => {
  it.each(SENTINELS)("recognizes %j as %s", (text, ruleId) => {
    const denial = parseCommandDenial(text);
    expect(denial).not.toBeNull();
    expect(denial?.ruleId).toBe(ruleId);
    expect(denial?.diagnostic).toBe(text);
  });

  it("returns null for empty input", () => {
    expect(parseCommandDenial("")).toBeNull();
    expect(parseCommandDenial(null)).toBeNull();
    expect(parseCommandDenial(undefined)).toBeNull();
  });

  it("returns null for non-denial tool errors", () => {
    // Runtime errors (exit code, timeout, MCP transport) should NOT
    // route through the denial render — they are real failures, not
    // policy guardrails.
    expect(parseCommandDenial("tool exited with code 2: file not found")).toBeNull();
    expect(parseCommandDenial("mcp service unreachable: timeout")).toBeNull();
    expect(
      parseCommandDenial("Error: connection refused at localhost:8080"),
    ).toBeNull();
  });

  it("tolerates 'error:' prefix on the persisted string", () => {
    const denial = parseCommandDenial("error: sed in-place edits are not allowed");
    expect(denial?.ruleId).toBe("readOnlyArgumentNotAllowed");
  });

  it("extracts denied_command from the forbidden-prefix sentinel", () => {
    const denial = parseCommandDenial(
      "command is forbidden by command execution policy prefix: git commit",
    );
    expect(denial?.deniedCommand).toBe("git");
  });

  it("extracts denied_subcommand from launchctl sentinel", () => {
    const denial = parseCommandDenial(
      "launchctl subcommand is not allowed by the read-only bash tool: bootout",
    );
    expect(denial?.deniedSubcommand).toBe("bootout");
    expect(denial?.deniedCommand).toBe("launchctl");
  });

  it("extracts denied_argument from curl arg sentinel", () => {
    const denial = parseCommandDenial(
      "curl argument is not allowed by the read-only bash tool: --data",
    );
    expect(denial?.deniedArgument).toBe("--data");
    expect(denial?.deniedCommand).toBe("curl");
  });
});

// ---------------------------------------------------------------------
// Component render — ensures the denial item is reachable from the
// transcript renderer path and carries the expected DOM classes /
// accessibility hooks (rule-id badge, amber dot, denial-attempt
// highlight). Mounts via the public MessageList entry point so the
// integration with Transcript.tsx::ToolGroups is exercised.
// ---------------------------------------------------------------------

function deniedToolView(
  text: string,
  denial?: CommandDenialView,
): RenderedToolCallView {
  return {
    itemKey: "tool-1",
    toolName: "bash_read_only · sed",
    status: "failed",
    statusKind: "error",
    args: null,
    result: { rawText: text, fields: [] },
    denial,
  };
}

describe("CommandDenialToolItem (direct)", () => {
  it("renders the amber dot, rule-id badge, category label", () => {
    const denial = parseCommandDenial("sed in-place edits are not allowed");
    expect(denial).not.toBeNull();
    const { container, getByText } = render(
      <CommandDenialToolItem tool={deniedToolView("ignored")} denial={denial!} />,
    );

    // Amber dot present
    expect(container.querySelector(".tool-item-dot-denied")).not.toBeNull();
    // The wrapping <details> carries the denial class + rule-id data attr
    const details = container.querySelector("details.tool-item-denied");
    expect(details).not.toBeNull();
    expect(details?.getAttribute("data-rule-id")).toBe("readOnlyArgumentNotAllowed");
    // Visible badge text
    expect(getByText("readOnlyArgumentNotAllowed")).toBeTruthy();
    expect(getByText("Read-only guard")).toBeTruthy();
    // The denied-token highlight on the --in-place arg
    expect(container.querySelector(".denied-token")).not.toBeNull();
  });
});

describe("Transcript ToolGroups integration", () => {
  it("routes a denied tool result through the CommandDenial render", () => {
    const items: RenderedTimelineItem[] = [
      {
        kind: "toolGroup",
        itemKey: "group-1",
        tools: [deniedToolView("sed in-place edits are not allowed")],
      },
    ];

    const { container } = render(<MessageList timelineItems={items} />);
    // Denial render took the slot — not the default tool-item.
    expect(container.querySelector(".tool-item-denied")).not.toBeNull();
    // The denial rule-id badge is visible.
    expect(
      container.querySelector("[data-rule-id]")?.getAttribute("data-rule-id"),
    ).toBe("readOnlyArgumentNotAllowed");
  });

  it("prefers structured denial fields over legacy result parsing", () => {
    const structured: CommandDenialView = {
      category: "forbidden-prefix",
      categoryLabel: "Forbidden prefix",
      ruleId: "forbiddenPrefix",
      reasonLine: "argv begins with a forbidden prefix configured on this behavior.",
      deniedCommand: "git",
      diagnostic: "structured diagnostic",
    };
    const items: RenderedTimelineItem[] = [
      {
        kind: "toolGroup",
        itemKey: "group-structured",
        tools: [deniedToolView("exit code 1: generic failure", structured)],
      },
    ];

    const { container } = render(<MessageList timelineItems={items} />);
    expect(container.querySelector(".tool-item-denied")).not.toBeNull();
    expect(
      container.querySelector("[data-rule-id]")?.getAttribute("data-rule-id"),
    ).toBe("forbiddenPrefix");
  });

  it("falls back to the default render when the result isn't a denial", () => {
    const items: RenderedTimelineItem[] = [
      {
        kind: "toolGroup",
        itemKey: "group-2",
        tools: [
          {
            itemKey: "tool-2",
            toolName: "bash_read_only · grep",
            status: "failed",
            statusKind: "error",
            args: null,
            result: { rawText: "exit code 2: file not found", fields: [] },
          },
        ],
      },
    ];

    const { container } = render(<MessageList timelineItems={items} />);
    // No denial routing.
    expect(container.querySelector(".tool-item-denied")).toBeNull();
    // Default error dot is present.
    expect(container.querySelector(".tool-item-dot-error")).not.toBeNull();
  });

  it("does not parse a successful tool's output", () => {
    // Defensive: even if a "success" tool's output happened to contain
    // a denial-looking string, we never route success → denial. The
    // parser only runs on statusKind === "error".
    const items: RenderedTimelineItem[] = [
      {
        kind: "toolGroup",
        itemKey: "group-3",
        tools: [
          {
            itemKey: "tool-3",
            toolName: "bash_read_only · grep",
            status: "completed",
            statusKind: "success",
            args: null,
            result: {
              rawText:
                "matched: sed in-place edits are not allowed (from CHANGELOG.md)",
              fields: [],
            },
          },
        ],
      },
    ];

    const { container } = render(<MessageList timelineItems={items} />);
    expect(container.querySelector(".tool-item-denied")).toBeNull();
    expect(container.querySelector(".tool-item-dot-success")).not.toBeNull();
  });
});
