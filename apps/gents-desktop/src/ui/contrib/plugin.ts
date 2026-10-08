/* The plugin authoring contract. A plugin is a module that default-exports a
   GentsUxPlugin; it never touches the registry directly. It receives a
   scoped UxContext whose `register` stamps provenance (`source:
   'plugin:<id>'`) and namespaces the contribution id (`<id>:<localId>`), so
   authors write plain contributions and two plugins cannot collide. Every
   effect taken through the context is tracked and torn down on
   disable/reload/uninstall; a bare global is the author's leak. After
   hermes-agent's contrib/plugin.ts. */
import type { Route } from "@/lib/router";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
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

/** what a plugin may read of the app, and the two verbs it may call on it */
export interface UxShell {
  route: () => Route;
  navigate: (route: Route) => void;
  deployments: () => readonly DeploymentView[];
  selectedDeployment: () => DeploymentView | null;
  selectedSessionId: () => string | null;
  /** a user turn in the selected session, attributed to the plugin */
  send: (content: string) => Promise<void>;
}

/** the app installs these once; a plugin reaches them only through ctx */
export interface UxHost {
  shell: UxShell;
  /** one bridge command, as bridgeCall.call does */
  bridge: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  /** run one of the plugin's own pack's .afb plugins; absent for a plugin
      that ships in no pack */
  plugin?: (pack: string, name: string, input: unknown) => Promise<unknown>;
  os: UxOs;
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
  bridge: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  /** one of this pack's .afb plugins by name; rejects for a plugin outside a pack */
  plugin: (name: string, input: unknown) => Promise<unknown>;
  /** a hidden user turn: the reverse channel of a transcript directive */
  send: (content: string) => Promise<void>;
  shell: UxShell;
  storage: UxStorage;
  os: UxOs;
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

export interface CreateContextOptions {
  /** every registration's disposer lands here; the loader runs them on unload */
  onDispose?: (dispose: () => void) => void;
  /** the manifest's declared contributions; absent means ungated (bundled) */
  declared?: UxDeclaredContributions;
  /** `namespace/name` of the pack whose .afb plugins ctx.plugin may call */
  pack?: string;
}

export function createUxContext(
  pluginId: string,
  options: CreateContextOptions = {},
): UxContext {
  const source = `plugin:${pluginId}`;
  const scope = (c: UxContribution): Contribution => {
    assertDeclared(pluginId, c, options.declared);
    return { ...c, id: `${pluginId}:${c.id}`, source };
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
    bridge: (command, args) => host().bridge(command, args),
    plugin: (name, input) => {
      const h = host();
      if (!options.pack || !h.plugin) {
        return Promise.reject(
          new Error(`${pluginId}: ctx.plugin needs a pack; this plugin ships in none`),
        );
      }
      return h.plugin(options.pack, name, input);
    },
    send: (content) => host().shell.send(content),
    get shell() {
      return host().shell;
    },
    storage: createStorage(pluginId),
    get os() {
      return host().os;
    },
  };
}

/** what every loader checks before trusting a module's default export */
export function isGentsUxPlugin(value: unknown): value is GentsUxPlugin {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { id?: unknown }).id === "string" &&
    (value as { id: string }).id.length > 0 &&
    typeof (value as { register?: unknown }).register === "function"
  );
}
