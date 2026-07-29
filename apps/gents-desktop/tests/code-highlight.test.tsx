import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { MessageList } from "@source-inc/gents-desktop-chat";

describe("fenced code highlighting", () => {
  it("highlights fenced code and labels the language", () => {
    const { container } = render(
      <MessageList
        timelineItems={[
          {
            kind: "assistantMessage",
            itemKey: "a1",
            content: '```rust\nfn main() { let x = "hi"; }\n```',
          },
        ]}
      />,
    );

    expect(screen.getByText("rust")).toHaveClass("code-block-language");
    expect(container.querySelector(".hljs-keyword")).not.toBeNull();
    expect(container.querySelector(".hljs-string")).not.toBeNull();
  });

  it("renders unhinted fenced code without crashing", () => {
    const { container } = render(
      <MessageList
        timelineItems={[
          {
            kind: "assistantMessage",
            itemKey: "a2",
            content: "```\nplain text block\n```",
          },
        ]}
      />,
    );
    expect(container.querySelector(".code-block pre")).not.toBeNull();
  });
});
