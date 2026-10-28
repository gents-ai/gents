// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

vi.mock("@source-inc/gents-desktop-client", async (importOriginal) => ({
  ...(await importOriginal<object>()),
  enrollmentActivationMessage: (
    await import("../../gents-desktop-client/src/enrollmentActivation.js")
  ).enrollmentActivationMessage,
}));

import { FleetDashboard } from "../src/components/FleetDashboard.js";

afterEach(cleanup);

it("shows the current activation failure and clears it after recovery", () => {
  const request = {
    requestId: "request",
    networkId: "network",
    adminDid: "admin",
    serverPeer: "peer",
    serverLabel: null,
    ownerNode: "node",
    state: "approved",
    expiresAt: "2099-01-01T00:00:00Z",
    activationError: "authentication",
  };
  const props = {
    addingPeer: false,
    bootstrap: null,
    deployments: [],
    loading: false,
    starting: false,
    onRequestStatusEnrollment: vi.fn(),
    onOpenChat: vi.fn(),
    onOpenConfig: vi.fn(),
  };
  const view = render(
    <FleetDashboard {...props} enrollmentRequests={[request]} />,
  );
  expect(screen.getByText(/could not authenticate the server/i)).toBeTruthy();
  expect(screen.queryByText(/finishing secure route/i)).toBeNull();
  view.rerender(
    <FleetDashboard
      {...props}
      enrollmentRequests={[{ ...request, activationError: "route" }]}
    />,
  );
  expect(
    screen.getByText(/could not finish the secure connection/i),
  ).toBeTruthy();
  view.rerender(
    <FleetDashboard
      {...props}
      enrollmentRequests={[{ ...request, activationError: undefined }]}
    />,
  );
  expect(screen.getByText(/finishing secure route/i)).toBeTruthy();
  expect(screen.queryByText(/needs attention/i)).toBeNull();
});
