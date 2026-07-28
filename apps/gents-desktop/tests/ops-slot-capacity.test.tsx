import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { BackgroundedToolsPanel } from "@source-inc/gents-desktop-operations";
import type { RuntimeView } from "@source-inc/gents-desktop-client";

describe("ops slot capacity", () => {
  it("shows runtime-reported capacity and queue depth", () => {
    const runtime: RuntimeView = {
      processState: "running",
      behaviorExecutorCapacity: 4,
      behaviorExecutorQueueDepth: 2,
    };
    render(<BackgroundedToolsPanel runtime={runtime} />);
    const capacity = screen.getByTestId("ops-slot-capacity");
    expect(capacity).toHaveTextContent("capacity 4");
    expect(capacity).toHaveTextContent("2 queued");
  });

  it("stays silent when the runtime reports no capacity", () => {
    render(<BackgroundedToolsPanel runtime={{ processState: "running" }} />);
    expect(screen.queryByTestId("ops-slot-capacity")).not.toBeInTheDocument();
  });
});
