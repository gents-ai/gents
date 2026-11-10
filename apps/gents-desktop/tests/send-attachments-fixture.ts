import type { SendAttachmentStore } from "../src/hooks/sendAttachments";

/** A send attachment store in memory: jsdom has no IndexedDB. */
export function memorySendAttachments(
  now: () => number = Date.now,
): SendAttachmentStore {
  const records = new Map<string, { data: string[]; keptAt: number }>();
  return {
    async put(key, data) {
      records.set(key, { data: [...data], keptAt: now() });
    },
    async get(key) {
      const record = records.get(key);
      return record && [...record.data];
    },
    async delete(key) {
      records.delete(key);
    },
    async prune(referenced, keptBefore) {
      for (const [key, { keptAt }] of records)
        if (!referenced(key) && keptAt < keptBefore) records.delete(key);
    },
  };
}
