import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
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

  it("shows the configured window the next request runs with, not the last request's", () => {
    render(
      <SessionContext
        context={context({
          lastRequest: {
            contextWindow: 128_000,
            compactionThresholdTokens: 102_400,
          } as Context["lastRequest"],
        })}
      />,
    );
    expect(screen.getByTestId("context-meter")).toHaveTextContent("500k");
    expect(screen.getByTestId("context-meter")).not.toHaveTextContent("128k");
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
    expect(meter).toHaveTextContent("window unavailable");
    expect(meter).not.toHaveTextContent("128k");
  });

  it("shows the latest model input reason and the before-to-after input estimate", () => {
    render(
      <SessionContext
        context={context({
          lastRequest: {
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
            compactionReason: "compacted",
            preCompactionInputTokens: 480,
            components: {
              messages: 400,
              documents: 0,
              toolSchemas: 20,
              additionalParameters: 0,
              outputSchema: 0,
            },
          },
        })}
      />,
    );
    fireEvent.click(screen.getByTestId("context-meter"));

    const dispatch = screen.getByTestId("last-context-dispatch");
    expect(dispatch).toHaveTextContent("Reduced before dispatch");
    expect(dispatch).toHaveTextContent("480 → 420 tokens");
    expect(dispatch).toHaveTextContent("400,000 tokens");
  });

  it("shows durable session compaction estimates and leaves absent evidence unclaimed", () => {
    render(
      <SessionContext
        context={context({
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
});
