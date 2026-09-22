import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import type {
  DesktopApiAdapter,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import { LocalServer } from "../src/ui/screens/agent/LocalServer";
import type { Shell } from "../src/ui/hooks/useShell";

// Match the existing switch-control harness: jsdom has no PointerEvent.
const originalPointerEvent = window.PointerEvent;
beforeAll(() => {
  window.PointerEvent = MouseEvent as typeof PointerEvent;
});
afterAll(() => {
  window.PointerEvent = originalPointerEvent;
});

const running = (
  ceiling: ManagedServerStatus["effectiveToolCeiling"],
  root: string | null,
  pairingReady: boolean,
): ManagedServerStatus => ({
  state: "running",
  autoStart: true,
  agentName: "Workshop Agent",
  agentDid: "did:key:agent",
  graphql: "http://127.0.0.1:9191/graphql",
  effectiveToolCeiling: ceiling,
  effectiveToolRoot: root,
  suggestedToolRoot: "/Users/test",
  pairingReady,
  error: null,
});

describe("managed runtime restart settings", () => {
  it("changes login startup without stopping the running agent", async () => {
    const user = userEvent.setup();
    const previous = running("readwrite", "/Users/test", true);
    const api = {
      managedServerStatus: vi.fn(async () => previous),
      setManagedServerAutoStart: vi.fn(async () => ({ ...previous, autoStart: false })),
      startManagedServer: vi.fn(),
      stopManagedServer: vi.fn(),
    } as unknown as DesktopApiAdapter;
    const shell = {
      api,
      snapshot: {},
      refreshSnapshot: vi.fn(),
    } as unknown as Shell;
    render(<LocalServer shell={shell} />);
    await screen.findByText("running");
    await user.click(screen.getByRole("switch", { name: "Start at login" }));
    expect(api.setManagedServerAutoStart).toHaveBeenCalledWith(false);
    expect(api.startManagedServer).not.toHaveBeenCalled();
    expect(api.stopManagedServer).not.toHaveBeenCalled();
    expect(screen.getByText("running")).toBeInTheDocument();
  });

  it("explains when service status is unavailable instead of claiming the agent stopped", async () => {
    const api = {
      managedServerStatus: vi.fn(async () => {
        throw new Error("user service manager unavailable");
      }),
    } as unknown as DesktopApiAdapter;
    const shell = { api, snapshot: {}, refreshSnapshot: vi.fn() } as unknown as Shell;
    render(<LocalServer shell={shell} />);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not check the background agent",
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "user service manager unavailable",
    );
    expect(screen.queryByText("stopped")).not.toBeInTheDocument();
  });

  it("shows the runtime-confirmed authority even when later pairing polling fails", async () => {
    const user = userEvent.setup();
    const previous = running("readwrite", "/Users/test", true);
    const restarted = running("meta-only", null, false);
    const managedServerStatus = vi
      .fn<DesktopApiAdapter["managedServerStatus"]>()
      .mockResolvedValueOnce(previous)
      .mockRejectedValue(new Error("pairing probe failed"));
    const api = {
      managedServerStatus,
      restartManagedServer: vi.fn(async () => restarted),
      validateManagedServerRoot: vi.fn(),
    } as unknown as DesktopApiAdapter;
    const shell = {
      api,
      snapshot: {},
      refreshSnapshot: vi.fn(),
    } as unknown as Shell;

    render(<LocalServer shell={shell} />);
    expect(
      screen.getByText(/Quit Desktop closes only this frontend/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/operating system—not the desktop app—start the agent/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/Start with the app/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/Supervised by the desktop/i)).not.toBeInTheDocument();
    await user.click(await screen.findByRole("button", { name: "Change access…" }));
    await user.selectOptions(
      screen.getByRole("combobox", { name: "Tool ceiling" }),
      "meta-only",
    );
    await user.click(screen.getByRole("button", { name: "Review complete — restart" }));

    expect(await screen.findByText("meta-only")).toBeInTheDocument();
    // Authority is published before pairing finishes. Wait for that second
    // phase too, so its timer cannot outlive the test's DOM environment.
    expect(await screen.findByText("pairing probe failed")).toBeInTheDocument();
    expect(api.restartManagedServer).toHaveBeenCalledWith("Workshop Agent", {
      toolCeiling: "meta-only",
      toolRoot: null,
    });
  });
});
