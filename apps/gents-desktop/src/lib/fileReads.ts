import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { isUnreachable, type SessionScope } from "./recordedFiles";

type Fetch = NonNullable<DesktopApiAdapter["fetchToolFileChanges"]>;

/* The panel's reads go one at a time, the file the reader chose first, so
   a turn with many files never holds up the one being looked at. Once a
   node cannot be reached, the reads still waiting for it fail at once
   rather than each waiting for the bridge to give up. */
const waiting: { file: string; start: () => Promise<void> }[] = [];
let running = false;
let preferred = "";
const unreachable = new Map<string, string>();

export const fileId = (scope: SessionScope, path: string) =>
  [scope.nodeDid, scope.sessionId, path].join("\0");

/** Puts reads of `path` ahead of the other waiting reads. */
export function preferFile(scope: SessionScope, path: string) {
  preferred = fileId(scope, path);
}

export const forgetUnreachable = () => unreachable.clear();

function next() {
  if (running || waiting.length === 0) return;
  const at = Math.max(
    0,
    waiting.findIndex((read) => read.file === preferred),
  );
  const [read] = waiting.splice(at, 1);
  running = true;
  void read!.start().finally(() => {
    running = false;
    next();
  });
}

/** `fetch` for one file's reads, run through the panel's queue. */
export function queued(fetch: Fetch, file: string): Fetch {
  return (args) =>
    new Promise((resolve, reject) => {
      waiting.push({
        file,
        start: async () => {
          try {
            const reason = unreachable.get(args.nodeDid);
            if (reason) throw new Error(reason);
            resolve(await fetch(args));
          } catch (error) {
            if (isUnreachable(error)) unreachable.set(args.nodeDid, error.message);
            reject(error);
          }
        },
      });
      queueMicrotask(next);
    });
}
