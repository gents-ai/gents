import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type {
  DesktopApiAdapter,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import { LocalServer } from "../src/ui/screens/agent/LocalServer";
import type { Shell } from "../src/ui/hooks/useShell";

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
