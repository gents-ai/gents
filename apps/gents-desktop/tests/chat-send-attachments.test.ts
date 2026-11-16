import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
  RequestAttachment,
} from "@source-inc/gents-desktop-client";
import { createChatDelivery } from "../src/hooks/chatDelivery";
import {
  chatDraftKey,
  chatDraftPayload,
  chatDraftRequest,
  loadChatDrafts,
  pruneChatDraftAttachments,
} from "../src/hooks/chatDrafts";
import { sendStatus } from "../src/hooks/chatDeliveryView";
import { createChatStore } from "../src/hooks/chatStore";
import type { SendAttachmentStore } from "../src/hooks/sendAttachments";
import { shellStores } from "./shell-fixture";
import { acceptedChat } from "./chat-delivery-fixture";
import { deferred } from "./deferred";
import { memorySendAttachments } from "./send-attachments-fixture";

const QUOTA = 5 * 1024 * 1024;
function webkitLocalStorage() {
  const entries = new Map<string, string>();
  const used = () =>
    [...entries].reduce((total, [key, value]) => total + key.length + value.length, 0);
  return {
    get length() {
      return entries.size;
    },
    key: (index: number) => [...entries.keys()][index] ?? null,
    getItem: (key: string) => entries.get(key) ?? null,
    setItem: (key: string, value: string) => {
      const previous = entries.get(key);
      if (
        used() -
          (previous ? key.length + previous.length : 0) +
          key.length +
          value.length >
        QUOTA
      )
        throw new DOMException("The quota has been exceeded.", "QuotaExceededError");
      entries.set(key, value);
    },
    removeItem: (key: string) => {
      entries.delete(key);
    },
    clear: () => entries.clear(),
  };
}

const LARGE: RequestAttachment = {
  name: "scan.pdf",
  mediaType: "application/pdf",
  data: "QUFB".repeat(1_398_101) + "QQ==",
};
const SECOND: RequestAttachment = {
  name: "notes.txt",
  mediaType: "text/plain",
  data: "bm90ZXM=",
};

beforeEach(() => vi.stubGlobal("localStorage", webkitLocalStorage()));
afterEach(() => vi.unstubAllGlobals());

function fixture(kept: SendAttachmentStore = memorySendAttachments()) {
  const stores = shellStores({
    deployments: [{ nodeDid: "node", sessions: [], mailboxItems: [] }],
    selection: { nodeDid: "node", sessionId: "session" },
  });
  stores.client.setState({
    snapshot: { client: { clientDid: "client" } } as DesktopClientSnapshot,
  });
  const send = vi
    .fn<(request: ChatSendRequest) => Promise<ChatSendResult>>()
    .mockRejectedValue(new Error("Outer reply was lost"));
  const delivery = () =>
    createChatDelivery(
      { sendChatMessage: send } as unknown as DesktopApiAdapter,
      stores,
      kept,
    );
  const submit = () =>
    delivery().submit(
      {
        clientDid: "client",
        nodeDid: "node",
        sessionId: "session",
        content: "Two files",
        attachments: [LARGE, SECOND],
      },
      "session",
      false,
      async () => true,
    );
  return { stores, kept, send, delivery, submit };
}

it("keeps large attachments out of localStorage and resends them after a restart", async () => {
  const f = fixture();
  await expect(f.submit()).rejects.toThrow("Outer reply was lost");
  const request = f.send.mock.calls[0][0];
  expect(request.attachments).toEqual([LARGE, SECOND]);
  const [failed] = loadChatDrafts().chatDrafts;
  const entry = localStorage.getItem(chatDraftKey(failed))!;
  expect(entry.length).toBeLessThan(2_000);
  expect(chatDraftRequest(failed).attachments).toEqual([
    expect.objectContaining({ name: "scan.pdf", size: 4 * 1024 * 1024 }),
    expect.objectContaining({ name: "notes.txt", size: 5 }),
  ]);
  f.stores.chat = createChatStore();
  expect(f.stores.chat.getState().chatDrafts).toEqual([failed]);
  f.send.mockImplementationOnce(async (original) => acceptedChat(original));
  await f.delivery().retry(failed.requestId, async () => true);
  expect(f.send.mock.calls[1][0]).toEqual(request);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  expect(await f.kept.get(chatDraftKey(failed))).toBeUndefined();
});

