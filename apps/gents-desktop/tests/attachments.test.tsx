import { acceptedChatReply } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { webcrypto, randomUUID } from "node:crypto";
import { MemoryNavProvider } from "@gents/shell";
import { SessionScreen } from "../src/ui/screens/SessionScreen";
import { SentAttachments } from "../src/ui/screens/session/attachments";
import { node, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";
import {
  contentKey,
  unresolvedSubmissionFor,
  rememberUnresolvedSubmission,
} from "../src/hooks/unresolvedSubmissions";

vi.mock("../src/lib/nativeShell", async (importOriginal) => ({
  ...(await importOriginal<object>()),
  inNativeShell: () => false,
  listenFileDrop: async () => () => {},
}));
vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));
vi.mock("../src/ui/screens/BehaviorPicker", () => ({ AgentPicker: () => null }));
vi.stubGlobal(
  "IntersectionObserver",
  class {
    observe() {}
    disconnect() {}
  },
);

const PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aWQAAAABJRU5ErkJggg==";
const attached = { name: "pixel.png", mediaType: "image/png", data: PNG };
function file(read?: () => Promise<ArrayBuffer>) {
  const value = new File(
    [Uint8Array.from(atob(PNG), (c) => c.charCodeAt(0))],
    "pixel.png",
    { type: "image/png" },
  );
  Object.defineProperty(value, "arrayBuffer", {
    value:
      read ?? (async () => Uint8Array.from(atob(PNG), (c) => c.charCodeAt(0)).buffer),
  });
  return value;
}
function setup() {
  const sendChatMessage = vi.fn(acceptedChatReply);
  const app = testApp({
    api: {
      sendChatMessage,
      fetchSessionSnapshot: vi.fn().mockResolvedValue(null),
      sessionProvenance: vi.fn().mockResolvedValue(null),
    },
    deployments: [node({})],
    selection: { nodeDid: deployment.nodeDid, sessionId: null },
  });
  render(
    <MemoryNavProvider initial={{ name: "session", sessionId: null }}>
      <SessionScreen />
    </MemoryNavProvider>,
    { wrapper: withApp(app) },
  );
  return { app, sendChatMessage };
}
beforeEach(() => {
  vi.stubGlobal("localStorage", goalDeliveryStorage());
  vi.stubGlobal("crypto", { subtle: webcrypto.subtle, randomUUID });
  URL.createObjectURL = vi.fn(() => "blob:preview");
  URL.revokeObjectURL = vi.fn();
});

