import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChatTranscriptPanel } from "../src/components/chat";
import { MessageList } from "../src/components/Transcript";
import { copyText } from "../src/lib/clipboard";
import type { DesktopSessionSnapshot } from "../src/lib/types";

describe("copyText", () => {
  afterEach(() => vi.restoreAllMocks());

  it("prefers navigator.clipboard and falls back to execCommand", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    expect(await copyText("hello")).toBe(true);
    expect(writeText).toHaveBeenCalledWith("hello");

    Object.assign(navigator, { clipboard: undefined });
    document.execCommand = vi.fn().mockReturnValue(true);
    expect(await copyText("legacy")).toBe(true);
    expect(document.execCommand).toHaveBeenCalledWith("copy");
  });
});

describe("transcript copy actions", () => {
  afterEach(() => vi.restoreAllMocks());

  it("copies a user message's content", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(
      <MessageList
        timelineItems={[
          {
            kind: "userMessage",
            itemKey: "u1",
            content: "copy me please",
          },
        ]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("copy me please"));
  });

  it("renders a copy button on fenced code blocks", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(
      <MessageList
        timelineItems={[
          {
            kind: "assistantMessage",
            itemKey: "a1",
            content: "```rust\nfn main() {}\n```",
          },
        ]}
      />,
    );

    const buttons = screen.getAllByRole("button", { name: "Copy" });
    // Message copy + code-block copy.
    expect(buttons.length).toBe(2);
    fireEvent.click(buttons[buttons.length - 1]);
    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith(expect.stringContaining("fn main")),
    );
  });
});

describe("error card retry", () => {
  const session: DesktopSessionSnapshot = {
    sessionId: "s1",
    turnState: "failed",
    latestResponse: { status: "failed", errorMessage: "provider exploded" },
    timelineItems: [
      {
        kind: "userMessage",
        itemKey: "u1",
        content: "the failed ask",
      },
    ],
  };

  it("summarizes the error, keeps raw text in a disclosure, and retries the failed content", () => {
    const onRetryMessage = vi.fn();
    render(
      <ChatTranscriptPanel
        selectedSessionId="s1"
        session={session}
        onRetryMessage={onRetryMessage}
      />,
    );

    const card = screen.getByTestId("response-error-card");
    expect(card).toHaveTextContent("couldn't complete this turn");
    expect(card).toHaveTextContent("provider exploded");

    fireEvent.click(screen.getByTestId("retry-turn"));
    expect(onRetryMessage).toHaveBeenCalledWith("the failed ask");
  });

  it("omits Retry when no handler is wired", () => {
    render(<ChatTranscriptPanel selectedSessionId="s1" session={session} />);
    expect(screen.queryByTestId("retry-turn")).not.toBeInTheDocument();
  });
});
