import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { AppBody } from "../src/App";
import { writeSessionLoad } from "../src/hooks/sessionStore";
import { RepairBanner } from "../src/ui/app/RepairBanner";
import { DELETED_HOME_RECHECK_MS } from "../src/ui/screens/LoadingStatus";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import { node, renderIn, renderRouted, testApp } from "./app-fixture";

describe("repair banner", () => {
  it("shows_what_was_repaired", () => {
    const app = testApp();
    app.stores.client.setState({
      repairNotes: ["Moved the old folder aside.", "Started the node again."],
    });
    renderIn(app, <RepairBanner />);
    expect(screen.getByText("Gents repaired your local node")).toBeInTheDocument();
    expect(
      screen.getByText("Moved the old folder aside. Started the node again."),
    ).toBeInTheDocument();
  });

  it("dismiss_hides_it", async () => {
    const app = testApp();
    app.stores.client.setState({ repairNotes: ["Moved the old folder aside."] });
    renderIn(app, <RepairBanner />);
    await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(
      screen.queryByText("Gents repaired your local node"),
    ).not.toBeInTheDocument();
    expect(app.stores.client.getState().repairNotes).toEqual([]);
  });

  it("the_main_view_shows_it_when_the_app_is_ready", () => {
    const app = testApp({
      snapshot: {
        bootstrap: {
          ...bootstrap,
          initConfigStatus: "configured",
          savedPeers: [
            {
              peerId: "studio",
              label: "Studio",
              nodeDid: "did:key:studio",
              addr: "a",
              source: "enrollment",
              graphql: null,
            },
          ],
        },
        client: {
          clientDid: "test-client",
          deployments: [node({ nodeDid: "did:key:studio", source: "enrollment" })],
          operatorConfigAuthoritativeNodeDids: [bootstrap.initNodeDid],
        },
      },
    });
    app.stores.client.setState({
      startupPhase: "ready",
      repairNotes: ["Moved the old folder aside."],
    });
    renderRouted(app, { name: "sessions" }, () => <AppBody />);
    expect(screen.getByText("Gents repaired your local node")).toBeInTheDocument();
  });

  const removedHome = {
    ...bootstrap,
    initConfigStatus: "absent" as const,
    initNodeName: null,
    initNodeDid: null,
    nodeHomeExists: false,
  };
  const deadLocalPeer = {
    peerId: "34b9a22e",
    label: "Forge",
    nodeDid: "did:key:z6Mkme2nSRm7bykKyXQvfKPsZ68RAGr4HoXbTCzzh9esNrZg",
    addr: "endpointaa2l",
    source: "enrollment",
    graphql: "http://127.0.0.1:21919/api/v0/graphql",
    local: true,
  };
  const studioPeer = {
    peerId: "studio",
    label: "Studio",
    nodeDid: "did:key:studio",
    addr: "a",
    source: "enrollment",
    graphql: null,
    local: false,
  };
  const launch = async (
    savedPeers: unknown[],
    deployments: unknown[],
    localNodeLost: boolean,
  ) => {
    const app = testApp({
      api: {
        managedServerStatus: async () => ({ state: "disabled" }),
        repairManagedServer: async () => ({
          repaired: ["Stopped an old node that kept crashing."],
          outcome: localNodeLost ? "needsSetup" : "noLocalNode",
          blocker: null,
          savedPeers: [],
          localNodeLost,
        }),
      },
      supportsManagedServer: true,
      snapshot: {
        bootstrap: { ...removedHome, savedPeers },
        client: {
          clientDid: "test-client",
          deployments,
          operatorConfigAuthoritativeNodeDids: [],
        },
      },
    });
    await app.lifecycle.initializeDesktop();
    app.stores.client.setState({ startupPhase: "ready" });
    renderRouted(app, { name: "sessions" }, () => <AppBody />);
    return app;
  };

  it("a_rebuilt_home_shows_the_wizard_before_the_banner", async () => {
    const app = await launch([deadLocalPeer], [], true);
    expect(screen.getByTestId("setup-screen")).toBeInTheDocument();
    expect(
      screen.queryByText("Gents repaired your local node"),
    ).not.toBeInTheDocument();
    expect(app.stores.client.getState().repairNotes).toEqual([
      "Stopped an old node that kept crashing.",
    ]);
  });

  it("a_lost_local_node_shows_the_wizard_even_with_a_saved_remote", async () => {
    await launch(
      [studioPeer],
      [node({ nodeDid: "did:key:studio", label: "Studio", source: "enrollment" })],
      true,
    );
    expect(screen.getByTestId("setup-screen")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Continue with Studio" }),
    ).toBeInTheDocument();
  });

  it("finishing_the_wizard_opens_the_app_with_the_banner", async () => {
    await launch(
      [studioPeer],
      [node({ nodeDid: "did:key:studio", label: "Studio", source: "enrollment" })],
      true,
    );
    await userEvent.click(screen.getByRole("button", { name: "Continue with Studio" }));
    expect(screen.queryByTestId("setup-screen")).not.toBeInTheDocument();
    expect(screen.getByText("Gents repaired your local node")).toBeInTheDocument();
  });

  it("a_remote_only_desktop_opens_on_its_remote", async () => {
    await launch(
      [studioPeer],
      [node({ nodeDid: "did:key:studio", label: "Studio", source: "enrollment" })],
      false,
    );
    expect(screen.queryByTestId("setup-screen")).not.toBeInTheDocument();
  });

  const openLocalSessionThenDeleteItsFolder = (statusNamesIt: () => boolean) => {
    vi.stubGlobal(
      "IntersectionObserver",
      class {
        observe() {}
        disconnect() {}
      },
    );
    const local = node({
      label: "Forge",
      inferenceBackends: [
        { ...deployment.inferenceBackends[0], apiKeyConfigured: true },
      ],
    });
    const localPeer = { ...deadLocalPeer, nodeDid: local.nodeDid };
    const app = testApp({
      api: {
        managedServerStatus: async () => ({
          state: "failed",
          nodeName: "Forge",
          deletedHome: statusNamesIt() ? "/Users/me/.gents" : null,
        }),
        repairManagedServer: async () => ({
          repaired: [],
          outcome: "needsSetup",
          blocker: null,
          savedPeers: [],
          localNodeLost: true,
        }),
        fetchDesktopSnapshot: async () => ({
          bootstrap: { ...removedHome, savedPeers: [] },
          client: null,
        }),
      },
      supportsManagedServer: true,
      snapshot: {
        bootstrap: {
          ...bootstrap,
          initConfigStatus: "configured",
          savedPeers: [localPeer],
        },
        client: {
          clientDid: "test-client",
          deployments: [local],
          operatorConfigAuthoritativeNodeDids: [bootstrap.initNodeDid],
        },
      },
      selection: { nodeDid: local.nodeDid, sessionId: "s1" },
    });
    writeSessionLoad(app.stores.session, {
      phase: "failed",
      sessionId: "s1",
      nodeDid: local.nodeDid,
      found: null,
      error: "BridgeInvokeError: session transcript page",
      reread: false,
    });
    app.stores.client.setState({ startupPhase: "ready" });
    renderRouted(
      app,
      { name: "session", nodeDid: local.nodeDid, sessionId: "s1" },
      () => <AppBody />,
    );
    // The folder goes while the session is open: init.json no longer names
    // the node, and only its saved connection still marks it as this Mac's.
    act(() => {
      const snapshot = app.stores.client.getState().snapshot!;
      app.stores.client.setState({
        snapshot: {
          ...snapshot,
          bootstrap: { ...removedHome, savedPeers: [localPeer] },
        },
      });
    });
    return app;
  };

  it("a_folder_deleted_under_the_open_app_is_named_in_its_session_and_opens_setup", async () => {
    openLocalSessionThenDeleteItsFolder(() => true);
    expect(
      await screen.findByText("Your local node’s folder was deleted"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "/Users/me/.gents is gone, so Forge stopped. Its sessions can’t be loaded.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText("Couldn’t load this session")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Set up a new node" }));
    expect(await screen.findByTestId("setup-screen")).toBeInTheDocument();
    expect(
      screen.queryByText("Gents repaired your local node"),
    ).not.toBeInTheDocument();
  });

  it("a_failed_local_session_keeps_checking_until_its_folder_is_found_deleted", async () => {
    let found = false;
    openLocalSessionThenDeleteItsFolder(() => found);
    expect(await screen.findByText("Couldn’t load this session")).toBeInTheDocument();
    // A dead node changes no snapshot, so only the session's own re-check reads it.
    found = true;
    expect(
      await screen.findByText(
        "Your local node’s folder was deleted",
        {},
        { timeout: DELETED_HOME_RECHECK_MS * 2 },
      ),
    ).toBeInTheDocument();
  });

  it("nothing_repaired_shows_nothing", () => {
    renderIn(testApp(), <RepairBanner />);
    expect(
      screen.queryByText("Gents repaired your local node"),
    ).not.toBeInTheDocument();
  });
});
