import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { renderIn, testApp } from "./app-fixture";
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
      return () => {
        popoverControl.completions.delete(id);
      };
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
import { AgentPicker } from "../src/ui/screens/BehaviorPicker";
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

async function acknowledgeClose() {
  await act(async () => {
    for (const complete of [...popoverControl.completions.values()]) complete();
  });
}

function Harness({ showPicker = true }: { showPicker?: boolean }) {
  return (
    <>
      <AgentPicker
        deployment={showPicker ? deployment : { ...deployment, agents: [] }}
        agentId="default"
        onChange={vi.fn()}
      />
      <SyncHealth syncHealth={healthy} />
    </>
  );
}

describe("AgentPicker popover lifetime", () => {
  it("releases an active picker removed while sync is pending", async () => {
    const user = userEvent.setup();
    const view = renderIn(testApp(), <Harness />);

    await user.click(screen.getByRole("button", { name: "Agent" }));
    expect(await screen.findByRole("dialog", { name: "Choose agent" })).toBeVisible();

    await user.click(screen.getByRole("button", { name: /Show sync diagnostics/ }));
    expect(screen.getByRole("dialog", { name: "Choose agent" })).toBeVisible();
    expect(
      screen.queryByRole("dialog", { name: "Database sync details" }),
    ).not.toBeInTheDocument();
    view.rerender(<Harness showPicker={false} />);

    expect(
      await screen.findByRole("dialog", { name: "Database sync details" }),
    ).toBeVisible();
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
  });

  it("opens the create sheet only after the picker has closed", async () => {
    const user = userEvent.setup();
    renderIn(testApp(), <Harness />);

    await user.click(screen.getByRole("button", { name: "Agent" }));
    await screen.findByRole("dialog", { name: "Choose agent" });
    await user.click(screen.getByRole("button", { name: /Create new agent/ }));

    // The closing picker stays mounted for its exit; the sheet must wait.
    expect(screen.getByRole("dialog", { name: "Choose agent" })).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "New agent" })).not.toBeInTheDocument();

    await acknowledgeClose();
    expect(await screen.findByRole("dialog", { name: "New agent" })).toBeVisible();
    expect(
      screen.queryByRole("dialog", { name: "Choose agent" }),
    ).not.toBeInTheDocument();
  });

  it("lets a popover opened during the picker's exit take the turn from create", async () => {
    const user = userEvent.setup();
    renderIn(testApp(), <Harness />);

    await user.click(screen.getByRole("button", { name: "Agent" }));
    await screen.findByRole("dialog", { name: "Choose agent" });
    await user.click(screen.getByRole("button", { name: /Create new agent/ }));
    await user.click(screen.getByRole("button", { name: /Show sync diagnostics/ }));

    await acknowledgeClose();
    expect(
      await screen.findByRole("dialog", { name: "Database sync details" }),
    ).toBeVisible();
    expect(screen.queryByRole("dialog", { name: "New agent" })).not.toBeInTheDocument();
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
  });

  it("withdraws a pending create when the picker is reopened during its exit", async () => {
    const user = userEvent.setup();
    renderIn(testApp(), <Harness />);

    await user.click(screen.getByRole("button", { name: "Agent" }));
    await screen.findByRole("dialog", { name: "Choose agent" });
    await user.click(screen.getByRole("button", { name: /Create new agent/ }));
    await user.click(screen.getByRole("button", { name: "Agent" }));
    expect(screen.getByRole("dialog", { name: "Choose agent" })).toBeVisible();

    // The next ordinary close must not replay the withdrawn create.
    await user.click(screen.getByRole("button", { name: "Agent" }));
    await acknowledgeClose();
    expect(screen.queryByRole("dialog", { name: "New agent" })).not.toBeInTheDocument();
    expect(screen.queryAllByRole("dialog")).toHaveLength(0);
  });

  it("cancels a pending picker when its chosen agent disappears", async () => {
    const user = userEvent.setup();
    const view = renderIn(testApp(), <Harness />);

    await user.click(screen.getByRole("button", { name: /Show sync diagnostics/ }));
    expect(
      await screen.findByRole("dialog", { name: "Database sync details" }),
    ).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Agent" }));
    expect(
      screen.queryByRole("dialog", { name: "Database sync details" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Database sync")).toBeVisible();
    expect(
      screen.queryByRole("dialog", { name: "Choose agent" }),
    ).not.toBeInTheDocument();
    view.rerender(<Harness showPicker={false} />);

    await acknowledgeClose();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    view.rerender(<Harness />);
    expect(
      screen.queryByRole("dialog", { name: "Choose agent" }),
    ).not.toBeInTheDocument();
    expect(screen.queryAllByRole("dialog")).toHaveLength(0);

    await user.click(screen.getByRole("button", { name: /Show sync diagnostics/ }));
    expect(
      await screen.findByRole("dialog", { name: "Database sync details" }),
    ).toBeVisible();
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
  });
});
