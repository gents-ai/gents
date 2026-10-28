import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type {
  DesktopClientSnapshot,
  EnrollmentRequestView,
} from "@source-inc/gents-desktop-client";
import { publishSnapshot, renderIn, testApp } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";

vi.mock("../src/lib/shellPlatform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../src/lib/shellPlatform")>()),
  supportsLocalManagedServer: () => false,
}));

const request: EnrollmentRequestView = {
  requestId: "request-phone",
  networkId: "network",
  adminDid: "did:key:admin",
  serverPeer: "peer-server",
  serverLabel: "My computer",
  ownerNode: "did:key:target",
  state: "pending_approval",
  expiresAt: "2030-01-01T00:00:00Z",
};
function snapshot(
  requests: EnrollmentRequestView[] | null,
  ready = false,
): DesktopClientSnapshot {
  return {
    bootstrap,
    client: {
      deployments: [
        deployment,
        {
          ...deployment,
          nodeDid: request.ownerNode,
          label: "My computer",
          source: "status",
          chatSafe: ready,
          dialSucceeded: false,
        },
      ],
      enrollmentRequests: requests,
    },
  } as DesktopClientSnapshot;
}

describe("remote onboarding", () => {
  it("waits for approval and the requested secure route, then completes directly on that node", async () => {
    const app = testApp({ snapshot: snapshot([]), supportsManagedServer: false });
    const enroll = vi
      .spyOn(app.actions, "requestStatusEnrollment")
      .mockResolvedValue(request);
    const config = vi.spyOn(app.actions, "changeConfig");
    const onDone = vi.fn();
    renderIn(app, <SetupScreen onDone={onDone} />);
    expect(screen.queryByText("Local node")).not.toBeInTheDocument();
    await userEvent.type(
      screen.getByRole("textbox"),
      "https://my-node.example:8787{enter}",
    );
    expect(enroll).toHaveBeenCalledWith("https://my-node.example:8787");
    expect(
      await screen.findByRole("heading", { name: "Waiting for access" }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
    expect(onDone).not.toHaveBeenCalled();
    act(() => publishSnapshot(app, snapshot([{ ...request, state: "approved" }])));
    expect(
      screen.getByRole("heading", { name: "Connecting to your node" }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
    const ready = snapshot([], true);
    act(() => publishSnapshot(app, ready));
    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(onDone).toHaveBeenCalledWith(ready, request.ownerNode);
    expect(config).not.toHaveBeenCalled();
    expect(screen.queryByText("Choose an inference provider")).not.toBeInTheDocument();
  });

  it("offers a saved pending request and never treats disappearance as approval", async () => {
    const app = testApp({
      snapshot: snapshot([request]),
      supportsManagedServer: false,
    });
    const onDone = vi.fn();
    renderIn(app, <SetupScreen onDone={onDone} />);
    await userEvent.click(
      screen.getByRole("button", { name: "View request to My computer" }),
    );
    expect(
      await screen.findByRole("heading", { name: "Waiting for access" }),
    ).toBeVisible();
    act(() => publishSnapshot(app, snapshot([])));
    expect(screen.getByText(/request is no longer listed/i)).toBeVisible();
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
    expect(onDone).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(screen.getByRole("button", { name: "Request access" })).toBeVisible();
  });

  it("offers a ready connection that arrives while the welcome screen is open", async () => {
    const app = testApp({ snapshot: snapshot([]), supportsManagedServer: false });
    const onDone = vi.fn();
    renderIn(app, <SetupScreen onDone={onDone} />);
    const ready = snapshot([], true);
    act(() => publishSnapshot(app, ready));
    await userEvent.click(
      screen.getByRole("button", { name: "Continue to My computer" }),
    );
    expect(onDone).toHaveBeenCalledWith(ready, request.ownerNode);
  });

  it("lets the user choose among saved requests instead of resuming the first ID", async () => {
    const other = {
      ...request,
      requestId: "a-first",
      ownerNode: "did:key:other",
      serverLabel: "Other computer",
    };
    const app = testApp({
      snapshot: snapshot([other, request]),
      supportsManagedServer: false,
    });
    renderIn(app, <SetupScreen onDone={vi.fn()} />);
    expect(screen.getByRole("heading", { name: "Connect to your node" })).toBeVisible();
    await userEvent.click(
      screen.getByRole("button", { name: "View request to My computer" }),
    );
    expect(screen.getByRole("heading", { name: "Waiting for access" })).toBeVisible();
    expect(screen.getByText("My computer", { exact: true })).toBeVisible();
  });

  it("shows the request expiry and distinguishes unreadable approval state", async () => {
    const app = testApp({
      snapshot: snapshot([request]),
      supportsManagedServer: false,
    });
    renderIn(app, <SetupScreen onDone={vi.fn()} />);
    await userEvent.click(
      screen.getByRole("button", { name: "View request to My computer" }),
    );
    expect(screen.getByText(/Request expires:/).querySelector("time")).toHaveAttribute(
      "datetime",
      request.expiresAt,
    );
    expect(screen.getByRole("status")).toHaveAttribute("aria-live", "polite");
    act(() => publishSnapshot(app, snapshot(null)));
    expect(screen.getByRole("heading", { name: "Checking access" })).toBeVisible();
    expect(
      screen.getByText("Approval status is unavailable. Refresh to check again."),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Refresh status" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
  });

  it("keeps a pending send on the address form without claiming the server is connected", async () => {
    const app = testApp({ snapshot: snapshot([]), supportsManagedServer: false });
    let finish!: (value: EnrollmentRequestView) => void;
    vi.spyOn(app.actions, "requestStatusEnrollment").mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    renderIn(app, <SetupScreen onDone={vi.fn()} />);
    await userEvent.type(screen.getByRole("textbox"), "https://my-node.example:8787");
    await userEvent.click(screen.getByRole("button", { name: "Request access" }));
    expect(screen.getByRole("heading", { name: "Connect to your node" })).toBeVisible();
    expect(screen.getByTestId("setup-next")).toBeDisabled();
    expect(screen.queryByText("Connect to server")).not.toBeInTheDocument();
    await act(async () => finish(request));
    expect(screen.getByRole("heading", { name: "Waiting for access" })).toBeVisible();
  });

  it("keeps enrollment errors retryable without configuring another node", async () => {
    const app = testApp({ snapshot: snapshot([]), supportsManagedServer: false });
    vi.spyOn(app.actions, "requestStatusEnrollment").mockImplementation(async () => {
      act(() => publishSnapshot(app, snapshot([request])));
      throw new Error("Address unavailable");
    });
    const onDone = vi.fn();
    renderIn(app, <SetupScreen onDone={onDone} />);
    await userEvent.type(screen.getByRole("textbox"), "https://my-node.example:8787");
    await userEvent.click(screen.getByRole("button", { name: "Request access" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Address unavailable");
    expect(screen.getByRole("button", { name: "Request access" })).toBeEnabled();
    expect(
      screen.getByRole("button", { name: "View request to My computer" }),
    ).toBeVisible();
    expect(screen.getByRole("heading", { name: "Connect to your node" })).toBeVisible();
    expect(onDone).not.toHaveBeenCalled();
  });
  it("keeps saved node choices disabled while submitting access", async () => {
    const app = testApp({
      snapshot: snapshot([request], true),
      supportsManagedServer: false,
    });
    let finish!: (value: EnrollmentRequestView) => void;
    vi.spyOn(app.actions, "requestStatusEnrollment").mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    renderIn(app, <SetupScreen onDone={vi.fn()} />);
    await userEvent.type(screen.getByRole("textbox"), "https://other.example");
    await userEvent.click(screen.getByRole("button", { name: "Request access" }));
    expect(
      screen.getByRole("button", { name: "View request to My computer" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Continue to My computer" }),
    ).toBeDisabled();
    await act(async () => finish(request));
  });
});
