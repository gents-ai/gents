import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { renderIn, testApp } from "./app-fixture";
import type {
  DesktopApiAdapter,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";
import { LocalServer } from "../src/ui/screens/agent/LocalServer";
import { deployment } from "./config-panel-wiring/fixtures";

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
  nodeName: "Workshop Node",
  nodeDid: "did:key:node",
  graphql: "http://127.0.0.1:9191/graphql",
  effectiveToolCeiling: ceiling,
  effectiveToolRoot: root,
  suggestedToolRoot: "/Users/test",
  pairingReady,
  approvalRequired: false,
  runtimeBooting: false,
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
    renderIn(testApp({ api }), <LocalServer />);
    await screen.findByText("running");
    await user.click(screen.getByRole("switch", { name: "Keep running after quit" }));
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
    renderIn(testApp({ api }), <LocalServer />);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not check the background node",
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "user service manager unavailable",
    );
    expect(screen.queryByText("stopped")).not.toBeInTheDocument();
    /* a message wraps under the description, not beside the state */
    expect(
      screen.getByRole("alert").closest("[data-slot=field-content]"),
    ).not.toBeNull();
  });

  it("shows the runtime-confirmed authority even when later pairing polling fails", async () => {
    const user = userEvent.setup();
    const previous = running("readwrite", "/Users/test", true);
    const restarted = running("meta-only", null, false);
    const managedServerStatus = vi
      .fn<NonNullable<DesktopApiAdapter["managedServerStatus"]>>()
      .mockResolvedValueOnce(previous)
      .mockRejectedValue(new Error("pairing probe failed"));
    const api = {
      managedServerStatus,
      restartManagedServer: vi.fn(async () => restarted),
      validateManagedServerRoot: vi.fn(),
    } as unknown as DesktopApiAdapter;

    renderIn(testApp({ api }), <LocalServer />);
    expect(
      screen.getByText(
        /Quitting stops the local node unless background running is enabled/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Keep triggers and schedules running after quitting/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/Start with the app/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/Supervised by the desktop/i)).not.toBeInTheDocument();
    await user.click(await screen.findByRole("button", { name: "Change access…" }));
    await user.click(screen.getByRole("combobox", { name: "Tool ceiling" }));
    await user.click(await screen.findByRole("option", { name: "Metatools only" }));
    await user.click(screen.getByRole("button", { name: "Review complete — restart" }));

    expect(await screen.findByText("meta-only")).toBeInTheDocument();
    // Authority is published before pairing finishes. Wait for that second
    // phase too, so its timer cannot outlive the test's DOM environment.
    expect(await screen.findByText("pairing probe failed")).toBeInTheDocument();
    expect(api.restartManagedServer).toHaveBeenCalledWith("Workshop Node", {
      toolCeiling: "meta-only",
      toolRoot: null,
    });
  });
  describe("a changed tool root", () => {
    const nodeDid = deployment.nodeDid;
    const at = (root: string | null, ceiling = "readwrite" as const) => ({
      ...running(ceiling, root, true),
      nodeDid,
    });
    const harness = (deployments: unknown[] | undefined) => {
      const api = {
        managedServerStatus: vi.fn(async () => at("/Users/test")),
        validateManagedServerRoot: vi.fn(async (path: string) => path),
        selectAgentWorkspaceRoot: vi.fn(async () => ({})),
        restartManagedServer: vi.fn(
          async (_name: string, authority: { toolRoot: string | null }) =>
            at(authority.toolRoot),
        ),
      };
      renderIn(
        testApp({ api: api as unknown as DesktopApiAdapter, deployments }),
        <LocalServer />,
      );
      return api;
    };
    const reviewRoot = async (root: string) => {
      const user = userEvent.setup();
      await user.click(await screen.findByRole("button", { name: "Change access…" }));
      const input = screen.getByLabelText("Tool root");
      await user.clear(input);
      await user.type(input, root);
      await user.tab();
      await user.click(
        screen.getByRole("button", { name: "Review complete — restart" }),
      );
      return user;
    };

    it("publishes the folder for the node's agent before the restart", async () => {
      const api = harness([deployment]);
      await reviewRoot("/Users/test/work");
      expect(await screen.findByText("/Users/test/work")).toBeInTheDocument();
      expect(api.selectAgentWorkspaceRoot).toHaveBeenCalledWith(
        nodeDid,
        "default",
        "/Users/test/work",
      );
      expect(api.restartManagedServer).toHaveBeenCalledWith("Workshop Node", {
        toolCeiling: "readwrite",
        toolRoot: "/Users/test/work",
      });
      expect(api.selectAgentWorkspaceRoot.mock.invocationCallOrder[0]).toBeLessThan(
        api.restartManagedServer.mock.invocationCallOrder[0],
      );
    });

    it("refuses before any write while the node's agents are unobserved, and keeping the current folder restarts", async () => {
      const api = harness(undefined);
      const user = await reviewRoot("/Users/test/work");
      const alert = await screen.findByRole("alert");
      expect(alert).toHaveTextContent("this node's agents have not loaded yet");
      expect(alert).toHaveTextContent("keep the current folder");
      expect(alert).toHaveTextContent("wait for the node's agents to appear");
      expect(api.selectAgentWorkspaceRoot).not.toHaveBeenCalled();
      expect(api.restartManagedServer).not.toHaveBeenCalled();

      await user.click(screen.getByRole("button", { name: "Keep current folder" }));
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      await user.click(
        screen.getByRole("button", { name: "Review complete — restart" }),
      );
      await vi.waitFor(() =>
        expect(api.restartManagedServer).toHaveBeenCalledWith("Workshop Node", {
          toolCeiling: "readwrite",
          toolRoot: "/Users/test",
        }),
      );
      expect(api.selectAgentWorkspaceRoot).not.toHaveBeenCalled();
    });

    it("refuses before any write when the node has no agent and names the next action", async () => {
      const api = harness([{ ...deployment, agents: [] }]);
      await reviewRoot("/Users/test/work");
      const alert = await screen.findByRole("alert");
      expect(alert).toHaveTextContent("this node has no agent");
      expect(alert).toHaveTextContent("add an agent to this node");
      expect(screen.getByRole("button", { name: "Keep current folder" })).toBeEnabled();
      expect(api.selectAgentWorkspaceRoot).not.toHaveBeenCalled();
      expect(api.restartManagedServer).not.toHaveBeenCalled();
    });

    it("does not publish when the restart keeps the current root", async () => {
      const api = harness([deployment]);
      await reviewRoot("/Users/test");
      await vi.waitFor(() =>
        expect(api.restartManagedServer).toHaveBeenCalledWith("Workshop Node", {
          toolCeiling: "readwrite",
          toolRoot: "/Users/test",
        }),
      );
      expect(api.selectAgentWorkspaceRoot).not.toHaveBeenCalled();
    });
  });
});
