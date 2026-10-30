import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
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
    fireEvent.click(screen.getByTestId("context-meter"));
    expect(screen.getByTestId("context-footer")).not.toHaveTextContent(/compaction/);
  });

  it("shows a queried zero session compaction count", () => {
    render(<SessionContext context={context({ sessionCompactionCount: 0 })} />);
    expect(screen.getByTestId("context-meter")).toHaveTextContent("0 compactions");
    fireEvent.click(screen.getByTestId("context-meter"));
    expect(screen.getByTestId("context-footer")).toHaveTextContent(
      "500k window · 0 compactions",
    );
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
    const next = screen.getByTestId("next-context-settings");
    expect(next).toHaveTextContent("Next call: 500k window · compacts at 400k");
    expect(next).toHaveAttribute(
      "title",
      "Next model call: 500,000-token window, compaction at 400,000 tokens",
    );
    const usage = screen.getByTestId("context-usage");
    expect(usage).toHaveTextContent("~420 / 102k · 0.4%");
    expect(usage).toHaveAttribute(
      "title",
      "0.4% of compaction threshold; 420 of 102,400 estimated tokens",
    );
    expect(screen.getByTestId("context-footer")).toHaveTextContent("128k window");
  });

  it("shows input as a share of the compaction threshold and omits history estimates", async () => {
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
    await waitFor(() => expect(details).toHaveFocus());
    expect(screen.queryByText(/Latest assembled model input/)).toBeNull();
    expect(screen.getByTestId("context-usage")).toHaveTextContent(
      "~78.5k / 786k · 10%",
    );
    expect(screen.getByTestId("last-context-dispatch")).toHaveTextContent(
      "No compaction needed for this model call",
    );
    const table = screen.getByRole("table", { name: "Input token estimates" });
    expect(
      within(table)
        .getAllByRole("row")
        .slice(1)
        .map((row) => row.textContent),
    ).toEqual([
      "Tool definitions14,73318.8%",
      "Messages & instructions63,73681.2%",
      "Request settings510.1%",
    ]);
    const footer = screen.getByTestId("context-footer");
    expect(footer).toHaveTextContent(/^1M window · 0 compactions$/);
    expect(within(footer).getByText("1M window")).toHaveAttribute(
      "title",
      "Model context window: 1,048,576 tokens",
    );
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
    await screen.findByRole("row", { name: "User messages 100 10.0%" });
    const table = screen.getByRole("table", { name: "Input token estimates" });
    for (const [label, tokens, share] of [
      ["Instructions & context", "300", "30.0%"],
      ["User messages", "100", "10.0%"],
      ["Assistant messages", "50", "5.0%"],
      ["Tool calls", "80", "8.0%"],
      ["Tool results", "240", "24.0%"],
      ["Other message data", "10", "1.0%"],
      ["Tool definitions", "200", "20.0%"],
      ["Request settings", "20", "2.0%"],
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
    expect(screen.getByTestId("context-usage")).toHaveTextContent("—");
    expect(screen.queryByTestId("context-bar")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "About model input" }),
    ).not.toBeInTheDocument();
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
    expect(screen.getByTestId("context-footer")).toHaveTextContent(
      "Window unavailable",
    );
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
    expect(dispatch).toHaveTextContent(
      "Input compacted before this model call · 480 → 420 tokens",
    );
    expect(screen.getByTestId("context-usage")).toHaveTextContent("~420 / 400k · 0.1%");
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

    const compactions = screen.getByRole("list", {
      name: "Recent session compactions",
    });
    expect(screen.getByTestId("context-footer")).toHaveTextContent(
      "500k window · 1 compaction",
    );
    /* the runtime counts what the model was sent, so no count is shown */
    expect(compactions).not.toHaveTextContent("12 messages");
    expect(compactions).toHaveTextContent("900 → 300");
    expect(screen.getByTestId("context-meter")).toHaveTextContent("1 compaction");
    expect(screen.queryByTestId("last-context-dispatch")).not.toBeInTheDocument();
  });
  it("gives a compaction's time and tokens as the transcript's marker does", () => {
    render(
      <SessionContext
        context={context({
          sessionCompactionCount: 1,
          compactions: [
            {
              compactionKey: "provider-context-reduction:1",
              sequence: null,
              messagesCompacted: 1,
              originalTokens: 16_523,
              compactedTokens: 516,
              createdAt: "2026-10-29T21:14:59.563619Z",
            },
          ],
        })}
      />,
    );
    fireEvent.click(screen.getByTestId("context-meter"));

    const [entry] = within(
      screen.getByRole("list", { name: "Recent session compactions" }),
    ).getAllByRole("listitem");
    expect(entry).not.toHaveTextContent("message");
    expect(entry).toHaveTextContent("16.5k → 516");
    expect(entry).not.toHaveTextContent("T21:14:59");
    expect(entry.querySelector("time")).toHaveAttribute(
      "dateTime",
      "2026-10-29T21:14:59.563619Z",
    );
  });
  it("opens a compaction it can place, and leaves one it can't as text", () => {
    const opened = vi.fn();
    const compaction = (key: string, messageSequence: number | null) => ({
      compactionKey: key,
      sequence: null,
      messagesCompacted: 12,
      originalTokens: 16_523,
      compactedTokens: 516,
      createdAt: null,
      itemKey: `request-compaction:${key}`,
      messageSequence,
    });
    render(
      <SessionContext
        context={context({
          sessionCompactionCount: 2,
          compactions: [compaction("unplaced", null), compaction("placed", 13)],
        })}
        onOpenCompaction={opened}
      />,
    );
    fireEvent.click(screen.getByTestId("context-meter"));
    const list = screen.getByRole("list", { name: "Recent session compactions" });
    const [placed, unplaced] = within(list).getAllByRole("listitem");
    expect(within(unplaced).queryByRole("button")).toBeNull();
    fireEvent.click(within(placed).getByRole("button", { name: /16.5k → 516/ }));
    expect(opened).toHaveBeenCalledWith(
      expect.objectContaining({ compactionKey: "placed", messageSequence: 13 }),
    );
    expect(screen.getByTestId("context-meter")).toHaveAttribute(
      "aria-expanded",
      "false",
    );
  });
  it("draws one bar over the window with a segment and legend row per part", () => {
    render(
      <SessionContext
        context={context({
          contextWindow: 1_000_000,
          compactionThresholdTokens: 750_000,
          sessionCompactionCount: 0,
          lastRequest: lastRequest({
            estimatedInputTokens: 20_486,
            contextWindow: 1_000_000,
            compactionThresholdTokens: 750_000,
            components: {
              messages: 12_000,
              toolSchemas: 8_000,
              documents: 400,
              additionalParameters: 50,
              outputSchema: 36,
            },
          }),
        })}
      />,
    );
    expect(screen.getByTestId("context-meter")).toHaveTextContent("2.7%");
    fireEvent.click(screen.getByTestId("context-meter"));
    const usage = screen.getByTestId("context-usage");
    expect(usage).toHaveTextContent("~20.5k / 750k · 2.7%");
    expect(usage).toHaveAttribute(
      "title",
      "2.7% of compaction threshold; 20,486 of 750,000 estimated tokens",
    );
    const bar = screen.getByRole("img", {
      name: "Model input by part: Tool definitions 8,000, Messages & instructions 12,000, Attached documents 400, Output schema 36, Request settings 50. 1,000,000-token context window; compaction threshold at 750,000 tokens",
    });
    const segments = within(bar).getAllByTestId("context-segment");
    expect(
      segments.map((segment) => [segment.dataset.part, segment.dataset.share]),
    ).toEqual([
      ["Tool definitions", "0.0080"],
      ["Messages & instructions", "0.0120"],
      ["Attached documents", "0.0004"],
      ["Output schema", "0.0000"],
      ["Request settings", "0.0001"],
    ]);
    for (const segment of segments) expect(segment).toHaveClass("min-w-[3px]");
    const rows = within(
      screen.getByRole("table", { name: "Input token estimates" }),
    ).getAllByRole("row");
    expect(rows.slice(1).map((row) => row.textContent)).toEqual([
      "Tool definitions8,00039.1%",
      "Messages & instructions12,00058.6%",
      "Attached documents4002.0%",
      "Output schema360.2%",
      "Request settings500.2%",
    ]);
    rows.slice(1).forEach((row, index) => {
      const dot = row.querySelector("[aria-hidden='true']");
      expect(dot?.className).toContain(segments[index].className.split(" ").at(-1));
    });
    const tick = within(bar).getByTestId("compaction-threshold");
    expect(tick).toHaveAttribute("data-position", "0.7500");
    expect(tick).toHaveClass("pointer-events-none");
    expect(screen.getByTestId("context-footer")).toHaveTextContent(
      /^1M window · 0 compactions$/,
    );
  });

  it("links each segment to its legend row on hover and focus", async () => {
    render(
      <SessionContext
        context={context({
          lastRequest: lastRequest({
            estimatedInputTokens: 20_000,
            components: {
              messages: 8_723,
              toolSchemas: 11_277,
              documents: 0,
              additionalParameters: 0,
              outputSchema: 0,
            },
          }),
        })}
      />,
    );
    fireEvent.click(screen.getByTestId("context-meter"));
    const [tools, messages] = screen.getAllByTestId("context-segment");
    const table = screen.getByRole("table", { name: "Input token estimates" });
    const toolsRow = within(table).getByRole("row", { name: /^Tool definitions/ });
    const messagesRow = within(table).getByRole("row", {
      name: /^Messages & instructions/,
    });
    expect(tools).not.toHaveClass("opacity-40");
    expect(messages).not.toHaveClass("opacity-40");

    await userEvent.hover(tools);
    expect(
      await screen.findByText("Tool definitions · 11,277 tokens · 56.4%"),
    ).toBeVisible();
    expect(toolsRow).toHaveClass("bg-accent");
    expect(messagesRow).not.toHaveClass("bg-accent");
    expect(tools).not.toHaveClass("opacity-40");
    expect(messages).toHaveClass("opacity-40");
    await userEvent.unhover(tools);
    expect(toolsRow).not.toHaveClass("bg-accent");
    expect(messages).not.toHaveClass("opacity-40");

    await userEvent.hover(messagesRow);
    expect(tools).toHaveClass("opacity-40");
    expect(messagesRow).toHaveClass("bg-accent");
    await userEvent.unhover(messagesRow);

    act(() => toolsRow.focus());
    expect(toolsRow).toHaveAttribute("tabindex", "0");
    expect(messages).toHaveClass("opacity-40");
    expect(toolsRow).toHaveClass("bg-accent");
    act(() => toolsRow.blur());
    expect(messages).not.toHaveClass("opacity-40");
  });

  it("gives each segment a taller hit area than the bar", () => {
    render(<SessionContext context={context({ lastRequest: lastRequest() })} />);
    fireEvent.click(screen.getByTestId("context-meter"));
    for (const segment of screen.getAllByTestId("context-segment")) {
      expect(segment).toHaveClass(
        "relative",
        "before:absolute",
        "before:inset-y-[-6px]",
      );
    }
  });

  it("keeps the explanatory notes in the info tooltip", async () => {
    render(<SessionContext context={context({ lastRequest: lastRequest() })} />);
    fireEvent.click(screen.getByTestId("context-meter"));
    const details = screen.getByTestId("context-details");
    expect(details).not.toHaveTextContent("Latest assembled model input");
    expect(details).not.toHaveTextContent("detailed message breakdown is unavailable");
    await userEvent.hover(screen.getByRole("button", { name: "About model input" }));
    const tip = await screen.findByText(/Latest assembled model input/);
    expect(tip).toHaveTextContent(
      "Latest assembled model input. Compaction is checked before each model call. The next call may differ. A detailed message breakdown is unavailable for this model call.",
    );
  });

  it("shows the loading breakdown, then splits the message segment into its parts", async () => {
    let finish!: (value: Awaited<ReturnType<typeof loadDetails>>) => void;
    const loadDetails = vi.fn(
      () =>
        new Promise<{
          systemPromptTokens: number;
          messageTokens: number;
          userInputTokens: number;
          assistantTokens: number;
          toolCallTokens: number;
          toolResultTokens: number;
          otherMessageTokens: number;
          messageCount: number;
          toolDefinitionsCount: number;
          toolCallCount: number;
        } | null>((resolve) => {
          finish = resolve;
        }),
    );
    render(
      <SessionContext
        loadDetails={loadDetails}
        context={context({ lastRequest: lastRequest() })}
      />,
    );
    fireEvent.click(screen.getByTestId("context-meter"));
    expect(screen.getByText("Loading message breakdown…")).toBeInTheDocument();
    expect(screen.getAllByTestId("context-segment")).toHaveLength(2);
    await act(async () => {
      finish({
        systemPromptTokens: 100,
        messageTokens: 300,
        userInputTokens: 200,
        assistantTokens: 100,
        toolCallTokens: 0,
        toolResultTokens: 0,
        otherMessageTokens: 0,
        messageCount: 2,
        toolDefinitionsCount: 1,
        toolCallCount: 0,
      });
    });
    expect(screen.queryByText("Loading message breakdown…")).not.toBeInTheDocument();
    expect(
      screen.getAllByTestId("context-segment").map((segment) => segment.dataset.part),
    ).toEqual([
      "Tool definitions",
      "Instructions & context",
      "User messages",
      "Assistant messages",
    ]);
  });

  it("places the threshold tick after the segments below it and fills an over-window input", () => {
    render(
      <SessionContext
        context={context({
          lastRequest: lastRequest({
            estimatedInputTokens: 600,
            contextWindow: 1_000,
            compactionThresholdTokens: 800,
            components: {
              messages: 500,
              toolSchemas: 100,
              documents: 0,
              additionalParameters: 0,
              outputSchema: 0,
            },
          }),
        })}
      />,
    );
    fireEvent.click(screen.getByTestId("context-meter"));
    const bar = screen.getByTestId("context-bar");
    const tick = within(bar).getByTestId("compaction-threshold");
    expect(tick).toHaveAttribute("data-position", "0.8000");
    expect(tick.getAttribute("style")).toMatch(/calc\(8px \+ 0\.8 \* \(100% - 8px\)\)/);
    expect(bar.lastElementChild?.previousElementSibling).toHaveClass("flex-1");
  });

  it("omits the empty remainder when the input fills the window", () => {
    render(
      <SessionContext
        context={context({
          lastRequest: lastRequest({
            estimatedInputTokens: 1_200,
            contextWindow: 1_000,
            compactionThresholdTokens: 800,
            compactionReason: "compactor_unavailable",
            components: {
              messages: 1_200,
              toolSchemas: 0,
              documents: 0,
              additionalParameters: 0,
              outputSchema: 0,
            },
          }),
        })}
      />,
    );
    expect(screen.getByTestId("context-meter")).toHaveTextContent("150%");
    fireEvent.click(screen.getByTestId("context-meter"));
    const bar = screen.getByTestId("context-bar");
    expect(bar.querySelector(".flex-1")).toBeNull();
    expect(within(bar).getAllByTestId("context-segment")).toHaveLength(1);
    expect(screen.getByTestId("last-context-dispatch")).toHaveTextContent(
      "Automatic compaction unavailable for this model call",
    );
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
