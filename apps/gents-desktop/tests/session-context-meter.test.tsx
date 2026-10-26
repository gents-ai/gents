import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { createContextDetailsCache } from "../src/hooks/sessionContextDetails";
import { SessionContext } from "../src/ui/screens/SessionContextMeter";

type Context = DesktopSessionSnapshot["context"];

const context = (overrides: Partial<Context> = {}): Context => ({
  estimatedDurableTokens: 0,
  estimatedConversationTokens: 1000,
  contextWindow: 500_000,
  compactionThreshold: 0.8,
  compactionThresholdTokens: 400_000,
  compactionStrategy: "StripThenSummarize",
  durableMessageCount: 1,
  providerMessageCount: 1,
  totalCompactedMessages: 0,
  sessionCompactionCount: null,
  requestReductionHistoryLoaded: false,
  compactions: [],
  lastRequest: null,
  ...overrides,
});

const lastRequest = (
  overrides: Partial<NonNullable<Context["lastRequest"]>> = {},
): NonNullable<Context["lastRequest"]> => ({
  requestId: "request-1",
  callId: "call-1",
  callSequence: 1,
  turnIndex: 2,
  attempt: 0,
  estimator: "wire-estimate",
  estimatedInputTokens: 420,
  contextWindow: 500_000,
  compactionThresholdTokens: 400_000,
  configuredMaxOutputTokens: null,
  effectiveMaxOutputTokens: null,
  compactionReason: "below_threshold",
  preCompactionInputTokens: null,
  components: {
    messages: 400,
    documents: 0,
    toolSchemas: 20,
    additionalParameters: 0,
    outputSchema: 0,
  },
  ...overrides,
});

