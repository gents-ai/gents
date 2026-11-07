/* A UX plugin's page in a window of its own: the address the shell opens
   it at, the page as the window's only content, the node it was opened
   from, closing when the plugin is gone, and every window following a
   toggle or a pack change made in another. */
import {
  act,
  fireEvent,
  render,
  renderHook,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

import App from "../src/App";
import { createUxContext } from "@/contrib/plugin";
import {
  followOtherWindowsDecisions,
  publishUxPlugin,
  patchUxPlugin,
  resetUxPluginStore,
  uxPluginRecords,
} from "@/contrib/plugins-store";
import {
  configureUxDoors,
  followOtherWindowsPackChanges,
  refreshRuntimePackUxPlugins,
  resetRuntimeDoor,
} from "@/contrib/runtime-door";
import { PLUGIN_PAGE_AREA } from "@/contrib/types";
import { pluginWindowStart, useClosesWhenPluginGone } from "@/app/pluginWindow";
import { resetUxDoors } from "@/hooks/useUxHost";
import { PluginPageScreen } from "@/screens/PluginPageScreen";
import { renderIn, testApp } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const invoke = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

/* the main window, the plugin windows open beside it, and what it's told */
const shell = vi.hoisted(() => {
  const listeners = new Map<string, (event: { payload: unknown }) => void>();
  const open = new Set<string>();
  const closed = vi.fn();
  return {
    open,
    closed,
    tell: (event: string, payload: unknown) => listeners.get(event)?.({ payload }),
    reset: () => {
      listeners.clear();
      open.clear();
      closed.mockClear();
    },
    module: {
      getCurrentWindow: () => ({
        label: "main",
        listen: async (event: string, heard: (event: { payload: unknown }) => void) => {
          listeners.set(event, heard);
          return () => listeners.delete(event);
        },
      }),
      Window: {
        getByLabel: async (label: string) =>
          open.has(label) ? { close: async () => closed(label) } : null,
      },
    },
  };
});
vi.mock("@tauri-apps/api/window", () => shell.module);

const disposers: Array<() => void> = [];
afterEach(() => {
  disposers.splice(0).forEach((dispose) => dispose());
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  resetUxPluginStore();
  resetRuntimeDoor();
  resetUxDoors();
  invoke.mockClear();
  shell.reset();
  vi.restoreAllMocks();
});

function registerBoard() {
  const board = createUxContext("board", { onDispose: (d) => disposers.push(d) });
  board.register({
    id: "page",
    area: PLUGIN_PAGE_AREA,
    render: () => <p data-testid="board-page">the board</p>,
  });
}

describe("the address a plugin window opens at", () => {
  it("names the plugin and, when given, the node", () => {
    expect(
      pluginWindowStart(
        "?view=plugin&plugin=acme%2Fboard+%26+co&node=did%3Akey%3Az6Mk",
      ),
    ).toEqual({ pluginId: "acme/board & co", nodeDid: "did:key:z6Mk" });
    expect(pluginWindowStart("?view=plugin&plugin=board")).toEqual({
      pluginId: "board",
      nodeDid: null,
    });
  });

  it("is any other window without a plugin view", () => {
    expect(pluginWindowStart("")).toBeNull();
    expect(pluginWindowStart("?plugin=board")).toBeNull();
    expect(pluginWindowStart("?view=plugin")).toBeNull();
    expect(pluginWindowStart("?view=plugin&plugin=")).toBeNull();
  });
});

function running(): DesktopClientSnapshot {
  return {
    bootstrap: { ...bootstrap, initConfigStatus: "configured" },
    client: {
      clientDid: "did:key:plugin-window",
      localPeerId: "local-peer",
      listenAddresses: [],
      p2pHealth: null,
      syncHealth: null,
      enrollmentRequests: [],
      bootstrapErrors: [],
      lastMutationError: null,
      focusedRequestId: null,
      configuredPeerCount: 1,
      dialedPeerCount: 1,
      peerIssueCount: 0,
      rowCount: 1,
      approxSerializedBytes: 1,
      operatorConfigAuthoritativeNodeDids: [deployment.nodeDid],
      deployments: [deployment],
    },
  } as unknown as DesktopClientSnapshot;
}

describe("a plugin window", () => {
  it("shows only the page, with its data going to the node it was opened from", async () => {
    registerBoard();
    const api = {
      fetchDesktopSnapshot: vi.fn(async () => running()),
      startDesktopClient: vi.fn(async () => running()),
      fetchSessionSnapshot: vi.fn(async () => null),
      setSelectedNode: vi.fn(async () => undefined),
    } as unknown as DesktopApiAdapter;
    render(
      <App
        bridge={{
          api,
          listenToUpdates: async () => () => {},
          supportsManagedServer: false,
        }}
        start={{ pluginId: "board", nodeDid: deployment.nodeDid }}
      />,
    );

    expect(await screen.findByTestId("board-page")).toHaveTextContent("the board");
    expect(screen.getByTestId("plugin-window")).toBeInTheDocument();
    expect(screen.queryByTestId("app-shell")).toBeNull();
    expect(screen.queryByTestId("setup-screen")).toBeNull();
    expect(screen.queryByTestId("plugin-page-window")).toBeNull();
    await waitFor(() =>
      expect(api.setSelectedNode).toHaveBeenLastCalledWith(deployment.nodeDid),
    );
  });

  it("closes once its plugin, having run, is turned off, but not one that never ran", async () => {
    const close = vi.fn();
    const record = { id: "board", name: "Board", door: "pack" as const };
    const { rerender } = renderHook(({ id }) => useClosesWhenPluginGone(id, close), {
      initialProps: { id: "board" },
    });
    act(() => publishUxPlugin({ ...record, status: "disabled" }));
    await act(async () => {});
    expect(close).not.toHaveBeenCalled();

    act(() => patchUxPlugin("board", { status: "loaded" }));
    rerender({ id: "board" });
    act(() => patchUxPlugin("board", { status: "disabled" }));
    await waitFor(() => expect(close).toHaveBeenCalledTimes(1));
  });
});

describe("the page's open-in-window action", () => {
  it("opens the page in a window of its own, on the selected node", () => {
    registerBoard();
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    publishUxPlugin({ id: "board", name: "Board", door: "pack", status: "loaded" });
    const app = testApp({ deployments: [deployment] });
    renderIn(app, <PluginPageScreen pluginId="board" />);

    fireEvent.click(screen.getByTestId("plugin-page-window"));
    expect(invoke).toHaveBeenCalledWith("desktop_open_plugin_window", {
      pluginId: "board",
      nodeDid: deployment.nodeDid,
      title: "Board",
    });
  });

  it("is offered only in the desktop app, and not inside the window it opened", () => {
    registerBoard();
    const app = testApp({ deployments: [deployment] });
    const view = renderIn(app, <PluginPageScreen pluginId="board" />);
    expect(screen.queryByTestId("plugin-page-window")).toBeNull();

    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    view.rerender(<PluginPageScreen pluginId="board" windowed />);
    expect(screen.queryByTestId("plugin-page-window")).toBeNull();
  });
});

describe("the page while it has a window of its own", () => {
  const BOARD_WINDOW = "gents-plugin-626f617264";
  const told = (pluginId: string, open: boolean) =>
    act(() => shell.tell("desktop://plugin-window", { pluginId, open }));

  function renderBoard(windowed = false) {
    registerBoard();
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    publishUxPlugin({ id: "board", name: "Board", door: "pack", status: "loaded" });
    const app = testApp({ deployments: [deployment] });
    return renderIn(app, <PluginPageScreen pluginId="board" windowed={windowed} />);
  }

  it("stands aside while that window is open, and comes back when it closes", async () => {
    shell.open.add(BOARD_WINDOW);
    renderBoard();
    expect(await screen.findByTestId("plugin-page-elsewhere")).toHaveTextContent(
      "Board is open in its own window",
    );
    expect(screen.queryByTestId("board-page")).toBeNull();

    told("other", false);
    expect(screen.getByTestId("plugin-page-elsewhere")).toBeInTheDocument();
    told("board", false);
    expect(screen.getByTestId("board-page")).toBeInTheDocument();
    told("board", true);
    expect(screen.getByTestId("plugin-page-elsewhere")).toBeInTheDocument();
  });

  it("brings that window forward, or closes it to show the page here", async () => {
    shell.open.add(BOARD_WINDOW);
    renderBoard();
    fireEvent.click(await screen.findByRole("button", { name: "Bring it forward" }));
    expect(invoke).toHaveBeenCalledWith("desktop_open_plugin_window", {
      pluginId: "board",
      nodeDid: deployment.nodeDid,
      title: "Board",
    });
    fireEvent.click(screen.getByRole("button", { name: "Show it here" }));
    await waitFor(() => expect(shell.closed).toHaveBeenCalledWith(BOARD_WINDOW));
  });

  it("stays the page inside that window", async () => {
    shell.open.add(BOARD_WINDOW);
    renderBoard(true);
    await act(async () => {});
    expect(screen.getByTestId("board-page")).toBeInTheDocument();
    expect(screen.queryByTestId("plugin-page-elsewhere")).toBeNull();
  });
});

describe("following other windows", () => {
  const storage = (key: string, value: unknown) =>
    window.dispatchEvent(
      new StorageEvent("storage", { key, newValue: JSON.stringify(value) }),
    );

  it("applies a plugin turned off or on in another window", () => {
    const activate = vi.fn(() => patchUxPlugin("board", { status: "loaded" }));
    const deactivate = vi.fn();
    publishUxPlugin(
      { id: "board", name: "Board", door: "pack", status: "loaded" },
      { activate, deactivate },
    );
    disposers.push(followOtherWindowsDecisions());

    act(() => storage("gents.ux-decisions.v1", { board: false }));
    expect(deactivate).toHaveBeenCalledTimes(1);
    expect(uxPluginRecords().board?.status).toBe("disabled");

    act(() => storage("gents.ux-decisions.v1", { board: true }));
    expect(activate).toHaveBeenCalledTimes(1);
    expect(uxPluginRecords().board?.status).toBe("loaded");
  });

  it("reloads a changed pack's pages here and in the other windows", async () => {
    const list = vi.fn(async () => ({ plugins: [] }));
    configureUxDoors({ list, source: vi.fn() });
    disposers.push(followOtherWindowsPackChanges());

    await refreshRuntimePackUxPlugins("acme/tools");
    expect(list).toHaveBeenCalledTimes(1);
    expect(
      JSON.parse(window.localStorage.getItem("gents.ux-plugins-changed.v1")!),
    ).toMatchObject({ pack: "acme/tools" });

    storage("gents.ux-plugins-changed.v1", { pack: "acme/tools", at: 1 });
    await waitFor(() => expect(list).toHaveBeenCalledTimes(2));
  });
});
