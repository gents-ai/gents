/* The plugin authoring contract. A plugin is a module that default-exports a
   GentsUxPlugin; it never touches the registry directly. It receives a
   scoped UxContext whose `register` stamps provenance (`source:
   'plugin:<id>'`) and namespaces the contribution id (`<id>:<localId>`), so
   authors write plain contributions and two plugins cannot collide. Every
   effect taken through the context is tracked and torn down on
   disable/reload/uninstall; a bare global is the author's leak. After
   hermes-agent's contrib/plugin.ts. */
import type { Route } from "@/lib/router";
import type { NodeView } from "../../hooks/fleetStore";
import { onUxEvent, type UxEventListener } from "./events";
import { registry } from "./registry";
import { TRANSCRIPT_DIRECTIVE_AREA, type Contribution } from "./types";
import type { UxDeclaredContributions } from "./plugins-store";

/** a contribution as an author writes it: provenance and id scoping are the
    host's job, so those fields are off limits */
export type UxContribution = Omit<Contribution, "source" | "id"> & { id: string };

/** namespaced JSON persistence under `gents.ux.<id>.*` */
export interface UxStorage {
  get<T>(key: string, fallback: T): T;
  set(key: string, value: unknown): void;
  remove(key: string): void;
}

/** the OS door: every member resolves instead of throwing when the
    capability is absent (plain browser, older shell), so a caller branches
    on the result */
export interface UxOs {
  openExternal: (url: string) => Promise<boolean>;
  revealPath: (path: string) => Promise<boolean>;
  pickDirectory: (options?: {
    title?: string;
    defaultPath?: string | null;
  }) => Promise<string | null>;
}

/** what a plugin may read of the app, and the two verbs it may call on it.
    A node is the fleet store's view of a deployment: everything but its
    sessions and mailbox, which have their own stores. */
export interface UxShell {
  route: () => Route;
  navigate: (route: Route) => void;
  deployments: () => readonly NodeView[];
  selectedDeployment: () => NodeView | null;
  selectedSessionId: () => string | null;
  /** posts `content` as the user's next turn in the selected session, in
   *  the user's name (the app records no plugin provenance yet); rejects
   *  without a selected session, a user gesture and confirmation of the text */
  send: (content: string) => Promise<void>;
}

/** the app installs these once; a plugin reaches them only through ctx.
    No raw bridge verb: a plugin reaches the runtime through these names
    alone, so what a plugin may do is what this interface lists. */
/** one data call's answer, as the bridge gives it */
export interface UxDataCallResult {
  kind: "query" | "create";
  output: unknown;
}

export interface UxHost {
  shell: UxShell;
  /** run a pack's .afb plugin `name`, keyed by the runtime listing
      (`<ns>/<pack>/<name>`) of the UX plugin calling; the bridge resolves
      the pack from the listing. Absent where no runtime is reachable. */
  plugin?: (listing: string, name: string, input: unknown, nodeDid?: string) => Promise<unknown>;
  /** the data door: a declared tool of a declared surface, on the selected
      node, under the calling plugin's listing; the bridge checks all three */
  data?: {
    call: (
      listing: string,
      nodeDid: string,
      surface: string,
      tool: string,
      args: Record<string, unknown>,
    ) => Promise<UxDataCallResult>;
    surfaces: (listing: string, nodeDid: string) => Promise<unknown[]>;
  };
  os: UxOs;
  /** a change of the route, the selection or the fleet: what a frame's
      mirror of the shell follows */
  onShellChange?: (listener: () => void) => () => void;
}

export interface UxContext {
  /** the resolved source tag, `'plugin:<id>'` */
  readonly source: string;
  readonly pluginId: string;
  register: (c: UxContribution) => () => void;
  registerMany: (cs: UxContribution[]) => () => void;
  /** a cleanup that is not a contribution (a store subscription) */
  onDispose: (fn: () => void) => void;
  /** the desktop update stream by type; `'*'` for everything */
  onEvent: (type: string, listener: UxEventListener) => () => void;
  setTimeout: (fn: () => void, ms: number) => () => void;
  setInterval: (fn: () => void, ms: number) => () => void;
  addEventListener: (
    target: EventTarget,
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: AddEventListenerOptions | boolean,
  ) => () => void;
  /** one of this pack's .afb plugins by name; rejects for a plugin outside a pack */
  plugin: (name: string, input: unknown) => Promise<unknown>;
  /** posts `content` as the user's next turn in the selected session,
   *  visibly and in the user's name; the reverse channel of a transcript
   *  directive. Rejects without a selected session or outside a user
   *  gesture (a click or a key press): never from a timer, an effect or a
   *  directive's mount. */
  send: (content: string) => Promise<void>;
  shell: UxShell;
  storage: UxStorage;
  os: UxOs;
  /** the data door, scoped to this plugin's listing and the selected node */
  data: {
    call: (
      surface: string,
      tool: string,
      args?: Record<string, unknown>,
    ) => Promise<UxDataCallResult>;
    surfaces: () => Promise<unknown[]>;
  };
}

