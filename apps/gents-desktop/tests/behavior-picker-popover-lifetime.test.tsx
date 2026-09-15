import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { SyncHealthView } from "@source-inc/gents-desktop-client";

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
  const Context = React.createContext<RootProps & { mounted: boolean }>({
    open: false,
    onOpenChange: () => undefined,
    mounted: false,
    children: null,
  });
  function Popover(props: RootProps) {
    const id = React.useId();
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
    return (
      <Context.Provider value={{ ...props, mounted }}>
        {props.children}
      </Context.Provider>
    );
  }
  function PopoverTrigger({ render, children, ...props }: any) {
    const root = React.useContext(Context);
    return React.cloneElement(render ?? <button />, {
      ...props,
      "aria-expanded": root.open,
      onClick: () => root.onOpenChange(!root.open),
      children,
    });
  }
  const PopoverContent = React.forwardRef<HTMLDivElement, any>(function Content(
    { align: _align, side: _side, children, ...props },
    ref,
  ) {
    const root = React.useContext(Context);
    return root.mounted ? (
      <div ref={ref} role="dialog" {...props}>
        {children}
      </div>
    ) : null;
  });
  return { Popover, PopoverTrigger, PopoverContent };
});

import { SyncHealth } from "../src/ui/app/SyncHealth";
import { BehaviorPicker } from "../src/ui/screens/BehaviorPicker";
import type { Shell } from "../src/hooks/useShell";
import { deployment } from "./config-panel-wiring/fixtures";

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

const shell = { behaviorDescriptions: {} } as Shell;

async function acknowledgeClose() {
  await act(async () => {
    for (const complete of [...popoverControl.completions.values()]) complete();
  });
}

function Harness({ showPicker = true }: { showPicker?: boolean }) {
  return (
    <>
      <BehaviorPicker
        shell={shell}
        deployment={showPicker ? deployment : { ...deployment, behaviors: [] }}
        behaviorId="default"
        onChange={vi.fn()}
      />
      <SyncHealth syncHealth={healthy} />
    </>
  );
}

describe("BehaviorPicker popover lifetime", () => {
  it("releases an active picker removed while sync is pending", async () => {
    const user = userEvent.setup();
    const view = render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Behaviour" }));
    expect(
      await screen.findByRole("dialog", { name: "Choose behavior" }),
    ).toBeVisible();

    await user.click(screen.getByRole("button", { name: /Show sync diagnostics/ }));
    expect(screen.getByRole("dialog", { name: "Choose behavior" })).toBeVisible();
    expect(
      screen.queryByRole("dialog", { name: "Database sync details" }),
    ).not.toBeInTheDocument();
    view.rerender(<Harness showPicker={false} />);

    expect(
      await screen.findByRole("dialog", { name: "Database sync details" }),
    ).toBeVisible();
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
  });

  it("cancels a pending picker when its chosen behavior disappears", async () => {
    const user = userEvent.setup();
    const view = render(<Harness />);

    await user.click(screen.getByRole("button", { name: /Show sync diagnostics/ }));
    expect(
      await screen.findByRole("dialog", { name: "Database sync details" }),
    ).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Behaviour" }));
    expect(
      screen.queryByRole("dialog", { name: "Database sync details" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Database sync")).toBeVisible();
    expect(
      screen.queryByRole("dialog", { name: "Choose behavior" }),
    ).not.toBeInTheDocument();
    view.rerender(<Harness showPicker={false} />);

    await acknowledgeClose();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    view.rerender(<Harness />);
    expect(
      screen.queryByRole("dialog", { name: "Choose behavior" }),
    ).not.toBeInTheDocument();
    expect(screen.queryAllByRole("dialog")).toHaveLength(0);

    await user.click(screen.getByRole("button", { name: /Show sync diagnostics/ }));
    expect(
      await screen.findByRole("dialog", { name: "Database sync details" }),
    ).toBeVisible();
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
  });
});
