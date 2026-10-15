/* The sandbox runtime: what runs inside a plugin's frame. It takes the port
   the shell built for this plugin, evaluates the plugin's module against
   the frame's own SDK, hands `register` a UxContext whose verbs cross the
   port, draws the plugin's pages into the frame when the shell shows them,
   and relays events, storage and the shell snapshot. The frame holds no
   authority of its own: no IPC, no parent document, no origin. Removing
   the frame is the whole teardown.

   The same runtime runs in-process under tests, over a direct transport,
   so the protocol is exercised end to end without a browser. */
import * as React from "react";
import * as jsxRuntime from "react/jsx-runtime";
import { createRoot, type Root } from "react-dom/client";
import type { ReactNode } from "react";

import type {
  GentsUxPlugin,
  UxContext,
  UxContribution,
  UxDataCallResult,
  UxDataSurfaceDecl,
  UxDeployment,
} from "./contract";
import { isGentsUxPlugin } from "./contract";
import { IMPORT_TIMEOUT_MS, rewriteSpecifiers, unsupportedImports } from "./module";
import {
  FRAME_RENDERED_AREAS,
  type FrameToHost,
  type HostToFrame,
  type RpcVerb,
  type ShellSnapshot,
  type WireContribution,
} from "./protocol";
import { setToastSink } from "./toast";
import type { Route } from "@gents/shell";

export interface FramePort {
  post: (message: FrameToHost) => void;
  onMessage: (handler: (message: HostToFrame) => void) => void;
}

export interface RuntimeOptions {
  /** where pages draw; the frame's root element */
  mount: HTMLElement;
  /** the module import; a test substitutes a data: URL or an object */
  importModule?: (source: string) => Promise<{ default?: unknown }>;
  /** the frame's document for css and theme; absent in a bare test */
  document?: Document;
}

const ALLOWED_SPECIFIERS = ["@gents/ux-sdk", "react", "react/jsx-runtime"] as const;

/* the SDK a module imports resolves to these globals through shim blobs;
   the names come from the namespace so the list cannot drift */
async function installSdkGlobals(): Promise<Record<string, string>> {
  const sdk = await import("./sdk-exports");
  const spaces: Record<string, object> = {
    __GENTS_UX_SDK__: sdk,
    __GENTS_REACT__: React,
    __GENTS_REACT_JSX__: jsxRuntime,
  };
  Object.assign(globalThis, spaces);
  const shim = (key: string) => {
    const names = Object.keys(spaces[key]!).filter(
      (name) => name !== "default" && /^[A-Za-z_$][\w$]*$/.test(name),
    );
    const source =
      `const m = globalThis.${key};\n` +
      `export default m.default ?? m;\n` +
      (names.length ? `export const { ${names.join(", ")} } = m;\n` : "");
    return URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
  };
  return {
    "@gents/ux-sdk": shim("__GENTS_UX_SDK__"),
    "react/jsx-runtime": shim("__GENTS_REACT_JSX__"),
    react: shim("__GENTS_REACT__"),
  };
}

/* the one dynamic import of a URL the bundler cannot see */
function importBlob(url: string): Promise<{ default?: unknown }> {
  return import(url) as Promise<{ default?: unknown }>;
}

async function defaultImport(source: string): Promise<{ default?: unknown }> {
  const unsupported = unsupportedImports(source, ALLOWED_SPECIFIERS);
  if (unsupported.length > 0) {
    throw new Error(
      `unsupported import${unsupported.length > 1 ? "s" : ""}: ${unsupported.join(", ")}; ` +
        `a UX plugin may only import @gents/ux-sdk, react and react/jsx-runtime`,
    );
  }
  const map = await installSdkGlobals();
  const url = URL.createObjectURL(
    new Blob([rewriteSpecifiers(source, map)], { type: "text/javascript" }),
  );
  let deadline: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      importBlob(url),
      new Promise<never>((_, reject) => {
        deadline = setTimeout(
          () =>
            reject(
              new Error(
                `import timed out after ${IMPORT_TIMEOUT_MS / 1000}s; module evaluation never settled`,
              ),
            ),
          IMPORT_TIMEOUT_MS,
        );
      }),
    ]);
  } finally {
    clearTimeout(deadline);
    URL.revokeObjectURL(url);
  }
}

/* ---- the frame-side renderer: one root, the surfaces the shell shows ---- */

type SurfaceRender = (props: Record<string, unknown>) => ReactNode;

interface Shown {
  surface: string;
  props: Record<string, unknown>;
}