export interface GentsUxPlugin {
  /** stable slug; the `plugin:<id>` source and the id namespace */
  id: string;
  name?: string;
  description?: string;
  /** registers on load when the user has not chosen; false ships opt-in */
  defaultEnabled?: boolean;
  register: (ctx: UxContext) => void | Promise<void>;
}

/* ------------------------------------------------------------------------ */

let installedHost: UxHost | null = null;

/** the app's one call, before any plugin loads */
export function installUxHost(host: UxHost): void {
  installedHost = host;
}

export function uxHost(): UxHost | null {
  return installedHost;
}

function hostOrThrow(): UxHost {
  if (!installedHost) throw new Error("the UX plugin host is not installed yet");
  return installedHost;
}

function createStorage(pluginId: string): UxStorage {
  const scoped = (key: string) => `gents.ux.${pluginId}.${key}`;
  return {
    get(key, fallback) {
      try {
        const raw = window.localStorage.getItem(scoped(key));
        return raw === null ? fallback : (JSON.parse(raw) as typeof fallback);
      } catch {
        return fallback;
      }
    },
    set(key, value) {
      try {
        window.localStorage.setItem(scoped(key), JSON.stringify(value));
      } catch {
        /* nonfatal */
      }
    },
    remove(key) {
      try {
        window.localStorage.removeItem(scoped(key));
      } catch {
        /* nonfatal */
      }
    },
  };
}

/* timers and listeners a plugin takes out, retired as one disposer; a fired
   timeout drops out on its own so a long-lived plugin does not accumulate */
function createLifetime(track: (dispose: () => void) => void) {
  const cleanups = new Set<() => void>();
  let tracked = false;
  const scoped = (cleanup: () => void) => {
    if (!tracked) {
      tracked = true;
      track(() => {
        cleanups.forEach((pending) => pending());
        cleanups.clear();
      });
    }
    cleanups.add(cleanup);
    return () => {
      cleanups.delete(cleanup);
      cleanup();
    };
  };
  return {
    setTimeout: (fn: () => void, ms: number) => {
      const clear = () => globalThis.clearTimeout(id);
      const id = globalThis.setTimeout(() => {
        cleanups.delete(clear);
        fn();
      }, ms);
      return scoped(clear);
    },
    setInterval: (fn: () => void, ms: number) => {
      const id = globalThis.setInterval(fn, ms);
      return scoped(() => globalThis.clearInterval(id));
    },
    addEventListener: (
      target: EventTarget,
      type: string,
      listener: EventListenerOrEventListenerObject,
      options?: AddEventListenerOptions | boolean,
    ) => {
      target.addEventListener(type, listener, options);
      return scoped(() => target.removeEventListener(type, listener, options));
    },
  };
}

export class UndeclaredContributionError extends Error {}

/* the declared-contributions gate: a plugin may only register into areas
   its manifest named, and only claim directive names it named. A bundled
   plugin declares nothing and is not gated. */
function assertDeclared(
  pluginId: string,
  c: UxContribution,
  declared?: UxDeclaredContributions,
) {
  if (!declared) return;
  if (!declared.areas.includes(c.area)) {
    throw new UndeclaredContributionError(
      `${pluginId}: contribution "${c.id}" targets area "${c.area}", which the manifest does not declare (declared: ${declared.areas.join(", ") || "none"})`,
    );
  }
  if (c.area === TRANSCRIPT_DIRECTIVE_AREA) {
    const name = (c.data as { name?: unknown } | undefined)?.name;
    if (typeof name !== "string" || !declared.directives.includes(name)) {
      throw new UndeclaredContributionError(
        `${pluginId}: directive "${String(name)}" is not declared in the manifest (declared: ${declared.directives.join(", ") || "none"})`,
      );
    }
  }
}

