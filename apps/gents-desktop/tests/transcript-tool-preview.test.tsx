import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { MessageList } from "@source-inc/gents-desktop-chat";
import type {
  RenderedTimelineItem,
  ToolDetailValueView,
} from "@source-inc/gents-desktop-client";

function renderToolArgs(args: ToolDetailValueView) {
  const items: RenderedTimelineItem[] = [
    {
      kind: "toolGroup",
      itemKey: "tools-1",
      messageSequence: 1,
      tools: [
        {
          itemKey: "tool-1",
          toolName: "call_tool",
          statusKind: "success",
          args,
          result: null,
        },
      ],
    },
  ];

  return render(<MessageList timelineItems={items} />).container;
}

describe("Transcript tool argument previews", () => {
  it("uses an allowlisted field instead of the first arbitrary field", () => {
    const container = renderToolArgs({
      rawText: '{"api_key":"sk-supersecret","path":"src/main.rs"}',
      fields: [
        { key: "api_key", value: "sk-supersecret" },
        { key: "path", value: "src/main.rs" },
      ],
    });

    expect(container.querySelector(".tool-item-preview")).toHaveTextContent(
      "src/main.rs",
    );
    expect(container.querySelector(".tool-item-preview")).not.toHaveTextContent(
      "sk-supersecret",
    );
  });

  it("does not fall back to raw arguments when no field is allowlisted", () => {
    const container = renderToolArgs({
      rawText: '{"api_key":"sk-supersecret"}',
      fields: [{ key: "api_key", value: "sk-supersecret" }],
    });

    expect(container.querySelector(".tool-item-preview")).toBeNull();
  });

  it("suppresses an allowlisted field when its value contains credentials", () => {
    const container = renderToolArgs({
      rawText:
        '{"command":"curl -H \'Authorization: Bearer sk-supersecret\' example.com"}',
      fields: [
        {
          key: "command",
          value: "curl -H 'Authorization: Bearer sk-supersecret' example.com",
        },
      ],
    });

    expect(container.querySelector(".tool-item-preview")).toBeNull();
  });
});
