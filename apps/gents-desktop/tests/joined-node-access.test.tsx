import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type {
  ClientAccessRequestView,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import { publishSnapshot, renderIn, testApp } from "./app-fixture";
import { AgentsScreen } from "../src/ui/screens/AgentsScreen";

const remote = "did:key:remote-runtime";
const address = "https://runtime.example/api/v0/graphql";
function snapshot(state: string, connected = false): DesktopClientSnapshot {
  return {
    bootstrap: {
      ...bootstrap,
      savedRuntimes: [
        { nodeDid: remote, label: "Shared node", graphql: address, local: false },
      ],
    },
    client: {
      clientDid: "did:key:client",
      syncHealth: {
        state: connected ? "healthy" : "blocked",
        cause: connected ? null : "runtime_refused",
        nextAction: null,
        runtimeCount: 1,
        connectedRuntimeCount: connected ? 1 : 0,
      },
      accessRequests: [
        {
          nodeDid: remote,
          clientDid: "did:key:client",
          requestId: "request-retained",
          state,
        },
      ],
      rowCount: 0,
      approxSerializedBytes: 0,
      deployments: [
        {
          ...deployment,
          nodeDid: remote,
          local: false,
          label: "Shared node",
          graphql: address,
          runtimeConnection: connected ? "connected" : "connecting",
          chatSafe: connected,
        },
      ],
      unkeptChanges: [],
    },
  };
}

describe("runtime access recovery", () => {
  it.each(["revoked", "rejected"])(
    "offers a new request for %s access even when the configured runtime has a placeholder row",
    async (state) => {
      const initial = snapshot(state);
      const app = testApp({ snapshot: initial });
      const requested: ClientAccessRequestView = {
        nodeDid: remote,
        clientDid: "did:key:client",
        requestId: "request-new",
        state: "pending",
      };
      const request = vi
        .spyOn(app.actions, "requestRuntimeAccess")
        .mockResolvedValue(requested);
      renderIn(app, <AgentsScreen />);
      expect(screen.getByTestId("pending-node")).toHaveTextContent(`Access ${state}`);
      await userEvent.click(
        screen.getByRole("button", { name: "Request access again" }),
      );
      expect(request).toHaveBeenCalledWith(address);
      act(() => publishSnapshot(app, snapshot("pending")));
      expect(screen.getByTestId("pending-node")).toHaveTextContent(
        "Waiting for Shared node to accept",
      );
      expect(
        screen.queryByRole("button", { name: "Request access again" }),
      ).not.toBeInTheDocument();
    },
  );

  it("offers a new request when native authority refuses an approved receipt", () => {
    const initial = snapshot("approved");
    initial.client!.deployments[0]!.runtimeConnection = "refused";
    const app = testApp({ snapshot: initial });
    renderIn(app, <AgentsScreen />);
    expect(screen.getByTestId("pending-node")).toHaveTextContent("Access refused");
    expect(screen.getByRole("button", { name: "Request access again" })).toBeVisible();
  });

  it("replaces the retained request with the runtime row only after the authenticated connection is ready", () => {
    const app = testApp({ snapshot: snapshot("approved") });
    renderIn(app, <AgentsScreen />);
    expect(screen.getByTestId("pending-node")).toHaveTextContent(
      "Access approved · connecting",
    );
    act(() => publishSnapshot(app, snapshot("approved", true)));
    expect(screen.queryByTestId("pending-node")).not.toBeInTheDocument();
    expect(screen.getByText("Shared node", { exact: true })).toBeVisible();
  });
});
