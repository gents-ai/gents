/* What the bridge answers about UX plugins: the inventory the doors scan,
   one plugin's produced module, and a call into a pack's own .afb plugin
   under the calling UX plugin's listing. */

/** What a plugin declares it may contribute; the host refuses the rest. */
export interface UxDeclaredContributions {
  areas: string[];
  directives: string[];
}

export interface UxPluginListing {
  /** `<name>` for a dev plugin, `<ns>/<pack>/<name>` for a pack one */
  id: string;
  door: "dev" | "pack";
  producer: "file" | "afb";
  file: string;
  pack?: string | null;
  description?: string | null;
  contributes?: UxDeclaredContributions | null;
  defaultEnabled?: boolean | null;
}

export interface UxPluginSource {
  source: string;
  css?: string | null;
  digest: string;
}

export interface PackPluginCallRequest {
  /** the calling UX plugin's runtime listing id; the pack is resolved from it */
  ux: string;
  /** one of that pack's .afb plugins */
  name: string;
  input?: unknown;
}

/** one data call a plugin's page makes: a declared tool of a declared
    surface on a node, under the calling plugin's listing */
export interface UxDataCallRequest {
  ux: string;
  nodeDid: string;
  surface: string;
  tool: string;
  args?: Record<string, unknown>;
}

export interface UxDataCallResult {
  kind: "query" | "create";
  output: unknown;
}

export interface UxDataSurfacesRequest {
  ux: string;
  nodeDid: string;
}
