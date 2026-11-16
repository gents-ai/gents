import { render } from "@testing-library/react";
import { CircleHelp, Inbox } from "lucide-react";
import type { ReactElement } from "react";
import { describe, expect, it } from "vitest";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { ToolIcon } from "../src/ui/screens/tool-icon";

const filing = (input: object) =>
  ({
    toolName: "file_mailbox_item",
    presentation: { kind: "generic", input: JSON.stringify(input), output: null },
  }) as unknown as RenderedToolCallView;

const drawn = (node: ReactElement) => render(node).container.innerHTML;

describe("the mailbox tool's icon", () => {
  it("is a question when it asks one, and the mailbox otherwise", () => {
    expect(drawn(<ToolIcon tool={filing({ question: { prompt: "Which?" } })} />)).toBe(
      drawn(<CircleHelp className="size-4" />),
    );
    expect(drawn(<ToolIcon tool={filing({ kind: "notice", title: "Done" })} />)).toBe(
      drawn(<Inbox className="size-4" />),
    );
  });
});
