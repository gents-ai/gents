import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  ChatSendRequest,
  ChatSendResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
  RequestAttachment,
} from "@source-inc/gents-desktop-client";
import { createChatDelivery } from "../src/hooks/chatDelivery";
import { chatDraftKey, loadChatDrafts } from "../src/hooks/chatDrafts";
import { createChatStore } from "../src/hooks/chatStore";
import {
  memorySendAttachments,
  setSendAttachments,
  type SendAttachmentStore,
} from "../src/hooks/sendAttachments";
import { shellStores } from "./shell-fixture";
import { acceptedChat } from "./chat-delivery-fixture";

/* WebKit refuses a localStorage write once the origin holds about 5 Mi
   characters, which one base64 attachment at the 4 MiB file cap exceeds. */
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

/* 4 MiB of raw bytes, as the composer encodes a file at its size cap */
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

let kept: SendAttachmentStore;
beforeEach(() => {
  vi.stubGlobal("localStorage", webkitLocalStorage());
  kept = memorySendAttachments();
  setSendAttachments(kept);
});
afterEach(() => vi.unstubAllGlobals());

function fixture() {
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
  return { stores, send, delivery, submit };
}

it("keeps large attachments out of localStorage and resends them after a restart", async () => {
  const f = fixture();
  await expect(f.submit()).rejects.toThrow("Outer reply was lost");
  const request = f.send.mock.calls[0][0];
  expect(request.attachments).toEqual([LARGE, SECOND]);
  const [failed] = loadChatDrafts().chatDrafts;
  const entry = localStorage.getItem(chatDraftKey(failed))!;
  expect(entry.length).toBeLessThan(2_000);
  expect(failed.attachments).toEqual([
    expect.objectContaining({ name: "scan.pdf", size: 4 * 1024 * 1024 }),
    expect.objectContaining({ name: "notes.txt", size: 5 }),
  ]);
  // A restart reads the saved sends again; Retry resends the same payload.
  f.stores.chat = createChatStore();
  expect(f.stores.chat.getState().chatDrafts).toEqual([failed]);
  f.send.mockImplementationOnce(async (original) => acceptedChat(original));
  await f.delivery().retry(failed.requestId, async () => true);
  expect(f.send.mock.calls[1][0]).toEqual(request);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  await vi.waitFor(async () =>
    expect(await kept.get(chatDraftKey(failed))).toBeUndefined(),
  );
});

it("drops the kept bytes when a failed send is discarded", async () => {
  const f = fixture();
  await expect(f.submit()).rejects.toThrow();
  const [failed] = loadChatDrafts().chatDrafts;
  expect(await kept.get(chatDraftKey(failed))).toHaveLength(2);
  await f.delivery().discard(failed.requestId);
  expect(loadChatDrafts().chatDrafts).toEqual([]);
  await vi.waitFor(async () =>
    expect(await kept.get(chatDraftKey(failed))).toBeUndefined(),
  );
});

it("refuses a send whose attachments cannot be kept, before saving or dispatching it", async () => {
  const f = fixture();
  setSendAttachments({
    ...kept,
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
  await kept.put(chatDraftKey(failed), [SECOND.data, SECOND.data]);
  await expect(f.delivery().retry(failed.requestId, async () => true)).rejects.toThrow(
    /attachments are no longer saved on this device/,
  );
  await kept.delete(chatDraftKey(failed));
  await expect(f.delivery().retry(failed.requestId, async () => true)).rejects.toThrow(
    /attachments are no longer saved on this device/,
  );
  expect(f.send).toHaveBeenCalledTimes(1);
  const [still] = loadChatDrafts().chatDrafts;
  expect(still.failure?.message).toMatch(/Discard it and attach the files again/);
});
