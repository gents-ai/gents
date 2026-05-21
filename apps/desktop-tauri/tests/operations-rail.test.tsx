import { describe, expect, it } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { useState } from "react";

import {
  OperationsRail,
  OperationsRailProvider,
  type OperationsRailTabDescriptor,
  useOperationsRail,
} from "../src/components/operations";

function HarnessOpenLineageButton() {
  const rail = useOperationsRail();
  return (
    <button onClick={() => rail.setActiveTab("lineage")}>
      open-lineage-button
    </button>
  );
}

function HarnessWithTabs({ tabs }: { tabs: OperationsRailTabDescriptor[] }) {
  return (
    <OperationsRailProvider tabs={tabs}>
      <HarnessOpenLineageButton />
      <OperationsRail />
    </OperationsRailProvider>
  );
}

function CollapsibleHarness({ tabs }: { tabs: OperationsRailTabDescriptor[] }) {
  const [open, setOpen] = useState(false);
  return (
    <OperationsRailProvider tabs={tabs}>
      <OperationsRail open={open} onOpenChange={setOpen} />
    </OperationsRailProvider>
  );
}

describe("OperationsRail", () => {
  it("renders empty when no tabs are registered", () => {
    render(
      <OperationsRailProvider tabs={[]}>
        <OperationsRail />
      </OperationsRailProvider>,
    );
    expect(
      screen.queryByRole("tablist", { name: /operations/i }),
    ).not.toBeInTheDocument();
  });

  it("renders the registered tabs and mounts only the active one", () => {
    const tabs: OperationsRailTabDescriptor[] = [
      {
        id: "background",
        label: "Background",
        render: () => <div data-testid="background-panel">bg</div>,
      },
      {
        id: "lineage",
        label: "Lineage",
        render: () => <div data-testid="lineage-panel">lin</div>,
      },
    ];
    render(<HarnessWithTabs tabs={tabs} />);

    // First tab is active by default.
    expect(screen.getByTestId("background-panel")).toBeInTheDocument();
    expect(screen.queryByTestId("lineage-panel")).not.toBeInTheDocument();

    // setActiveTab via external caller switches the active tab.
    fireEvent.click(screen.getByText("open-lineage-button"));
    expect(screen.getByTestId("lineage-panel")).toBeInTheDocument();
    expect(screen.queryByTestId("background-panel")).not.toBeInTheDocument();
  });

  it("clicking a tab button activates that tab", () => {
    const tabs: OperationsRailTabDescriptor[] = [
      {
        id: "background",
        label: "Background",
        render: () => <div data-testid="background-panel">bg</div>,
      },
      {
        id: "lineage",
        label: "Lineage",
        render: () => <div data-testid="lineage-panel">lin</div>,
      },
    ];
    render(<HarnessWithTabs tabs={tabs} />);
    fireEvent.click(screen.getByRole("tab", { name: "Lineage" }));
    expect(screen.getByTestId("lineage-panel")).toBeInTheDocument();
  });

  it("collapses into a drawer handle without mounting the active panel", () => {
    const tabs: OperationsRailTabDescriptor[] = [
      {
        id: "background",
        label: "Background",
        render: () => <div data-testid="background-panel">bg</div>,
      },
    ];
    render(<CollapsibleHarness tabs={tabs} />);

    expect(screen.queryByTestId("background-panel")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /open operations drawer/i }));
    expect(screen.getByTestId("background-panel")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /close operations drawer/i }));
    expect(screen.queryByTestId("background-panel")).not.toBeInTheDocument();
  });
});
