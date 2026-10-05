import { describe, expect, it, vi } from "vitest";
import {
  adoptNewChatFolder,
  loadChatFolders,
  withFolder,
} from "../src/hooks/chatFolders";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { createDesktopShellChatActions } from "../src/hooks/desktopShellChatActions";
import { admittingProjection, shellStores } from "./fleet-fixture";
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
  it("sends the new chat's folder as cwd and hands it to the session the send created", async () => {
    const sendChatMessage = vi.fn(async () => ({
      sessionId: "s9",
      requestId: "r1",
      agentDid: "agent",
      behaviorId: "coding",
    }));
    const stores = shellStores({
      deployments: [{ agentDid: "agent", sessions: [], mailboxItems: [] }],
      selection: { agentDid: "agent" },
    });
    const actions = createDesktopShellChatActions({
      api: { sendChatMessage } as unknown as DesktopApiAdapter,
      stores,
      project: () => admittingProjection(),
      refreshSession: vi.fn(),
      refreshSnapshot: vi.fn(),
      setError: vi.fn(),
    });
    actions.setChatFolder("/work/notes");
    await actions.submitContent("what is in todo.txt");
    expect(sendChatMessage).toHaveBeenCalledWith(
      expect.objectContaining({ cwd: "/work/notes" }),
    );
    expect(stores.chat.getState().folders).toEqual({ s9: "/work/notes" });
  });
});