describe("session context meter (#1618)", () => {
  it("does not report an unavailable compaction count as zero", () => {
    render(<SessionContext context={context()} />);
    expect(screen.getByTestId("context-meter")).toHaveTextContent(
      "compactions unavailable",
    );
  });

  it("shows a queried zero session compaction count", () => {
    render(<SessionContext context={context({ sessionCompactionCount: 0 })} />);
    expect(screen.getByTestId("context-meter")).toHaveTextContent("0 compactions");
  });

  it("keeps compact compaction status short and accessible", () => {
    render(<SessionContext context={context()} compact />);
    const meter = screen.getByTestId("context-meter-compact");
    expect(meter).toHaveTextContent("—");
    expect(meter).toHaveAccessibleName(/Session compactions unavailable/);
  });

  it("pairs the latest input with its recorded window and shows changed settings separately", () => {
    render(
      <SessionContext
        context={context({
          lastRequest: lastRequest({
            contextWindow: 128_000,
            compactionThresholdTokens: 102_400,
          }),
        })}
      />,
    );
    expect(screen.getByTestId("context-meter")).toHaveTextContent("0.4%");
    expect(screen.getByTestId("context-meter")).not.toHaveTextContent("500k");
    fireEvent.click(screen.getByTestId("context-meter"));
    expect(screen.getByTestId("next-context-settings")).toHaveTextContent(
      "500,000 tokens",
    );
    expect(screen.getByTestId("next-context-settings")).toHaveTextContent(
      "400,000 tokens",
    );
    expect(screen.getByTestId("context-details")).toHaveTextContent(
      "420 / 102,400 estimated tokens",
    );
  });

  it("shows input as a share of the compaction threshold and omits history estimates", () => {
    render(
      <SessionContext
        context={context({
          estimatedConversationTokens: 6_047,
          estimatedDurableTokens: 13_983,
          contextWindow: 1_048_576,
          compactionThresholdTokens: 786_432,
          sessionCompactionCount: 0,
          lastRequest: lastRequest({
            estimatedInputTokens: 78_520,
            contextWindow: 1_048_576,
            compactionThresholdTokens: 786_432,
            components: {
              messages: 63_736,
              toolSchemas: 14_733,
              additionalParameters: 51,
              documents: 0,
              outputSchema: 0,
            },
          }),
        })}
      />,
    );
    expect(screen.getByTestId("context-meter")).toHaveTextContent("10%");
    expect(screen.getByTestId("context-ring")).toHaveAttribute("data-share", "0.10");
    fireEvent.click(screen.getByTestId("context-meter"));
    const details = screen.getByTestId("context-details");
    expect(details).toHaveTextContent("78,520 / 786,432 estimated tokens");
    expect(details).toHaveTextContent("10%of compaction threshold");
    expect(details).toHaveTextContent("No compaction needed for this model call");
    expect(details).toHaveTextContent("Messages & instructions63,736");
    expect(details).toHaveTextContent("Tool definitions14,733");
    expect(details).toHaveTextContent("Request settings51");
    expect(details).not.toHaveTextContent("6,047");
    expect(details).not.toHaveTextContent("13,983");
    expect(screen.queryByTestId("next-context-settings")).not.toBeInTheDocument();
  });

  it("preserves recorded input when the next call's configured window is invalid", () => {
    render(
      <SessionContext
        context={context({
          contextWindowError: "invalid window",
          lastRequest: lastRequest(),
        })}
      />,
    );
    expect(screen.getByTestId("context-meter")).toHaveTextContent("0.1%");
    fireEvent.click(screen.getByTestId("context-meter"));
    expect(screen.getByRole("alert")).toHaveTextContent("invalid window");
  });

  it("shows separate token shares for instructions, user messages, assistant text and tool activity", async () => {
    render(
      <SessionContext
        loadDetails={async () => ({
          systemPromptTokens: 300,
          messageTokens: 480,
          userInputTokens: 100,
          assistantTokens: 50,
          toolCallTokens: 80,
          toolResultTokens: 240,
          otherMessageTokens: 10,
          messageCount: 12,
          toolDefinitionsCount: 4,
          toolCallCount: 3,
        })}
        context={context({
          lastRequest: lastRequest({
            estimatedInputTokens: 1_000,
            compactionThresholdTokens: 2_000,
            components: {
              messages: 780,
              toolSchemas: 200,
              documents: 0,
              additionalParameters: 20,
              outputSchema: 0,
            },
          }),
        })}
      />,
    );
    expect(screen.getByTestId("context-meter")).toHaveTextContent("50%");
    fireEvent.click(screen.getByTestId("context-meter"));
    await screen.findByRole("row", { name: "User messages 100 10%" });
    const table = screen.getByRole("table", { name: "Input token estimates" });
    for (const [label, tokens, share] of [
      ["Instructions & context", "300", "30%"],
      ["User messages", "100", "10%"],
      ["Assistant messages", "50", "5%"],
      ["Tool calls", "80", "8%"],
      ["Tool results", "240", "24%"],
      ["Other message data", "10", "1%"],
      ["Tool definitions", "200", "20%"],
      ["Request settings", "20", "2%"],
    ]) {
      const row = within(table).getByRole("row", {
        name: `${label} ${tokens} ${share}`,
      });
      expect(
        within(row)
          .getAllByRole("cell")
          .map((cell) => cell.textContent),
      ).toEqual([tokens, share]);
    }
    expect(screen.queryByText("Messages & instructions")).not.toBeInTheDocument();
  });

  it("does not cap the numeric percentage when an input exceeds the threshold", () => {
    render(
      <SessionContext
        context={context({
          lastRequest: lastRequest({
            estimatedInputTokens: 500_000,
          }),
        })}
      />,
    );
    expect(screen.getByTestId("context-meter")).toHaveTextContent("125%");
    expect(screen.getByTestId("context-ring")).toHaveAttribute("data-share", "1.00");
  });

  it("identifies partial history and does not claim it is a full model input", () => {
    render(<SessionContext context={context({ transcriptTotalsExact: false })} />);
    fireEvent.click(screen.getByTestId("context-meter"));
    const details = screen.getByTestId("context-details");
    expect(details).toHaveTextContent("Model-input estimate unavailable");
    expect(details).not.toHaveTextContent("1,000");
    expect(screen.getByTestId("context-meter")).toHaveTextContent("—");
    expect(screen.queryByTestId("last-context-dispatch")).not.toBeInTheDocument();
  });

  it("reports a window the runtime rejects instead of presenting it as in use", () => {
    render(
      <SessionContext
        context={context({
          contextWindow: 128_000,
          contextWindowError:
            "profile p context window 900000 exceeds model m advertised maximum 872000",
        })}
      />,
    );
    const meter = screen.getByTestId("context-meter");
    expect(meter).toHaveTextContent("—");
    fireEvent.click(meter);
    expect(screen.getByRole("alert")).toHaveTextContent("exceeds model");
    expect(meter).not.toHaveTextContent("128k");
  });

  it("shows the latest model input reason and the before-to-after input estimate", () => {
    render(
      <SessionContext
        context={context({
          lastRequest: lastRequest({
            compactionReason: "compacted",
            preCompactionInputTokens: 480,
          }),
        })}
      />,
    );
    fireEvent.click(screen.getByTestId("context-meter"));

    const dispatch = screen.getByTestId("last-context-dispatch");
    expect(dispatch).toHaveTextContent("Input compacted before this model call");
    expect(dispatch).toHaveTextContent("480 → 420 tokens");
    expect(screen.getByTestId("context-details")).toHaveTextContent(
      "420 / 400,000 estimated tokens",
    );
  });

  it("shows durable session compaction estimates and leaves absent evidence unclaimed", () => {
    render(
      <SessionContext
        context={context({
          sessionCompactionCount: 1,
          compactions: [
            {
              compactionKey: "session:1",
              sequence: 1,
              messagesCompacted: 12,
              originalTokens: 900,
              compactedTokens: 300,
              createdAt: null,
            },
          ],
        })}
      />,
    );
    fireEvent.click(screen.getByTestId("context-meter"));

    const compactions = screen.getByTestId("session-compactions");
    expect(compactions).toHaveTextContent("Session compactions");
    expect(compactions).toHaveTextContent("12 messages");
    expect(compactions).toHaveTextContent("900 → 300");
    expect(screen.getByTestId("context-meter")).toHaveTextContent("1 compaction");
    expect(screen.queryByTestId("last-context-dispatch")).not.toBeInTheDocument();
  });
  it("loads only on expansion, caches the exact call, and ignores stale responses", async () => {
    const details = {
      systemPromptTokens: 0,
      messageTokens: 400,
      userInputTokens: 400,
      assistantTokens: 0,
      toolCallTokens: 0,
      toolResultTokens: 0,
      otherMessageTokens: 0,
      messageCount: 1,
      toolDefinitionsCount: 1,
      toolCallCount: 0,
    };
    let finish!: (value: typeof details) => void;
    const fetch = vi
      .fn()
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            finish = resolve;
          }),
      )
      .mockResolvedValue(details);
    const cache = createContextDetailsCache(fetch);
    const load = (input: NonNullable<Context["lastRequest"]>) =>
      cache({
        nodeDid: "node",
        sessionId: "session",
        requestId: input.requestId,
        callId: input.callId,
      });
    const view = render(
      <SessionContext
        context={context({ lastRequest: lastRequest() })}
        loadDetails={load}
      />,
    );
    expect(fetch).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("context-meter"));
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
    view.rerender(
      <SessionContext
        context={context({ lastRequest: lastRequest({ callId: "next" }) })}
        loadDetails={load}
      />,
    );
    await screen.findByRole("row", { name: /User messages 400/ });
    expect(fetch).toHaveBeenCalledTimes(2);
    await act(async () => {
      finish({ ...details, userInputTokens: 999 });
    });
    expect(screen.queryByText("999")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close context details" }));
    fireEvent.click(screen.getByTestId("context-meter"));
    await waitFor(() =>
      expect(
        screen.getByRole("row", { name: /User messages 400/ }),
      ).toBeInTheDocument(),
    );
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it("deduplicates simultaneous detail reads and retries missing or failed captures", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(null)
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue({ userInputTokens: 1 });
    const cache = createContextDetailsCache(fetch);
    const request = {
      nodeDid: "node",
      sessionId: "session",
      requestId: "request",
      callId: "call",
    };
    const first = cache(request);
    expect(cache({ ...request })).toBe(first);
    await expect(first).resolves.toBeNull();
    await expect(cache(request)).rejects.toThrow("offline");
    await expect(cache(request)).resolves.toEqual({ userInputTokens: 1 });
    await cache(request);
    expect(fetch).toHaveBeenCalledTimes(3);
    await cache({ ...request, nodeDid: "other-node" });
    expect(fetch).toHaveBeenCalledTimes(4);
  });
});
