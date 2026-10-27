import { acceptedChatReply } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  adoptNewChatFolder,
  loadChatFolders,
  recentFolders,
  withFolder,
} from "../src/hooks/chatFolders";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { createChatActions } from "../src/hooks/chatActions";
import { admittingProjection, shellStores } from "./shell-fixture";

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

  it("lists distinct folders, the last picked first", () => {
    let folders = withFolder({}, "s1", "/work/a");
    folders = withFolder(folders, "s2", "/work/b");
    folders = withFolder(folders, "s3", "/work/a");
    folders = adoptNewChatFolder(withFolder(folders, null, "/work/c"), "s4");
    expect(recentFolders(folders)).toEqual(["/work/c", "/work/a", "/work/b"]);
    expect(recentFolders(folders, 1)).toEqual(["/work/c"]);
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
});

describe("sending with a chat folder", () => {
  it("sends the new chat's folder as cwd and hands it to the session the send created", async () => {
    const sendChatMessage = vi.fn(acceptedChatReply);
    const stores = shellStores({
      deployments: [{ nodeDid: "agent", sessions: [], mailboxItems: [] }],
      selection: { nodeDid: "agent" },
    });
    const actions = createChatActions({
      api: { sendChatMessage } as unknown as DesktopApiAdapter,
      stores,
      project: () => admittingProjection(),
      refreshSession: vi.fn(),
      refreshSnapshot: vi.fn(),
      reportFailure: vi.fn(),
    });
    actions.setChatFolder("/work/notes");
    await actions.sendMessage("what is in todo.txt");
    expect(sendChatMessage).toHaveBeenCalledWith(
      expect.objectContaining({ cwd: "/work/notes" }),
    );
    expect(stores.chat.getState().folders).toMatchObject({
      [sendChatMessage.mock.calls[0][0].sessionId!]: "/work/notes",
    });
  });
});

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());