class SurfaceBoundary extends React.Component<
  {
    surface: string;
    onError: (surface: string, error: unknown) => void;
    children: ReactNode;
  },
  { error: unknown }
> {
  state = { error: null as unknown };
  static getDerivedStateFromError(error: unknown) {
    return { error };
  }
  componentDidCatch(error: unknown) {
    this.props.onError(this.props.surface, error);
  }
  render() {
    if (this.state.error) {
      const message =
        this.state.error instanceof Error
          ? this.state.error.message
          : String(this.state.error);
      return jsxRuntime.jsx("div", {
        role: "alert",
        className: "rounded border border-destructive/40 p-3 text-sm text-destructive",
        "data-testid": "frame-surface-error",
        children: `This page failed: ${message}`,
      });
    }
    return this.props.children;
  }
}

/* ---- the runtime ---- */

export interface SandboxRuntime {
  /** test seam: the shown surfaces, by id */
  shown(): readonly string[];
  dispose(): void;
}

export function createSandboxRuntime(
  port: FramePort,
  options: RuntimeOptions,
): SandboxRuntime {
  const doc =
    options.document ?? (typeof document !== "undefined" ? document : undefined);
  const importModule = options.importModule ?? defaultImport;

  let plugin: GentsUxPlugin | null = null;
  let shell: ShellSnapshot = {
    route: { name: "sessions" },
    deployments: [],
    selectedDeployment: null,
    selectedSessionId: null,
  };
  let storage: Record<string, unknown> = {};
  const callbacks = new Map<string, (...args: unknown[]) => unknown>();
  const surfaces = new Map<string, SurfaceRender>();
  const surfaceOwner = new Map<string, string>();
  const eventListeners = new Map<
    string,
    Set<(event: { type: string; payload: unknown }) => void>
  >();
  const disposers: Array<() => void> = [];
  const pending = new Map<
    number,
    { resolve: (v: unknown) => void; reject: (e: Error) => void }
  >();
  let nextId = 1;
  let shown: Shown[] = [];
  let root: Root | null = null;
  let renderScheduled = false;
  let styleEl: HTMLStyleElement | null = null;

  const log = (level: "error" | "warn", message: string) =>
    port.post({ t: "log", level, message });

  const rpc = (verb: RpcVerb, ...args: unknown[]): Promise<unknown> =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      port.post({ t: "call", id, verb, args });
    });

  /* the callback handles minted while wiring one contribution, so a
     replaced or disposed contribution frees its functions */
  let minted: string[] = [];
  const callbacksByKey = new Map<string, { handle: string; ids: string[] }>();

  /* data with functions swapped for handles; a page's render stays here */
  const wire = (value: unknown, depth = 0): unknown => {
    if (typeof value === "function") {
      const handle = `cb${nextId++}`;
      callbacks.set(handle, value as (...args: unknown[]) => unknown);
      minted.push(handle);
      return { $cb: handle };
    }
    if (value === null || typeof value !== "object" || depth > 6) {
      return typeof value === "symbol" || typeof value === "undefined"
        ? undefined
        : value;
    }
    if (Array.isArray(value)) return value.map((v) => wire(v, depth + 1));
    if (React.isValidElement(value)) return undefined;
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      const w = wire(v, depth + 1);
      if (w !== undefined) out[k] = w;
    }
    return out;
  };

  const scheduleRender = () => {
    if (renderScheduled) return;
    renderScheduled = true;
    queueMicrotask(() => {
      renderScheduled = false;
      if (!root) root = createRoot(options.mount);
      root.render(
        jsxRuntime.jsx(React.Fragment, {
          children: shown.map(({ surface, props }) => {
            const render = surfaces.get(surface);
            return jsxRuntime.jsx(
              SurfaceBoundary,
              {
                surface,
                onError: (s: string, error: unknown) =>
                  port.post({
                    t: "render-error",
                    surface: s,
                    error: error instanceof Error ? error.message : String(error),
                  }),
                children: render
                  ? jsxRuntime.jsx(Mounted, { render, props })
                  : jsxRuntime.jsx("p", {
                      className: "text-sm text-muted-foreground",
                      children: `No page is registered as "${surface}".`,
                    }),
              },
              surface,
            );
          }),
        }),
      );
    });
  };

  /* one component type per render function, so the shell's prop changes
     reach a mounted page instead of remounting it */
  const mountedCache = new WeakMap<
    SurfaceRender,
    React.FC<{ props: Record<string, unknown> }>
  >();
  function Mounted({
    render,
    props,
  }: {
    render: SurfaceRender;
    props: Record<string, unknown>;
  }) {
    let C = mountedCache.get(render);
    if (!C) {
      C = ({ props: p }) => jsxRuntime.jsx(React.Fragment, { children: render(p) });
      mountedCache.set(render, C);
    }
    return jsxRuntime.jsx(C, { props });
  }

  const makeContext = (id: string): UxContext => {
    const source = `plugin:${id}`;
    const registerOne = (c: UxContribution): WireContribution => {
      if (typeof c.id !== "string")
        throw new Error(`${id}: a contribution needs a string id`);
      const handle = `h${nextId++}`;
      const namespaced = `${id}:${c.id}`;
      let data = c.data;
      let rendered = false;
      if ((FRAME_RENDERED_AREAS as readonly string[]).includes(c.area)) {
        const render =
          typeof c.render === "function"
            ? c.render
            : typeof (data as { render?: unknown } | undefined)?.render === "function"
              ? ((data as { render: SurfaceRender }).render as SurfaceRender)
              : null;
        if (render) {
          surfaces.set(namespaced, render);
          surfaceOwner.set(namespaced, handle);
          rendered = true;
          if (data && typeof data === "object") {
            const { render: _render, ...rest } = data as Record<string, unknown>;
            data = rest;
          }
        }
      }
      minted = [];
      const wired = wire(data);
      /* the same area and id again replaces the earlier contribution in the
         shell; its functions are released here, since nothing will call them */
      const key = `${c.area}\u0000${c.id}`;
      for (const stale of callbacksByKey.get(key)?.ids ?? []) callbacks.delete(stale);
      callbacksByKey.set(key, { handle, ids: minted });
      minted = [];
      return {
        handle,
        id: c.id,
        area: c.area,
        order: c.order,
        title: c.title,
        enabled: c.enabled,
        data: wired,
        render: rendered,
      };
    };
    const unregister = (contributions: WireContribution[]) => {
      for (const c of contributions) {
        const namespaced = `${id}:${c.id}`;
        if (surfaceOwner.get(namespaced) === c.handle) {
          surfaces.delete(namespaced);
          surfaceOwner.delete(namespaced);
          scheduleRender();
        }
        const key = `${c.area}\u0000${c.id}`;
        const owned = callbacksByKey.get(key);
        if (owned?.handle === c.handle) {
          for (const stale of owned.ids) callbacks.delete(stale);
          callbacksByKey.delete(key);
        }
      }
      port.post({ t: "unregister", handles: contributions.map((c) => c.handle) });
    };
    const register = (cs: UxContribution[]) => {
      const contributions = cs.map(registerOne);
      port.post({ t: "register", contributions });
      let disposed = false;
      const dispose = () => {
        if (disposed) return;
        disposed = true;
        unregister(contributions);
      };
      disposers.push(dispose);
      return dispose;
    };
    const scoped = (key: string) => `gents.ux.${id}.${key}`;
    const os = {
      openExternal: (url: string) =>
        rpc("os.openExternal", url).then(
          (v) => v === true,
          () => false,
        ),
      revealPath: (path: string) =>
        rpc("os.revealPath", path).then(
          (v) => v === true,
          () => false,
        ),
      pickDirectory: (o?: { title?: string; defaultPath?: string | null }) =>
        rpc("os.pickDirectory", o ?? {}).then(
          (v) => (typeof v === "string" ? v : null),
          () => null,
        ),
    };
    return {
      source,
      pluginId: id,
      register: (c) => register([c]),
      registerMany: (cs) => register(cs),
      onDispose: (fn) => {
        disposers.push(fn);
      },
      onEvent: (type, listener) => {
        const set = eventListeners.get(type) ?? new Set();
        set.add(listener);
        eventListeners.set(type, set);
        const off = () => {
          set.delete(listener);
        };
        disposers.push(off);
        return off;
      },
      setTimeout: (fn, ms) => {
        const t = globalThis.setTimeout(fn, ms);
        const off = () => globalThis.clearTimeout(t);
        disposers.push(off);
        return off;
      },
      setInterval: (fn, ms) => {
        const t = globalThis.setInterval(fn, ms);
        const off = () => globalThis.clearInterval(t);
        disposers.push(off);
        return off;
      },
      addEventListener: (target, type, listener, o) => {
        target.addEventListener(type, listener, o);
        const off = () => target.removeEventListener(type, listener, o);
        disposers.push(off);
        return off;
      },
      plugin: (name, input) => rpc("plugin", name, input),
      send: (content) => rpc("send", content).then(() => undefined),
      shell: {
        route: () => shell.route as Route,
        navigate: (route) => void rpc("navigate", route),
        deployments: () => shell.deployments as UxDeployment[],
        selectedDeployment: () =>
          (shell.selectedDeployment as UxDeployment | null) ?? null,
        selectedSessionId: () => shell.selectedSessionId,
        send: (content) => rpc("send", content).then(() => undefined),
      },
      storage: {
        get: (key, fallback) => {
          const k = scoped(key);
          return Object.prototype.hasOwnProperty.call(storage, k)
            ? (storage[k] as typeof fallback)
            : fallback;
        },
        set: (key, value) => {
          storage = { ...storage, [scoped(key)]: value };
          void rpc("storage.set", key, value);
        },
        remove: (key) => {
          const { [scoped(key)]: _gone, ...rest } = storage;
          storage = rest;
          void rpc("storage.remove", key);
        },
      },
      os,
      data: {
        call: (surface, tool, args) =>
          rpc("data.call", surface, tool, args ?? {}) as Promise<UxDataCallResult>,
        surfaces: () => rpc("data.surfaces") as Promise<UxDataSurfaceDecl[]>,
      },
    };
  };

  const applyTheme = (theme: "light" | "dark") => {
    if (!doc) return;
    if (theme === "dark") doc.documentElement.dataset.theme = "dark";
    else delete doc.documentElement.dataset.theme;
  };

  const handle = async (message: HostToFrame) => {
    switch (message.t) {
      case "init": {
        storage = message.storage;
        shell = message.shell;
        applyTheme(message.theme);
        if (message.css && doc) {
          const el = doc.createElement("style");
          el.dataset.gentsUx = "plugin";
          el.textContent = message.css;
          doc.head.appendChild(el);
          styleEl = el;
          disposers.push(() => el.remove());
        }
        try {
          const mod = await importModule(message.source);
          const candidate = mod.default;
          if (!isGentsUxPlugin(candidate)) {
            throw new Error("the module has no valid default GentsUxPlugin export");
          }
          plugin = candidate;
          if (styleEl) styleEl.dataset.gentsUx = candidate.id;
          port.post({
            t: "loaded",
            plugin: {
              id: candidate.id,
              name: candidate.name,
              description: candidate.description,
              defaultEnabled: candidate.defaultEnabled,
            },
          });
        } catch (error) {
          port.post({
            t: "failed",
            error: error instanceof Error ? error.message : String(error),
          });
        }
        return;
      }
      case "activate": {
        if (!plugin) {
          port.post({ t: "registered", ok: false, error: "no plugin is loaded" });
          return;
        }
        try {
          await plugin.register(makeContext(plugin.id));
          port.post({ t: "registered", ok: true });
        } catch (error) {
          port.post({
            t: "registered",
            ok: false,
            error: error instanceof Error ? error.message : String(error),
          });
        }
        return;
      }
      case "show":
        shown = [...shown.filter((s) => s.surface !== message.surface), message];
        scheduleRender();
        return;
      case "hide":
        shown = shown.filter((s) => s.surface !== message.surface);
        scheduleRender();
        return;
      case "event": {
        const event = { type: message.type, payload: message.payload };
        for (const type of [message.type, "*"]) {
          eventListeners.get(type)?.forEach((listener) => {
            try {
              listener(event);
            } catch (error) {
              log("error", `listener for ${type} threw: ${String(error)}`);
            }
          });
        }
        return;
      }
      case "shell":
        shell = message.shell;
        return;
      case "theme":
        applyTheme(message.theme);
        return;
      case "invoke": {
        const fn = callbacks.get(message.callback);
        if (!fn) return;
        try {
          await fn(...message.args);
        } catch (error) {
          log("error", `callback threw: ${String(error)}`);
        }
        return;
      }
      case "reply": {
        const waiter = pending.get(message.id);
        if (!waiter) return;
        pending.delete(message.id);
        if (message.ok) waiter.resolve(message.value);
        else waiter.reject(new Error(message.error));
        return;
      }
    }
  };

  setToastSink((kind, text) => void rpc("toast", kind, text));
  port.onMessage((message) => void handle(message));

  return {
    shown: () => shown.map((s) => s.surface),
    dispose: () => {
      for (const dispose of disposers.splice(0)) {
        try {
          dispose();
        } catch {
          /* a disposer with a bug must not block the rest */
        }
      }
      root?.unmount();
      root = null;
      shown = [];
    },
  };
}
