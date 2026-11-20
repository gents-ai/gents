import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type {
  DesktopClientSnapshot,
  ClientAccessRequestView,
} from "@source-inc/gents-desktop-client";
import { publishSnapshot, renderIn, testApp } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import { SetupScreen } from "../src/ui/screens/setup/SetupScreen";
import { AgentsScreen } from "../src/ui/screens/AgentsScreen";

vi.mock("../src/lib/shellPlatform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../src/lib/shellPlatform")>()),
  supportsLocalManagedServer: () => false,
}));

const request: ClientAccessRequestView = {
  requestId: "request-phone",
  nodeDid: "did:key:target",
  clientDid: "did:key:client",
  state: "pending",
};
function snapshot(
  requests: ClientAccessRequestView[] | null,
  ready = false,
): DesktopClientSnapshot {
  return {
    bootstrap,
    client: {
      deployments: [
        deployment,
        {
          ...deployment,
          nodeDid: request.nodeDid,
          label: "My computer",
          local: false,
          chatSafe: ready,
          runtimeConnection: ready ? "connected" : "connecting",
        },
      ],
      accessRequests: requests,
    },
  } as DesktopClientSnapshot;
}

describe("remote onboarding", () => {
  it("requests access at the supplied server address", async () => {
    const app = testApp({ snapshot: snapshot([]), supportsManagedServer: false });
    const enroll = vi
      .spyOn(app.actions, "requestRuntimeAccess")
      .mockResolvedValue(request);
    renderIn(app, <SetupScreen onDone={vi.fn()} />);
    const invite = "https://my-node.example:8787";
    await userEvent.type(
      screen.getByRole("textbox", { name: "Server address" }),
      invite,
    );
    await userEvent.click(screen.getByRole("button", { name: "Request access" }));
    expect(enroll).toHaveBeenCalledWith(invite);
    expect(
      await screen.findByRole("heading", { name: "Waiting for access" }),
    ).toBeVisible();
  });

  it("waits for approval and the requested secure route, then completes directly on that node", async () => {
    const app = testApp({ snapshot: snapshot([]), supportsManagedServer: false });
    const enroll = vi
      .spyOn(app.actions, "requestRuntimeAccess")
      .mockResolvedValue(request);
    const config = vi.spyOn(app.actions, "changeConfig");
    const onDone = vi.fn();
    renderIn(app, <SetupScreen onDone={onDone} />);
    expect(screen.queryByText("Local node")).not.toBeInTheDocument();
    await userEvent.type(
      screen.getByRole("textbox", { name: "Server address" }),
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
    expect(onDone).toHaveBeenCalledWith(ready, request.nodeDid);
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
      screen.getByRole("button", { name: "View request to did:key:target" }),
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
      screen.getByRole("button", { name: "Continue with My computer" }),
    );
    expect(onDone).toHaveBeenCalledWith(ready, request.nodeDid);
  });

  it("lets the user choose among saved requests instead of resuming the first ID", async () => {
    const other = {
      ...request,
      requestId: "a-first",
      nodeDid: "did:key:other",
    };
    const app = testApp({
      snapshot: snapshot([other, request]),
      supportsManagedServer: false,
    });
    renderIn(app, <SetupScreen onDone={vi.fn()} />);
    expect(screen.getByRole("heading", { name: "Connect to your node" })).toBeVisible();
    await userEvent.click(
      screen.getByRole("button", { name: "View request to did:key:target" }),
    );
    expect(screen.getByRole("heading", { name: "Waiting for access" })).toBeVisible();
    expect(screen.getByText("My computer", { exact: true })).toBeVisible();
  });

  it("distinguishes unreadable approval state", async () => {
    const app = testApp({
      snapshot: snapshot([request]),
      supportsManagedServer: false,
    });
    renderIn(app, <SetupScreen onDone={vi.fn()} />);
    await userEvent.click(
      screen.getByRole("button", { name: "View request to did:key:target" }),
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
    let finish!: (value: ClientAccessRequestView) => void;
    vi.spyOn(app.actions, "requestRuntimeAccess").mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    renderIn(app, <SetupScreen onDone={vi.fn()} />);
    await userEvent.type(
      screen.getByRole("textbox", { name: "Server address" }),
      "https://my-node.example:8787",
    );
    await userEvent.click(screen.getByRole("button", { name: "Request access" }));
    expect(screen.getByRole("heading", { name: "Connect to your node" })).toBeVisible();
    expect(screen.getByTestId("setup-next")).toBeDisabled();
    expect(screen.queryByText("Connect to server")).not.toBeInTheDocument();
    await act(async () => finish(request));
    expect(screen.getByRole("heading", { name: "Waiting for access" })).toBeVisible();
  });

  it("keeps enrollment errors retryable without configuring another node", async () => {
    const app = testApp({ snapshot: snapshot([]), supportsManagedServer: false });
    vi.spyOn(app.actions, "requestRuntimeAccess").mockImplementation(async () => {
      act(() => publishSnapshot(app, snapshot([request])));
      throw new Error("Address unavailable");
    });
    const onDone = vi.fn();
    renderIn(app, <SetupScreen onDone={onDone} />);
    await userEvent.type(
      screen.getByRole("textbox", { name: "Server address" }),
      "https://my-node.example:8787",
    );
    await userEvent.click(screen.getByRole("button", { name: "Request access" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Address unavailable");
    expect(screen.getByRole("button", { name: "Request access" })).toBeEnabled();
    expect(
      screen.getByRole("button", { name: "View request to did:key:target" }),
    ).toBeVisible();
    expect(screen.getByRole("heading", { name: "Connect to your node" })).toBeVisible();
    expect(onDone).not.toHaveBeenCalled();
  });
  it("keeps saved node choices disabled while submitting access", async () => {
    const app = testApp({
      snapshot: snapshot([request], true),
      supportsManagedServer: false,
    });
    let finish!: (value: ClientAccessRequestView) => void;
    vi.spyOn(app.actions, "requestRuntimeAccess").mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    renderIn(app, <SetupScreen onDone={vi.fn()} />);
    await userEvent.type(
      screen.getByRole("textbox", { name: "Server address" }),
      "https://other.example",
    );
    await userEvent.click(screen.getByRole("button", { name: "Request access" }));
    expect(
      screen.getByRole("button", { name: "View request to did:key:target" }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Continue with My computer" }),
    ).toBeDisabled();
    await act(async () => finish(request));
  });
  it("lists a waiting request among the remote nodes, until its node takes its place", () => {
    const waiting = snapshot([request]);
    waiting.client!.deployments = [deployment];
    const app = testApp({ snapshot: waiting, supportsManagedServer: false });
    renderIn(app, <AgentsScreen />);
    const remote = screen
      .getByRole("heading", { name: /Remote nodes/ })
      .closest("section")!;
    const row = within(remote).getByTestId("pending-node");
    expect(row).toHaveTextContent(request.nodeDid);
    expect(row).toHaveTextContent(`Waiting for ${request.nodeDid} to accept`);
    /* a request on its way is not "nothing paired" */
    expect(screen.queryByText("No remote nodes paired.")).not.toBeInTheDocument();

    /* once the node is listed, its own row stands in the request's place */
    act(() =>
      publishSnapshot(app, snapshot([{ ...request, state: "approved" }], true)),
    );
    expect(screen.queryByTestId("pending-node")).not.toBeInTheDocument();
    expect(within(remote).getByText("My computer")).toBeVisible();
  });
});
