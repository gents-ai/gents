import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CommandDenialToolItem } from "@source-inc/gents-desktop-chat";
import {
  parseCommandDenial,
  type CommandDenialView,
  type DenialRuleId,
} from "@source-inc/gents-desktop-client";
import { MessageList } from "@source-inc/gents-desktop-chat";
import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";

// ---------------------------------------------------------------------
// Sentinel matrix
//
// Each (rust_bail_text, expected_rule_id) pair below corresponds to a
// `bail!()` site in crates/gents/src/toolset/shared/command.rs.
// When the runtime is enriched to persist structured DenialReason
// (issue #286 follow-up), this regex parser retires — but until then,
// the parser MUST match every command-policy bail string that ships.
// If a bail string is reworded upstream without updating the parser,
// the denial silently falls back to the generic tool-failure render.
// ---------------------------------------------------------------------
const SENTINELS: Array<[string, DenialRuleId]> = [
  [
    "command is forbidden by command execution policy prefix: git commit",
    "forbiddenPrefix",
  ],
  [
    "command is not allowed by command execution policy prefixes: ls /etc",
    "allowedPrefixRequired",
  ],
  [
    "command is not allowed by the read-only bash tool: rm",
    "readOnlyCommandNotAllowlisted",
  ],
  ["sed in-place edits are not allowed", "readOnlyArgumentNotAllowed"],
  [
    "find arguments that can write or execute are not allowed",
    "readOnlyArgumentNotAllowed",
  ],
  [
    "command_network_mode=disabled cannot be enforced for unrestricted bash",
    "disabledNetworkUnenforceable",
  ],
  ["curl is not allowed when command_network_mode=disabled", "disabledNetworkCommand"],
  [
    "tailscale network probes are not allowed when command_network_mode=disabled",
    "disabledNetworkCommand",
  ],
  [
    "macOS sandbox-exec is required for workspace_write bash but was not found",
    "workspaceWriteSandboxUnavailable",
  ],
  [
    "workspace_write bash requires macOS seatbelt sandbox enforcement on this build",
    "workspaceWriteSandboxUnavailable",
  ],
  [
    "launchctl subcommand is not allowed by the read-only bash tool: bootout",
    "readOnlySubcommandNotAllowlisted",
  ],
  [
    "tailscale subcommand is not allowed by the read-only bash tool: up",
    "readOnlySubcommandNotAllowlisted",
  ],
  [
    "curl argument is not allowed by the read-only bash tool: --data",
    "readOnlyArgumentNotAllowed",
  ],
  [
    "curl requires an http:// or https:// URL in the read-only bash tool",
    "readOnlyUrlRequired",
  ],
  [
    "sudo launchctl must use the absolute /bin/launchctl path",
    "readOnlyArgumentNotAllowed",
  ],
  [
    "sudo command is not allowed by the read-only bash tool: rm",
    "readOnlySubcommandNotAllowlisted",
  ],
  [
    "git global options that redirect config or helper lookup are not allowed",
    "readOnlyArgumentNotAllowed",
  ],
  [
    "git subcommand is not allowed by the read-only bash tool: commit",
    "readOnlySubcommandNotAllowlisted",
  ],
  [
    "rg argument is not allowed by the read-only bash tool: --pre",
    "readOnlyArgumentNotAllowed",
  ],
  [
    "git argument is not allowed by the read-only bash tool: --output",
    "readOnlyArgumentNotAllowed",
  ],
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

    expect(container.querySelector(".tool-item-dot-denied")).not.toBeNull();
    const details = container.querySelector("details.tool-item-denied");
    expect(details).not.toBeNull();
    expect(details?.getAttribute("data-rule-id")).toBe("readOnlyArgumentNotAllowed");
    expect(getByText("readOnlyArgumentNotAllowed")).toBeTruthy();
    expect(getByText("Read-only guard")).toBeTruthy();
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
    expect(container.querySelector(".tool-item-denied")).not.toBeNull();
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
    expect(container.querySelector(".tool-item-denied")).toBeNull();
    expect(container.querySelector(".tool-item-dot-error")).not.toBeNull();
  });

  it("does not parse a successful tool's output", () => {
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
