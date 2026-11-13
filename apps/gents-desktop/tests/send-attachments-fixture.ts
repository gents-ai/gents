import type { SendAttachmentStore } from "../src/hooks/sendAttachments";

export function memorySendAttachments(): SendAttachmentStore {
  const records = new Map<string, string[]>();
  return {
    async put(key, data) {
      records.set(key, [...data]);
    },
    async get(key) {
      const data = records.get(key);
      return data && [...data];
    },
    async delete(key) {
      records.delete(key);
    },
    async prune(referenced) {
      for (const key of records.keys()) if (!referenced(key)) records.delete(key);
    },
  };
}
