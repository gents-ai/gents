import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { SyncHealthView } from "@source-inc/gents-desktop-client";

/* Base UI keeps a closing popover mounted for its exit animation; jsdom has
   none. This popover stays mounted after closing until the test acknowledges
   the exit, and closes on an outside press like the real one. */
const popoverControl = vi.hoisted(() => ({
  completions: new Map<string, () => void>(),
}));

vi.mock("@gents/ui/components/popover", async () => {
  const React = await import("react");
  type RootProps = {
    open: boolean;
    onOpenChange: (open: boolean) => void;
    onOpenChangeComplete?: (open: boolean) => void;
    children: React.ReactNode;
  };
  const Context = React.createContext<
    RootProps & { mounted: boolean; triggerRef: React.RefObject<HTMLElement | null> }
  >({
    open: false,
    onOpenChange: () => undefined,
    mounted: false,
    children: null,
    triggerRef: { current: null },
  });
  function Popover(props: RootProps) {
    const id = React.useId();
    const triggerRef = React.useRef<HTMLElement | null>(null);
    const [mounted, setMounted] = React.useState(props.open);
    React.useEffect(() => {
      if (props.open) {
        setMounted(true);
        popoverControl.completions.delete(id);
      } else if (mounted) {
        popoverControl.completions.set(id, () => {
          setMounted(false);
          popoverControl.completions.delete(id);
          props.onOpenChangeComplete?.(false);
        });
      }
      return () => popoverControl.completions.delete(id);
    }, [id, mounted, props.open, props.onOpenChangeComplete]);
    const { open, onOpenChange } = props;
    React.useEffect(() => {
      if (!open) return;
      const outside = (event: PointerEvent) => {
        const target = event.target as Node;
        if (triggerRef.current?.contains(target)) return;
        if ((target as Element).closest?.("[data-test-popup]")) return;
        onOpenChange(false);
      };
      document.addEventListener("pointerdown", outside, true);
      return () => document.removeEventListener("pointerdown", outside, true);
    }, [open, onOpenChange]);
    return (
      <Context.Provider value={{ ...props, mounted, triggerRef }}>
        {props.children}
      </Context.Provider>
    );
  }
  function PopoverTrigger({
    render,
    children,
    onMouseEnter: _e,
    onMouseLeave: _l,
    ...props
  }: any) {
    const root = React.useContext(Context);
    return React.cloneElement(render ?? <button />, {
      ...props,
      ref: root.triggerRef,
      "aria-expanded": root.open,
      onClick: () => root.onOpenChange(!root.open),
      children,
    });
  }
  const PopoverContent = React.forwardRef<HTMLDivElement, any>(function Content(
    {
      align: _align,
      side: _side,
      onMouseEnter: _e,
      onMouseLeave: _l,
      children,
      ...props
    },
    ref,
  ) {
    const root = React.useContext(Context);
    return root.mounted ? (
      <div ref={ref} role="dialog" data-test-popup="" {...props}>
        {children}
      </div>
    ) : null;
  });
  return { Popover, PopoverTrigger, PopoverContent };
});

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));
vi.mock("../src/ui/screens/BehaviorPicker", () => ({ BehaviorPicker: () => null }));
vi.mock("../src/ui/screens/TracePanel", () => ({
  TracePanel: () => <p>Trace</p>,
}));
vi.stubGlobal(
  "IntersectionObserver",
  class {
    observe() {}
    disconnect() {}
  },
);

import { SyncHealth } from "../src/ui/app/SyncHealth";
import type { Shell } from "../src/ui/hooks/useShell";
import { SessionScreen } from "../src/ui/screens/SessionScreen";

const healthy: SyncHealthView = {
  state: "healthy",
  lastError: null,
  connectedPeerCount: 1,
  pendingDagCount: 0,
  persistedPendingDagCount: 0,
  pushRetryMarkerCount: 0,
  exhaustedFetchCount: 0,
  quarantinedDagCount: 0,
};

