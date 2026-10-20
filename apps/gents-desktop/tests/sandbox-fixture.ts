/* The sandbox, in-process: every test that loads a plugin runs the real
   frame runtime over a direct transport instead of an iframe, so the
   protocol (`@/sandbox/protocol.ts`) is exercised end to end — messages
   are JSON-cloned across, as a port would, so nothing but data crosses.
   The runtime's blob import is rerouted to a data: URL node's ESM loader
   handles; the SDK shims take the same path, so a plugin's `@gents/ux-sdk`
   import resolves to the frame's namespace. */
import { vi } from "vitest";

import {
  setSandboxTransportFactory,
  type FrameTransport,
} from "@/contrib/sandbox-host";
import type { FrameToHost, HostToFrame } from "@/sandbox/protocol";
import { createSandboxRuntime, type SandboxRuntime } from "@/sandbox/runtime";

const RealBlob = globalThis.Blob;
const realCreate = URL.createObjectURL;
const realRevoke = URL.revokeObjectURL;

class FakeBlob {
  parts: string[];
  constructor(parts: string[]) {
    this.parts = parts;
  }
}

const clone = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;

/** the live in-process frames, by key: a test reads what a page drew */
export const frames = new Map<
  string,
  { mount: HTMLElement; runtime: SandboxRuntime }
>();

/** the mount of the plugin's frame (where its pages draw), by key */
export function frameMount(key: string): HTMLElement | null {
  return frames.get(key)?.mount ?? null;
}

/** settle the microtask hops a round trip takes */
export async function settle(rounds = 6): Promise<void> {
  for (let i = 0; i < rounds; i++) await new Promise((r) => setTimeout(r, 0));
}

/* jsdom's storage is unusable under the test origin; a plain in-memory
   Storage stands in, with the index walk readStorage uses */
function memoryStorage(): Storage {
  const data = new Map<string, string>();
  return {
    get length() {
      return data.size;
    },
    key: (i: number) => [...data.keys()][i] ?? null,
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, String(v)),
    removeItem: (k: string) => void data.delete(k),
    clear: () => data.clear(),
  } as Storage;
}

export function installInProcessSandbox(): void {
  vi.stubGlobal("localStorage", memoryStorage());
  vi.stubGlobal("Blob", FakeBlob);
  URL.createObjectURL = (blob: unknown) =>
    `data:text/javascript;base64,${Buffer.from((blob as FakeBlob).parts.join("")).toString("base64")}`;
  URL.revokeObjectURL = () => undefined;
  setSandboxTransportFactory((key): FrameTransport => {
    const mount = document.createElement("div");
    mount.dataset.uxFrameMount = key;
    document.body.appendChild(mount);
    let toHost: ((message: FrameToHost) => void) | null = null;
    let toFrame: ((message: HostToFrame) => void) | null = null;
    const runtime = createSandboxRuntime(
      {
        post: (message) => queueMicrotask(() => toHost?.(clone(message))),
        onMessage: (handler) => {
          toFrame = handler;
        },
      },
      { mount, document },
    );
    frames.set(key, { mount, runtime });
    return {
      post: (message) => queueMicrotask(() => toFrame?.(clone(message))),
      onMessage: (handler) => {
        toHost = handler;
      },
      destroy: () => {
        toFrame = null;
        toHost = null;
        runtime.dispose();
        mount.remove();
        if (frames.get(key)?.runtime === runtime) frames.delete(key);
      },
    };
  });
}

export function uninstallInProcessSandbox(): void {
  for (const [, frame] of frames) {
    frame.runtime.dispose();
    frame.mount.remove();
  }
  frames.clear();
  setSandboxTransportFactory(null);
  vi.unstubAllGlobals();
  vi.stubGlobal("Blob", RealBlob);
  URL.createObjectURL = realCreate;
  URL.revokeObjectURL = realRevoke;
}