it("drops the kept bytes when a failed send is discarded", async () => {
  const f = fixture();
  await expect(f.submit()).rejects.toThrow();
  const [failed] = loadChatDrafts().chatDrafts;
  expect(await f.kept.get(chatDraftKey(failed))).toHaveLength(2);
  const handed = await f.delivery().discard(failed.requestId);
  expect(handed?.attachments).toEqual([LARGE, SECOND]);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  expect(await f.kept.get(chatDraftKey(failed))).toBeUndefined();
});

it("refuses a send whose attachments cannot be kept, before saving or dispatching it", async () => {
  const f = fixture({
    ...memorySendAttachments(),
    put: async () => {
      throw new Error("This window has no IndexedDB.");
    },
  });
  await expect(f.submit()).rejects.toThrow(
    "The attachments could not be saved on this device, so the message was not sent: This window has no IndexedDB.",
  );
  expect(f.send).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts).toEqual([]);
});

it("fails a retry clearly when the kept bytes are gone or changed, without sending", async () => {
  const f = fixture();
  await expect(f.submit()).rejects.toThrow();
  const [failed] = loadChatDrafts().chatDrafts;
  await f.kept.put(chatDraftKey(failed), [SECOND.data, SECOND.data]);
  await expect(f.delivery().retry(failed.requestId, async () => true)).rejects.toThrow(
    /attachments are no longer saved on this device/,
  );
  await f.kept.delete(chatDraftKey(failed));
  await expect(f.delivery().retry(failed.requestId, async () => true)).rejects.toThrow(
    /attachments are no longer saved on this device/,
  );
  expect(f.send).toHaveBeenCalledTimes(1);
  const [still] = loadChatDrafts().chatDrafts;
  expect(still.failure?.message).toMatch(/Discard it and attach the files again/);
  const handed = await f.delivery().discard(still.requestId);
  expect(handed).toMatchObject({ content: "Two files", attachments: undefined });
});

it("stores no bytes for a send whose draft cannot be saved", async () => {
  const f = fixture();
  const put = vi.spyOn(f.kept, "put");
  vi.spyOn(localStorage, "setItem").mockImplementationOnce(() => {
    throw new DOMException("The quota has been exceeded.", "QuotaExceededError");
  });
  await expect(f.submit()).rejects.toThrow("The quota has been exceeded.");
  expect(f.send).not.toHaveBeenCalled();
  expect(put).not.toHaveBeenCalled();
  expect(loadChatDrafts().chatDrafts).toEqual([]);
});

it("saves the draft before its bytes; a read in between fails as missing bytes", async () => {
  const f = fixture();
  const stored = deferred<void>();
  const put = f.kept.put.bind(f.kept);
  vi.spyOn(f.kept, "put").mockImplementationOnce(async (key, data) => {
    await stored.promise;
    await put(key, data);
  });
  const sent = f.submit();
  await vi.waitFor(() => expect(loadChatDrafts().chatDrafts).toHaveLength(1));
  const [saving] = loadChatDrafts().chatDrafts;
  expect(sendStatus(saving, Date.now())).toBe("pending");
  await expect(f.delivery().retry(saving.requestId, async () => true)).rejects.toThrow(
    "This message is still being sent.",
  );
  await expect(chatDraftPayload(f.kept, saving)).rejects.toThrow(
    /attachments are no longer saved on this device/,
  );
  await pruneChatDraftAttachments(f.kept);
  stored.resolve();
  await expect(sent).rejects.toThrow("Outer reply was lost");
  expect(f.send).toHaveBeenCalledTimes(1);
  expect(f.send.mock.calls[0][0].attachments).toEqual([LARGE, SECOND]);
});

it("prunes the bytes no saved send refers to", async () => {
  const f = fixture();
  await expect(f.submit()).rejects.toThrow();
  const [failed] = loadChatDrafts().chatDrafts;
  await f.kept.put("released", [SECOND.data]);
  await pruneChatDraftAttachments(f.kept);
  expect(await f.kept.get("released")).toBeUndefined();
  expect(await f.kept.get(chatDraftKey(failed))).toHaveLength(2);
});