/* a directive name has one owner: the first live registration. A second
   claim is refused here, when it is made, so neither `order` nor a later
   reload can move the name; it frees when the owner's registration is
   disposed. */
function assertUnclaimed(pluginId: string, id: string, c: UxContribution) {
  const name = (c.data as { name?: unknown } | undefined)?.name;
  if (typeof name !== "string" || name.length === 0) {
    throw new Error(`${pluginId}: a transcript directive needs a name`);
  }
  const owner = registry.find(
    TRANSCRIPT_DIRECTIVE_AREA,
    (e) => (e.data as { name?: unknown } | undefined)?.name === name,
  );
  if (owner && owner.id !== id) {
    throw new Error(
      `${pluginId}: directive "${name}" is already claimed by ${owner.source ?? owner.id}`,
    );
  }
}

export interface CreateContextOptions {
  /** every registration's disposer lands here; the loader runs them on unload */
  onDispose?: (dispose: () => void) => void;
  /** the manifest's declared contributions; absent means ungated (bundled) */
  declared?: UxDeclaredContributions;
  /** the runtime listing id (`<ns>/<pack>/<name>`) of a pack-shipped
      plugin: the key ctx.plugin calls under, which the bridge resolves to
      the pack; absent for a plugin outside a pack */
  listing?: string;
}

export function createUxContext(
  pluginId: string,
  options: CreateContextOptions = {},
): UxContext {
  const source = `plugin:${pluginId}`;
  const scope = (c: UxContribution): Contribution => {
    if (typeof c.id !== "string" || !CONTRIBUTION_ID_RE.test(c.id)) {
      throw new Error(
        `${pluginId}: contribution id ${JSON.stringify(c.id)} must be one segment without whitespace, "/", "?", "#" or ":"`,
      );
    }
    assertDeclared(pluginId, c, options.declared);
    const id = `${pluginId}:${c.id}`;
    if (c.area === TRANSCRIPT_DIRECTIVE_AREA) assertUnclaimed(pluginId, id, c);
    /* the registry keeps its own frozen snapshot of the payload: what was
       checked is what is served, whatever the plugin does to its object
       afterwards */
    const data =
      typeof c.data === "object" && c.data !== null && !Array.isArray(c.data)
        ? Object.freeze({ ...(c.data as Record<string, unknown>) })
        : c.data;
    return { ...c, id, source, data };
  };
  const track = (dispose: () => void) => {
    options.onDispose?.(dispose);
    return dispose;
  };
  const host = () => hostOrThrow();
  return {
    source,
    pluginId,
    register: (c) => track(registry.register(scope(c))),
    registerMany: (cs) => track(registry.registerMany(cs.map(scope))),
    onDispose: (fn) => void track(fn),
    onEvent: (type, listener) => track(onUxEvent(type, listener)),
    ...createLifetime(track),
    plugin: (name, input) => {
      const h = host();
      if (!options.listing || !h.plugin) {
        return Promise.reject(
          new Error(`${pluginId}: ctx.plugin needs a pack; this plugin ships in none`),
        );
      }
      return h.plugin(options.listing, name, input, h.shell.selectedDeployment()?.nodeDid);
    },
    send: (content) => host().shell.send(content),
    get shell() {
      return host().shell;
    },
    storage: createStorage(pluginId),
    get os() {
      return host().os;
    },
    data: {
      call: (surface, tool, args) => {
        const h = host();
        const node = h.shell.selectedDeployment();
        if (!options.listing || !h.data) {
          return Promise.reject(
            new Error(`${pluginId}: ctx.data needs a pack; this plugin ships in none`),
          );
        }
        if (!node) return Promise.reject(new Error(`${pluginId}: no node is selected`));
        return h.data.call(options.listing, node.nodeDid, surface, tool, args ?? {});
      },
      surfaces: () => {
        const h = host();
        const node = h.shell.selectedDeployment();
        if (!options.listing || !h.data) {
          return Promise.reject(
            new Error(`${pluginId}: ctx.data needs a pack; this plugin ships in none`),
          );
        }
        if (!node) return Promise.reject(new Error(`${pluginId}: no node is selected`));
        return h.data.surfaces(options.listing, node.nodeDid);
      },
    },
  };
}

export { UX_PLUGIN_ID_RE, isValidUxPluginId } from "@/sandbox/contract";

/** a contribution's local id: one segment, legal in a route */
const CONTRIBUTION_ID_RE = /^[^\s/?#:]{1,128}$/;

export { isGentsUxPlugin } from "@/sandbox/contract";
