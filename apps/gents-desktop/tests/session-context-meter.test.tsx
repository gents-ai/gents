import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { SessionContext } from "../src/ui/screens/SessionScreen";

type Context = DesktopSessionSnapshot["context"];

const context = (overrides: Partial<Context> = {}): Context => ({
  estimatedDurableTokens: 0,
  estimatedConversationTokens: 1000,
  contextWindow: 500_000,
  compactionThreshold: 0.8,
  compactionThresholdTokens: 400_000,
  durableMessageCount: 1,
  providerMessageCount: 1,
  totalCompactedMessages: 0,
  compactions: [],
  lastRequest: null,
  ...overrides,
});

describe("session context meter (#1618)", () => {
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
});
