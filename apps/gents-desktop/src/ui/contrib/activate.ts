/* One activation path for every door. A loader hands over a validated
   plugin and where it came from; this publishes the inventory row with
   activate/deactivate handles, consults the user's decision, and on
   activate runs register(ctx) with every disposer collected so a throw
   mid-way rolls back cleanly and lands on the plugin's own row. Bundled
   and runtime loaders differ only in how they obtain the module. */
import { createUxContext, type GentsUxPlugin } from "./plugin";
import {
  patchUxPlugin,
  publishUxPlugin,
  uxPluginActive,
  type UxDeclaredContributions,
  type UxPluginDoor,
  type UxPluginProducer,
} from "./plugins-store";

export interface ActivateOptions {
  door: UxPluginDoor;
  file?: string;
  producer?: UxPluginProducer;
  pack?: string;
  /** the runtime listing id, for a pack-shipped plugin: what ctx.plugin calls under */
  listing?: string;
  declared?: UxDeclaredContributions;
  /** a root-level cap: false ships the plugin opt-in whatever it says */
  defaultEnabled?: boolean;
}

/** live plugins: id -> the disposers of the current incarnation */
const live = new Map<string, (() => void)[]>();

export function unloadUxPlugin(id: string): void {
  const disposers = live.get(id);
  /* released before the disposers run, each in its own try: a disposer with
     a bug must not wedge the registry, or every later reload re-runs the
     same broken cleanup and dies before the fresh register() */
  live.delete(id);
  disposers?.forEach((dispose) => {
    try {
      dispose();
    } catch (error) {
      console.error(`[ux-plugins] ${id}: disposer failed during unload`, error);
    }
  });
}

export function isUxPluginLive(id: string): boolean {
  return live.has(id);
}

/** publishes the row; resolves once the first activation, if any, settled */
export function activateUxPlugin(
  plugin: GentsUxPlugin,
  options: ActivateOptions,
): Promise<void> {
  const record = {
    id: plugin.id,
    name: plugin.name ?? plugin.id,
    description: plugin.description,
    door: options.door,
    file: options.file,
    producer: options.producer,
    pack: options.pack,
    declared: options.declared,
  };

  const fail = (error: unknown) => {
    unloadUxPlugin(plugin.id);
    console.error(`[ux-plugins] ${plugin.id} failed to register`, error);
    publishUxPlugin({
      ...record,
      status: "error",
      error: error instanceof Error ? error.message : String(error),
    });
  };

  const activate = (): Promise<void> => {
    unloadUxPlugin(plugin.id);
    const disposers: (() => void)[] = [];
    live.set(plugin.id, disposers);
    let result: unknown;
    try {
      result = plugin.register(
        createUxContext(plugin.id, {
          /* a registration made after this incarnation was retired (a
             register() still pending across a deactivate) is disposed at
             once rather than pushed into a list nobody will run */
          onDispose: (dispose) => {
            if (live.get(plugin.id) === disposers) disposers.push(dispose);
            else dispose();
          },
          declared: options.declared,
          listing: options.listing,
        }),
      );
    } catch (error) {
      fail(error);
      return Promise.resolve();
    }
    publishUxPlugin({ ...record, status: "loaded" });
    /* an async register (a frame's round trip) settles the row when it
       settles: a rejection lands on the row, never as an unhandled one */
    if (result && typeof (result as PromiseLike<unknown>).then === "function") {
      return Promise.resolve(result).then(
        () => undefined,
        (error: unknown) => {
          if (live.get(plugin.id) === disposers) fail(error);
        },
      );
    }
    return Promise.resolve();
  };

  const deactivate = () => {
    unloadUxPlugin(plugin.id);
    patchUxPlugin(plugin.id, { status: "disabled" });
  };

  publishUxPlugin({ ...record, status: "disabled" }, { activate, deactivate });
  if (
    uxPluginActive(
      plugin.id,
      (plugin.defaultEnabled ?? true) && (options.defaultEnabled ?? true),
    )
  ) {
    return activate();
  }
  return Promise.resolve();
}