describe("desktop attachments", () => {
  it("sends an image with no text through the real composer and clears only the accepted attachments", async () => {
    const { sendChatMessage } = setup();
    fireEvent.change(screen.getByLabelText("Choose attachments"), {
      target: { files: [file()] },
    });
    await waitFor(() =>
      expect(
        within(screen.getByTestId("composer")).getByRole("button", { name: "Send" }),
      ).toBeEnabled(),
    );
    expect(screen.getByTestId("attachment-tray")).toHaveTextContent("pixel.png");
    fireEvent.click(
      within(screen.getByTestId("composer")).getByRole("button", { name: "Send" }),
    );
    await waitFor(() =>
      expect(sendChatMessage).toHaveBeenCalledWith(
        expect.objectContaining({ content: "", attachments: [attached] }),
      ),
    );
    await waitFor(() =>
      expect(screen.queryByTestId("attachment-tray")).not.toBeInTheDocument(),
    );
  });

  it("shows a pre-submission failure inline and keeps the selected image for an explicit retry", async () => {
    const { sendChatMessage } = setup();
    sendChatMessage.mockRejectedValueOnce(
      new Error("This runtime does not support attachment input"),
    );
    fireEvent.change(screen.getByLabelText("Choose attachments"), {
      target: { files: [file()] },
    });
    await waitFor(() =>
      expect(
        within(screen.getByTestId("composer")).getByRole("button", { name: "Send" }),
      ).toBeEnabled(),
    );
    fireEvent.click(
      within(screen.getByTestId("composer")).getByRole("button", { name: "Send" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "does not support attachment input",
      ),
    );
    expect(screen.getByTestId("attachment-tray")).toHaveTextContent("pixel.png");
    expect(sendChatMessage).toHaveBeenCalledTimes(1);
  });

  it("does not resurrect an attachment removed while its file is still loading", async () => {
    let resolve!: (value: ArrayBuffer) => void;
    setup();
    fireEvent.change(screen.getByLabelText("Choose attachments"), {
      target: {
        files: [
          file(
            () =>
              new Promise((done) => {
                resolve = done;
              }),
          ),
        ],
      },
    });
    fireEvent.click(screen.getByRole("button", { name: "Remove pixel.png" }));
    await act(async () => {
      resolve(Uint8Array.from(atob(PNG), (c) => c.charCodeAt(0)).buffer);
    });
    expect(screen.queryByTestId("attachment-tray")).not.toBeInTheDocument();
    expect(
      within(screen.getByTestId("composer")).getByRole("button", { name: "Send" }),
    ).toBeDisabled();
  });

  it("keeps a late file read in its originating draft after changing sessions", async () => {
    let resolve!: (value: ArrayBuffer) => void;
    const { app } = setup();
    const key = app.project().draftKey;
    fireEvent.change(screen.getByLabelText("Choose attachments"), {
      target: {
        files: [
          file(
            () =>
              new Promise((done) => {
                resolve = done;
              }),
          ),
        ],
      },
    });
    act(() => app.stores.selection.setState({ sessionId: "another-session" }));
    await act(async () => {
      resolve(Uint8Array.from(atob(PNG), (c) => c.charCodeAt(0)).buffer);
    });
    await waitFor(() =>
      expect(app.drafts.getState().attachments[key]?.[0]?.content).toEqual(attached),
    );
    expect(app.drafts.getState().attachments[app.project().draftKey] ?? []).toEqual([]);
  });

  it("keeps rich clipboard text editable instead of attaching its image representation", () => {
    setup();
    fireEvent.paste(screen.getByRole("textbox"), {
      clipboardData: {
        files: [file()],
        types: ["text/plain", "Files"],
        getData: () => "copied text",
      },
    });
    expect(screen.queryByTestId("attachment-tray")).not.toBeInTheDocument();
  });

  it("does not reconcile changed images under an earlier text-only fingerprint", () => {
    rememberUnresolvedSubmission({
      nodeDid: "node",
      sessionId: "session",
      selectedSessionId: "session",
      requestId: "original",
      contentKey: contentKey("", [attached]),
    });
    expect(unresolvedSubmissionFor("node", "session", "", [attached])?.requestId).toBe(
      "original",
    );
    expect(
      unresolvedSubmissionFor("node", "session", "", [
        { ...attached, data: PNG + "AAAA" },
      ]),
    ).toBeUndefined();
  });

  it("loads forked-message previews without a request ID and can retry a failed read", async () => {
    vi.stubGlobal(
      "IntersectionObserver",
      class {
        constructor(
          private callback: (entries: { isIntersecting: boolean }[]) => void,
        ) {}
        observe() {
          this.callback([{ isIntersecting: true }]);
        }
        disconnect() {}
      },
    );
    const requestAttachments = vi
      .fn()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue([attached]);
    const app = testApp({
      api: { requestAttachments },
      deployments: [node({})],
      selection: { nodeDid: deployment.nodeDid, sessionId: "fork-session" },
    });
    render(
      <SentAttachments
        items={[
          {
            name: attached.name,
            mediaType: attached.mediaType,
            digest: "image",
            size: 64,
          },
        ]}
        messageKey="forked-message"
      />,
      { wrapper: withApp(app) },
    );
    fireEvent.click(await screen.findByRole("button", { name: "Retry preview" }));
    await waitFor(() =>
      expect(screen.getByRole("img", { name: attached.name })).toHaveAttribute(
        "src",
        "blob:preview",
      ),
    );
    expect(requestAttachments).toHaveBeenLastCalledWith(
      deployment.nodeDid,
      "fork-session",
      "",
      "forked-message",
    );
  });
});
