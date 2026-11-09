/** Where a saved send keeps its attachment bytes, one record per send under
 * its draft key, until the send is confirmed or discarded. */
export type SendAttachmentStore = {
  put(key: string, data: readonly string[]): Promise<void>;
  get(key: string): Promise<string[] | undefined>;
  delete(key: string): Promise<void>;
};

const DATABASE = "gents.chat.sendAttachments";
const RECORDS = "attachments";

function settled<T>(request: IDBRequest<T>) {
  return new Promise<T>((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function committed(transaction: IDBTransaction) {
  return new Promise<void>((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error);
    transaction.onabort = () =>
      reject(transaction.error ?? new Error("The attachment write was aborted."));
  });
}

/** WebKit's localStorage refuses a single value over about 5 Mi characters,
 * less than one base64 attachment at the 4 MiB file limit, so the bytes live
 * in IndexedDB. A write resolves only once its transaction commits. */
export function indexedDbSendAttachments(
  factory: () => IDBFactory | undefined = () => globalThis.indexedDB,
): SendAttachmentStore {
  let database: Promise<IDBDatabase> | null = null;
  function open() {
    database ??= (async () => {
      const idb = factory();
      if (!idb) throw new Error("This window has no IndexedDB.");
      const request = idb.open(DATABASE, 1);
      request.onupgradeneeded = () => request.result.createObjectStore(RECORDS);
      const opened = await settled(request);
      opened.onversionchange = () => {
        opened.close();
        database = null;
      };
      return opened;
    })().catch((error: unknown) => {
      database = null;
      throw error;
    });
    return database;
  }
  async function write(apply: (records: IDBObjectStore) => void) {
    const transaction = (await open()).transaction(RECORDS, "readwrite");
    const done = committed(transaction);
    apply(transaction.objectStore(RECORDS));
    await done;
  }
  return {
    put: (key, data) => write((records) => records.put([...data], key)),
    async get(key) {
      const value: unknown = await settled(
        (await open()).transaction(RECORDS).objectStore(RECORDS).get(key),
      );
      return Array.isArray(value) && value.every((item) => typeof item === "string")
        ? value
        : undefined;
    },
    delete: (key) => write((records) => records.delete(key)),
  };
}

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
  };
}

let current: SendAttachmentStore = indexedDbSendAttachments();

export function sendAttachments() {
  return current;
}

/** Tests replace the store; jsdom has no IndexedDB. */
export function setSendAttachments(store: SendAttachmentStore) {
  current = store;
}
