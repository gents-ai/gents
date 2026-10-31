import { describe, expect, it } from "vitest";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { toolSummary } from "../src/ui/screens/tool-summary";

const filed = (args: Record<string, unknown>) =>
  ({
    toolName: "file_mailbox_item",
    statusKind: "success",
    denial: null,
    presentation: {
      kind: "generic",
      summary: null,
      input: JSON.stringify(args),
      output: null,
    },
  }) as unknown as RenderedToolCallView;

describe("the mailbox tool's row", () => {
  it("says the agent asked a question, with its title", () => {
    expect(toolSummary(filed({ kind: "ask", title: "Button colour" }))).toMatchObject({
      primary: "Asked you a question",
      secondary: "Button colour",
    });
  });

  it("says something else was sent to the mailbox", () => {
    expect(
      toolSummary(filed({ kind: "finished", title: "Release done" })),
    ).toMatchObject({
      primary: "Sent to your mailbox",
      secondary: "Release done",
    });
  });
});
