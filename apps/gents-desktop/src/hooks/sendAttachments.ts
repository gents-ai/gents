/** Where a saved send keeps its attachment bytes, one record per send under
 * its draft key, until the send is confirmed or discarded. A record is stored
 * only after its draft is saved. */
export type SendAttachmentStore = {
  put(key: string, data: readonly string[]): Promise<void>;
  get(key: string): Promise<string[] | undefined>;
  delete(key: string): Promise<void>;
  /** Deletes each record whose key `referenced` rejects when the record is
   * visited. */
  prune(referenced: (key: string) => boolean): Promise<void>;
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

const record = (value: unknown): value is string[] =>
  Array.isArray(value) && value.every((item) => typeof item === "string");

/** WebKit's localStorage refuses a single value over about 5 Mi characters,
 * less than one base64 attachment at the 4 MiB file limit, so the bytes live
 * in IndexedDB. A write resolves only once its transaction commits. */
export function indexedDbSendAttachments(): SendAttachmentStore {
  let database: Promise<IDBDatabase> | null = null;
  function open() {
    database ??= (async () => {
      const idb = globalThis.indexedDB as IDBFactory | undefined;
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
      return record(value) ? value : undefined;
    },
    delete: (key) => write((records) => records.delete(key)),
    // `referenced` is asked as each record is visited, inside the
    // transaction: a record stored before it began had its draft saved
    // first, so only a released send's record is unreferenced.
    prune: (referenced) =>
      write((records) => {
        const cursor = records.openCursor();
        cursor.onsuccess = () => {
          const at = cursor.result;
          if (!at) return;
          if (typeof at.key === "string" && !referenced(at.key)) at.delete();
          at.continue();
        };
      }),
  };
}