const context = {
  estimatedDurableTokens: 0,
  estimatedConversationTokens: 0,
  contextWindow: 128_000,
  compactionThreshold: 0.8,
  compactionThresholdTokens: 102_400,
  durableMessageCount: 0,
  providerMessageCount: 0,
  totalCompactedMessages: 0,
  compactions: [],
  lastRequest: null,
};

function sessionShell(forkSession = vi.fn().mockResolvedValue("fork-1")): Shell {
  return {
    selectedSessionId: "session",
    selectedSession: {
      sessionId: "session",
      agentDid: "did:key:agent",
      behaviorId: "behavior",
      title: "Session",
      turnState: "completed",
      timelineItems: [],
      timelinePage: { hasOlder: false },
      latestRequestId: "request",
      latestRequestOutcome: null,
      goal: null,
      context,
    },
    selectedBehaviorId: "behavior",
    selectedAgentDid: "did:key:agent",
    selectedDeployment: null,
    draft: "",
    setDraft: vi.fn(),
    mailboxCause: null,
    sending: false,
    error: null,
    activityStatus: null,
    nonEmptyContentSendStatus: { kind: "ready" },
    captureComposeIntent: () => 0,
    acceptsComposeIntent: () => true,
    selectBehavior: vi.fn(),
    holds: [],
    interruptVisible: false,
    selectedTrackedRequestId: null,
    activeRequestId: null,
    sessionLoad: { phase: "ready" },
    loadOlderSessionTimeline: vi.fn(),
    retryMessage: vi.fn(),
    forkSession,
    refreshSnapshot: vi.fn(),
    api: {
      sessionProvenance: vi.fn().mockResolvedValue(null),
      fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
    },
  } as unknown as Shell;
}

function Harness({ shell }: { shell: Shell }) {
  return (
    <>
      <SyncHealth syncHealth={healthy} />
      <SessionScreen shell={shell} />
    </>
  );
}

async function acknowledgeClose() {
  await act(async () => {
    for (const complete of [...popoverControl.completions.values()]) complete();
  });
}

const dialogs = () => document.querySelectorAll('[role="dialog"]');

describe("shell dialogs take turns (#1778)", () => {
  it("sync popover, then context meter, then the side panel never stack", async () => {
    const user = userEvent.setup();
    render(<Harness shell={sessionShell()} />);

    await user.click(screen.getByRole("button", { name: /Show sync diagnostics/ }));
    expect(screen.getByRole("dialog", { name: "Database sync details" })).toBeVisible();

    await user.click(screen.getByTestId("context-meter"));
    expect(dialogs().length).toBeLessThanOrEqual(1);
    await acknowledgeClose();
    expect(
      await screen.findByRole("dialog", { name: "Session context details" }),
    ).toBeVisible();
    expect(dialogs()).toHaveLength(1);

    // The header's side panel button: in a narrow window it opens a sheet.
    const [sidePanel] = screen.getAllByRole("button", { name: "Open side panel" });
    await user.click(sidePanel!);
    // The context popover is still leaving; the sheet waits for it.
    expect(dialogs()).toHaveLength(1);
    expect(
      screen.queryByRole("dialog", { name: "Side panel" }),
    ).not.toBeInTheDocument();

    await acknowledgeClose();
    expect(
      await screen.findByRole("dialog", { name: "Side panel" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("dialog", { name: "Session context details" }),
    ).not.toBeInTheDocument();
    expect(dialogs()).toHaveLength(1);
  });

  it("the fork notice waits for an open popover to leave", async () => {
    const user = userEvent.setup();
    render(<Harness shell={sessionShell()} />);

    await user.click(screen.getByTestId("context-meter"));
    expect(
      await screen.findByRole("dialog", { name: "Session context details" }),
    ).toBeVisible();

    const [fork] = screen.getAllByRole("button", { name: "Fork session" });
    await user.click(fork!);
    expect(dialogs()).toHaveLength(1);
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();

    await acknowledgeClose();
    expect(await screen.findByText(/is now its own session/)).toBeInTheDocument();
    expect(
      screen.queryByRole("dialog", { name: "Session context details" }),
    ).not.toBeInTheDocument();
  });
});
