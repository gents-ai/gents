/* The runtime loader: a plugin as code, loaded after build time. Every
   plugin, whatever its door, takes this pipeline:

     module text -> import allowlist (`@gents/ux-sdk` / `react*` only)
       -> a sandbox frame of its own, which evaluates the module against
          the frame's SDK and reports the plugin's id
       -> activate (register(ctx) runs in the frame; its contributions
          land in the shell's registry under the plugin's own context;
          a throw rolls back and lands on the plugin's row)

   Loading the same id again disposes the previous incarnation first (its
   frame goes with it), so a rewritten file is a clean reload. Failures
   land on the plugin's own inventory row; a broken plugin can never take
   the app down. The frame is the capability boundary: an opaque origin
   with no IPC and no parent document, reached only over its port
   (`@/sandbox/protocol.ts`). The allowlist is an honest-author lint, kept
   so a bad import fails on the row with its name rather than as the
   frame's silence. */
import { activateUxPlugin, unloadUxPlugin, type ActivateOptions } from "./activate";
import type { GentsUxPlugin, UxContext } from "./plugin";
import { dropUxPlugin, publishUxPlugin, uxPluginRecords } from "./plugins-store";
import { SandboxHost } from "./sandbox-host";
import { ALLOWED_SPECIFIERS } from "@/sdk/runtime";
import { unsupportedImports } from "@/sandbox/module";

export {
  COMPUTED_IMPORT,
  IMPORT_TIMEOUT_MS,
  codeRanges,
  rewriteSpecifiers,
  scan,
  unsupportedImports,
} from "@/sandbox/module";

export interface RuntimeModule {
  /** the ESM source of the plugin */
  source: string;
  /** optional stylesheet installed in the plugin's frame */
  css?: string | null;
}

export interface LoadOptions extends Omit<ActivateOptions, "door"> {
  door?: ActivateOptions["door"];
  /** the inventory name for a module that never yields a plugin id */
  origin: string;
  /** the id the listing declared; a module claiming another is refused,
      since decisions and the inventory row were keyed on this one */
  expectId?: string;
}

/* the frame host of every loaded plugin, by plugin id */
const hosts = new Map<string, SandboxHost>();

/** test seam: the frame host behind a loaded plugin */
export function sandboxHostOf(id: string): SandboxHost | undefined {
  return hosts.get(id);
}

/* a module whose id is not named up front (a bare loader call) is
   evaluated once under a probe frame to learn it; every door that lists a
   plugin names the id before a frame is built */
async function probeId(module: RuntimeModule, origin: string): Promise<string> {
  const probe = new SandboxHost(module.source, {
    key: `${origin}#probe`,
    pluginId: "",
    css: null,
  });
  try {
    await probe.load();
    throw new Error(`${origin} exports a plugin with an empty id`);
  } catch (error) {
    const m = /exports a plugin with id "([^"]+)"/.exec(
      error instanceof Error ? error.message : String(error),
    );
    if (m) return m[1]!;
    throw error;
  } finally {
    probe.dispose();
  }
}

/* evaluate one runtime module through the whole pipeline; the loaded
   plugin id, or null on failure (the failure is on an inventory row) */
export async function loadRuntimeUxPlugin(
  module: RuntimeModule,
  options: LoadOptions,
): Promise<string | null> {
  const door = options.door ?? "dev";
  try {
    const unsupported = unsupportedImports(module.source, ALLOWED_SPECIFIERS);
    if (unsupported.length > 0) {
      throw new Error(
        `unsupported import${unsupported.length > 1 ? "s" : ""}: ${unsupported.join(", ")}; ` +
          `a UX plugin may only import @gents/ux-sdk, react and react/jsx-runtime`,
      );
    }
    const pluginId = options.expectId ?? (await probeId(module, options.origin));
    const host = new SandboxHost(module.source, {
      key: options.origin,
      pluginId,
      css: module.css,
      listing: options.listing,
      declared: options.declared,
    });
    const meta = await host.load();
    /* a runtime copy of a plugin that ships bundled must not register a
       second time; the bundled copy wins, visibly */
    const existing = uxPluginRecords()[meta.id];
    if (existing?.door === "bundled" && door !== "bundled") {
      host.dispose();
      publishUxPlugin({
        id: `${meta.id}:shadowed`,
        name: `${meta.name ?? meta.id} (shadowed)`,
        description: `Shadowed by the bundled "${meta.id}" plugin; this copy is not used.`,
        door,
        file: options.file,
        status: "disabled",
      });
      return null;
    }
    /* two sources claiming one id: the first loaded owns it */
    if (existing && existing.file !== options.file) {
      host.dispose();
      throw new Error(
        `duplicate id "${meta.id}", already loaded from ${existing.file ?? existing.door}`,
      );
    }
    hosts.get(meta.id)?.dispose();
    hosts.set(meta.id, host);
    /* the plugin as the activation path sees it: register() runs in the
       frame; a frame per activation, gone with the deactivation */
    const facade: GentsUxPlugin = {
      id: meta.id,
      name: meta.name,
      description: meta.description,
      defaultEnabled: meta.defaultEnabled,
      register: async (ctx: UxContext) => {
        ctx.onDispose(() => host.dispose());
        await host.activate(ctx);
      },
    };
    await activateUxPlugin(facade, { ...options, door });
    /* not enabled: the frame built to learn the id has no work; the switch
       builds a new one */
    if (uxPluginRecords()[meta.id]?.status !== "loaded") host.dispose();
    return meta.id;
  } catch (error) {
    /* the row carries the failure; the console line is a developer aid, and
       a navigation that aborts the runtime fetch mid-load is not an error */
    console.warn(`[ux-plugins] runtime load failed (${options.origin})`, error);
    publishUxPlugin({
      id: options.origin,
      name: options.origin,
      door,
      file: options.file,
      pack: options.pack,
      status: "error",
      error: error instanceof Error ? error.message : String(error),
    });
    return null;
  }
}

/** forget a runtime plugin entirely: registrations, frame, inventory row */
export function retireRuntimeUxPlugin(id: string): void {
  unloadUxPlugin(id);
  hosts.get(id)?.dispose();
  hosts.delete(id);
  dropUxPlugin(id);
}
