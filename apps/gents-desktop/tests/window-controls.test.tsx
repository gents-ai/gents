import { fireEvent, render, screen } from "@testing-library/react";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppShell } from "../src/ui/app/AppShell";
import { WindowControls } from "../src/ui/app/WindowControls";

const controls = vi.hoisted(() => ({
  closeWindow: vi.fn(),
  minimizeWindow: vi.fn(),
  toggleMaximizeWindow: vi.fn(),
  onMaximizedChange: vi.fn(),
}));

vi.mock("../src/lib/windowControls", () => controls);
vi.mock("../src/lib/shellPlatform", () => ({
  headerIsWindowBar: () => true,
  isWindowsTauriShell: () => true,
}));

describe("Windows caption controls", () => {
  afterEach(() => vi.clearAllMocks());

  it("exposes and invokes the native window actions", () => {
    controls.onMaximizedChange.mockImplementation(() => vi.fn());
    render(<WindowControls />);

    fireEvent.click(screen.getByRole("button", { name: "Minimize" }));
    fireEvent.click(screen.getByRole("button", { name: "Maximize" }));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    expect(controls.minimizeWindow).toHaveBeenCalledOnce();
    expect(controls.toggleMaximizeWindow).toHaveBeenCalledOnce();
    expect(controls.closeWindow).toHaveBeenCalledOnce();
  });

  it("reflects maximize changes and unregisters the listener", () => {
    const stop = vi.fn();
    controls.onMaximizedChange.mockImplementation((handler) => {
      handler(true);
      return stop;
    });

    const { unmount } = render(<WindowControls />);

    expect(screen.getByRole("button", { name: "Restore" })).toBeInTheDocument();
    unmount();
    expect(stop).toHaveBeenCalledOnce();
  });

  it("keeps interactive controls inside the draggable application header", () => {
    controls.onMaximizedChange.mockImplementation(() => vi.fn());
    const { container } = render(
      <TooltipProvider>
        <AppShell
          route={{ name: "agents" }}
          agentName={null}
          agentDid={null}
          deployment={null}
          online
          mailboxCount={0}
        >
          <p>Content</p>
        </AppShell>
      </TooltipProvider>,
    );

    const header = container.querySelector("header");
    expect(header).toHaveAttribute("data-tauri-drag-region");
    expect(header?.querySelector("a[aria-label='Agents']")).toBeInTheDocument();
    expect(
      header?.querySelector("button[aria-label$='Show sync diagnostics.']"),
    ).toBeInTheDocument();
    expect(screen.getByTestId("window-controls")).toBeInTheDocument();
  });
});
