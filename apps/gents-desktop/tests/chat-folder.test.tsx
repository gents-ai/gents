import { describe, expect, it, vi } from "vitest";
import {
  adoptNewChatFolder,
  loadChatFolders,
  withFolder,
} from "../src/hooks/useChatFolders";
import { createDesktopShellChatActions } from "../src/hooks/desktopShellChatActions";
import { folderLabel } from "../src/ui/screens/ChatFolderPicker";

describe("chat folders", () => {
  it("keeps a pick per chat and clears it with null", () => {
    let folders = withFolder({}, null, "/work/a");
    folders = withFolder(folders, "s1", "/work/b");
    expect(folders).toEqual({ "": "/work/a", s1: "/work/b" });
    expect(withFolder(folders, "s1", null)).toEqual({ "": "/work/a" });
  });

  it("hands the new chat's pick to the session its first send created", () => {
    expect(adoptNewChatFolder({ "": "/work/a", s1: "/x" }, "s2")).toEqual({
      s1: "/x",
      s2: "/work/a",
    });
    const none = { s1: "/x" };
    expect(adoptNewChatFolder(none, "s2")).toBe(none);
  });

  it("reads nothing from corrupt or missing storage", () => {
    const store = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => store.set(key, value),
    });
    try {
      expect(loadChatFolders()).toEqual({});
      store.set("gents.chatFolders", "{not json");
      expect(loadChatFolders()).toEqual({});
      store.set("gents.chatFolders", JSON.stringify({ a: "/x", b: 3 }));
      expect(loadChatFolders()).toEqual({ a: "/x" });
    } finally {
      vi.unstubAllGlobals();
    }
    expect(loadChatFolders()).toEqual({});
  });

  it("labels a folder by its last segment", () => {
    expect(folderLabel("/home/u/notes/")).toBe("notes");
    expect(folderLabel("C:\\Users\\u\\docs")).toBe("docs");
  });
});

describe("sending with a chat folder", () => {
  it("sends the folder as cwd and adopts the session", async () => {
    const sendChatMessage = vi.fn(async () => ({
      sessionId: "s9",
      requestId: "r1",
      agentDid: "agent",
      behaviorId: "coding",
    }));
    const adoptChatFolder = vi.fn();
    const actions = createDesktopShellChatActions({
      setLocalWorkflow: vi.fn(),
      setError: vi.fn(),
      setSending: vi.fn(),
      setOptimisticPendingTurn: vi.fn(),
      setSelectedSessionId: vi.fn(),
      setPendingMailboxCauseId: vi.fn(),
      setDraft: vi.fn(),
      submissionInFlight: { current: false },
      acceptsComposeIntent: () => true,
      captureComposeIntent: () => 0,
      newSessionAgentRef: { current: null },
      api: { sendChatMessage },
      chatFolder: "/work/notes",
      adoptChatFolder,
      selectedDeployment: { agentDid: "agent" },
      deployments: [],
      selectedSessionId: null,
      pendingMailboxCauseId: null,
      behaviorReadiness: { kind: "ready", behaviorId: "coding" },
      shellProjection: { nonEmptyContentSendStatus: { kind: "ready" } },
    } as never);
    await actions.submitContent("what is in todo.txt");
    expect(sendChatMessage).toHaveBeenCalledWith(
      expect.objectContaining({ cwd: "/work/notes" }),
    );
    expect(adoptChatFolder).toHaveBeenCalledWith("s9");
  });
});
