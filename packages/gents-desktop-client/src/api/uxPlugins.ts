import type { JsonValue } from "../generated/serde_json/JsonValue.js";
import type {
  PackPluginCallRequest,
  UxDataCallRequest,
  UxDataCallResult,
  UxDataSurfacesRequest,
  UxPluginListing,
  UxPluginSource,
} from "../types/uxPlugins.js";
import type { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter, UxPluginCommand } from "./types.js";

/* the five ux commands answer JSON the bridge does not type yet; the shapes
   in ../types/uxPlugins.ts are the client's reading of them */
const shaped = <T>(answer: Promise<JsonValue>) => answer as Promise<unknown> as Promise<T>;

export function createUxPluginCommands(
  invokeDesktop: ReturnType<typeof createDesktopInvoker>,
): Pick<DesktopApiAdapter, UxPluginCommand> {
  return {
    listUxPlugins: (nodeDid?: string | null) =>
      shaped<{ plugins: UxPluginListing[]; pagesError?: string | null }>(
        invokeDesktop("desktop_ux_plugins_list", { nodeDid: nodeDid ?? null }),
      ),
    readUxPluginSource: (id: string, nodeDid?: string | null) =>
      shaped<UxPluginSource>(
        invokeDesktop("desktop_ux_plugin_source", { id, nodeDid: nodeDid ?? null }),
      ),
    callPackPlugin: (request: PackPluginCallRequest) =>
      shaped<{ output: unknown }>(
        invokeDesktop("desktop_plugin_call", {
          request: { ...request, input: (request.input ?? null) as JsonValue },
        }),
      ),
    callUxData: (request: UxDataCallRequest) =>
      shaped<UxDataCallResult>(
        invokeDesktop("desktop_ux_data_call", {
          request: { ...request, args: (request.args ?? {}) as JsonValue },
        }),
      ),
    listUxDataSurfaces: (request: UxDataSurfacesRequest) =>
      shaped<{ surfaces: unknown[] }>(invokeDesktop("desktop_ux_data_surfaces", { request })),
  };
}
